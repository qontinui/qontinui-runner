//! Coord-sync loop — drains the local outbox to coord, heartbeats every
//! active session, and replays on reconnect.
//!
//! Plan: `D:/qontinui-root/qontinui-dev-notes/plans/
//! 2026-05-22-coord-native-session-coordination.md` §Phase 3 (stacked on
//! top of Phase 2's substrate, PR #240).
//!
//! ## What runs
//!
//! Two long-lived tokio tasks materialized via [`CoordSync::start_drain_task`]
//! and [`CoordSync::start_heartbeat_task`] (the registry boots both at the
//! end of `main.rs`'s `.setup()` closure):
//!
//! 1. **Drain loop** — reads undelivered rows from the [`OutboxWriter`] in seq
//!    order, dispatches each to the right coord endpoint based on
//!    `event_kind`, and ACKs them back into the outbox's drain cursor.
//!    Records for one session go out serially (seq order is part of the
//!    contract); up to [`MAX_CONCURRENT_PUSH_CHAINS`] different sessions are
//!    pushed concurrently. Tick: 1s when there are undelivered rows, 5s when
//!    caught up. Backs off to 60s ceiling on repeated transport errors so
//!    coord-down sessions don't burn the CPU.
//!
//! 2. **Heartbeat loop** — every `QONTINUI_SESSION_HEARTBEAT_SECS`
//!    (default 15s, plan §D13), iterates the [`SessionRegistry`] and emits
//!    a heartbeat outbox row per active session — all of them in ONE batched
//!    append covered by ONE fsync. The drain loop then PATCHes coord with
//!    `{heartbeat: true}` which refreshes `last_heartbeat_at = now()` on
//!    coord-side. Stale-detection at 45s, auto-close at 180s.
//!
//!    It also HOSTS the cadence of [`crate::coord_outside_observer`] — the
//!    outside observer of coord's own liveness (plan
//!    `2026-09-12-merge-train-alerts-page-a-reader-and-act-on-nothing`
//!    Phase 3b). Every N-th tick, N chosen so the probe lands at about one a
//!    minute, it spawns a detached `coord_query_workers` read. This loop is
//!    the host because it already runs on coord's own cadence; the probe is
//!    detached so it can never delay a heartbeat, and the observer never
//!    writes into a coord it could not read.
//!
//! ## Wire mapping
//!
//! - `event_kind = "started"`  → `POST   /sessions` with the full create
//!   body (id, tenant_id, device_id, session_kind, intent).
//! - `event_kind = "heartbeat"`→ `PATCH  /sessions/:id` with
//!   `{heartbeat: true}` (coord's UpdateSessionRequest.heartbeat refreshes
//!   `last_heartbeat_at = now()`).
//! - `event_kind = "state_change"` → `PATCH  /sessions/:id` with the
//!   subset of fields the payload carries (state, repo, branch).
//! - `event_kind = "closed"`   → `DELETE /sessions/:id`.
//! - `event_kind = "claim_stolen"` → `POST   /sessions/:id/steal` with the
//!   typed reason payload (best-effort; the audit row is the substrate).
//! - `event_kind = "gate_registration"` → `POST /coord/work-units/:slug/
//!   register-gate` with the register-gate body rebuilt from the payload
//!   (the slug is a PATH segment carried in the payload). Best-effort; a
//!   404 `work_unit_not_found` triggers the lazy `work_unit_upsert`
//!   bootstrap — see [`gate_registration_outcome`].
//! - `event_kind = "finding_posted"` → `POST /coord/agent-findings` with the
//!   payload forwarded verbatim. Best-effort.
//!
//! ## Idempotency
//!
//! Coord's `coord.session_events` enforces `UNIQUE (session_id, seq)`.
//! A duplicate write (e.g. replay after partial failure) returns 200 with
//! the existing row, so this loop treats any 2xx **and** 409 as success
//! and ACKs the outbox row. The runner-side `seq` lives in the
//! [`OutboxWriter`] and is monotonic per `(machine_id, session_id)`.
//!
//! ## Disconnect tolerance
//!
//! Every HTTP failure (network, 5xx, timeout) leaves the row unacked. The
//! next tick re-reads `pending()` in seq order so the catch-up after a
//! reconnect is automatic. The file is the queue.
//!
//! ## One session's failure is that session's problem
//!
//! A failing row stops ITS session's chain (seq order is per session) and
//! puts that session on its own exponential backoff; every other session keeps
//! draining. Until 2026-09-23 a single non-best-effort failure tripped a
//! batch-wide abort on every tick, so one row coord kept answering 5xx stalled
//! every session on the box indefinitely — merytshost registered nothing for
//! days behind one poisoned row. The outage bound the abort exists for is kept:
//! any failure trips it unless the session is already failing AND coord has
//! taken some other row since that session's previous failure, so a real coord
//! outage still costs at most [`MAX_CONCURRENT_PUSH_CHAINS`] requests a tick,
//! and a tick in which every pending session is sitting out its own backoff
//! does not reset the loop's outage backoff. A session that keeps failing
//! while coord keeps taking OTHER rows is quarantined after
//! [`QUARANTINE_AFTER_SERVING_FAILURES`] such failures (a 429 on
//! `output_chunk`, the only kind that retries one, never counts — every other
//! kind treats a 429 as coord refusing the row and ACK-drops it):
//! its rows move to the `<outbox>.quarantine.jsonl` sidecar with a `warn!`,
//! and stop being retried. Only a row coord itself took counts as "taking" —
//! a locally ACK-dropped row (spent best-effort budget, a 4xx) does not — so
//! an outage can never quarantine anything.
//!
//! ## Conflict-on-acquire
//!
//! 409 from coord on `POST /sessions` (the row already exists for this
//! `(tenant, machine, session_id)` tuple — most often because a peer
//! stole the claim) marks the local session `PendingResolution` and
//! emits the Tauri event `agent-claim-conflict` so the existing
//! `ConflictModal` (Phase 6 will demote it to a toast) picks it up.
//! Wire payload matches the existing claim-conflict body shape so the
//! frontend doesn't need a schema update for Phase 3.

use std::collections::{HashMap, HashSet};
use std::io::Write as _;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use futures::stream::StreamExt;
use reqwest::StatusCode;
use serde::Serialize;
use serde_json::{json, Value as JsonValue};
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::auth::TenantScope;

use super::closeout_spool::{classify_coord_write_status, CoordWriteClass};
use super::dual_write::DualWriteGate;
use super::local_store::{OutboxEvent, OutboxRecord, OutboxWriter};
use super::{Intent, SessionEventKind, SessionRegistry, SessionState};

// ---------------------------------------------------------------------------
// Env tunables
// ---------------------------------------------------------------------------

/// Default heartbeat cadence (plan §D13).
const DEFAULT_HEARTBEAT_SECS: u64 = 15;
/// Default stale threshold — 3 missed heartbeats.
const DEFAULT_STALE_SECS: u64 = 45;

/// Read a `u64` env var with a sane default.
fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(default)
}

// ---------------------------------------------------------------------------
// CoordSync facade
// ---------------------------------------------------------------------------

/// Coord-sync facade. Owns the outbox handle + HTTP client + tunable
/// settings. Materialized once per process at startup; cloning is cheap
/// (everything is `Arc`-wrapped).
///
/// The Phase 2 stub exposed just `outbox()` + a no-op `start_drain_task()`.
/// Phase 3 keeps that surface stable so callers don't change, and adds the
/// real drain/heartbeat loops via [`CoordSync::start_drain_task`] +
/// [`CoordSync::start_heartbeat_task`].
#[derive(Clone)]
pub struct CoordSync {
    inner: Arc<CoordSyncInner>,
}

struct CoordSyncInner {
    outbox: Arc<OutboxWriter>,
    coord_url: String,
    http: reqwest::Client,
    heartbeat: Duration,
    stale: Duration,
    /// Set the first time we successfully reach coord. Used to decide
    /// whether to log a noisy "reconnected" line on the next success.
    has_been_online: AtomicBool,
    /// Optional Tauri AppHandle for emitting conflict events. None in
    /// tests; Some in production after [`CoordSync::attach_app_handle`].
    app_handle: Mutex<Option<tauri::AppHandle>>,
    /// Weak back-pointer to the registry so the heartbeat loop can
    /// enumerate active sessions and the drain loop can flip session
    /// state on conflict. Wired by [`CoordSync::attach_registry`] after
    /// `SessionRegistry::new` returns — the `Arc<SessionRegistry>`
    /// itself owns the `CoordSync`, so a strong handle here would
    /// cycle.
    registry: Mutex<Option<Weak<SessionRegistry>>>,
    /// Phase 10 cutover gate (plan
    /// `2026-05-23-coord-native-sessions-phase-7-10.md` §Phase 10).
    /// Caches the per-tenant `session_coordination_enabled` flag;
    /// default dormant. The poll task ([`CoordSync::start_flag_poll_task`])
    /// refreshes it from coord's `/tenant-policy` endpoint.
    dual_write: DualWriteGate,
    /// Invoked with the payload's `claude_session_id` and `finished_at` when coord ACKs a
    /// `finished` record — main.rs attaches `SessionLifecycleStore::
    /// mark_finish_synced` (plan
    /// `2026-09-01-session-finished-marker-and-unfinished-resume` §5.2, "Clear
    /// `finish_synced` on ACK"). Only a real `PushOutcome::Acked` counts; a
    /// permanent failure is ACK-dropped from the outbox but NOT synced.
    /// Unattached (tests, pre-wiring) → the flag is never stamped.
    finished_ack_observer: OnceLock<FinishedAckObserver>,
    /// The outside observer of coord's OWN liveness (plan
    /// `2026-09-12-merge-train-alerts-page-a-reader-and-act-on-nothing`
    /// Phase 3b). `Some` in production, `None` under
    /// [`CoordSync::new_for_test`] — so the fake-coord harness never issues a
    /// `tools/call` at the real upstream, and the observer's own predicates
    /// are tested where they live instead.
    ///
    /// Hosted HERE rather than in `health_monitor` because this loop already
    /// runs on coord's own cadence (15 s, backing off to 60 s on transport
    /// errors) while `health_monitor`'s 5 s self-probe thread is coord-blind
    /// and far too hot for a `POST /mcp` per runner.
    outside_observer: Option<Arc<crate::coord_outside_observer::CoordOutsideObserver>>,
    /// Sessions whose rows the drain loop must NOT push, because a caller is
    /// pushing that session's `started` row itself and awaiting coord's answer
    /// ([`CoordSync::confirm_started`]). Without the hold the drain could POST
    /// the same row concurrently and the loser's `409` would flip a healthy
    /// session to `PendingResolution`. Entries live only as long as a
    /// [`DrainHold`] guard.
    held: Mutex<HashSet<Uuid>>,
}

/// Boxed `finished`-ACK callback (see `CoordSyncInner::finished_ack_observer`).
struct FinishedAckObserver(Box<dyn Fn(&str, Option<i64>) + Send + Sync>);

impl std::fmt::Debug for CoordSync {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CoordSync")
            .field("coord_url", &self.inner.coord_url)
            .field("heartbeat", &self.inner.heartbeat)
            .field("stale", &self.inner.stale)
            .finish()
    }
}

impl CoordSync {
    /// Construct a CoordSync with all settings resolved from env. Used at
    /// app startup; tests prefer [`CoordSync::new_for_test`].
    pub fn new(outbox: Arc<OutboxWriter>) -> Self {
        let (coord_url, _coord_base_source) =
            qontinui_runner_lib::profiles::coord_base_with_source();
        let heartbeat = Duration::from_secs(env_u64(
            "QONTINUI_SESSION_HEARTBEAT_SECS",
            DEFAULT_HEARTBEAT_SECS,
        ));
        let stale = Duration::from_secs(env_u64("QONTINUI_SESSION_STALE_SECS", DEFAULT_STALE_SECS));

        let http = reqwest::Client::builder()
            // Per-request timeout — slow enough to ride out a hiccup,
            // fast enough that the drain loop doesn't stall on a hung
            // coord. 30s matches the `agent_claims` heartbeat client.
            .timeout(Duration::from_secs(30))
            // A dead route must fail the CONNECT fast rather than eat the whole
            // 30 s budget, and an idle pooled socket a NAT/proxy silently
            // dropped must be probed (keepalive) or retired (idle timeout)
            // before a push lands on it — the merytshost 2026-09-23 drain
            // failed every row with a bare "error sending request".
            .connect_timeout(Duration::from_secs(10))
            .tcp_keepalive(Duration::from_secs(30))
            .pool_idle_timeout(Duration::from_secs(60))
            .build()
            .unwrap_or_else(|e| {
                tracing::warn!(error = %e, "coord_sync: reqwest client build failed; using default");
                reqwest::Client::new()
            });

        Self {
            inner: Arc::new(CoordSyncInner {
                outbox,
                coord_url,
                http,
                heartbeat,
                stale,
                has_been_online: AtomicBool::new(false),
                app_handle: Mutex::new(None),
                registry: Mutex::new(None),
                dual_write: DualWriteGate::new(),
                finished_ack_observer: OnceLock::new(),
                outside_observer: Some(Arc::new(
                    crate::coord_outside_observer::CoordOutsideObserver::new(),
                )),
                held: Mutex::new(HashSet::new()),
            }),
        }
    }

    /// Test-only constructor. Pins the coord URL and runs heartbeats on
    /// the millisecond cadence the tests need without polluting global
    /// env vars.
    #[cfg(test)]
    pub fn new_for_test(
        outbox: Arc<OutboxWriter>,
        coord_url: String,
        heartbeat: Duration,
        stale: Duration,
    ) -> Self {
        Self {
            inner: Arc::new(CoordSyncInner {
                outbox,
                coord_url,
                http: reqwest::Client::builder()
                    .timeout(Duration::from_secs(5))
                    .build()
                    .unwrap(),
                heartbeat,
                stale,
                has_been_online: AtomicBool::new(false),
                app_handle: Mutex::new(None),
                registry: Mutex::new(None),
                dual_write: DualWriteGate::new_for_test(None, Duration::from_secs(60)),
                finished_ack_observer: OnceLock::new(),
                // Inert by construction under test: the heartbeat harness
                // drives this loop on a millisecond cadence against a fake
                // coord that serves no `/mcp`, and an observer here would
                // probe the REAL upstream from a unit test.
                outside_observer: None,
                held: Mutex::new(HashSet::new()),
            }),
        }
    }

    /// Borrow the local outbox. Phase 2 surface — unchanged.
    pub fn outbox(&self) -> &OutboxWriter {
        &self.inner.outbox
    }

    /// Coord URL the loops POST/PATCH/DELETE against. Surfaced for tests
    /// and reused by the Phase 7 handoff trigger + receiver so they hit
    /// the same coord this runner already syncs to.
    pub fn coord_url(&self) -> &str {
        &self.inner.coord_url
    }

    /// The shared reqwest client (connection-pooled, 30s timeout). The
    /// Phase 7 handoff trigger + receiver reuse it rather than minting a
    /// fresh client per call.
    pub fn http_client(&self) -> reqwest::Client {
        self.inner.http.clone()
    }

    /// The tenant scope that owns `session_id`, for per-session credential
    /// selection by callers outside this module.
    ///
    /// Only for callers that KNOW the registry already holds the session —
    /// `SessionRegistry::attach_output_pipe`, which runs after the record is
    /// inserted. Callers on a pre-insert path must pass the intent's tenant
    /// down instead (see [`CoordSync::probe_resume`]); this returns
    /// [`TenantScope::Unresolved`] there, which is honest but degrades on a
    /// multi-bound device rather than resolving to the tenant the caller could
    /// have supplied.
    pub fn session_tenant(&self, session_id: Uuid) -> TenantScope {
        session_tenant_by_id(&self.inner, session_id)
    }

    /// Heartbeat interval. Surfaced for tests + tracing.
    #[allow(dead_code)]
    pub fn heartbeat_interval(&self) -> Duration {
        self.inner.heartbeat
    }

    /// Stale threshold (3 missed heartbeats).
    #[allow(dead_code)]
    pub fn stale_threshold(&self) -> Duration {
        self.inner.stale
    }

    /// Attach the Tauri AppHandle so the drain loop can emit
    /// `agent-claim-conflict` events. Called from `main.rs::setup` after
    /// the AppHandle is available.
    pub fn attach_app_handle(&self, handle: tauri::AppHandle) {
        let mut slot = self
            .inner
            .app_handle
            .lock()
            .expect("coord_sync app_handle slot poisoned");
        *slot = Some(handle);
    }

    /// Attach a weak back-pointer to the registry. Called from
    /// `SessionRegistry::new` after the Arc is built. Must run before
    /// either loop starts — neither loop guards against `None` so the
    /// heartbeat enumeration / conflict state-flip happens with full
    /// fidelity once the registry is wired.
    pub fn attach_registry(&self, registry: &Arc<SessionRegistry>) {
        let mut slot = self
            .inner
            .registry
            .lock()
            .expect("coord_sync registry slot poisoned");
        *slot = Some(Arc::downgrade(registry));
    }

    /// Attach the `finished`-ACK observer (once, at startup) — invoked with the
    /// `claude_session_id` from a `finished` record's payload each time coord
    /// ACKs one. See `CoordSyncInner::finished_ack_observer`.
    pub fn attach_finished_ack_observer(
        &self,
        f: impl Fn(&str, Option<i64>) + Send + Sync + 'static,
    ) {
        if self
            .inner
            .finished_ack_observer
            .set(FinishedAckObserver(Box::new(f)))
            .is_err()
        {
            tracing::warn!("coord_sync: finished-ACK observer already attached — ignoring");
        }
    }

    /// Start the drain task. Returns the [`JoinHandle`] so `main.rs` can
    /// keep it alive for the lifetime of the process.
    pub fn start_drain_task(&self) -> JoinHandle<()> {
        let inner = Arc::clone(&self.inner);
        tokio::spawn(run_drain_loop(inner))
    }

    /// Start the heartbeat task. Returns the [`JoinHandle`] so `main.rs`
    /// can keep it alive.
    pub fn start_heartbeat_task(&self) -> JoinHandle<()> {
        let inner = Arc::clone(&self.inner);
        tokio::spawn(run_heartbeat_loop(inner))
    }

    /// Keep the drain loop off `session_id`'s rows until the returned guard
    /// drops. Take it BEFORE the session's `started` row is written, so no
    /// drain tick can ever see that row un-held.
    pub fn hold_drain(&self, session_id: Uuid) -> DrainHold {
        self.inner
            .held
            .lock()
            .expect("coord_sync held-set poisoned")
            .insert(session_id);
        DrainHold {
            inner: Arc::clone(&self.inner),
            session_id,
        }
    }

    /// Push ONE session's `started` row to coord now and wait — at most
    /// `timeout` — for coord's answer, instead of leaving it to the drain loop.
    ///
    /// For a caller that is about to hand the session id to someone who will
    /// immediately ask coord about it (a remote create's source mints an attach
    /// grant BY session id). Reporting an id coord has never heard of is what
    /// made every such attach `404` while the row sat in a stalled outbox.
    ///
    /// `Ok(())` only on a coord 2xx, and the row is then ACKed in the outbox so
    /// the drain never re-POSTs it (a re-POST could answer `409` and flip the
    /// session to `PendingResolution`). Every other answer is an `Err` carrying
    /// the typed kind and, where coord answered at all, its status. The row is
    /// left in the outbox on `Err`; what to do with it is the caller's call.
    ///
    /// The caller must hold a [`DrainHold`] for the session for the duration.
    pub async fn confirm_started(
        &self,
        rec: &OutboxRecord,
        timeout: Duration,
    ) -> Result<(), CoordRegistrationFailure> {
        let failure = match tokio::time::timeout(timeout, push_record(&self.inner, rec)).await {
            Ok(PushOutcome::Acked) => {
                if let Err(e) = self.inner.outbox.ack(&[(rec.session_id, rec.seq)]) {
                    // Coord HAS the row, so the confirmation stands; the drain
                    // will replay it once the hold drops.
                    tracing::warn!(
                        session = %rec.session_id,
                        seq = rec.seq,
                        error = %e,
                        "coord_sync: started row confirmed by coord but the local ACK failed — \
                         the drain will replay it"
                    );
                }
                note_outbox_ack();
                return Ok(());
            }
            Ok(PushOutcome::Conflict { .. }) => CoordRegistrationFailure {
                kind: "conflict",
                status: Some(409),
                detail: "coord answered 409 to POST /sessions — a row with this id already \
                         exists and is not confirmed as this device's"
                    .to_string(),
            },
            Ok(PushOutcome::Transport(msg)) => {
                let (kind, status) = classify_push_failure(&msg, false);
                CoordRegistrationFailure {
                    kind,
                    status,
                    detail: snippet(&msg),
                }
            }
            Ok(PushOutcome::PermanentFailure(msg)) => {
                let (kind, status) = classify_push_failure(&msg, true);
                CoordRegistrationFailure {
                    kind,
                    status,
                    detail: snippet(&msg),
                }
            }
            Err(_elapsed) => CoordRegistrationFailure {
                kind: "timeout",
                status: None,
                detail: format!(
                    "coord did not answer POST /sessions within {:.1}s",
                    timeout.as_secs_f32()
                ),
            },
        };
        note_outbox_failure(failure.kind, failure.status);
        Err(failure)
    }

    // -----------------------------------------------------------------
    // Phase 10 — flag-gated dual-write (plan
    // `2026-05-23-coord-native-sessions-phase-7-10.md` §Phase 10).
    //
    // DORMANT by default. The gate is closed unless the runner's tenant
    // has flipped `coord.tenant_policies.session_coordination_enabled`.
    // With the flag off there is ZERO production behavior change: the
    // poll task refreshes a `false` atom and `mirror_legacy_session` is
    // a no-op. See `super::dual_write` for the full safety argument.
    // -----------------------------------------------------------------

    /// Hot-path read of the cutover gate. `true` only when the resolved
    /// tenant has flipped the flag. Default `false` (dormant).
    pub fn dual_write_enabled(&self) -> bool {
        self.inner.dual_write.enabled()
    }

    /// R2 (session-lifecycle-cleanup) — probe coord to decide whether a
    /// persisted session id can be RESUMED (PATCH) or must be re-registered
    /// fresh (POST). Issues `PATCH /sessions/:id` with
    /// `{state:"active", heartbeat:true}` — the same body the drain loop's
    /// `state_change` push sends, so a successful probe also re-activates +
    /// heartbeats the row (the resume is effectively done coord-side on a
    /// 2xx). Coord already supports `state` + `heartbeat` on
    /// `UpdateSessionRequest`, so **no coord-side change is required**.
    ///
    /// Maps the response to a coarse [`ResumeProbe`]:
    /// - 2xx → [`ResumeProbe::Found`] (row exists, now re-activated)
    /// - 404 / 410 → [`ResumeProbe::NotFound`] (GC'd or never existed)
    /// - everything else (network, timeout, 5xx, other 4xx) →
    ///   [`ResumeProbe::Unreachable`] (treat as transient; caller resumes
    ///   optimistically and the drain loop retries).
    ///
    /// Called by [`SessionRegistry::resume_external`].
    ///
    /// `tenant` is the OWNING session's tenant and must be supplied by the
    /// caller — it cannot be resolved here. This probe runs *before*
    /// `insert_resumed_record`, so the registry provably does not hold
    /// `session_id` yet; a [`session_tenant_by_id`] lookup would answer
    /// [`TenantScope::Unresolved`] on every call, which on a multi-bound device
    /// degrades the probe to unauthenticated for a session whose tenant the
    /// caller was holding all along. The caller has the intent; it passes the
    /// tenant down.
    pub async fn probe_resume(&self, session_id: Uuid, tenant: Option<Uuid>) -> ResumeProbe {
        let base = self.inner.coord_url.trim_end_matches('/');
        let url = format!("{base}/sessions/{session_id}");
        let body = json!({ "state": SessionState::Active.as_str(), "heartbeat": true });
        // Per-session credential selection, same as every other write in this
        // module. This one was anonymous: it lands the identical body on the
        // identical route as the drain loop's `state_change` push, which has
        // presented the owning session's slot since Phase 8b. Nobody chose
        // that asymmetry — `probe_resume` was added later, for a different
        // reason, and the auth was attached per-function rather than by a
        // predicate over all of them.
        match crate::auth::attach_device_auth_for(
            self.inner.http.patch(&url).json(&body),
            TenantScope::for_session(tenant),
        )
        .send()
        .await
        {
            Ok(resp) => {
                let status = resp.status();
                if status.is_success() {
                    ResumeProbe::Found
                } else if status == StatusCode::NOT_FOUND || status == StatusCode::GONE {
                    ResumeProbe::NotFound
                } else {
                    tracing::debug!(
                        session = %session_id,
                        %status,
                        "coord_sync: resume probe got non-2xx/non-404 — treating as unreachable"
                    );
                    ResumeProbe::Unreachable
                }
            }
            Err(e) => {
                tracing::debug!(
                    session = %session_id,
                    error = %e,
                    "coord_sync: resume probe transport error — treating as unreachable"
                );
                ResumeProbe::Unreachable
            }
        }
    }

    /// Test-only: force the cutover gate to a given value, simulating
    /// what the poll loop does when a tenant flips the flag.
    #[cfg(test)]
    pub fn force_dual_write_for_test(&self, value: bool) {
        self.inner.dual_write.apply(value);
    }

    /// Start the Phase 10 flag-refresh task. Polls coord's
    /// `/tenant-policy?tenant_id=<id>` on a slow cadence and caches the
    /// `session_coordination_enabled` flag into the [`DualWriteGate`].
    ///
    /// Short-circuits to a permanent no-op when no `active_tenant_id`
    /// resolved from `machine.json` (single-tenant operators, MSI today)
    /// — the gate then never opens regardless of any tenant's flag.
    /// Returns `None` in that case so `main.rs` doesn't hold a dead
    /// handle.
    pub fn start_flag_poll_task(&self) -> Option<JoinHandle<()>> {
        let tenant_id = self.inner.dual_write.tenant_id()?;
        let inner = Arc::clone(&self.inner);
        Some(tokio::spawn(run_flag_poll_loop(inner, tenant_id)))
    }

    /// Phase 10 dual-write entry point — called by the **legacy** session
    /// surface (`commands/terminal.rs`, `claude_session/`) right after it
    /// spawns its PTY / CLI subprocess. When the cutover flag is OFF
    /// (default), returns `None` immediately without touching the
    /// registry, the outbox, or coord — the legacy path behaves exactly
    /// as it does today.
    ///
    /// When the flag is ON, mirrors the legacy session into the
    /// coord-native primitive via [`SessionRegistry::register_external`]
    /// so the dashboard renders the same session from `coord.sessions`.
    /// `register_external` does NOT spawn a transport — the operator's
    /// real PTY / CLI subprocess is owned by the legacy path, so the
    /// mirror is pure bookkeeping (no double-spawn, no second window).
    ///
    /// The returned [`Uuid`] is the coord-native session id, which the
    /// legacy caller should retain so it can close the mirror when the
    /// legacy session ends (via `registry.close_by_id(id)`, surfaced in a
    /// later wiring step). Errors are logged and swallowed — a mirror
    /// failure must never break the legacy session the operator actually
    /// wants.
    ///
    /// `registry` is passed in (rather than read from the weak back-
    /// pointer) so the legacy caller, which already holds the managed
    /// `Arc<SessionRegistry>` via `tauri::State`, drives the mirror
    /// without this facade needing a strong upgrade.
    pub fn mirror_legacy_session(
        &self,
        registry: &Arc<SessionRegistry>,
        intent: Intent,
    ) -> Option<Uuid> {
        if !self.dual_write_enabled() {
            // Dormant — the overwhelming common case. No allocation, no
            // I/O, no behavior change.
            return None;
        }
        match registry.register_external(intent) {
            Ok(id) => {
                tracing::info!(
                    session = %id,
                    "dual_write: mirrored legacy session into coord-native primitive"
                );
                Some(id)
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "dual_write: mirror session register failed — legacy session unaffected"
                );
                None
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Confirmed registration (remote create)
// ---------------------------------------------------------------------------

/// RAII guard from [`CoordSync::hold_drain`]: while it lives the drain loop
/// skips every row of its session.
pub struct DrainHold {
    inner: Arc<CoordSyncInner>,
    session_id: Uuid,
}

impl Drop for DrainHold {
    fn drop(&mut self) {
        if let Ok(mut held) = self.inner.held.lock() {
            held.remove(&self.session_id);
        }
    }
}

/// Why coord did NOT confirm a session's `started` row
/// ([`CoordSync::confirm_started`]).
///
/// `kind` is one of `server_error` (5xx), `rate_limited` (429),
/// `unauthorized` (401/403 — coord refused the credential, or its absence),
/// `rejected` (another 4xx), `http_error` (any other non-2xx), `network` (no HTTP answer),
/// `timeout` (no answer inside the bound), `conflict` (409), or `local` (the
/// runner failed before reaching coord). `status` is coord's HTTP status where
/// it answered one, `None` where it never answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoordRegistrationFailure {
    pub kind: &'static str,
    pub status: Option<u16>,
    pub detail: String,
}

impl std::fmt::Display for CoordRegistrationFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.status {
            Some(status) => write!(f, "{} (HTTP {status}): {}", self.kind, self.detail),
            None => write!(f, "{}: {}", self.kind, self.detail),
        }
    }
}

/// Type a failed push by its leading HTTP status, which every non-2xx arm
/// formats as `"{status}: {body}"` (reqwest's `StatusCode` display starts with
/// the three digits). A message with no leading status never got an HTTP
/// answer at all.
fn classify_push_failure(msg: &str, permanent: bool) -> (&'static str, Option<u16>) {
    let status = msg
        .split_whitespace()
        .next()
        .map(|t| t.trim_end_matches(':'))
        .and_then(|t| t.parse::<u16>().ok())
        .filter(|s| (100..=599).contains(s));
    if status.is_none() && msg.starts_with("[timeout]") {
        return ("timeout", None);
    }
    let kind = match status {
        Some(429) => "rate_limited",
        Some(s) if s >= 500 => "server_error",
        Some(409) => "conflict",
        Some(401 | 403) => "unauthorized",
        Some(s) if (400..500).contains(&s) => "rejected",
        Some(_) => "http_error",
        None if permanent => "rejected",
        None => "network",
    };
    (kind, status)
}

/// Bound a coord response body for a log line or a typed error.
fn snippet(msg: &str) -> String {
    const CAP: usize = 600;
    if msg.chars().count() <= CAP {
        msg.to_string()
    } else {
        let mut out: String = msg.chars().take(CAP).collect();
        out.push('…');
        out
    }
}

// ---------------------------------------------------------------------------
// Session outbox health (`GET /health` `sessionOutbox`)
// ---------------------------------------------------------------------------

/// The last push failure the drain (or a confirmed registration) saw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OutboxFailure {
    pub kind: &'static str,
    pub status: Option<u16>,
    pub at: DateTime<Utc>,
}

/// What `GET /health` `sessionOutbox` reports. Written by the drain loop at the
/// end of every tick, so `pending` / `oldestUnackedAt` are as of `observed_at`;
/// `None` there means no tick has run in this process — UNKNOWN, not empty.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SessionOutboxHealth {
    pub observed_at: Option<DateTime<Utc>>,
    pub pending: u64,
    pub oldest_unacked_at: Option<DateTime<Utc>>,
    pub last_ack_at: Option<DateTime<Utc>>,
    pub last_failure: Option<OutboxFailure>,
    pub retrying_sessions: u64,
    pub quarantined_sessions: u64,
}

fn session_outbox_health_cell() -> &'static Mutex<SessionOutboxHealth> {
    static CELL: OnceLock<Mutex<SessionOutboxHealth>> = OnceLock::new();
    CELL.get_or_init(|| Mutex::new(SessionOutboxHealth::default()))
}

fn with_outbox_health(f: impl FnOnce(&mut SessionOutboxHealth)) {
    if let Ok(mut h) = session_outbox_health_cell().lock() {
        f(&mut h);
    }
}

fn note_outbox_ack() {
    with_outbox_health(|h| h.last_ack_at = Some(Utc::now()));
}

fn note_outbox_failure(kind: &'static str, status: Option<u16>) {
    with_outbox_health(|h| {
        h.last_failure = Some(OutboxFailure {
            kind,
            status,
            at: Utc::now(),
        })
    });
}

/// `GET /health` `sessionOutbox`, rendered from the live process state.
pub(crate) fn session_outbox_health_json() -> JsonValue {
    let snapshot = session_outbox_health_cell()
        .lock()
        .map(|h| h.clone())
        .unwrap_or_default();
    render_session_outbox_health(&snapshot)
}

/// Render one `sessionOutbox` block from an explicit snapshot (so a test can
/// pin the shape without the process-global state).
pub(crate) fn render_session_outbox_health(h: &SessionOutboxHealth) -> JsonValue {
    let observed = h.observed_at.is_some();
    let counted = |n: u64| if observed { json!(n) } else { JsonValue::Null };
    json!({
        // `null` until the first drain tick: an unobserved queue is UNKNOWN.
        "pending": counted(h.pending),
        "oldestUnackedAt": h.oldest_unacked_at,
        "lastAckAt": h.last_ack_at,
        "lastFailure": h.last_failure.as_ref().map(|f| json!({
            "kind": f.kind,
            "status": f.status,
            "at": f.at,
        })),
        "retryingSessions": counted(h.retrying_sessions),
        "quarantinedSessions": counted(h.quarantined_sessions),
        "observedAt": h.observed_at,
    })
}

// ---------------------------------------------------------------------------
// Resume probe outcome (R2)
// ---------------------------------------------------------------------------

/// Coarse outcome of [`CoordSync::probe_resume`], driving
/// [`super::SessionRegistry::resume_external`]'s PATCH-vs-fresh-POST
/// decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResumeProbe {
    /// The coord row exists and was re-activated by the probe PATCH.
    Found,
    /// Coord returned 404/410 — the row was GC'd or never existed. The
    /// caller registers a fresh session.
    NotFound,
    /// Coord was unreachable / returned a transient error. The caller
    /// resumes optimistically under the persisted id; the drain loop
    /// retries.
    Unreachable,
}

// ---------------------------------------------------------------------------
// Conflict payload shape (matches existing `agent-claim-conflict` event)
// ---------------------------------------------------------------------------

/// Frontend payload for the `agent-claim-conflict` Tauri event. Mirrors
/// the existing `ConflictModal` listener wire shape so Phase 3 doesn't
/// require a frontend schema bump.
#[derive(Debug, Clone, Serialize)]
struct AgentClaimConflict {
    kind: String,
    resource_key: String,
    current_holder: Option<Uuid>,
    intent: Option<String>,
    /// Phase 3 add — the session id that hit the conflict. Existing
    /// listener ignores unknown fields.
    session_id: Uuid,
}

// ---------------------------------------------------------------------------
// Drain loop
// ---------------------------------------------------------------------------

/// Tick cadence when there's work to drain.
const TICK_BUSY: Duration = Duration::from_secs(1);
/// Tick cadence when the outbox is empty.
const TICK_IDLE: Duration = Duration::from_secs(5);
/// Max backoff after repeated transport errors.
const MAX_BACKOFF: Duration = Duration::from_secs(60);

/// Failed attempts — counted only when coord took some OTHER row since the
/// session's previous failure — after which a session's queue is quarantined to the sidecar rather
/// than retried forever. With the per-session backoff (1 s doubling to
/// [`MAX_BACKOFF`]) this is roughly three minutes of a row coord keeps refusing
/// while it accepts everything else.
const QUARANTINE_AFTER_SERVING_FAILURES: u32 = 8;

/// An ACK this recent means coord is serving, so a failure in the same window
/// is about the ROW, not about coord.
const SERVING_WINDOW: Duration = Duration::from_secs(120);

/// A session whose chain is failing: when it may be retried, and how many of
/// its failures happened while coord was serving other rows.
#[derive(Debug, Clone)]
struct SessionRetry {
    failures: u32,
    serving_failures: u32,
    next_attempt_at: Instant,
    /// [`DrainState::ack_ticks`] as of the START of the tick this session last
    /// failed in. Coord has taken some other row since that failure iff
    /// `ack_ticks` has moved past it — a strictly increasing counter, so two
    /// events in one tick can never compare ambiguously the way two `Instant`s
    /// can.
    failed_at_ack_tick: u64,
}

impl SessionRetry {
    /// 1 s, 2 s, 4 s … capped at [`MAX_BACKOFF`].
    fn backoff(failures: u32) -> Duration {
        let secs = 1u64 << failures.saturating_sub(1).min(6);
        std::cmp::min(Duration::from_secs(secs), MAX_BACKOFF)
    }
}

/// `<outbox>.quarantine.jsonl` — where a quarantined session's rows go.
fn quarantine_path(outbox: &OutboxWriter) -> std::path::PathBuf {
    let p = outbox.path();
    let name = p
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "session-outbox.jsonl".to_string());
    p.with_file_name(format!("{name}.quarantine.jsonl"))
}

/// Append rows to the quarantine sidecar. Only an `Ok` lets the caller ACK
/// them out of the outbox — a row is never dropped without landing somewhere.
fn append_quarantine(outbox: &OutboxWriter, rows: &[OutboxRecord]) -> std::io::Result<()> {
    if rows.is_empty() {
        return Ok(());
    }
    let mut buf = String::new();
    for r in rows {
        buf.push_str(&serde_json::to_string(r).map_err(std::io::Error::other)?);
        buf.push('\n');
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(quarantine_path(outbox))?;
    f.write_all(buf.as_bytes())?;
    f.sync_data()
}

/// Bounded retry budget for a BEST-EFFORT record (see
/// [`is_best_effort_kind`]). These kinds must never head-of-line-block session
/// lifecycle events queued behind them, so a failing record is skipped (not
/// batch-breaking) and Ack-dropped once its budget is spent.
const BEST_EFFORT_MAX_ATTEMPTS: u32 = 3;

/// Map a non-2xx coord write response onto a [`PushOutcome`], through the ONE
/// classifier both halves of plan
/// `2026-08-28-closeout-has-no-durable-store-when-the-runner-is-offline` read
/// ([`classify_coord_write_status`]): a 4xx is coord refusing the CONTENT and
/// is Ack-dropped, anything else non-2xx is the retryable class.
///
/// Extracted in Phase 3 from the arms that each spelled `status.is_client_error()`
/// inline. Phase 3 added a SECOND consumer of exactly this rule — the loopback
/// coord-write forwarders, which must spool a transport failure but must NOT
/// spool a 4xx (that would replay a guaranteed failure three times and then
/// drop it silently). Two copies of one retry policy is the pair that drifts,
/// so both call this.
fn write_failure_outcome(status: StatusCode, message: String) -> PushOutcome {
    match classify_coord_write_status(status.as_u16()) {
        CoordWriteClass::Permanent => PushOutcome::PermanentFailure(message),
        CoordWriteClass::Spoolable => PushOutcome::Transport(message),
    }
}

/// `coord-transport-rung` rows the drain Ack-DROPPED on a 404 (coord does not
/// know the lane's session id) since this process booted. Module-level, not
/// function-local, so `GET /health` `transportRung.drainDropped` can read it
/// (plan 2026-09-18-runner-transport-rung-rows-never-reach-coord-despite-a-
/// serving-emitter, Phase 1): a drop that leaves no counter is
/// indistinguishable from a fleet where nobody calls coord.
pub(crate) static TRANSPORT_RUNG_DROPPED_404: AtomicU64 = AtomicU64::new(0);
/// The 405 twin: rows dropped because the serving coord has no session-events
/// ingest route. The push arm's throttled `warn!` reads this same counter, so
/// hoisting it out of the function changed nothing about the log cadence.
pub(crate) static TRANSPORT_RUNG_DROPPED_405: AtomicU64 = AtomicU64::new(0);

/// Rows Ack-dropped on any OTHER 4xx — a status the shared classifier
/// ([`write_failure_outcome`]) maps to `PermanentFailure`, including a 429,
/// which this kind does not special-case the way `output_chunk` does. The
/// drain logs each one at `error!`; this is the total `GET /health`
/// `transportRung.drainDropped.other4xx` reads, so the block no longer has an
/// uncounted 4xx arm.
pub(crate) static TRANSPORT_RUNG_DROPPED_OTHER_4XX: AtomicU64 = AtomicU64::new(0);

/// The drain's Ack-drop totals for `coord-transport-rung` rows, one per arm,
/// for `GET /health` `transportRung.drainDropped`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TransportRungDrainDropped {
    pub not_found: u64,
    pub method_not_allowed: u64,
    pub other_client_error: u64,
}

pub(crate) fn transport_rung_drain_dropped() -> TransportRungDrainDropped {
    TransportRungDrainDropped {
        not_found: TRANSPORT_RUNG_DROPPED_404.load(Ordering::Relaxed),
        method_not_allowed: TRANSPORT_RUNG_DROPPED_405.load(Ordering::Relaxed),
        other_client_error: TRANSPORT_RUNG_DROPPED_OTHER_4XX.load(Ordering::Relaxed),
    }
}

/// Kinds drained under the BEST-EFFORT posture: a transport failure skips the
/// record instead of breaking the batch, and the record is Ack-dropped once
/// [`BEST_EFFORT_MAX_ATTEMPTS`] is spent.
///
/// All three are coord writes that are NOT session lifecycle events — a
/// helper task, and the two closeout kinds from plan
/// `2026-08-28-closeout-has-no-durable-store-when-the-runner-is-offline`.
/// Stalling a session's `started`/`closed` behind any of them would be worse
/// than losing one of them, which is exactly the trade the posture encodes.
fn is_best_effort_kind(kind: &str) -> bool {
    kind == SessionEventKind::HelperTaskCreated.as_str()
        || kind == SessionEventKind::GateRegistration.as_str()
        || kind == SessionEventKind::FindingPosted.as_str()
        || kind == SessionEventKind::AgentNotification.as_str()
}

/// How many per-session push chains run at once (plan
/// `2026-07-28-runner-many-sessions-performance` §7a/B6).
///
/// Records for one session MUST go out in seq order, so a session's records
/// stay a serial chain; different sessions are independent and run
/// concurrently. The bound is what keeps a coord outage from becoming N
/// parallel retries: the first transport error trips a shared abort flag that
/// every chain checks before its next push, so at most this many requests are
/// ever in flight when the outage is noticed.
const MAX_CONCURRENT_PUSH_CHAINS: usize = 8;

/// What one per-session chain reports back to the drain tick.
struct ChainOutcome {
    session_id: Uuid,
    /// Rows to ACK out of the outbox — delivered OR deliberately dropped.
    succeeded: Vec<(Uuid, i64)>,
    /// Coord itself took at least one row (2xx or a 409 on create). The ONLY
    /// evidence the drain treats as "coord is serving": `succeeded` also holds
    /// rows ACK-dropped after a spent best-effort budget or a 4xx, and those
    /// say nothing about whether coord is up.
    acked_by_coord: bool,
    /// Updated best-effort attempt counters for this session's records.
    attempts: HashMap<(Uuid, i64), u32>,
    /// Keys whose best-effort budget should be forgotten (delivered/dropped).
    cleared: Vec<(Uuid, i64)>,
    had_transport_error: bool,
    /// `Some(error)` when a NON-best-effort row failed and stopped this chain:
    /// the session is blocked on its head row and goes on its own backoff.
    blocked_on: Option<String>,
    /// The chain stopped because ANOTHER chain tripped the abort flag, not
    /// because of anything about this session — its retry state is untouched.
    aborted: bool,
}

/// Push one session's records in seq order.
///
/// A non-best-effort failure stops THIS chain (seq order within a session is
/// the contract). It trips the shared `abort` flag only when `trip_abort` is
/// set — the caller clears it for a session that is already known to be
/// failing WHILE coord is serving other rows, because that session's retry is
/// evidence about its own row only, and letting it trip the flag on every
/// retry is how one poisoned row used to stall every session on the box. Any
/// other failure still trips it, so an outage stays bounded to
/// [`MAX_CONCURRENT_PUSH_CHAINS`] requests a tick.
async fn push_chain(
    inner: Arc<CoordSyncInner>,
    records: Vec<OutboxRecord>,
    mut attempts: HashMap<(Uuid, i64), u32>,
    abort: Arc<AtomicBool>,
    trip_abort: bool,
) -> ChainOutcome {
    let mut out = ChainOutcome {
        session_id: records.first().map(|r| r.session_id).unwrap_or_default(),
        succeeded: Vec::with_capacity(records.len()),
        acked_by_coord: false,
        attempts: HashMap::new(),
        cleared: Vec::new(),
        had_transport_error: false,
        blocked_on: None,
        aborted: false,
    };

    for rec in records {
        if abort.load(Ordering::Relaxed) {
            // Another chain hit a transport failure while coord may be down —
            // do not add to the pile.
            out.aborted = true;
            break;
        }
        match push_record(&inner, &rec).await {
            PushOutcome::Acked => {
                out.acked_by_coord = true;
                out.succeeded.push((rec.session_id, rec.seq));
                out.cleared.push((rec.session_id, rec.seq));
                notify_finished_ack(&inner, &rec);
            }
            PushOutcome::Conflict { row } => {
                out.acked_by_coord = true;
                out.succeeded.push((rec.session_id, rec.seq));
                handle_conflict(&inner, &rec, row).await;
            }
            PushOutcome::Transport(e) => {
                let (fail_kind, fail_status) = classify_push_failure(&e, false);
                note_outbox_failure(fail_kind, fail_status);
                // A best-effort kind (helper tasks + the two closeout kinds)
                // must never break the batch (session lifecycle events queued
                // behind it would stall indefinitely). Skip it WITHOUT acking
                // — `OutboxWriter::ack` marks exact (session_id, seq) pairs,
                // so acking later records leaves this one pending — and
                // keep draining. Retried on subsequent ticks up to
                // BEST_EFFORT_MAX_ATTEMPTS, then Ack-dropped with a warn.
                if is_best_effort_kind(&rec.event_kind) {
                    let key = (rec.session_id, rec.seq);
                    let entry = attempts.entry(key).or_insert(0);
                    *entry += 1;
                    if *entry >= BEST_EFFORT_MAX_ATTEMPTS {
                        tracing::warn!(
                            session = %rec.session_id,
                            seq = rec.seq,
                            kind = %rec.event_kind,
                            error = %e,
                            "coord_sync: best-effort push failed {BEST_EFFORT_MAX_ATTEMPTS} \
                             time(s) — dropping"
                        );
                        out.succeeded.push(key);
                        out.cleared.push(key);
                        attempts.remove(&key);
                    } else {
                        tracing::warn!(
                            session = %rec.session_id,
                            seq = rec.seq,
                            kind = %rec.event_kind,
                            attempt = *entry,
                            error = %e,
                            "coord_sync: best-effort push failed — will retry \
                             (does not block the batch)"
                        );
                    }
                    out.had_transport_error = true;
                    continue;
                }
                tracing::warn!(
                    session = %rec.session_id,
                    seq = rec.seq,
                    kind = %rec.event_kind,
                    status = ?fail_status,
                    error = %snippet(&e),
                    "coord_sync: push failed; this session backs off and retries \
                     (other sessions keep draining)"
                );
                out.had_transport_error = true;
                out.blocked_on = Some(e);
                // Stop THIS chain — preserves (session, seq) order on retry.
                // The unACKed tail stays in the file for a later tick.
                if trip_abort {
                    abort.store(true, Ordering::Relaxed);
                }
                break;
            }
            PushOutcome::PermanentFailure(reason) => {
                // We treat 4xx (other than 409) as "the runner sent
                // a bad record that coord refuses". ACK it locally
                // so the queue moves forward — the dashboard will
                // miss this event but the session itself isn't
                // hostage to a corrupt row. Logged with the status and a
                // bounded body (a 401 names its cause — "operator context
                // missing" — which is the whole diagnosis), because a dropped
                // `started` row is a session coord will never know about.
                let (fail_kind, fail_status) = classify_push_failure(&reason, true);
                note_outbox_failure(fail_kind, fail_status);
                tracing::error!(
                    session = %rec.session_id,
                    seq = rec.seq,
                    kind = %rec.event_kind,
                    status = ?fail_status,
                    body = %snippet(&reason),
                    "coord_sync: coord refused the row (4xx) — ACK-dropping it locally"
                );
                out.succeeded.push((rec.session_id, rec.seq));
                out.cleared.push((rec.session_id, rec.seq));
            }
        }
    }

    out.attempts = attempts;
    out
}

/// Past this size the quarantine sidecar stops growing: further quarantined
/// rows are dropped with a `warn!` rather than filling the disk.
const QUARANTINE_MAX_BYTES: u64 = 16 * 1024 * 1024;

/// Per-session drain state carried across ticks by [`run_drain_loop`].
#[derive(Default)]
struct DrainState {
    /// Best-effort retry budget for the `is_best_effort_kind` records, keyed by
    /// (session_id, seq). In-memory by design: a restart resets the budget,
    /// which only re-grants retries — never duplicates (coord POST is the side
    /// effect, and an unacked record retries anyway).
    best_effort_attempts: HashMap<(Uuid, i64), u32>,
    /// Sessions whose chain is failing, with their own backoff. Pruned every
    /// tick to the sessions that still have pending rows.
    retry: HashMap<Uuid, SessionRetry>,
    /// Sessions whose rows go to the quarantine sidecar instead of coord.
    /// In-memory: a restart gives a quarantined session one fresh budget.
    /// Kept (not pruned) so a quarantined session's LATER rows follow its
    /// earlier ones instead of re-earning a retry budget; bounded by the
    /// number of poisoned sessions this process has seen.
    quarantined: HashSet<Uuid>,
    /// Quarantined rows already written to the sidecar whose outbox ACK has
    /// not landed yet — so a failed ACK never appends the same row twice.
    in_sidecar: HashSet<(Uuid, i64)>,
    /// When coord last took a row from this drain.
    last_ack: Option<Instant>,
    /// Ticks in which coord took at least one row. Strictly increasing.
    ack_ticks: u64,
}

/// What one drain tick did, for the loop's sleep decision.
struct TickResult {
    /// A push failed and coord took NOTHING — the outage posture.
    outage: bool,
    /// Nothing was pending (or the outbox could not be read) — idle cadence.
    idle: bool,
    /// At least one chain issued a push. A tick where every pending session
    /// sat out its own backoff did nothing, and must not reset the loop's
    /// outage backoff.
    ran_any: bool,
}

impl TickResult {
    fn idle() -> Self {
        Self {
            outage: false,
            idle: true,
            ran_any: false,
        }
    }
}

/// Move `rows` to the quarantine sidecar. Returns the keys that may now be
/// ACKed out of the outbox (written now, written by an earlier tick, or
/// dropped because the sidecar is at its cap).
fn quarantine_rows(
    inner: &CoordSyncInner,
    state: &mut DrainState,
    rows: Vec<OutboxRecord>,
) -> Vec<(Uuid, i64)> {
    let mut done: Vec<(Uuid, i64)> = Vec::new();
    let fresh: Vec<OutboxRecord> = rows
        .into_iter()
        .filter(|r| {
            let key = (r.session_id, r.seq);
            if state.in_sidecar.contains(&key) {
                done.push(key);
                false
            } else {
                true
            }
        })
        .collect();
    if fresh.is_empty() {
        return done;
    }
    let path = quarantine_path(&inner.outbox);
    let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    if size >= QUARANTINE_MAX_BYTES {
        tracing::warn!(
            rows = fresh.len(),
            path = %path.display(),
            "coord_sync: quarantine sidecar is at its cap — dropping quarantined rows"
        );
        done.extend(fresh.iter().map(|r| (r.session_id, r.seq)));
        return done;
    }
    match append_quarantine(&inner.outbox, &fresh) {
        Ok(()) => {
            for r in &fresh {
                state.in_sidecar.insert((r.session_id, r.seq));
                done.push((r.session_id, r.seq));
            }
        }
        Err(e) => tracing::warn!(
            error = %e,
            path = %path.display(),
            "coord_sync: quarantine sidecar write failed — rows stay in the outbox"
        ),
    }
    done
}

/// One drain pass over the outbox. Split out of [`run_drain_loop`] so a test
/// can drive ticks deterministically.
async fn drain_tick(inner: &Arc<CoordSyncInner>, state: &mut DrainState) -> TickResult {
    // Read the pending rows and the held set as ONE snapshot, under the held
    // lock (order: held -> the outbox's write lock). Reading them separately
    // let a confirmation finish in between: the row was pending, the hold was
    // already gone, and the drain re-POSTed a row coord had just confirmed —
    // whose 409 flipped a healthy session to PendingResolution.
    let (pending, held) = {
        let held = match inner.held.lock() {
            Ok(h) => h,
            Err(_) => return TickResult::idle(),
        };
        match inner.outbox.pending() {
            Ok(p) => (p, held.clone()),
            Err(e) => {
                tracing::warn!(error = %e, "coord_sync: outbox pending() failed");
                return TickResult::idle();
            }
        }
    };
    let pending_sessions: HashSet<Uuid> = pending.iter().map(|r| r.session_id).collect();
    state.retry.retain(|sid, _| pending_sessions.contains(sid));
    if pending.is_empty() {
        let quarantined = state.quarantined.len() as u64;
        with_outbox_health(|h| {
            h.observed_at = Some(Utc::now());
            h.pending = 0;
            h.oldest_unacked_at = None;
            h.retrying_sessions = 0;
            h.quarantined_sessions = quarantined;
        });
        return TickResult::idle();
    }
    let now = Instant::now();
    // Coord took a row since a session's failure iff `ack_ticks` has moved past
    // the value that failure recorded — the evidence that the session is
    // failing on its own row, not because coord is down. An outage moves no
    // counter, so it can never quarantine anything.
    let ack_ticks_at_start = state.ack_ticks;

    // (key, recorded_at) of every pending row, for the health snapshot.
    let pending_meta: Vec<((Uuid, i64), DateTime<Utc>)> = pending
        .iter()
        .map(|r| ((r.session_id, r.seq), r.recorded_at))
        .collect();

    // Group into per-session chains. `pending()` returns records sorted by
    // (session_id, seq), so each chain is already in seq order.
    let mut chains: Vec<Vec<OutboxRecord>> = Vec::new();
    let mut to_quarantine: Vec<OutboxRecord> = Vec::new();
    for rec in pending {
        if held.contains(&rec.session_id) {
            continue;
        }
        if state.quarantined.contains(&rec.session_id) {
            to_quarantine.push(rec);
            continue;
        }
        match chains.last_mut() {
            Some(chain) if chain[0].session_id == rec.session_id => chain.push(rec),
            _ => chains.push(vec![rec]),
        }
    }

    // Quarantined sessions' rows land in the sidecar, then leave the outbox.
    let mut succeeded: Vec<(Uuid, i64)> = quarantine_rows(inner, state, to_quarantine);

    // A session still inside its own backoff sits this tick out.
    chains.retain(|chain| {
        state
            .retry
            .get(&chain[0].session_id)
            .is_none_or(|r| r.next_attempt_at <= now)
    });
    let ran_any = !chains.is_empty();

    let abort = Arc::new(AtomicBool::new(false));
    let outcomes: Vec<ChainOutcome> = futures::stream::iter(chains.into_iter().map(|chain| {
        let sid = chain[0].session_id;
        let session_attempts: HashMap<(Uuid, i64), u32> = state
            .best_effort_attempts
            .iter()
            .filter(|((s, _), _)| *s == sid)
            .map(|(k, v)| (*k, *v))
            .collect();
        let trip_abort = !state
            .retry
            .get(&sid)
            .is_some_and(|r| state.ack_ticks > r.failed_at_ack_tick);
        push_chain(
            inner.clone(),
            chain,
            session_attempts,
            abort.clone(),
            trip_abort,
        )
    }))
    .buffer_unordered(MAX_CONCURRENT_PUSH_CHAINS)
    .collect()
    .await;

    let mut had_transport_error = false;
    let mut delivered = false;
    let mut blocked: Vec<(Uuid, String)> = Vec::new();
    for outcome in outcomes {
        delivered |= outcome.acked_by_coord;
        succeeded.extend(outcome.succeeded);
        had_transport_error |= outcome.had_transport_error;
        for (key, attempts) in outcome.attempts {
            state.best_effort_attempts.insert(key, attempts);
        }
        // Cleared wins over the carried counters: a record that delivered
        // (or spent its budget) forgets its retry count, as before.
        for key in outcome.cleared {
            state.best_effort_attempts.remove(&key);
        }
        match outcome.blocked_on {
            Some(err) => blocked.push((outcome.session_id, err)),
            None if !outcome.aborted => {
                // Ran to the end: whatever blocked it before has cleared.
                state.retry.remove(&outcome.session_id);
            }
            None => {}
        }
    }

    if delivered {
        state.last_ack = Some(now);
        state.ack_ticks += 1;
    }
    let last_ack = state.last_ack;
    for (sid, err) in blocked {
        // First failure: coord counts as serving if it took a row within the
        // window. Later failures: only if it took one since THIS session's
        // previous failure (this tick's rows included).
        let serving = match state.retry.get(&sid) {
            Some(r) => state.ack_ticks > r.failed_at_ack_tick,
            None => last_ack.is_some_and(|t| now.saturating_duration_since(t) <= SERVING_WINDOW),
        };
        let entry = state.retry.entry(sid).or_insert(SessionRetry {
            failures: 0,
            serving_failures: 0,
            next_attempt_at: now,
            failed_at_ack_tick: ack_ticks_at_start,
        });
        entry.failures += 1;
        entry.failed_at_ack_tick = ack_ticks_at_start;
        // A 429 on the one kind that retries it (`output_chunk`) is coord
        // pacing this runner, not refusing the row — it never counts toward
        // quarantine.
        let rate_limited = classify_push_failure(&err, false).0 == "rate_limited";
        if serving && !rate_limited {
            entry.serving_failures += 1;
        }
        entry.next_attempt_at = now + SessionRetry::backoff(entry.failures);
        if entry.serving_failures >= QUARANTINE_AFTER_SERVING_FAILURES {
            tracing::warn!(
                session = %sid,
                failures = entry.failures,
                last_error = %snippet(&err),
                sidecar = %quarantine_path(&inner.outbox).display(),
                "coord_sync: session QUARANTINED — its head row kept failing while coord \
                 accepted other sessions' rows; its rows move to the sidecar and are no \
                 longer retried (a runner restart retries it once more)"
            );
            state.retry.remove(&sid);
            state.quarantined.insert(sid);
        }
    }

    if !succeeded.is_empty() {
        match inner.outbox.ack(&succeeded) {
            Ok(()) => {
                for key in &succeeded {
                    state.in_sidecar.remove(key);
                }
            }
            Err(e) => tracing::warn!(error = %e, "coord_sync: ack write failed"),
        }
    }
    if delivered {
        inner.has_been_online.store(true, Ordering::Relaxed);
        note_outbox_ack();
    }

    // Health snapshot: what is still undelivered after this tick.
    let acked: HashSet<(Uuid, i64)> = succeeded.iter().copied().collect();
    let remaining: Vec<&DateTime<Utc>> = pending_meta
        .iter()
        .filter(|(k, _)| !acked.contains(k))
        .map(|(_, at)| at)
        .collect();
    let retrying = state.retry.len() as u64;
    let quarantined = state.quarantined.len() as u64;
    with_outbox_health(|h| {
        h.observed_at = Some(Utc::now());
        h.pending = remaining.len() as u64;
        h.oldest_unacked_at = remaining.iter().min().map(|at| **at);
        h.retrying_sessions = retrying;
        h.quarantined_sessions = quarantined;
    });

    TickResult {
        // An outage is a failure that TRIPPED the abort (a fresh failure, or
        // one with no coord-taken row since the last) while coord took
        // nothing. A lone session failing on its own row does not trip it, so
        // it can no longer push the whole loop into the 60 s backoff and
        // delay every healthy row behind it.
        outage: abort.load(Ordering::Relaxed) && !delivered && had_transport_error,
        idle: false,
        ran_any,
    }
}

async fn run_drain_loop(inner: Arc<CoordSyncInner>) {
    tracing::info!(
        coord_url = %inner.coord_url,
        "coord_sync: drain loop starting"
    );
    let mut backoff = TICK_BUSY;
    let mut state = DrainState::default();
    loop {
        let tick = drain_tick(&inner, &mut state).await;
        if tick.idle {
            backoff = TICK_BUSY;
            tokio::time::sleep(TICK_IDLE).await;
        } else if tick.outage {
            tokio::time::sleep(backoff).await;
            backoff = std::cmp::min(backoff * 2, MAX_BACKOFF);
        } else if !tick.ran_any {
            // Every pending session is sitting out its own backoff: nothing
            // happened, so nothing is learned — keep the outage backoff where
            // it is rather than resetting it to the busy cadence.
            tokio::time::sleep(std::cmp::max(backoff, TICK_BUSY)).await;
        } else {
            backoff = TICK_BUSY;
            tokio::time::sleep(TICK_BUSY).await;
        }
    }
}

/// Stamp `finish_synced` back onto the local record once coord has ACKed a
/// `finished` row. A record whose payload carries no `claude_session_id` (none
/// is written without one) is skipped rather than guessed at.
fn notify_finished_ack(inner: &CoordSyncInner, rec: &OutboxRecord) {
    if rec.event_kind != SessionEventKind::Finished.as_str() {
        return;
    }
    let Some(obs) = inner.finished_ack_observer.get() else {
        return;
    };
    match rec
        .payload
        .get("claude_session_id")
        .and_then(|v| v.as_str())
    {
        Some(csid) => (obs.0)(
            csid,
            rec.payload.get("finished_at").and_then(|v| v.as_i64()),
        ),
        None => tracing::warn!(
            session = %rec.session_id,
            seq = rec.seq,
            "coord_sync: finished record ACKed without a claude_session_id — cannot mark synced"
        ),
    }
}

#[derive(Debug)]
enum PushOutcome {
    /// Coord accepted the row (2xx).
    Acked,
    /// Coord returned 409 on `POST /sessions` — a peer holds the row.
    /// The runner flips the session to `PendingResolution` and surfaces
    /// the conflict to the frontend.
    Conflict { row: Option<JsonValue> },
    /// Network / 5xx / timeout. Re-try next tick.
    Transport(String),
    /// 4xx (other than 409) — coord refuses this payload permanently.
    /// ACK locally so the queue moves forward.
    PermanentFailure(String),
}

async fn push_record(inner: &Arc<CoordSyncInner>, rec: &OutboxRecord) -> PushOutcome {
    let base = inner.coord_url.trim_end_matches('/');
    let kind = rec.event_kind.as_str();

    // Phase 8b (plan 2026-07-02-session-scoped-multi-tenant-device-binding
    // §D4) — per-session credential selection: every push presents the
    // OWNING SESSION's device-JWT slot.
    //
    // Phase 5 of `2026-08-29-runner-work-scoped-writes-default-tenant-credential`
    // typed the unknown arm. Every outbox record belongs to a session, so an
    // unknown tenant here (pre-8b rows, registry gone after a restart, a
    // session stamped before any default existed) is a resolution FAILURE, not
    // "this route has no tenant" — `TenantScope::Unresolved`. That keeps the
    // default slot on a single-bound device, byte-identical to the old
    // behaviour, and degrades to unauthenticated on a multi-bound one instead
    // of filing another tenant's session row.
    let scope = record_session_tenant(inner, rec);

    let result = match kind {
        "started" => {
            // POST /sessions with the full create body. The runner
            // already stamped the row's id + intent into the payload at
            // session start; we just forward it.
            let body = rebuild_create_body(rec);
            let url = format!("{base}/sessions");
            crate::auth::attach_device_auth_for(inner.http.post(&url).json(&body), scope)
                .send()
                .await
        }
        "heartbeat" => {
            let url = format!("{base}/sessions/{}", rec.session_id);
            let body = json!({ "heartbeat": true });
            crate::auth::attach_device_auth_for(inner.http.patch(&url).json(&body), scope)
                .send()
                .await
        }
        "state_change" => {
            let url = format!("{base}/sessions/{}", rec.session_id);
            // The payload carries whatever fields the runner changed —
            // forward the subset coord understands.
            let body = state_change_body(&rec.payload);
            crate::auth::attach_device_auth_for(inner.http.patch(&url).json(&body), scope)
                .send()
                .await
        }
        "closed" => {
            let url = format!("{base}/sessions/{}", rec.session_id);
            crate::auth::attach_device_auth_for(inner.http.delete(&url), scope)
                .send()
                .await
        }
        "claim_stolen" => {
            let url = format!("{base}/sessions/{}/steal", rec.session_id);
            let body = steal_body(rec);
            crate::auth::attach_device_auth_for(inner.http.post(&url).json(&body), scope)
                .send()
                .await
        }
        "progress" => {
            // Work-progress report (plan
            // 2026-06-25-session-progress-reporting-and-agent-session-linkage.md).
            // PATCH /sessions/:id {progress:{…}} advances
            // coord.sessions.last_progress_at on a work-activity boundary —
            // orthogonal to the {heartbeat:true} liveness PATCH above. Coord
            // stamps last_progress_at=now() when the body omits it.
            let url = format!("{base}/sessions/{}", rec.session_id);
            let body = progress_body(&rec.payload);
            crate::auth::attach_device_auth_for(inner.http.patch(&url).json(&body), scope)
                .send()
                .await
        }
        "finished" => {
            // Session FINISHED marker (plan
            // 2026-09-01-session-finished-marker-and-unfinished-resume, Phase 2).
            // Rides the SAME PATCH /sessions/:id door as "progress" above, and
            // that choice is load-bearing: the target session is the URL PATH,
            // so this rung cannot hit the trap that makes an omitted
            // `claude_code_session_id` on `coord_report_status` land the write
            // on whichever of this DEVICE's sessions started most recently —
            // i.e. usually a peer's, on a box running many sessions.
            //
            // `session_status = "finished"` is terminal on coord's work axis and
            // gates prompting; no coord production writer moves a row off it.
            let url = format!("{base}/sessions/{}", rec.session_id);
            let body = json!({
                "progress": { "session_status": "finished" },
            });
            crate::auth::attach_device_auth_for(inner.http.patch(&url).json(&body), scope)
                .send()
                .await
        }
        "helper_task_created" => {
            // Helper Task Queue (plan 2026-06-29-helper-task-queue, Phase 1.3).
            // The payload is the full CreateHelperTaskRequest body recorded by
            // HelperTaskRegistrar — forward it verbatim. Phase 8b: this is a
            // SESSION-scoped data-plane push (the helper task is created by an
            // owning session), so it presents that session's device-JWT slot
            // via `attach_device_auth_for(scope)` like every other
            // per-session arm above — an `Unresolved` scope keeps the default
            // slot on a single-bound device, byte-identical to the pre-8b bare
            // `attach_device_auth`.
            // Responses take the dedicated best-effort path in
            // `helper_task_outcome` (201 provenance capture, the
            // `helper_task_queue_unavailable` 503 drop, bounded retry for
            // everything else).
            let url = format!("{base}/coord/helper-tasks");
            crate::auth::attach_device_auth_for(inner.http.post(&url).json(&rec.payload), scope)
                .send()
                .await
        }
        "gate_registration" => {
            // Closeout gate registration (plan
            // 2026-08-28-closeout-has-no-durable-store-when-the-runner-is-offline,
            // Phase 2). POST /coord/work-units/:slug/register-gate with the
            // register-gate body rebuilt from the payload; the slug rides IN
            // the payload because it is a PATH segment, not a body field.
            // A payload with no usable slug can never succeed — fail it
            // permanently here rather than burning the retry budget on a URL
            // that cannot be built.
            let slug = match gate_registration_slug(&rec.payload) {
                Some(s) => s,
                None => {
                    return PushOutcome::PermanentFailure(
                        "gate_registration payload carries no usable `work_unit_slug` \
                         (absent, empty, or containing a path separator) — the \
                         register-gate URL cannot be built"
                            .to_string(),
                    )
                }
            };
            let url = format!("{base}/coord/work-units/{slug}/register-gate");
            let body = gate_registration_body(&rec.payload);
            let mut rb =
                crate::auth::attach_device_auth_for(inner.http.post(&url).json(&body), scope);
            // The author the live forward would have sent, recorded by the
            // write forwarder when it spooled the row. coord's
            // `register_unit_gate` stamps it only if `session_on_device` binds
            // it to this device — closed or not, deliberately, so a session
            // reaped while still running keeps its provenance — and otherwise
            // records NULL. It never refuses the gate over this header, so a
            // replay cannot be lost to it.
            rb = with_gate_caller_session(rb, &rec.payload);
            rb.send().await
        }
        "finding_posted" => {
            // Closeout finding (same plan/phase). POST /coord/agent-findings
            // with the payload forwarded VERBATIM — coord's `PostFindingBody`
            // is `deny_unknown_fields` and rejects the three identity fields
            // BY NAME, so there is nothing here to reshape: either the
            // producer recorded a valid body or coord answers 400, which the
            // generic arm below turns into an Ack-drop (a retry could never
            // fix a body the queue cannot edit).
            let url = format!("{base}/coord/agent-findings");
            crate::auth::attach_device_auth_for(inner.http.post(&url).json(&rec.payload), scope)
                .send()
                .await
        }
        "agent_notification" => {
            // Sensitive-action notification (plan
            // 2026-09-18-notifications-are-agent-actions-and-alerts-are-agent-work,
            // Phase 9). POST /coord/agent-notifications with the payload
            // forwarded verbatim — it IS the body. Its own function because a
            // 400 naming the `undo` field earns exactly one retry without it.
            return agent_notification_push(inner, rec, scope, base).await;
        }
        "commit_report" => {
            // Commit ↔ session lineage push-report (plan
            // 2026-06-07-coord-commit-session-lineage.md, Population path 2).
            // Body is the payload verbatim ({repo, branch, shas}); coord
            // resolves the session server-side from (repo, branch). Tenant
            // comes from the X-Qontinui-Tenant-Id header (post_device_register
            // posture) — Phase 8b: the OWNING SESSION's binding wins; the
            // machine.json default only backfills tenant-less legacy rows.
            let url = format!("{base}/coord/commits/report");
            let mut rb = crate::auth::attach_device_auth_for(
                inner.http.post(&url).json(&rec.payload),
                scope,
            );
            if let Some(tid) = scope
                .declared_tenant()
                .or_else(crate::session::dual_write::resolve_active_tenant_id)
            {
                rb = rb.header("X-Qontinui-Tenant-Id", tid.to_string());
            }
            rb.send().await
        }
        "output_chunk" => {
            // Transcript/output chunk (plan
            // 2026-07-09-runner-session-history-cloud-sync §3.2). POST
            // /sessions/:id/output {chunk_offset, payload_b64, stream}.
            // Unlike the lossy live PTY pipe (`output_pipe.rs`, direct POST,
            // drops on 429/5xx), these chunks ride the outbox for
            // at-least-once delivery — coord's warm tier is idempotent on
            // (session_id, stream, chunk_offset), so a replay is a no-op.
            let url = format!("{base}/sessions/{}/output", rec.session_id);
            let body = output_chunk_body(&rec.payload);
            crate::auth::attach_device_auth_for(inner.http.post(&url).json(&body), scope)
                .send()
                .await
        }
        "restore-record" => {
            // Restore-registry mirror (plan
            // 2026-07-09-runner-session-history-cloud-sync §3.4, Phase 4).
            // POST /sessions/:id/events {seq, event_kind, payload} — coord's
            // session-events ingest stores event_kind verbatim in
            // coord.session_events, idempotent on (session_id, seq), so the
            // outbox can replay freely. The payload is the binding
            // {provider, authoritative_session_id, cwd, launch_command,
            // restore_tier, machine_id} contract the web UI reads.
            let url = format!("{base}/sessions/{}/events", rec.session_id);
            let body = json!({
                "seq": rec.seq,
                "event_kind": rec.event_kind,
                "payload": rec.payload,
            });
            crate::auth::attach_device_auth_for(inner.http.post(&url).json(&body), scope)
                .send()
                .await
        }
        "coord-transport-rung" => {
            // WHICH transport rung carried one coord call (plan
            // 2026-09-07-no-per-session-record-of-which-transport-rung-carried-
            // a-coord-read, Phase 1). Same ingest and same body shape as
            // "restore-record" above: POST /sessions/:id/events
            // {seq, event_kind, payload}, which stores event_kind verbatim in
            // coord.session_events and is idempotent on (session_id, seq), so
            // the outbox may replay freely.
            //
            // ⚠️ THIS ARM IS LOAD-BEARING. Without it the kind falls to the
            // `other` catch-all below, which ACKs and DROPS at debug level:
            // the row would be written durably, drained, silently discarded
            // and acked as delivered, and
            // success_metric/coord-mcp-first-rung-reachability would read a
            // clean zero with nothing erroring anywhere. That is the exact
            // failure the phase exists to prevent — see
            // `drain_pushes_coord_transport_rung_to_events_endpoint`, which
            // fails if this arm is removed.
            let url = format!("{base}/sessions/{}/events", rec.session_id);
            let body = json!({
                "seq": rec.seq,
                "event_kind": rec.event_kind,
                "payload": rec.payload,
            });
            crate::auth::attach_device_auth_for(inner.http.post(&url).json(&body), scope)
                .send()
                .await
        }
        other => {
            // HandoffRequest is Phase 7 — defined now for wire shape, not
            // pushed yet. Quietly ACK so the file doesn't grow.
            tracing::debug!(
                kind = %other,
                "coord_sync: event kind not yet pushed to coord — ACKing"
            );
            return PushOutcome::Acked;
        }
    };

    match result {
        Ok(resp) => {
            if kind == "helper_task_created" {
                return helper_task_outcome(rec, resp).await;
            }
            if kind == "gate_registration" {
                return gate_registration_outcome(inner, rec, resp, scope).await;
            }
            if kind == "finding_posted" {
                return finding_outcome(rec, resp).await;
            }
            let status = resp.status();
            if kind == "output_chunk" && status == StatusCode::TOO_MANY_REQUESTS {
                let detail = resp.text().await.unwrap_or_default();
                if detail.contains("warm_quota_exceeded") {
                    // Tenant warm-quota exceeded (gate 2, enforced
                    // coord-side). Retrying can't help until quota frees,
                    // and stalling the batch would head-of-line-block this
                    // session's lifecycle events — ACK-drop with an info
                    // line instead of the error-level PermanentFailure path.
                    tracing::info!(
                        session = %rec.session_id,
                        seq = rec.seq,
                        "coord_sync: output chunk rejected by warm quota (429) — dropping"
                    );
                    return PushOutcome::Acked;
                }
                // Any other 429 (proxy / edge rate limit) is transient —
                // keep the row and retry, same as a 5xx.
                return PushOutcome::Transport(format!("{status}: {detail}"));
            }
            if status.is_success() {
                return PushOutcome::Acked;
            }
            if matches!(kind, "restore-record" | "coord-transport-rung")
                && matches!(
                    status,
                    StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED
                )
            {
                // Coord build without the session-events ingest route (the
                // coord slice of Phase 4 ships it in parallel), or a
                // `coord.sessions` row that has since been GC'd — the ingest
                // 404s on an unknown :id rather than raising the raw FK
                // violation. Both mirrors are best-effort observability, so
                // drop quietly instead of error-spamming the PermanentFailure
                // path. Note restore-record's emitter debounce map already
                // counted the record as emitted, so an UNCHANGED record will
                // not re-emit until the runner restarts or the record
                // materially changes — acceptable for a mirror whose readers
                // always take the newest event. `coord-transport-rung` has no
                // debounce: it is one row per proxied call, so the next call
                // simply produces the next row.
                if kind == "coord-transport-rung" {
                    // This mirror IS the population of
                    // `success_metric/coord-mcp-first-rung-reachability`, so a
                    // quiet `info` drop is exactly how "40 rows out of 4000
                    // calls" becomes invisible. `warn` instead, and name WHICH
                    // of the two causes it was — they need opposite fixes.
                    //
                    // The two causes get DIFFERENT log cadences, because they
                    // are different KINDS of fact and this kind has no
                    // debounce (one row per proxied call):
                    //
                    //   * 405 is a PROCESS-WIDE, persistent fact — the serving
                    //     coord has no session-events ingest route, so EVERY
                    //     row 405s until that coord slice lands. Warning once
                    //     per call would emit thousands of identical lines and
                    //     dominate the 15 other `warn!` sites in this file,
                    //     burying them in `.dev-logs` — the fleet's first
                    //     debugging surface and what `/review-logs` consumes.
                    //     So: first occurrence, then every 1000th, carrying the
                    //     running total so the under-count stays quantified.
                    //   * 404 is PER-SESSION and should be genuinely rare (a
                    //     lane resolved, but coord does not know that session
                    //     id — a wrong lane, or a GC'd `coord.sessions` row).
                    //     Per-occurrence detail is what makes it diagnosable,
                    //     so it is NOT throttled.
                    //
                    // Both are counted at module level
                    // (`TRANSPORT_RUNG_DROPPED_404` / `_405`) so `GET /health`
                    // `transportRung.drainDropped` carries the totals a log
                    // grep used to be the only way to reach.
                    if status == StatusCode::METHOD_NOT_ALLOWED {
                        let n = TRANSPORT_RUNG_DROPPED_405.fetch_add(1, Ordering::Relaxed) + 1;
                        if n == 1 || n % 1000 == 0 {
                            tracing::warn!(
                                dropped_total = n,
                                kind = %kind,
                                status = %status,
                                "coord_sync: transport-rung rows dropped — coord \
                                 build lacks the session-events ingest route; \
                                 first-rung reachability under-counts"
                            );
                        }
                        return PushOutcome::Acked;
                    }
                    TRANSPORT_RUNG_DROPPED_404.fetch_add(1, Ordering::Relaxed);
                    tracing::warn!(
                        session = %rec.session_id,
                        seq = rec.seq,
                        kind = %kind,
                        status = %status,
                        cause = "coord does not know this session id (404 — wrong \
                                 lane, or the coord.sessions row was GC'd)",
                        "coord_sync: transport-rung row dropped — the first-rung \
                         reachability metric will under-count"
                    );
                    return PushOutcome::Acked;
                }
                tracing::info!(
                    session = %rec.session_id,
                    seq = rec.seq,
                    kind = %kind,
                    status = %status,
                    "coord_sync: session-events ingest unavailable — dropping mirror event"
                );
                return PushOutcome::Acked;
            }
            if status == StatusCode::CONFLICT {
                // Plan §Phase 3 conflict-on-acquire. Only POST
                // /sessions carries semantic meaning for 409; for
                // PATCH/DELETE it means "row gone or already in target
                // state", which we treat as success (idempotent).
                if kind == "started" {
                    let row = resp.json::<JsonValue>().await.ok();
                    return PushOutcome::Conflict { row };
                }
                return PushOutcome::Acked;
            }
            let detail = resp.text().await.unwrap_or_default();
            // 4xx → Ack-drop; 5xx → transient. Shared classifier.
            let outcome = write_failure_outcome(status, format!("{status}: {detail}"));
            if kind == "coord-transport-rung" && matches!(outcome, PushOutcome::PermanentFailure(_))
            {
                // The third drain-drop arm for this mirror (404 and 405 are
                // counted above): without it `/health` `transportRung`
                // could read all-zero drops while rows vanished at `error!`.
                TRANSPORT_RUNG_DROPPED_OTHER_4XX.fetch_add(1, Ordering::Relaxed);
            }
            outcome
        }
        // The FULL source chain: reqwest's top-level Display is only "error
        // sending request for url (…)", which names no cause — the connect /
        // TLS / DNS / timeout reason lives in `.source()`.
        Err(e) => PushOutcome::Transport(transport_error(&e)),
    }
}

/// A reqwest transport failure rendered with its FULL source chain
/// ([`crate::util::error_chain::error_chain`]) and a leading `[timeout]` /
/// `[connect]` tag, so a log line and `/health` `sessionOutbox.lastFailure`
/// say WHICH transport fault it was instead of reqwest's generic
/// "error sending request for url (…)".
fn transport_error(e: &reqwest::Error) -> String {
    let chain = crate::util::error_chain::error_chain(e);
    if e.is_timeout() {
        format!("[timeout] {chain}")
    } else if e.is_connect() {
        format!("[connect] {chain}")
    } else {
        chain
    }
}

/// Resolve which tenant binding OWNS an outbox record's session — the slot
/// selector for the per-session credential seam (Phase 8b, plan §D4).
///
/// Resolution order:
/// 1. The record payload — the `started` create body carries the full
///    intent (whose `tenant_id` the registry stamped at creation), and a
///    top-level `tenant_id` is honored for future event kinds.
/// 2. The live [`SessionRegistry`] record for `rec.session_id` — thin
///    payloads (heartbeat / state_change / closed) carry no intent, but the
///    registry still holds the session's stamped tenant while it's alive.
/// 3. [`TenantScope::Unresolved`] — replayed rows for sessions the registry no
///    longer holds (post-restart) and pre-8b rows. A single-bound device still
///    presents the default slot (the pre-8b behavior, so nothing regresses); a
///    multi-bound one degrades to unauthenticated rather than filing the row
///    under whichever tenant happens to be default.
fn record_session_tenant(inner: &Arc<CoordSyncInner>, rec: &OutboxRecord) -> TenantScope {
    let from_payload = rec
        .payload
        .get("intent")
        .and_then(|i| i.get("tenant_id"))
        .or_else(|| rec.payload.get("tenant_id"))
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s.trim()).ok());
    if let Some(t) = from_payload {
        return TenantScope::Owned(t);
    }
    session_tenant_by_id(inner, rec.session_id)
}

/// The tenant that OWNS a session, read from the live registry.
///
/// The registry arm of [`record_session_tenant`], split out because two
/// callers need a tenant from a bare `session_id` with no outbox record in
/// hand: [`CoordSync::probe_resume`] and the output pipe.
///
/// [`TenantScope::Unresolved`] whenever the registry is gone (post-restart),
/// the session is unknown, or its intent carries no tenant. It is never
/// [`TenantScope::Device`]: a session row HAS an owning tenant, so failing to
/// find it is a failure, and the D2 degrade is what should decide the outcome.
fn session_tenant_by_id(inner: &Arc<CoordSyncInner>, session_id: Uuid) -> TenantScope {
    TenantScope::for_session(
        inner
            .registry
            .lock()
            .expect("coord_sync registry slot poisoned")
            .as_ref()
            .and_then(Weak::upgrade)
            .and_then(|reg| reg.describe_by_id(session_id).ok())
            .and_then(|d| d.intent.tenant_id),
    )
}

/// Response handling for `helper_task_created` POSTs — best-effort posture.
///
/// - **2xx** — the 201 body is the created `HelperTask` (camelCase JSON with
///   `id` / `appId` / `source.pageId`); its provenance is recorded into the
///   persisted helper-task store so reflection verdict lines can name the
///   page/app instead of an unactionable coord task UUID. The store is the
///   `helper_tasks` module's process-global (`OnceLock`) store, so no state
///   handle needs plumbing into the drain.
/// - **503 with body `helper_task_queue_unavailable`** — coord's helper-task
///   tables aren't migrated; retrying can't help until the operator runs
///   migrations, so the record is Ack-dropped with a warn.
/// - **any other 503 (ELB/deploy blip), 5xx** — `Transport`, which the drain
///   loop maps to a bounded per-record retry that never breaks the batch.
/// - **4xx** — `PermanentFailure` (Ack-drop), same as other kinds.
async fn helper_task_outcome(rec: &OutboxRecord, resp: reqwest::Response) -> PushOutcome {
    let status = resp.status();
    if status.is_success() {
        match resp.json::<JsonValue>().await {
            Ok(body) => {
                let task_id = body.get("id").and_then(JsonValue::as_str);
                let app_id = body.get("appId").and_then(JsonValue::as_str);
                let page_id = body
                    .pointer("/source/pageId")
                    .and_then(JsonValue::as_str)
                    .map(str::to_string);
                if let (Some(task_id), Some(app_id)) = (task_id, app_id) {
                    crate::helper_tasks::record_task_metadata(task_id, app_id, page_id);
                } else {
                    tracing::debug!(
                        session = %rec.session_id,
                        seq = rec.seq,
                        "coord_sync: helper-task 201 body missing id/appId — provenance skipped"
                    );
                }
            }
            Err(e) => tracing::debug!(
                session = %rec.session_id,
                seq = rec.seq,
                error = %e,
                "coord_sync: helper-task 201 body not parseable — provenance skipped"
            ),
        }
        return PushOutcome::Acked;
    }
    let detail = resp.text().await.unwrap_or_default();
    if status == StatusCode::SERVICE_UNAVAILABLE {
        if detail.contains("helper_task_queue_unavailable") {
            tracing::warn!(
                session = %rec.session_id,
                seq = rec.seq,
                "coord_sync: POST /coord/helper-tasks returned 503 \
                 helper_task_queue_unavailable (tables not migrated) — dropping \
                 helper task event"
            );
            return PushOutcome::Acked;
        }
        // Transient 503 (ELB / deploy) — bounded-retry, don't drop the task.
        return PushOutcome::Transport(format!("{status}: {detail}"));
    }
    write_failure_outcome(status, format!("{status}: {detail}"))
}

/// Response handling for `finding_posted` POSTs.
///
/// Storage-wise this is the generic path — 2xx acks, 4xx is a
/// `PermanentFailure` (Ack-drop; the queue cannot edit a body coord refuses),
/// 5xx is `Transport` and gets the bounded best-effort retry. The one thing it
/// adds is DISCLOSURE of coord's graceful degradation: when the `coord_findings`
/// migration has not been applied, coord answers **200** with
/// `{"posted": false, "reason": …}` rather than an error. That is a 2xx, so the
/// generic arm would ack it silently and the finding would read as delivered
/// while nothing was stored. The record is still dropped — retrying cannot
/// apply a migration, the same call the helper-task `helper_task_queue_unavailable`
/// 503 makes — but it says so.
async fn finding_outcome(rec: &OutboxRecord, resp: reqwest::Response) -> PushOutcome {
    let status = resp.status();
    if status.is_success() {
        match resp.json::<JsonValue>().await {
            Ok(body) if body.get("posted").and_then(JsonValue::as_bool) == Some(false) => {
                tracing::warn!(
                    session = %rec.session_id,
                    seq = rec.seq,
                    reason = ?body.get("reason").and_then(JsonValue::as_str),
                    "coord_sync: coord accepted the closeout finding but did NOT store it \
                     (coord.findings not provisioned) — dropping"
                );
            }
            Ok(_) => tracing::info!(
                session = %rec.session_id,
                seq = rec.seq,
                "coord_sync: closeout finding posted"
            ),
            Err(e) => tracing::debug!(
                session = %rec.session_id,
                seq = rec.seq,
                error = %e,
                "coord_sync: agent-findings 2xx body not parseable — storage not confirmed"
            ),
        }
        return PushOutcome::Acked;
    }
    let detail = resp.text().await.unwrap_or_default();
    write_failure_outcome(status, format!("{status}: {detail}"))
}

/// Push one `agent_notification` row to `POST /coord/agent-notifications`.
///
/// ## The `undo` fallback (plan `2026-09-18-notifications-are-agent-actions-and-alerts-are-agent-work`)
///
/// The runner sends `undo` — the prior sha of a force-updated ref — because
/// that plan's Phase 4 adds it to coord's `AgentNotificationBody`. That coord
/// change ships in a PARALLEL pull request, and until it deploys coord's body
/// is `deny_unknown_fields` WITHOUT `undo`: the whole notification is a 400.
/// A 400 is a permanent failure to the drain, so without a fallback every
/// force-push notification would be dropped for the entire window between the
/// two deploys — losing the record of an action that already happened, which
/// is the failure this plan exists to prevent.
///
/// So: on a 400 whose body names an unknown `undo` field, retry ONCE with
/// `undo` removed. Only that exact refusal earns the retry; any other 400
/// (unknown action, empty artifact, a field too long) is a body the queue
/// cannot fix and is dropped as before. Once coord serves `undo`, the first
/// POST succeeds and the retry never runs, so this arm needs no removal to be
/// correct — it can be deleted once every coord this runner can talk to
/// carries the field.
async fn agent_notification_push(
    inner: &Arc<CoordSyncInner>,
    rec: &OutboxRecord,
    scope: TenantScope,
    base: &str,
) -> PushOutcome {
    let url = format!("{base}/coord/agent-notifications");
    let post = |body: &JsonValue| {
        crate::auth::attach_device_auth_for(inner.http.post(&url).json(body), scope).send()
    };

    let resp = match post(&rec.payload).await {
        Ok(r) => r,
        Err(e) => return PushOutcome::Transport(transport_error(&e)),
    };
    let status = resp.status();
    if status.is_success() {
        return agent_notification_acked(rec, resp).await;
    }
    let detail = resp.text().await.unwrap_or_default();
    if status == StatusCode::BAD_REQUEST
        && rec.payload.get("undo").is_some()
        && rejects_unknown_undo_field(&detail)
    {
        tracing::info!(
            session = %rec.session_id,
            seq = rec.seq,
            "coord_sync: coord predates the notification `undo` field — retrying once without it"
        );
        let mut stripped = rec.payload.clone();
        if let Some(obj) = stripped.as_object_mut() {
            obj.remove("undo");
        }
        let resp = match post(&stripped).await {
            Ok(r) => r,
            Err(e) => return PushOutcome::Transport(transport_error(&e)),
        };
        let status = resp.status();
        if status.is_success() {
            return agent_notification_acked(rec, resp).await;
        }
        let detail = resp.text().await.unwrap_or_default();
        return agent_notification_failure(rec, status, detail);
    }
    agent_notification_failure(rec, status, detail)
}

/// Whether a coord 400 body is the deserializer refusing an unknown `undo`
/// field. Coord answers `{"error":"invalid_body","detail":"… unknown field
/// `undo`, expected one of …"}`; the check reads `detail` when the body is
/// JSON and the raw text otherwise.
fn rejects_unknown_undo_field(body: &str) -> bool {
    let detail = serde_json::from_str::<JsonValue>(body)
        .ok()
        .and_then(|v| {
            v.get("detail")
                .and_then(JsonValue::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| body.to_string());
    detail.contains("unknown field `undo`")
}

/// A 2xx from the notifications door. Logs the line coord will show the
/// operator (`summary`), so the runner log carries what was actually said.
async fn agent_notification_acked(rec: &OutboxRecord, resp: reqwest::Response) -> PushOutcome {
    let summary = resp.json::<JsonValue>().await.ok().and_then(|b| {
        b.get("summary")
            .and_then(JsonValue::as_str)
            .map(str::to_string)
    });
    tracing::info!(
        session = %rec.session_id,
        seq = rec.seq,
        summary = summary.as_deref().unwrap_or("<no summary in response>"),
        "coord_sync: agent notification recorded"
    );
    PushOutcome::Acked
}

/// A refused notification. A 429 is coord's per-tenant fatigue bound: the
/// window reopens, so it is retried within the best-effort budget rather than
/// dropped on first sight like other 4xx. Everything else follows the shared
/// classifier (4xx permanent, 5xx transient).
fn agent_notification_failure(
    rec: &OutboxRecord,
    status: StatusCode,
    detail: String,
) -> PushOutcome {
    if status == StatusCode::TOO_MANY_REQUESTS {
        return PushOutcome::Transport(format!("{status}: {detail}"));
    }
    if status.is_client_error() {
        tracing::warn!(
            session = %rec.session_id,
            seq = rec.seq,
            status = %status,
            detail = %detail,
            "coord_sync: coord refused an agent notification; the action it reports already happened, so this line is its only remaining trace"
        );
    }
    write_failure_outcome(status, format!("{status}: {detail}"))
}

/// The work-unit slug a `gate_registration` payload names, validated as a
/// single URL path segment.
///
/// `None` when the key is absent, not a string, empty after trimming, or
/// carries anything that would change the request's shape rather than its
/// path segment (`/`, `?`, `#`, or whitespace). The runner has no
/// percent-encoder on this path, so refusing is the honest answer — and a
/// slug that cannot be a path segment can never succeed, which is why the
/// caller turns this into a `PermanentFailure` instead of a retry.
fn gate_registration_slug(payload: &JsonValue) -> Option<String> {
    let slug = payload.get("work_unit_slug")?.as_str()?.trim();
    if slug.is_empty()
        || slug.contains('/')
        || slug.contains('?')
        || slug.contains('#')
        || slug.chars().any(char::is_whitespace)
    {
        return None;
    }
    Some(slug.to_string())
}

/// The runner-resolved author a `gate_registration` payload carries under
/// [`super::closeout_spool::GATE_CALLER_SESSION_KEY`], as the strict UUID the
/// `X-Coord-Caller-Session` header must be — or `None`.
///
/// Absent (a row spooled with no resolution, or by a build predating the key)
/// and malformed both mean "replay headerless": a value that is not a UUID can
/// name no `coord.agent_sessions` row, and coord would only ignore it.
fn gate_registration_caller_session(payload: &JsonValue) -> Option<Uuid> {
    let raw = payload
        .get(super::closeout_spool::GATE_CALLER_SESSION_KEY)?
        .as_str()?;
    Uuid::parse_str(raw.trim()).ok()
}

/// Attach the spooled author ([`gate_registration_caller_session`]) to a
/// register-gate request, when the row carries one.
///
/// The ONE place a replayed gate gets its `X-Coord-Caller-Session`, used by
/// both register-gate POSTs — the first replay and the retry after a
/// `work_unit_not_found` bootstrap — so the retry cannot land the very gates
/// the bootstrap exists for without their author.
fn with_gate_caller_session(
    rb: reqwest::RequestBuilder,
    payload: &JsonValue,
) -> reqwest::RequestBuilder {
    match gate_registration_caller_session(payload) {
        Some(sid) => rb.header(crate::coord_mcp::CALLER_SESSION_HEADER, sid.to_string()),
        None => rb,
    }
}

/// Build the `POST /coord/work-units/:slug/register-gate` body from a
/// `gate_registration` outbox payload — coord's `UnitGateRequest` shape.
///
/// Only the five body fields are forwarded: `work_unit_slug` is a PATH
/// segment and `work_unit_upsert` is the runner-side lazy bootstrap (see
/// [`gate_registration_upsert_body`]), so neither belongs on the wire here.
/// `continuation_spawn` / `clearance_audience` / `gate_class` are omitted
/// when absent OR null so coord's `#[serde(default)]` /
/// `default_clearance_audience` apply — a literal `null` would be a
/// deserialize error on the non-`Option` `clearance_audience`.
fn gate_registration_body(payload: &JsonValue) -> JsonValue {
    let mut body = serde_json::Map::new();
    // Required by coord; forwarded verbatim when present. Absent means coord
    // answers 4xx, which is an Ack-drop — correct, since the queue cannot
    // invent a predicate the producer never recorded.
    for key in ["predicate", "phase_name"] {
        if let Some(v) = payload.get(key) {
            body.insert(key.to_string(), v.clone());
        }
    }
    for key in ["continuation_spawn", "clearance_audience", "gate_class"] {
        if let Some(v) = payload.get(key) {
            if !v.is_null() {
                body.insert(key.to_string(), v.clone());
            }
        }
    }
    JsonValue::Object(body)
}

/// Build the `POST /coord/work-units/upsert` body from a `gate_registration`
/// payload's optional `work_unit_upsert` bootstrap, or `None` when the
/// producer recorded none.
///
/// `slug` always comes from the record's own `work_unit_slug` — the same
/// value the register-gate path uses — so the bootstrap can never create a
/// work unit under a different slug than the gate it is unblocking.
fn gate_registration_upsert_body(payload: &JsonValue) -> Option<JsonValue> {
    let slug = gate_registration_slug(payload)?;
    let bootstrap = payload.get("work_unit_upsert")?.as_object()?;
    let mut body = serde_json::Map::new();
    body.insert("slug".to_string(), JsonValue::String(slug));
    // Every other column coord's `UpsertRequest` accepts is optional and
    // OVERWRITES when present — so a null is dropped rather than sent.
    for key in ["title", "status", "metadata", "by_actor"] {
        if let Some(v) = bootstrap.get(key) {
            if !v.is_null() {
                body.insert(key.to_string(), v.clone());
            }
        }
    }
    Some(JsonValue::Object(body))
}

/// Response handling for `gate_registration` POSTs — best-effort posture plus
/// the one recovery this kind needs.
///
/// - **2xx** — coord created the gate; the `gate_id` is logged so a replayed
///   closeout is traceable back to the gate it actually produced.
/// - **404 `work_unit_not_found`** — coord's register-gate door never upserts
///   the work unit, so a closeout recorded while coord was unreachable can
///   land on a slug coord has never seen. Recovered LAZILY: the payload's
///   optional `work_unit_upsert` bootstrap is sent to
///   `POST /coord/work-units/upsert`, then the gate is registered once more.
///   With no bootstrap recorded, the row is Ack-dropped with a warn — a retry
///   could only produce the same 404 forever.
/// - **any other 4xx** — `PermanentFailure` (Ack-drop): a body the queue
///   cannot edit is not going to start being accepted.
/// - **5xx** — `Transport`, which the drain maps to the bounded per-record
///   retry that never breaks the batch.
///
/// **Why the upsert is lazy and not unconditional.** Coord's `UpsertRequest`
/// overwrites `title` / `status` / `metadata` whenever they are present, so
/// upserting on every push would let a row replayed hours later stamp a live
/// work unit's status back to whatever the offline session happened to
/// record. Firing it only on the 404 means the bootstrap runs exactly when
/// there is no row to clobber.
async fn gate_registration_outcome(
    inner: &Arc<CoordSyncInner>,
    rec: &OutboxRecord,
    resp: reqwest::Response,
    scope: TenantScope,
) -> PushOutcome {
    let status = resp.status();
    if status.is_success() {
        log_registered_gate(rec, resp).await;
        return PushOutcome::Acked;
    }
    let detail = resp.text().await.unwrap_or_default();
    if status == StatusCode::NOT_FOUND && detail.contains("work_unit_not_found") {
        return bootstrap_then_register(inner, rec, scope).await;
    }
    write_failure_outcome(status, format!("{status}: {detail}"))
}

/// Log the `gate_id` a successful register-gate returned. Best-effort: an
/// unparseable body costs a debug line, never the ACK.
async fn log_registered_gate(rec: &OutboxRecord, resp: reqwest::Response) {
    match resp.json::<JsonValue>().await {
        Ok(body) => tracing::info!(
            session = %rec.session_id,
            seq = rec.seq,
            gate_id = ?body.get("gate_id").and_then(JsonValue::as_str),
            "coord_sync: closeout gate registered"
        ),
        Err(e) => tracing::debug!(
            session = %rec.session_id,
            seq = rec.seq,
            error = %e,
            "coord_sync: register-gate 201 body not parseable — gate_id not logged"
        ),
    }
}

/// The `work_unit_not_found` recovery: upsert the work unit from the recorded
/// bootstrap, then register the gate once more. See
/// [`gate_registration_outcome`] for why this is reached only on the 404.
async fn bootstrap_then_register(
    inner: &Arc<CoordSyncInner>,
    rec: &OutboxRecord,
    scope: TenantScope,
) -> PushOutcome {
    let Some(slug) = gate_registration_slug(&rec.payload) else {
        // Unreachable in practice — the push arm refuses to build a URL
        // without a valid slug — but stated rather than unwrapped.
        return PushOutcome::PermanentFailure(
            "gate_registration payload lost its `work_unit_slug` between push and \
             recovery"
                .to_string(),
        );
    };
    let Some(upsert) = gate_registration_upsert_body(&rec.payload) else {
        tracing::warn!(
            session = %rec.session_id,
            seq = rec.seq,
            %slug,
            "coord_sync: register-gate 404 work_unit_not_found and the record carries \
             no `work_unit_upsert` bootstrap — dropping the closeout gate (retrying \
             can only reproduce the same 404)"
        );
        return PushOutcome::Acked;
    };

    let base = inner.coord_url.trim_end_matches('/');
    let upsert_url = format!("{base}/coord/work-units/upsert");
    match crate::auth::attach_device_auth_for(inner.http.post(&upsert_url).json(&upsert), scope)
        .send()
        .await
    {
        Ok(resp) => {
            let status = resp.status();
            if !status.is_success() {
                let detail = resp.text().await.unwrap_or_default();
                return write_failure_outcome(
                    status,
                    format!("work-unit bootstrap upsert: {status}: {detail}"),
                );
            }
            tracing::info!(
                session = %rec.session_id,
                seq = rec.seq,
                %slug,
                "coord_sync: bootstrapped the missing work unit for a replayed closeout gate"
            );
        }
        Err(e) => return PushOutcome::Transport(format!("work-unit bootstrap upsert failed: {e}")),
    }

    // Register once more. A failure here leaves the row unacked, and the next
    // tick re-runs the whole arm — the upsert is idempotent on the same
    // values, so the retry costs nothing beyond one extra request.
    let url = format!("{base}/coord/work-units/{slug}/register-gate");
    let body = gate_registration_body(&rec.payload);
    let rb = crate::auth::attach_device_auth_for(inner.http.post(&url).json(&body), scope);
    // Same author as the first attempt — see [`with_gate_caller_session`].
    match with_gate_caller_session(rb, &rec.payload).send().await {
        Ok(resp) => {
            let status = resp.status();
            if status.is_success() {
                log_registered_gate(rec, resp).await;
                return PushOutcome::Acked;
            }
            let detail = resp.text().await.unwrap_or_default();
            write_failure_outcome(
                status,
                format!("register-gate after bootstrap: {status}: {detail}"),
            )
        }
        Err(e) => PushOutcome::Transport(format!("register-gate after bootstrap: {e}")),
    }
}

/// Reassemble a `POST /sessions` body from the outbox payload + the row's
/// machine_id. The session start path writes the create body shape into
/// `payload` directly (`{id, kind, intent, state, started_at}`), so
/// rebuilding for the wire is mostly relabeling.
fn rebuild_create_body(rec: &OutboxRecord) -> JsonValue {
    // tenant_id resolution order: the intent/payload body (Phase 8b: the
    // registry stamps the session's tenant into the intent at creation —
    // spawn input or the machine.json default-for-new-sessions, so this arm
    // is the common case now) → the device's `active_tenant_id` from
    // `~/.qontinui/machine.json` (pre-8b outbox rows) → nil. The
    // machine.json fallback is what makes a single-tenant operator's
    // sessions visible on their tenant-scoped dashboard: without it, every
    // session registers under the nil tenant `00000000-…` and the
    // operator's `/sessions` view (which scopes to their resolved tenant)
    // shows nothing despite a healthy pipeline.
    let intent = rec
        .payload
        .get("intent")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let tenant_id = intent
        .get("tenant_id")
        .or_else(|| rec.payload.get("tenant_id"))
        .cloned()
        .or_else(|| {
            crate::session::dual_write::resolve_active_tenant_id()
                .map(|u| JsonValue::String(u.to_string()))
        })
        .unwrap_or_else(|| JsonValue::String(Uuid::nil().to_string()));
    let kind = rec
        .payload
        .get("kind")
        .or_else(|| rec.payload.get("session_kind"))
        .cloned()
        .unwrap_or_else(|| JsonValue::String("terminal_shell".to_string()));
    let parent = rec.payload.get("parent_session_id").cloned();
    let mut body = json!({
        "id": rec.session_id,
        "tenant_id": tenant_id,
        "device_id": rec.machine_id,
        "session_kind": kind,
        "intent": intent,
    });
    if let Some(p) = parent {
        if !p.is_null() {
            body["parent_session_id"] = p;
        }
    }
    // Forward the ambient Claude Code session id (the `Session-Id` git-trailer
    // id) as a first-class field so coord can join session rows to commit
    // history. Omitted when absent/null — coord tolerates its absence.
    if let Some(ccsid) = rec.payload.get("claude_code_session_id") {
        if !ccsid.is_null() {
            body["claude_code_session_id"] = ccsid.clone();
        }
    }
    // Session-automation Phase 0 — forward the runner `SessionManager` key
    // (UUIDv4 `task_run_id`) so coord persists `coord.sessions.task_run_id`
    // for inject-target resolution. Omitted when absent/null; coord's
    // `CreateSessionRequest.task_run_id` is `#[serde(default)]`.
    if let Some(trid) = rec.payload.get("task_run_id") {
        if !trid.is_null() {
            body["task_run_id"] = trid.clone();
        }
    }
    body
}

/// Extract the subset of `state_change` payload fields that map to
/// `UpdateSessionRequest` (state, repo, branch, intent_updates,
/// claude_code_session_id).
fn state_change_body(payload: &JsonValue) -> JsonValue {
    let mut body = serde_json::Map::new();
    if let Some(state) = payload.get("state") {
        body.insert("state".into(), state.clone());
    }
    if let Some(repo) = payload.get("repo") {
        body.insert("repo".into(), repo.clone());
    }
    if let Some(branch) = payload.get("branch") {
        body.insert("branch".into(), branch.clone());
    }
    if let Some(intent_updates) = payload.get("intent_updates") {
        body.insert("intent_updates".into(), intent_updates.clone());
    }
    // Phase 2b of plan
    // `2026-09-02-coord-report-status-unscoped-write-hits-a-peer-session`: the
    // harness session id a provider CONFIRMED for this row, emitted by
    // `SessionRegistry::confirm_claude_code_session_id`. Coord validates it as
    // a Uuid and refuses (whole-PATCH) an id another active row on the device
    // holds, so the runner sends the value and reads the outcome from the
    // response rather than pre-checking ownership.
    if let Some(ccsid) = payload.get("claude_code_session_id") {
        body.insert("claude_code_session_id".into(), ccsid.clone());
    }
    // Coord refreshes last_heartbeat_at on any PATCH — passing
    // heartbeat=true ensures the row gets a fresh stamp even when the
    // caller only changed metadata.
    body.insert("heartbeat".into(), JsonValue::Bool(true));
    JsonValue::Object(body)
}

/// Build the `PATCH /sessions/:id {progress:{…}}` body from a `progress`
/// outbox payload. The registrar records the work-progress fields flat in the
/// payload (`session_status`, optional `last_progress_at` / `progress_detail`);
/// coord's `UpdateSessionRequest` nests them under `progress` and stamps
/// `last_progress_at = now()` when omitted. Advances the work-progress axis
/// (`coord.sessions.last_progress_at`), independent of the liveness heartbeat.
///
/// `tool_name` / `tool_input_digest` / `model` are the TOOL-GRAIN half (plan
/// `2026-08-11-coord-hook-sourced-agent-status`), written by the OSC 9999
/// sideband (`terminal::agent_status_sideband`) and by the Claude Code
/// `PostToolUse` hook. **Coord accepts them only once that plan's Phase 2
/// widens `ProgressUpdate`.** Forwarding them before then is safe and inert,
/// not a 4xx: coord's `ProgressUpdate` carries no `deny_unknown_fields`, so
/// serde drops an unknown key rather than rejecting the body. (Verified
/// against `qontinui-coord/src/sessions.rs`.) The three pre-existing keys
/// above are byte-identical to what they were.
fn progress_body(payload: &JsonValue) -> JsonValue {
    let mut progress = serde_json::Map::new();
    if let Some(status) = payload.get("session_status") {
        progress.insert("session_status".into(), status.clone());
    }
    if let Some(at) = payload.get("last_progress_at") {
        progress.insert("last_progress_at".into(), at.clone());
    }
    if let Some(detail) = payload.get("progress_detail") {
        progress.insert("progress_detail".into(), detail.clone());
    }
    if let Some(tool_name) = payload.get("tool_name") {
        progress.insert("tool_name".into(), tool_name.clone());
    }
    if let Some(digest) = payload.get("tool_input_digest") {
        progress.insert("tool_input_digest".into(), digest.clone());
    }
    if let Some(model) = payload.get("model") {
        progress.insert("model".into(), model.clone());
    }
    json!({ "progress": JsonValue::Object(progress) })
}

/// Build the `POST /sessions/:id/output` body from an `output_chunk`
/// outbox payload. The transcript emitter records `{stream, chunk_offset,
/// payload_b64}` — forward exactly the subset coord's ingest understands.
/// `stream` defaults to "transcript" when absent: the ONLY writer of
/// `output_chunk` outbox rows is the transcript emitter (the PTY stream
/// bypasses the outbox via `output_pipe.rs`), so a missing field can only
/// be a transcript row.
fn output_chunk_body(payload: &JsonValue) -> JsonValue {
    json!({
        "chunk_offset": payload.get("chunk_offset").cloned().unwrap_or(json!(0)),
        "payload_b64": payload.get("payload_b64").cloned().unwrap_or(json!("")),
        "stream": payload
            .get("stream")
            .cloned()
            .unwrap_or_else(|| json!(crate::session::transcript_emitter::TRANSCRIPT_STREAM)),
    })
}

/// Build the `POST /sessions/:id/steal` body from the claim_stolen
/// outbox payload. The runner's machine_id is the stealer.
fn steal_body(rec: &OutboxRecord) -> JsonValue {
    let reason = rec
        .payload
        .get("reason")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    json!({
        "reason": reason,
        "machine_id": rec.machine_id,
    })
}

async fn handle_conflict(inner: &Arc<CoordSyncInner>, rec: &OutboxRecord, row: Option<JsonValue>) {
    tracing::warn!(
        session = %rec.session_id,
        "coord_sync: POST /sessions returned 409 — conflict on acquire"
    );

    // Flip the in-memory session state so the frontend hides the
    // half-acquired session under PendingResolution.
    if let Some(reg) = inner
        .registry
        .lock()
        .expect("coord_sync registry slot poisoned")
        .as_ref()
        .and_then(Weak::upgrade)
    {
        reg.set_state(rec.session_id, SessionState::PendingResolution);
    }

    // Emit the existing `agent-claim-conflict` event so ConflictModal
    // picks it up unchanged.
    let handle = {
        let slot = inner
            .app_handle
            .lock()
            .expect("coord_sync app_handle slot poisoned");
        slot.clone()
    };
    if let Some(handle) = handle {
        let current_holder = row
            .as_ref()
            .and_then(|r| r.get("device_id"))
            .and_then(|d| d.as_str())
            .and_then(|s| Uuid::parse_str(s).ok());
        let intent = row
            .as_ref()
            .and_then(|r| r.get("intent"))
            .and_then(|i| i.get("purpose"))
            .and_then(|p| p.as_str())
            .map(|s| s.to_string());
        let payload = AgentClaimConflict {
            kind: "Session".to_string(),
            resource_key: format!("session:{}", rec.session_id),
            current_holder,
            intent,
            session_id: rec.session_id,
        };
        use tauri::Emitter;
        if let Err(e) = handle.emit("agent-claim-conflict", payload) {
            tracing::warn!(error = %e, "coord_sync: emit agent-claim-conflict failed");
        }
    }
}

// ---------------------------------------------------------------------------
// Heartbeat loop
// ---------------------------------------------------------------------------

async fn run_heartbeat_loop(inner: Arc<CoordSyncInner>) {
    let interval = inner.heartbeat;
    tracing::info!(
        ?interval,
        stale_after = ?inner.stale,
        "coord_sync: heartbeat loop starting"
    );

    let mut tick: u64 = 0;

    loop {
        tokio::time::sleep(interval).await;
        tick = tick.wrapping_add(1);

        // Phase 3b: the outside observer of coord's OWN liveness rides this
        // tick. It is hosted here and evaluated BEFORE the registry upgrade
        // deliberately — coord liveness is a property of the fleet, not of
        // this runner's session population, so a runner with no live session
        // (the `continue` below) must still observe it. `on_host_tick`
        // spawns detached and returns immediately, so a slow coord can never
        // delay a session heartbeat, and its own single-flight latch drops a
        // tick rather than stacking probes.
        if let Some(observer) = inner.outside_observer.as_ref() {
            let app = inner
                .app_handle
                .lock()
                .expect("coord_sync app_handle slot poisoned")
                .clone();
            observer.on_host_tick(tick, interval, app);
        }

        let reg = match inner
            .registry
            .lock()
            .expect("coord_sync registry slot poisoned")
            .as_ref()
            .and_then(Weak::upgrade)
        {
            Some(r) => r,
            None => continue,
        };

        let now = Utc::now();
        let snapshot = reg.snapshot();
        let machine_id = reg.machine_id();

        let mut to_stale: Vec<Uuid> = Vec::new();
        let mut to_heartbeat: HashMap<Uuid, ()> = HashMap::new();

        for desc in snapshot {
            // Already closed — skip.
            if matches!(desc.state, SessionState::Closed) {
                continue;
            }
            let last = desc.last_heartbeat_at.unwrap_or(desc.started_at);
            let elapsed = (now - last)
                .to_std()
                .unwrap_or_else(|_| Duration::from_secs(0));

            // NOTE (plan A3): the runner deliberately does NOT auto-close /
            // DELETE an abandoned session here. When heartbeats cease (runner
            // stopped/crashed/slept), coord's own watcher ages the row
            // (Active→Stale at 600s, →Closed at 1800s). Self-closing at a
            // shorter local threshold would race coord and prematurely DELETE
            // a session coord still considers live. The local Stale flip below
            // is a UI affordance only — it never emits a DELETE.
            if elapsed >= inner.stale && !matches!(desc.state, SessionState::Stale) {
                to_stale.push(desc.id);
            }
            // Always emit a heartbeat row for active/stale sessions —
            // the drain loop folds it into a PATCH and the row's
            // `last_heartbeat_at` moves forward on success, eventually
            // pulling the session out of `Stale`.
            if matches!(
                desc.state,
                SessionState::Active | SessionState::Stale | SessionState::PendingResolution
            ) {
                to_heartbeat.insert(desc.id, ());
            }
        }

        // Phase 3: flip local state for stale sessions so the UI tile
        // greys out even when coord is unreachable.
        for id in to_stale {
            reg.set_state(id, SessionState::Stale);
        }

        // Emit heartbeat outbox rows. The drain loop picks them up on
        // its next tick and PATCHes coord with `{heartbeat: true}`.
        //
        // ONE batched append + ONE fsync for the whole sweep (plan
        // `2026-07-28-runner-many-sessions-performance` §7a/B6). The per-row
        // `record` loop this replaces cost one fsync per live session per
        // tick. This is purely the runner's local write path — each session
        // still gets its own row and its own `PATCH /sessions/:id`, so it
        // carries no coord-side dependency (that is Phase 7b).
        if !to_heartbeat.is_empty() {
            let events: Vec<OutboxEvent> = to_heartbeat
                .keys()
                .map(|id| {
                    OutboxEvent::new(
                        machine_id,
                        *id,
                        SessionEventKind::Heartbeat,
                        json!({ "id": id, "at": now }),
                    )
                })
                .collect();
            let count = events.len();
            if let Err(e) = inner.outbox.record_batch(events) {
                tracing::warn!(
                    sessions = count,
                    error = %e,
                    "coord_sync: heartbeat outbox write failed"
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Phase 10 — flag-refresh loop
// ---------------------------------------------------------------------------

/// Poll coord's `/tenant-policy?tenant_id=<id>` and cache the
/// `session_coordination_enabled` flag into the [`DualWriteGate`].
///
/// Only spawned when a tenant resolved (see
/// [`CoordSync::start_flag_poll_task`]). Robust to coord being down: a
/// fetch failure leaves the cached value untouched (so a transient outage
/// never spuriously flips the gate) and the loop simply tries again next
/// tick. The flag is a rollout knob that changes rarely, so the default
/// 60s cadence is plenty.
async fn run_flag_poll_loop(inner: Arc<CoordSyncInner>, tenant_id: Uuid) {
    let interval = inner.dual_write.poll_interval();
    tracing::info!(
        %tenant_id,
        ?interval,
        "coord_sync: Phase 10 cutover-flag poll loop starting (dormant until flag flips)"
    );
    let mut refusals = TenantPolicyAuthReporter::default();
    loop {
        match fetch_session_coordination_flag(&inner, tenant_id).await {
            Ok(enabled) => {
                if let Some(line) = refusals.clear() {
                    tracing::info!(%tenant_id, "{line}");
                }
                inner.dual_write.apply(enabled);
            }
            Err(FlagPollError::Unauthorized { status, reason }) => {
                // Resolving the presented credential costs a local file read,
                // so it happens HERE — on the report path the throttle has
                // already decided to take — and not on every pass. Note what
                // it does and does not establish: it names what this device
                // would resolve NOW, a moment AFTER the request that was
                // refused, not what that request carried. The two differ if
                // the refresher lands a slot in between, which is why the
                // line says "currently resolves" and coord's own stated
                // `reason` travels beside it.
                if let Some(line) = refusals.observe(status, &reason, tenant_id, || {
                    crate::auth::presented_tenant(tenant_policy_scope(tenant_id))
                }) {
                    tracing::warn!(%tenant_id, "{line}");
                }
            }
            Err(FlagPollError::Other(e)) => {
                // Leave the cached value as-is — a coord hiccup must not
                // flip the gate in either direction.
                //
                // But DO end any standing refusal: coord answering some other
                // way is coord answering differently, and leaving the key in
                // place silences the next genuine refusal (`403`, `403`xN,
                // `500`, `403` — the second `403` went unreported).
                refusals.interrupted();
                tracing::debug!(
                    %tenant_id,
                    error = %e,
                    "coord_sync: tenant-policy fetch failed; keeping cached cutover flag"
                );
            }
        }
        tokio::time::sleep(interval).await;
    }
}

/// The credential scope the tenant-policy poll presents.
///
/// `Owned(tenant_id)` — the tenant the poll is ASKING ABOUT, because
/// `sessions::get_tenant_policy` requires the `?tenant_id=` query to equal the
/// presented token's `tenant_id` claim. A request that names one tenant while
/// carrying another's credential cannot satisfy that equality, so it is
/// refused `403`.
///
/// That `403` is NOT the same refusal an unauthenticated caller gets, and an
/// earlier revision of this comment claimed it was. Verified against
/// `qontinui-coord` `origin/main` `de4107e2`: the mismatch arm answers
/// `{"error":"tenant_id does not match principal"}`
/// (`crates/coord/src/sessions.rs::get_tenant_policy`) while a caller with no
/// principal gets `{"error":"auth_required"}`
/// (`crates/coord/src/fleet_principal.rs::auth_required`). Same status, two
/// machine-readable bodies — and the mismatch body NAMES this defect. What
/// made it read for months as "not paired yet" is that the runner discarded
/// the body and reported the status alone; [`FlagPollError::Unauthorized`]
/// now carries it.
///
/// That is exactly what the plain [`crate::coord_http::coord_get`] did here:
/// it asserts [`TenantScope::Device`], which selects the LEGACY `access_token`
/// slot — the DEFAULT binding's JWT — regardless of the tenant in the query
/// string. The poll was correct only while the default binding happened to be
/// the tenant frozen into this loop at process construction.
///
/// **Slot-miss posture is deliberately unchanged and is the isolation
/// guarantee**: `select_device_bearer` returns `None` for a tenant this device
/// holds no usable slot for, the request goes out unauthenticated, and coord
/// answers. It must never fall back to the legacy slot — presenting another
/// tenant's credential is the bug, not the recovery.
///
/// A named function rather than an inline expression so the call site and the
/// refusal diagnostic below cannot drift apart: both ask this one question.
fn tenant_policy_scope(tenant_id: Uuid) -> TenantScope {
    TenantScope::Owned(tenant_id)
}

/// Why one tenant-policy poll pass produced no flag.
///
/// Split from the old flat `String` for one reason: a `401`/`403` is a
/// STANDING condition (this device cannot present the tenant it is asking
/// about) while everything else is a transient, and the two want opposite
/// reporting. Collapsing them is what produced 656 consecutive identical
/// warnings — one per minute for the life of the process.
#[derive(Debug)]
enum FlagPollError {
    /// Coord refused the credential presented. Standing until the credential
    /// or the tenant changes.
    ///
    /// Carries coord's OWN stated cause, because the status alone does not
    /// distinguish the two conditions this route answers `403` to — and the
    /// two want different operator actions. See [`coord_refusal_reason`].
    Unauthorized { status: u16, reason: String },
    /// Transport, other non-2xx, decode, or a missing field — transient.
    Other(String),
}

impl std::fmt::Display for FlagPollError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FlagPollError::Unauthorized { status, reason } => {
                // `{reason:?}`, not `{reason}`: this string originates in a
                // response body, and nothing downstream of a `Display` impl
                // can un-inject a newline that reaches a log. The report path
                // escapes it for the same reason; a second, unescaped channel
                // is exactly how that control gets lost.
                write!(f, "status {status}: {reason:?}")
            }
            FlagPollError::Other(e) => f.write_str(e),
        }
    }
}

/// Reduce coord's refusal body to the one field it states a cause in.
///
/// Coord answers every refusal on this chain as `{"error": "<static
/// string>"}` — `auth_required` (`fleet_principal::auth_required`),
/// `tenant_id does not match principal` (`sessions::get_tenant_policy`),
/// `strategy_admin_required`, `tenant_not_resolved`. Verified against
/// `qontinui-coord` `origin/main` `de4107e2`: every one is a compile-time
/// literal, so echoing this field carries no credential material and widens
/// no leakage. Nothing ELSE from the body is echoed, for exactly that reason
/// — an intermediary's 403 page is not coord's contract and is not something
/// this function may paste into a log. Such a body is reported by SHAPE
/// instead, which still separates "coord refused" from "something in the
/// path refused".
///
/// Bounded: the `error` field is truncated, so a body that claims the shape
/// without honouring the contract cannot write an unbounded line.
///
/// **JSON is not enough to make a body coord's.** An intermediary answering
/// `{"error":"invalid bearer eyJhbGciOi…"}` — a WAF or proxy reflecting the
/// credential it just rejected — satisfies the shape exactly, and echoing it
/// would paste the presented token into a log. So the echo is additionally
/// gated on the ALPHABET coord's four literals are drawn from
/// ([`is_coord_literal_shaped`]); anything else falls through to the shape
/// line with the rest.
///
/// Takes a `Result` because the body may not have been read at all. A read
/// error is NOT a measurement of the body and must not be rendered as one:
/// `unwrap_or_default()` turned a mid-body connection reset into `""` and
/// then reported "an unrecognized 0-byte body, so the refusal may not be
/// coord's own", casting doubt on coord when the only established fact was a
/// local read failure (served policy `verification-and-evidence`
/// `unknown-must-not-render-as-a-default`).
fn coord_refusal_reason(body: Result<&str, &str>) -> String {
    const MAX: usize = 200;
    let body = match body {
        Ok(b) => b,
        Err(e) => {
            return format!(
                "the refusal body could not be READ ({e}) — nothing about it was measured, so \
                 coord's stated cause is UNKNOWN; this says nothing either way about whose \
                 refusal it was"
            )
        }
    };
    let reason = serde_json::from_str::<JsonValue>(body)
        .ok()
        .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_owned))
        .filter(|r| is_coord_literal_shaped(r));
    match reason {
        // By CHARS, not bytes: `String::truncate` panics on a non-boundary
        // index, and a log-formatting helper must not be the thing that kills
        // the poll loop. The charset gate above admits ASCII only, so a
        // multi-byte cut is unreachable TODAY — the cut itself is not, and
        // the gate is the thing a future edit widens.
        Some(r) if r.chars().count() > MAX => r.chars().take(MAX).chain(['…']).collect(),
        Some(r) => r,
        None => format!(
            "no coord `error` field — an unrecognized {}-byte body{}, so the refusal may not be \
             coord's own",
            body.len(),
            if body.len() >= REFUSAL_BODY_CAP {
                // At the cap, so the length is a floor, not a measurement of
                // the whole body. Say which.
                " (at the read cap — the body may be longer)"
            } else {
                ""
            }
        ),
    }
}

/// Whether a string is drawn from the alphabet coord's own stated causes are.
///
/// All four are compile-time literals of lowercase ASCII words joined by
/// spaces or underscores — `auth_required`, `tenant_id does not match
/// principal`, `strategy_admin_required`, `tenant_not_resolved` (verified
/// against `qontinui-coord` `origin/main` `de4107e2`). A credential cannot
/// survive this predicate: every bearer this runner presents is a JWT or a
/// `qontinui_runner_*` opaque token, and both carry uppercase, digits, `.`
/// or `-`.
///
/// Deliberately a CHARSET test and not an allowlist of the four literals: a
/// literal coord adds should reach the operator, and an allowlist would
/// silently report every new one by shape.
fn is_coord_literal_shaped(reason: &str) -> bool {
    !reason.is_empty()
        && reason
            .chars()
            .all(|c| c.is_ascii_lowercase() || c == '_' || c == ' ')
}

/// How much of a refusal body this loop will buffer.
///
/// The LOG LINE was bounded from the start; the READ was not. `resp.text()`
/// buffers whatever the peer sends, the client sets a timeout and no size
/// cap, and this runs once per poll interval for the life of the process —
/// so a misbehaving intermediary answering `403` with a large body had an
/// unbounded, indefinitely repeating allocation on the other end of it.
/// Coord's own bodies are tens of bytes.
const REFUSAL_BODY_CAP: usize = 4 * 1024;

/// Read at most [`REFUSAL_BODY_CAP`] bytes of a refusal body, and say so when
/// the read FAILED rather than substituting an empty body for one.
async fn read_refusal_body(mut resp: reqwest::Response) -> Result<String, String> {
    let mut buf: Vec<u8> = Vec::new();
    while buf.len() < REFUSAL_BODY_CAP {
        match resp.chunk().await {
            Ok(Some(chunk)) => {
                let room = REFUSAL_BODY_CAP - buf.len();
                buf.extend_from_slice(&chunk[..room.min(chunk.len())]);
            }
            Ok(None) => break,
            // A partial read is still a read failure: what was buffered so
            // far is not the body, and reporting its length would be a
            // measurement of a truncation.
            Err(e) => return Err(e.to_string()),
        }
    }
    // Lossy rather than fatal: a cap can land mid-codepoint, and the result
    // is only ever JSON-parsed or counted. A body that is genuinely not UTF-8
    // fails the parse and is reported by shape, which is the right answer.
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// Report a STANDING tenant-policy refusal once, not once per pass.
///
/// The measured defect this closes: the poll emitted **656 consecutive
/// identical** `403` warnings, one per configured interval, for the life of
/// the process — and the advice they carried ("retrying after device
/// pairing/auth") named a flow the runner cannot reach, because pairing was
/// never the missing thing. N identical lines are not N pieces of
/// information.
///
/// What counts as "the same refusal" is the pair `(status, tenant asked
/// about)`, and the silence is broken by any of three things: coord answering
/// with a different status, the poll succeeding, and a pass that failed some
/// OTHER way ([`TenantPolicyAuthReporter::interrupted`]). The second is the
/// one that matters — a device that acquires the queried tenant's slot stops
/// being refused, so recovery arrives as a success, not as a changed
/// credential — and it emits one line naming how many passes were suppressed,
/// so the quiet period is legible rather than merely absent.
///
/// The third arm is there because without it the silence outlived the
/// condition it described: `403`, `403`×N, `500`, `403` reported the first
/// `403` and nothing after, since a transient failure never touched the
/// standing state. Coord answering `500` and then `403` again IS coord
/// answering differently, which this type's own contract says breaks the
/// silence. The cost is honest and bounded: a coord flapping between a
/// refusal and a transient failure reports once per flap, at most once per
/// poll interval, and every such line carries the accumulated suppressed
/// count rather than resetting it — a changed answer is news, N identical
/// ones are not.
///
/// The tenant is in the key for the same reason, though no caller varies it
/// today: this loop freezes one tenant at construction, so it CANNOT vary,
/// and a suppression key that silently spans tenants would be a defect
/// waiting for the first caller that reuses the reporter.
///
/// Keying on the credential TOO was considered and rejected: it would put a
/// local encrypted-file read on every pass of a periodic loop to detect a
/// change that, when it is the change anyone cares about, announces itself as
/// a success on the very next pass. The cost is per-pass and permanent; the
/// information is duplicated.
///
/// Deliberately per-loop state, not a process-global `Once`: two loops polling
/// two tenants are two independent conditions, and a `Once` would let the
/// first silence the second forever.
#[derive(Default)]
struct TenantPolicyAuthReporter {
    /// The `(status, tenant asked about, epoch)` last reported out loud, if a
    /// refusal is standing.
    ///
    /// **Only [`Self::clear`] ever takes it**, which is what keeps the
    /// invariant `reported == None` ⟹ `suppressed == 0`: the one writer that
    /// drops a standing refusal is the one that prints its count.
    reported: Option<(u16, Uuid, u64)>,
    /// Passes suppressed since that report. Carried THROUGH a changed answer
    /// rather than reset by it, so a count is only ever dropped by being
    /// printed.
    suppressed: u64,
    /// Bumped by [`Self::interrupted`], and part of the suppression key.
    ///
    /// This is how a transient pass breaks the silence WITHOUT voiding the
    /// standing refusal: the key stops matching, so the next identical
    /// `401`/`403` is reported again, while `reported` stays `Some` and the
    /// recovery line the first report promised is still owed and still
    /// printed. Clearing `reported` instead — which an earlier revision did —
    /// bought the first half at the cost of the second, and stranded the
    /// suppressed count in a closed episode for some unrelated future refusal
    /// to print.
    epoch: u64,
}

impl TenantPolicyAuthReporter {
    /// Record one refused pass; returns the line to warn, or `None` when this
    /// pass repeats a refusal already reported.
    ///
    /// `presented` is a closure, not a value, because resolving the credential
    /// is a local encrypted-file read and the SUPPRESSED path — which is every
    /// pass but the first — must not pay for it. It is called only on the pass
    /// that actually emits a line.
    ///
    /// **`reason` is coord's own stated cause, and the line reports it
    /// ALONGSIDE the locally resolved credential rather than instead of it.**
    /// The two answer different questions — coord says whether it saw no
    /// principal or a mismatched one; the local read says which tenant this
    /// device would have presented — and neither is derivable from the other.
    /// What this must never do again is assert a cause it did not measure:
    /// the line this replaced ended "stays refused until this device holds a
    /// usable credential slot for {asked_about}", which is one of several
    /// conditions coord answers `403` to and was never established by
    /// anything the code read.
    fn observe(
        &mut self,
        status: u16,
        reason: &str,
        asked_about: Uuid,
        presented: impl FnOnce() -> crate::auth::PresentedTenant,
    ) -> Option<String> {
        if self.reported == Some((status, asked_about, self.epoch)) {
            self.suppressed += 1;
            return None;
        }
        self.reported = Some((status, asked_about, self.epoch));
        let suppressed = std::mem::replace(&mut self.suppressed, 0);
        let presented = presented();
        let carried = if suppressed == 0 {
            String::new()
        } else {
            format!(" (after {suppressed} suppressed identical refusals)")
        };
        Some(format!(
            "coord_sync: tenant-policy GET refused ({status}){carried} — coord says \
             {reason:?}; asked about tenant {asked_about}, and this device currently resolves \
             {presented}. Coord requires the query's tenant and the presented credential's \
             tenant_id claim to MATCH; the cached cutover flag is kept meanwhile. Identical \
             refusals from here on are suppressed — one line will report the recovery."
        ))
    }

    /// Record a pass that failed some OTHER way (transport, a non-auth
    /// status, a decode error).
    ///
    /// It emits nothing — the loop logs those at debug — but it DOES end the
    /// SILENCE, so the next `401`/`403` is reported rather than swallowed by
    /// a key that outlived the answer it described.
    ///
    /// It ends the silence by bumping the epoch, NOT by dropping the standing
    /// refusal, and the distinction is the whole of this method. The first
    /// report ends *"one line will report the recovery"*, and that line is
    /// owed across a transient blip — which is precisely the interleaving
    /// this method exists for. Dropping `reported` voided the promise: a
    /// `403`, `403`×N, blip, `200` sequence emitted no recovery line at all
    /// (`clear`'s `?` returned early), and left the N suppressed passes in
    /// the field for some unrelated later refusal to print as its own.
    ///
    /// The suppressed count survives either way: it belongs to the quiet
    /// period, not to the status that opened it, and is dropped only by being
    /// printed.
    fn interrupted(&mut self) {
        self.epoch = self.epoch.wrapping_add(1);
    }

    /// Record a pass that succeeded; returns the one recovery line when a
    /// refusal was standing, or `None` when nothing was.
    ///
    /// "Succeeded" is the WHOLE fetch, parse included: a 200 whose body is
    /// missing `session_coordination_enabled` is a [`FlagPollError::Other`]
    /// and reaches [`Self::interrupted`] instead. Deliberate, and narrow —
    /// such a pass is not announced as a recovery (nothing about the flag was
    /// established), but it does end the silence, so the next refusal is
    /// reported. Before `interrupted` existed it did neither, and a standing
    /// `403` key survived every 200-but-unparseable pass in between.
    ///
    /// It reports a refusal that an [`Self::interrupted`] pass intervened in,
    /// because that pass ended the SILENCE and not the refusal — see there.
    /// This is the only writer that drops `reported`, and it always prints
    /// the count as it goes, which is the invariant that keeps a closed
    /// episode's count from surfacing on an unrelated later line.
    fn clear(&mut self) -> Option<String> {
        let (status, _tenant, _epoch) = self.reported.take()?;
        let suppressed = std::mem::replace(&mut self.suppressed, 0);
        Some(format!(
            "coord_sync: tenant-policy GET authorized again — the standing {status} cleared \
             after {suppressed} suppressed identical refusals"
        ))
    }
}

/// GET `/tenant-policy?tenant_id=<id>` and pull out
/// `session_coordination_enabled`. Returns the bool on success; any
/// transport / non-2xx / shape error is an `Err` the caller treats as
/// "keep the cached value".
///
/// **Presents the credential for the tenant it is querying**
/// ([`tenant_policy_scope`]), through the tenant-STATING
/// [`crate::coord_http::coord_get_for`] seam rather than the defaulting
/// `coord_get`. Never fatal in either direction: a tenant this device holds
/// no usable slot for sends the request unauthenticated and coord answers,
/// which is a retry-after-credential signal, not an error to propagate. This
/// function no longer logs — the loop owns reporting, because only the loop
/// can tell a standing refusal from a first one. It DOES read the refusal
/// body ([`coord_refusal_reason`]) and hand coord's stated cause to the loop,
/// because a `403` is the only thing the caller would otherwise have, and a
/// status is not a cause.
async fn fetch_session_coordination_flag(
    inner: &Arc<CoordSyncInner>,
    tenant_id: Uuid,
) -> Result<bool, FlagPollError> {
    let base = inner.coord_url.trim_end_matches('/');
    let url = format!("{base}/tenant-policy?tenant_id={tenant_id}");
    let resp = crate::coord_http::coord_get_for(&inner.http, &url, tenant_policy_scope(tenant_id))
        .send()
        .await
        .map_err(|e| FlagPollError::Other(format!("transport: {e}")))?;
    let status = resp.status();
    if !status.is_success() {
        let code = status.as_u16();
        if code == 401 || code == 403 {
            // Keep coord's OWN stated cause. The status alone cannot tell
            // "no principal resolved" from "the principal's tenant is not
            // the one you asked about", and those are different operator
            // actions; coord distinguishes them in the body and the earlier
            // revision of this function dropped it on the floor, then
            // reconstructed a guess from a local file read.
            let body = read_refusal_body(resp).await;
            let reason = coord_refusal_reason(body.as_deref().map_err(String::as_str));
            return Err(FlagPollError::Unauthorized {
                status: code,
                reason,
            });
        }
        return Err(FlagPollError::Other(format!("status {status}")));
    }
    let body: JsonValue = resp
        .json()
        .await
        .map_err(|e| FlagPollError::Other(format!("decode: {e}")))?;
    body.get("session_coordination_enabled")
        .and_then(|v| v.as_bool())
        .ok_or_else(|| FlagPollError::Other("missing session_coordination_enabled field".into()))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::transport::{DynTransport, Transport, TransportError, TransportHandle};

    /// SOURCE GUARD: every session-outbox event kind has a dispatch arm.
    ///
    /// The drain matches on `&str`, not on [`SessionEventKind`], and its
    /// fallthrough **quietly ACKs and drops** ("so the file doesn't grow"). So a
    /// new variant added to the enum but missed here COMPILES CLEANLY and
    /// silently discards every event of that kind — no exhaustiveness error, no
    /// warning, and no failing test unless one is written on purpose. This is
    /// that test.
    ///
    /// `MemoryRecord` is deliberately excluded: it is written to its own
    /// `memory-outbox.jsonl` and the coord session drain never sees it. Any
    /// OTHER kind reaching this list without an arm is a silent data-loss bug.
    #[test]
    fn every_session_outbox_kind_has_a_dispatch_arm() {
        let src = include_str!("coord_sync.rs");
        // Isolate the dispatch match so an arm name appearing in a doc comment
        // elsewhere cannot satisfy the assertion.
        let dispatch = src
            .split_once("async fn push_record")
            .expect("the dispatch function must exist")
            .1;

        for kind in [
            SessionEventKind::Started,
            SessionEventKind::StateChange,
            SessionEventKind::Closed,
            SessionEventKind::Heartbeat,
            SessionEventKind::ClaimStolen,
            SessionEventKind::OutputChunk,
            SessionEventKind::CommitReport,
            SessionEventKind::Progress,
            SessionEventKind::HelperTaskCreated,
            SessionEventKind::RestoreRecord,
            SessionEventKind::GateRegistration,
            SessionEventKind::FindingPosted,
            SessionEventKind::Finished,
            SessionEventKind::CoordTransportRung,
            SessionEventKind::AgentNotification,
        ] {
            let arm = format!("\"{}\" =>", kind.as_str());
            assert!(
                dispatch.contains(&arm),
                "SessionEventKind::{kind:?} (wire {:?}) has NO dispatch arm — the \
                 `other =>` fallthrough will ACK and DROP every one of these \
                 silently. Add the arm in push_record.",
                kind.as_str()
            );
        }
    }

    /// The finished marker must ride `PATCH /sessions/:id`, where the target is
    /// the URL PATH. Routing it through `coord_report_status` instead would
    /// reintroduce the peer-clobber trap: an omitted `claude_code_session_id`
    /// there resolves to the DEVICE's most-recently-started active session,
    /// which on a multi-session box is usually a peer's row.
    #[test]
    fn finished_rides_the_path_addressed_patch_not_a_device_wide_write() {
        let src = include_str!("coord_sync.rs");
        let dispatch = src
            .split_once("async fn push_record")
            .expect("the dispatch function must exist")
            .1;
        let arm = dispatch
            .split_once("\"finished\" =>")
            .expect("the finished arm must exist")
            .1;
        // Bound the scan to this arm's body.
        let body = &arm[..arm.find("\n        \"").unwrap_or(arm.len().min(1200))];

        assert!(
            body.contains("{base}/sessions/{}") && body.contains("rec.session_id"),
            "the finished write must address the session by PATH: {body}"
        );
        assert!(
            body.contains(".patch(&url)"),
            "the finished write must be a PATCH, matching the progress arm: {body}"
        );
        assert!(
            body.contains("\"session_status\": \"finished\""),
            "the body must set coord's WORK axis to finished: {body}"
        );
    }
    use crate::session::{Intent, SessionKind, SessionRegistry, SessionTransports};
    use axum::{
        extract::{Path as AxumPath, Query as AxumQuery, State as AxumState},
        http::StatusCode as AxumStatus,
        response::IntoResponse,
        routing::{get, patch, post},
        Json, Router,
    };
    use std::sync::Arc;
    use tokio::net::TcpListener;
    use tokio::sync::Mutex as TokMutex;

    use crate::test_env::env_lock;

    /// In-memory transport that lets sessions start without touching
    /// PTY / Claude / workflow subsystems.
    struct NoopTransport(SessionKind);
    impl Transport for NoopTransport {
        fn start(&self, _intent: &Intent) -> Result<TransportHandle, TransportError> {
            Ok(TransportHandle::Pty {
                terminal_id: format!("noop-{:?}", self.0),
            })
        }
        fn write_input(&self, _h: &TransportHandle, _b: &[u8]) -> Result<(), TransportError> {
            Ok(())
        }
        fn resize(&self, _h: &TransportHandle, _c: u16, _r: u16) -> Result<(), TransportError> {
            Ok(())
        }
        fn close(&self, _h: &TransportHandle) -> Result<(), TransportError> {
            Ok(())
        }
        /// No PTY behind this fake, so nothing to tap — these tests exercise
        /// the coord-sync outbox, not output streaming.
        fn tap_output(
            &self,
            _h: &TransportHandle,
        ) -> Option<tokio::sync::broadcast::Receiver<String>> {
            None
        }
    }

    fn make_test_intent() -> Intent {
        Intent {
            kind: SessionKind::TerminalShell,
            purpose: "coord-sync test".into(),
            repo: Some("qontinui-runner".into()),
            branch: Some("feat/coord-sync".into()),
            work_unit_slug: None,
            plan_slug: None,
            correlation_topic: None,
            page_id: None,
            declared_paths: vec![],
            share_output: false,
            redact_secrets: None,
            tenant_id: None,
        }
    }

    /// Per-test recorder: every coord call lands here so the test can
    /// assert on count + body.
    #[derive(Default)]
    struct CoordRecorder {
        posts: Vec<JsonValue>,
        patches: Vec<(Uuid, JsonValue)>,
        deletes: Vec<Uuid>,
        steals: Vec<(Uuid, JsonValue)>,
        outputs: Vec<(Uuid, JsonValue)>,
        events: Vec<(Uuid, JsonValue)>,
        /// `(slug, body)` per accepted
        /// `POST /coord/work-units/:slug/register-gate`.
        gates: Vec<(String, JsonValue)>,
        /// The `X-Coord-Caller-Session` each accepted register-gate carried,
        /// index-aligned with `gates` (`None` = no header).
        gate_callers: Vec<Option<String>>,
        /// Bodies accepted by `POST /coord/work-units/upsert`.
        unit_upserts: Vec<JsonValue>,
        /// Bodies accepted by `POST /coord/agent-findings`.
        findings: Vec<JsonValue>,
        /// When true, register-gate answers 404 `work_unit_not_found` for any
        /// slug no upsert has created yet — the shape that drives the lazy
        /// `work_unit_upsert` bootstrap.
        gate_needs_work_unit: bool,
        /// Slugs `POST /coord/work-units/upsert` has created.
        upserted_slugs: Vec<String>,
        /// When true, `POST /coord/agent-findings` answers coord's graceful
        /// degradation: 200 with `{"posted": false}` (the `coord_findings`
        /// migration is not applied).
        findings_degraded: bool,
        /// Bodies accepted by `POST /coord/agent-notifications`.
        notifications: Vec<JsonValue>,
        /// Every `POST /coord/agent-notifications` attempt, accepted or not.
        notification_attempts: usize,
        /// When true, `POST /coord/agent-notifications` refuses a body carrying
        /// `undo` exactly as a coord predating the field does: 400
        /// `invalid_body` naming the unknown field.
        notifications_reject_undo: bool,
        /// When true, the next POST returns 409 + a synthetic row.
        next_post_conflict: bool,
        /// When >0, the next N POSTs return 500.
        next_post_5xx: usize,
        /// `POST /sessions` for any of these session ids ALWAYS answers 500 —
        /// one poisoned session among healthy ones.
        poison_post_ids: Vec<Uuid>,
        /// When true, `POST /sessions` answers coord's unauthenticated 401.
        post_unauthorized: bool,
        /// When true, `POST /sessions`, `PATCH /sessions/:id` and
        /// `POST /coord/agent-findings` all answer 500 — coord down.
        fail_all: bool,
        /// Milliseconds `POST /sessions` sleeps before answering.
        post_delay_ms: u64,
        /// The `Authorization` header each `POST /sessions` carried, in order
        /// (`None` = went out unauthenticated).
        post_auth: Vec<Option<String>>,
        /// When >0, the next N PATCHes return 500. Each attempt decrements it,
        /// so `budget - remaining` counts how many pushes were actually
        /// issued — which is how the bounded-parallel drain is asserted.
        next_patch_5xx: usize,
        /// When true, every PATCH returns 404 (simulates a GC'd / missing
        /// coord row — drives the R2 resume 404-fallback path).
        patch_returns_404: bool,
        /// When set, `POST /sessions/:id/events` answers this status instead
        /// of 201 (still recording the body) — drives the mirror-drop arms
        /// (404 unknown session id, 405 no ingest route).
        events_status: Option<u16>,
        /// The `Authorization` header each `GET /tenant-policy` carried, in
        /// order. `None` = the request went out UNAUTHENTICATED, which is
        /// the fail-closed slot-miss posture and an observable in its own
        /// right.
        tenant_policy_auth: Vec<Option<String>>,
        /// The `?tenant_id=` each `GET /tenant-policy` named, index-aligned
        /// with `tenant_policy_auth` — the other half of the equality coord
        /// checks.
        tenant_policy_queries: Vec<String>,
        /// When set, `GET /tenant-policy` answers this status carrying
        /// coord's own mismatch body instead of 200.
        tenant_policy_status: Option<u16>,
        /// When set, `GET /tenant-policy` answers with this RAW body instead
        /// of coord's own — the intermediary the refusal-body read is bounded
        /// against.
        tenant_policy_body: Option<String>,
    }

    impl CoordRecorder {
        fn new() -> Arc<TokMutex<Self>> {
            Arc::new(TokMutex::new(Self::default()))
        }
    }

    /// Spin up a fake coord server. Returns the base URL + the recorder.
    async fn spawn_fake_coord() -> (String, Arc<TokMutex<CoordRecorder>>) {
        let rec = CoordRecorder::new();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app: Router = Router::new()
            .route(
                "/sessions",
                post(
                    |AxumState(state): AxumState<Arc<TokMutex<CoordRecorder>>>,
                     headers: axum::http::HeaderMap,
                     Json(body): Json<JsonValue>| async move {
                        let delay = state.lock().await.post_delay_ms;
                        if delay > 0 {
                            tokio::time::sleep(Duration::from_millis(delay)).await;
                        }
                        let mut g = state.lock().await;
                        if g.fail_all {
                            return (
                                AxumStatus::INTERNAL_SERVER_ERROR,
                                Json(json!({"error": "fake-down"})),
                            )
                                .into_response();
                        }
                        g.post_auth.push(
                            headers
                                .get("authorization")
                                .and_then(|v| v.to_str().ok())
                                .map(str::to_string),
                        );
                        if g.next_post_5xx > 0 {
                            g.next_post_5xx -= 1;
                            return (
                                AxumStatus::INTERNAL_SERVER_ERROR,
                                Json(json!({"error": "fake-5xx"})),
                            )
                                .into_response();
                        }
                        let body_id = body
                            .get("id")
                            .and_then(|v| v.as_str())
                            .and_then(|s| Uuid::parse_str(s).ok());
                        if body_id.is_some_and(|id| g.poison_post_ids.contains(&id)) {
                            return (
                                AxumStatus::INTERNAL_SERVER_ERROR,
                                Json(json!({"error": "fake-poison"})),
                            )
                                .into_response();
                        }
                        if g.post_unauthorized {
                            return (
                                AxumStatus::UNAUTHORIZED,
                                Json(json!({"error": "operator context missing; SSO required"})),
                            )
                                .into_response();
                        }
                        if g.next_post_conflict {
                            g.next_post_conflict = false;
                            g.posts.push(body.clone());
                            return (
                                AxumStatus::CONFLICT,
                                Json(json!({
                                    "id": body.get("id"),
                                    "device_id": "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
                                    "intent": body.get("intent"),
                                })),
                            )
                                .into_response();
                        }
                        g.posts.push(body.clone());
                        (AxumStatus::CREATED, Json(body)).into_response()
                    },
                ),
            )
            .route(
                "/sessions/{id}",
                patch(
                    |AxumState(state): AxumState<Arc<TokMutex<CoordRecorder>>>,
                     AxumPath(id): AxumPath<Uuid>,
                     Json(body): Json<JsonValue>| async move {
                        let mut g = state.lock().await;
                        if g.fail_all {
                            return (
                                AxumStatus::INTERNAL_SERVER_ERROR,
                                Json(json!({"error": "fake-down"})),
                            )
                                .into_response();
                        }
                        if g.next_patch_5xx > 0 {
                            g.next_patch_5xx -= 1;
                            return (
                                AxumStatus::INTERNAL_SERVER_ERROR,
                                Json(json!({"error": "fake-5xx"})),
                            )
                                .into_response();
                        }
                        g.patches.push((id, body.clone()));
                        if g.patch_returns_404 {
                            return (
                                AxumStatus::NOT_FOUND,
                                Json(json!({"error": "session not found"})),
                            )
                                .into_response();
                        }
                        (AxumStatus::OK, Json(json!({"id": id}))).into_response()
                    },
                )
                .delete(
                    |AxumState(state): AxumState<Arc<TokMutex<CoordRecorder>>>,
                     AxumPath(id): AxumPath<Uuid>| async move {
                        state.lock().await.deletes.push(id);
                        (AxumStatus::OK, Json(json!({"id": id}))).into_response()
                    },
                ),
            )
            .route(
                "/sessions/{id}/steal",
                post(
                    |AxumState(state): AxumState<Arc<TokMutex<CoordRecorder>>>,
                     AxumPath(id): AxumPath<Uuid>,
                     Json(body): Json<JsonValue>| async move {
                        state.lock().await.steals.push((id, body.clone()));
                        (AxumStatus::OK, Json(json!({"id": id}))).into_response()
                    },
                ),
            )
            .route(
                "/sessions/{id}/output",
                post(
                    |AxumState(state): AxumState<Arc<TokMutex<CoordRecorder>>>,
                     AxumPath(id): AxumPath<Uuid>,
                     Json(body): Json<JsonValue>| async move {
                        state.lock().await.outputs.push((id, body.clone()));
                        (AxumStatus::OK, Json(json!({"id": id}))).into_response()
                    },
                ),
            )
            .route(
                "/sessions/{id}/events",
                post(
                    |AxumState(state): AxumState<Arc<TokMutex<CoordRecorder>>>,
                     AxumPath(id): AxumPath<Uuid>,
                     Json(body): Json<JsonValue>| async move {
                        let mut g = state.lock().await;
                        g.events.push((id, body.clone()));
                        let status = g
                            .events_status
                            .and_then(|s| AxumStatus::from_u16(s).ok())
                            .unwrap_or(AxumStatus::CREATED);
                        (status, Json(json!({"id": id}))).into_response()
                    },
                ),
            )
            // The Phase-10 cutover-flag poll. Records the CREDENTIAL each
            // request presented, which is the only observable that separates
            // `coord_get_for(.., Owned(t))` from the defaulting `coord_get`:
            // same url, same query, different bearer.
            .route(
                "/tenant-policy",
                get(
                    |AxumState(state): AxumState<Arc<TokMutex<CoordRecorder>>>,
                     headers: axum::http::HeaderMap,
                     AxumQuery(q): AxumQuery<std::collections::HashMap<String, String>>| async move {
                        let auth = headers
                            .get("authorization")
                            .and_then(|v| v.to_str().ok())
                            .map(str::to_string);
                        let mut g = state.lock().await;
                        g.tenant_policy_auth.push(auth);
                        g.tenant_policy_queries
                            .push(q.get("tenant_id").cloned().unwrap_or_default());
                        match g.tenant_policy_status {
                            // An INTERMEDIARY's body, when one is staged:
                            // whatever it says, verbatim, as a proxy would.
                            Some(s) if g.tenant_policy_body.is_some() => (
                                AxumStatus::from_u16(s).unwrap_or(AxumStatus::FORBIDDEN),
                                g.tenant_policy_body.clone().unwrap_or_default(),
                            )
                                .into_response(),
                            // Coord's REAL mismatch body, verbatim from
                            // `sessions::get_tenant_policy` — the thing the
                            // runner used to throw away.
                            Some(s) => (
                                AxumStatus::from_u16(s).unwrap_or(AxumStatus::FORBIDDEN),
                                Json(json!({"error": "tenant_id does not match principal"})),
                            )
                                .into_response(),
                            None => (
                                AxumStatus::OK,
                                Json(json!({"session_coordination_enabled": true})),
                            )
                                .into_response(),
                        }
                    },
                ),
            )
            .route(
                "/coord/work-units/upsert",
                post(
                    |AxumState(state): AxumState<Arc<TokMutex<CoordRecorder>>>,
                     Json(body): Json<JsonValue>| async move {
                        let mut g = state.lock().await;
                        if let Some(slug) = body.get("slug").and_then(JsonValue::as_str) {
                            g.upserted_slugs.push(slug.to_string());
                        }
                        g.unit_upserts.push(body.clone());
                        (AxumStatus::OK, Json(body)).into_response()
                    },
                ),
            )
            .route(
                "/coord/work-units/{slug}/register-gate",
                post(
                    |AxumState(state): AxumState<Arc<TokMutex<CoordRecorder>>>,
                     AxumPath(slug): AxumPath<String>,
                     headers: axum::http::HeaderMap,
                     Json(body): Json<JsonValue>| async move {
                        let caller = headers
                            .get("x-coord-caller-session")
                            .and_then(|v| v.to_str().ok())
                            .map(str::to_string);
                        let mut g = state.lock().await;
                        if g.gate_needs_work_unit && !g.upserted_slugs.iter().any(|s| *s == slug) {
                            // Byte-shape of coord's own refusal
                            // (`api::gate_routes::register_unit_gate`).
                            return (
                                AxumStatus::NOT_FOUND,
                                Json(json!({
                                    "error": "work_unit_not_found",
                                    "message": "no work unit with this slug in your tenant; \
                                                create it first via POST /coord/work-units/upsert",
                                })),
                            )
                                .into_response();
                        }
                        g.gates.push((slug, body));
                        g.gate_callers.push(caller);
                        (
                            AxumStatus::CREATED,
                            Json(json!({
                                "gate_id": "11111111-1111-1111-1111-111111111111",
                            })),
                        )
                            .into_response()
                    },
                ),
            )
            .route(
                "/coord/agent-findings",
                post(
                    |AxumState(state): AxumState<Arc<TokMutex<CoordRecorder>>>,
                     Json(body): Json<JsonValue>| async move {
                        let mut g = state.lock().await;
                        if g.fail_all {
                            return (
                                AxumStatus::INTERNAL_SERVER_ERROR,
                                Json(json!({"error": "fake-down"})),
                            )
                                .into_response();
                        }
                        if g.findings_degraded {
                            return (
                                AxumStatus::OK,
                                Json(json!({
                                    "posted": false,
                                    "reason": "coord.findings is not provisioned yet",
                                })),
                            )
                                .into_response();
                        }
                        g.findings.push(body.clone());
                        (AxumStatus::CREATED, Json(json!({"posted": true}))).into_response()
                    },
                ),
            )
            .route(
                "/coord/agent-notifications",
                post(
                    |AxumState(state): AxumState<Arc<TokMutex<CoordRecorder>>>,
                     Json(body): Json<JsonValue>| async move {
                        let mut g = state.lock().await;
                        g.notification_attempts += 1;
                        if g.notifications_reject_undo && body.get("undo").is_some() {
                            // Byte-shape of coord's own deserializer refusal
                            // (`notifications::post_agent_notification`).
                            return (
                                AxumStatus::BAD_REQUEST,
                                Json(json!({
                                    "error": "invalid_body",
                                    "detail": "Failed to deserialize the JSON body into the \
                                               target type: unknown field `undo`, expected one \
                                               of `action`, `artifact`, `reversible`, `checks`, \
                                               `repo`, `pr_number`, `actor`",
                                    "message": "`kind` is not a parameter of this door",
                                })),
                            )
                                .into_response();
                        }
                        if body.get("action").and_then(JsonValue::as_str) == Some("bogus") {
                            return (
                                AxumStatus::BAD_REQUEST,
                                Json(json!({"error": "unknown_action"})),
                            )
                                .into_response();
                        }
                        g.notifications.push(body.clone());
                        (
                            AxumStatus::OK,
                            Json(json!({
                                "notification_id": "22222222-2222-2222-2222-222222222222",
                                "kind": "agent_took_sensitive_action",
                                "summary": "agent force-pushed feat/x",
                            })),
                        )
                            .into_response()
                    },
                ),
            )
            .with_state(rec.clone());

        let rec_clone = rec.clone();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (format!("http://{}", addr), rec_clone)
    }

    fn build_outbox(dir: &std::path::Path) -> Arc<OutboxWriter> {
        Arc::new(OutboxWriter::open(dir.join("outbox.jsonl")).unwrap())
    }

    fn build_registry(coord: CoordSync) -> Arc<SessionRegistry> {
        let transports = SessionTransports {
            pty: Arc::new(NoopTransport(SessionKind::TerminalShell)) as DynTransport,
            claude_cli: Arc::new(NoopTransport(SessionKind::TerminalClaude)) as DynTransport,
            workflow: Arc::new(NoopTransport(SessionKind::Workflow)) as DynTransport,
        };
        let registry = SessionRegistry::new(Uuid::new_v4(), transports, coord.clone());
        coord.attach_registry(&registry);
        registry
    }

    /// Wait until `cond` returns true or `timeout` elapses.
    async fn wait_until<F>(timeout: Duration, mut cond: F)
    where
        F: FnMut() -> bool,
    {
        let started = std::time::Instant::now();
        while !cond() {
            if started.elapsed() > timeout {
                panic!("wait_until: timed out after {timeout:?}");
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    #[test]
    fn dual_write_dormant_by_default() {
        // The test CoordSync ctor pins the gate to no-tenant → permanently
        // dormant. mirror_legacy_session must be a no-op: returns None and
        // writes NOTHING to the outbox.
        let dir = tempfile::tempdir().unwrap();
        let outbox = build_outbox(dir.path());
        let coord = CoordSync::new_for_test(
            outbox.clone(),
            "http://127.0.0.1:1".to_string(),
            Duration::from_millis(50),
            Duration::from_secs(10),
        );
        let registry = build_registry(coord.clone());

        assert!(!coord.dual_write_enabled(), "gate defaults closed");
        let mirror = coord.mirror_legacy_session(&registry, make_test_intent());
        assert!(mirror.is_none(), "dormant gate mirrors nothing");
        assert!(
            outbox.pending().unwrap().is_empty(),
            "no outbox row written when dual-write is off — zero behavior change"
        );
        // And no session landed in the registry either.
        assert!(registry.snapshot().is_empty());
    }

    #[test]
    fn dual_write_mirrors_when_flag_on() {
        let _amb = crate::test_env::isolated_ambient();
        // Force the gate open via the DualWriteGate's apply path (the
        // poll loop's effect) and assert mirror_legacy_session registers
        // an external session + writes a Started outbox row.
        let dir = tempfile::tempdir().unwrap();
        let outbox = build_outbox(dir.path());
        let coord = CoordSync::new_for_test(
            outbox.clone(),
            "http://127.0.0.1:1".to_string(),
            Duration::from_millis(50),
            Duration::from_secs(10),
        );
        let registry = build_registry(coord.clone());

        // Open the gate as the poll loop would on a flipped tenant.
        coord.force_dual_write_for_test(true);
        assert!(coord.dual_write_enabled());

        let mirror = coord.mirror_legacy_session(&registry, make_test_intent());
        let id = mirror.expect("flag on → mirror created");
        let desc = registry.describe_by_id(id).unwrap();
        assert_eq!(desc.transport_handle_kind, "external");
        // A Started row is queued for the drain loop.
        let pending = outbox.pending().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].event_kind, SessionEventKind::Started.as_str());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn drain_pushes_started_event_as_post_sessions() {
        let _amb = crate::test_env::isolated_ambient();
        let dir = tempfile::tempdir().unwrap();
        let outbox = build_outbox(dir.path());
        let (base, rec) = spawn_fake_coord().await;
        let coord = CoordSync::new_for_test(
            outbox.clone(),
            base,
            Duration::from_millis(50),
            Duration::from_secs(10),
        );
        let registry = build_registry(coord.clone());
        // Establish the session (writes the `started` row) BEFORE starting
        // the drain loop. On a multi_thread runtime the loop runs at once;
        // if it polls an empty outbox first it sleeps a full TICK_IDLE (5s)
        // before re-polling, colliding with the 5s budget (~50% flake).
        // Ordering the row first makes the drain's first poll find work.
        let _handle = registry.start(make_test_intent()).unwrap();
        let _drain = coord.start_drain_task();

        wait_until(Duration::from_secs(5), || {
            let r = rec.try_lock();
            r.map(|g| !g.posts.is_empty()).unwrap_or(false)
        })
        .await;

        let g = rec.lock().await;
        assert_eq!(g.posts.len(), 1, "exactly one POST /sessions");
        let body = &g.posts[0];
        assert_eq!(body["session_kind"], "terminal_shell");
        assert_eq!(body["intent"]["purpose"], "coord-sync test");
        assert!(body["id"].as_str().is_some(), "id present");
        assert!(body["device_id"].as_str().is_some(), "device_id present");

        drop(g);
        // Outbox row was ACKed.
        wait_until(Duration::from_secs(3), || {
            outbox.pending().map(|p| p.is_empty()).unwrap_or(false)
        })
        .await;
    }

    /// Transcript cloud sync (plan 2026-07-09) — an `output_chunk` outbox
    /// row drains to `POST /sessions/:id/output` carrying
    /// `{chunk_offset, payload_b64, stream}` and is ACKed on 2xx.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn drain_pushes_output_chunk_to_output_endpoint() {
        let _amb = crate::test_env::isolated_ambient();
        let dir = tempfile::tempdir().unwrap();
        let outbox = build_outbox(dir.path());
        let (base, rec) = spawn_fake_coord().await;
        let coord = CoordSync::new_for_test(
            outbox.clone(),
            base,
            Duration::from_millis(50),
            Duration::from_secs(10),
        );
        let _registry = build_registry(coord.clone());

        let machine_id = Uuid::new_v4();
        let session_id = Uuid::new_v4();
        outbox
            .record(
                machine_id,
                session_id,
                SessionEventKind::OutputChunk,
                json!({
                    "stream": "transcript",
                    "chunk_offset": 128,
                    "payload_b64": "aGVsbG8=",
                }),
            )
            .unwrap();
        let _drain = coord.start_drain_task();

        wait_until(Duration::from_secs(5), || {
            let r = rec.try_lock();
            r.map(|g| !g.outputs.is_empty()).unwrap_or(false)
        })
        .await;

        let g = rec.lock().await;
        assert_eq!(g.outputs.len(), 1, "exactly one POST /sessions/:id/output");
        let (posted_id, body) = &g.outputs[0];
        assert_eq!(*posted_id, session_id);
        assert_eq!(body["chunk_offset"], json!(128));
        assert_eq!(body["payload_b64"], json!("aGVsbG8="));
        assert_eq!(body["stream"], json!("transcript"));
        drop(g);

        // The row is ACKed (at-least-once delivery confirmed).
        wait_until(Duration::from_secs(3), || {
            outbox.pending().map(|p| p.is_empty()).unwrap_or(false)
        })
        .await;
    }

    /// ANTI-TRAP TEST (plan
    /// 2026-09-07-no-per-session-record-of-which-transport-rung-carried-a-coord-read,
    /// Phase 1, the whole reason the phase exists).
    ///
    /// `push_record`'s `other =>` catch-all ACKs and DROPS: a kind added to
    /// [`SessionEventKind`] with no arm of its own is written durably to the
    /// outbox, drained, discarded at `debug` level and acked as delivered.
    /// Nothing errors, and `success_metric/coord-mcp-first-rung-reachability`
    /// reads a clean zero.
    ///
    /// So this asserts on the HTTP request coord actually receives, not on a
    /// second list of handled kinds that could drift from the match: delete the
    /// `"coord-transport-rung"` arm and the record falls to the catch-all,
    /// which issues NO request at all, `g.events` stays empty and this test
    /// fails at the `assert_eq!` below.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn drain_pushes_coord_transport_rung_to_events_endpoint() {
        let dir = tempfile::tempdir().unwrap();
        let outbox = build_outbox(dir.path());
        let (base, rec) = spawn_fake_coord().await;
        let coord = CoordSync::new_for_test(
            outbox.clone(),
            base,
            Duration::from_millis(50),
            Duration::from_secs(10),
        );
        let _registry = build_registry(coord.clone());

        let machine_id = Uuid::new_v4();
        let session_id = Uuid::new_v4();
        let agent_session_id = Uuid::new_v4();
        let obs = crate::session::coord_transport_rung::RungObservation::from_declaration(
            Some("loopback_proxy"),
            Some("policy"),
            Some("2"),
            Some("native_mcp"),
            crate::session::coord_transport_rung::OUTCOME_OK,
            "https://coord.qontinui.io/mcp",
            crate::session::coord_transport_rung::OPERATION_READ,
            Some(agent_session_id),
        );
        outbox
            .record(
                machine_id,
                session_id,
                SessionEventKind::CoordTransportRung,
                obs.payload(),
            )
            .unwrap();
        let _drain = coord.start_drain_task();

        wait_until(Duration::from_secs(5), || {
            let r = rec.try_lock();
            r.map(|g| !g.events.is_empty()).unwrap_or(false)
        })
        .await;

        let g = rec.lock().await;
        assert_eq!(
            g.events.len(),
            1,
            "exactly one POST /sessions/:id/events — an empty `events` here means \
             push_record has no `coord-transport-rung` arm and the row was \
             Ack-DROPPED by the catch-all"
        );
        let (posted_id, body) = &g.events[0];
        assert_eq!(*posted_id, session_id, "the lane is the coord.sessions.id");
        assert_eq!(body["event_kind"], json!("coord-transport-rung"));
        assert!(
            body["seq"].as_i64().is_some(),
            "the outbox allocated the seq"
        );
        assert_eq!(body["payload"]["v"], json!(1));
        assert_eq!(body["payload"]["transport"], json!("loopback_proxy"));
        assert_eq!(body["payload"]["reporter"], json!("policy"));
        assert_eq!(body["payload"]["reporter_step"], json!("2"));
        assert_eq!(body["payload"]["attempted"], json!(["native_mcp"]));
        assert_eq!(body["payload"]["operation"], json!("read"));
        assert_eq!(body["payload"]["outcome"], json!("ok"));
        assert_eq!(body["payload"]["off_cascade"], json!(false));
        assert_eq!(
            body["payload"]["agent_session_id"],
            json!(agent_session_id.to_string()),
            "the runner-observed agent-session anchor rides the payload — it is a \
             DIFFERENT id space from the row's session_id"
        );
        // The row's own columns must not be duplicated into the payload.
        assert!(body["payload"].get("session_id").is_none());
        assert!(body["payload"].get("occurred_at").is_none());
        drop(g);

        // ACKed (at-least-once delivery confirmed).
        wait_until(Duration::from_secs(3), || {
            outbox.pending().map(|p| p.is_empty()).unwrap_or(false)
        })
        .await;
    }

    /// The three drop counters are process-wide and each test below asserts
    /// the OTHER two did not move, so they serialise on one lock — the
    /// same shared-static remedy as `series_lock` in `mcp_api`'s tests.
    fn drop_counter_lock() -> &'static TokMutex<()> {
        static LOCK: OnceLock<TokMutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| TokMutex::new(()))
    }

    /// Drain one `coord-transport-rung` row into a fake coord answering
    /// `status` on the events route; returns how much the given drop counter
    /// moved, plus the `/health` accessor's totals read while the guard is
    /// still held. The row must end ACKed (the arm drops, it does not retry),
    /// and neither SIBLING counter may move.
    async fn drain_rung_row_dropped_with(
        status: u16,
        counter: &'static AtomicU64,
        siblings: [&'static AtomicU64; 2],
    ) -> (u64, TransportRungDrainDropped) {
        let _guard = drop_counter_lock().lock().await;
        let dir = tempfile::tempdir().unwrap();
        let outbox = build_outbox(dir.path());
        let (base, rec) = spawn_fake_coord().await;
        rec.lock().await.events_status = Some(status);
        let coord = CoordSync::new_for_test(
            outbox.clone(),
            base,
            Duration::from_millis(50),
            Duration::from_secs(10),
        );
        let _registry = build_registry(coord.clone());

        let obs = crate::session::coord_transport_rung::RungObservation::from_declaration(
            None,
            None,
            None,
            None,
            crate::session::coord_transport_rung::OUTCOME_OK,
            "https://coord.qontinui.io/mcp",
            crate::session::coord_transport_rung::OPERATION_READ,
            None,
        );
        let before = counter.load(Ordering::Relaxed);
        let siblings_before = siblings.map(|c| c.load(Ordering::Relaxed));
        outbox
            .record(
                Uuid::new_v4(),
                Uuid::new_v4(),
                SessionEventKind::CoordTransportRung,
                obs.payload(),
            )
            .unwrap();
        let _drain = coord.start_drain_task();

        wait_until(Duration::from_secs(5), || {
            let r = rec.try_lock();
            r.map(|g| !g.events.is_empty()).unwrap_or(false)
        })
        .await;
        // Ack-dropped, never retried: the outbox empties.
        wait_until(Duration::from_secs(3), || {
            outbox.pending().map(|p| p.is_empty()).unwrap_or(false)
        })
        .await;
        assert!(
            outbox.pending().unwrap().is_empty(),
            "a {status} on the events route is an Ack-drop, not a retry"
        );
        assert_eq!(
            rec.lock().await.events.len(),
            1,
            "exactly one POST — the drop must not be retried"
        );
        assert_eq!(
            siblings.map(|c| c.load(Ordering::Relaxed)),
            siblings_before,
            "the {status} drop must not move a sibling counter"
        );
        (
            counter.load(Ordering::Relaxed) - before,
            transport_rung_drain_dropped(),
        )
    }

    /// Phase 1 of plan 2026-09-18-runner-transport-rung-rows-never-reach-coord-
    /// despite-a-serving-emitter: a 404 Ack-drop is COUNTED at module level so
    /// `GET /health` can read it. Before this the arm only logged.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn drain_counts_a_404_transport_rung_drop() {
        let before = TRANSPORT_RUNG_DROPPED_404.load(Ordering::Relaxed);
        let (moved, totals) = drain_rung_row_dropped_with(
            404,
            &TRANSPORT_RUNG_DROPPED_404,
            [
                &TRANSPORT_RUNG_DROPPED_405,
                &TRANSPORT_RUNG_DROPPED_OTHER_4XX,
            ],
        )
        .await;
        assert_eq!(
            moved, 1,
            "one 404-dropped row moves `drainDropped.404` by one"
        );
        assert!(
            totals.not_found >= before + 1,
            "the /health accessor reads the hoisted 404 counter"
        );
    }

    /// The hoisted 405 counter: it used to be a function-local `static` the
    /// throttled `warn!` alone could read.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn drain_counts_a_405_transport_rung_drop() {
        let before = TRANSPORT_RUNG_DROPPED_405.load(Ordering::Relaxed);
        let (moved, totals) = drain_rung_row_dropped_with(
            405,
            &TRANSPORT_RUNG_DROPPED_405,
            [
                &TRANSPORT_RUNG_DROPPED_404,
                &TRANSPORT_RUNG_DROPPED_OTHER_4XX,
            ],
        )
        .await;
        assert_eq!(
            moved, 1,
            "one 405-dropped row moves `drainDropped.405` by one"
        );
        assert!(
            totals.method_not_allowed >= before + 1,
            "the /health accessor reads the hoisted 405 counter"
        );
    }

    /// Any other 4xx (here a 422) is `PermanentFailure` through the shared
    /// classifier and Ack-dropped at `error!` — the third drain-drop arm,
    /// counted as `drainDropped.other4xx` so `/health` has no uncounted 4xx.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn drain_counts_an_other_4xx_transport_rung_drop() {
        let before = TRANSPORT_RUNG_DROPPED_OTHER_4XX.load(Ordering::Relaxed);
        let (moved, totals) = drain_rung_row_dropped_with(
            422,
            &TRANSPORT_RUNG_DROPPED_OTHER_4XX,
            [&TRANSPORT_RUNG_DROPPED_404, &TRANSPORT_RUNG_DROPPED_405],
        )
        .await;
        assert_eq!(
            moved, 1,
            "one 422-dropped row moves `drainDropped.other4xx` by one"
        );
        assert!(
            totals.other_client_error >= before + 1,
            "the /health accessor reads the other-4xx counter"
        );
    }

    /// Restore-registry mirror (plan 2026-07-09 §3.4, Phase 4) — a
    /// `restore-record` outbox row drains to `POST /sessions/:id/events`
    /// carrying `{seq, event_kind, payload}` with the binding payload
    /// verbatim, and is ACKed on 2xx.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn drain_pushes_restore_record_to_events_endpoint() {
        let _amb = crate::test_env::isolated_ambient();
        let dir = tempfile::tempdir().unwrap();
        let outbox = build_outbox(dir.path());
        let (base, rec) = spawn_fake_coord().await;
        let coord = CoordSync::new_for_test(
            outbox.clone(),
            base,
            Duration::from_millis(50),
            Duration::from_secs(10),
        );
        let _registry = build_registry(coord.clone());

        let machine_id = Uuid::new_v4();
        let session_id = Uuid::new_v4();
        outbox
            .record(
                machine_id,
                session_id,
                SessionEventKind::RestoreRecord,
                json!({
                    "provider": "claude",
                    "authoritative_session_id": "abc-123",
                    "cwd": "C:/repo",
                    "launch_command": "claude --resume abc-123",
                    "restore_tier": "full",
                    "machine_id": machine_id,
                }),
            )
            .unwrap();
        let _drain = coord.start_drain_task();

        wait_until(Duration::from_secs(5), || {
            let r = rec.try_lock();
            r.map(|g| !g.events.is_empty()).unwrap_or(false)
        })
        .await;

        let g = rec.lock().await;
        assert_eq!(g.events.len(), 1, "exactly one POST /sessions/:id/events");
        let (posted_id, body) = &g.events[0];
        assert_eq!(*posted_id, session_id);
        assert_eq!(body["event_kind"], json!("restore-record"));
        assert!(body["seq"].as_i64().is_some(), "seq forwarded");
        assert_eq!(body["payload"]["restore_tier"], json!("full"));
        assert_eq!(
            body["payload"]["authoritative_session_id"],
            json!("abc-123")
        );
        assert_eq!(
            body["payload"]["launch_command"],
            json!("claude --resume abc-123")
        );
        drop(g);

        // The row is ACKed (at-least-once delivery confirmed).
        wait_until(Duration::from_secs(3), || {
            outbox.pending().map(|p| p.is_empty()).unwrap_or(false)
        })
        .await;
    }

    /// `output_chunk_body` forwards the recorded subset and defaults the
    /// stream to "transcript" (the outbox's only output_chunk writer).
    #[test]
    fn output_chunk_body_forwards_fields_and_defaults_stream() {
        let body = output_chunk_body(&json!({
            "stream": "transcript",
            "chunk_offset": 42,
            "payload_b64": "eA==",
            "extraneous": true,
        }));
        assert_eq!(
            body,
            json!({"chunk_offset": 42, "payload_b64": "eA==", "stream": "transcript"})
        );

        let defaulted = output_chunk_body(&json!({"chunk_offset": 0, "payload_b64": "eA=="}));
        assert_eq!(defaulted["stream"], json!("transcript"));
    }

    /// Phase 8b — an explicit spawn-input tenant survives to the coord
    /// `POST /sessions` body verbatim (session-create goes explicit).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn drain_forwards_explicit_session_tenant_in_create_body() {
        let _amb = crate::test_env::isolated_ambient();
        let dir = tempfile::tempdir().unwrap();
        let outbox = build_outbox(dir.path());
        let (base, rec) = spawn_fake_coord().await;
        let coord = CoordSync::new_for_test(
            outbox.clone(),
            base,
            Duration::from_millis(50),
            Duration::from_secs(10),
        );
        let registry = build_registry(coord.clone());
        let tenant = Uuid::from_bytes([0x42; 16]);
        let mut intent = make_test_intent();
        intent.tenant_id = Some(tenant);
        let _handle = registry.start(intent).unwrap();
        let _drain = coord.start_drain_task();

        wait_until(Duration::from_secs(5), || {
            let r = rec.try_lock();
            r.map(|g| !g.posts.is_empty()).unwrap_or(false)
        })
        .await;

        let g = rec.lock().await;
        assert_eq!(
            g.posts[0]["tenant_id"],
            JsonValue::String(tenant.to_string()),
            "explicit spawn tenant must land in the create body"
        );
        assert_eq!(
            g.posts[0]["intent"]["tenant_id"],
            JsonValue::String(tenant.to_string()),
            "the stamped intent carries the session tenant too"
        );
    }

    /// Phase 8b slot-selector resolution: payload intent wins, then the
    /// top-level payload field, then the live registry record, else None
    /// (→ default slot, the pre-8b behavior).
    #[test]
    fn record_session_tenant_prefers_payload_then_registry() {
        let dir = tempfile::tempdir().unwrap();
        let outbox = build_outbox(dir.path());
        let coord = CoordSync::new_for_test(
            outbox,
            "http://127.0.0.1:1".to_string(),
            Duration::from_millis(50),
            Duration::from_secs(10),
        );
        let registry = build_registry(coord.clone());
        let intent_tenant = Uuid::from_bytes([0xA1; 16]);
        let payload_tenant = Uuid::from_bytes([0xB2; 16]);
        let registry_tenant = Uuid::from_bytes([0xC3; 16]);

        let mk = |session_id: Uuid, payload: JsonValue| OutboxRecord {
            machine_id: Uuid::new_v4(),
            session_id,
            seq: 1,
            event_kind: "heartbeat".to_string(),
            payload,
            recorded_at: chrono::Utc::now(),
            acked_at: None,
        };

        // 1. intent.tenant_id wins over the top-level field.
        let rec1 = mk(
            Uuid::new_v4(),
            json!({
                "intent": { "tenant_id": intent_tenant.to_string() },
                "tenant_id": payload_tenant.to_string(),
            }),
        );
        assert_eq!(
            record_session_tenant(&coord.inner, &rec1),
            TenantScope::Owned(intent_tenant)
        );

        // 2. top-level payload tenant_id when the intent has none.
        let rec2 = mk(
            Uuid::new_v4(),
            json!({ "tenant_id": payload_tenant.to_string() }),
        );
        assert_eq!(
            record_session_tenant(&coord.inner, &rec2),
            TenantScope::Owned(payload_tenant)
        );

        // 3. thin payload (heartbeat shape) → the live registry record's
        //    stamped tenant.
        let mut intent = make_test_intent();
        intent.tenant_id = Some(registry_tenant);
        let handle = registry.start(intent).unwrap();
        let rec3 = mk(handle.id(), json!({ "at": chrono::Utc::now() }));
        assert_eq!(
            record_session_tenant(&coord.inner, &rec3),
            TenantScope::Owned(registry_tenant)
        );

        // 4. unknown session + thin payload → Unresolved. NOT `Device`: the
        //    row has an owning session, we just cannot name its tenant, and
        //    that distinction is what arms the D2 degrade on a multi-bound
        //    device while leaving a single-bound one on the default slot.
        let rec4 = mk(Uuid::new_v4(), json!({}));
        assert_eq!(
            record_session_tenant(&coord.inner, &rec4),
            TenantScope::Unresolved
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn drain_treats_409_as_acked_for_started() {
        let _amb = crate::test_env::isolated_ambient();
        let dir = tempfile::tempdir().unwrap();
        let outbox = build_outbox(dir.path());
        let (base, rec) = spawn_fake_coord().await;
        rec.lock().await.next_post_conflict = true;

        let coord = CoordSync::new_for_test(
            outbox.clone(),
            base,
            Duration::from_millis(50),
            Duration::from_secs(10),
        );
        let registry = build_registry(coord.clone());
        // See drain_pushes: start the session before the drain loop so its
        // first poll finds the `started` row instead of sleeping TICK_IDLE.
        let handle = registry.start(make_test_intent()).unwrap();
        let id = handle.id();
        let _drain = coord.start_drain_task();

        wait_until(Duration::from_secs(5), || {
            let r = rec.try_lock();
            r.map(|g| !g.posts.is_empty()).unwrap_or(false)
        })
        .await;
        // Give the drain loop a beat to finish the ACK + state flip
        // after the response is observed.
        wait_until(Duration::from_secs(3), || {
            outbox.pending().map(|p| p.is_empty()).unwrap_or(false)
                && registry
                    .describe_by_id(id)
                    .map(|d| matches!(d.state, SessionState::PendingResolution))
                    .unwrap_or(false)
        })
        .await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn drain_retries_after_5xx() {
        let _amb = crate::test_env::isolated_ambient();
        let dir = tempfile::tempdir().unwrap();
        let outbox = build_outbox(dir.path());
        let (base, rec) = spawn_fake_coord().await;
        rec.lock().await.next_post_5xx = 2; // fail twice, then succeed

        let coord = CoordSync::new_for_test(
            outbox.clone(),
            base,
            Duration::from_millis(50),
            Duration::from_secs(10),
        );
        let registry = build_registry(coord.clone());
        // See drain_pushes: session before the drain loop to avoid the
        // empty-poll TICK_IDLE sleep racing the test budget.
        let _h = registry.start(make_test_intent()).unwrap();
        let _drain = coord.start_drain_task();

        // Backoff escalates after each fail (1s, 2s, …). After two 5xx
        // failures + one success the queue is empty.
        wait_until(Duration::from_secs(30), || {
            outbox.pending().map(|p| p.is_empty()).unwrap_or(false)
        })
        .await;
    }

    /// Bounded-parallel drain, semantic half: a transport error stops the
    /// failing session's chain at the FIRST failure (seq order on reconnect)
    /// and trips the shared abort flag.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn transport_error_stops_the_chain_and_trips_the_abort_flag() {
        let _amb = crate::test_env::isolated_ambient();
        let dir = tempfile::tempdir().unwrap();
        let outbox = build_outbox(dir.path());
        let (base, rec) = spawn_fake_coord().await;
        rec.lock().await.next_patch_5xx = 100;

        let coord = CoordSync::new_for_test(
            outbox.clone(),
            base,
            Duration::from_secs(3600),
            Duration::from_secs(3600),
        );
        let m = Uuid::new_v4();
        let s = Uuid::new_v4();
        let records: Vec<OutboxRecord> = (0..3)
            .map(|_| {
                outbox
                    .record(m, s, SessionEventKind::Heartbeat, json!({}))
                    .unwrap()
            })
            .collect();

        let abort = Arc::new(AtomicBool::new(false));
        let outcome = push_chain(
            coord.inner.clone(),
            records,
            HashMap::new(),
            abort.clone(),
            true,
        )
        .await;

        assert!(outcome.had_transport_error);
        assert!(
            outcome.succeeded.is_empty(),
            "nothing may be acked when the transport is down"
        );
        assert!(
            abort.load(Ordering::Relaxed),
            "a transport error must trip the shared abort flag"
        );
        assert_eq!(
            100 - rec.lock().await.next_patch_5xx,
            1,
            "the chain must stop at the FIRST transport error, not push its tail"
        );
    }

    /// The other half of the abort contract: a chain that starts after the
    /// flag is set issues no requests at all.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_aborted_chain_issues_no_requests() {
        let dir = tempfile::tempdir().unwrap();
        let outbox = build_outbox(dir.path());
        let (base, rec) = spawn_fake_coord().await;
        rec.lock().await.next_patch_5xx = 100;

        let coord = CoordSync::new_for_test(
            outbox.clone(),
            base,
            Duration::from_secs(3600),
            Duration::from_secs(3600),
        );
        let m = Uuid::new_v4();
        let s = Uuid::new_v4();
        let records: Vec<OutboxRecord> = (0..3)
            .map(|_| {
                outbox
                    .record(m, s, SessionEventKind::Heartbeat, json!({}))
                    .unwrap()
            })
            .collect();

        let abort = Arc::new(AtomicBool::new(true));
        let outcome = push_chain(coord.inner.clone(), records, HashMap::new(), abort, false).await;

        assert!(outcome.succeeded.is_empty());
        assert!(!outcome.had_transport_error);
        assert_eq!(
            rec.lock().await.next_patch_5xx,
            100,
            "an already-aborted chain must not issue a single request"
        );
    }

    /// Bounded-parallel drain, budget half: a coord outage with many pending
    /// sessions must cost at most MAX_CONCURRENT_PUSH_CHAINS in-flight
    /// requests, not one per pending record.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_coord_outage_bounds_in_flight_pushes() {
        let dir = tempfile::tempdir().unwrap();
        let outbox = build_outbox(dir.path());
        let (base, rec) = spawn_fake_coord().await;
        rec.lock().await.next_patch_5xx = 1000;

        let coord = CoordSync::new_for_test(
            outbox.clone(),
            base,
            Duration::from_secs(3600),
            Duration::from_secs(3600),
        );
        // 40 independent sessions, one undelivered row each.
        let m = Uuid::new_v4();
        let events: Vec<crate::session::local_store::OutboxEvent> = (0..40)
            .map(|_| {
                crate::session::local_store::OutboxEvent::new(
                    m,
                    Uuid::new_v4(),
                    SessionEventKind::Heartbeat,
                    json!({}),
                )
            })
            .collect();
        outbox.record_batch(events).unwrap();
        assert_eq!(outbox.pending().unwrap().len(), 40);

        let _drain = coord.start_drain_task();
        // The first tick fires immediately, then backs off for TICK_BUSY (1s),
        // so this samples exactly one tick.
        tokio::time::sleep(Duration::from_millis(350)).await;
        let attempted = 1000 - rec.lock().await.next_patch_5xx;
        assert!(
            attempted <= MAX_CONCURRENT_PUSH_CHAINS,
            "an outage must not fan out to one request per pending record: \
             {attempted} issued for 40 pending rows"
        );
        // And nothing was acked — every row survives for the next tick.
        assert_eq!(outbox.pending().unwrap().len(), 40);
    }

    /// A session already known to be failing does NOT trip the shared abort
    /// flag when it is retried — its failure says nothing about coord.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_known_failing_chain_does_not_trip_the_abort_flag() {
        let dir = tempfile::tempdir().unwrap();
        let outbox = build_outbox(dir.path());
        let (base, rec) = spawn_fake_coord().await;
        rec.lock().await.next_patch_5xx = 100;
        let coord = CoordSync::new_for_test(
            outbox.clone(),
            base,
            Duration::from_secs(3600),
            Duration::from_secs(3600),
        );
        let records = vec![outbox
            .record(
                Uuid::new_v4(),
                Uuid::new_v4(),
                SessionEventKind::Heartbeat,
                json!({}),
            )
            .unwrap()];
        let abort = Arc::new(AtomicBool::new(false));
        let outcome = push_chain(
            coord.inner.clone(),
            records,
            HashMap::new(),
            abort.clone(),
            false,
        )
        .await;
        assert!(
            outcome.blocked_on.is_some(),
            "the chain is blocked on its row"
        );
        assert!(
            !abort.load(Ordering::Relaxed),
            "a known-failing session's retry must not stall every other session"
        );
    }

    /// Record a `started` row for `session` straight into the outbox.
    fn record_started(outbox: &OutboxWriter, machine: Uuid, session: Uuid) -> OutboxRecord {
        outbox
            .record(
                machine,
                session,
                SessionEventKind::Started,
                json!({
                    "id": session,
                    "kind": "terminal_shell",
                    "intent": { "purpose": "poison test", "tenant_id": Uuid::nil() },
                }),
            )
            .unwrap()
    }

    /// Item 2 of the 2026-09-23 remediation: a session whose `started` row coord
    /// keeps answering 5xx must not block another session's POST — and, while
    /// coord keeps serving the others, is quarantined to the sidecar.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_stuck_session_does_not_block_another_sessions_post() {
        let _amb = crate::test_env::isolated_ambient();
        let dir = tempfile::tempdir().unwrap();
        let outbox = build_outbox(dir.path());
        let (base, rec) = spawn_fake_coord().await;
        let machine = Uuid::new_v4();
        let stuck = Uuid::new_v4();
        rec.lock().await.poison_post_ids.push(stuck);
        let coord = CoordSync::new_for_test(
            outbox.clone(),
            base,
            Duration::from_secs(3600),
            Duration::from_secs(3600),
        );
        let mut state = DrainState::default();

        // Coord is serving: an unrelated session's row goes through first.
        let other = Uuid::new_v4();
        record_started(&outbox, machine, other);
        drain_tick(&coord.inner, &mut state).await;
        assert!(state.last_ack.is_some());

        // The stuck row fails and its session goes on backoff.
        record_started(&outbox, machine, stuck);
        drain_tick(&coord.inner, &mut state).await;
        assert!(state.retry.contains_key(&stuck));

        // Coord keeps taking OTHER rows while the stuck one sits out its
        // backoff.
        outbox
            .record(machine, other, SessionEventKind::Heartbeat, json!({}))
            .unwrap();
        drain_tick(&coord.inner, &mut state).await;

        // A healthy session appears, with a tail behind its `started`. The
        // stuck one is due again, so both chains run in the same tick — and
        // the stuck one's retry must not abort the healthy chain.
        let healthy = Uuid::new_v4();
        record_started(&outbox, machine, healthy);
        for _ in 0..2 {
            outbox
                .record(machine, healthy, SessionEventKind::Heartbeat, json!({}))
                .unwrap();
        }
        state.retry.get_mut(&stuck).unwrap().next_attempt_at = Instant::now();
        drain_tick(&coord.inner, &mut state).await;

        let g = rec.lock().await;
        assert!(
            g.posts
                .iter()
                .any(|b| b["id"].as_str() == Some(&healthy.to_string())),
            "the healthy session's POST /sessions must go out despite the stuck one"
        );
        assert_eq!(
            g.patches.iter().filter(|(id, _)| *id == healthy).count(),
            2,
            "and its whole tail with it — the stuck session must not abort the batch"
        );
        drop(g);
        let pending = outbox.pending().unwrap();
        assert!(pending.iter().all(|r| r.session_id == stuck));
        assert_eq!(pending.len(), 1, "only the stuck row stays queued");

        // Keep coord serving others while the stuck row keeps failing: it is
        // quarantined, moved to the sidecar, and leaves the outbox.
        for _ in 0..QUARANTINE_AFTER_SERVING_FAILURES + 1 {
            outbox
                .record(machine, other, SessionEventKind::Heartbeat, json!({}))
                .unwrap();
            if let Some(r) = state.retry.get_mut(&stuck) {
                r.next_attempt_at = Instant::now();
            }
            drain_tick(&coord.inner, &mut state).await;
        }
        assert!(
            state.quarantined.contains(&stuck),
            "the stuck session is quarantined"
        );
        // The row moves on the tick after the quarantine decision.
        drain_tick(&coord.inner, &mut state).await;
        assert!(
            outbox.pending().unwrap().is_empty(),
            "nothing left blocking the queue"
        );
        let sidecar = std::fs::read_to_string(quarantine_path(&outbox)).unwrap();
        assert!(
            sidecar.contains(&stuck.to_string()),
            "the quarantined row landed in the sidecar, not nowhere"
        );
    }

    /// Failures while NOTHING reaches coord are an outage: they back off but
    /// never count toward quarantine, so an outage cannot quarantine the fleet.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_outage_never_quarantines_a_session() {
        let _amb = crate::test_env::isolated_ambient();
        let dir = tempfile::tempdir().unwrap();
        let outbox = build_outbox(dir.path());
        let (base, rec) = spawn_fake_coord().await;
        let stuck = Uuid::new_v4();
        rec.lock().await.poison_post_ids.push(stuck);
        let coord = CoordSync::new_for_test(
            outbox.clone(),
            base,
            Duration::from_secs(3600),
            Duration::from_secs(3600),
        );
        record_started(&outbox, Uuid::new_v4(), stuck);
        let mut state = DrainState::default();
        for _ in 0..QUARANTINE_AFTER_SERVING_FAILURES + 2 {
            if let Some(r) = state.retry.get_mut(&stuck) {
                r.next_attempt_at = Instant::now();
            }
            drain_tick(&coord.inner, &mut state).await;
        }
        assert!(state.quarantined.is_empty());
        assert_eq!(state.retry[&stuck].serving_failures, 0);
        assert_eq!(
            outbox.pending().unwrap().len(),
            1,
            "the row is kept for later"
        );
    }

    /// Review finding 2: coord took rows, THEN went down — with a best-effort
    /// row in the queue whose spent budget ACK-drops it locally. That drop is
    /// not coord taking anything, so the failing session is never quarantined.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_outage_after_healthy_acks_never_quarantines_even_with_best_effort_drops() {
        let _amb = crate::test_env::isolated_ambient();
        let dir = tempfile::tempdir().unwrap();
        let outbox = build_outbox(dir.path());
        let (base, rec) = spawn_fake_coord().await;
        let coord = CoordSync::new_for_test(
            outbox.clone(),
            base,
            Duration::from_secs(3600),
            Duration::from_secs(3600),
        );
        let machine = Uuid::new_v4();
        let mut state = DrainState::default();
        record_started(&outbox, machine, Uuid::new_v4());
        drain_tick(&coord.inner, &mut state).await;
        assert!(state.last_ack.is_some(), "coord took the healthy row");

        rec.lock().await.fail_all = true;
        let failing = Uuid::new_v4();
        outbox
            .record(machine, failing, SessionEventKind::Heartbeat, json!({}))
            .unwrap();
        outbox
            .record(
                machine,
                Uuid::new_v4(),
                SessionEventKind::FindingPosted,
                json!({ "topic": "t", "summary": "s" }),
            )
            .unwrap();
        for _ in 0..QUARANTINE_AFTER_SERVING_FAILURES + 4 {
            if let Some(r) = state.retry.get_mut(&failing) {
                r.next_attempt_at = Instant::now();
            }
            drain_tick(&coord.inner, &mut state).await;
        }
        assert!(
            state.quarantined.is_empty(),
            "an outage quarantined a session"
        );
        assert!(
            state.retry[&failing].serving_failures <= 1,
            "only a failure with a coord-taken row around it may count"
        );
        assert!(
            outbox
                .pending()
                .unwrap()
                .iter()
                .any(|r| r.session_id == failing),
            "the failing row is kept for when coord is back"
        );
    }

    #[test]
    fn push_failures_are_typed_by_their_leading_status() {
        assert_eq!(
            classify_push_failure("500 Internal Server Error: {}", false),
            ("server_error", Some(500))
        );
        assert_eq!(
            classify_push_failure("429 Too Many Requests: x", false),
            ("rate_limited", Some(429))
        );
        assert_eq!(
            classify_push_failure(
                "401 Unauthorized: {\"error\":\"operator context missing; SSO required\"}",
                true
            ),
            ("unauthorized", Some(401))
        );
        assert_eq!(
            classify_push_failure("422 Unprocessable Entity: bad", true),
            ("rejected", Some(422))
        );
        assert_eq!(
            classify_push_failure("error sending request for url (https://x/sessions)", false),
            ("network", None)
        );
    }

    /// A refused connect is tagged `[connect]` and carries its OS cause, not
    /// only reqwest's generic head.
    #[tokio::test]
    async fn a_transport_error_is_tagged_and_carries_its_source_chain() {
        // Bind then drop: the port is closed, so the connect is refused.
        let port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let err = reqwest::Client::new()
            .get(format!("http://127.0.0.1:{port}/sessions"))
            .send()
            .await
            .expect_err("nothing listens there");
        let msg = transport_error(&err);
        assert!(msg.starts_with("[connect] "), "{msg}");
        assert!(
            msg.matches(": ").count() >= 1,
            "the source chain is rendered: {msg}"
        );
        assert_eq!(classify_push_failure(&msg, false), ("network", None));
        assert_eq!(
            classify_push_failure("[timeout] operation timed out", false),
            ("timeout", None)
        );
    }

    #[test]
    fn session_outbox_health_is_unknown_until_observed_and_camel_cased() {
        let unobserved = render_session_outbox_health(&SessionOutboxHealth::default());
        assert!(
            unobserved["pending"].is_null(),
            "no tick yet is UNKNOWN, not zero"
        );
        assert!(unobserved["lastFailure"].is_null());

        let at = Utc::now();
        let observed = render_session_outbox_health(&SessionOutboxHealth {
            observed_at: Some(at),
            pending: 3,
            oldest_unacked_at: Some(at),
            last_ack_at: Some(at),
            last_failure: Some(OutboxFailure {
                kind: "server_error",
                status: Some(503),
                at,
            }),
            retrying_sessions: 1,
            quarantined_sessions: 0,
        });
        for key in [
            "pending",
            "oldestUnackedAt",
            "lastAckAt",
            "lastFailure",
            "retryingSessions",
            "quarantinedSessions",
            "observedAt",
        ] {
            assert!(
                !observed[key].is_null(),
                "sessionOutbox.{key} must be present"
            );
        }
        assert_eq!(observed["pending"], 3);
        assert_eq!(observed["lastFailure"]["kind"], "server_error");
        assert_eq!(observed["lastFailure"]["status"], 503);
        assert!(observed["lastFailure"]["at"].is_string());
    }

    /// Item 1: a confirmed registration — coord 2xx → the id is returned, the
    /// `started` row is ACKed, and the drain never POSTs it a second time.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn confirmed_registration_returns_the_id_only_after_coord_2xx() {
        let _amb = crate::test_env::isolated_ambient();
        let dir = tempfile::tempdir().unwrap();
        let outbox = build_outbox(dir.path());
        let (base, rec) = spawn_fake_coord().await;
        let coord = CoordSync::new_for_test(
            outbox.clone(),
            base,
            Duration::from_secs(3600),
            Duration::from_secs(3600),
        );
        let registry = build_registry(coord.clone());
        let _drain = coord.start_drain_task();

        let id = registry
            .register_external_confirmed(make_test_intent(), None, None, Duration::from_secs(5))
            .await
            .expect("coord 2xx confirms the registration");
        assert_eq!(
            rec.lock().await.posts.len(),
            1,
            "one POST /sessions, before returning"
        );
        assert!(
            outbox
                .pending()
                .unwrap()
                .iter()
                .all(|r| !(r.session_id == id && r.event_kind == "started")),
            "the confirmed started row is ACKed"
        );
        // Let the drain run a couple of ticks: it must not re-create the row.
        tokio::time::sleep(Duration::from_millis(1500)).await;
        assert_eq!(
            rec.lock().await.posts.len(),
            1,
            "no double-create by the drain"
        );
        assert!(matches!(
            registry.describe_by_id(id).unwrap().state,
            SessionState::Active
        ));
    }

    /// Item 1: coord 5xx → typed failure, the session is closed locally, and the
    /// unconfirmed `started` row is never delivered afterwards.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn unconfirmed_registration_fails_typed_and_is_never_created_later() {
        let _amb = crate::test_env::isolated_ambient();
        let dir = tempfile::tempdir().unwrap();
        let outbox = build_outbox(dir.path());
        let (base, rec) = spawn_fake_coord().await;
        rec.lock().await.next_post_5xx = 1;
        let coord = CoordSync::new_for_test(
            outbox.clone(),
            base,
            Duration::from_secs(3600),
            Duration::from_secs(3600),
        );
        let registry = build_registry(coord.clone());
        let _drain = coord.start_drain_task();

        let err = registry
            .register_external_confirmed(make_test_intent(), None, None, Duration::from_secs(5))
            .await
            .expect_err("a 5xx is not a confirmation");
        assert_eq!(err.kind, "server_error");
        assert_eq!(err.status, Some(500));
        assert!(
            registry.snapshot().is_empty(),
            "the unconfirmed session is removed locally"
        );
        tokio::time::sleep(Duration::from_millis(1500)).await;
        assert!(
            rec.lock().await.posts.is_empty(),
            "the drain must never create a row the caller was told does not exist"
        );
    }

    /// The merytshost shape (2026-09-23, the PRIMARY fix): an UNPINNED device
    /// (no `machine.json` active tenant) holding THREE bindings with default T.
    /// The session must be owned by T — T in the `POST /sessions` body and T's
    /// device-JWT slot as the bearer — instead of an Unresolved scope that on a
    /// multi-bound device goes out unauthenticated and lands under whatever
    /// tenant coord's device row names.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_unpinned_multi_bound_device_registers_under_its_default_binding() {
        let amb = crate::test_env::isolated_ambient();
        std::env::set_var("QONTINUI_DISABLE_KEYCHAIN", "1");
        amb.write_machine_json("{\"device_id\":\"fixture-device\"}");
        let storage = std::path::PathBuf::from(
            std::env::var("QONTINUI_SECURE_STORAGE_DIR")
                .expect("the ambient fixture pins the secure-storage dir"),
        );
        std::fs::create_dir_all(&storage).unwrap();
        let t = Uuid::now_v7();
        let (b, c) = (Uuid::now_v7(), Uuid::now_v7());
        std::fs::write(
            storage.join("paired_user.json"),
            json!({
                "default_tenant_id": t,
                "bindings": [{ "tenant_id": t }, { "tenant_id": b }, { "tenant_id": c }],
            })
            .to_string(),
        )
        .unwrap();
        assert_eq!(crate::auth::device_binding_count(), 3);
        assert_eq!(
            crate::session::tenant_pin::resolve_tenant_pin(),
            crate::session::tenant_pin::TenantPin::Unpinned,
            "the fixture machine is unpinned"
        );
        // Review finding 3: an UNRESOLVABLE machine must not borrow the
        // default binding — it fails closed with no tenant at all.
        assert_eq!(
            crate::session::tenant_for_new_session(
                crate::session::tenant_pin::TenantPin::Unresolvable
            ),
            None
        );
        let t_jwt = device_jwt_for(&t);
        crate::auth::AuthManager::new()
            .store_tenant_device_jwt(&t, &t_jwt)
            .expect("T's own credential slot");

        let dir = tempfile::tempdir().unwrap();
        let outbox = build_outbox(dir.path());
        let (base, rec) = spawn_fake_coord().await;
        let coord = CoordSync::new_for_test(
            outbox,
            base,
            Duration::from_secs(3600),
            Duration::from_secs(3600),
        );
        let registry = build_registry(coord.clone());
        let id = registry
            .register_external_confirmed(make_test_intent(), None, None, Duration::from_secs(5))
            .await
            .expect("coord 2xx");

        assert_eq!(
            registry.describe_by_id(id).unwrap().intent.tenant_id,
            Some(t)
        );
        assert_eq!(coord.session_tenant(id), TenantScope::Owned(t));
        let g = rec.lock().await;
        assert_eq!(
            g.posts[0]["tenant_id"],
            t.to_string(),
            "T in the create body"
        );
        assert_eq!(
            g.post_auth[0],
            Some(format!("Bearer {t_jwt}")),
            "T's credential on the create, not an unauthenticated push"
        );
    }

    /// Review finding 1: the caller's future is DROPPED mid-confirmation (a
    /// relay reconnect or shutdown). The spawned confirmation still finishes
    /// its cleanup: the session is removed, the unconfirmed `started` row is
    /// gone from the outbox, the hold is released, and nothing ever POSTs it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_dropped_caller_still_gets_the_unconfirmed_session_cleaned_up() {
        let _amb = crate::test_env::isolated_ambient();
        let dir = tempfile::tempdir().unwrap();
        let outbox = build_outbox(dir.path());
        let (base, rec) = spawn_fake_coord().await;
        {
            let mut g = rec.lock().await;
            g.next_post_5xx = 1;
            g.post_delay_ms = 400;
        }
        let coord = CoordSync::new_for_test(
            outbox.clone(),
            base,
            Duration::from_secs(3600),
            Duration::from_secs(3600),
        );
        let registry = build_registry(coord.clone());
        let _drain = coord.start_drain_task();

        let dropped = tokio::time::timeout(
            Duration::from_millis(100),
            registry.register_external_confirmed(
                make_test_intent(),
                None,
                None,
                Duration::from_secs(5),
            ),
        )
        .await;
        assert!(dropped.is_err(), "the caller gave up mid-confirmation");

        wait_until(Duration::from_secs(5), || {
            registry.snapshot().is_empty()
                && coord
                    .inner
                    .held
                    .lock()
                    .map(|h| h.is_empty())
                    .unwrap_or(false)
        })
        .await;
        assert!(
            outbox
                .pending()
                .unwrap()
                .iter()
                .all(|r| r.event_kind != "started"),
            "the unconfirmed started row is discarded"
        );
        tokio::time::sleep(Duration::from_millis(1500)).await;
        assert!(
            rec.lock().await.posts.is_empty(),
            "nothing may create the abandoned session later"
        );
    }

    /// Review finding 4: with the drain ticking as hard as it can, confirmed
    /// registrations are never POSTed twice (a second POST would 409 and flip
    /// the session to PendingResolution).
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_busy_drain_never_reposts_a_confirmed_started_row() {
        let _amb = crate::test_env::isolated_ambient();
        let dir = tempfile::tempdir().unwrap();
        let outbox = build_outbox(dir.path());
        let (base, rec) = spawn_fake_coord().await;
        let coord = CoordSync::new_for_test(
            outbox.clone(),
            base,
            Duration::from_secs(3600),
            Duration::from_secs(3600),
        );
        let registry = build_registry(coord.clone());
        let stop = Arc::new(AtomicBool::new(false));
        let spinner = {
            let inner = coord.inner.clone();
            let stop = stop.clone();
            tokio::spawn(async move {
                let mut state = DrainState::default();
                while !stop.load(Ordering::Relaxed) {
                    drain_tick(&inner, &mut state).await;
                    tokio::task::yield_now().await;
                }
            })
        };
        const N: usize = 30;
        let mut ids = Vec::with_capacity(N);
        for _ in 0..N {
            ids.push(
                registry
                    .register_external_confirmed(
                        make_test_intent(),
                        None,
                        None,
                        Duration::from_secs(5),
                    )
                    .await
                    .expect("coord 2xx"),
            );
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
        stop.store(true, Ordering::Relaxed);
        spinner.await.unwrap();

        assert_eq!(
            rec.lock().await.posts.len(),
            N,
            "exactly one POST per session"
        );
        for id in ids {
            assert!(matches!(
                registry.describe_by_id(id).unwrap().state,
                SessionState::Active
            ));
        }
    }

    /// Round-2 finding 2: the heartbeat loop runs FAST during a confirmation
    /// that fails. The session was never in the registry, so no heartbeat row
    /// is ever queued for it — nothing outlives the failure.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_failed_confirmation_leaves_no_heartbeat_rows_behind() {
        let _amb = crate::test_env::isolated_ambient();
        let dir = tempfile::tempdir().unwrap();
        let outbox = build_outbox(dir.path());
        let (base, rec) = spawn_fake_coord().await;
        {
            let mut g = rec.lock().await;
            g.next_post_5xx = 1;
            g.post_delay_ms = 500;
        }
        let coord = CoordSync::new_for_test(
            outbox.clone(),
            base,
            Duration::from_millis(20),
            Duration::from_secs(60),
        );
        let registry = build_registry(coord.clone());
        let _hb = coord.start_heartbeat_task();

        registry
            .register_external_confirmed(make_test_intent(), None, None, Duration::from_secs(5))
            .await
            .expect_err("a 5xx is not a confirmation");
        tokio::time::sleep(Duration::from_millis(200)).await;
        let pending = outbox.pending().unwrap();
        assert!(
            pending.is_empty(),
            "no row may outlive a failed confirmation: {:?}",
            pending.iter().map(|r| &r.event_kind).collect::<Vec<_>>()
        );
        assert!(registry.snapshot().is_empty());
    }

    /// Round-2 finding 3: one session failing on its own row while coord takes
    /// everything else is not an outage — the tick must not report one (which
    /// would push the whole loop into the 60 s backoff).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_lone_failing_session_is_not_an_outage() {
        let _amb = crate::test_env::isolated_ambient();
        let dir = tempfile::tempdir().unwrap();
        let outbox = build_outbox(dir.path());
        let (base, rec) = spawn_fake_coord().await;
        let stuck = Uuid::new_v4();
        rec.lock().await.poison_post_ids.push(stuck);
        let coord = CoordSync::new_for_test(
            outbox.clone(),
            base,
            Duration::from_secs(3600),
            Duration::from_secs(3600),
        );
        let machine = Uuid::new_v4();
        let other = Uuid::new_v4();
        let mut state = DrainState::default();
        record_started(&outbox, machine, other);
        drain_tick(&coord.inner, &mut state).await;
        record_started(&outbox, machine, stuck);
        drain_tick(&coord.inner, &mut state).await;
        outbox
            .record(machine, other, SessionEventKind::Heartbeat, json!({}))
            .unwrap();
        drain_tick(&coord.inner, &mut state).await;

        // Only the known-failing session is due; coord took a row since its
        // failure, so its retry trips nothing and is not an outage.
        state.retry.get_mut(&stuck).unwrap().next_attempt_at = Instant::now();
        let tick = drain_tick(&coord.inner, &mut state).await;
        assert!(tick.ran_any);
        assert!(
            !tick.outage,
            "a lone poisoned row must not back off the whole loop"
        );

        // Whereas a FRESH failure with nothing taken is.
        rec.lock().await.fail_all = true;
        outbox
            .record(
                machine,
                Uuid::new_v4(),
                SessionEventKind::Heartbeat,
                json!({}),
            )
            .unwrap();
        let tick = drain_tick(&coord.inner, &mut state).await;
        assert!(tick.outage);
    }

    /// The merytshost shape (2026-09-23): an unauthenticated push answered
    /// 401 is unconfirmed, and the error carries coord's stated cause.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_401_is_unconfirmed_and_names_coords_cause() {
        let _amb = crate::test_env::isolated_ambient();
        let dir = tempfile::tempdir().unwrap();
        let outbox = build_outbox(dir.path());
        let (base, rec) = spawn_fake_coord().await;
        rec.lock().await.post_unauthorized = true;
        let coord = CoordSync::new_for_test(
            outbox.clone(),
            base,
            Duration::from_secs(3600),
            Duration::from_secs(3600),
        );
        let registry = build_registry(coord.clone());
        let err = registry
            .register_external_confirmed(make_test_intent(), None, None, Duration::from_secs(5))
            .await
            .expect_err("a 401 is not a confirmation");
        assert_eq!(err.kind, "unauthorized");
        assert_eq!(err.status, Some(401));
        assert!(err.detail.contains("operator context missing"), "{err}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn heartbeat_emits_outbox_rows_for_active_sessions() {
        let _amb = crate::test_env::isolated_ambient();
        let dir = tempfile::tempdir().unwrap();
        let outbox = build_outbox(dir.path());
        let (base, _rec) = spawn_fake_coord().await;
        // Heartbeat every 100ms so the test completes quickly. No
        // drain task — we want to see the outbox row before it gets
        // ACKed.
        let coord = CoordSync::new_for_test(
            outbox.clone(),
            base,
            Duration::from_millis(100),
            Duration::from_secs(60),
        );
        let registry = build_registry(coord.clone());
        // Session before the heartbeat loop, consistent with the drain
        // tests (establish state before starting background loops).
        let handle = registry.start(make_test_intent()).unwrap();
        let id = handle.id();
        let _hb = coord.start_heartbeat_task();

        // Wait for at least one heartbeat outbox row to land.
        wait_until(Duration::from_secs(5), || {
            outbox
                .pending()
                .map(|p| {
                    p.iter()
                        .any(|r| r.event_kind == "heartbeat" && r.session_id == id)
                })
                .unwrap_or(false)
        })
        .await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn heartbeat_records_patch_to_coord() {
        let _amb = crate::test_env::isolated_ambient();
        let dir = tempfile::tempdir().unwrap();
        let outbox = build_outbox(dir.path());
        let (base, rec) = spawn_fake_coord().await;
        let coord = CoordSync::new_for_test(
            outbox.clone(),
            base,
            Duration::from_millis(100),
            Duration::from_secs(60),
        );
        let registry = build_registry(coord.clone());
        // Session before the loops (see drain_pushes).
        let _h = registry.start(make_test_intent()).unwrap();
        let _drain = coord.start_drain_task();
        let _hb = coord.start_heartbeat_task();

        // Wait for at least one PATCH carrying heartbeat=true.
        wait_until(Duration::from_secs(10), || {
            let r = rec.try_lock();
            r.map(|g| g.patches.iter().any(|(_, b)| b["heartbeat"] == true))
                .unwrap_or(false)
        })
        .await;
    }

    /// The round trip the source-text guards above cannot observe: a `finished`
    /// row enqueued in the outbox drains to the path-addressed PATCH, and
    /// coord's 2xx ACK reaches the observer with the row's `claude_session_id`
    /// (plan `2026-09-01-session-finished-marker-and-unfinished-resume` §5.2).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn finished_row_drains_to_patch_and_its_ack_reaches_the_observer() {
        let dir = tempfile::tempdir().unwrap();
        let outbox = build_outbox(dir.path());
        let (base, rec) = spawn_fake_coord().await;
        let coord = CoordSync::new_for_test(
            outbox.clone(),
            base,
            Duration::from_millis(100),
            Duration::from_secs(60),
        );
        let acked: Arc<std::sync::Mutex<Vec<(String, Option<i64>)>>> = Default::default();
        {
            let acked = acked.clone();
            coord.attach_finished_ack_observer(move |csid, at| {
                acked.lock().unwrap().push((csid.to_string(), at))
            });
        }
        let session_id = Uuid::new_v4();
        outbox
            .record(
                Uuid::new_v4(),
                session_id,
                SessionEventKind::Finished,
                json!({ "id": session_id, "claude_session_id": "csid-1", "finished_at": 42 }),
            )
            .unwrap();
        let _drain = coord.start_drain_task();

        wait_until(Duration::from_secs(10), || {
            acked
                .lock()
                .unwrap()
                .iter()
                .any(|a| *a == ("csid-1".to_string(), Some(42)))
        })
        .await;
        let g = rec.lock().await;
        assert!(
            g.patches
                .iter()
                .any(|(id, b)| *id == session_id && b["progress"]["session_status"] == "finished"),
            "the marker must reach coord as PATCH /sessions/:id: {:?}",
            g.patches
        );
    }

    /// A refused `finished` write is ACK-dropped from the outbox so the queue
    /// moves, but it must NOT stamp `finish_synced` — coord never took it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_refused_finished_row_never_reaches_the_ack_observer() {
        let dir = tempfile::tempdir().unwrap();
        let outbox = build_outbox(dir.path());
        let (base, rec) = spawn_fake_coord().await;
        rec.lock().await.patch_returns_404 = true;
        let coord = CoordSync::new_for_test(
            outbox.clone(),
            base,
            Duration::from_millis(100),
            Duration::from_secs(60),
        );
        let acked: Arc<std::sync::Mutex<Vec<(String, Option<i64>)>>> = Default::default();
        {
            let acked = acked.clone();
            coord.attach_finished_ack_observer(move |csid, at| {
                acked.lock().unwrap().push((csid.to_string(), at))
            });
        }
        let session_id = Uuid::new_v4();
        outbox
            .record(
                Uuid::new_v4(),
                session_id,
                SessionEventKind::Finished,
                json!({ "id": session_id, "claude_session_id": "csid-404" }),
            )
            .unwrap();
        let _drain = coord.start_drain_task();

        wait_until(Duration::from_secs(10), || {
            outbox.pending().map(|p| p.is_empty()).unwrap_or(false)
        })
        .await;
        assert!(
            !rec.lock().await.patches.is_empty(),
            "the PATCH was attempted"
        );
        assert!(
            acked.lock().unwrap().is_empty(),
            "a 404-refused marker is dropped, never reported as synced"
        );
    }

    /// Plan A3 — an ABANDONED session (heartbeats ceased) must NOT
    /// self-delete. The runner leaves it for coord's own watcher to age;
    /// the sweep only flips local state to Stale (a UI affordance) and keeps
    /// emitting heartbeats. It must never emit a `closed`→DELETE.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn abandoned_session_goes_stale_but_never_self_deletes() {
        let _amb = crate::test_env::isolated_ambient();
        let dir = tempfile::tempdir().unwrap();
        let outbox = build_outbox(dir.path());
        let (base, rec) = spawn_fake_coord().await;
        // Heartbeat every 50ms; stale at 60ms. There is no autoclose
        // threshold any longer — abandonment is coord's job to reap.
        let coord = CoordSync::new_for_test(
            outbox.clone(),
            base,
            Duration::from_millis(50),
            Duration::from_millis(60),
        );
        let registry = build_registry(coord.clone());

        // Start the session and shove its last_heartbeat 10s into the past —
        // well beyond the OLD 180s/120ms autoclose window — so we prove that
        // even a long-abandoned session is never self-DELETEd.
        let handle = registry.start(make_test_intent()).unwrap();
        let id = handle.id();
        registry.force_heartbeat_to_for_test(id, Utc::now() - chrono::Duration::seconds(10));

        let _drain = coord.start_drain_task();
        let _hb = coord.start_heartbeat_task();

        // The sweep should flip the session to Stale locally.
        wait_until(Duration::from_secs(5), || {
            registry
                .describe_by_id(id)
                .map(|d| d.state == SessionState::Stale)
                .unwrap_or(false)
        })
        .await;

        // Give the loops several more ticks to (NOT) self-delete.
        tokio::time::sleep(Duration::from_millis(400)).await;

        let g = rec.lock().await;
        assert!(
            !g.deletes.contains(&id),
            "abandoned session must NOT be self-DELETEd by the runner — coord reaps it"
        );
        drop(g);

        // Local state stays Stale (never Closed) — only an explicit close
        // would flip it to Closed + emit a DELETE.
        let desc = registry.describe_by_id(id).unwrap();
        assert_eq!(desc.state, SessionState::Stale);
    }

    /// Smoke test: rebuild_create_body honors the payload's intent +
    /// kind so the wire body matches coord's `CreateSessionRequest`.
    #[test]
    fn rebuild_create_body_threads_kind_intent() {
        let rec = OutboxRecord {
            machine_id: Uuid::nil(),
            session_id: Uuid::nil(),
            seq: 1,
            event_kind: "started".into(),
            payload: json!({
                "id": Uuid::nil(),
                "kind": "terminal_claude",
                "intent": {
                    "kind": "terminal_claude",
                    "purpose": "p",
                    "tenant_id": "11111111-1111-1111-1111-111111111111"
                },
                "state": "active"
            }),
            recorded_at: Utc::now(),
            acked_at: None,
        };
        let body = rebuild_create_body(&rec);
        assert_eq!(body["session_kind"], "terminal_claude");
        assert_eq!(body["intent"]["purpose"], "p");
        assert_eq!(body["tenant_id"], "11111111-1111-1111-1111-111111111111");
    }

    /// rebuild_create_body forwards the ambient Claude Code session id as a
    /// first-class field when present, and omits it entirely when absent —
    /// so coord can join session rows to commit `Session-Id` trailers.
    #[test]
    fn rebuild_create_body_forwards_claude_code_session_id() {
        let _amb = crate::test_env::isolated_ambient();
        let with = OutboxRecord {
            machine_id: Uuid::nil(),
            session_id: Uuid::nil(),
            seq: 1,
            event_kind: "started".into(),
            payload: json!({
                "id": Uuid::nil(),
                "kind": "terminal_claude",
                "intent": { "kind": "terminal_claude", "purpose": "p" },
                "claude_code_session_id": "7e0b5d6a-9b8e-4f2c-a3d1-c1d9f0e7a2b4"
            }),
            recorded_at: Utc::now(),
            acked_at: None,
        };
        let body = rebuild_create_body(&with);
        assert_eq!(
            body["claude_code_session_id"],
            "7e0b5d6a-9b8e-4f2c-a3d1-c1d9f0e7a2b4"
        );

        // Absent (manual launch / CI) → key omitted, not null.
        let without = OutboxRecord {
            payload: json!({
                "id": Uuid::nil(),
                "kind": "terminal_shell",
                "intent": { "kind": "terminal_shell", "purpose": "p" }
            }),
            ..with
        };
        let body = rebuild_create_body(&without);
        assert!(body.get("claude_code_session_id").is_none());
    }

    /// Session-automation Phase 0 — `rebuild_create_body` forwards the
    /// `task_run_id` from a Started payload into the `POST /sessions` body so
    /// coord persists `coord.sessions.task_run_id`; absent → key omitted.
    #[test]
    fn rebuild_create_body_forwards_task_run_id() {
        let _amb = crate::test_env::isolated_ambient();
        let with = OutboxRecord {
            machine_id: Uuid::nil(),
            session_id: Uuid::nil(),
            seq: 1,
            event_kind: "started".into(),
            payload: json!({
                "id": Uuid::nil(),
                "kind": "agentic",
                "intent": { "purpose": "ai session" },
                "task_run_id": "11111111-2222-3333-4444-555555555555"
            }),
            recorded_at: Utc::now(),
            acked_at: None,
        };
        let body = rebuild_create_body(&with);
        assert_eq!(body["task_run_id"], "11111111-2222-3333-4444-555555555555");
        assert_eq!(body["session_kind"], "agentic");

        // Absent (terminal pane / peer mirror) → key omitted, not null.
        let without = OutboxRecord {
            payload: json!({
                "id": Uuid::nil(),
                "kind": "terminal_shell",
                "intent": { "kind": "terminal_shell", "purpose": "p" }
            }),
            ..with
        };
        let body = rebuild_create_body(&without);
        assert!(body.get("task_run_id").is_none());
    }

    /// state_change_body forwards only the known coord fields and
    /// always sets `heartbeat: true`.
    #[test]
    fn state_change_body_strips_to_known_fields() {
        let payload = json!({
            "state": "pending_resolution",
            "repo": "qontinui-runner",
            "branch": "main",
            "unrelated": "ignored",
        });
        let body = state_change_body(&payload);
        assert_eq!(body["state"], "pending_resolution");
        assert_eq!(body["repo"], "qontinui-runner");
        assert_eq!(body["branch"], "main");
        assert_eq!(body["heartbeat"], true);
        assert!(body.get("unrelated").is_none());
        assert!(
            body.get("claude_code_session_id").is_none(),
            "a state change that did not confirm an id must not send the key at all"
        );
    }

    /// Phase 2b of plan
    /// `2026-09-02-coord-report-status-unscoped-write-hits-a-peer-session`: the
    /// confirmation event's own field reaches coord's `UpdateSessionRequest`.
    /// It rides ALONE (plus the heartbeat every state change carries) because
    /// coord refuses the whole PATCH when the id is already bound elsewhere —
    /// folding it into an unrelated state change would make that refusal cost
    /// the other fields too.
    #[test]
    fn state_change_body_forwards_a_confirmed_claude_code_session_id() {
        let confirmed = Uuid::new_v4();
        let body = state_change_body(&json!({
            "id": Uuid::nil(),
            "claude_code_session_id": confirmed.to_string(),
        }));
        assert_eq!(body["claude_code_session_id"], confirmed.to_string());
        assert_eq!(body["heartbeat"], true);
        assert!(body.get("state").is_none());
        assert!(body.get("repo").is_none());
        assert!(body.get("branch").is_none());
        assert!(body.get("intent_updates").is_none());
    }

    /// `progress` body nests the flat work-progress fields under `progress`
    /// (coord's `UpdateSessionRequest` shape) and drops unknown keys. A minimal
    /// payload yields a `{progress:{session_status}}` body — coord stamps
    /// last_progress_at=now() when the body omits it.
    #[test]
    fn progress_body_nests_known_fields_under_progress() {
        let minimal = json!({ "id": Uuid::new_v4(), "session_status": "working" });
        let body = progress_body(&minimal);
        assert_eq!(body["progress"]["session_status"], "working");
        // `id` is the outbox routing key, not a progress field — not forwarded.
        assert!(body["progress"].get("id").is_none());
        assert!(body["progress"].get("last_progress_at").is_none());

        let full = json!({
            "session_status": "blocked",
            "last_progress_at": "2026-06-28T10:30:00Z",
            "progress_detail": { "step": "tests", "pct": 60 },
            "unrelated": "ignored",
        });
        let body = progress_body(&full);
        assert_eq!(body["progress"]["session_status"], "blocked");
        assert_eq!(body["progress"]["last_progress_at"], "2026-06-28T10:30:00Z");
        assert_eq!(body["progress"]["progress_detail"]["pct"], 60);
        assert!(body["progress"].get("unrelated").is_none());
    }

    /// Tool-grain half (plan `2026-08-11-coord-hook-sourced-agent-status`):
    /// `tool_name` / `tool_input_digest` / `model` pass through into the nested
    /// `progress` object ALONGSIDE the pre-existing three, which must keep
    /// behaving exactly as before.
    #[test]
    fn progress_body_passes_tool_grain_fields_through() {
        let payload = json!({
            "session_status": "working",
            "tool_name": "Bash",
            "tool_input_digest": "9f86d081884c7d65",
            "model": "opus",
        });
        let body = progress_body(&payload);
        assert_eq!(body["progress"]["session_status"], "working");
        assert_eq!(body["progress"]["tool_name"], "Bash");
        assert_eq!(body["progress"]["tool_input_digest"], "9f86d081884c7d65");
        assert_eq!(body["progress"]["model"], "opus");

        // Absent tool-grain fields are omitted, not nulled — a `progress`
        // report from the older writers is byte-identical to what it was.
        let legacy = json!({
            "session_status": "blocked",
            "last_progress_at": "2026-06-28T10:30:00Z",
            "progress_detail": { "step": "tests" },
        });
        let body = progress_body(&legacy);
        assert_eq!(
            body,
            json!({ "progress": {
                "session_status": "blocked",
                "last_progress_at": "2026-06-28T10:30:00Z",
                "progress_detail": { "step": "tests" },
            }})
        );
    }

    /// `claim_stolen` body carries `reason` + the runner's machine_id.
    #[test]
    fn steal_body_carries_reason_and_machine() {
        let m = Uuid::new_v4();
        let s = Uuid::new_v4();
        let rec = OutboxRecord {
            machine_id: m,
            session_id: s,
            seq: 7,
            event_kind: "claim_stolen".into(),
            payload: json!({"reason": "Need this for the hotfix, releasing in 30"}),
            recorded_at: Utc::now(),
            acked_at: None,
        };
        let body = steal_body(&rec);
        assert_eq!(body["reason"], "Need this for the hotfix, releasing in 30");
        assert_eq!(body["machine_id"], m.to_string());
    }

    /// Belt-and-suspenders: env_u64 falls back on bad input.
    #[test]
    fn env_u64_falls_back_on_unparseable() {
        let _env_lock = env_lock();
        let var = "__COORD_SYNC_TEST_ENV_U64";
        std::env::set_var(var, "not-a-number");
        assert_eq!(env_u64(var, 42), 42);
        std::env::remove_var(var);
    }

    // -----------------------------------------------------------------
    // R2 — probe_resume + resume_external (PATCH-vs-POST + 404 fallback)
    // -----------------------------------------------------------------

    /// `probe_resume` against an existing row returns `Found` and the
    /// probe itself issues the re-activating PATCH (state=active +
    /// heartbeat).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn probe_resume_found_emits_activating_patch() {
        let _amb = crate::test_env::isolated_ambient();
        let dir = tempfile::tempdir().unwrap();
        let outbox = build_outbox(dir.path());
        let (base, rec) = spawn_fake_coord().await;
        let coord = CoordSync::new_for_test(
            outbox,
            base,
            Duration::from_millis(50),
            Duration::from_secs(10),
        );
        let id = Uuid::new_v4();
        // `None` = the default binding, which is what an unpaired test box
        // holds; the tenant argument is the caller's to supply now.
        let probe = coord.probe_resume(id, None).await;
        assert_eq!(probe, ResumeProbe::Found);
        let g = rec.lock().await;
        assert_eq!(g.patches.len(), 1, "probe issues exactly one PATCH");
        let (patched_id, body) = &g.patches[0];
        assert_eq!(*patched_id, id);
        assert_eq!(body["state"], "active");
        assert_eq!(body["heartbeat"], true);
    }

    /// `probe_resume` maps a 404 to `NotFound` (drives the fresh-register
    /// fallback).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn probe_resume_404_is_not_found() {
        let _amb = crate::test_env::isolated_ambient();
        let dir = tempfile::tempdir().unwrap();
        let outbox = build_outbox(dir.path());
        let (base, rec) = spawn_fake_coord().await;
        rec.lock().await.patch_returns_404 = true;
        let coord = CoordSync::new_for_test(
            outbox,
            base,
            Duration::from_millis(50),
            Duration::from_secs(10),
        );
        let probe = coord.probe_resume(Uuid::new_v4(), None).await;
        assert_eq!(probe, ResumeProbe::NotFound);
    }

    /// `probe_resume` maps an unreachable coord (connection refused) to
    /// `Unreachable` (drives the optimistic-resume path).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn probe_resume_transport_error_is_unreachable() {
        let _amb = crate::test_env::isolated_ambient();
        let dir = tempfile::tempdir().unwrap();
        let outbox = build_outbox(dir.path());
        // Port 1 is reserved/unbindable — connection refused.
        let coord = CoordSync::new_for_test(
            outbox,
            "http://127.0.0.1:1".to_string(),
            Duration::from_millis(50),
            Duration::from_secs(10),
        );
        let probe = coord.probe_resume(Uuid::new_v4(), None).await;
        assert_eq!(probe, ResumeProbe::Unreachable);
    }

    /// `resume_external` on an existing row REUSES the persisted id (no new
    /// id, no `Started` POST) and emits a `state_change` outbox row.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn resume_external_reuses_id_when_row_exists() {
        let _amb = crate::test_env::isolated_ambient();
        let dir = tempfile::tempdir().unwrap();
        let outbox = build_outbox(dir.path());
        let (base, _rec) = spawn_fake_coord().await;
        let coord = CoordSync::new_for_test(
            outbox.clone(),
            base,
            Duration::from_millis(50),
            Duration::from_secs(10),
        );
        let registry = build_registry(coord.clone());

        let persisted = Uuid::new_v4();
        let resumed = registry
            .resume_external(persisted, make_test_intent())
            .await
            .unwrap();
        assert_eq!(resumed, persisted, "resume must reuse the persisted id");

        // The in-memory mirror exists under the persisted id.
        let desc = registry.describe_by_id(persisted).unwrap();
        assert_eq!(desc.state, SessionState::Active);
        assert_eq!(desc.transport_handle_kind, "external");

        // A state_change (NOT started) row is queued for the drain loop.
        let pending = outbox.pending().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(
            pending[0].event_kind,
            SessionEventKind::StateChange.as_str()
        );
        assert_eq!(pending[0].session_id, persisted);
    }

    /// `resume_external` on a GC'd row (PATCH 404) falls back to a fresh
    /// `register_external`: a NEW id + a `Started` POST row.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn resume_external_falls_back_to_fresh_on_404() {
        let _amb = crate::test_env::isolated_ambient();
        let dir = tempfile::tempdir().unwrap();
        let outbox = build_outbox(dir.path());
        let (base, rec) = spawn_fake_coord().await;
        rec.lock().await.patch_returns_404 = true;
        let coord = CoordSync::new_for_test(
            outbox.clone(),
            base,
            Duration::from_millis(50),
            Duration::from_secs(10),
        );
        let registry = build_registry(coord.clone());

        let persisted = Uuid::new_v4();
        let fresh = registry
            .resume_external(persisted, make_test_intent())
            .await
            .unwrap();
        assert_ne!(fresh, persisted, "404 fallback mints a NEW id");

        // The fresh id has a mirror; the stale persisted id does not.
        assert!(registry.describe_by_id(fresh).is_ok());
        assert!(registry.describe_by_id(persisted).is_err());

        // A `started` row (fresh register), keyed by the new id.
        let pending = outbox.pending().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].event_kind, SessionEventKind::Started.as_str());
        assert_eq!(pending[0].session_id, fresh);
    }

    /// `resume_external` against an unreachable coord resumes optimistically
    /// under the persisted id (idempotent reconnect; a fresh POST would
    /// fail identically while coord is down).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn resume_external_optimistic_when_unreachable() {
        let _amb = crate::test_env::isolated_ambient();
        let dir = tempfile::tempdir().unwrap();
        let outbox = build_outbox(dir.path());
        let coord = CoordSync::new_for_test(
            outbox.clone(),
            "http://127.0.0.1:1".to_string(),
            Duration::from_millis(50),
            Duration::from_secs(10),
        );
        let registry = build_registry(coord.clone());

        let persisted = Uuid::new_v4();
        let resumed = registry
            .resume_external(persisted, make_test_intent())
            .await
            .unwrap();
        assert_eq!(resumed, persisted);
        assert!(registry.describe_by_id(persisted).is_ok());
        // Still emits a state_change for the drain loop to retry.
        let pending = outbox.pending().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(
            pending[0].event_kind,
            SessionEventKind::StateChange.as_str()
        );
    }

    /// `resume_external` rejects an invalid intent up front (before any
    /// network probe).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn resume_external_rejects_invalid_intent() {
        let dir = tempfile::tempdir().unwrap();
        let outbox = build_outbox(dir.path());
        let coord = CoordSync::new_for_test(
            outbox,
            "http://127.0.0.1:1".to_string(),
            Duration::from_millis(50),
            Duration::from_secs(10),
        );
        let registry = build_registry(coord.clone());
        let mut intent = make_test_intent();
        intent.purpose = "x".into();
        let err = registry
            .resume_external(Uuid::new_v4(), intent)
            .await
            .unwrap_err();
        assert!(matches!(err, crate::session::SessionError::Intent(_)));
    }

    // -----------------------------------------------------------------
    // Closeout kinds — plan
    // 2026-08-28-closeout-has-no-durable-store-when-the-runner-is-offline,
    // Phase 2.
    // -----------------------------------------------------------------

    fn gate_payload() -> JsonValue {
        json!({
            "work_unit_slug": "2026-08-28-closeout-store",
            "predicate": {
                "kind": "pr_merged",
                "repo": "qontinui/qontinui-runner",
                "pr_number": 1266,
            },
            "phase_name": "Phase 2",
            "clearance_audience": "agent",
            "gate_class": "closeout",
            "work_unit_upsert": {
                "title": "Closeout has no durable store",
                "status": "in_progress",
            },
            // The author the write forwarder resolved when it spooled the row.
            "caller_session": "0199a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5b",
        })
    }

    /// A `gate_registration` row drains to
    /// `POST /coord/work-units/:slug/register-gate` — the slug in the PATH,
    /// the register-gate fields in the BODY, and neither runner-side field
    /// (`work_unit_slug` / `work_unit_upsert`) on the wire.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn drain_pushes_gate_registration_to_register_gate_endpoint() {
        let dir = tempfile::tempdir().unwrap();
        let outbox = build_outbox(dir.path());
        let (base, rec) = spawn_fake_coord().await;
        let coord = CoordSync::new_for_test(
            outbox.clone(),
            base,
            Duration::from_millis(50),
            Duration::from_secs(10),
        );
        let _registry = build_registry(coord.clone());

        outbox
            .record(
                Uuid::new_v4(),
                Uuid::new_v4(),
                SessionEventKind::GateRegistration,
                gate_payload(),
            )
            .unwrap();
        let _drain = coord.start_drain_task();

        wait_until(Duration::from_secs(5), || {
            rec.try_lock().map(|g| !g.gates.is_empty()).unwrap_or(false)
        })
        .await;

        let g = rec.lock().await;
        assert_eq!(g.gates.len(), 1, "exactly one register-gate POST");
        let (slug, body) = &g.gates[0];
        assert_eq!(slug, "2026-08-28-closeout-store", "slug rides the PATH");
        assert_eq!(body["predicate"]["kind"], json!("pr_merged"));
        assert_eq!(body["predicate"]["pr_number"], json!(1266));
        assert_eq!(body["phase_name"], json!("Phase 2"));
        assert_eq!(body["clearance_audience"], json!("agent"));
        assert_eq!(body["gate_class"], json!("closeout"));
        // The two runner-side fields never reach coord.
        assert!(body.get("work_unit_slug").is_none());
        assert!(body.get("work_unit_upsert").is_none());
        // The spooled author rides as the HEADER coord stamps the gate's
        // session from — never in the body.
        assert!(body.get("caller_session").is_none());
        assert_eq!(
            g.gate_callers,
            vec![Some("0199a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5b".to_string())],
            "the replay carries the spooled author as X-Coord-Caller-Session"
        );
        // No bootstrap upsert fires when the work unit already exists.
        assert!(g.unit_upserts.is_empty());
        drop(g);

        wait_until(Duration::from_secs(3), || {
            outbox.pending().map(|p| p.is_empty()).unwrap_or(false)
        })
        .await;
    }

    // ── agent_notification (plan
    // 2026-09-18-notifications-are-agent-actions-and-alerts-are-agent-work,
    // Phase 9) ──────────────────────────────────────────────────────────────

    fn force_push_notification() -> JsonValue {
        json!({
            "action": "force_push",
            "artifact": "feat/x",
            "reversible": "restore",
            "repo": "qontinui/qontinui-runner",
            "undo": "1a2b3c4",
        })
    }

    /// Record one `agent_notification` row, drain it against the fake coord,
    /// and wait until the outbox is empty (acked or dropped).
    async fn drain_one_notification(
        payload: JsonValue,
        reject_undo: bool,
    ) -> Arc<TokMutex<CoordRecorder>> {
        let dir = tempfile::tempdir().unwrap();
        let outbox = build_outbox(dir.path());
        let (base, rec) = spawn_fake_coord().await;
        rec.lock().await.notifications_reject_undo = reject_undo;
        let coord = CoordSync::new_for_test(
            outbox.clone(),
            base,
            Duration::from_millis(50),
            Duration::from_secs(10),
        );
        let _registry = build_registry(coord.clone());
        outbox
            .record(
                Uuid::new_v4(),
                Uuid::new_v4(),
                SessionEventKind::AgentNotification,
                payload,
            )
            .unwrap();
        let _drain = coord.start_drain_task();
        wait_until(Duration::from_secs(5), || {
            outbox.pending().map(|p| p.is_empty()).unwrap_or(false)
        })
        .await;
        rec
    }

    /// The row drains to `POST /coord/agent-notifications` with the payload
    /// forwarded VERBATIM — `undo` included — against a coord that serves it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn drain_pushes_agent_notification_verbatim() {
        let payload = force_push_notification();
        let rec = drain_one_notification(payload.clone(), false).await;
        let g = rec.lock().await;
        assert_eq!(g.notification_attempts, 1, "one POST, no retry");
        assert_eq!(g.notifications, vec![payload], "body forwarded verbatim");
    }

    /// A coord predating the `undo` field 400s on it; the drain retries ONCE
    /// without `undo`, so the notification still lands.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn drain_retries_agent_notification_without_undo_on_an_old_coord() {
        let payload = force_push_notification();
        let rec = drain_one_notification(payload.clone(), true).await;
        let g = rec.lock().await;
        assert_eq!(g.notification_attempts, 2, "the refusal, then one retry");
        let mut expected = payload;
        expected.as_object_mut().unwrap().remove("undo");
        assert_eq!(
            g.notifications,
            vec![expected],
            "the retry carries every field but `undo`"
        );
    }

    /// Any OTHER 400 is a body the queue cannot fix: no retry, and the row is
    /// dropped rather than wedging the lane.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn drain_does_not_retry_an_agent_notification_refused_for_another_reason() {
        let mut payload = force_push_notification();
        payload["action"] = json!("bogus");
        let rec = drain_one_notification(payload, false).await;
        let g = rec.lock().await;
        assert!(g.notifications.is_empty(), "nothing stored");
        assert_eq!(
            g.notification_attempts, 1,
            "a non-undo 400 is never retried"
        );
    }

    /// The undo retry happens at most ONCE per push: a retried body refused
    /// for another reason is dropped, not retried again.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_undo_retry_fires_once_then_the_row_drops() {
        let mut payload = force_push_notification();
        payload["action"] = json!("bogus");
        let rec = drain_one_notification(payload, true).await;
        let g = rec.lock().await;
        assert!(g.notifications.is_empty(), "nothing stored");
        assert_eq!(
            g.notification_attempts, 2,
            "the undo refusal, one retry without undo, then the drop"
        );
    }

    #[test]
    fn unknown_undo_refusal_is_recognised_only_for_undo() {
        assert!(rejects_unknown_undo_field(
            r#"{"error":"invalid_body","detail":"Failed to deserialize the JSON body into the target type: unknown field `undo`, expected one of `action`"}"#
        ));
        assert!(rejects_unknown_undo_field(
            "Failed to deserialize: unknown field `undo`, expected one of"
        ));
        assert!(!rejects_unknown_undo_field(
            r#"{"error":"invalid_body","detail":"unknown field `kind`, expected one of `action`"}"#
        ));
        assert!(!rejects_unknown_undo_field(r#"{"error":"unknown_action"}"#));
        // A newer coord's accepted-fields MESSAGE may name `undo` without it
        // being the refusal — only `detail`'s unknown-field text counts.
        assert!(!rejects_unknown_undo_field(
            r#"{"error":"invalid_body","detail":"missing field `artifact`","message":"accepted: action, artifact, unknown field `undo`"}"#
        ));
    }

    /// A `finding_posted` row drains to `POST /coord/agent-findings` with the
    /// payload forwarded VERBATIM (coord's body is `deny_unknown_fields`, so
    /// there is nothing to reshape and nothing may be added).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn drain_pushes_finding_to_agent_findings_endpoint() {
        let dir = tempfile::tempdir().unwrap();
        let outbox = build_outbox(dir.path());
        let (base, rec) = spawn_fake_coord().await;
        let coord = CoordSync::new_for_test(
            outbox.clone(),
            base,
            Duration::from_millis(50),
            Duration::from_secs(10),
        );
        let _registry = build_registry(coord.clone());

        let payload = json!({
            "title": "coord unreachable at closeout",
            "body": "spooled to the session outbox; replayed by the drain",
            "kind": "investigation",
            "scope": "tenant",
            "topic": "coord",
            "resource_keys": ["qontinui-runner"],
        });
        outbox
            .record(
                Uuid::new_v4(),
                Uuid::new_v4(),
                SessionEventKind::FindingPosted,
                payload.clone(),
            )
            .unwrap();
        let _drain = coord.start_drain_task();

        wait_until(Duration::from_secs(5), || {
            rec.try_lock()
                .map(|g| !g.findings.is_empty())
                .unwrap_or(false)
        })
        .await;

        let g = rec.lock().await;
        assert_eq!(
            g.findings.len(),
            1,
            "exactly one POST /coord/agent-findings"
        );
        assert_eq!(g.findings[0], payload, "body forwarded verbatim");
        drop(g);

        wait_until(Duration::from_secs(3), || {
            outbox.pending().map(|p| p.is_empty()).unwrap_or(false)
        })
        .await;
    }

    /// A replayed closeout gate whose work unit coord has never seen gets the
    /// 404 `work_unit_not_found`, and the drain recovers it LAZILY: upsert the
    /// recorded bootstrap, then register once more. The upsert carries the
    /// record's own slug and only the columns the producer recorded.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn gate_registration_404_bootstraps_the_work_unit_then_registers() {
        let dir = tempfile::tempdir().unwrap();
        let outbox = build_outbox(dir.path());
        let (base, rec) = spawn_fake_coord().await;
        rec.lock().await.gate_needs_work_unit = true;
        let coord = CoordSync::new_for_test(
            outbox.clone(),
            base,
            Duration::from_millis(50),
            Duration::from_secs(10),
        );
        let _registry = build_registry(coord.clone());

        outbox
            .record(
                Uuid::new_v4(),
                Uuid::new_v4(),
                SessionEventKind::GateRegistration,
                gate_payload(),
            )
            .unwrap();
        let _drain = coord.start_drain_task();

        wait_until(Duration::from_secs(5), || {
            rec.try_lock().map(|g| !g.gates.is_empty()).unwrap_or(false)
        })
        .await;

        let g = rec.lock().await;
        assert_eq!(g.unit_upserts.len(), 1, "exactly one bootstrap upsert");
        let upsert = &g.unit_upserts[0];
        assert_eq!(upsert["slug"], json!("2026-08-28-closeout-store"));
        assert_eq!(upsert["title"], json!("Closeout has no durable store"));
        assert_eq!(upsert["status"], json!("in_progress"));
        // The gate landed on the second attempt, at the same slug.
        assert_eq!(g.gates.len(), 1);
        assert_eq!(g.gates[0].0, "2026-08-28-closeout-store");
        // …and that second POST still carries the spooled author. This is the
        // offline case the bootstrap exists for, so a header on the first
        // POST only would land exactly the gates that need it authorless.
        assert_eq!(
            g.gate_callers,
            vec![Some("0199a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5b".to_string())],
            "the post-bootstrap register-gate carries X-Coord-Caller-Session"
        );
        drop(g);

        wait_until(Duration::from_secs(3), || {
            outbox.pending().map(|p| p.is_empty()).unwrap_or(false)
        })
        .await;
    }

    /// A 404 with NO recorded bootstrap can never succeed on retry, so the row
    /// is Ack-dropped rather than left to wedge the queue behind it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn gate_registration_404_without_bootstrap_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let outbox = build_outbox(dir.path());
        let (base, rec) = spawn_fake_coord().await;
        rec.lock().await.gate_needs_work_unit = true;
        let coord = CoordSync::new_for_test(
            outbox.clone(),
            base,
            Duration::from_millis(50),
            Duration::from_secs(10),
        );
        let _registry = build_registry(coord.clone());

        let mut payload = gate_payload();
        payload.as_object_mut().unwrap().remove("work_unit_upsert");
        outbox
            .record(
                Uuid::new_v4(),
                Uuid::new_v4(),
                SessionEventKind::GateRegistration,
                payload,
            )
            .unwrap();
        // A session lifecycle row queued behind it must still drain.
        let session_id = Uuid::new_v4();
        outbox
            .record(
                Uuid::new_v4(),
                session_id,
                SessionEventKind::Heartbeat,
                json!({"id": session_id}),
            )
            .unwrap();
        let _drain = coord.start_drain_task();

        wait_until(Duration::from_secs(5), || {
            outbox.pending().map(|p| p.is_empty()).unwrap_or(false)
        })
        .await;

        let g = rec.lock().await;
        assert!(g.gates.is_empty(), "the gate never registered");
        assert!(g.unit_upserts.is_empty(), "and nothing was upserted");
        assert_eq!(g.patches.len(), 1, "the heartbeat behind it still drained");
    }

    /// A payload whose slug cannot be a URL path segment can never succeed, so
    /// the drain fails it permanently WITHOUT issuing a request.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn gate_registration_with_unusable_slug_is_dropped_unsent() {
        let dir = tempfile::tempdir().unwrap();
        let outbox = build_outbox(dir.path());
        let (base, rec) = spawn_fake_coord().await;
        let coord = CoordSync::new_for_test(
            outbox.clone(),
            base,
            Duration::from_millis(50),
            Duration::from_secs(10),
        );
        let _registry = build_registry(coord.clone());

        let mut payload = gate_payload();
        payload["work_unit_slug"] = json!("has/a/separator");
        outbox
            .record(
                Uuid::new_v4(),
                Uuid::new_v4(),
                SessionEventKind::GateRegistration,
                payload,
            )
            .unwrap();
        let _drain = coord.start_drain_task();

        wait_until(Duration::from_secs(5), || {
            outbox.pending().map(|p| p.is_empty()).unwrap_or(false)
        })
        .await;

        let g = rec.lock().await;
        assert!(g.gates.is_empty());
        assert!(g.unit_upserts.is_empty());
    }

    /// `gate_registration_body` forwards exactly coord's `UnitGateRequest`
    /// fields — dropping the two runner-side ones and every null, so coord's
    /// `#[serde(default)]` / `default_clearance_audience` apply rather than a
    /// literal `null` failing to deserialize.
    #[test]
    fn gate_registration_body_forwards_only_the_wire_fields() {
        let body = gate_registration_body(&gate_payload());
        assert_eq!(body["phase_name"], json!("Phase 2"));
        assert_eq!(body["predicate"]["kind"], json!("pr_merged"));
        assert_eq!(body["clearance_audience"], json!("agent"));
        assert_eq!(body["gate_class"], json!("closeout"));
        assert!(body.get("work_unit_slug").is_none());
        assert!(body.get("work_unit_upsert").is_none());
        assert!(body.get("caller_session").is_none());
        assert!(body.get("continuation_spawn").is_none());

        let nulled = gate_registration_body(&json!({
            "predicate": {"kind": "unit_ready"},
            "phase_name": "P1",
            "continuation_spawn": null,
            "clearance_audience": null,
            "gate_class": null,
        }));
        assert_eq!(
            nulled,
            json!({"predicate": {"kind": "unit_ready"}, "phase_name": "P1"}),
            "nulls are dropped, not forwarded"
        );
    }

    /// The slug must be usable as a single URL path segment — the runner has
    /// no percent-encoder here, so anything else is refused.
    #[test]
    fn gate_registration_slug_accepts_only_a_path_segment() {
        assert_eq!(
            gate_registration_slug(&json!({"work_unit_slug": "  a-slug-2026  "})),
            Some("a-slug-2026".to_string()),
            "trimmed"
        );
        for bad in [
            json!({}),
            json!({"work_unit_slug": ""}),
            json!({"work_unit_slug": "   "}),
            json!({"work_unit_slug": 7}),
            json!({"work_unit_slug": "a/b"}),
            json!({"work_unit_slug": "a?b"}),
            json!({"work_unit_slug": "a#b"}),
            json!({"work_unit_slug": "a b"}),
        ] {
            assert_eq!(gate_registration_slug(&bad), None, "rejected: {bad}");
        }
    }

    /// The spooled author replays only as a strict UUID; anything else (or
    /// nothing) replays headerless rather than sending a value coord can only
    /// refuse.
    #[test]
    fn gate_registration_caller_session_is_a_strict_uuid_or_nothing() {
        let sid = Uuid::parse_str("0199a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5b").unwrap();
        assert_eq!(gate_registration_caller_session(&gate_payload()), Some(sid));
        assert_eq!(
            gate_registration_caller_session(&json!({
                "caller_session": " 0199a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5b "
            })),
            Some(sid),
            "trimmed"
        );
        for absent in [
            json!({}),
            json!({"caller_session": null}),
            json!({"caller_session": 7}),
            json!({"caller_session": ""}),
            json!({"caller_session": "not-a-uuid"}),
        ] {
            assert_eq!(
                gate_registration_caller_session(&absent),
                None,
                "replays headerless: {absent}"
            );
        }
    }

    /// The bootstrap body always takes its slug from the record, never from
    /// the bootstrap object, and drops nulls so an upsert cannot blank a
    /// column it was not asked to set. No bootstrap recorded → `None`.
    #[test]
    fn gate_registration_upsert_body_is_slug_pinned_and_null_free() {
        let body = gate_registration_upsert_body(&gate_payload()).unwrap();
        assert_eq!(
            body,
            json!({
                "slug": "2026-08-28-closeout-store",
                "title": "Closeout has no durable store",
                "status": "in_progress",
            })
        );

        // A bootstrap that names a different slug does not get to use it.
        let pinned = gate_registration_upsert_body(&json!({
            "work_unit_slug": "real-slug",
            "work_unit_upsert": {"slug": "other-slug", "status": null},
        }))
        .unwrap();
        assert_eq!(pinned, json!({"slug": "real-slug"}));

        assert!(gate_registration_upsert_body(&json!({"work_unit_slug": "s"})).is_none());
        assert!(gate_registration_upsert_body(&gate_registration_no_slug()).is_none());
    }

    fn gate_registration_no_slug() -> JsonValue {
        json!({"work_unit_upsert": {"title": "t"}})
    }

    /// Coord's degraded findings answer is a **200** carrying
    /// `{"posted": false}`. Retrying cannot apply a migration, so the row is
    /// dropped rather than left to wedge the queue — and the drain says so
    /// instead of reporting the finding as delivered.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn finding_degraded_200_is_dropped_not_retried() {
        let dir = tempfile::tempdir().unwrap();
        let outbox = build_outbox(dir.path());
        let (base, rec) = spawn_fake_coord().await;
        rec.lock().await.findings_degraded = true;
        let coord = CoordSync::new_for_test(
            outbox.clone(),
            base,
            Duration::from_millis(50),
            Duration::from_secs(10),
        );
        let _registry = build_registry(coord.clone());

        outbox
            .record(
                Uuid::new_v4(),
                Uuid::new_v4(),
                SessionEventKind::FindingPosted,
                // `topic` included: it is one of coord's three required
                // fields, so a fixture without it would be pinning a body the
                // producer refuses to spool in the first place.
                json!({"title": "t", "body": "b", "topic": "coord"}),
            )
            .unwrap();
        let _drain = coord.start_drain_task();

        wait_until(Duration::from_secs(5), || {
            outbox.pending().map(|p| p.is_empty()).unwrap_or(false)
        })
        .await;

        // Nothing was stored coord-side, and nothing is left to retry.
        assert!(rec.lock().await.findings.is_empty());
    }

    /// The best-effort posture covers the two closeout kinds as well as helper
    /// tasks — and does NOT cover session lifecycle events, which must break
    /// the batch so their seq order survives a reconnect.
    #[test]
    fn best_effort_posture_covers_the_closeout_kinds() {
        for kind in [
            SessionEventKind::HelperTaskCreated,
            SessionEventKind::GateRegistration,
            SessionEventKind::FindingPosted,
        ] {
            assert!(is_best_effort_kind(kind.as_str()), "{kind:?}");
        }
        for kind in [
            SessionEventKind::Started,
            SessionEventKind::Heartbeat,
            SessionEventKind::Closed,
            SessionEventKind::StateChange,
            SessionEventKind::OutputChunk,
        ] {
            assert!(!is_best_effort_kind(kind.as_str()), "{kind:?}");
        }
    }

    // ========================================================================
    // P3 of plan
    // `2026-09-17-device-holds-one-credential-slot-so-a-session-cannot-work-a-bound-tenant`
    // — the tenant-policy poll presents the credential for the tenant it is
    // querying, and reports a standing refusal once rather than once a minute.
    // ========================================================================

    /// The tenant-policy fetch's own body, isolated from the rest of this
    /// file — the substrate both source guards below read.
    ///
    /// Two traps it exists to close, both of which bit the first cut:
    ///
    /// * the needle is ASSEMBLED rather than written literally, so it does not
    ///   match this test file's own mention of the function and the
    ///   uniqueness check below is meaningful. A plain
    ///   `split_once("async fn fetch_session_coordination_flag")` took the
    ///   FIRST hit, so a future `..._v2` declared above would have silently
    ///   retargeted every guard at the wrong function while still passing;
    /// * it asserts the declaration is UNIQUE, so that retargeting is a named
    ///   failure rather than a silent one.
    fn tenant_policy_fetch_body() -> &'static str {
        let src = include_str!("coord_sync.rs");
        // Assembled at runtime: a literal here would appear in `src` itself.
        let needle = format!("async fn {}(", "fetch_session_coordination_flag");
        assert_eq!(
            src.matches(needle.as_str()).count(),
            1,
            "exactly one `{needle}` declaration must exist; a second one makes every source \
             guard in this module silently pin whichever comes first"
        );
        let (_, after) = src
            .split_once(needle.as_str())
            .expect("the tenant-policy fetch must exist");
        after
            .split_once("\n}\n")
            .expect("the fetch body must terminate")
            .0
    }

    /// SOURCE GUARD — the call-site half of the mutation proof for this phase.
    ///
    /// The defect was a CALL-SITE choice, not a value: the poll built its
    /// request with [`crate::coord_http::coord_get`], which asserts
    /// [`TenantScope::Device`] and therefore presents the LEGACY
    /// `access_token` slot — the DEFAULT binding's JWT — no matter which
    /// tenant the `?tenant_id=` query names. Coord's
    /// `sessions::get_tenant_policy` requires those two to be equal, so the
    /// request was refused `403`.
    ///
    /// This guard is KEPT, but it is no longer the only proof:
    /// [`the_poll_presents_the_queried_tenants_credential`] below asserts the
    /// same property on an OBSERVABLE — the `Authorization` header a fake
    /// coord actually received. A source guard alone punishes cleanup, so it
    /// pins the two structural tokens (`coord_get_for(` and the scope call)
    /// rather than a 99-column literal that rustfmt reflows the moment anyone
    /// renames `url` or hoists the scope into a `let`.
    #[test]
    fn the_tenant_policy_poll_presents_the_tenant_it_queries() {
        let body = tenant_policy_fetch_body();

        assert!(
            body.contains("coord_get_for(") && body.contains("tenant_policy_scope(tenant_id)"),
            "the tenant-policy poll must present the credential for the tenant it QUERIES, via \
             the tenant-stating `coord_get_for` seam. Body was:\n{body}"
        );
        assert!(
            !body.contains("coord_http::coord_get(") && !body.contains("coord_get(&inner.http"),
            "the tenant-policy poll must NOT use the defaulting `coord_get`: it asserts \
             TenantScope::Device, which presents the legacy/default slot regardless of the \
             tenant in the query string — the exact mismatch coord answers 403 to. Body \
             was:\n{body}"
        );
    }

    /// BEHAVIOURAL MUTATION PROOF — the credential that actually went on the
    /// wire, asserted on an observable rather than on a substring.
    ///
    /// The first cut of this phase shipped only the source guard above, on the
    /// stated grounds that "nothing about the poll's VALUES changes when the
    /// call site regresses". That is true of the url and the query and false
    /// of the thing that matters: the `Authorization` header. This file
    /// already had a fake-coord harness with a request recorder, so the
    /// observable was there all along.
    ///
    /// The device here is the measured situation — bound to several tenants,
    /// legacy slot holding the DEFAULT binding's JWT, polling about a
    /// NON-default tenant it does hold a slot for. Revert the call site to
    /// `coord_get(..)` and the recorded bearer becomes the default binding's,
    /// which is the cross-tenant presentation coord answers `403` to.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_poll_presents_the_queried_tenants_credential() {
        let _amb = crate::test_env::isolated_ambient();
        // The fixture pins QONTINUI_SECURE_STORAGE_DIR into its own tempdir
        // and restores every ambient key on drop; the keychain is not one of
        // the keys it steers, and a real one would put this test's tokens on
        // the developer's box.
        std::env::set_var("QONTINUI_DISABLE_KEYCHAIN", "1");

        let queried = Uuid::now_v7();
        let default_tenant = Uuid::now_v7();
        let queried_jwt = device_jwt_for(&queried);
        let default_jwt = device_jwt_for(&default_tenant);
        assert_ne!(queried_jwt, default_jwt);

        let am = crate::auth::AuthManager::new();
        am.store_tokens(&default_jwt, "")
            .expect("the legacy slot holds the DEFAULT binding's JWT");
        am.store_tenant_device_jwt(&queried, &queried_jwt)
            .expect("and this device also holds the queried tenant's own slot");

        let dir = tempfile::tempdir().unwrap();
        let outbox = build_outbox(dir.path());
        let (base, rec) = spawn_fake_coord().await;
        let coord = CoordSync::new_for_test(
            outbox,
            base,
            Duration::from_secs(3600),
            Duration::from_secs(3600),
        );

        let enabled = fetch_session_coordination_flag(&coord.inner, queried)
            .await
            .expect("the fake coord answers 200");
        assert!(enabled);

        let g = rec.lock().await;
        assert_eq!(
            g.tenant_policy_queries,
            vec![queried.to_string()],
            "the poll must ask about the tenant it was given"
        );
        assert_eq!(
            g.tenant_policy_auth,
            vec![Some(format!("Bearer {queried_jwt}"))],
            "the request must carry the QUERIED tenant's credential — the equality coord \
             checks. Carrying the default binding's is the defect this phase closes"
        );
        assert!(
            !g.tenant_policy_auth
                .iter()
                .flatten()
                .any(|h| h.contains(&default_jwt)),
            "the default binding's JWT must never travel on a poll about another tenant"
        );
    }

    /// The other half of the same observable: a tenant this device holds NO
    /// usable slot for sends the request UNAUTHENTICATED and lets coord
    /// answer. Substituting the legacy slot would be the original bug wearing
    /// the fix's name, and unlike the scope-level assertions above this one
    /// watches the wire.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_slot_miss_goes_out_unauthenticated_rather_than_borrowing_the_default() {
        let _amb = crate::test_env::isolated_ambient();
        std::env::set_var("QONTINUI_DISABLE_KEYCHAIN", "1");

        let slotless = Uuid::now_v7();
        let default_jwt = device_jwt_for(&Uuid::now_v7());
        let am = crate::auth::AuthManager::new();
        am.store_tokens(&default_jwt, "").expect("legacy slot");

        let dir = tempfile::tempdir().unwrap();
        let outbox = build_outbox(dir.path());
        let (base, rec) = spawn_fake_coord().await;
        let coord = CoordSync::new_for_test(
            outbox,
            base,
            Duration::from_secs(3600),
            Duration::from_secs(3600),
        );

        let _ = fetch_session_coordination_flag(&coord.inner, slotless).await;

        let g = rec.lock().await;
        assert_eq!(
            g.tenant_policy_auth,
            vec![None],
            "a slot MISS must present nothing at all; presenting the default binding's \
             credential is the cross-tenant substitution this scope exists to prevent"
        );
    }

    /// Coord's refusal BODY reaches the report, because the status alone
    /// cannot tell the two `403`s apart.
    ///
    /// MUTATION PROOF for that: drop the body on the refusal path (the code
    /// this replaced did exactly that, leaving `resp` unread) and the reason
    /// this asserts on is gone.
    ///
    /// The bodies are coord's real ones, verified against `qontinui-coord`
    /// `origin/main` `de4107e2`: `sessions::get_tenant_policy` answers
    /// `{"error":"tenant_id does not match principal"}` on a mismatch while
    /// `fleet_principal::auth_required` answers `{"error":"auth_required"}`
    /// when nothing authenticated at all. Two different operator actions
    /// behind one status code.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_refusal_carries_coords_own_stated_cause() {
        let _amb = crate::test_env::isolated_ambient();
        std::env::set_var("QONTINUI_DISABLE_KEYCHAIN", "1");

        let dir = tempfile::tempdir().unwrap();
        let outbox = build_outbox(dir.path());
        let (base, rec) = spawn_fake_coord().await;
        rec.lock().await.tenant_policy_status = Some(403);
        let coord = CoordSync::new_for_test(
            outbox,
            base,
            Duration::from_secs(3600),
            Duration::from_secs(3600),
        );

        let err = fetch_session_coordination_flag(&coord.inner, Uuid::now_v7())
            .await
            .expect_err("a 403 must not be read as a flag");
        match err {
            FlagPollError::Unauthorized { status, reason } => {
                assert_eq!(status, 403);
                assert_eq!(
                    reason, "tenant_id does not match principal",
                    "coord's stated cause must survive the refusal path — the status alone \
                     cannot tell a tenant mismatch from an unauthenticated caller"
                );
            }
            other => panic!("a 403 must be typed as Unauthorized, got {other:?}"),
        }
    }

    /// F4(b) MUTATION PROOF — the refusal body READ is bounded, not just the
    /// line built from it.
    ///
    /// `resp.text()` buffers whatever the peer sends. This loop polls once
    /// per interval for the life of the process, so a misbehaving
    /// intermediary answering `403` with a large body had an unbounded,
    /// indefinitely repeating allocation behind it. Restore `resp.text()` and
    /// the reported length becomes the whole 512 KiB instead of the cap.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_large_refusal_body_is_read_only_up_to_the_cap() {
        let _amb = crate::test_env::isolated_ambient();
        std::env::set_var("QONTINUI_DISABLE_KEYCHAIN", "1");

        let huge = "A".repeat(512 * 1024);
        let dir = tempfile::tempdir().unwrap();
        let outbox = build_outbox(dir.path());
        let (base, rec) = spawn_fake_coord().await;
        {
            let mut g = rec.lock().await;
            g.tenant_policy_status = Some(403);
            g.tenant_policy_body = Some(huge.clone());
        }
        let coord = CoordSync::new_for_test(
            outbox,
            base,
            Duration::from_secs(3600),
            Duration::from_secs(3600),
        );

        let err = fetch_session_coordination_flag(&coord.inner, Uuid::now_v7())
            .await
            .expect_err("a 403 must not be read as a flag");
        let FlagPollError::Unauthorized { reason, .. } = err else {
            panic!("a 403 must be typed as Unauthorized, got {err:?}");
        };
        assert!(
            reason.contains(&REFUSAL_BODY_CAP.to_string()) && reason.contains("read cap"),
            "the read must stop at the cap and SAY it stopped: {reason}"
        );
        assert!(
            !reason.contains(&huge.len().to_string()),
            "the whole body must never be buffered: {reason}"
        );
        assert!(!reason.contains("AAAA"), "and never echoed: {reason}");
    }

    /// `{"error": …}` is the only field echoed, and it is bounded.
    ///
    /// Coord states every refusal on this chain as a compile-time literal in
    /// that field, so echoing it widens no leakage. Anything else — an
    /// intermediary's 403 page, a proxy's HTML — is reported by SHAPE, which
    /// still separates "coord refused" from "something in the path refused"
    /// without pasting an unknown body into a log line.
    #[test]
    fn only_coords_error_field_is_echoed_and_it_is_bounded() {
        assert_eq!(
            coord_refusal_reason(Ok(r#"{"error":"auth_required"}"#)),
            "auth_required"
        );
        assert_eq!(
            coord_refusal_reason(Ok(r#"{"error":"tenant_id does not match principal"}"#)),
            "tenant_id does not match principal"
        );

        // A body that is not coord's contract is described, never echoed.
        let html = "<html><body>Forbidden by corporate-proxy, token=SECRET</body></html>";
        let described = coord_refusal_reason(Ok(html));
        assert!(
            !described.contains("SECRET") && !described.contains("proxy"),
            "a non-coord body must not be pasted into a log line: {described}"
        );
        assert!(
            described.contains(&html.len().to_string()),
            "…but its shape must still be reported: {described}"
        );

        // Bounded even when the shape IS honoured.
        let long = format!(r#"{{"error":"{}"}}"#, "x".repeat(5_000));
        let bounded = coord_refusal_reason(Ok(&long));
        assert!(
            bounded.chars().count() <= 201,
            "the echoed field must be truncated, got {} chars",
            bounded.chars().count()
        );
        // Multi-byte: the charset gate rejects it before truncation ever sees
        // it, so this now proves the gate rather than the cut — and either
        // way a BYTE truncation at the cap would panic instead of returning.
        let wide = format!(r#"{{"error":"{}"}}"#, "é".repeat(5_000));
        let wide_out = coord_refusal_reason(Ok(&wide));
        assert!(wide_out.chars().count() <= 201);
        assert!(!wide_out.contains('é'), "{wide_out}");
    }

    /// F4(a) MUTATION PROOF — a JSON body is not a COORD body.
    ///
    /// The shape gate alone admits an intermediary that reflects the
    /// credential it just rejected: a WAF answering
    /// `{"error":"invalid bearer <jwt>"}` satisfies `error`-is-a-string
    /// exactly, and echoing it pastes the presented token into a
    /// `tracing::warn!`. Drop [`is_coord_literal_shaped`] from the filter and
    /// this goes red on the token.
    #[test]
    fn a_json_lookalike_reflecting_the_credential_is_not_echoed() {
        let jwt = "eyJhbGciOiJIUzI1NiJ9.eyJ0ZW5hbnRfaWQiOiJhIn0.SiGnAtUrE";
        let reflected = format!(r#"{{"error":"invalid bearer {jwt}"}}"#);
        let out = coord_refusal_reason(Ok(&reflected));
        assert!(
            !out.contains(jwt) && !out.contains("eyJ"),
            "a body claiming coord's SHAPE but not its alphabet must not be echoed — this one \
             carries the credential back: {out}"
        );
        assert!(
            out.contains(&reflected.len().to_string()),
            "it is still reported by shape: {out}"
        );

        // The four coord literals themselves stay echoable — the gate is a
        // charset, not an allowlist, so a fifth literal reaches the operator.
        for literal in [
            "auth_required",
            "tenant_id does not match principal",
            "strategy_admin_required",
            "tenant_not_resolved",
            "some_future_cause coord adds",
        ] {
            assert_eq!(
                coord_refusal_reason(Ok(&format!(r#"{{"error":"{literal}"}}"#))),
                literal
            );
        }
    }

    /// F4(c) MUTATION PROOF — a read FAILURE is not a measurement.
    ///
    /// `unwrap_or_default()` turned a mid-body connection reset into `""` and
    /// then reported "an unrecognized 0-byte body, so the refusal may not be
    /// coord's own" — a doubt about coord manufactured out of a local read
    /// error, one layer below the defect this commit exists to remove
    /// (served policy `verification-and-evidence`
    /// `unknown-must-not-render-as-a-default`). Collapse the `Err` arm back
    /// into `Ok("")` and this goes red.
    #[test]
    fn a_body_that_could_not_be_read_is_unknown_not_a_zero_byte_body() {
        let unread = coord_refusal_reason(Err("connection reset by peer"));
        assert!(
            unread.contains("UNKNOWN") && unread.contains("connection reset by peer"),
            "a read failure must name itself: {unread}"
        );
        assert!(
            !unread.contains("0-byte"),
            "nothing was measured, so no length may be reported: {unread}"
        );

        // …and a body that genuinely WAS empty still reports as one, because
        // that IS a measurement.
        let empty = coord_refusal_reason(Ok(""));
        assert!(empty.contains("0-byte"), "{empty}");
        assert!(!empty.contains("UNKNOWN"), "{empty}");
        assert_ne!(
            unread, empty,
            "an unread body and an empty one are different facts"
        );
    }

    /// A device JWT of the shape coord issues — the `tenant_id` claim plus a
    /// live `exp`, which is what `slot_jwt_is_usable` requires for a slot to
    /// be a HIT rather than `PresentButDead`.
    fn device_jwt_for(tenant: &Uuid) -> String {
        use base64::Engine as _;
        let header = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(br#"{"alg":"none","typ":"JWT"}"#);
        let exp = chrono::Utc::now().timestamp() + 3 * 60 * 60;
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(format!(r#"{{"tenant_id":"{tenant}","exp":{exp}}}"#).as_bytes());
        format!("{header}.{payload}.sig")
    }

    /// The scope itself: the tenant ASKED ABOUT, never the device default.
    ///
    /// `Device` would be a claim that this route takes no tenancy from the
    /// bearer. `/tenant-policy` does — it compares the bearer's `tenant_id`
    /// claim to the query — so `Device` here is not a shrug but a false
    /// statement about the route.
    #[test]
    fn tenant_policy_scope_is_owned_by_the_queried_tenant() {
        let t = Uuid::now_v7();
        assert_eq!(tenant_policy_scope(t), TenantScope::Owned(t));
        assert_ne!(tenant_policy_scope(t), TenantScope::Device);
        assert_ne!(tenant_policy_scope(t), TenantScope::Unresolved);
    }

    /// A slot MISS must stay a miss. `TenantScope::Owned` routes through
    /// `select_device_bearer`, whose documented posture is that a non-default
    /// tenant with no usable slot yields `None` — the request goes out
    /// unauthenticated and coord answers. Substituting the legacy slot would
    /// be the original bug wearing the fix's name, so this pins that the scope
    /// the poll declares is the one that carries the fail-closed rule.
    #[test]
    fn the_polls_scope_is_the_fail_closed_one() {
        let queried = Uuid::now_v7();
        match tenant_policy_scope(queried) {
            TenantScope::Owned(t) => assert_eq!(t, queried),
            other => panic!(
                "the poll must declare Owned(queried tenant) — only that variant applies \
                 select_device_bearer's no-substitution rule; got {other:?}"
            ),
        }
        // And it names that tenant in the body-carrying form too, so a future
        // reader cannot conclude the scope is decorative.
        assert_eq!(
            tenant_policy_scope(queried).declared_tenant(),
            Some(queried)
        );
    }

    /// N consecutive identical refusals produce ONE report, not N.
    ///
    /// The measured defect: 656 consecutive identical 403 warnings, one per
    /// poll interval, for the life of the process.
    #[test]
    fn a_standing_refusal_is_reported_once_not_once_per_pass() {
        let mut r = TenantPolicyAuthReporter::default();
        let asked = Uuid::now_v7();
        let presented = crate::auth::PresentedTenant::Tenant(Uuid::now_v7());

        let line = r
            .observe(403, MISMATCH, asked, || presented)
            .expect("the first refusal must be reported");
        assert!(
            line.contains(&asked.to_string()),
            "the report must name the tenant asked about: {line}"
        );
        assert!(
            line.contains(&presented.to_string()),
            "the report must name the tenant its credential claims: {line}"
        );
        assert!(
            line.contains(MISMATCH),
            "the report must carry COORD's stated cause too — the half the code used to throw \
             away and then guess at: {line}"
        );
        assert!(
            !line.contains("stays refused until"),
            "and it must not ASSERT a cause it never measured — the line this replaced named \
             one of several conditions coord answers 403 to: {line}"
        );
        assert!(
            !line.contains("pairing"),
            "the advice must not name device pairing — pairing was never the missing thing, \
             and naming a flow the runner cannot reach is half the defect: {line}"
        );

        for pass in 0..656 {
            assert!(
                r.observe(403, MISMATCH, asked, || presented).is_none(),
                "pass {pass} repeated an already-reported refusal and must stay quiet"
            );
        }
        assert_eq!(r.suppressed, 656);
    }

    /// Coord's real mismatch body, verified against `qontinui-coord`
    /// `origin/main` `de4107e2` (`sessions::get_tenant_policy`).
    const MISMATCH: &str = "tenant_id does not match principal";

    /// MUTATION PROOF for the four-cause split, at the line an operator
    /// actually reads: an expired slot must not be reported as a missing one.
    ///
    /// The device DOES hold the queried tenant's slot in that state — it
    /// lapsed, and the refresher re-mints it with no operator action at all.
    /// Collapse `PresentedTenant::Anonymous`'s cause back to a single value
    /// and these lines become identical, which is what sent an operator to
    /// re-pair a runner that needed nothing.
    #[test]
    fn an_expired_slot_is_not_reported_as_a_missing_one() {
        use crate::auth::{NoCredential, PresentedTenant, SlotState};
        let asked = Uuid::now_v7();

        let line_for = |cause: NoCredential| {
            TenantPolicyAuthReporter::default()
                .observe(403, MISMATCH, asked, || PresentedTenant::Anonymous(cause))
                .expect("a first refusal is always reported")
        };

        let absent = line_for(NoCredential::Slot(SlotState::Absent));
        let dead = line_for(NoCredential::Slot(SlotState::PresentButDead));
        let unreadable = line_for(NoCredential::Slot(SlotState::Unreadable));

        assert_ne!(
            absent, dead,
            "a slot that LAPSED and a slot that was never issued want different actions"
        );
        assert_ne!(
            absent, unreadable,
            "an unreadable store is UNKNOWN, not an absence"
        );
        assert_ne!(dead, unreadable);
        assert!(dead.contains(SlotState::PresentButDead.label()), "{dead}");
    }

    /// Coord answering differently is new information and breaks the silence;
    /// the same answer never does.
    #[test]
    fn a_changed_status_breaks_the_silence() {
        let asked = Uuid::now_v7();
        let cred = crate::auth::PresentedTenant::Tenant(Uuid::now_v7());

        let mut r = TenantPolicyAuthReporter::default();
        assert!(r.observe(403, MISMATCH, asked, || cred).is_some());
        assert!(r.observe(403, MISMATCH, asked, || cred).is_none());
        assert!(
            r.observe(401, "auth_required", asked, || cred).is_some(),
            "coord answering differently is new information"
        );
        assert!(r.observe(401, "auth_required", asked, || cred).is_none());
        assert!(
            r.observe(403, MISMATCH, asked, || cred).is_some(),
            "and so is it answering differently again"
        );
    }

    /// A refusal that changes and changes BACK must be reported, and the
    /// suppressed count must survive the detour.
    ///
    /// The measured gap this closes: suppression keyed on the status alone
    /// and the transient arm never touched the standing state, so
    /// `403`, `403`×N, `500`, `403` reported the FIRST 403 and then went
    /// silent forever — while this type's own contract claimed the silence
    /// breaks whenever "coord answers differently". A `500` is coord
    /// answering differently.
    #[test]
    fn a_transient_failure_between_refusals_breaks_the_silence() {
        let mut r = TenantPolicyAuthReporter::default();
        let asked = Uuid::now_v7();
        let cred = crate::auth::PresentedTenant::Tenant(Uuid::now_v7());

        assert!(r.observe(403, MISMATCH, asked, || cred).is_some());
        for _ in 0..5 {
            assert!(r.observe(403, MISMATCH, asked, || cred).is_none());
        }
        r.interrupted();
        let line = r
            .observe(403, MISMATCH, asked, || cred)
            .expect("a refusal after coord answered some other way is NEW information");
        assert!(
            line.contains("after 5 suppressed"),
            "and the quiet period it followed must not be silently discarded: {line}"
        );
    }

    /// F1 MUTATION PROOF — a transient blip must not VOID the recovery line
    /// the standing refusal promised, nor strand its count.
    ///
    /// The first report ends "one line will report the recovery". Ending the
    /// silence by clearing `reported` (rather than by bumping the epoch)
    /// makes `clear`'s `?` return early on exactly this sequence — `403`,
    /// `403`×5, a blip, `200` — so the promised line never arrives, and the
    /// 5 suppressed passes sit in the field until some unrelated future
    /// refusal prints them as its own. Both halves are asserted here:
    /// revert [`TenantPolicyAuthReporter::interrupted`] to `self.reported =
    /// None` and this goes red on the first `expect`.
    #[test]
    fn a_transient_failure_does_not_void_the_promised_recovery_line() {
        let mut r = TenantPolicyAuthReporter::default();
        let asked = Uuid::now_v7();
        let cred = crate::auth::PresentedTenant::Tenant(Uuid::now_v7());

        assert!(r.observe(403, MISMATCH, asked, || cred).is_some());
        for _ in 0..5 {
            assert!(r.observe(403, MISMATCH, asked, || cred).is_none());
        }
        r.interrupted();

        let recovery = r
            .clear()
            .expect("the recovery line the first report PROMISED is owed across a blip");
        assert!(
            recovery.contains("after 5 suppressed"),
            "and it must carry the quiet period's own count: {recovery}"
        );
        assert_eq!(
            r.suppressed, 0,
            "a count is dropped only by being PRINTED — leaving it set leaks a closed \
             episode's 5 into an unrelated future refusal"
        );
        assert!(
            r.clear().is_none(),
            "and the recovery is reported exactly once"
        );
    }

    /// The invariant the F1 regression broke: `reported == None` implies
    /// `suppressed == 0`, for every interleaving of the three writers.
    ///
    /// `clear` is the only writer that may drop a standing refusal, and it
    /// prints the count as it goes. `interrupted` was the first writer to
    /// break that, and `clear` had been written assuming it.
    #[test]
    fn a_dropped_refusal_never_leaves_a_count_behind() {
        let asked = Uuid::now_v7();
        let cred = crate::auth::PresentedTenant::Tenant(Uuid::now_v7());
        // Every word over {observe 403, observe 401, interrupted, clear} of
        // length 4 — 256 interleavings, checked after every single step.
        for word in 0..256u32 {
            let mut r = TenantPolicyAuthReporter::default();
            for step in 0..4 {
                match (word >> (step * 2)) & 0b11 {
                    0 => {
                        r.observe(403, MISMATCH, asked, || cred);
                    }
                    1 => {
                        r.observe(401, "auth_required", asked, || cred);
                    }
                    2 => r.interrupted(),
                    _ => {
                        r.clear();
                    }
                }
                assert!(
                    r.reported.is_some() || r.suppressed == 0,
                    "word {word:08b} step {step}: a refusal dropped without printing its \
                     count leaks that count into the next episode"
                );
            }
        }
    }

    /// The suppressed count is dropped only by being PRINTED — a changed
    /// status must carry it, not reset it.
    ///
    /// Before this, `403`×657 followed by a `401` reported the 401 and said
    /// nothing about the 656 quiet passes, and the eventual recovery line
    /// read "after 0 suppressed".
    #[test]
    fn a_changed_status_carries_the_suppressed_count_rather_than_dropping_it() {
        let mut r = TenantPolicyAuthReporter::default();
        let asked = Uuid::now_v7();
        let cred = crate::auth::PresentedTenant::Tenant(Uuid::now_v7());

        assert!(r.observe(403, MISMATCH, asked, || cred).is_some());
        for _ in 0..656 {
            assert!(r.observe(403, MISMATCH, asked, || cred).is_none());
        }
        let line = r
            .observe(401, "auth_required", asked, || cred)
            .expect("a different status is reported");
        assert!(
            line.contains("after 656 suppressed"),
            "the 656 suppressed passes must be named by the line that ends them: {line}"
        );
        assert_eq!(r.suppressed, 0, "and only then are they cleared");
    }

    /// The tenant is part of the suppression key.
    ///
    /// No caller varies it today — this loop freezes one tenant at
    /// construction — so this pins a property rather than fixing a live bug.
    /// Keyed on the status alone, a future caller that reused one reporter
    /// across tenants would have one tenant's refusal silence another's,
    /// which is a cross-tenant silence and exactly the class of mistake this
    /// phase exists to remove.
    #[test]
    fn the_suppression_key_includes_the_tenant() {
        let mut r = TenantPolicyAuthReporter::default();
        let a = Uuid::now_v7();
        let b = Uuid::now_v7();
        let cred = crate::auth::PresentedTenant::Tenant(Uuid::now_v7());

        assert!(r.observe(403, MISMATCH, a, || cred).is_some());
        assert!(r.observe(403, MISMATCH, a, || cred).is_none());
        assert!(
            r.observe(403, MISMATCH, b, || cred).is_some(),
            "a refusal about a DIFFERENT tenant is a different condition"
        );
    }

    /// Resolving the presented credential is a local encrypted-file read, so
    /// the SUPPRESSED path — every pass but the first — must not pay for it.
    /// That is why `observe` takes a closure rather than a value: the read
    /// happens only on the pass that emits a line.
    #[test]
    fn the_suppressed_path_never_reads_the_credential() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let reads = AtomicUsize::new(0);
        let mut r = TenantPolicyAuthReporter::default();
        let asked = Uuid::now_v7();
        let cred = crate::auth::PresentedTenant::Tenant(Uuid::now_v7());
        let mut read = || {
            reads.fetch_add(1, Ordering::Relaxed);
            cred
        };

        assert!(r.observe(403, MISMATCH, asked, &mut read).is_some());
        assert_eq!(reads.load(Ordering::Relaxed), 1);
        for _ in 0..100 {
            assert!(r.observe(403, MISMATCH, asked, &mut read).is_none());
        }
        assert_eq!(
            reads.load(Ordering::Relaxed),
            1,
            "a suppressed pass must cost no credential read at all — this loop runs for the \
             life of the process"
        );
    }

    /// Recovery emits exactly one line, naming how many passes were
    /// suppressed — so the quiet period is legible rather than merely absent.
    #[test]
    fn recovery_reports_once_with_the_suppressed_count() {
        let mut r = TenantPolicyAuthReporter::default();
        let asked = Uuid::now_v7();
        let cred = crate::auth::PresentedTenant::Anonymous(crate::auth::NoCredential::Slot(
            crate::auth::SlotState::Absent,
        ));

        assert!(r.clear().is_none(), "nothing standing, nothing to report");
        assert!(r.observe(403, MISMATCH, asked, || cred).is_some());
        for _ in 0..12 {
            assert!(r.observe(403, MISMATCH, asked, || cred).is_none());
        }
        let line = r
            .clear()
            .expect("a standing refusal that clears is reported");
        assert!(
            line.contains("after 12 suppressed"),
            "the suppressed count must be named: {line}"
        );
        assert!(line.contains("403"), "{line}");
        assert!(
            r.clear().is_none(),
            "a cleared condition must not report a second time"
        );
        assert!(
            r.observe(403, MISMATCH, asked, || cred).is_some(),
            "a refusal after a recovery is a NEW condition and is reported again"
        );
    }

    /// A 401/403 is typed apart from every other failure, because the two want
    /// opposite reporting: one is standing, the rest are transient. Collapsing
    /// them into a flat string is what produced the per-minute warning.
    #[test]
    fn unauthorized_is_typed_apart_from_transient_failures() {
        assert_eq!(
            FlagPollError::Unauthorized {
                status: 401,
                reason: "auth_required".into()
            }
            .to_string(),
            r#"status 401: "auth_required""#
        );
        // The reason originates in a response body, so this channel escapes
        // it exactly as the report path does. Unescaped, a body carrying a
        // newline forges a second log line from inside one.
        assert_eq!(
            FlagPollError::Unauthorized {
                status: 403,
                reason: "a\nforged ERROR line".into()
            }
            .to_string(),
            r#"status 403: "a\nforged ERROR line""#
        );
        assert_eq!(
            FlagPollError::Other("transport: x".into()).to_string(),
            "transport: x"
        );

        let body = tenant_policy_fetch_body();
        assert!(
            !body.contains("tracing::warn!") && !body.contains("tracing::info!"),
            "the fetch must not log a refusal itself — only the loop can tell a standing \
             refusal from a first one, and a per-call warn is the defect. Body was:\n{body}"
        );
    }
}

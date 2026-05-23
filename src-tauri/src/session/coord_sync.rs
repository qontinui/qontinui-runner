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
//! 1. **Drain loop** — reads unacked rows from the [`OutboxWriter`] in seq
//!    order, dispatches each to the right coord endpoint based on
//!    `event_kind`, and ACKs by rewriting the JSONL line with `acked_at`.
//!    Tick: 1s when there are unacked rows, 5s when caught up. Backs off
//!    to 60s ceiling on repeated transport errors so coord-down sessions
//!    don't burn the CPU.
//!
//! 2. **Heartbeat loop** — every `QONTINUI_SESSION_HEARTBEAT_SECS`
//!    (default 15s, plan §D13), iterates the [`SessionRegistry`] and emits
//!    a heartbeat outbox row per active session. The drain loop then
//!    PATCHes coord with `{heartbeat: true}` which refreshes
//!    `last_heartbeat_at = now()` on coord-side. Stale-detection at 45s,
//!    auto-close at 180s.
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
//! reconnect is automatic. There is no in-memory retry counter; the file
//! is the queue.
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

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use chrono::Utc;
use reqwest::StatusCode;
use serde::Serialize;
use serde_json::{json, Value as JsonValue};
use tokio::task::JoinHandle;
use uuid::Uuid;

use super::local_store::{OutboxRecord, OutboxWriter};
use super::{SessionEventKind, SessionRegistry, SessionState};

// ---------------------------------------------------------------------------
// Env tunables
// ---------------------------------------------------------------------------

/// Default heartbeat cadence (plan §D13).
const DEFAULT_HEARTBEAT_SECS: u64 = 15;
/// Default stale threshold — 3 missed heartbeats.
const DEFAULT_STALE_SECS: u64 = 45;
/// Default auto-close threshold — 12 missed heartbeats.
const DEFAULT_AUTOCLOSE_SECS: u64 = 180;
/// Fallback coord URL when neither `COORD_HTTP_URL` env nor profile is set.
const DEFAULT_COORD_URL: &str = "http://localhost:9870";

/// Read a `u64` env var with a sane default.
fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(default)
}

/// Resolve the coord HTTP base URL. Priority: `COORD_HTTP_URL` env →
/// active profile (`profiles::load_strict().coord_url`, ws→http) →
/// fallback `http://localhost:9870`.
fn resolve_coord_url() -> String {
    if let Ok(v) = std::env::var("COORD_HTTP_URL") {
        let t = v.trim();
        if !t.is_empty() {
            return t.trim_end_matches('/').to_string();
        }
    }
    if let Ok(profile) = qontinui_runner_lib::profiles::load_strict() {
        if let Some(coord_url) = profile.coord_url {
            let trimmed = coord_url.trim_end_matches("/ws");
            let with_http = trimmed
                .strip_prefix("wss://")
                .map(|rest| format!("https://{rest}"))
                .or_else(|| {
                    trimmed
                        .strip_prefix("ws://")
                        .map(|rest| format!("http://{rest}"))
                })
                .unwrap_or_else(|| trimmed.to_string());
            return with_http.trim_end_matches('/').to_string();
        }
    }
    DEFAULT_COORD_URL.to_string()
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
    autoclose: Duration,
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
}

impl std::fmt::Debug for CoordSync {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CoordSync")
            .field("coord_url", &self.inner.coord_url)
            .field("heartbeat", &self.inner.heartbeat)
            .field("stale", &self.inner.stale)
            .field("autoclose", &self.inner.autoclose)
            .finish()
    }
}

impl CoordSync {
    /// Construct a CoordSync with all settings resolved from env. Used at
    /// app startup; tests prefer [`CoordSync::new_for_test`].
    pub fn new(outbox: Arc<OutboxWriter>) -> Self {
        let coord_url = resolve_coord_url();
        let heartbeat = Duration::from_secs(env_u64(
            "QONTINUI_SESSION_HEARTBEAT_SECS",
            DEFAULT_HEARTBEAT_SECS,
        ));
        let stale = Duration::from_secs(env_u64("QONTINUI_SESSION_STALE_SECS", DEFAULT_STALE_SECS));
        let autoclose = Duration::from_secs(env_u64(
            "QONTINUI_SESSION_AUTOCLOSE_SECS",
            DEFAULT_AUTOCLOSE_SECS,
        ));

        let http = reqwest::Client::builder()
            // Per-request timeout — slow enough to ride out a hiccup,
            // fast enough that the drain loop doesn't stall on a hung
            // coord. 30s matches the `agent_claims` heartbeat client.
            .timeout(Duration::from_secs(30))
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
                autoclose,
                has_been_online: AtomicBool::new(false),
                app_handle: Mutex::new(None),
                registry: Mutex::new(None),
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
        autoclose: Duration,
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
                autoclose,
                has_been_online: AtomicBool::new(false),
                app_handle: Mutex::new(None),
                registry: Mutex::new(None),
            }),
        }
    }

    /// Borrow the local outbox. Phase 2 surface — unchanged.
    pub fn outbox(&self) -> &OutboxWriter {
        &self.inner.outbox
    }

    /// Coord URL the loops POST/PATCH/DELETE against. Surfaced for tests.
    #[allow(dead_code)]
    pub fn coord_url(&self) -> &str {
        &self.inner.coord_url
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

    /// Auto-close threshold (12 missed heartbeats).
    #[allow(dead_code)]
    pub fn autoclose_threshold(&self) -> Duration {
        self.inner.autoclose
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

async fn run_drain_loop(inner: Arc<CoordSyncInner>) {
    tracing::info!(
        coord_url = %inner.coord_url,
        "coord_sync: drain loop starting"
    );
    let mut backoff = TICK_BUSY;
    loop {
        let pending = match inner.outbox.pending() {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(error = %e, "coord_sync: outbox pending() failed");
                tokio::time::sleep(TICK_IDLE).await;
                continue;
            }
        };

        if pending.is_empty() {
            backoff = TICK_BUSY;
            tokio::time::sleep(TICK_IDLE).await;
            continue;
        }

        let total = pending.len();
        let mut succeeded: Vec<(Uuid, i64)> = Vec::with_capacity(total);
        let mut had_transport_error = false;

        for rec in pending {
            match push_record(&inner, &rec).await {
                PushOutcome::Acked => succeeded.push((rec.session_id, rec.seq)),
                PushOutcome::Conflict { row } => {
                    succeeded.push((rec.session_id, rec.seq));
                    handle_conflict(&inner, &rec, row).await;
                }
                PushOutcome::Transport(e) => {
                    tracing::warn!(
                        session = %rec.session_id,
                        seq = rec.seq,
                        kind = %rec.event_kind,
                        error = %e,
                        "coord_sync: push failed; will retry"
                    );
                    had_transport_error = true;
                    // Stop the batch on transport error — preserves
                    // (session, seq) order on reconnect. The unACKed
                    // tail stays in the file for the next tick.
                    break;
                }
                PushOutcome::PermanentFailure(reason) => {
                    // We treat 4xx (other than 409) as "the runner sent
                    // a bad record that coord refuses". ACK it locally
                    // so the queue moves forward — the dashboard will
                    // miss this event but the session itself isn't
                    // hostage to a corrupt row.
                    tracing::error!(
                        session = %rec.session_id,
                        seq = rec.seq,
                        kind = %rec.event_kind,
                        reason = %reason,
                        "coord_sync: permanent failure — ACKing locally"
                    );
                    succeeded.push((rec.session_id, rec.seq));
                }
            }
        }

        if !succeeded.is_empty() {
            if let Err(e) = inner.outbox.ack(&succeeded) {
                tracing::warn!(error = %e, "coord_sync: ack write failed");
            }
            inner.has_been_online.store(true, Ordering::Relaxed);
        }

        if had_transport_error {
            tokio::time::sleep(backoff).await;
            backoff = std::cmp::min(backoff * 2, MAX_BACKOFF);
        } else {
            backoff = TICK_BUSY;
            tokio::time::sleep(TICK_BUSY).await;
        }
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

    let result = match kind {
        "started" => {
            // POST /sessions with the full create body. The runner
            // already stamped the row's id + intent into the payload at
            // session start; we just forward it.
            let body = rebuild_create_body(rec);
            let url = format!("{base}/sessions");
            inner.http.post(&url).json(&body).send().await
        }
        "heartbeat" => {
            let url = format!("{base}/sessions/{}", rec.session_id);
            let body = json!({ "heartbeat": true });
            inner.http.patch(&url).json(&body).send().await
        }
        "state_change" => {
            let url = format!("{base}/sessions/{}", rec.session_id);
            // The payload carries whatever fields the runner changed —
            // forward the subset coord understands.
            let body = state_change_body(&rec.payload);
            inner.http.patch(&url).json(&body).send().await
        }
        "closed" => {
            let url = format!("{base}/sessions/{}", rec.session_id);
            inner.http.delete(&url).send().await
        }
        "claim_stolen" => {
            let url = format!("{base}/sessions/{}/steal", rec.session_id);
            let body = steal_body(rec);
            inner.http.post(&url).json(&body).send().await
        }
        other => {
            // OutputChunk + HandoffRequest are Phase 7/8 — defined now
            // for wire shape, not pushed yet. Quietly ACK so the file
            // doesn't grow.
            tracing::debug!(
                kind = %other,
                "coord_sync: event kind not yet pushed to coord — ACKing"
            );
            return PushOutcome::Acked;
        }
    };

    match result {
        Ok(resp) => {
            let status = resp.status();
            if status.is_success() {
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
            if status.is_client_error() {
                return PushOutcome::PermanentFailure(format!("{status}: {detail}"));
            }
            // 5xx → transient.
            PushOutcome::Transport(format!("{status}: {detail}"))
        }
        Err(e) => PushOutcome::Transport(format!("{e}")),
    }
}

/// Reassemble a `POST /sessions` body from the outbox payload + the row's
/// machine_id. The session start path writes the create body shape into
/// `payload` directly (`{id, kind, intent, state, started_at}`), so
/// rebuilding for the wire is mostly relabeling.
fn rebuild_create_body(rec: &OutboxRecord) -> JsonValue {
    // tenant_id is *not* on the outbox row — it lives inside the intent
    // body the operator (or future tenant-resolver) supplies. Phase 2's
    // local-first path persists the intent into the payload, so we look
    // for it there. If absent, fall back to nil so coord returns a clean
    // 400 (caller fix) rather than a 500.
    let intent = rec
        .payload
        .get("intent")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let tenant_id = intent
        .get("tenant_id")
        .or_else(|| rec.payload.get("tenant_id"))
        .cloned()
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
    body
}

/// Extract the subset of `state_change` payload fields that map to
/// `UpdateSessionRequest` (state, repo, branch, intent_updates).
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
    // Coord refreshes last_heartbeat_at on any PATCH — passing
    // heartbeat=true ensures the row gets a fresh stamp even when the
    // caller only changed metadata.
    body.insert("heartbeat".into(), JsonValue::Bool(true));
    JsonValue::Object(body)
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

async fn handle_conflict(
    inner: &Arc<CoordSyncInner>,
    rec: &OutboxRecord,
    row: Option<JsonValue>,
) {
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
        autoclose_after = ?inner.autoclose,
        "coord_sync: heartbeat loop starting"
    );

    loop {
        tokio::time::sleep(interval).await;

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

        let mut to_close: Vec<Uuid> = Vec::new();
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

            if elapsed >= inner.autoclose {
                to_close.push(desc.id);
                continue;
            }
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
        for id in to_heartbeat.keys() {
            let payload = json!({
                "id": id,
                "at": now,
            });
            if let Err(e) = inner.outbox.record(
                machine_id,
                *id,
                SessionEventKind::Heartbeat,
                payload,
            ) {
                tracing::warn!(
                    session = %id,
                    error = %e,
                    "coord_sync: heartbeat outbox write failed"
                );
            }
        }

        // Auto-close the truly-gone sessions. `close_by_id` records the
        // `closed` event in the outbox, which the drain loop folds into
        // a DELETE on its next tick.
        for id in to_close {
            tracing::info!(
                session = %id,
                "coord_sync: auto-closing session past autoclose threshold"
            );
            if let Err(e) = reg.close_by_id(id) {
                tracing::warn!(
                    session = %id,
                    error = %e,
                    "coord_sync: auto-close failed"
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::transport::{DynTransport, Transport, TransportError, TransportHandle};
    use crate::session::{Intent, SessionKind, SessionRegistry, SessionTransports};
    use axum::{
        extract::{Path as AxumPath, State as AxumState},
        http::StatusCode as AxumStatus,
        response::IntoResponse,
        routing::{patch, post},
        Json, Router,
    };
    use std::sync::Arc;
    use tokio::net::TcpListener;
    use tokio::sync::Mutex as TokMutex;

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
    }

    fn make_test_intent() -> Intent {
        Intent {
            kind: SessionKind::TerminalShell,
            purpose: "coord-sync test".into(),
            repo: Some("qontinui-runner".into()),
            branch: Some("feat/coord-sync".into()),
            declared_paths: vec![],
            share_output: false,
            redact_secrets: None,
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
        /// When true, the next POST returns 409 + a synthetic row.
        next_post_conflict: bool,
        /// When >0, the next N POSTs return 500.
        next_post_5xx: usize,
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
                     Json(body): Json<JsonValue>| async move {
                        let mut g = state.lock().await;
                        if g.next_post_5xx > 0 {
                            g.next_post_5xx -= 1;
                            return (
                                AxumStatus::INTERNAL_SERVER_ERROR,
                                Json(json!({"error": "fake-5xx"})),
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
                        state.lock().await.patches.push((id, body.clone()));
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

    #[tokio::test]
    async fn drain_pushes_started_event_as_post_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let outbox = build_outbox(dir.path());
        let (base, rec) = spawn_fake_coord().await;
        let coord = CoordSync::new_for_test(
            outbox.clone(),
            base,
            Duration::from_millis(50),
            Duration::from_secs(10),
            Duration::from_secs(60),
        );
        let registry = build_registry(coord.clone());
        let _drain = coord.start_drain_task();

        let _handle = registry.start(make_test_intent()).unwrap();

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

    #[tokio::test]
    async fn drain_treats_409_as_acked_for_started() {
        let dir = tempfile::tempdir().unwrap();
        let outbox = build_outbox(dir.path());
        let (base, rec) = spawn_fake_coord().await;
        rec.lock().await.next_post_conflict = true;

        let coord = CoordSync::new_for_test(
            outbox.clone(),
            base,
            Duration::from_millis(50),
            Duration::from_secs(10),
            Duration::from_secs(60),
        );
        let registry = build_registry(coord.clone());
        let _drain = coord.start_drain_task();

        let handle = registry.start(make_test_intent()).unwrap();
        let id = handle.id();

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

    #[tokio::test]
    async fn drain_retries_after_5xx() {
        let dir = tempfile::tempdir().unwrap();
        let outbox = build_outbox(dir.path());
        let (base, rec) = spawn_fake_coord().await;
        rec.lock().await.next_post_5xx = 2; // fail twice, then succeed

        let coord = CoordSync::new_for_test(
            outbox.clone(),
            base,
            Duration::from_millis(50),
            Duration::from_secs(10),
            Duration::from_secs(60),
        );
        let registry = build_registry(coord.clone());
        let _drain = coord.start_drain_task();

        let _h = registry.start(make_test_intent()).unwrap();

        // Backoff escalates after each fail (1s, 2s, …). After two 5xx
        // failures + one success the queue is empty.
        wait_until(Duration::from_secs(30), || {
            outbox.pending().map(|p| p.is_empty()).unwrap_or(false)
        })
        .await;
    }

    #[tokio::test]
    async fn heartbeat_emits_outbox_rows_for_active_sessions() {
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
            Duration::from_secs(600),
        );
        let registry = build_registry(coord.clone());
        let _hb = coord.start_heartbeat_task();

        let handle = registry.start(make_test_intent()).unwrap();
        let id = handle.id();

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

    #[tokio::test]
    async fn heartbeat_records_patch_to_coord() {
        let dir = tempfile::tempdir().unwrap();
        let outbox = build_outbox(dir.path());
        let (base, rec) = spawn_fake_coord().await;
        let coord = CoordSync::new_for_test(
            outbox.clone(),
            base,
            Duration::from_millis(100),
            Duration::from_secs(60),
            Duration::from_secs(600),
        );
        let registry = build_registry(coord.clone());
        let _drain = coord.start_drain_task();
        let _hb = coord.start_heartbeat_task();

        let _h = registry.start(make_test_intent()).unwrap();

        // Wait for at least one PATCH carrying heartbeat=true.
        wait_until(Duration::from_secs(10), || {
            let r = rec.try_lock();
            r.map(|g| g.patches.iter().any(|(_, b)| b["heartbeat"] == true))
                .unwrap_or(false)
        })
        .await;
    }

    #[tokio::test]
    async fn autoclose_fires_delete_after_threshold() {
        let dir = tempfile::tempdir().unwrap();
        let outbox = build_outbox(dir.path());
        let (base, rec) = spawn_fake_coord().await;
        // Heartbeat every 50ms; stale at 60ms; autoclose at 120ms so
        // the natural elapsed time after a session start crosses the
        // threshold within ~half a second of wall-clock.
        let coord = CoordSync::new_for_test(
            outbox.clone(),
            base,
            Duration::from_millis(50),
            Duration::from_millis(60),
            Duration::from_millis(120),
        );
        let registry = build_registry(coord.clone());
        let _drain = coord.start_drain_task();
        let _hb = coord.start_heartbeat_task();

        // Force the session's last_heartbeat backwards so the auto-
        // close trigger fires on the first sweep without waiting for
        // real wall-clock to pass beyond the threshold mid-loop.
        let handle = registry.start(make_test_intent()).unwrap();
        let id = handle.id();
        registry.force_heartbeat_to_for_test(id, Utc::now() - chrono::Duration::seconds(10));

        wait_until(Duration::from_secs(5), || {
            let r = rec.try_lock();
            r.map(|g| g.deletes.iter().any(|d| *d == id)).unwrap_or(false)
        })
        .await;

        let desc = registry.describe_by_id(id).unwrap();
        assert_eq!(desc.state, SessionState::Closed);
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
        let var = "__COORD_SYNC_TEST_ENV_U64";
        std::env::set_var(var, "not-a-number");
        assert_eq!(env_u64(var, 42), 42);
        std::env::remove_var(var);
    }
}

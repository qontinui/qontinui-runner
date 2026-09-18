//! Cross-machine session handoff — trigger + receiver.
//!
//! Plan: `D:/qontinui-root/qontinui-dev-notes/plans/
//! 2026-05-23-coord-native-sessions-phase-7-10.md` §Phase 7. One-way move
//! ("Continue elsewhere"): an operator moves an active session from one
//! machine to another, with cwd + held claims + recent PTY scrollback
//! following.
//!
//! ## Transport: WebSocket-relay push + on-connect catch-up (NOT polling)
//!
//! The plan text says the handoff request travels via JetStream. The
//! runner has **no NATS client** — but it already maintains a persistent
//! WebSocket relay to coord's `/ws` Redis-pub/sub fan-out (the same
//! channel `agent_runtime.rs` uses to receive `events.agent.spawn_*`).
//! Coord's `post_handoff` handler dual-publishes the
//! `handoff_request` payload on subject
//! `qontinui.sessions.<tenant>.<target-machine>.handoff_request` over BOTH
//! JetStream AND Redis pub/sub (see coord `build_events::dual_publish_body`).
//! The Redis arm is exactly what coord's `/ws` fan-out relays.
//!
//! So the receiver is **push-driven, not poll-driven**:
//!
//! 1. **Real-time push.** The receiver opens a coord `/ws` subscription
//!    under the closed-set name `sessions`
//!    (`qontinui_runner_lib::coord_ws::Subscription::Sessions`), which coord
//!    resolves server-side to `qontinui.sessions.<tenant>.<self-device>.*`
//!    from the upgrade credential's claims, and filters inbound envelopes
//!    for `…<self-device>.handoff_request`. Server-side fan-out
//!    (coord PUBLISHes to the target machine's subject) means no
//!    N-runners-polling — the target machine sees the request the instant
//!    coord records it. This reuses the existing runner↔coord relay
//!    transport rather than adding a second poll loop or a fleet-wide NATS
//!    client.
//! 2. **On-(re)connect catch-up.** Every time the relay WS connects (or
//!    reconnects after a drop), the receiver does ONE
//!    `GET /sessions/handoff-requests?device_id=<self>` and materializes
//!    anything that arrived while it was offline. Coord's durable
//!    `handoff_request` event row is the source of truth for this
//!    catch-up, so a request published while the runner was disconnected
//!    is never lost. This is the robustness backstop — steady-state
//!    delivery is the push.
//! 3. **Periodic catch-up tick.** The HANDOFF catch-up GET also runs every
//!    [`CATCHUP_TICK`] while the socket is up, so the window is bounded even
//!    when the socket is healthy but DELIVERS nothing. That is exactly the
//!    shape against a coord predating the closed subscription set: such a
//!    coord ignores `?subscribe=` and PSUBSCRIBEs its default `events.*`,
//!    which never matches `qontinui.sessions.*` — so with the socket held
//!    open, no `handoff_request` frame ever arrives on it. Against that coord
//!    this lane degrades to POLL cadence (one tick), not to silence. The
//!    other three lanes are harmless against the old coord because their
//!    subjects sit under `events.*`; this one is not, which is why the tick
//!    exists. The tick re-sights every handoff whose `close_source` failed,
//!    so [`MaterializedSources`] turns a repeat sighting into a close-only
//!    retry — a second child is never started for a source this process
//!    already materialized.
//!
//!    The tick drives only the arms with no OTHER periodic owner — handoff
//!    and respawn. The remote-attach and remote-create arms that share this
//!    socket's on-connect replay each already have their own 60 s poll task
//!    in `main.rs`, on the same period, so putting them on this tick as well
//!    would double their GETs and deliver nothing sooner.
//!    [`catchups_for`] is where that split lives.
//!
//! ## Receiver flow (one handoff)
//!
//! 1. A push frame (or the on-connect catch-up GET) yields a
//!    [`PendingHandoff`] addressed to this device.
//! 2. Fetch `GET /sessions/:id/handoff-state` → [`HandoffState`].
//! 3. Build an [`Intent`] from the source intent (cwd via `repo` /
//!    `declared_paths`), materialize a child session via
//!    [`SessionRegistry::start_with_parent`] so coord stamps
//!    `parent_session_id`.
//! 4. Re-acquire each held claim under this device (`POST /claims/acquire`
//!    — idempotent by resource_key, so this is the "claim transfer").
//! 5. Replay warm-tier scrollback into the new PTY.
//! 5b. Materialize a local restore-registry record from the newest
//!    `restore-record` session event (plan
//!    `2026-07-09-runner-session-history-cloud-sync` §3.4, Phase 4).
//!    Coord's `HandoffState` bundle does NOT carry session events (verified
//!    against coord `sessions.rs::HandoffState` 2026-07-09), so the
//!    receiver fetches them from the durable-replay portion of
//!    `GET /sessions/:id/events` (bounded read; the endpoint replays the
//!    last rows immediately, then live-tails). Tier honesty carries over:
//!    a `"full"` record materializes AUTHORITATIVE + CONFIRMED (the
//!    existing restore classifier auto-resumes it by authoritative id); a
//!    `"terminal_only"` record materializes AUTHORITATIVE + PROVISIONAL
//!    (the classifier restores terminal+cwd with a fresh conversation —
//!    exactly the existing phantom-shell branch, no new restore path).
//!    Best-effort: any failure here never fails the handoff.
//! 6. Close the source session (`DELETE /sessions/:id`) so it transitions
//!    to `closed` (`closed_at = now()`); coord's delete handler releases
//!    the source claim and publishes `closed`. The child's `started`
//!    event carries `parent_session_id`, which is the durable
//!    `handoff_to` link (parent → child by `parent_session_id` index).
//!
//! ## This loop also carries the RESPAWN arm
//!
//! Coord publishes `respawn_request` on the SAME
//! `qontinui.sessions.<tenant>.<device>.<kind>` family (plan
//! `2026-08-26-sessions-console-consolidation` §6 Phase 5), so
//! [`connect_and_pump`] forwards every frame to [`super::respawn`] as well and
//! [`connect_and_pump`]'s on-connect catch-up runs
//! [`super::respawn::run_catchup`] beside this module's own. The two arms are
//! disambiguated ONLY by the channel's trailing segment —
//! [`parse_handoff_push`] requires `.handoff_request`,
//! [`super::respawn::parse_respawn_push`] requires `.respawn_request` — so
//! neither swallows the other's frames and nothing is materialized twice.
//! A respawn deliberately does NOT run step 6 below: its source is already
//! closed, which is the premise of the feature.
//!
//! Step 3 happening before step 6 is deliberate: the source is only torn
//! down once the child exists, so a failed materialization leaves the
//! source intact and the next push/catch-up retries. Idempotency: coord's
//! `get_handoff_requests` filters out any source that already has a
//! materialized child on this device, so a push + catch-up double-delivery
//! never materializes twice.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use super::intent::Intent;
use super::restore_record_emitter::{RESTORE_RECORD_EVENT, TIER_FULL};
use super::session_lifecycle_store::{
    SessionLifecycleStore, TerminalSessionRecord, DEFAULT_PROVIDER, ORIGIN_AUTHORITATIVE,
};
use crate::auth::TenantScope;

use super::{SessionKind, SessionRegistry};

/// Reconnect backoff floor for the push-subscriber WS loop. Matches the
/// `agent_runtime.rs` reconnect posture (2s → 60s capped).
const RECONNECT_BACKOFF_FLOOR: Duration = Duration::from_secs(2);
/// Reconnect backoff ceiling.
const RECONNECT_BACKOFF_CEIL: Duration = Duration::from_secs(60);
/// How often the catch-up GETs re-run on a LIVE socket (module doc, point 3).
/// Same interval class as `agent_runtime`'s poll backstop. Against a coord
/// predating `?subscribe=` this is the lane's whole delivery cadence.
const CATCHUP_TICK: Duration = Duration::from_secs(60);

/// The periodic catch-up ticker (module doc, point 3): fires immediately
/// once (the caller consumes that tick, since the on-connect catch-up just
/// ran), then every [`CATCHUP_TICK`]; a tick missed while a catch-up was
/// still running is delayed, not bunched. Factored out so the schedule is
/// unit-testable on a paused clock without a socket.
fn catchup_interval() -> tokio::time::Interval {
    let mut tick = tokio::time::interval(CATCHUP_TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    tick
}

/// What a sighting of a pending handoff for a given source should do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Sighting {
    /// First sighting this run: start the child, then close the source.
    Materialize,
    /// A child for this source was already started by THIS process: do not
    /// start another; only retry the `close_source` that must have failed.
    CloseOnly,
}

/// Source sessions this process has already started a child for.
///
/// The only "ack" of a handoff is `close_source`, which runs AFTER the child
/// is started (`materialize`). A close that keeps failing — 403 in a
/// credential gap, coord 5xx — leaves the handoff in coord's pending list, and
/// with the [`CATCHUP_TICK`] every tick would otherwise start ANOTHER child
/// terminal for the same source: an unbounded duplicate-spawn loop, one per
/// minute. This set turns a repeat sighting into a close-only retry.
///
/// Per-process on purpose: it is the process that started the child, so it is
/// the process that knows. A restart forgets it, and the next sighting after
/// a restart materializes again — one duplicate per restart, bounded, versus
/// one per tick. Marked at the moment the child is STARTED, not when the
/// close succeeds, because the child is what must not be duplicated.
#[derive(Default)]
pub(super) struct MaterializedSources(Mutex<HashSet<Uuid>>);

impl MaterializedSources {
    /// The pure decision for one sighting of `source`.
    pub(super) fn sighting(&self, source: Uuid) -> Sighting {
        sighting_for(&self.0.lock().unwrap_or_else(|p| p.into_inner()), source)
    }

    /// Record that a child for `source` has been started. Returns `true` when
    /// this is the first record (the set changed).
    pub(super) fn mark(&self, source: Uuid) -> bool {
        self.0
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(source)
    }
}

/// [`Sighting`] for `source` against the set of sources already materialized.
pub(super) fn sighting_for(seen: &HashSet<Uuid>, source: Uuid) -> Sighting {
    if seen.contains(&source) {
        Sighting::CloseOnly
    } else {
        Sighting::Materialize
    }
}

/// Purpose suffix a HANDOFF stamps on the child intent. Parameterised (rather
/// than inlined) because the respawn receiver reuses
/// [`build_child_intent`] and must say what it actually did — a respawn is not
/// a continuation of a live session.
pub(super) const HANDOFF_CONTINUATION_NOTE: &str = "continued here";

// ---------------------------------------------------------------------------
// Wire types — mirror `qontinui-coord/src/sessions.rs` Phase 7 shapes.
// ---------------------------------------------------------------------------

/// One pending handoff, as returned by
/// `GET /sessions/handoff-requests`. Mirrors coord's `PendingHandoff`.
#[derive(Debug, Clone, Deserialize)]
pub struct PendingHandoff {
    pub source_session_id: Uuid,
    pub target_device_id: Uuid,
    pub tenant_id: Uuid,
    pub session_kind: String,
}

/// Envelope coord returns from `GET /sessions/handoff-requests`.
#[derive(Debug, Clone, Deserialize)]
struct HandoffListResponse {
    #[serde(default)]
    handoffs: Vec<PendingHandoff>,
}

/// State-transfer bundle from `GET /sessions/:id/handoff-state`. Mirrors
/// coord's `HandoffState`.
#[derive(Debug, Clone, Deserialize)]
pub struct HandoffState {
    pub source_session_id: Uuid,
    #[allow(dead_code)]
    pub tenant_id: Uuid,
    #[allow(dead_code)]
    pub source_device_id: Uuid,
    pub session_kind: String,
    pub intent: serde_json::Value,
    pub repo: Option<String>,
    pub branch: Option<String>,
    #[serde(default)]
    pub held_claims: Vec<HeldClaim>,
    #[serde(default)]
    pub output_chunks: Vec<OutputChunk>,
}

/// One held claim to re-acquire under the new device.
#[derive(Debug, Clone, Deserialize)]
pub struct HeldClaim {
    pub kind: String,
    pub resource_key: String,
}

/// One warm-tier output chunk (base64) for scrollback replay.
#[derive(Debug, Clone, Deserialize)]
pub struct OutputChunk {
    #[allow(dead_code)]
    pub chunk_offset: i64,
    pub payload_b64: String,
}

/// Errors raised by the handoff trigger + receiver.
#[derive(Debug, thiserror::Error)]
pub enum HandoffError {
    #[error("coord HTTP error: {0}")]
    Http(String),
    #[error("coord returned status {0}: {1}")]
    Status(u16, String),
    #[error("response parse failed: {0}")]
    Parse(String),
    #[error("session error: {0}")]
    Session(String),
}

// ---------------------------------------------------------------------------
// Trigger
// ---------------------------------------------------------------------------

/// Body of `POST /sessions/:id/handoff`.
#[derive(Debug, Serialize)]
struct TriggerBody {
    target_device_id: Uuid,
}

/// Publish a handoff request for `source_session_id` to
/// `target_device_id`. POSTs `/sessions/:id/handoff`; coord records the
/// durable event + publishes the JetStream subject. Plan §Phase 7.
///
/// This is the runner-side trigger surface (also reachable from the
/// dashboard's "Continue elsewhere" button via the web backend proxy —
/// the dashboard hits coord directly through the proxy, this exists for
/// a runner-initiated handoff and is exercised by the unit test).
///
/// `tenant` is the SOURCE session's scope. `/sessions/{id}/…` is a
/// per-session route, so it presents that session's device-JWT slot rather than
/// the device default — the same rule the drain loop and the output pipe
/// follow. A caller that cannot resolve the session passes
/// [`TenantScope::Unresolved`], which keeps the default binding on a
/// single-bound device and degrades to unauthenticated on a multi-bound one.
pub async fn trigger_handoff(
    http: &reqwest::Client,
    coord_url: &str,
    source_session_id: Uuid,
    target_device_id: Uuid,
    tenant: TenantScope,
) -> Result<(), HandoffError> {
    let url = format!(
        "{}/sessions/{}/handoff",
        coord_url.trim_end_matches('/'),
        source_session_id
    );
    let resp = crate::auth::attach_device_auth_for(
        http.post(&url).json(&TriggerBody { target_device_id }),
        tenant,
    )
    .send()
    .await
    .map_err(|e| HandoffError::Http(format!("POST {url}: {e}")))?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(HandoffError::Status(
            status.as_u16(),
            body.chars().take(500).collect(),
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Receiver
// ---------------------------------------------------------------------------

/// Start the handoff-receiver task. Returns the [`JoinHandle`] so
/// `main.rs` can keep it alive for the lifetime of the process.
///
/// The task is **push-driven**: it opens a coord `/ws` subscription under
/// the `sessions` name (this tenant's subjects for this device) and
/// materializes each `handoff_request` addressed to this device the
/// instant coord fans it out. On every
/// (re)connect it also runs a single catch-up GET so anything published
/// while the runner was offline is replayed. Plan §Phase 7.
///
/// `lifecycle_store` is the durable restore registry: an accepted handoff
/// for a session with a mirrored `restore-record` event materializes a
/// local registry record so the EXISTING restore flow can resurrect the
/// session here (plan `2026-07-09-runner-session-history-cloud-sync` §3.4).
pub fn start_receiver_task(
    registry: Arc<SessionRegistry>,
    lifecycle_store: Arc<SessionLifecycleStore>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(run_receiver_loop(registry, lifecycle_store))
}

/// Derive coord's `/ws` URL (with the `sessions` subscription) from the
/// resolved coord HTTP base in `CoordSync`. The runner's `CoordSync` stores
/// the coord base in HTTP(S) form; the shared builder swaps the scheme and
/// appends `/ws` idempotently. Coord narrows the subscription to
/// `qontinui.sessions.<tenant>.<device>.*` from the upgrade credential — the
/// receiver's own filter ([`parse_handoff_push`]) already matches on
/// `.<self-device>.handoff_request`, so the tighter server-side scope
/// removes only frames it discarded anyway.
fn coord_ws_url(coord_http_base: &str) -> String {
    qontinui_runner_lib::coord_ws::build_ws_url(
        coord_http_base,
        qontinui_runner_lib::coord_ws::Subscription::Sessions,
    )
}

/// The receiver loop. Reconnects the coord `/ws` push subscription with
/// capped exponential backoff; on each successful connect it fires the
/// catch-up GET, then pumps inbound frames until the socket drops.
async fn run_receiver_loop(
    registry: Arc<SessionRegistry>,
    lifecycle_store: Arc<SessionLifecycleStore>,
) {
    let http = registry.coord_sync().http_client();
    let coord_url = registry.coord_sync().coord_url().to_string();
    let device_id = registry.machine_id();
    let ws_url = coord_ws_url(&coord_url);

    tracing::info!(
        coord_url = %coord_url,
        ws_url = %ws_url,
        device = %device_id,
        "session handoff: push receiver starting"
    );

    // Lives for the whole receiver (every reconnect): the duplicate-spawn
    // guard has to outlive the socket, or a reconnect would forget it.
    let sources = MaterializedSources::default();

    let mut backoff = RECONNECT_BACKOFF_FLOOR;
    loop {
        match connect_and_pump(
            &registry,
            &lifecycle_store,
            &http,
            &coord_url,
            &ws_url,
            device_id,
            &sources,
        )
        .await
        {
            Ok(()) => {
                tracing::debug!("session handoff: push WS closed cleanly; reconnecting");
                backoff = RECONNECT_BACKOFF_FLOOR;
            }
            Err(e) => {
                tracing::debug!(
                    error = %e,
                    backoff_secs = backoff.as_secs(),
                    "session handoff: push WS error; reconnecting after backoff"
                );
            }
        }
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(RECONNECT_BACKOFF_CEIL);
    }
}

/// One connect-and-pump iteration: open the coord `/ws` subscription, run
/// the on-connect catch-up, then forward each inbound `handoff_request`
/// frame addressed to this device to [`materialize`]. Returns on
/// disconnect (Ok = clean close, Err = transport error).
async fn connect_and_pump(
    registry: &Arc<SessionRegistry>,
    lifecycle_store: &Arc<SessionLifecycleStore>,
    http: &reqwest::Client,
    coord_url: &str,
    ws_url: &str,
    device_id: Uuid,
    sources: &MaterializedSources,
) -> Result<(), HandoffError> {
    let mut ws = qontinui_runner_lib::coord_ws::connect(ws_url, "session handoff")
        .await
        .map_err(|e| HandoffError::Http(format!("connect coord /ws {ws_url}: {e}")))?;

    tracing::info!(device = %device_id, "session handoff: push WS connected");

    // On-connect catch-up: replay anything that landed while we were
    // offline. Best-effort — a failure here doesn't abort the pump (the push
    // path still works, and the next tick or reconnect retries it).
    run_all_catchups(
        CatchupPass::OnConnect,
        registry,
        lifecycle_store,
        http,
        coord_url,
        device_id,
        sources,
    )
    .await;

    // Periodic catch-up (module doc, point 3): bounds the delivery window on a
    // socket that is up but delivers nothing — the pre-`subscribe=` coord.
    let mut catchup_tick = catchup_interval();
    // The first tick fires immediately; the on-connect catch-up just ran.
    catchup_tick.tick().await;

    loop {
        tokio::select! {
            _ = catchup_tick.tick() => {
                run_all_catchups(CatchupPass::Tick, registry, lifecycle_store, http, coord_url, device_id, sources).await;
            }
            maybe_msg = ws.next() => {
                let Some(msg) = maybe_msg else {
                    // Stream ended (peer hung up without a Close frame).
                    return Ok(());
                };
                let msg = msg.map_err(|e| HandoffError::Http(format!("coord /ws recv: {e}")))?;
                match msg {
                    tokio_tungstenite::tungstenite::Message::Text(t) => {
                        handle_push_frame(
                            registry,
                            lifecycle_store,
                            http,
                            coord_url,
                            device_id,
                            sources,
                            t.as_str(),
                        )
                        .await;
                    }
                    tokio_tungstenite::tungstenite::Message::Binary(b) => {
                        let s = String::from_utf8_lossy(&b);
                        handle_push_frame(
                            registry,
                            lifecycle_store,
                            http,
                            coord_url,
                            device_id,
                            sources,
                            &s,
                        )
                        .await;
                    }
                    tokio_tungstenite::tungstenite::Message::Ping(p) => {
                        // Keep the socket alive — coord's `/ws` answers our pings,
                        // but reply to server pings too.
                        let _ = ws
                            .send(tokio_tungstenite::tungstenite::Message::Pong(p))
                            .await;
                    }
                    tokio_tungstenite::tungstenite::Message::Close(_) => {
                        tracing::debug!("session handoff: push WS closed by peer");
                        return Ok(());
                    }
                    _ => {}
                }
            }
        }
    }
}

/// Which pass is asking for a catch-up — the input to [`catchups_for`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CatchupPass {
    /// A (re)connect of this receiver's socket. Everything that landed while
    /// the socket was down has to be replayed, so this runs EVERY arm.
    OnConnect,
    /// A [`CATCHUP_TICK`] on a live socket.
    Tick,
}

/// The catch-up arms this one socket's (re)connect drives, each on its own
/// coord route. They ride together because they share a socket, not because
/// they share a schedule — see [`catchups_for`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CatchupKind {
    /// The durable pending `handoff_request` list.
    Handoff,
    /// The pending respawn requests (`session::respawn`).
    Respawn,
    /// The remote-ATTACH grants (`session::attach`).
    Attach,
    /// The remote-CREATE grants (`session::create`).
    Create,
}

/// Which arms a pass drives, and the whole of why the two passes differ.
///
/// `OnConnect` runs all four: a socket that was down missed every push, so
/// every arm needs its replay.
///
/// `Tick` runs only the two arms that have **no other periodic owner**:
///
/// - `Handoff` and `Respawn` are driven by nothing else in the process. The
///   on-connect replay was their only backstop, which is the gap
///   [`CATCHUP_TICK`] exists to close.
/// - `Attach` and `Create` each already have a process-lifetime 60 s poll
///   task — `session::attach::start_poll_task` and
///   `session::create::start_poll_task`, both spawned in `main.rs`, both on a
///   `POLL_INTERVAL` equal to [`CATCHUP_TICK`]. Driving them from here as
///   well issues two GETs a minute to each of those routes and delivers
///   nothing the existing poll would not have delivered within the same
///   minute.
///
/// Expressed as a function over the pass rather than as "which helper the
/// select arm happens to call", because the asymmetry IS the decision and at
/// the call site it is invisible.
pub(super) fn catchups_for(pass: CatchupPass) -> &'static [CatchupKind] {
    match pass {
        CatchupPass::OnConnect => &[
            CatchupKind::Handoff,
            CatchupKind::Respawn,
            CatchupKind::Attach,
            CatchupKind::Create,
        ],
        CatchupPass::Tick => &[CatchupKind::Handoff, CatchupKind::Respawn],
    }
}

/// Run the catch-up GETs [`catchups_for`] selects for `pass`, in order. Each
/// one is best-effort and self-logging; none aborts the pump.
async fn run_all_catchups(
    pass: CatchupPass,
    registry: &Arc<SessionRegistry>,
    lifecycle_store: &Arc<SessionLifecycleStore>,
    http: &reqwest::Client,
    coord_url: &str,
    device_id: Uuid,
    sources: &MaterializedSources,
) {
    for kind in catchups_for(pass) {
        match kind {
            // The durable `handoff_request` event row in coord is the source
            // of truth; this GET drains it.
            CatchupKind::Handoff => {
                run_catchup(
                    registry,
                    lifecycle_store,
                    http,
                    coord_url,
                    device_id,
                    sources,
                )
                .await
            }
            // The RESPAWN catch-up, on its own coord route. Separate on
            // purpose: the handoff read filters `s.state <> 'closed'` (fatal
            // for a respawn, whose source is closed by construction) and
            // `PendingHandoff` carries neither the account pin nor the Claude
            // session id. Plan `2026-08-26-sessions-console-consolidation` §6
            // Phase 5. Safe to repeat on the tick without a
            // [`MaterializedSources`]-style guard: its dedup is coord's
            // server-side materialized-child filter (keyed on the
            // `parent_session_id` the resume stamps), and the per-session
            // `MIGRATION_CAP` bounds any residue at 3 per 24 h rather than
            // one per tick.
            CatchupKind::Respawn => {
                super::respawn::run_catchup(registry, lifecycle_store, http, coord_url, device_id)
                    .await
            }
            // The remote-ATTACH grants coord minted for this device while the
            // socket was down (plan
            // `2026-08-31-remote-session-tabs-in-runner-terminal`, Phase 3c),
            // into the table the backend relay's terminal handlers enforce
            // against.
            CatchupKind::Attach => {
                super::attach::run_catchup(http, coord_url, device_id, super::attach::CATCHUP_TIMEOUT)
                    .await
            }
            // The remote-CREATE grants beside them (plan
            // `2026-09-11-headless-runner-parity-from-a-headed-runner`, Phase
            // 3b). Without this the target has no source for the grants coord
            // minted while it was down, and a `terminal_create` arriving under
            // one of them is refused — correct, but avoidable.
            CatchupKind::Create => {
                super::create::run_catchup(
                    http,
                    coord_url,
                    device_id,
                    super::create::CATCHUP_TIMEOUT,
                )
                .await
            }
        }
    }
}

/// Run the one-shot handoff catch-up: GET the durable pending list and
/// materialize each. Used on every (re)connect and on every [`CATCHUP_TICK`].
async fn run_catchup(
    registry: &Arc<SessionRegistry>,
    lifecycle_store: &Arc<SessionLifecycleStore>,
    http: &reqwest::Client,
    coord_url: &str,
    device_id: Uuid,
    sources: &MaterializedSources,
) {
    match fetch_pending(http, coord_url, device_id).await {
        Ok(pending) => {
            if !pending.is_empty() {
                tracing::info!(
                    count = pending.len(),
                    "session handoff: catch-up replaying pending handoffs"
                );
            }
            for handoff in pending {
                materialize_logged(
                    registry,
                    lifecycle_store,
                    http,
                    coord_url,
                    sources,
                    &handoff,
                )
                .await;
            }
        }
        Err(HandoffError::Status(401 | 403, _)) => {
            // Pre-pairing / early-reconnect window: coord (once it gates the
            // handoff readers with FleetPrincipal) rejects the anonymous GET
            // until the device-JWT lands. Not fatal — the catch-up re-runs on
            // the next (re)connect, and the push path stays active. One line.
            tracing::warn!(
                "session handoff: catch-up GET unauthorized (401/403) — retrying after device pairing/auth"
            );
        }
        Err(e) => {
            tracing::debug!(error = %e, "session handoff: catch-up GET failed (push path still active)");
        }
    }
}

/// Parse one inbound coord `/ws` envelope. Coord wraps each pub/sub
/// message as `{"channel": "<subject>", "payload": "<json-string>"}`.
/// We accept handoff frames whose channel is
/// `qontinui.sessions.<tenant>.<self-device>.handoff_request` (the
/// machine-scoped subject coord publishes on for the TARGET device), then
/// materialize. Frames for other devices / other subjects are ignored.
async fn handle_push_frame(
    registry: &Arc<SessionRegistry>,
    lifecycle_store: &Arc<SessionLifecycleStore>,
    http: &reqwest::Client,
    coord_url: &str,
    device_id: Uuid,
    sources: &MaterializedSources,
    text: &str,
) {
    // The RESPAWN arm shares this one socket (coord publishes respawns on the
    // same `qontinui.sessions.<tenant>.<device>.<kind>` family). The two arms
    // are disambiguated ONLY by the channel's trailing segment — this parser
    // requires `.handoff_request`, `parse_respawn_push` requires
    // `.respawn_request` — so neither can swallow the other's frames and no
    // frame is materialized twice. Both directions are asserted in
    // `respawn`'s tests.
    super::respawn::handle_push_frame(registry, lifecycle_store, http, coord_url, device_id, text)
        .await;
    // The ATTACH arm — third suffix on the same socket (`.attach_request`),
    // same disambiguation. Records a grant; materializes nothing.
    super::attach::handle_push_frame(device_id, text);
    // The CREATE arm — fourth suffix on the same socket (`.create_request`),
    // same disambiguation. Records a grant; materializes nothing. The two
    // grant arms are disjoint in both directions (asserted in `create`'s
    // tests), so an attach grant can never land in the create table.
    super::create::handle_push_frame(device_id, text);

    let Some(handoff) = parse_handoff_push(text, device_id) else {
        return;
    };
    tracing::info!(
        source = %handoff.source_session_id,
        "session handoff: push received; materializing"
    );
    materialize_logged(
        registry,
        lifecycle_store,
        http,
        coord_url,
        sources,
        &handoff,
    )
    .await;
}

/// Pure parse+filter of a coord `/ws` envelope into a [`PendingHandoff`]
/// addressed to `device_id`. Returns `None` when the frame isn't a
/// handoff for this device (so the pump can ignore it). Factored out so
/// the unit tests can exercise the channel-matching + payload-decode
/// without a live WS.
pub(super) fn parse_handoff_push(text: &str, device_id: Uuid) -> Option<PendingHandoff> {
    let envelope: serde_json::Value = serde_json::from_str(text).ok()?;

    // Coord `/ws` envelope: {"channel": "<subject>", "payload": "<json>"}.
    // The payload is itself a JSON string (Redis pub/sub carries strings).
    let channel = envelope.get("channel").and_then(|c| c.as_str())?;

    // Match `qontinui.sessions.<tenant>.<device>.handoff_request` and
    // require the device segment == this device. We match on the
    // device + kind segments rather than reconstructing the full subject
    // (we don't carry the tenant here) — the device segment is the
    // address, the trailing segment is the event kind.
    let suffix = format!(".{device_id}.handoff_request");
    if !channel.starts_with("qontinui.sessions.") || !channel.ends_with(&suffix) {
        return None;
    }

    // Payload may be a JSON string (Redis arm) or an inlined object
    // (defensive — some envelopes inline). Handle both.
    let payload_val = match envelope.get("payload") {
        Some(serde_json::Value::String(s)) => serde_json::from_str::<serde_json::Value>(s).ok()?,
        Some(other) => other.clone(),
        None => return None,
    };

    let source_session_id = payload_val
        .get("source_session_id")
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok())?;
    let target_device_id = payload_val
        .get("target_device_id")
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok())
        .unwrap_or(device_id);
    // Defense-in-depth: the channel already filtered by device, but if a
    // payload's target disagrees, trust the address, not the body.
    if target_device_id != device_id {
        return None;
    }
    let tenant_id = payload_val
        .get("tenant_id")
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok())
        .unwrap_or_else(Uuid::nil);
    let session_kind = payload_val
        .get("session_kind")
        .and_then(|v| v.as_str())
        .unwrap_or("terminal_shell")
        .to_string();

    Some(PendingHandoff {
        source_session_id,
        target_device_id,
        tenant_id,
        session_kind,
    })
}

/// Materialize a handoff and log (but swallow) any error. The source is
/// left intact on failure so the next push/catch-up retries.
///
/// A source this process has ALREADY started a child for (`sources`) is not
/// materialized again: the repeat sighting means the earlier `close_source`
/// failed and the handoff is still pending, so only the close is retried —
/// never a second child (module doc, point 3).
async fn materialize_logged(
    registry: &Arc<SessionRegistry>,
    lifecycle_store: &Arc<SessionLifecycleStore>,
    http: &reqwest::Client,
    coord_url: &str,
    sources: &MaterializedSources,
    handoff: &PendingHandoff,
) {
    match sources.sighting(handoff.source_session_id) {
        Sighting::Materialize => {
            if let Err(e) =
                materialize(registry, lifecycle_store, http, coord_url, sources, handoff).await
            {
                tracing::warn!(
                    source = %handoff.source_session_id,
                    error = %e,
                    "session handoff: materialize failed; source left intact, will retry on next push/catch-up"
                );
            }
        }
        Sighting::CloseOnly => {
            tracing::info!(
                source = %handoff.source_session_id,
                "session handoff: source already materialized by this process; retrying close only (no second child)"
            );
            match close_source(http, coord_url, handoff.source_session_id).await {
                Ok(()) => tracing::info!(
                    source = %handoff.source_session_id,
                    "session handoff: deferred close of the source succeeded"
                ),
                Err(e) => tracing::warn!(
                    source = %handoff.source_session_id,
                    error = %e,
                    "session handoff: deferred close of the source failed again; will retry on next tick"
                ),
            }
        }
    }
}

/// Fetch the durable pending-handoff list for this device. Used by the
/// on-(re)connect catch-up. (Same coord endpoint the previous poll loop
/// used — now invoked once per connect rather than every 5s.)
async fn fetch_pending(
    http: &reqwest::Client,
    coord_url: &str,
    device_id: Uuid,
) -> Result<Vec<PendingHandoff>, HandoffError> {
    let url = format!(
        "{}/sessions/handoff-requests?device_id={}",
        coord_url.trim_end_matches('/'),
        device_id
    );
    let resp = crate::coord_http::coord_get(http, &url)
        .send()
        .await
        .map_err(|e| HandoffError::Http(format!("GET {url}: {e}")))?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(HandoffError::Status(
            status.as_u16(),
            body.chars().take(300).collect(),
        ));
    }
    let parsed: HandoffListResponse = resp
        .json()
        .await
        .map_err(|e| HandoffError::Parse(format!("decode handoff list: {e}")))?;
    Ok(parsed.handoffs)
}

/// Fetch the state-transfer bundle for a source session.
pub(super) async fn fetch_state(
    http: &reqwest::Client,
    coord_url: &str,
    source_session_id: Uuid,
) -> Result<HandoffState, HandoffError> {
    let url = format!(
        "{}/sessions/{}/handoff-state",
        coord_url.trim_end_matches('/'),
        source_session_id
    );
    let resp = crate::coord_http::coord_get(http, &url)
        .send()
        .await
        .map_err(|e| HandoffError::Http(format!("GET {url}: {e}")))?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(HandoffError::Status(
            status.as_u16(),
            body.chars().take(300).collect(),
        ));
    }
    resp.json()
        .await
        .map_err(|e| HandoffError::Parse(format!("decode handoff state: {e}")))
}

/// Materialize one handoff: build the child intent, start the child
/// session with `parent_session_id`, re-acquire claims, replay
/// scrollback, then close the source. Plan §Phase 7.
async fn materialize(
    registry: &Arc<SessionRegistry>,
    lifecycle_store: &Arc<SessionLifecycleStore>,
    http: &reqwest::Client,
    coord_url: &str,
    sources: &MaterializedSources,
    handoff: &PendingHandoff,
) -> Result<(), HandoffError> {
    let state = fetch_state(http, coord_url, handoff.source_session_id).await?;

    let intent = build_child_intent(&state, HANDOFF_CONTINUATION_NOTE)?;
    // Captured before `intent` moves into the registry: the child inherits the
    // SOURCE session's tenant (`build_child_intent` carries `tenant_id` across),
    // and the claims re-acquired below belong to that same tenant.
    let tenant = TenantScope::for_session(intent.tenant_id);

    // Start the child session locally with lineage back to the source.
    let child = registry
        .start_with_parent(intent, handoff.source_session_id)
        .map_err(|e| HandoffError::Session(e.to_string()))?;
    let child_id = child.id();
    // Marked the moment the child EXISTS — before the close below, whose
    // failure is exactly what makes this source get sighted again.
    sources.mark(handoff.source_session_id);

    tracing::info!(
        source = %handoff.source_session_id,
        child = %child_id,
        "session handoff: materialized child session"
    );

    // Re-acquire held claims under this device. Idempotent by
    // resource_key; failures are logged but don't abort the handoff —
    // the session row + scrollback are the load-bearing artifacts.
    let device_id = registry.machine_id();
    for claim in &state.held_claims {
        if let Err(e) = reacquire_claim(http, coord_url, claim, device_id, tenant).await {
            tracing::warn!(
                kind = %claim.kind,
                resource_key = %claim.resource_key,
                error = %e,
                "session handoff: claim re-acquire failed (best-effort)"
            );
        }
    }

    // Replay warm-tier scrollback into the new PTY, in order.
    for chunk in &state.output_chunks {
        match base64::engine::general_purpose::STANDARD.decode(&chunk.payload_b64) {
            Ok(bytes) => {
                if let Err(e) = registry.write_input(child_id, &bytes) {
                    tracing::warn!(
                        child = %child_id,
                        error = %e,
                        "session handoff: scrollback replay write failed"
                    );
                    break;
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "session handoff: scrollback chunk base64 decode failed");
            }
        }
    }

    // Tear down the source FIRST — one-way move. coord's DELETE sets
    // state='closed', closed_at=now(), and releases the source claim. This
    // deliberately runs before the restore-registry materialization below
    // (F6): the materialization's bounded SSE read pays an idle wait (up to
    // RESTORE_RECORD_FETCH_DEADLINE) even when the source mirrored nothing,
    // and the load-bearing teardown must not queue behind it. Ordering is
    // safe: coord's DELETE is a soft close (the row and its
    // coord.session_events rows survive), so the events replay still serves
    // the mirror afterwards.
    let close_result = close_source(http, coord_url, handoff.source_session_id).await;

    // Phase 4 (session-history cloud sync §3.4) — materialize a local
    // restore-registry record from the source's newest mirrored
    // `restore-record` event, so the EXISTING restore flow can resurrect
    // the session on THIS machine. Best-effort: a session with no mirror
    // (cloud sync off at the source, no linked terminal) simply has no
    // record to materialize, and no failure here aborts the handoff. Runs
    // regardless of the close outcome so a failed close doesn't also cost
    // the registry record.
    materialize_restore_registry(
        registry,
        lifecycle_store,
        http,
        coord_url,
        handoff.source_session_id,
        child_id,
    )
    .await;

    close_result
}

// ---------------------------------------------------------------------------
// Phase 4 — restore-registry materialization (plan
// `2026-07-09-runner-session-history-cloud-sync` §3.4)
// ---------------------------------------------------------------------------

/// Overall deadline for the bounded `GET /sessions/:id/events` read.
const RESTORE_RECORD_FETCH_DEADLINE: Duration = Duration::from_secs(8);
/// Per-chunk idle timeout: the SSE endpoint replays the durable rows
/// immediately on connect and then live-tails (silence until the next
/// published event, keep-alive pings every 15s) — a short idle gap after
/// the replay burst means the durable window is fully read.
const RESTORE_RECORD_FETCH_IDLE: Duration = Duration::from_secs(2);

/// Fixed UUIDv5 namespace for provisional (terminal-only) handoff record
/// keys: the key is `Uuid::new_v5(&NS, source_session_id)`, so retrying a
/// materialization for the same source upserts the SAME registry record
/// instead of minting a fresh `Uuid::new_v4` duplicate each attempt.
/// Randomly generated once for this purpose — never change it, or retries
/// stop being idempotent across builds.
const HANDOFF_PROVISIONAL_KEY_NS: Uuid = Uuid::from_u128(0x8f1d_5b0e_43c2_4a7a_9f6e_2d81_c4b3_7a59);

/// Fetch + materialize, logging (but swallowing) every failure.
async fn materialize_restore_registry(
    registry: &Arc<SessionRegistry>,
    lifecycle_store: &Arc<SessionLifecycleStore>,
    http: &reqwest::Client,
    coord_url: &str,
    source_session_id: Uuid,
    child_id: Uuid,
) {
    let Some(payload) = fetch_latest_restore_record(http, coord_url, source_session_id).await
    else {
        tracing::debug!(
            source = %source_session_id,
            "session handoff: no restore-record event for source — skipping registry materialization"
        );
        return;
    };

    // Attach the record to the child's REAL PTY terminal when it has one,
    // so the registry's liveness poll matches a live terminal instead of
    // orphan-closing a synthetic id. Non-PTY child kinds fall back to a
    // deterministic placeholder.
    let terminal_id = registry
        .pty_terminal_id(child_id)
        .unwrap_or_else(|| format!("handoff-{source_session_id}"));

    let record = registry_record_from_restore_payload(&payload, &terminal_id, source_session_id);
    let session_key = record.claude_session_id.clone();
    let tier_full = record.confirmed_at.is_some();
    lifecycle_store.record_open(record);
    tracing::info!(
        source = %source_session_id,
        child = %child_id,
        session = %session_key,
        tier = if tier_full { "full" } else { "terminal_only" },
        "session handoff: materialized restore-registry record"
    );
}

/// Read the durable-replay window of `GET /sessions/:id/events` (SSE) and
/// return the payload of the NEWEST (highest-seq) `restore-record` event,
/// if any. Coord's `HandoffState` bundle does not carry session events, so
/// this is the read path for the mirrored registry record. Bounded: the
/// stream live-tails after the replay, so reading stops on a short idle
/// gap, the overall deadline, or stream end — whichever comes first.
async fn fetch_latest_restore_record(
    http: &reqwest::Client,
    coord_url: &str,
    session_id: Uuid,
) -> Option<serde_json::Value> {
    let url = format!(
        "{}/sessions/{}/events",
        coord_url.trim_end_matches('/'),
        session_id
    );
    let resp = match crate::coord_http::coord_get(http, &url).send().await {
        Ok(r) => r,
        Err(e) => {
            tracing::debug!(error = %e, "session handoff: restore-record fetch failed (GET events)");
            return None;
        }
    };
    if !resp.status().is_success() {
        tracing::debug!(
            status = %resp.status(),
            session = %session_id,
            "session handoff: restore-record fetch rejected — proceeding without registry record"
        );
        return None;
    }

    let mut resp = resp;
    let mut buf = String::new();
    let deadline = tokio::time::Instant::now() + RESTORE_RECORD_FETCH_DEADLINE;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(RESTORE_RECORD_FETCH_IDLE.min(remaining), resp.chunk()).await {
            Ok(Ok(Some(bytes))) => buf.push_str(&String::from_utf8_lossy(&bytes)),
            Ok(Ok(None)) => break, // stream ended (e.g. no live tail configured)
            Ok(Err(e)) => {
                tracing::debug!(error = %e, "session handoff: restore-record fetch stream error");
                break;
            }
            Err(_) => break, // idle — the replay burst is fully read
        }
    }
    latest_restore_record_from_sse(&buf)
}

/// Pure parse of an accumulated SSE buffer into the newest `restore-record`
/// payload. Frames are `\n\n`-separated; each carries `data: <json>` lines
/// (the replay frames serialize coord's `SessionEventRow`:
/// `{id, session_id, seq, event_kind, payload, occurred_at}`). "Newest" is
/// max `seq` with last-wins on ties, so re-emitted (debounce-reset) rows
/// resolve to the latest state.
fn latest_restore_record_from_sse(buf: &str) -> Option<serde_json::Value> {
    let mut best: Option<(i64, serde_json::Value)> = None;
    for frame in buf.split("\n\n") {
        let mut data = String::new();
        for line in frame.lines() {
            if let Some(rest) = line.strip_prefix("data:") {
                data.push_str(rest.trim_start());
            }
        }
        if data.is_empty() {
            continue;
        }
        let Ok(row) = serde_json::from_str::<serde_json::Value>(&data) else {
            continue;
        };
        if row.get("event_kind").and_then(|v| v.as_str()) != Some(RESTORE_RECORD_EVENT) {
            continue;
        }
        let Some(payload) = row.get("payload").filter(|p| p.is_object()).cloned() else {
            continue;
        };
        let seq = row.get("seq").and_then(|v| v.as_i64()).unwrap_or(0);
        if best.as_ref().map(|(s, _)| seq >= *s).unwrap_or(true) {
            best = Some((seq, payload));
        }
    }
    best.map(|(_, p)| p)
}

/// Pure mapping: a mirrored `restore-record` payload → a local
/// [`TerminalSessionRecord`] that FEEDS THE EXISTING restore flow (the
/// frontend `classifyRestoreAction` gate), honoring tiers honestly:
///
/// - `restore_tier == "full"` with an authoritative id → the record is
///   keyed by that id, origin `authoritative`, CONFIRMED (the emitter only
///   claims `full` for source-confirmed records) — the classifier
///   auto-resumes the conversation via the provider's `--resume <id>`.
/// - anything else (`terminal_only`, or a malformed `full` with no id) →
///   a deterministic per-source key (UUIDv5 of `source_session_id` under
///   [`HANDOFF_PROVISIONAL_KEY_NS`], so a materialization retry upserts the
///   SAME record instead of piling up duplicates), origin `authoritative`,
///   PROVISIONAL (`confirmed_at` unset) — the classifier's phantom-shell
///   branch restores terminal+cwd with an honest fresh conversation and
///   never types a resume. (NOT `reconciled`: that origin quarantines
///   behind a resume-confirm banner, which would be dishonest for a record
///   that has no conversation to resume.)
///
/// `record_open` stamps the timestamps; placeholders here are overwritten.
fn registry_record_from_restore_payload(
    payload: &serde_json::Value,
    terminal_id: &str,
    source_session_id: Uuid,
) -> TerminalSessionRecord {
    let provider = payload
        .get("provider")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or(DEFAULT_PROVIDER)
        .to_string();
    let cwd = payload
        .get("cwd")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let authoritative_id = payload
        .get("authoritative_session_id")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let tier = payload
        .get("restore_tier")
        .and_then(|v| v.as_str())
        .unwrap_or("terminal_only");

    let (claude_session_id, confirmed_at) = match (tier, authoritative_id) {
        (t, Some(id)) if t == TIER_FULL => {
            (id.to_string(), Some(chrono::Utc::now().timestamp_millis()))
        }
        // Honest degrade: no resumable id ⇒ terminal-only semantics under a
        // DETERMINISTIC per-source key (a real UUID, so shell-safety
        // validation and future confirmations behave normally; v5 of the
        // source session id, so a materialization retry is idempotent —
        // record_open upserts by this key instead of minting a duplicate).
        _ => (
            Uuid::new_v5(&HANDOFF_PROVISIONAL_KEY_NS, source_session_id.as_bytes()).to_string(),
            None,
        ),
    };

    TerminalSessionRecord {
        claude_session_id,
        config_dir: None,
        working_dir: cwd,
        page_id: "default".to_string(),
        zone_index: 0,
        title: Some(format!("{provider} (handed off)")),
        terminal_id: terminal_id.to_string(),
        // record_open seeds these from `now`; values here are placeholders.
        opened_at: 0,
        last_seen_at: 0,
        state: "open".to_string(),
        closed_at: None,
        close_reason: None,
        provider,
        origin: Some(ORIGIN_AUTHORITATIVE.to_string()),
        restore_pending_at: None,
        confirmed_at,
        handle: None,
        account_label: None,
        account_wrapper: None,
        session_name: None,
        name_source: None,
        tenant_id: None,
        task_run_id: None,
        bypass_permissions: None,
        restored_from_boot_at: None,
        restore_tier: None,
        finished_at: None,
        finish_reason: None,
        finish_synced: false,
    }
}

/// Build the child session [`Intent`] from the source state. cwd comes
/// from `repo` (the PTY transport uses `intent.repo` as the working
/// dir); `declared_paths` + branch carry over verbatim. The purpose is
/// annotated so the dashboard shows the lineage at a glance.
pub(super) fn build_child_intent(
    state: &HandoffState,
    continuation_note: &str,
) -> Result<Intent, HandoffError> {
    let kind = SessionKind::parse(&state.session_kind).ok_or_else(|| {
        HandoffError::Parse(format!("unknown session_kind: {}", state.session_kind))
    })?;

    // Pull purpose + declared_paths + share_output from the source intent
    // JSON. Default sensibly on any missing field so a sparse source
    // intent still materializes.
    let src = &state.intent;
    let source_purpose = src
        .get("purpose")
        .and_then(|v| v.as_str())
        .unwrap_or("handoff session");
    let purpose = format!("{source_purpose} ({continuation_note})");
    let declared_paths = src
        .get("declared_paths")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|p| p.as_str())
                .map(std::path::PathBuf::from)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let share_output = src
        .get("share_output")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let redact_secrets = src.get("redact_secrets").and_then(|v| v.as_bool());
    // Dual-read: coord renamed the wire key `plan_slug` → `work_unit_slug`.
    // Read the new name first and fall back to the legacy one so a source
    // intent written by EITHER coord release materializes.
    // (The writer leg has since switched: `Intent` now carries the canonical
    // `work_unit_slug` and emits ONLY that key — plan
    // `2026-07-30-coord-web-plan-slug-wire-key-retirement` Phase 1 step 6.
    // That makes this fallback the ONLY thing standing between a pre-rename
    // blob and a lost slug.)
    //
    // ** THE `plan_slug` FALLBACK IS PERMANENT. DO NOT "CLEAN IT UP". **
    // This is not a migration window that eventually closes. `src` is a
    // persisted `coord.sessions.intent` JSONB blob, and Phase 5 of plan
    // `2026-07-30-coord-web-plan-slug-wire-key-retirement` decided (a) LEAVE
    // the historical blobs unmigrated: every session row written before the
    // rename carries only `plan_slug`, and those rows never expire. Deleting
    // the fallback would silently hand `None` to every handoff off a
    // pre-rename session — a quiet degradation, not an error. The three-line
    // fallback is the entire cost of never having to rewrite that table.
    // `.as_str()` must be applied BEFORE the fallback, not after. `.get()`
    // returns `Some(Value::Null)` for a present-but-null key, so an
    // `.or_else(...).and_then(as_str)` chain would treat an explicit
    // `{"work_unit_slug": null, "plan_slug": "x"}` as "new key present" and
    // drop the slug entirely instead of falling back. Coord emits explicit
    // nulls on exactly this shape — its autonomous-dispatch metadata is built
    // with `json!({"plan_slug": slug, "work_unit_slug": slug})`, which
    // serializes `None` as `null` rather than omitting the key.
    let work_unit_slug = src
        .get("work_unit_slug")
        .and_then(|v| v.as_str())
        .or_else(|| src.get("plan_slug").and_then(|v| v.as_str()))
        .map(str::to_string);
    let correlation_topic = src
        .get("correlation_topic")
        .and_then(|v| v.as_str())
        .map(str::to_string);

    // Phase 8b — carry the SOURCE session's tenant binding across the
    // handoff (session tenancy is immutable; the child continues the same
    // tenant's work). Absent on legacy source intents → None, and the
    // registry stamps this machine's default at materialization.
    let tenant_id = src
        .get("tenant_id")
        .and_then(|v| v.as_str())
        .and_then(|s| uuid::Uuid::parse_str(s.trim()).ok());

    Ok(Intent {
        kind,
        purpose,
        repo: state.repo.clone(),
        branch: state.branch.clone(),
        // The child intent is re-serialized under the canonical key alone —
        // the dual-read above is what folds a legacy blob into it.
        work_unit_slug,
        plan_slug: None,
        correlation_topic,
        page_id: None,
        declared_paths,
        share_output,
        redact_secrets,
        tenant_id,
    })
}

/// Re-acquire one claim under `device_id` via `POST /claims/acquire`.
/// The kind string maps to coord's `ClaimKind` snake_case wire form.
/// `tenant` is the SOURCE session's scope, carried down from the child intent
/// `build_child_intent` just derived (Phase 5, plan
/// `2026-08-29-runner-work-scoped-writes-default-tenant-credential` §D1). Passed
/// rather than looked up because the caller is already holding it: the claim
/// being re-acquired belongs to the session being moved, and coord stamps
/// `metadata.tenant_id` on the acquire audit row from the body it is given.
///
/// `pub(super)` because `session::respawn` re-acquires the same claims for the
/// same reason; it derives its `tenant` from its own `child_intent`, so the two
/// callers agree by construction rather than by convention.
pub(super) async fn reacquire_claim(
    http: &reqwest::Client,
    coord_url: &str,
    claim: &HeldClaim,
    device_id: Uuid,
    tenant: TenantScope,
) -> Result<(), HandoffError> {
    let url = format!("{}/claims/acquire", coord_url.trim_end_matches('/'));
    let mut body = json!({
        "kind": claim.kind,
        "resource_key": claim.resource_key,
        "machine_id": device_id.to_string(),
    });
    if let Some(t) = tenant.declared_tenant() {
        body["tenant_id"] = json!(t);
    }
    let resp = crate::auth::attach_device_auth_for(http.post(&url).json(&body), tenant)
        .send()
        .await
        .map_err(|e| HandoffError::Http(format!("POST {url}: {e}")))?;
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(HandoffError::Status(
            status.as_u16(),
            text.chars().take(200).collect(),
        ));
    }
    Ok(())
}

/// Close the source session via `DELETE /sessions/:id`.
async fn close_source(
    http: &reqwest::Client,
    coord_url: &str,
    source_session_id: Uuid,
) -> Result<(), HandoffError> {
    let url = format!(
        "{}/sessions/{}",
        coord_url.trim_end_matches('/'),
        source_session_id
    );
    // coord-tenant-scope(escalated): source_session_id is the fn's parameter, so a tenant IS resolvable here -- but census E2 found DELETE /sessions/{id} mounted on coord's admin-gated operator_admin_writes router, which needs the coord `admin` role from a forwarded Cognito operator bearer and whose own comment asserts "the runner does NOT call these". No device-JWT slot satisfies that, so the open question is whether the mount or this call is wrong, not which credential to present. Census E2.
    let resp = crate::auth::attach_device_auth(http.delete(&url))
        .send()
        .await
        .map_err(|e| HandoffError::Http(format!("DELETE {url}: {e}")))?;
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(HandoffError::Status(
            status.as_u16(),
            text.chars().take(200).collect(),
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // =======================================================================
    // Catch-up dedupe (module doc, point 3): a source already materialized by
    // this process is sighted again only because its close failed, so the
    // second sighting is close-only — never a second child.
    // =======================================================================

    #[test]
    fn handoff_dedupe_first_sighting_materializes_second_is_close_only() {
        let sources = MaterializedSources::default();
        let src = Uuid::new_v4();
        let other = Uuid::new_v4();

        assert_eq!(sources.sighting(src), Sighting::Materialize);
        // The child is started: mark. First record changes the set.
        assert!(sources.mark(src));
        // Every later sighting of the SAME source (the next tick, the next
        // reconnect's catch-up, a replayed push frame) is close-only.
        assert_eq!(sources.sighting(src), Sighting::CloseOnly);
        assert_eq!(sources.sighting(src), Sighting::CloseOnly);
        // Re-marking is idempotent and does not flip the decision.
        assert!(!sources.mark(src));
        assert_eq!(sources.sighting(src), Sighting::CloseOnly);
        // A different source is unaffected.
        assert_eq!(sources.sighting(other), Sighting::Materialize);
    }

    #[test]
    fn handoff_dedupe_pure_decision_is_membership() {
        let src = Uuid::new_v4();
        let mut seen = HashSet::new();
        assert_eq!(sighting_for(&seen, src), Sighting::Materialize);
        seen.insert(src);
        assert_eq!(sighting_for(&seen, src), Sighting::CloseOnly);
        assert_eq!(sighting_for(&seen, Uuid::new_v4()), Sighting::Materialize);
    }

    /// A (re)connect missed every push, so it replays EVERY arm.
    #[test]
    fn on_connect_replays_every_catchup_arm() {
        assert_eq!(
            catchups_for(CatchupPass::OnConnect),
            &[
                CatchupKind::Handoff,
                CatchupKind::Respawn,
                CatchupKind::Attach,
                CatchupKind::Create,
            ]
        );
    }

    /// The tick drives ONLY the arms nothing else drives. `attach` and
    /// `create` each have their own 60 s poll task in `main.rs`, so adding
    /// them here is a doubled GET, not a second backstop. This test fails the
    /// moment someone "restores symmetry" between the two passes.
    #[test]
    fn the_tick_drives_only_the_arms_with_no_other_periodic_owner() {
        let ticked = catchups_for(CatchupPass::Tick);
        assert_eq!(ticked, &[CatchupKind::Handoff, CatchupKind::Respawn]);
        for owned_elsewhere in [CatchupKind::Attach, CatchupKind::Create] {
            assert!(
                !ticked.contains(&owned_elsewhere),
                "{owned_elsewhere:?} already has a 60s poll task; the tick must not double it"
            );
        }
    }

    /// The doubling this split removes is only a doubling because the two
    /// schedules coincide. Pinned so a change to either period is a decision
    /// taken here rather than a silent re-divergence.
    #[test]
    fn the_polled_arms_share_the_ticks_period() {
        assert_eq!(crate::session::attach::POLL_INTERVAL, CATCHUP_TICK);
        assert_eq!(crate::session::create::POLL_INTERVAL, CATCHUP_TICK);
    }

    /// Every arm the tick drives must be one `OnConnect` drives too —
    /// otherwise a reconnect would SKIP a replay the tick was covering.
    #[test]
    fn the_tick_arms_are_a_subset_of_the_on_connect_arms() {
        let on_connect = catchups_for(CatchupPass::OnConnect);
        for arm in catchups_for(CatchupPass::Tick) {
            assert!(on_connect.contains(arm), "{arm:?} missing from OnConnect");
        }
    }

    /// The tick arm's schedule, on a paused clock: the first tick is
    /// immediate (the caller consumes it because the on-connect catch-up
    /// just ran), the next does not fire before `CATCHUP_TICK`, and does
    /// fire at it.
    #[tokio::test(start_paused = true)]
    async fn catchup_tick_fires_immediately_once_then_every_catchup_tick() {
        let mut tick = catchup_interval();
        let start = tokio::time::Instant::now();

        // First tick: immediate.
        tick.tick().await;
        assert_eq!(tokio::time::Instant::now() - start, Duration::ZERO);

        // Not before the period.
        let early = tokio::time::timeout(CATCHUP_TICK - Duration::from_secs(1), tick.tick()).await;
        assert!(early.is_err(), "tick fired before CATCHUP_TICK");

        // At the period.
        tick.tick().await;
        assert_eq!(tokio::time::Instant::now() - start, CATCHUP_TICK);

        // And again one period later.
        tick.tick().await;
        assert_eq!(tokio::time::Instant::now() - start, CATCHUP_TICK * 2);
    }

    fn make_state(kind: &str) -> HandoffState {
        HandoffState {
            source_session_id: Uuid::nil(),
            tenant_id: Uuid::nil(),
            source_device_id: Uuid::nil(),
            session_kind: kind.to_string(),
            intent: json!({
                "kind": kind,
                "purpose": "fix the auth bug",
                "declared_paths": ["/repo/a", "/repo/b"],
                "share_output": true,
                "redact_secrets": false,
            }),
            repo: Some("qontinui-runner".to_string()),
            branch: Some("main".to_string()),
            held_claims: vec![HeldClaim {
                kind: "session".to_string(),
                resource_key: "session:t:m:s".to_string(),
            }],
            output_chunks: vec![OutputChunk {
                chunk_offset: 0,
                payload_b64: base64::engine::general_purpose::STANDARD.encode(b"$ ls\n"),
            }],
        }
    }

    #[test]
    fn build_child_intent_threads_cwd_and_purpose() {
        let state = make_state("terminal_shell");
        let intent = build_child_intent(&state, HANDOFF_CONTINUATION_NOTE).unwrap();
        assert_eq!(intent.kind, SessionKind::TerminalShell);
        assert_eq!(intent.repo.as_deref(), Some("qontinui-runner"));
        assert_eq!(intent.branch.as_deref(), Some("main"));
        assert_eq!(intent.declared_paths.len(), 2);
        assert!(intent.purpose.contains("fix the auth bug"));
        assert!(intent.purpose.contains("continued here"));
        assert!(intent.share_output);
        assert_eq!(intent.redact_secrets, Some(false));
        // Built intent must pass validation so start_with_parent accepts it.
        intent.validate().unwrap();
    }

    #[test]
    fn build_child_intent_defaults_sparse_source() {
        let state = HandoffState {
            source_session_id: Uuid::nil(),
            tenant_id: Uuid::nil(),
            source_device_id: Uuid::nil(),
            session_kind: "terminal_claude".to_string(),
            intent: json!({}),
            repo: None,
            branch: None,
            held_claims: vec![],
            output_chunks: vec![],
        };
        let intent = build_child_intent(&state, HANDOFF_CONTINUATION_NOTE).unwrap();
        assert_eq!(intent.kind, SessionKind::TerminalClaude);
        assert!(intent.purpose.contains("handoff session"));
        assert!(intent.declared_paths.is_empty());
        assert!(!intent.share_output);
        intent.validate().unwrap();
    }

    /// `make_state` with an extra key spliced into the source intent JSON,
    /// so the dual-read tests below differ ONLY in which slug key they carry.
    fn state_with_intent_key(key: &str, value: &str) -> HandoffState {
        let mut state = make_state("terminal_shell");
        state
            .intent
            .as_object_mut()
            .unwrap()
            .insert(key.to_string(), json!(value));
        state
    }

    #[test]
    fn build_child_intent_reads_legacy_plan_slug_key() {
        // Un-renamed coord (every coord shipping today) writes `plan_slug`.
        let state = state_with_intent_key("plan_slug", "2026-07-28-some-unit");
        let intent = build_child_intent(&state, HANDOFF_CONTINUATION_NOTE).unwrap();
        assert_eq!(
            intent.work_unit_slug.as_deref(),
            Some("2026-07-28-some-unit")
        );
        // …and the child re-emits it under the CANONICAL key alone: the
        // legacy blob is folded forward, never propagated.
        assert_eq!(
            intent.plan_slug, None,
            "the deprecated field is accept-only; the child must not re-emit it"
        );
        let json = serde_json::to_value(&intent).unwrap();
        assert_eq!(
            json.get("work_unit_slug").and_then(|v| v.as_str()),
            Some("2026-07-28-some-unit")
        );
        assert!(json.get("plan_slug").is_none(), "{json}");
    }

    #[test]
    fn build_child_intent_reads_new_work_unit_slug_key() {
        // Post-rename coord writes `work_unit_slug`.
        let state = state_with_intent_key("work_unit_slug", "2026-07-28-some-unit");
        let intent = build_child_intent(&state, HANDOFF_CONTINUATION_NOTE).unwrap();
        assert_eq!(
            intent.work_unit_slug.as_deref(),
            Some("2026-07-28-some-unit")
        );
    }

    #[test]
    fn build_child_intent_prefers_work_unit_slug_over_plan_slug() {
        // Both present (a coord mid-rename): the NEW key wins.
        let mut state = state_with_intent_key("plan_slug", "old-name");
        state
            .intent
            .as_object_mut()
            .unwrap()
            .insert("work_unit_slug".to_string(), json!("new-name"));
        let intent = build_child_intent(&state, HANDOFF_CONTINUATION_NOTE).unwrap();
        assert_eq!(intent.work_unit_slug.as_deref(), Some("new-name"));
    }

    #[test]
    fn build_child_intent_absent_slug_is_none() {
        let intent =
            build_child_intent(&make_state("terminal_shell"), HANDOFF_CONTINUATION_NOTE).unwrap();
        assert_eq!(intent.work_unit_slug, None);
    }

    #[test]
    fn build_child_intent_falls_back_when_new_key_is_explicitly_null() {
        // Present-but-null is NOT the same as absent: `.get()` yields
        // `Some(Value::Null)`, so a naive `.or_else(get).and_then(as_str)`
        // chain would see "new key present", get `None` from `as_str`, and
        // silently drop the slug. Coord emits this exact shape — its
        // autonomous-dispatch metadata uses
        // `json!({"plan_slug": slug, "work_unit_slug": slug})`, which writes
        // `null` for a `None` rather than omitting the key.
        let mut state = make_state("terminal_shell");
        let obj = state.intent.as_object_mut().unwrap();
        obj.insert("work_unit_slug".to_string(), serde_json::Value::Null);
        obj.insert("plan_slug".to_string(), json!("2026-07-28-some-unit"));
        let intent = build_child_intent(&state, HANDOFF_CONTINUATION_NOTE).unwrap();
        assert_eq!(
            intent.work_unit_slug.as_deref(),
            Some("2026-07-28-some-unit"),
            "an explicit null on the new key must fall back to the legacy key"
        );
    }

    #[test]
    fn build_child_intent_both_keys_null_is_none() {
        let mut state = make_state("terminal_shell");
        let obj = state.intent.as_object_mut().unwrap();
        obj.insert("work_unit_slug".to_string(), serde_json::Value::Null);
        obj.insert("plan_slug".to_string(), serde_json::Value::Null);
        let intent = build_child_intent(&state, HANDOFF_CONTINUATION_NOTE).unwrap();
        assert_eq!(intent.work_unit_slug, None);
    }

    #[test]
    fn build_child_intent_rejects_unknown_kind() {
        let state = make_state("nonsense_kind");
        let err = build_child_intent(&state, HANDOFF_CONTINUATION_NOTE).unwrap_err();
        assert!(matches!(err, HandoffError::Parse(_)));
    }

    #[test]
    fn pending_handoff_deserializes() {
        let v = json!({
            "source_session_id": Uuid::nil(),
            "target_device_id": Uuid::nil(),
            "tenant_id": Uuid::nil(),
            "session_kind": "agentic",
        });
        let p: PendingHandoff = serde_json::from_value(v).unwrap();
        assert_eq!(p.session_kind, "agentic");
    }

    #[test]
    fn handoff_list_response_defaults_empty() {
        // Coord may return only {count: 0} on an empty poll; the
        // default-empty serde attr keeps that from being a parse error.
        let v = json!({ "count": 0 });
        let r: HandoffListResponse = serde_json::from_value(v).unwrap();
        assert!(r.handoffs.is_empty());
    }

    #[test]
    fn output_chunk_round_trips_base64() {
        let encoded = base64::engine::general_purpose::STANDARD.encode(b"hello world");
        let v = json!({ "chunk_offset": 3, "payload_b64": encoded });
        let c: OutputChunk = serde_json::from_value(v).unwrap();
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(&c.payload_b64)
            .unwrap();
        assert_eq!(decoded, b"hello world");
    }

    // -----------------------------------------------------------------------
    // Push-transport tests (WebSocket-relay path — Phase 7 rework)
    // -----------------------------------------------------------------------

    /// Build a coord `/ws` envelope as the Redis pub/sub arm produces it:
    /// `{"channel": "<subject>", "payload": "<json-string>"}`.
    fn ws_envelope(channel: &str, payload: serde_json::Value) -> String {
        json!({
            "channel": channel,
            "payload": payload.to_string(),
        })
        .to_string()
    }

    fn handoff_payload(source: Uuid, target: Uuid, tenant: Uuid, kind: &str) -> serde_json::Value {
        json!({
            "event_kind": "handoff_request",
            "source_session_id": source,
            "target_device_id": target,
            "tenant_id": tenant,
            "session_kind": kind,
        })
    }

    #[test]
    fn coord_ws_url_swaps_scheme_and_subscribes_as_sessions() {
        assert_eq!(
            coord_ws_url("http://localhost:9870"),
            "ws://localhost:9870/ws?subscribe=sessions"
        );
        assert_eq!(
            coord_ws_url("https://coord.qontinui.io/"),
            "wss://coord.qontinui.io/ws?subscribe=sessions"
        );
    }

    #[test]
    fn parse_handoff_push_accepts_frame_for_this_device() {
        let device = Uuid::new_v4();
        let tenant = Uuid::new_v4();
        let source = Uuid::new_v4();
        let channel = format!("qontinui.sessions.{tenant}.{device}.handoff_request");
        let frame = ws_envelope(&channel, handoff_payload(source, device, tenant, "agentic"));

        let parsed = parse_handoff_push(&frame, device).expect("frame for this device parses");
        assert_eq!(parsed.source_session_id, source);
        assert_eq!(parsed.target_device_id, device);
        assert_eq!(parsed.tenant_id, tenant);
        assert_eq!(parsed.session_kind, "agentic");
    }

    #[test]
    fn parse_handoff_push_ignores_other_devices() {
        let device = Uuid::new_v4();
        let other = Uuid::new_v4();
        let tenant = Uuid::new_v4();
        let source = Uuid::new_v4();
        // Subject addressed to `other`, not us.
        let channel = format!("qontinui.sessions.{tenant}.{other}.handoff_request");
        let frame = ws_envelope(&channel, handoff_payload(source, other, tenant, "agentic"));
        assert!(parse_handoff_push(&frame, device).is_none());
    }

    #[test]
    fn parse_handoff_push_ignores_other_event_kinds() {
        let device = Uuid::new_v4();
        let tenant = Uuid::new_v4();
        // A `started`/`heartbeat` subject for this device must not trigger
        // a handoff materialization.
        let channel = format!("qontinui.sessions.{tenant}.{device}.started");
        let frame = ws_envelope(&channel, json!({"event_kind": "started"}));
        assert!(parse_handoff_push(&frame, device).is_none());
    }

    #[test]
    fn parse_handoff_push_ignores_non_session_subjects() {
        let device = Uuid::new_v4();
        // The broader `events.*` family `agent_runtime` consumes must not
        // be mistaken for a handoff even if it somehow reaches this socket.
        let channel = format!("events.agent.spawn_requested.{device}");
        let frame = ws_envelope(&channel, json!({"agent_id": Uuid::nil()}));
        assert!(parse_handoff_push(&frame, device).is_none());
    }

    #[test]
    fn parse_handoff_push_accepts_inlined_payload_object() {
        // Defensive: some envelopes inline the payload as an object rather
        // than a JSON string. The parser must handle both.
        let device = Uuid::new_v4();
        let tenant = Uuid::new_v4();
        let source = Uuid::new_v4();
        let channel = format!("qontinui.sessions.{tenant}.{device}.handoff_request");
        let envelope = json!({
            "channel": channel,
            "payload": handoff_payload(source, device, tenant, "workflow"),
        })
        .to_string();
        let parsed = parse_handoff_push(&envelope, device).expect("inlined payload parses");
        assert_eq!(parsed.source_session_id, source);
        assert_eq!(parsed.session_kind, "workflow");
    }

    #[test]
    fn parse_handoff_push_rejects_payload_target_mismatch() {
        // Channel says us, but the payload's target_device_id disagrees —
        // trust the address (channel), reject the frame.
        let device = Uuid::new_v4();
        let other = Uuid::new_v4();
        let tenant = Uuid::new_v4();
        let source = Uuid::new_v4();
        let channel = format!("qontinui.sessions.{tenant}.{device}.handoff_request");
        let frame = ws_envelope(&channel, handoff_payload(source, other, tenant, "agentic"));
        assert!(parse_handoff_push(&frame, device).is_none());
    }

    #[test]
    fn parse_handoff_push_ignores_garbage() {
        let device = Uuid::new_v4();
        assert!(parse_handoff_push("not json", device).is_none());
        assert!(parse_handoff_push("{}", device).is_none());
    }

    // -----------------------------------------------------------------------
    // Phase 4 — restore-registry materialization (plan
    // `2026-07-09-runner-session-history-cloud-sync` §3.4)
    // -----------------------------------------------------------------------

    fn restore_payload(tier: &str, authoritative: Option<&str>) -> serde_json::Value {
        json!({
            "provider": "claude",
            "authoritative_session_id": authoritative,
            "cwd": "C:/repo",
            "launch_command": match authoritative {
                Some(id) => format!("claude --resume {id}"),
                None => "claude".to_string(),
            },
            "restore_tier": tier,
            "machine_id": Uuid::new_v4(),
        })
    }

    /// A `full`-tier payload materializes an AUTHORITATIVE + CONFIRMED
    /// record keyed by the authoritative id — exactly the shape the
    /// existing restore classifier auto-resumes.
    #[test]
    fn restore_payload_full_tier_materializes_confirmed_authoritative_record() {
        let rec = registry_record_from_restore_payload(
            &restore_payload(TIER_FULL, Some("11111111-2222-3333-4444-555555555555")),
            "term-child",
            Uuid::new_v4(),
        );
        assert_eq!(
            rec.claude_session_id,
            "11111111-2222-3333-4444-555555555555"
        );
        assert_eq!(rec.origin.as_deref(), Some(ORIGIN_AUTHORITATIVE));
        assert!(
            rec.confirmed_at.is_some(),
            "full tier ⇒ confirmed ⇒ classifyRestoreAction auto-resumes"
        );
        assert_eq!(rec.provider, "claude");
        assert_eq!(rec.working_dir.as_deref(), Some("C:/repo"));
        assert_eq!(rec.terminal_id, "term-child");
        assert_eq!(rec.state, "open");
    }

    /// A `terminal_only` payload materializes a PROVISIONAL authoritative
    /// record under a deterministic per-source key — the classifier's
    /// phantom-shell branch restores terminal+cwd with an honest fresh
    /// conversation, never typing a resume against a null id, and a
    /// materialization retry upserts the SAME record (F7 idempotency).
    #[test]
    fn restore_payload_terminal_only_materializes_provisional_record() {
        let source = Uuid::new_v4();
        let rec = registry_record_from_restore_payload(
            &restore_payload("terminal_only", None),
            "term-child",
            source,
        );
        assert!(
            Uuid::parse_str(&rec.claude_session_id).is_ok(),
            "terminal_only key is a real uuid, got {}",
            rec.claude_session_id
        );
        assert_eq!(rec.origin.as_deref(), Some(ORIGIN_AUTHORITATIVE));
        assert!(
            rec.confirmed_at.is_none(),
            "terminal_only ⇒ provisional ⇒ classifyRestoreAction restores terminal-only"
        );
        assert_eq!(rec.working_dir.as_deref(), Some("C:/repo"));

        // Deterministic: same source ⇒ same key (retry idempotency);
        // different source ⇒ different key (no cross-session collision).
        let retry = registry_record_from_restore_payload(
            &restore_payload("terminal_only", None),
            "term-child",
            source,
        );
        assert_eq!(rec.claude_session_id, retry.claude_session_id);
        let other = registry_record_from_restore_payload(
            &restore_payload("terminal_only", None),
            "term-child",
            Uuid::new_v4(),
        );
        assert_ne!(rec.claude_session_id, other.claude_session_id);
    }

    /// Honest degrade: a `full` claim WITHOUT an authoritative id cannot
    /// resume anything — it materializes as terminal-only (provisional,
    /// minted key), never a confirmed record with a fabricated id.
    #[test]
    fn restore_payload_full_without_id_degrades_to_terminal_only() {
        let rec = registry_record_from_restore_payload(
            &restore_payload(TIER_FULL, None),
            "term-child",
            Uuid::new_v4(),
        );
        assert!(Uuid::parse_str(&rec.claude_session_id).is_ok());
        assert!(rec.confirmed_at.is_none());
    }

    /// Missing/blank provider defaults to claude (the pre-provider-aware
    /// registry default), so a sparse mirror still restores.
    #[test]
    fn restore_payload_defaults_provider() {
        let rec = registry_record_from_restore_payload(
            &json!({"restore_tier": "terminal_only"}),
            "term-child",
            Uuid::new_v4(),
        );
        assert_eq!(rec.provider, DEFAULT_PROVIDER);
        assert!(rec.working_dir.is_none());
    }

    /// End-to-end into the real registry: `record_open` on the mapped
    /// record lands a RESTORABLE row with the tier-honest fields intact.
    #[test]
    fn materialized_record_feeds_the_existing_registry_restorably() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionLifecycleStore::open(dir.path().join("terminal-sessions.json")).unwrap();
        let rec = registry_record_from_restore_payload(
            &restore_payload(TIER_FULL, Some("sess-full-1")),
            "term-child",
            Uuid::new_v4(),
        );
        store.record_open(rec);

        let restorable =
            store.restorable_records(chrono::Utc::now().timestamp_millis(), None, true);
        assert_eq!(restorable.len(), 1, "materialized record is restorable");
        let r = &restorable[0];
        assert_eq!(r.claude_session_id, "sess-full-1");
        assert_eq!(r.origin.as_deref(), Some(ORIGIN_AUTHORITATIVE));
        assert!(r.confirmed_at.is_some());
        assert!(r.opened_at > 0, "record_open stamped real timestamps");
    }

    /// The SSE-buffer parser picks the NEWEST (highest-seq) restore-record
    /// row out of the replay window, ignoring other event kinds, non-JSON
    /// noise, and payload-less rows.
    #[test]
    fn latest_restore_record_from_sse_picks_newest_and_ignores_noise() {
        let old = json!({
            "id": 1, "session_id": Uuid::nil(), "seq": 3,
            "event_kind": "restore-record",
            "payload": {"restore_tier": "terminal_only", "provider": "claude"},
        });
        let newest = json!({
            "id": 2, "session_id": Uuid::nil(), "seq": 7,
            "event_kind": "restore-record",
            "payload": {"restore_tier": "full", "provider": "claude",
                         "authoritative_session_id": "sess-9"},
        });
        let other_kind = json!({
            "id": 3, "session_id": Uuid::nil(), "seq": 9,
            "event_kind": "handoff_request",
            "payload": {"target_device_id": Uuid::nil()},
        });
        let buf = format!(
            "event: replay\ndata: {old}\n\nevent: replay\ndata: {newest}\n\n\
             event: replay\ndata: {other_kind}\n\nevent: live\ndata: not json\n\n"
        );
        let payload = latest_restore_record_from_sse(&buf).expect("newest restore-record");
        assert_eq!(payload["restore_tier"], "full");
        assert_eq!(payload["authoritative_session_id"], "sess-9");

        // Empty / noise-only buffers yield nothing.
        assert!(latest_restore_record_from_sse("").is_none());
        assert!(latest_restore_record_from_sse("event: replay\ndata: {}\n\n").is_none());
    }
}

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
//!    scoped to `?pattern=qontinui.sessions.*` and filters inbound
//!    envelopes for `…<self-device>.handoff_request`. Server-side fan-out
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
//! Step 3 happening before step 6 is deliberate: the source is only torn
//! down once the child exists, so a failed materialization leaves the
//! source intact and the next push/catch-up retries. Idempotency: coord's
//! `get_handoff_requests` filters out any source that already has a
//! materialized child on this device, so a push + catch-up double-delivery
//! never materializes twice.

use std::sync::Arc;
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
use super::{SessionKind, SessionRegistry};

/// Reconnect backoff floor for the push-subscriber WS loop. Matches the
/// `agent_runtime.rs` reconnect posture (2s → 60s capped).
const RECONNECT_BACKOFF_FLOOR: Duration = Duration::from_secs(2);
/// Reconnect backoff ceiling.
const RECONNECT_BACKOFF_CEIL: Duration = Duration::from_secs(60);

/// Coord `/ws` Redis-pub/sub glob the receiver subscribes to. Scopes the
/// fan-out to session subjects only (NOT the broader `events.*` family
/// `agent_runtime.rs` consumes), so coord's PSUBSCRIBE doesn't relay every
/// build/claim event to this socket. The handoff payload lands on
/// `qontinui.sessions.<tenant>.<target-machine>.handoff_request`.
const SESSION_WS_PATTERN: &str = "qontinui.sessions.*";

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
pub async fn trigger_handoff(
    http: &reqwest::Client,
    coord_url: &str,
    source_session_id: Uuid,
    target_device_id: Uuid,
) -> Result<(), HandoffError> {
    let url = format!(
        "{}/sessions/{}/handoff",
        coord_url.trim_end_matches('/'),
        source_session_id
    );
    let resp = http
        .post(&url)
        .json(&TriggerBody { target_device_id })
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
/// The task is **push-driven**: it opens a coord `/ws` subscription scoped
/// to `qontinui.sessions.*` and materializes each `handoff_request`
/// addressed to this device the instant coord fans it out. On every
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

/// Derive coord's `/ws` URL (with the session pattern) from the resolved
/// coord HTTP base in `CoordSync`. The runner's `CoordSync` stores the
/// coord base in HTTP(S) form; the WS upgrade endpoint is the same host
/// with the scheme swapped and `/ws` appended.
fn coord_ws_url(coord_http_base: &str) -> String {
    let base = coord_http_base.trim_end_matches('/');
    let ws_base = base
        .strip_prefix("https://")
        .map(|rest| format!("wss://{rest}"))
        .or_else(|| {
            base.strip_prefix("http://")
                .map(|rest| format!("ws://{rest}"))
        })
        .unwrap_or_else(|| base.to_string());
    format!(
        "{ws_base}/ws?pattern={}",
        urlencode_pattern(SESSION_WS_PATTERN)
    )
}

/// Minimal percent-encoding for the one glob char (`*`) we put in the
/// query string. Avoids pulling a urlencoding dep for a single literal.
fn urlencode_pattern(pattern: &str) -> String {
    pattern.replace('*', "%2A")
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

    let mut backoff = RECONNECT_BACKOFF_FLOOR;
    loop {
        match connect_and_pump(
            &registry,
            &lifecycle_store,
            &http,
            &coord_url,
            &ws_url,
            device_id,
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
) -> Result<(), HandoffError> {
    let (mut ws, _resp) = tokio_tungstenite::connect_async(ws_url)
        .await
        .map_err(|e| HandoffError::Http(format!("connect coord /ws {ws_url}: {e}")))?;

    tracing::info!(device = %device_id, "session handoff: push WS connected");

    // On-connect catch-up: replay anything that landed while we were
    // offline. The durable `handoff_request` event row in coord is the
    // source of truth; this GET drains it. Best-effort — a failure here
    // doesn't abort the pump (the push path still works, and the next
    // reconnect retries the catch-up).
    run_catchup(registry, lifecycle_store, http, coord_url, device_id).await;

    while let Some(msg) = ws.next().await {
        let msg = msg.map_err(|e| HandoffError::Http(format!("coord /ws recv: {e}")))?;
        match msg {
            tokio_tungstenite::tungstenite::Message::Text(t) => {
                handle_push_frame(
                    registry,
                    lifecycle_store,
                    http,
                    coord_url,
                    device_id,
                    t.as_str(),
                )
                .await;
            }
            tokio_tungstenite::tungstenite::Message::Binary(b) => {
                let s = String::from_utf8_lossy(&b);
                handle_push_frame(registry, lifecycle_store, http, coord_url, device_id, &s).await;
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
    Ok(())
}

/// Run the one-shot catch-up: GET the durable pending list and materialize
/// each. Used on every (re)connect.
async fn run_catchup(
    registry: &Arc<SessionRegistry>,
    lifecycle_store: &Arc<SessionLifecycleStore>,
    http: &reqwest::Client,
    coord_url: &str,
    device_id: Uuid,
) {
    match fetch_pending(http, coord_url, device_id).await {
        Ok(pending) => {
            if !pending.is_empty() {
                tracing::info!(
                    count = pending.len(),
                    "session handoff: on-connect catch-up replaying pending handoffs"
                );
            }
            for handoff in pending {
                materialize_logged(registry, lifecycle_store, http, coord_url, &handoff).await;
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
    text: &str,
) {
    let Some(handoff) = parse_handoff_push(text, device_id) else {
        return;
    };
    tracing::info!(
        source = %handoff.source_session_id,
        "session handoff: push received; materializing"
    );
    materialize_logged(registry, lifecycle_store, http, coord_url, &handoff).await;
}

/// Pure parse+filter of a coord `/ws` envelope into a [`PendingHandoff`]
/// addressed to `device_id`. Returns `None` when the frame isn't a
/// handoff for this device (so the pump can ignore it). Factored out so
/// the unit tests can exercise the channel-matching + payload-decode
/// without a live WS.
fn parse_handoff_push(text: &str, device_id: Uuid) -> Option<PendingHandoff> {
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
async fn materialize_logged(
    registry: &Arc<SessionRegistry>,
    lifecycle_store: &Arc<SessionLifecycleStore>,
    http: &reqwest::Client,
    coord_url: &str,
    handoff: &PendingHandoff,
) {
    if let Err(e) = materialize(registry, lifecycle_store, http, coord_url, handoff).await {
        tracing::warn!(
            source = %handoff.source_session_id,
            error = %e,
            "session handoff: materialize failed; source left intact, will retry on next push/catch-up"
        );
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
async fn fetch_state(
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
    handoff: &PendingHandoff,
) -> Result<(), HandoffError> {
    let state = fetch_state(http, coord_url, handoff.source_session_id).await?;

    let intent = build_child_intent(&state)?;

    // Start the child session locally with lineage back to the source.
    let child = registry
        .start_with_parent(intent, handoff.source_session_id)
        .map_err(|e| HandoffError::Session(e.to_string()))?;
    let child_id = child.id();

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
        if let Err(e) = reacquire_claim(http, coord_url, claim, device_id).await {
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
    }
}

/// Build the child session [`Intent`] from the source state. cwd comes
/// from `repo` (the PTY transport uses `intent.repo` as the working
/// dir); `declared_paths` + branch carry over verbatim. The purpose is
/// annotated so the dashboard shows the lineage at a glance.
fn build_child_intent(state: &HandoffState) -> Result<Intent, HandoffError> {
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
    let purpose = format!("{source_purpose} (continued here)");
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
    let plan_slug = src
        .get("plan_slug")
        .and_then(|v| v.as_str())
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
        plan_slug,
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
async fn reacquire_claim(
    http: &reqwest::Client,
    coord_url: &str,
    claim: &HeldClaim,
    device_id: Uuid,
) -> Result<(), HandoffError> {
    let url = format!("{}/claims/acquire", coord_url.trim_end_matches('/'));
    let body = json!({
        "kind": claim.kind,
        "resource_key": claim.resource_key,
        "machine_id": device_id.to_string(),
    });
    let resp = http
        .post(&url)
        .json(&body)
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
    let resp = http
        .delete(&url)
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
        let intent = build_child_intent(&state).unwrap();
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
        let intent = build_child_intent(&state).unwrap();
        assert_eq!(intent.kind, SessionKind::TerminalClaude);
        assert!(intent.purpose.contains("handoff session"));
        assert!(intent.declared_paths.is_empty());
        assert!(!intent.share_output);
        intent.validate().unwrap();
    }

    #[test]
    fn build_child_intent_rejects_unknown_kind() {
        let state = make_state("nonsense_kind");
        let err = build_child_intent(&state).unwrap_err();
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
    fn coord_ws_url_swaps_scheme_and_appends_pattern() {
        assert_eq!(
            coord_ws_url("http://localhost:9870"),
            "ws://localhost:9870/ws?pattern=qontinui.sessions.%2A"
        );
        assert_eq!(
            coord_ws_url("https://coord.qontinui.io/"),
            "wss://coord.qontinui.io/ws?pattern=qontinui.sessions.%2A"
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

        let restorable = store.restorable_records(chrono::Utc::now().timestamp_millis(), None);
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

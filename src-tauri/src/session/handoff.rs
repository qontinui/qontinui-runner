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
pub fn start_receiver_task(registry: Arc<SessionRegistry>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(run_receiver_loop(registry))
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
async fn run_receiver_loop(registry: Arc<SessionRegistry>) {
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
        match connect_and_pump(&registry, &http, &coord_url, &ws_url, device_id).await {
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
    run_catchup(registry, http, coord_url, device_id).await;

    while let Some(msg) = ws.next().await {
        let msg = msg.map_err(|e| HandoffError::Http(format!("coord /ws recv: {e}")))?;
        match msg {
            tokio_tungstenite::tungstenite::Message::Text(t) => {
                handle_push_frame(registry, http, coord_url, device_id, t.as_str()).await;
            }
            tokio_tungstenite::tungstenite::Message::Binary(b) => {
                let s = String::from_utf8_lossy(&b);
                handle_push_frame(registry, http, coord_url, device_id, &s).await;
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
                materialize_logged(registry, http, coord_url, &handoff).await;
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
    materialize_logged(registry, http, coord_url, &handoff).await;
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
    http: &reqwest::Client,
    coord_url: &str,
    handoff: &PendingHandoff,
) {
    if let Err(e) = materialize(registry, http, coord_url, handoff).await {
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

    // Tear down the source — one-way move. coord's DELETE sets
    // state='closed', closed_at=now(), and releases the source claim.
    close_source(http, coord_url, handoff.source_session_id).await?;

    Ok(())
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
}

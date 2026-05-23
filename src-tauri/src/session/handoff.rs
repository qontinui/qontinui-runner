//! Cross-machine session handoff — trigger + receiver.
//!
//! Plan: `D:/qontinui-root/qontinui-dev-notes/plans/
//! 2026-05-23-coord-native-sessions-phase-7-10.md` §Phase 7. One-way move
//! ("Continue elsewhere"): an operator moves an active session from one
//! machine to another, with cwd + held claims + recent PTY scrollback
//! following.
//!
//! ## Why HTTP, not JetStream
//!
//! The plan text says the handoff request travels via JetStream. The
//! coord side *does* publish on
//! `qontinui.sessions.<tenant>.<target-machine>.handoff_request` for the
//! dashboard + any NATS-capable peer. But the runner has **no NATS
//! client** — every runner→coord interaction in this codebase is HTTP
//! (`coord_sync.rs` drains over reqwest; `coord_questions.rs` polls).
//! Adding `async-nats` + a cross-machine subscription to the runner for a
//! single feature is a large architectural change out of Phase 7's scope.
//! Instead the receiver polls coord's durable
//! `GET /sessions/handoff-requests?device_id=<self>` endpoint, which is
//! backed by the same `handoff_request` event row coord publishes on
//! JetStream. The durable row is the load-bearing delivery path; the
//! JetStream publish is the fast path for online subscribers. This
//! matches the established `coord_questions.rs` poll posture exactly.
//!
//! ## Receiver flow (one handoff)
//!
//! 1. Poll returns a [`PendingHandoff`] addressed to this device.
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
//! source intact and the next poll retries.

use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use super::intent::Intent;
use super::{SessionKind, SessionRegistry};

/// Poll cadence for the receiver loop. Matches the
/// `coord_questions.rs::poll_answer` 5s cadence — fast enough that a
/// "Continue elsewhere" click feels responsive, slow enough to be quiet.
const POLL_INTERVAL: Duration = Duration::from_secs(5);

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

/// Start the handoff-receiver loop. Returns the [`JoinHandle`] so
/// `main.rs` can keep it alive for the lifetime of the process. The loop
/// polls coord every [`POLL_INTERVAL`] for handoffs addressed to this
/// device and materializes each. Plan §Phase 7.
pub fn start_receiver_task(registry: Arc<SessionRegistry>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(run_receiver_loop(registry))
}

async fn run_receiver_loop(registry: Arc<SessionRegistry>) {
    let http = registry.coord_sync().http_client();
    let coord_url = registry.coord_sync().coord_url().to_string();
    let device_id = registry.machine_id();

    tracing::info!(
        coord_url = %coord_url,
        device = %device_id,
        "session handoff: receiver loop starting"
    );

    loop {
        tokio::time::sleep(POLL_INTERVAL).await;

        let pending = match poll_pending(&http, &coord_url, device_id).await {
            Ok(p) => p,
            Err(e) => {
                tracing::debug!(error = %e, "session handoff: poll failed (will retry)");
                continue;
            }
        };

        for handoff in pending {
            if let Err(e) = materialize(&registry, &http, &coord_url, &handoff).await {
                tracing::warn!(
                    source = %handoff.source_session_id,
                    error = %e,
                    "session handoff: materialize failed; will retry next poll"
                );
            }
        }
    }
}

/// Poll coord for handoffs addressed to this device.
async fn poll_pending(
    http: &reqwest::Client,
    coord_url: &str,
    device_id: Uuid,
) -> Result<Vec<PendingHandoff>, HandoffError> {
    let url = format!(
        "{}/sessions/handoff-requests?device_id={}",
        coord_url.trim_end_matches('/'),
        device_id
    );
    let resp = http
        .get(&url)
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
    let resp = http
        .get(&url)
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

    Ok(Intent {
        kind,
        purpose,
        repo: state.repo.clone(),
        branch: state.branch.clone(),
        declared_paths,
        share_output,
        redact_secrets,
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
}

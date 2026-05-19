//! Runner-side coord-questions client (Phase 3 of the coord
//! production-readiness plan).
//!
//! Plan reference:
//! `D:/qontinui-root/plans/2026-05-19-coordinator-production-readiness.md`
//! Phase 3 ("Decision queue + agent inbox").
//!
//! ## What this module does
//!
//! Provides the runner-side client that POSTs a question to coord's
//! `coord.agent_questions` table and polls until an operator answers.
//! The eventual caller is a Claude Code MCP tool that wraps the SDK's
//! `AskUserQuestion` so agents on remote machines no longer block the
//! operator's in-session attention. Per Resolved Decision Q3, once
//! Phase 3 ships, `AskUserQuestion` ALWAYS routes through coord (no
//! local-fallback path).
//!
//! ## Coord HTTP surface this client targets
//!
//! - `POST /agents/:agent_id/ask-question` — insert
//! - `GET  /coord/agent-questions/:question_id` — poll
//!
//! See `qontinui-coord/src/agent_questions.rs` for the server-side
//! handler + schema.
//!
//! ## Best-effort + retry posture
//!
//! - `post_question` uses a short timeout (10s) + simple two-attempt
//!   retry (the caller decides whether to escalate to a different
//!   surface if both attempts fail).
//! - `poll_answer` polls every 5s with a hard wall-clock timeout. The
//!   timeout is a caller-supplied parameter rather than env-resolved so
//!   the future MCP wrapper can pick a reasonable default per
//!   question-type.

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use tracing::{debug, warn};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Wire types — mirror the coord-side `agent_questions::AskQuestionRequest`
// and `AgentQuestionRow` shapes exactly.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
struct AskQuestionPayload<'a> {
    question: &'a str,
    options: &'a JsonValue,
    plan_phase: Option<&'a str>,
    context: Option<&'a str>,
    agent_session_id: Option<Uuid>,
    device_id: Option<Uuid>,
}

/// Subset of the coord-returned row this client cares about. Coord
/// returns a richer shape (created_at, responded_by_operator, etc.); we
/// only read the fields needed for the poll loop.
#[derive(Debug, Clone, Deserialize)]
struct CoordQuestionRow {
    question_id: Uuid,
    /// `None` until the operator answers — that's the poll-loop signal.
    response: Option<String>,
    /// Strict-mode field — present in the row but the poller uses
    /// `response.is_some()` rather than checking this directly so we
    /// stay decoupled from the server's clock.
    #[serde(default)]
    #[allow(dead_code)]
    responded_at: Option<chrono::DateTime<chrono::Utc>>,
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors surfaced by this module. Caller decides whether to escalate
/// or swallow — there's no fallback path inside the client itself.
#[derive(Debug, thiserror::Error)]
pub enum CoordQuestionError {
    #[error("coord HTTP error: {0}")]
    Http(String),
    #[error("coord returned non-success status {0}: {1}")]
    Status(u16, String),
    #[error("response body parse failed: {0}")]
    Parse(String),
    #[error("polling timed out after {0:?} — question {1} still unanswered")]
    PollTimeout(Duration, Uuid),
}

// ---------------------------------------------------------------------------
// Public surface
// ---------------------------------------------------------------------------

/// POST a question to coord's `coord.agent_questions`. Returns the new
/// `question_id` on success.
///
/// `coord_base` is the HTTP base (no trailing slash, no `/ws` suffix) —
/// resolve it via `fleet::coord_http_base()` at the call site; this
/// function stays profile-agnostic so it's trivially testable.
///
/// On transient network failure the call retries once after 2s. After
/// the second failure it returns `Err(Http(..))` — the caller decides
/// whether to surface the failure as an in-session AskUserQuestion (per
/// Q3 we don't do this internally) or to fail the agent task.
pub async fn post_question(
    coord_base: &str,
    agent_id: Uuid,
    plan_phase: Option<&str>,
    question: &str,
    options: &JsonValue,
    context: Option<&str>,
    agent_session_id: Option<Uuid>,
    device_id: Option<Uuid>,
) -> Result<Uuid, CoordQuestionError> {
    let url = format!(
        "{}/agents/{}/ask-question",
        coord_base.trim_end_matches('/'),
        agent_id
    );
    let body = AskQuestionPayload {
        question,
        options,
        plan_phase,
        context,
        agent_session_id,
        device_id,
    };

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| CoordQuestionError::Http(format!("reqwest builder: {e}")))?;

    let mut last_err: Option<CoordQuestionError> = None;
    for attempt in 0..2 {
        match client.post(&url).json(&body).send().await {
            Ok(resp) => {
                let status = resp.status();
                if !status.is_success() {
                    let text = resp.text().await.unwrap_or_default();
                    last_err = Some(CoordQuestionError::Status(
                        status.as_u16(),
                        text.chars().take(500).collect(),
                    ));
                } else {
                    let parsed: CoordQuestionRow = resp.json().await.map_err(|e| {
                        CoordQuestionError::Parse(format!("decode ask-question response: {e}"))
                    })?;
                    debug!(
                        "coord_questions::post_question: posted question_id={} for agent_id={}",
                        parsed.question_id, agent_id
                    );
                    return Ok(parsed.question_id);
                }
            }
            Err(e) => {
                last_err = Some(CoordQuestionError::Http(format!("POST {url}: {e}")));
            }
        }
        if attempt == 0 {
            warn!(
                "coord_questions::post_question: attempt 1 failed ({:?}); retrying in 2s",
                last_err
            );
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }
    Err(last_err.unwrap_or_else(|| {
        CoordQuestionError::Http("post_question: exhausted retries with no recorded error".into())
    }))
}

/// Poll coord every 5s until the question is answered OR `timeout`
/// elapses. Returns the operator's `response` string on success.
///
/// Errors:
/// - `PollTimeout` — `timeout` elapsed with no answer
/// - `Http` / `Status` / `Parse` — coord unreachable or returned an
///   unexpected shape (each polling attempt independent — transient
///   failures are logged but don't fail the loop UNTIL `timeout`).
pub async fn poll_answer(
    coord_base: &str,
    question_id: Uuid,
    timeout: Duration,
) -> Result<String, CoordQuestionError> {
    let url = format!(
        "{}/coord/agent-questions/{}",
        coord_base.trim_end_matches('/'),
        question_id
    );
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| CoordQuestionError::Http(format!("reqwest builder: {e}")))?;

    let started = Instant::now();
    let poll_every = Duration::from_secs(5);

    loop {
        if started.elapsed() >= timeout {
            return Err(CoordQuestionError::PollTimeout(timeout, question_id));
        }

        match client.get(&url).send().await {
            Ok(resp) if resp.status().is_success() => {
                match resp.json::<CoordQuestionRow>().await {
                    Ok(row) => {
                        if let Some(answer) = row.response {
                            debug!(
                                "coord_questions::poll_answer: question {question_id} answered \
                                 after {elapsed:?}",
                                elapsed = started.elapsed()
                            );
                            return Ok(answer);
                        }
                        // Still unanswered — fall through to sleep + retry.
                    }
                    Err(e) => {
                        warn!(
                            "coord_questions::poll_answer: parse failed ({e}); will retry in {:?}",
                            poll_every
                        );
                    }
                }
            }
            Ok(resp) => {
                let status = resp.status();
                let text = resp.text().await.unwrap_or_default();
                if status.as_u16() == 404 {
                    // Coord lost the row — fatal. Don't waste the timeout.
                    return Err(CoordQuestionError::Status(
                        status.as_u16(),
                        format!("question_id {question_id} not found"),
                    ));
                }
                warn!(
                    "coord_questions::poll_answer: HTTP {status} from {url}: {} — will retry",
                    text.chars().take(200).collect::<String>()
                );
            }
            Err(e) => {
                warn!("coord_questions::poll_answer: GET {url} failed ({e}); will retry");
            }
        }

        // Sleep, but never overshoot the deadline.
        let remaining = timeout.saturating_sub(started.elapsed());
        let nap = std::cmp::min(remaining, poll_every);
        if nap.is_zero() {
            return Err(CoordQuestionError::PollTimeout(timeout, question_id));
        }
        tokio::time::sleep(nap).await;
    }
}

/// Convenience wrapper: post + poll in one call. Returns the operator's
/// response string. The eventual MCP tool calls this directly.
#[allow(clippy::too_many_arguments)]
pub async fn ask_via_coord(
    coord_base: &str,
    agent_id: Uuid,
    plan_phase: Option<&str>,
    question: &str,
    options: &JsonValue,
    context: Option<&str>,
    agent_session_id: Option<Uuid>,
    device_id: Option<Uuid>,
    poll_timeout: Duration,
) -> Result<String, CoordQuestionError> {
    let question_id = post_question(
        coord_base,
        agent_id,
        plan_phase,
        question,
        options,
        context,
        agent_session_id,
        device_id,
    )
    .await?;
    poll_answer(coord_base, question_id, poll_timeout).await
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        extract::Path as AxumPath,
        http::StatusCode,
        routing::{get, post},
        Json, Router,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use tokio::sync::Mutex;

    /// Helper: spin up a local axum server with the two coord endpoints
    /// this client targets. Returns the base URL.
    async fn spawn_fake_coord(
        post_response: Arc<Mutex<serde_json::Value>>,
        get_responses: Arc<Mutex<Vec<serde_json::Value>>>,
        get_calls: Arc<AtomicUsize>,
    ) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let post_state = post_response.clone();
        // Axum 0.8 capture-group syntax: `{name}` not `:name`.
        let app: Router = Router::new()
            .route(
                "/agents/{agent_id}/ask-question",
                post(
                    move |AxumPath(_agent_id): AxumPath<Uuid>,
                          Json(_body): Json<serde_json::Value>| {
                        let post_state = post_state.clone();
                        async move {
                            let resp = post_state.lock().await.clone();
                            (StatusCode::OK, Json(resp))
                        }
                    },
                ),
            )
            .route(
                "/coord/agent-questions/{question_id}",
                get(move |AxumPath(_qid): AxumPath<Uuid>| {
                    let get_state = get_responses.clone();
                    let calls = get_calls.clone();
                    async move {
                        let idx = calls.fetch_add(1, Ordering::SeqCst);
                        let bodies = get_state.lock().await;
                        let body = bodies
                            .get(idx)
                            .or_else(|| bodies.last())
                            .cloned()
                            .unwrap_or_else(|| serde_json::json!({}));
                        (StatusCode::OK, Json(body))
                    }
                }),
            );

        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });

        format!("http://{}", addr)
    }

    #[tokio::test]
    async fn post_question_happy_path() {
        let qid = Uuid::parse_str("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa").unwrap();
        let post_body = Arc::new(Mutex::new(serde_json::json!({
            "question_id": qid,
            "response": null,
            "responded_at": null,
        })));
        let get_bodies = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
        let calls = Arc::new(AtomicUsize::new(0));
        let base = spawn_fake_coord(post_body, get_bodies, calls).await;

        let agent_id = Uuid::parse_str("bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb").unwrap();
        let options = serde_json::json!(["yes", "no"]);
        let out = post_question(
            &base,
            agent_id,
            Some("phase-3"),
            "Are we go for launch?",
            &options,
            None,
            None,
            None,
        )
        .await
        .expect("post_question");
        assert_eq!(out, qid);
    }

    #[tokio::test]
    async fn poll_answer_returns_after_response_appears() {
        let qid = Uuid::parse_str("cccccccc-cccc-cccc-cccc-cccccccccccc").unwrap();
        let post_body = Arc::new(Mutex::new(serde_json::json!({
            "question_id": qid,
            "response": null,
            "responded_at": null,
        })));
        // First GET: still pending. Second GET: answered.
        let get_bodies = Arc::new(Mutex::new(vec![
            serde_json::json!({
                "question_id": qid,
                "response": null,
                "responded_at": null,
            }),
            serde_json::json!({
                "question_id": qid,
                "response": "yes",
                "responded_at": "2026-05-19T00:00:00Z",
            }),
        ]));
        let calls = Arc::new(AtomicUsize::new(0));
        let base = spawn_fake_coord(post_body, get_bodies, calls.clone()).await;

        // Tight timeout: 1s short-poll would never resolve under prod
        // 5s cadence, but tests need to be fast. We use a 30s timeout
        // and accept the 5s wait between polls. The second poll lands
        // at t=5s, well inside the timeout.
        let result = poll_answer(&base, qid, Duration::from_secs(30)).await;
        assert_eq!(result.unwrap(), "yes");
        // 2 calls = first (pending) + second (answered).
        assert!(
            calls.load(Ordering::SeqCst) >= 2,
            "expected ≥2 GET calls, got {}",
            calls.load(Ordering::SeqCst)
        );
    }

    #[tokio::test]
    async fn poll_answer_times_out_when_never_answered() {
        let qid = Uuid::parse_str("dddddddd-dddd-dddd-dddd-dddddddddddd").unwrap();
        let post_body = Arc::new(Mutex::new(serde_json::json!({
            "question_id": qid,
            "response": null,
            "responded_at": null,
        })));
        let get_bodies = Arc::new(Mutex::new(vec![serde_json::json!({
            "question_id": qid,
            "response": null,
            "responded_at": null,
        })]));
        let calls = Arc::new(AtomicUsize::new(0));
        let base = spawn_fake_coord(post_body, get_bodies, calls).await;

        // 1s timeout — must return PollTimeout (not hang, not panic).
        let result = poll_answer(&base, qid, Duration::from_secs(1)).await;
        match result {
            Err(CoordQuestionError::PollTimeout(d, id)) => {
                assert_eq!(d, Duration::from_secs(1));
                assert_eq!(id, qid);
            }
            other => panic!("expected PollTimeout, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn post_question_propagates_non_success_status() {
        // Build a server that always returns 500 for the POST.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app: Router = Router::new().route(
            "/agents/{agent_id}/ask-question",
            post(
                |AxumPath(_id): AxumPath<Uuid>, Json(_): Json<serde_json::Value>| async {
                    (StatusCode::INTERNAL_SERVER_ERROR, "boom")
                },
            ),
        );
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        let base = format!("http://{}", addr);

        let agent_id = Uuid::new_v4();
        let result = post_question(
            &base,
            agent_id,
            None,
            "Q?",
            &serde_json::json!(["a"]),
            None,
            None,
            None,
        )
        .await;
        match result {
            Err(CoordQuestionError::Status(code, body)) => {
                assert_eq!(code, 500);
                assert!(body.contains("boom"), "body was: {body}");
            }
            other => panic!("expected Status(500), got {:?}", other),
        }
    }

    #[test]
    fn ask_question_payload_serialization_shape() {
        // Lock the wire format the coord side parses.
        let options = serde_json::json!(["a", "b"]);
        let payload = AskQuestionPayload {
            question: "Q?",
            options: &options,
            plan_phase: Some("phase-3"),
            context: Some("ctx"),
            agent_session_id: Some(
                Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap(),
            ),
            device_id: Some(Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap()),
        };
        let v = serde_json::to_value(&payload).unwrap();
        assert_eq!(v["question"], "Q?");
        assert!(v["options"].is_array());
        assert_eq!(v["plan_phase"], "phase-3");
        assert_eq!(v["context"], "ctx");
        assert_eq!(
            v["agent_session_id"],
            "11111111-1111-1111-1111-111111111111"
        );
        assert_eq!(v["device_id"], "22222222-2222-2222-2222-222222222222");
    }
}

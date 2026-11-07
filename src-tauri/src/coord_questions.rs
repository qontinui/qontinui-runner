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
    /// The asking session's recommended answer, so the operator receives a
    /// fork WITH a recommendation rather than an open question (plan
    /// `2026-09-20-what-is-the-state-of-my-projects-and-what-needs-me-is-answerable-from-one-screen`,
    /// Phase 3). Omitted from the wire when `None`, and [`post_question`]
    /// sends a blank one as `None`: coord's
    /// `AskQuestionRequest` is read with a plain (non-strict) `Json`
    /// extractor, so a coord without the field (every coord as of this
    /// change — the reader lands with the plan's coord Phase 3 PR) ignores the
    /// key, and one with it stores NULL for an absent one — sending `null`
    /// would say nothing more.
    ///
    /// That ignored key still answers 200, so the status says nothing about
    /// whether the value was recorded: [`post_question`] reads that from the
    /// reply's fork-shape disclosure instead ([`RecommendationRecorded`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    recommendation: Option<&'a str>,
    /// What changes if the operator overturns `recommendation`. Same
    /// omit-when-`None` (or blank) rule, and the same read-back.
    #[serde(skip_serializing_if = "Option::is_none")]
    if_overturned: Option<&'a str>,
}

/// The `shape` a coord with the plan's Phase 3 reader serves for a pending
/// question that carries a recommendation.
const SHAPE_FORK: &str = "fork_with_recommendation";
/// The `shape` it serves for one that does not: the operator is shown an open
/// question.
const SHAPE_OPEN: &str = "open_question";

/// The `POST /agents/:agent_id/ask-question` reply: the row (only its id is
/// read here) plus the fork-shape disclosure that the plan's coord Phase 3
/// reader adds to a PENDING question's reply. That reader is unlanded as of
/// this change, so every coord today sends none of the three disclosure keys.
///
/// With the reader, `shape` is on every pending reply, and
/// `recommendation_recorded` + `recommendation_note` appear only when a
/// supplied recommendation's recording is not confirmed. An auto-answered reply
/// carries none of the three.
///
/// The disclosure keys are kept as raw JSON, so a malformed value reads as
/// UNKNOWN instead of failing the decode of a question coord HAS recorded.
#[derive(Debug, Clone, Deserialize)]
struct AskQuestionReply {
    question_id: Uuid,
    /// How the operator will be shown the row: [`SHAPE_FORK`] or
    /// [`SHAPE_OPEN`].
    #[serde(default)]
    shape: Option<JsonValue>,
    /// `false` — nothing was written, so the operator sees an open question;
    /// `true` — written, but reading it back failed. Absent when the write was
    /// confirmed (or there was nothing to write).
    #[serde(default)]
    recommendation_recorded: Option<JsonValue>,
    /// coord's reason, beside `recommendation_recorded`.
    #[serde(default)]
    recommendation_note: Option<JsonValue>,
}

/// Whether coord recorded the `recommendation` / `if_overturned` a question
/// was posted with. Read from the ask reply's fork-shape disclosure, never
/// inferred from the 200: a coord without the reader drops both fields under
/// that same 200.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecommendationRecorded {
    /// Neither field was supplied (each `None` or blank): nothing to record.
    NotSupplied,
    /// The reply says they were recorded. Either it carries a recognised
    /// `shape` that does not contradict what was sent, and no
    /// `recommendation_recorded`; or it says `recommendation_recorded: true`.
    /// `note` is coord's `recommendation_note` in the second case: the write
    /// succeeded but could not be read back.
    Yes { note: Option<String> },
    /// The reply says they were NOT recorded, so the operator is shown an open
    /// question: `recommendation_recorded: false`, with `note` set to coord's
    /// `recommendation_note`. Or a `shape` of `open_question` although a
    /// recommendation was sent, with `note` saying so.
    No { note: Option<String> },
    /// The reply cannot say. Either it carries no fork-shape disclosure at all
    /// (a coord without the reader, or an auto-answered reply), or it carries
    /// a disclosure value this client does not recognise. Never to be read as
    /// `Yes`.
    Unknown { reason: String },
}

/// What [`post_question`] returns: the new question's id, and whether coord
/// recorded the recommendation fields it was posted with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PostedQuestion {
    pub question_id: Uuid,
    pub recommendation: RecommendationRecorded,
}

/// What [`ask_via_coord`] returns: the operator's answer, plus whether the
/// question it answers carried the recommendation. An answer to a fork and an
/// answer to an open question are different evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnsweredQuestion {
    pub question_id: Uuid,
    pub recommendation: RecommendationRecorded,
    pub response: String,
}

/// A blank value is no value. It is sent as `None` (omitted from the wire),
/// so it is never counted as supplied when the reply is read.
fn non_blank(v: Option<&str>) -> Option<&str> {
    v.filter(|s| !s.trim().is_empty())
}

/// Classify the reply's fork-shape disclosure against what was SENT (the
/// already-[`non_blank`] values). PURE: [`post_question`] does the warning.
fn recommendation_recorded(
    recommendation: Option<&str>,
    if_overturned: Option<&str>,
    reply: &AskQuestionReply,
) -> RecommendationRecorded {
    if recommendation.is_none() && if_overturned.is_none() {
        return RecommendationRecorded::NotSupplied;
    }
    let note = reply
        .recommendation_note
        .as_ref()
        .and_then(JsonValue::as_str)
        .map(str::to_owned);
    match &reply.recommendation_recorded {
        None => {}
        Some(JsonValue::Bool(false)) => return RecommendationRecorded::No { note },
        Some(JsonValue::Bool(true)) => return RecommendationRecorded::Yes { note },
        Some(other) => {
            return RecommendationRecorded::Unknown {
                reason: format!("the reply's recommendation_recorded is not a boolean: {other}"),
            }
        }
    }
    match &reply.shape {
        None => RecommendationRecorded::Unknown {
            reason: "the reply carries no fork-shape disclosure (`shape`): a coord without \
                     the recommendation reader drops both fields under a 200, and an \
                     auto-answered reply carries none either"
                .to_string(),
        },
        Some(JsonValue::String(s)) if s == SHAPE_OPEN && recommendation.is_some() => {
            RecommendationRecorded::No {
                note: Some(format!(
                    "the reply's shape is `{SHAPE_OPEN}`: the operator is shown an open \
                     question although a recommendation was sent"
                )),
            }
        }
        // An `if_overturned` without a recommendation is an open question by
        // definition, so `open_question` contradicts nothing there.
        Some(JsonValue::String(s)) if s == SHAPE_FORK || s == SHAPE_OPEN => {
            RecommendationRecorded::Yes { note: None }
        }
        Some(other) => RecommendationRecorded::Unknown {
            reason: format!("the reply's shape {other} is not one this client recognises"),
        },
    }
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
/// question's id, and whether coord recorded the `recommendation` /
/// `if_overturned` it was posted with ([`PostedQuestion`]). That second half is
/// read from the reply's fork-shape disclosure, never assumed from the 200. A
/// `No`, an `Unknown` and an unconfirmed `Yes` are also warned here.
///
/// `coord_base` is the HTTP base (no trailing slash, no `/ws` suffix) —
/// resolve it via `profiles::connected_coord_base()` at the call site; this
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
    recommendation: Option<&str>,
    if_overturned: Option<&str>,
) -> Result<PostedQuestion, CoordQuestionError> {
    let url = format!(
        "{}/agents/{}/ask-question",
        coord_base.trim_end_matches('/'),
        agent_id
    );
    let recommendation = non_blank(recommendation);
    let if_overturned = non_blank(if_overturned);
    let body = AskQuestionPayload {
        question,
        options,
        plan_phase,
        context,
        agent_session_id,
        device_id,
        recommendation,
        if_overturned,
    };

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| CoordQuestionError::Http(format!("reqwest builder: {e}")))?;

    let mut last_err: Option<CoordQuestionError> = None;
    for attempt in 0..2 {
        // coord-tenant-scope(session-noop): agent_id, agent_session_id and device_id are all parameters, but coord's post_ask_question resolves the tenant with resolve_tenant_from_agent_id off the PATH agent_id -- not the bearer, not the body -- on an anonymous device-keyed write. No credential change reaches this row; its tenancy is set at allocate. Terminal.
        match crate::auth::attach_device_auth(client.post(&url).json(&body))
            .send()
            .await
        {
            Ok(resp) => {
                let status = resp.status();
                if !status.is_success() {
                    let text = resp.text().await.unwrap_or_default();
                    last_err = Some(CoordQuestionError::Status(
                        status.as_u16(),
                        text.chars().take(500).collect(),
                    ));
                } else {
                    let parsed: AskQuestionReply = resp.json().await.map_err(|e| {
                        CoordQuestionError::Parse(format!("decode ask-question response: {e}"))
                    })?;
                    let question_id = parsed.question_id;
                    let recorded = recommendation_recorded(recommendation, if_overturned, &parsed);
                    match &recorded {
                        RecommendationRecorded::NotSupplied
                        | RecommendationRecorded::Yes { note: None } => {}
                        RecommendationRecorded::Yes { note: Some(note) } => warn!(
                            "coord_questions::post_question: question {question_id} (agent \
                             {agent_id}): coord recorded its recommendation / if_overturned \
                             but could not confirm the write: {note}"
                        ),
                        RecommendationRecorded::No { note } => warn!(
                            "coord_questions::post_question: question {question_id} (agent \
                             {agent_id}) is recorded WITHOUT its recommendation / \
                             if_overturned, so the operator is shown an open question: {}",
                            note.as_deref()
                                .unwrap_or("coord gave no recommendation_note")
                        ),
                        RecommendationRecorded::Unknown { reason } => warn!(
                            "coord_questions::post_question: question {question_id} (agent \
                             {agent_id}): whether coord recorded its recommendation / \
                             if_overturned is UNKNOWN: {reason}"
                        ),
                    }
                    debug!(
                        "coord_questions::post_question: posted question_id={} for agent_id={}",
                        question_id, agent_id
                    );
                    return Ok(PostedQuestion {
                        question_id,
                        recommendation: recorded,
                    });
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
/// response together with [`post_question`]'s verdict on the recommendation
/// fields, so a caller can tell an answered fork from an answered open
/// question. The eventual MCP tool calls this directly.
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
    recommendation: Option<&str>,
    if_overturned: Option<&str>,
    poll_timeout: Duration,
) -> Result<AnsweredQuestion, CoordQuestionError> {
    let posted = post_question(
        coord_base,
        agent_id,
        plan_phase,
        question,
        options,
        context,
        agent_session_id,
        device_id,
        recommendation,
        if_overturned,
    )
    .await?;
    let response = poll_answer(coord_base, posted.question_id, poll_timeout).await?;
    Ok(AnsweredQuestion {
        question_id: posted.question_id,
        recommendation: posted.recommendation,
        response,
    })
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
        let _amb = crate::test_env::isolated_ambient();
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
            None,
            None,
        )
        .await
        .expect("post_question");
        assert_eq!(out.question_id, qid);
        assert_eq!(out.recommendation, RecommendationRecorded::NotSupplied);
    }

    /// A fake coord whose ask reply is `reply`. Returns the base URL and every
    /// POST body it received, so a test can see what went on the wire.
    async fn spawn_ask_coord(
        reply: serde_json::Value,
    ) -> (String, Arc<Mutex<Vec<serde_json::Value>>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let bodies = Arc::new(Mutex::new(Vec::new()));
        let seen = bodies.clone();
        let app: Router = Router::new().route(
            "/agents/{agent_id}/ask-question",
            post(
                move |AxumPath(_id): AxumPath<Uuid>, Json(body): Json<serde_json::Value>| {
                    let seen = seen.clone();
                    let reply = reply.clone();
                    async move {
                        seen.lock().await.push(body);
                        (StatusCode::OK, Json(reply))
                    }
                },
            ),
        );
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        (format!("http://{addr}"), bodies)
    }

    /// Capture this thread's WARN-and-above `tracing` output until the guard
    /// drops. A `#[tokio::test]` runtime is current-thread, so the client's
    /// warnings are emitted here.
    fn capture_warnings() -> (
        tracing::subscriber::DefaultGuard,
        Arc<std::sync::Mutex<Vec<u8>>>,
    ) {
        #[derive(Clone)]
        struct Buf(Arc<std::sync::Mutex<Vec<u8>>>);
        impl std::io::Write for Buf {
            fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
                self.0
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .extend_from_slice(b);
                Ok(b.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let buf = Buf(Arc::default());
        let sink = buf.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(move || sink.clone())
            .with_ansi(false)
            .with_max_level(tracing::Level::WARN)
            .finish();
        (tracing::subscriber::set_default(subscriber), buf.0)
    }

    /// The question id every fork-shape fixture reply carries.
    const FORK_QID: &str = "eeeeeeee-eeee-eeee-eeee-eeeeeeeeeeee";

    /// Post one question through the real POST path to a fake coord whose ask
    /// reply is the row plus `disclosure` (the keys of coord's fork-shape
    /// disclosure, `add_fork_shape_disclosure` in the plan's coord Phase 3 PR,
    /// or none of them for a coord without it). Returns the verdict and the
    /// WARN output the client logged.
    async fn post_against_reply(
        disclosure: serde_json::Value,
        recommendation: Option<&str>,
        if_overturned: Option<&str>,
    ) -> (RecommendationRecorded, String) {
        let _amb = crate::test_env::isolated_ambient();
        let qid = Uuid::parse_str(FORK_QID).unwrap();
        let mut reply = serde_json::json!({
            "question_id": qid,
            "response": null,
            "responded_at": null,
        });
        for (k, v) in disclosure.as_object().unwrap() {
            reply[k] = v.clone();
        }
        let (base, _) = spawn_ask_coord(reply).await;
        let (guard, logs) = capture_warnings();
        let out = post_question(
            &base,
            Uuid::new_v4(),
            None,
            "Q?",
            &serde_json::json!(["a", "b"]),
            None,
            None,
            None,
            recommendation,
            if_overturned,
        )
        .await
        .expect("post_question");
        drop(guard);
        assert_eq!(out.question_id, qid);
        let log = String::from_utf8(logs.lock().unwrap().clone()).unwrap();
        (out.recommendation, log)
    }

    /// The client warned with `fragment`, naming the question.
    fn assert_warned(log: &str, fragment: &str) {
        assert!(
            log.contains(fragment) && log.contains(FORK_QID),
            "expected a warning containing {fragment:?} and the question id, got: {log}"
        );
    }

    /// The client logged no warning of its own.
    fn assert_not_warned(log: &str) {
        assert!(
            !log.contains("coord_questions::post_question"),
            "expected no warning, got: {log}"
        );
    }

    /// A coord without the reader answers a bare 200 and drops both fields: the
    /// reply cannot say, so the verdict is UNKNOWN and warned — never `Yes`.
    #[tokio::test]
    async fn recommendation_on_a_reply_without_a_disclosure_is_unknown() {
        let (got, log) = post_against_reply(
            serde_json::json!({}),
            Some("ship it"),
            Some("the phase slips"),
        )
        .await;
        assert!(
            matches!(&got, RecommendationRecorded::Unknown { reason }
                if reason.contains("no fork-shape disclosure")),
            "{got:?}"
        );
        assert_warned(&log, "is UNKNOWN");
    }

    /// A confirmed write: a fork shape and no `recommendation_recorded` key.
    #[tokio::test]
    async fn recommendation_confirmed_as_a_fork_is_recorded() {
        let (got, log) = post_against_reply(
            serde_json::json!({ "shape": "fork_with_recommendation" }),
            Some("ship it"),
            Some("the phase slips"),
        )
        .await;
        assert_eq!(got, RecommendationRecorded::Yes { note: None });
        assert_not_warned(&log);
    }

    /// `recommendation_recorded: false`: nothing was written, the operator is
    /// shown an open question, and coord's note is carried and warned.
    #[tokio::test]
    async fn recommendation_coord_says_not_recorded_is_no() {
        let (got, log) = post_against_reply(
            serde_json::json!({
                "shape": "open_question",
                "recommendation_recorded": false,
                "recommendation_note": "the question is recorded, but its recommendation / \
                    if_overturned could NOT be (the column is absent)",
            }),
            Some("ship it"),
            None,
        )
        .await;
        assert!(
            matches!(&got, RecommendationRecorded::No { note: Some(n) } if n.contains("could NOT be")),
            "{got:?}"
        );
        assert_warned(&log, "recorded WITHOUT its recommendation");
    }

    /// `recommendation_recorded: true`: written but not read back. coord says
    /// recorded, so `Yes` — carrying coord's note, which is warned.
    #[tokio::test]
    async fn recommendation_written_but_unconfirmed_is_yes_with_the_note() {
        let (got, log) = post_against_reply(
            serde_json::json!({
                "shape": "fork_with_recommendation",
                "recommendation_recorded": true,
                "recommendation_note": "the recommendation / if_overturned were recorded, but \
                    the write could not be confirmed (read-back failed)",
            }),
            Some("ship it"),
            None,
        )
        .await;
        assert!(
            matches!(&got, RecommendationRecorded::Yes { note: Some(n) }
                if n.contains("could not be confirmed")),
            "{got:?}"
        );
        assert_warned(&log, "could not confirm the write");
    }

    /// A sent recommendation whose reply shape is `open_question` was not
    /// recorded, whatever the reply omits: `No`, warned.
    #[tokio::test]
    async fn recommendation_sent_but_shown_as_an_open_question_is_no() {
        let (got, log) = post_against_reply(
            serde_json::json!({ "shape": "open_question" }),
            Some("ship it"),
            None,
        )
        .await;
        assert!(
            matches!(&got, RecommendationRecorded::No { note: Some(n) } if n.contains("open_question")),
            "{got:?}"
        );
        assert_warned(&log, "recorded WITHOUT its recommendation");
    }

    /// `if_overturned` alone makes no fork, so `open_question` contradicts
    /// nothing: recorded, not warned.
    #[tokio::test]
    async fn recommendation_if_overturned_alone_on_an_open_question_is_recorded() {
        let (got, log) = post_against_reply(
            serde_json::json!({ "shape": "open_question" }),
            None,
            Some("the phase slips"),
        )
        .await;
        assert_eq!(got, RecommendationRecorded::Yes { note: None });
        assert_not_warned(&log);
    }

    /// A shape this client does not know is UNKNOWN, warned.
    #[tokio::test]
    async fn recommendation_with_an_unrecognised_shape_is_unknown() {
        let (got, log) = post_against_reply(
            serde_json::json!({ "shape": "fork_with_two_recommendations" }),
            Some("ship it"),
            None,
        )
        .await;
        assert!(
            matches!(&got, RecommendationRecorded::Unknown { reason }
                if reason.contains("not one this client recognises")),
            "{got:?}"
        );
        assert_warned(&log, "is UNKNOWN");
    }

    /// A non-boolean `recommendation_recorded` is UNKNOWN (and still decodes:
    /// the question itself WAS recorded), warned.
    #[tokio::test]
    async fn recommendation_with_a_non_boolean_recorded_flag_is_unknown() {
        let (got, log) = post_against_reply(
            serde_json::json!({
                "shape": "fork_with_recommendation",
                "recommendation_recorded": "yes",
            }),
            Some("ship it"),
            None,
        )
        .await;
        assert!(
            matches!(&got, RecommendationRecorded::Unknown { reason } if reason.contains("not a boolean")),
            "{got:?}"
        );
        assert_warned(&log, "is UNKNOWN");
    }

    /// Nothing sent, nothing to record: the reply's silence is not UNKNOWN.
    #[tokio::test]
    async fn recommendation_not_supplied_needs_no_disclosure() {
        let (got, log) = post_against_reply(serde_json::json!({}), None, None).await;
        assert_eq!(got, RecommendationRecorded::NotSupplied);
        assert_not_warned(&log);
    }

    /// A blank recommendation is no recommendation: it is omitted from the
    /// wire like `None` (so a reply with no disclosure is not UNKNOWN), and a
    /// non-blank one is sent verbatim.
    #[tokio::test]
    async fn post_question_sends_a_blank_recommendation_as_none() {
        let _amb = crate::test_env::isolated_ambient();
        let qid = Uuid::parse_str("12121212-1212-1212-1212-121212121212").unwrap();
        let (base, bodies) = spawn_ask_coord(serde_json::json!({
            "question_id": qid,
            "response": null,
            "responded_at": null,
        }))
        .await;
        let options = serde_json::json!(["a"]);
        for (recommendation, if_overturned) in [(Some("   "), Some("")), (Some("ship it"), None)] {
            let out = post_question(
                &base,
                Uuid::new_v4(),
                None,
                "Q?",
                &options,
                None,
                None,
                None,
                recommendation,
                if_overturned,
            )
            .await
            .expect("post_question");
            if recommendation == Some("   ") {
                assert_eq!(out.recommendation, RecommendationRecorded::NotSupplied);
            }
        }

        let bodies = bodies.lock().await;
        assert_eq!(bodies.len(), 2, "{bodies:?}");
        let blank = bodies[0].as_object().unwrap();
        assert!(
            !blank.contains_key("recommendation"),
            "got: {:?}",
            bodies[0]
        );
        assert!(!blank.contains_key("if_overturned"), "got: {:?}", bodies[0]);
        assert_eq!(bodies[1]["recommendation"], "ship it");
        assert!(!bodies[1].as_object().unwrap().contains_key("if_overturned"));
    }

    /// `ask_via_coord` hands back the post's recommendation verdict with the
    /// answer, so the caller knows whether the operator answered a fork.
    #[tokio::test]
    async fn ask_via_coord_returns_the_recommendation_verdict_with_the_answer() {
        let _amb = crate::test_env::isolated_ambient();
        let qid = Uuid::parse_str("34343434-3434-3434-3434-343434343434").unwrap();
        let post_body = Arc::new(Mutex::new(serde_json::json!({
            "question_id": qid,
            "response": null,
            "responded_at": null,
            "shape": "fork_with_recommendation",
        })));
        let get_bodies = Arc::new(Mutex::new(vec![serde_json::json!({
            "question_id": qid,
            "response": "b",
            "responded_at": "2026-10-27T00:00:00Z",
        })]));
        let base = spawn_fake_coord(post_body, get_bodies, Arc::new(AtomicUsize::new(0))).await;

        let answered = ask_via_coord(
            &base,
            Uuid::new_v4(),
            None,
            "Q?",
            &serde_json::json!(["a", "b"]),
            None,
            None,
            None,
            Some("b"),
            Some("the phase slips"),
            Duration::from_secs(30),
        )
        .await
        .expect("ask_via_coord");
        assert_eq!(
            answered,
            AnsweredQuestion {
                question_id: qid,
                recommendation: RecommendationRecorded::Yes { note: None },
                response: "b".to_string(),
            }
        );
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
        let _amb = crate::test_env::isolated_ambient();
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
            recommendation: Some("ship it"),
            if_overturned: Some("the phase slips a week"),
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
        assert_eq!(v["recommendation"], "ship it");
        assert_eq!(v["if_overturned"], "the phase slips a week");
    }

    #[test]
    fn ask_question_payload_omits_absent_recommendation_fields() {
        // An absent recommendation is omitted from the wire, not sent as
        // `null` — the question is then served as an open question.
        let options = serde_json::json!(["a"]);
        let payload = AskQuestionPayload {
            question: "Q?",
            options: &options,
            plan_phase: None,
            context: None,
            agent_session_id: None,
            device_id: None,
            recommendation: None,
            if_overturned: None,
        };
        let v = serde_json::to_value(&payload).unwrap();
        let obj = v.as_object().expect("payload is an object");
        assert!(!obj.contains_key("recommendation"), "got: {v}");
        assert!(!obj.contains_key("if_overturned"), "got: {v}");
    }
}

//! Phase 5 — promote `/summarize-session` to an in-product Rust feature.
//!
//! Mirrors the personal slash command at
//! `qontinui-claude-config/.claude/commands/summarize-session.md` so any
//! qontinui user can extract learnings from a finished AI session and
//! persist them to `productivity_knowledge` without depending on the
//! Claude CLI or the personal config repo.
//!
//! High-level flow (matches the slash command's §1–§6):
//!
//! 1. Validate the `task_run_id` exists. If the session is still running,
//!    return [`SummarizeError::SessionNotComplete`].
//! 2. Load the session transcript (DB `output_log` column).
//! 3. Fetch the operative review verdict for the session
//!    ([`PgDb::get_latest_review_for_session`]). Default to `approved`
//!    when no review exists.
//! 4. Use [`OneshotLlm`] to extract learnings as a structured array — the
//!    LLM gets the verdict in the user prompt so it can apply the
//!    failed-attempt Outcome-tag rule per the slash command's §4.
//! 5. POST each learning to the runner's `/productivity-knowledge` HTTP
//!    endpoint (loopback). Server-side embedding generation, FTS index
//!    population, etc. happens in that handler — we don't duplicate it.
//! 6. Return a `SummarizeResult` with counts by area.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use crate::ai_provider::oneshot::{OneshotError, OneshotLlm, OneshotLlmExt};
use crate::database::pg::PgDb;

// =============================================================================
// LLM response schema
// =============================================================================

/// Top-level LLM response: a flat list of learnings extracted from the
/// session transcript. Round-trips through [`OneshotLlmExt::call_typed`]
/// which auto-derives the JSON schema from this struct via `schemars`.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct SummarizedLearnings {
    pub learnings: Vec<Learning>,
}

/// One learning extracted from the session. `area` is the bucket in
/// `productivity_knowledge.area`; `summary` is the 1-3 sentence
/// description; `body` is the full markdown explanation with file:line
/// citations.
///
/// For verdict ∈ {needs_fix, escalate}, the LLM is instructed to lead
/// `body` with the H2 Outcome tag per the slash command's §4 — see
/// [`SUMMARIZE_SYSTEM_PROMPT`].
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct Learning {
    /// One of: executor | claude_session | dispatcher | database | ui-bridge |
    /// frontend | migrations | tests | coordinator | testing-infra | other.
    /// Free-form so new areas can be coined sparingly without a code change.
    pub area: String,
    /// 1-3 sentence description.
    pub summary: String,
    /// Markdown explanation with file:line citations. For failed-attempt
    /// sessions (verdict=needs_fix or verdict=escalate) MUST lead with
    /// `## Outcome: APPROACH FAILED — do not retry without addressing X`.
    pub body: String,
}

// =============================================================================
// Public API
// =============================================================================

/// Successful return from [`summarize_session_in_product`]. The Tauri
/// command surfaces this directly to the UI for a toast / inline panel.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SummarizeResult {
    pub task_run_id: String,
    /// Verdict that drove the Outcome-tag rule; one of `approved`,
    /// `needs_fix`, `escalate`. Defaults to `approved` when no review
    /// exists.
    pub verdict: String,
    pub learning_count: usize,
    /// Per-area counts. Stable keys = whatever the LLM emitted; the
    /// frontend doesn't enumerate, just renders.
    pub by_area: HashMap<String, usize>,
    /// True if the placeholder fallback path was used (no LLM provider
    /// configured). The UI renders a "manual summary required" hint and
    /// links to the knowledge browser.
    pub placeholder: bool,
}

/// Errors surfaced by [`summarize_session_in_product`]. The Tauri command
/// maps these to UI hint strings.
#[derive(Debug)]
pub enum SummarizeError {
    SessionNotFound(String),
    SessionNotComplete(String),
    TranscriptUnavailable(String),
    LlmDisabled,
    LlmNoCredentials,
    LlmCallFailed(String),
    LlmSchemaParse(String),
    HttpPost(String),
    Db(String),
}

impl std::fmt::Display for SummarizeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SummarizeError::SessionNotFound(id) => {
                write!(f, "no task run with id: {}", id)
            }
            SummarizeError::SessionNotComplete(id) => write!(
                f,
                "session {} is still running; summarize after the session completes",
                id
            ),
            SummarizeError::TranscriptUnavailable(id) => {
                write!(f, "no transcript available for session {}", id)
            }
            SummarizeError::LlmDisabled => {
                write!(f, "no LLM provider is configured (Settings → AI)")
            }
            SummarizeError::LlmNoCredentials => write!(
                f,
                "the active LLM provider has no credentials configured"
            ),
            SummarizeError::LlmCallFailed(e) => write!(f, "LLM call failed: {}", e),
            SummarizeError::LlmSchemaParse(e) => {
                write!(f, "LLM response did not match expected schema: {}", e)
            }
            SummarizeError::HttpPost(e) => {
                write!(f, "POST /productivity-knowledge failed: {}", e)
            }
            SummarizeError::Db(e) => write!(f, "database error: {}", e),
        }
    }
}

impl std::error::Error for SummarizeError {}

impl From<OneshotError> for SummarizeError {
    fn from(e: OneshotError) -> Self {
        match e {
            OneshotError::NoCredentials => SummarizeError::LlmNoCredentials,
            OneshotError::Disabled => SummarizeError::LlmDisabled,
            OneshotError::SchemaParse(msg) => SummarizeError::LlmSchemaParse(msg),
            OneshotError::ProviderTransport(msg) => SummarizeError::LlmCallFailed(msg),
            OneshotError::BudgetExhausted => {
                SummarizeError::LlmCallFailed("budget exhausted".to_string())
            }
        }
    }
}

// =============================================================================
// System prompt — encodes slash command §2-§4
// =============================================================================

/// Verdict values that trigger the failed-attempt Outcome-tag rule per
/// the slash command's §4. Kept as a `&[&str]` so the test for
/// "verdict=approved must NOT trigger the tag" can iterate easily.
pub const FAILED_VERDICTS: &[&str] = &["needs_fix", "escalate"];

/// System prompt used for the LLM round-trip. Encodes the slash command's
/// §2 (what counts as a learning), §3 (area bucketing), and §4 (the
/// failed-attempt Outcome tag rule). Update history: any edit requires
/// updating the `system_prompt_snapshot` test below.
pub const SUMMARIZE_SYSTEM_PROMPT: &str = "You are extracting durable learnings from a finished AI worker session's transcript.\n\
Your job: read the transcript and emit a JSON object with a `learnings` array describing every non-obvious fact about the codebase the worker discovered.\n\
\n\
WHAT COUNTS AS A LEARNING (slash command rule 2):\n\
- A non-obvious fact about the codebase (e.g. \"FileRegistryManager.normalize_path lowercases drive letters on Windows\", \"PG migrations live inline in mod.rs::MIGRATIONS, not a separate migrations/ directory\").\n\
- A failure-mode discovery: \"approach X did not work because Y\". Failed-attempt summaries are the highest-leverage knowledge — emit them aggressively.\n\
- A non-obvious invariant the worker observed (e.g. \"the dispatcher emits file-lock-acquired only after a wait, not on immediate acquisition\").\n\
\n\
WHAT IS NOT A LEARNING:\n\
- \"I edited file X.\" / \"The test passed.\" / \"The task was done.\"\n\
- A summary of the session itself. The transcript is the summary; the knowledge table is for facts that survive the session.\n\
- Speculation. \"I think X\" beats nothing, but \"I observed X\" is preferred.\n\
\n\
AIM FOR 1-4 LEARNINGS PER SESSION. Many sessions have zero — do not manufacture. An empty array is a valid response.\n\
\n\
AREA BUCKETING (slash command rule 3):\n\
Pick `area` from: executor, claude_session, dispatcher, database, ui-bridge, frontend, migrations, tests, coordinator, testing-infra, other.\n\
Invent new areas sparingly — they are free-form text and grouped via FTS.\n\
\n\
FAILED-ATTEMPT OUTCOME TAG (slash command rule 4 — REQUIRED for verdict=needs_fix or verdict=escalate):\n\
The user prompt tells you the verdict. If the verdict is `needs_fix` or `escalate`, EVERY learning's `body` MUST lead with this exact line, on its own line, before any other content:\n\
\n\
## Outcome: APPROACH FAILED — do not retry without addressing X\n\
\n\
Replace `X` with the concrete blocker the next session must resolve before re-trying (e.g. \"the migration ordering causes a TIMESTAMPTZ drift\" or \"the snapshot prune races with the dispatcher snapshot insert\").\n\
The tag is parsed by the knowledge browser to surface failure-mode warnings distinctly.\n\
\n\
For verdict=approved sessions, OMIT the Outcome tag — the body is a positive learning and adding the tag would be misleading.\n\
\n\
OUTPUT REQUIREMENTS:\n\
- summary: 1-3 sentences, no leading H2 tag.\n\
- body: full markdown with at least one file:line citation. Lead with the Outcome tag for verdict=needs_fix/escalate, omit it for verdict=approved.\n\
- area: one of the canonical buckets above, or a new one only when none fit.\n\
- learnings: an array. Empty is acceptable when nothing non-obvious surfaced.\n\
\n\
Pull learnings from observation, not speculation.";

// =============================================================================
// Implementation
// =============================================================================

/// Summarize a session's transcript: extract learnings via the configured
/// `OneshotLlm` and persist each via loopback POST to
/// `/productivity-knowledge`.
///
/// See module docs for the high-level flow. Returns
/// [`SummarizeError::LlmDisabled`] / [`SummarizeError::LlmNoCredentials`]
/// when no provider is configured; the Tauri command catches these and
/// inserts a placeholder knowledge row instead.
pub async fn summarize_session_in_product(
    pg: &Arc<PgDb>,
    llm: &dyn OneshotLlm,
    runner_port: u16,
    task_run_id: &str,
) -> Result<SummarizeResult, SummarizeError> {
    info!(
        "productivity.summarize.started task_run_id={} provider={}",
        task_run_id,
        llm.name()
    );

    // 1. Validate session exists + completion state.
    let task_run = pg
        .get_task_run(task_run_id)
        .await
        .map_err(SummarizeError::Db)?
        .ok_or_else(|| SummarizeError::SessionNotFound(task_run_id.to_string()))?;

    if task_run.status == "running" || task_run.status == "paused" {
        return Err(SummarizeError::SessionNotComplete(task_run_id.to_string()));
    }

    // 2. Load transcript. The runner persists output_log via the same
    //    column the `/sessions/<id>/transcript` HTTP endpoint reads, so
    //    we hit it directly rather than going back over loopback.
    let transcript = pg
        .get_task_output(task_run_id)
        .await
        .map_err(SummarizeError::Db)?;

    if transcript.trim().is_empty() {
        return Err(SummarizeError::TranscriptUnavailable(
            task_run_id.to_string(),
        ));
    }

    // 3. Fetch verdict. Treat "no review yet" as approved per slash
    //    command §1 — we still emit learnings, just without the Outcome
    //    tag. Failure to query the reviews table is logged but doesn't
    //    abort: the summary still has value.
    let verdict = match pg.get_latest_review_for_session(task_run_id).await {
        Ok(Some(r)) => r.verdict,
        Ok(None) => "approved".to_string(),
        Err(e) => {
            warn!(
                "productivity.summarize.review_query_failed task_run_id={} err={} (defaulting to approved)",
                task_run_id, e
            );
            "approved".to_string()
        }
    };

    debug!(
        "productivity.summarize.verdict task_run_id={} verdict={}",
        task_run_id, verdict
    );

    // 4. LLM extraction. The verdict goes into the user prompt so the
    //    LLM applies the §4 Outcome-tag rule correctly.
    let user_prompt = build_user_prompt(task_run_id, &verdict, &transcript);
    let summarized: SummarizedLearnings = llm
        .call_typed::<SummarizedLearnings>(SUMMARIZE_SYSTEM_PROMPT, &user_prompt)
        .await?;

    info!(
        "productivity.summarize.llm_ok task_run_id={} learning_count={}",
        task_run_id,
        summarized.learnings.len()
    );

    // 5. Persist via loopback. Each learning is a separate POST so a
    //    transient error on one row doesn't drop the others. We collect
    //    by-area counts as we go.
    let mut by_area: HashMap<String, usize> = HashMap::new();
    let client = build_client()?;

    for learning in summarized.learnings.iter() {
        match post_knowledge(&client, runner_port, task_run_id, learning).await {
            Ok(()) => {
                *by_area.entry(learning.area.clone()).or_insert(0) += 1;
            }
            Err(e) => {
                warn!(
                    "productivity.summarize.persist_failed task_run_id={} area={} err={}",
                    task_run_id, learning.area, e
                );
            }
        }
    }

    let learning_count: usize = by_area.values().sum();

    info!(
        "productivity.summarize.completed task_run_id={} verdict={} learning_count={}",
        task_run_id, verdict, learning_count
    );

    Ok(SummarizeResult {
        task_run_id: task_run_id.to_string(),
        verdict,
        learning_count,
        by_area,
        placeholder: false,
    })
}

/// Insert a single placeholder knowledge row when no LLM provider is
/// configured. The UI surfaces this as "manual summary required" with a
/// link to the knowledge browser.
///
/// Returns the same `SummarizeResult` shape as the success path but with
/// `placeholder: true` and `learning_count: 1`.
pub async fn write_placeholder_summary(
    runner_port: u16,
    task_run_id: &str,
) -> Result<SummarizeResult, SummarizeError> {
    let placeholder = Learning {
        area: "other".to_string(),
        summary: format!(
            "Session {} completed but no LLM provider was configured to extract learnings.",
            task_run_id
        ),
        body: "LLM provider not configured; manual summary required.".to_string(),
    };

    let client = build_client()?;
    post_knowledge(&client, runner_port, task_run_id, &placeholder).await?;

    let mut by_area = HashMap::new();
    by_area.insert("other".to_string(), 1usize);

    Ok(SummarizeResult {
        task_run_id: task_run_id.to_string(),
        verdict: "approved".to_string(),
        learning_count: 1,
        by_area,
        placeholder: true,
    })
}

// =============================================================================
// Helpers — prompt building, HTTP
// =============================================================================

/// Build the LLM user prompt: verdict header + transcript body. The
/// verdict line is a structured token the system prompt's §4 rule keys
/// off. Transcript is truncated server-side by the model's context
/// window; we don't pre-truncate here because cheap providers (Gemini
/// 1.5+, Claude with prompt caching) handle long inputs well.
fn build_user_prompt(task_run_id: &str, verdict: &str, transcript: &str) -> String {
    format!(
        "Session id: {task_run_id}\n\
         Verdict: {verdict}\n\
         \n\
         --- BEGIN TRANSCRIPT ---\n\
         {transcript}\n\
         --- END TRANSCRIPT ---",
        task_run_id = task_run_id,
        verdict = verdict,
        transcript = transcript
    )
}

fn build_client() -> Result<reqwest::Client, SummarizeError> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| SummarizeError::HttpPost(format!("client build: {}", e)))
}

/// Wire shape of the `/productivity-knowledge` POST body. Mirrors
/// [`crate::mcp::knowledge::InsertKnowledgeRequest`]. We re-declare it
/// here rather than importing because the receiving struct is
/// `Deserialize`-only; `Serialize` is needed for the outgoing JSON.
#[derive(Debug, Serialize)]
struct WireInsertKnowledgeRequest<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    task_id: Option<&'a str>,
    session_id: Option<&'a str>,
    area: &'a str,
    summary: &'a str,
    body: &'a str,
    /// Always omitted — endpoint embeds server-side via `embed_text`.
    #[serde(skip_serializing_if = "Option::is_none")]
    embedding_b64: Option<&'a str>,
}

async fn post_knowledge(
    client: &reqwest::Client,
    runner_port: u16,
    task_run_id: &str,
    learning: &Learning,
) -> Result<(), SummarizeError> {
    let url = format!("http://127.0.0.1:{}/productivity-knowledge", runner_port);

    let body = WireInsertKnowledgeRequest {
        task_id: None,
        session_id: Some(task_run_id),
        area: &learning.area,
        summary: &learning.summary,
        body: &learning.body,
        embedding_b64: None,
    };

    let response = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| SummarizeError::HttpPost(format!("send to {}: {}", url, e)))?;

    let status = response.status();
    if !status.is_success() {
        let body_text = response.text().await.unwrap_or_default();
        return Err(SummarizeError::HttpPost(format!(
            "{} returned {}: {}",
            url, status, body_text
        )));
    }

    Ok(())
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai_provider::oneshot::OneshotError;
    use async_trait::async_trait;
    use serde_json::Value;
    use std::sync::Mutex;

    /// Minimal mock adapter — returns a canned response or error, and
    /// records the prompts it was called with so tests can assert on the
    /// verdict pass-through.
    struct MockOneshot {
        canned: Mutex<Option<Result<Value, OneshotError>>>,
        last_user_prompt: Mutex<Option<String>>,
        last_system_prompt: Mutex<Option<String>>,
    }

    #[async_trait]
    impl OneshotLlm for MockOneshot {
        fn name(&self) -> &'static str {
            "mock"
        }

        async fn call_json(
            &self,
            system_prompt: &str,
            user_prompt: &str,
            _schema: Value,
            _schema_name: &str,
        ) -> Result<Value, OneshotError> {
            *self.last_system_prompt.lock().unwrap() = Some(system_prompt.to_string());
            *self.last_user_prompt.lock().unwrap() = Some(user_prompt.to_string());
            self.canned
                .lock()
                .unwrap()
                .take()
                .expect("canned response already consumed")
        }
    }

    fn mock_ok(value: Value) -> MockOneshot {
        MockOneshot {
            canned: Mutex::new(Some(Ok(value))),
            last_user_prompt: Mutex::new(None),
            last_system_prompt: Mutex::new(None),
        }
    }

    fn mock_err(e: OneshotError) -> MockOneshot {
        MockOneshot {
            canned: Mutex::new(Some(Err(e))),
            last_user_prompt: Mutex::new(None),
            last_system_prompt: Mutex::new(None),
        }
    }

    // --- System prompt snapshot ------------------------------------------

    #[test]
    fn system_prompt_snapshot() {
        let p = SUMMARIZE_SYSTEM_PROMPT;
        // Header.
        assert!(p.starts_with("You are extracting durable learnings"));
        // Each rule block.
        assert!(p.contains("WHAT COUNTS AS A LEARNING (slash command rule 2)"));
        assert!(p.contains("WHAT IS NOT A LEARNING"));
        assert!(p.contains("AREA BUCKETING (slash command rule 3)"));
        assert!(p.contains("FAILED-ATTEMPT OUTCOME TAG (slash command rule 4"));
        // The literal Outcome tag header must be embedded in the prompt
        // verbatim so the LLM can copy it byte-for-byte.
        assert!(p.contains("## Outcome: APPROACH FAILED — do not retry without addressing X"));
        // The negative case must be explicit so the LLM does not over-tag.
        assert!(p.contains("verdict=approved sessions, OMIT the Outcome tag"));
        // No trailing whitespace surprises.
        for line in p.lines() {
            assert!(
                !line.ends_with(' ') && !line.ends_with('\t'),
                "trailing whitespace in line: {:?}",
                line
            );
        }
    }

    #[test]
    fn system_prompt_size_is_reasonable() {
        let len = SUMMARIZE_SYSTEM_PROMPT.len();
        assert!(
            len > 1000,
            "prompt suspiciously short ({}b); did rules get dropped?",
            len
        );
        assert!(
            len < 5000,
            "prompt grew past 5KB ({}b); cache cost goes up",
            len
        );
    }

    // --- Verdict pass-through into user prompt ----------------------------

    /// CRITICAL guard: the slash command's §4 rule depends on the LLM
    /// receiving the verdict in the user prompt so it can apply the
    /// Outcome-tag rule. If this string drifts, every failed-attempt
    /// summary becomes a poison pill (next session reads the body as
    /// guidance).
    #[test]
    fn user_prompt_includes_verdict_token() {
        let prompt = build_user_prompt("task-123", "needs_fix", "transcript body");
        assert!(prompt.contains("Verdict: needs_fix"), "{}", prompt);
        assert!(prompt.contains("Session id: task-123"), "{}", prompt);
        assert!(prompt.contains("transcript body"), "{}", prompt);
        assert!(prompt.contains("--- BEGIN TRANSCRIPT ---"), "{}", prompt);
        assert!(prompt.contains("--- END TRANSCRIPT ---"), "{}", prompt);
    }

    #[test]
    fn user_prompt_includes_verdict_for_each_failed_value() {
        for v in FAILED_VERDICTS {
            let prompt = build_user_prompt("t", v, "body");
            assert!(prompt.contains(&format!("Verdict: {}", v)), "{}: {}", v, prompt);
        }
    }

    #[tokio::test]
    async fn verdict_passes_through_to_user_prompt_via_typed_call() {
        // End-to-end check on the call_typed surface: build the user
        // prompt with a failed verdict and confirm the mock saw it. This
        // is the unit-test equivalent of "did the LLM receive the
        // verdict as a token?" without needing PG.
        let mock = mock_ok(serde_json::json!({ "learnings": [] }));
        let user_prompt = build_user_prompt("session-A", "escalate", "body text");
        let _: SummarizedLearnings = mock
            .call_typed::<SummarizedLearnings>(SUMMARIZE_SYSTEM_PROMPT, &user_prompt)
            .await
            .expect("typed call");

        let captured = mock.last_user_prompt.lock().unwrap().clone().expect("captured");
        assert!(captured.contains("Verdict: escalate"), "{}", captured);
    }

    // --- LLM-disabled / no-credentials mappings ---------------------------

    #[tokio::test]
    async fn llm_disabled_maps_to_summarize_error() {
        let mock = mock_err(OneshotError::Disabled);
        let result: Result<SummarizedLearnings, OneshotError> =
            mock.call_typed::<SummarizedLearnings>("sys", "user").await;
        let err = SummarizeError::from(result.expect_err("disabled"));
        match err {
            SummarizeError::LlmDisabled => {}
            other => panic!("expected LlmDisabled, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn no_credentials_maps_to_summarize_error() {
        let mock = mock_err(OneshotError::NoCredentials);
        let result: Result<SummarizedLearnings, OneshotError> =
            mock.call_typed::<SummarizedLearnings>("sys", "user").await;
        let err = SummarizeError::from(result.expect_err("no creds"));
        match err {
            SummarizeError::LlmNoCredentials => {}
            other => panic!("expected LlmNoCredentials, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn schema_parse_maps_to_summarize_error() {
        let mock = mock_ok(serde_json::json!({ "wrong": 42 }));
        let result: Result<SummarizedLearnings, OneshotError> =
            mock.call_typed::<SummarizedLearnings>("sys", "user").await;
        let err = SummarizeError::from(result.expect_err("schema mismatch"));
        match err {
            SummarizeError::LlmSchemaParse(msg) => {
                assert!(msg.contains("deserialize") || msg.contains("missing"), "{}", msg);
            }
            other => panic!("expected LlmSchemaParse, got {:?}", other),
        }
    }

    // --- Empty learnings array round-trips --------------------------------

    #[tokio::test]
    async fn empty_learnings_is_a_valid_response() {
        // Sessions with nothing non-obvious produce zero learnings — the
        // slash command's §2 explicitly allows this.
        let mock = mock_ok(serde_json::json!({ "learnings": [] }));
        let r: SummarizedLearnings = mock
            .call_typed::<SummarizedLearnings>("sys", "user")
            .await
            .expect("zero ok");
        assert_eq!(r.learnings.len(), 0);
    }

    // --- Error display -----------------------------------------------------

    #[test]
    fn summarize_error_display_messages_are_actionable() {
        let cases = [
            SummarizeError::SessionNotFound("abc".into()),
            SummarizeError::SessionNotComplete("abc".into()),
            SummarizeError::TranscriptUnavailable("abc".into()),
            SummarizeError::LlmDisabled,
            SummarizeError::LlmNoCredentials,
            SummarizeError::LlmCallFailed("transport".into()),
            SummarizeError::LlmSchemaParse("nope".into()),
            SummarizeError::HttpPost("connreset".into()),
            SummarizeError::Db("pool empty".into()),
        ];
        let strings: Vec<String> = cases.iter().map(|e| format!("{}", e)).collect();
        let mut sorted = strings.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            strings.len(),
            "error display strings collide: {:?}",
            strings
        );
    }

    // --- Failed-verdict guard --------------------------------------------

    #[test]
    fn failed_verdicts_table_matches_slash_command() {
        // The slash command §1 explicitly names two verdicts that drive
        // the Outcome-tag rule. Drift here would either over-tag (poison
        // pills) or under-tag (next session reads body as guidance).
        assert_eq!(FAILED_VERDICTS.len(), 2);
        assert!(FAILED_VERDICTS.contains(&"needs_fix"));
        assert!(FAILED_VERDICTS.contains(&"escalate"));
    }
}

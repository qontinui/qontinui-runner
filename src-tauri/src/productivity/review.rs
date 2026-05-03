//! In-product `/auto-review` — Phase 4 of `productivity-stack-product-readiness.md`.
//!
//! Promotes the personal `/auto-review` slash command into a Rust function so
//! the runner can fire reviews against completed tasks without spawning a
//! Claude pty per task.
//!
//! ## Pipeline
//!
//! 1. Resolve the task row + reviewed worker session via [`PgDb`] directly.
//! 2. Plan-version mismatch guard: bail if the plan markdown's current SHA
//!    differs from `task.plan_version_hash`.
//! 3. Fetch the worker's transcript from `task_run_events` (event_type =
//!    `ai_output`, oldest-first) and truncate to the last 30k chars.
//! 4. Inspect `git diff` against the runner repo, scoped to
//!    `expected_file_claims`. Cross-check `session_touched_files` for
//!    scope-creep candidates.
//! 5. Best-effort test runs (`cargo check`, `npx tsc --noEmit`) bounded by
//!    a 2-min wall-clock timeout. Capture exit code separately from
//!    stdout/stderr.
//! 6. Send everything to [`OneshotLlm`] with a typed
//!    [`AutoReviewVerdict`] schema; the system prompt encodes the
//!    confidence-band guidance from the slash command.
//! 7. POST the verdict + reasoning + diff_summary + test_results to
//!    [`POST /reviews`] via loopback. Self-review (HTTP 409) passes
//!    through cleanly.
//!
//! ## Read-only invariant
//!
//! This module never modifies any code under review. The `cargo check` and
//! `tsc` invocations are read-only verifications; the diff inspection uses
//! `git diff` (no checkout/reset). The reviewer is the *reviewer*, not a
//! second worker.

#![allow(dead_code)]

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::process::Command;
use tokio::time::timeout;
use tracing::{debug, info, warn};

use crate::ai_provider::oneshot::{OneshotError, OneshotLlm, OneshotLlmExt};
use crate::database::pg::reviews::InsertReviewInput;
use crate::database::pg::PgDb;

// =============================================================================
// Public types
// =============================================================================

/// Outcome of an in-product auto-review.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewResult {
    pub review_id: String,
    pub verdict: String,
    pub confidence: f64,
}

/// Errors returned by [`auto_review_in_product`].
///
/// Coarse-grained on purpose — the Tauri-command wrapper turns the variant
/// into a single user-facing string. Add a finer taxonomy only when the UI
/// actually wants to branch on it.
#[derive(Debug)]
pub enum ReviewError {
    /// Task with the given id doesn't exist.
    TaskNotFound(String),
    /// The reviewed task has no `assigned_session_id` to attribute the work
    /// to. The slash command falls back to the most-recent task_run; we
    /// don't have that surface server-side, so we surface the gap as a
    /// dedicated error.
    NoAssignedSession(String),
    /// The plan markdown's current SHA differs from `task.plan_version_hash`.
    /// The task is being reviewed against a stale plan; the user should
    /// re-decompose first.
    PlanVersionMismatch {
        task_id: String,
        expected: String,
        actual: String,
    },
    /// LLM provider returned [`OneshotError::Disabled`] or
    /// [`OneshotError::NoCredentials`]. The Tauri-command path inserts a
    /// stub `escalate` review row with confidence 0; the scheduler path
    /// just logs and skips. Wrap message identifies which.
    LlmDisabled(String),
    /// LLM call failed for transport / shape / budget reasons.
    LlmCallFailed(String),
    /// Inserting the review row hit a DB error. Self-review (HTTP 409) is
    /// not a hard error — the upstream layer maps it to a clean skip.
    Db(String),
    /// Filesystem / git invocation failed.
    Io(String),
}

impl std::fmt::Display for ReviewError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReviewError::TaskNotFound(id) => write!(f, "task {} not found", id),
            ReviewError::NoAssignedSession(id) => {
                write!(f, "task {} has no assigned worker session", id)
            }
            ReviewError::PlanVersionMismatch {
                task_id,
                expected,
                actual,
            } => write!(
                f,
                "plan-version mismatch for task {}: task references {} but markdown is {}",
                task_id, expected, actual
            ),
            ReviewError::LlmDisabled(msg) => write!(f, "LLM provider unavailable: {}", msg),
            ReviewError::LlmCallFailed(msg) => write!(f, "LLM call failed: {}", msg),
            ReviewError::Db(msg) => write!(f, "DB error: {}", msg),
            ReviewError::Io(msg) => write!(f, "IO error: {}", msg),
        }
    }
}

impl std::error::Error for ReviewError {}

// =============================================================================
// LLM verdict shape
// =============================================================================

/// Typed verdict the reviewer LLM emits. Matches the `/auto-review` slash
/// command's §5 output contract verbatim:
///
/// - `verdict ∈ { approved, needs_fix, escalate }`
/// - `confidence ∈ [0, 1]` (slash command bands: 0.9–1.0 small/obvious;
///   0.7–0.89 large/unfamiliar; 0.5–0.69 partial coverage; <0.5 escalate)
/// - `reasoning` is a markdown body with file:line citations
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct AutoReviewVerdict {
    /// One of `approved`, `needs_fix`, `escalate`.
    pub verdict: String,
    /// Confidence in `[0.0, 1.0]`.
    pub confidence: f64,
    /// Markdown reasoning body. Should cite specific file:line locations
    /// for any defects (the slash command's §Rules require this).
    pub reasoning: String,
}

// =============================================================================
// Tunables
// =============================================================================

/// Cap on the worker transcript text sent to the reviewer LLM (chars).
/// Roughly 30k matches the slash-command's "feed the conversation" style
/// without blowing past Claude's input-token cost. We take the *tail* —
/// the latest exchanges are the most informative for verdict-forming.
const TRANSCRIPT_TRUNCATE_CHARS: usize = 30_000;

/// Wall-clock budget for any single test command (`cargo check`, `tsc`).
/// Per spec: 2 minutes, with `tokio::time::timeout`. On timeout the
/// command is marked `"timeout"` and the reviewer's confidence drops.
const TEST_COMMAND_TIMEOUT_SECS: u64 = 120;

/// Cap on captured test-output kept in `test_results` so the JSONB blob
/// in PG stays small. The reviewer sees the same truncation.
const TEST_OUTPUT_TAIL_CHARS: usize = 8_000;

// =============================================================================
// System prompt
// =============================================================================

/// System prompt for the reviewer LLM. Encodes the slash-command rules
/// from §5 verbatim — confidence bands, "no edits" rule, "be specific"
/// rule, "don't fabricate test results" rule.
const REVIEW_SYSTEM_PROMPT: &str = r#"You are an automated code reviewer for the qontinui productivity stack.

Your job is to emit a structured verdict for one completed task: a single JSON object matching the AutoReviewVerdict schema. You do NOT propose code edits; you describe what the worker did and whether it looks correct.

Constraints:
- You are READ-ONLY. If you spot a bug, describe it. Do not produce code patches.
- Cite file:line locations for any defects you call out. "Looks wrong" with no citation is not acceptable — confidence drops to <0.5 in that case and the verdict becomes escalate.
- Do not fabricate test results. If a test surface was not exercised, say so explicitly in the reasoning.

Verdict scale:
- "approved" — diff is consistent with the task description, claims align, all tests that ran passed, no obvious correctness/security issues.
- "needs_fix" — concrete defects identified. List each one with file:line and a short description.
- "escalate" — the task assumption is wrong, scope creep beyond the plan, or you cannot form an opinion confidently.

Confidence bands (use this directly):
- 0.90-1.00 — tests pass, claims match, diff is small and obvious
- 0.70-0.89 — tests pass, claims match, but the change is large or touches unfamiliar territory
- 0.50-0.69 — partial coverage (some tests didn't run, claim coverage is ambiguous)
- < 0.50 — you cannot form a real opinion. Pair with verdict="escalate".

Output JSON only. No prose outside the JSON."#;

// =============================================================================
// Public entry points
// =============================================================================

/// Run an in-product auto-review against `task_id`.
///
/// `reviewer_session_id` must differ from the reviewed worker's session id;
/// if they match the `/reviews` insert returns `SelfReview` and we propagate
/// it up (caller treats this as "no-op clean exit").
///
/// `runner_port` is currently unused but threaded through so future
/// work (HTTP-loopback POST to /reviews instead of direct DB insert) can
/// land without changing the call site.
pub async fn auto_review_in_product(
    pg: &Arc<PgDb>,
    llm: &dyn OneshotLlm,
    runner_port: u16,
    task_id: &str,
    reviewer_session_id: &str,
) -> Result<ReviewResult, ReviewError> {
    // 1. Resolve task + plan
    let task = pg
        .get_task_by_id(task_id)
        .await
        .map_err(ReviewError::Db)?
        .ok_or_else(|| ReviewError::TaskNotFound(task_id.to_string()))?;

    let reviewed_session_id = task
        .assigned_session_id
        .clone()
        .ok_or_else(|| ReviewError::NoAssignedSession(task_id.to_string()))?;

    if reviewer_session_id == reviewed_session_id {
        // Pre-flight self-review guard so we don't waste an LLM call.
        // Mirrors the slash command's `/Rules` "Don't review yourself".
        return Err(ReviewError::Db(
            "reviewer_session_id == reviewed_session_id (no self-review)".to_string(),
        ));
    }

    let plan = pg
        .get_plan_by_id(&task.plan_id)
        .await
        .map_err(ReviewError::Db)?
        .ok_or_else(|| ReviewError::Db(format!("plan {} not found", task.plan_id)))?;

    // 2. Plan-version mismatch guard
    let current_plan_hash = compute_plan_hash(&plan.markdown_path).map_err(ReviewError::Io)?;
    if current_plan_hash != task.plan_version_hash {
        return Err(ReviewError::PlanVersionMismatch {
            task_id: task_id.to_string(),
            expected: task.plan_version_hash.clone(),
            actual: current_plan_hash,
        });
    }

    // 3. Worker transcript (truncated to the latest 30k chars)
    let transcript = fetch_session_transcript(pg, &reviewed_session_id)
        .await
        .unwrap_or_else(|e| {
            warn!(
                "auto_review: failed to fetch transcript for {}: {}",
                reviewed_session_id, e
            );
            String::new()
        });
    let transcript_for_llm = tail_chars(&transcript, TRANSCRIPT_TRUNCATE_CHARS);

    // 4. Repo + diff inspection
    let repo_path = resolve_runner_repo_path()
        .ok_or_else(|| ReviewError::Io("could not resolve runner repo path".to_string()))?;

    let diff_summary = inspect_diff(&repo_path, &task.expected_file_claims).await;
    let scoped_diff_text = diff_summary.diff_text.clone();

    let touched_files = pg
        .get_files_touched(&reviewed_session_id)
        .await
        .unwrap_or_default();
    let scope_creep = scope_creep_candidates(&touched_files, &task.expected_file_claims);

    // 5. Test runs (best-effort, bounded)
    let test_results = run_relevant_tests(&repo_path, &touched_files).await;

    // 6. LLM verdict
    let user_prompt = build_user_prompt(
        &task.description,
        &task.expected_file_claims,
        &transcript_for_llm,
        &diff_summary,
        &scoped_diff_text,
        &scope_creep,
        &test_results,
    );

    let verdict = match llm
        .call_typed::<AutoReviewVerdict>(REVIEW_SYSTEM_PROMPT, &user_prompt)
        .await
    {
        Ok(v) => normalize_verdict(v),
        Err(OneshotError::Disabled) | Err(OneshotError::NoCredentials) => {
            return Err(ReviewError::LlmDisabled(
                "no LLM provider configured for auto-review".to_string(),
            ));
        }
        Err(other) => {
            return Err(ReviewError::LlmCallFailed(format!("{}", other)));
        }
    };

    info!(
        "auto_review: task={} verdict={} confidence={:.2} reasoning_len={}",
        task_id,
        verdict.verdict,
        verdict.confidence,
        verdict.reasoning.len()
    );

    // 7. Persist via PG (skipping the HTTP roundtrip — runner_port reserved
    //    for a future loopback-POST mode if we want the `review-completed`
    //    Tauri event emission for free. See plan §note on `/reviews`).
    let _ = runner_port; // currently unused; keep param shape stable.

    let diff_summary_json = serde_json::to_value(&diff_summary)
        .unwrap_or_else(|_| serde_json::json!({"files_changed": 0, "lines_added": 0}));
    let test_results_json = serde_json::to_value(&test_results).unwrap_or(serde_json::json!({}));

    let row = pg
        .insert_review(&InsertReviewInput {
            task_id,
            reviewer_session_id,
            reviewed_session_id: &reviewed_session_id,
            verdict: &verdict.verdict,
            confidence: verdict.confidence,
            reasoning: &verdict.reasoning,
            diff_summary: Some(diff_summary_json),
            test_results: Some(test_results_json),
        })
        .await
        .map_err(|e| ReviewError::Db(format!("{}", e)))?;

    Ok(ReviewResult {
        review_id: row.id,
        verdict: row.verdict,
        confidence: row.confidence,
    })
}

/// Persist a stub `escalate / confidence=0` review when the LLM provider
/// is unavailable. The Tauri-command path uses this so the dashboard
/// shows the queued review (per Phase 4 spec: "Insert a stub reviews row
/// … so the dashboard surfaces it.").
///
/// Rejects self-review the same way the live path does (returns Db error).
pub async fn insert_disabled_stub_review(
    pg: &Arc<PgDb>,
    task_id: &str,
    reviewer_session_id: &str,
    reviewed_session_id: &str,
) -> Result<ReviewResult, ReviewError> {
    let row = pg
        .insert_review(&InsertReviewInput {
            task_id,
            reviewer_session_id,
            reviewed_session_id,
            verdict: "escalate",
            confidence: 0.0,
            reasoning: "awaiting LLM provider — configure an LLM in Settings → AI to enable auto-review",
            diff_summary: None,
            test_results: None,
        })
        .await
        .map_err(|e| ReviewError::Db(format!("{}", e)))?;

    Ok(ReviewResult {
        review_id: row.id,
        verdict: row.verdict,
        confidence: row.confidence,
    })
}

// =============================================================================
// Plan-hash + transcript helpers
// =============================================================================

/// SHA-256 of the plan markdown's bytes, hex-encoded. Mirrors the hash
/// scheme the `/decompose-plan` slash command writes to
/// `tasks.plan_version_hash` so this comparison is meaningful.
fn compute_plan_hash(markdown_path: &str) -> Result<String, String> {
    let bytes = std::fs::read(markdown_path)
        .map_err(|e| format!("failed to read plan {}: {}", markdown_path, e))?;
    let mut h = Sha256::new();
    h.update(&bytes);
    Ok(hex_encode(&h.finalize()))
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

/// Pull the worker's `ai_output` events for the session, oldest-first, and
/// concatenate them into a single transcript string. The slash command
/// reads the same data via `GET /sessions/<id>/transcript`.
async fn fetch_session_transcript(
    pg: &Arc<PgDb>,
    reviewed_session_id: &str,
) -> Result<String, String> {
    // No limit — we'll truncate at the char level after concatenation.
    let events = pg
        .get_task_run_events(reviewed_session_id, Some("ai_output"), None)
        .await?;

    let mut out = String::new();
    for ev in events {
        if !out.is_empty() {
            out.push_str("\n---\n");
        }
        out.push_str(&ev.message);
    }
    Ok(out)
}

/// Take the last `max_chars` characters of `s` (char-aligned, not byte-
/// aligned, so we don't slice mid-codepoint). Returns the input verbatim
/// when shorter.
fn tail_chars(s: &str, max_chars: usize) -> String {
    let total = s.chars().count();
    if total <= max_chars {
        return s.to_string();
    }
    let skip = total - max_chars;
    let mut out = String::with_capacity(max_chars + 8);
    out.push_str("[…truncated…]\n");
    for (i, c) in s.chars().enumerate() {
        if i >= skip {
            out.push(c);
        }
    }
    out
}

// =============================================================================
// Diff inspection
// =============================================================================

/// Compact summary of `git diff` against the worker repo, scoped to the
/// task's `expected_file_claims`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct DiffSummary {
    pub files_changed: u32,
    pub lines_added: u32,
    pub lines_removed: u32,
    /// Best-effort capture of the working-tree diff scoped to claims. Truncated
    /// for the LLM — full text lives in git history. Not stored in the JSON
    /// blob written to `reviews.diff_summary` (kept as private context for
    /// the LLM only) — the JSON shape stays {files_changed, lines_added,
    /// lines_removed}.
    #[serde(skip)]
    pub diff_text: String,
}

async fn inspect_diff(repo_path: &str, expected_claims: &[String]) -> DiffSummary {
    let stat_text = run_git(repo_path, &["diff", "--stat"]).await.unwrap_or_default();
    let (files_changed, lines_added, lines_removed) = parse_diff_stat(&stat_text);

    let mut diff_text = String::new();
    if !expected_claims.is_empty() {
        // `git diff -- <paths…>` to scope the body to the claimed files.
        let mut args: Vec<&str> = vec!["diff", "--"];
        for p in expected_claims {
            args.push(p.as_str());
        }
        if let Ok(scoped) = run_git(repo_path, &args).await {
            diff_text = tail_chars(&scoped, 12_000);
        }
    }
    if diff_text.is_empty() {
        // Fallback: full stat is enough context if no claims were listed.
        diff_text = stat_text;
    }

    DiffSummary {
        files_changed,
        lines_added,
        lines_removed,
        diff_text,
    }
}

/// Parse `git diff --stat`'s tail line: `" 4 files changed, 120 insertions(+), 30 deletions(-)"`.
/// Returns zeros on a parse failure (no diff or unrecognised format).
fn parse_diff_stat(text: &str) -> (u32, u32, u32) {
    let last = text.lines().last().unwrap_or("").trim();
    let parts: Vec<&str> = last.split(',').map(|s| s.trim()).collect();
    let mut files = 0u32;
    let mut added = 0u32;
    let mut removed = 0u32;
    for p in parts {
        let leading_num: String = p.chars().take_while(|c| c.is_ascii_digit()).collect();
        let n: u32 = leading_num.parse().unwrap_or(0);
        if p.contains("file") {
            files = n;
        } else if p.contains("insertion") {
            added = n;
        } else if p.contains("deletion") {
            removed = n;
        }
    }
    (files, added, removed)
}

async fn run_git(repo_path: &str, args: &[&str]) -> Result<String, String> {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(repo_path)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    // Run with a 30s wall-clock cap — diffs should complete in <1s on
    // anything sane; 30s prevents a runaway hang from blocking the reviewer.
    let fut = cmd.output();
    let output = match timeout(Duration::from_secs(30), fut).await {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => return Err(format!("git spawn failed: {}", e)),
        Err(_) => return Err("git timeout".to_string()),
    };

    if !output.status.success() {
        return Err(format!(
            "git exit {}: {}",
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Files touched by the worker that are NOT in `expected_file_claims`.
/// Used as "scope creep" hints for the LLM. Comparison is done on the
/// path string verbatim (no normalization) — claims lists in
/// `coord.tasks` are stored canonically by `/decompose-plan`.
fn scope_creep_candidates(touched: &[String], claims: &[String]) -> Vec<String> {
    use std::collections::HashSet;
    let claim_set: HashSet<&str> = claims.iter().map(|s| s.as_str()).collect();
    touched
        .iter()
        .filter(|p| !claim_set.contains(p.as_str()))
        .cloned()
        .collect()
}

/// Resolve `<workspace_root>/qontinui-runner` for git/cargo/tsc invocations.
/// Mirrors the helper in `commands/productivity.rs` but avoids importing
/// across module boundaries — same logic, lifted here so this module is
/// self-contained.
fn resolve_runner_repo_path() -> Option<String> {
    let workspace_root = crate::mcp::shared::current_project_path()?;
    let repo = std::path::Path::new(&workspace_root).join("qontinui-runner");
    if repo.is_dir() {
        Some(repo.to_string_lossy().to_string())
    } else {
        Some(workspace_root)
    }
}

// =============================================================================
// Test runs
// =============================================================================

/// Bundle of per-tool test outcomes. Serialized into `reviews.test_results`
/// as a flat JSON object so the existing dashboard can render it without
/// schema changes. Empty surfaces collapse to absent keys.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct TestResults {
    /// One of `"ok"`, `"failed"`, `"timeout"`, `"skipped"`, or
    /// `"not_run: <reason>"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cargo_check: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cargo_check_tail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tsc_no_emit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tsc_tail: Option<String>,
    /// Surfaces we deliberately didn't try (e.g. Python tests).
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub skipped: Vec<String>,
}

/// Run cargo-check and/or tsc based on which surfaces the worker touched.
/// Other surfaces (Python, multistate) get a "tests not run: unfamiliar
/// surface" note rather than fabricating green lights — same rule as the
/// slash command's §4.
async fn run_relevant_tests(repo_path: &str, touched: &[String]) -> TestResults {
    let mut out = TestResults::default();

    let touches_rust = touched
        .iter()
        .any(|p| p.contains("qontinui-runner/src-tauri/") || p.ends_with(".rs"));
    let touches_ts = touched.iter().any(|p| {
        let lo = p.to_ascii_lowercase();
        lo.ends_with(".ts") || lo.ends_with(".tsx") || lo.ends_with(".js") || lo.ends_with(".jsx")
    });
    let touches_py = touched.iter().any(|p| p.ends_with(".py"));

    if touches_rust {
        let manifest = PathBuf::from(repo_path).join("src-tauri").join("Cargo.toml");
        match run_command_with_timeout(
            "cargo",
            &[
                "check",
                "--manifest-path",
                manifest.to_string_lossy().as_ref(),
            ],
            repo_path,
            TEST_COMMAND_TIMEOUT_SECS,
        )
        .await
        {
            CommandOutcome::Success(s) => {
                out.cargo_check = Some("ok".to_string());
                out.cargo_check_tail = Some(tail_chars(&s, TEST_OUTPUT_TAIL_CHARS));
            }
            CommandOutcome::Failure(code, s) => {
                out.cargo_check = Some(format!("failed (exit {})", code));
                out.cargo_check_tail = Some(tail_chars(&s, TEST_OUTPUT_TAIL_CHARS));
            }
            CommandOutcome::Timeout => {
                out.cargo_check = Some("timeout".to_string());
            }
            CommandOutcome::SpawnError(e) => {
                out.cargo_check = Some(format!("spawn-error: {}", e));
            }
        }
    }

    if touches_ts {
        // Run `npx tsc --noEmit` from the runner repo root. We rely on
        // PATH-resolved npx; if it's missing the spawn-error path catches it.
        match run_command_with_timeout(
            "npx",
            &["tsc", "--noEmit"],
            repo_path,
            TEST_COMMAND_TIMEOUT_SECS,
        )
        .await
        {
            CommandOutcome::Success(s) => {
                out.tsc_no_emit = Some("ok".to_string());
                out.tsc_tail = Some(tail_chars(&s, TEST_OUTPUT_TAIL_CHARS));
            }
            CommandOutcome::Failure(code, s) => {
                out.tsc_no_emit = Some(format!("failed (exit {})", code));
                out.tsc_tail = Some(tail_chars(&s, TEST_OUTPUT_TAIL_CHARS));
            }
            CommandOutcome::Timeout => {
                out.tsc_no_emit = Some("timeout".to_string());
            }
            CommandOutcome::SpawnError(e) => {
                out.tsc_no_emit = Some(format!("spawn-error: {}", e));
            }
        }
    }

    if touches_py {
        out.skipped
            .push("python: tests not run (unfamiliar surface)".to_string());
    }

    out
}

/// Outcome of a bounded subprocess invocation. `Failure(code, stderr+stdout)`
/// reports the exit code separately so the caller doesn't have to parse it
/// out of the captured text — see memory feedback `pipe_tail_masks_exit_codes`.
enum CommandOutcome {
    Success(String),
    Failure(i32, String),
    Timeout,
    SpawnError(String),
}

async fn run_command_with_timeout(
    program: &str,
    args: &[&str],
    cwd: &str,
    timeout_secs: u64,
) -> CommandOutcome {
    let mut cmd = Command::new(program);
    cmd.args(args)
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    debug!(
        "auto_review.run_command: program={} args={:?} cwd={} timeout={}s",
        program, args, cwd, timeout_secs
    );

    let fut = cmd.output();
    match timeout(Duration::from_secs(timeout_secs), fut).await {
        Ok(Ok(output)) => {
            let mut text = String::new();
            text.push_str(&String::from_utf8_lossy(&output.stdout));
            if !output.stderr.is_empty() {
                text.push_str("\n--- stderr ---\n");
                text.push_str(&String::from_utf8_lossy(&output.stderr));
            }
            if output.status.success() {
                CommandOutcome::Success(text)
            } else {
                CommandOutcome::Failure(output.status.code().unwrap_or(-1), text)
            }
        }
        Ok(Err(e)) => CommandOutcome::SpawnError(format!("{}", e)),
        Err(_) => CommandOutcome::Timeout,
    }
}

// =============================================================================
// LLM prompt assembly
// =============================================================================

fn build_user_prompt(
    description: &str,
    expected_claims: &[String],
    transcript: &str,
    diff_summary: &DiffSummary,
    scoped_diff_text: &str,
    scope_creep: &[String],
    test_results: &TestResults,
) -> String {
    let mut s = String::with_capacity(8192);
    s.push_str("# Task description\n\n");
    s.push_str(description);
    s.push_str("\n\n# Expected file claims\n\n");
    if expected_claims.is_empty() {
        s.push_str("(none declared)\n");
    } else {
        for p in expected_claims {
            s.push_str("- ");
            s.push_str(p);
            s.push('\n');
        }
    }

    s.push_str("\n# Diff stat\n\n");
    s.push_str(&format!(
        "files_changed={} lines_added={} lines_removed={}\n",
        diff_summary.files_changed, diff_summary.lines_added, diff_summary.lines_removed
    ));

    if !scoped_diff_text.is_empty() {
        s.push_str("\n# Diff (scoped to expected claims)\n\n```diff\n");
        s.push_str(scoped_diff_text);
        s.push_str("\n```\n");
    }

    if !scope_creep.is_empty() {
        s.push_str("\n# Scope-creep candidates (touched outside expected claims)\n\n");
        for p in scope_creep {
            s.push_str("- ");
            s.push_str(p);
            s.push('\n');
        }
    }

    s.push_str("\n# Test results\n\n");
    s.push_str(&format_test_results_for_prompt(test_results));

    s.push_str("\n# Worker transcript (tail, truncated)\n\n");
    if transcript.is_empty() {
        s.push_str("(no transcript captured for this session)\n");
    } else {
        s.push_str(transcript);
    }

    s.push_str(
        "\n\n# Verdict\n\nEmit a single AutoReviewVerdict JSON object. \
         Cite file:line for any defects you call out.\n",
    );
    s
}

fn format_test_results_for_prompt(t: &TestResults) -> String {
    let mut s = String::new();
    if let Some(c) = &t.cargo_check {
        s.push_str(&format!("- cargo check: {}\n", c));
    }
    if let Some(c) = &t.tsc_no_emit {
        s.push_str(&format!("- tsc --noEmit: {}\n", c));
    }
    for line in &t.skipped {
        s.push_str(&format!("- {}\n", line));
    }
    if s.is_empty() {
        s.push_str("(no relevant test surface ran)\n");
    }
    s
}

// =============================================================================
// Verdict normalization
// =============================================================================

/// Normalize the LLM's free-form verdict to the canonical strings
/// (`approved` | `needs_fix` | `escalate`) and clamp confidence to
/// `[0.0, 1.0]`. The `/reviews` insert validator rejects anything else
/// with HTTP 400; doing it here avoids a wasted round-trip when the
/// model emits `"APPROVED"` or `"escalate_to_user"`.
fn normalize_verdict(v: AutoReviewVerdict) -> AutoReviewVerdict {
    let normalized_verdict = match v.verdict.trim().to_ascii_lowercase().as_str() {
        "approved" | "approve" | "ok" | "pass" => "approved".to_string(),
        "needs_fix" | "needs-fix" | "needsfix" | "fix" | "reject" | "rejected" => {
            "needs_fix".to_string()
        }
        "escalate" | "escalate_to_user" | "escalation" | "user" => "escalate".to_string(),
        // Unknown verdict → escalate (the safe fallback per the slash
        // command's "don't fabricate" rule).
        _ => {
            warn!(
                "auto_review: unknown LLM verdict {:?}, downgrading to escalate",
                v.verdict
            );
            "escalate".to_string()
        }
    };

    let confidence = if !v.confidence.is_finite() {
        0.0
    } else {
        v.confidence.clamp(0.0, 1.0)
    };

    AutoReviewVerdict {
        verdict: normalized_verdict,
        confidence,
        reasoning: if v.reasoning.trim().is_empty() {
            "(reviewer returned empty reasoning)".to_string()
        } else {
            v.reasoning
        },
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // --- normalize_verdict --------------------------------------------------

    #[test]
    fn normalize_verdict_canonicalizes_known_strings() {
        let v = AutoReviewVerdict {
            verdict: "APPROVED".into(),
            confidence: 0.91,
            reasoning: "looks good".into(),
        };
        let out = normalize_verdict(v);
        assert_eq!(out.verdict, "approved");
        assert_eq!(out.confidence, 0.91);
    }

    #[test]
    fn normalize_verdict_maps_aliases() {
        for (input, expected) in [
            ("approve", "approved"),
            ("needs-fix", "needs_fix"),
            ("escalate_to_user", "escalate"),
            ("reject", "needs_fix"),
            ("OK", "approved"),
        ] {
            let v = AutoReviewVerdict {
                verdict: input.to_string(),
                confidence: 0.5,
                reasoning: "x".into(),
            };
            assert_eq!(normalize_verdict(v).verdict, expected, "{}", input);
        }
    }

    #[test]
    fn normalize_verdict_unknown_falls_back_to_escalate() {
        let v = AutoReviewVerdict {
            verdict: "wat".into(),
            confidence: 0.7,
            reasoning: "x".into(),
        };
        assert_eq!(normalize_verdict(v).verdict, "escalate");
    }

    #[test]
    fn normalize_verdict_clamps_confidence() {
        let v = AutoReviewVerdict {
            verdict: "approved".into(),
            confidence: 5.0,
            reasoning: "x".into(),
        };
        assert_eq!(normalize_verdict(v).confidence, 1.0);

        let v = AutoReviewVerdict {
            verdict: "approved".into(),
            confidence: -0.5,
            reasoning: "x".into(),
        };
        assert_eq!(normalize_verdict(v).confidence, 0.0);

        let v = AutoReviewVerdict {
            verdict: "approved".into(),
            confidence: f64::NAN,
            reasoning: "x".into(),
        };
        assert_eq!(normalize_verdict(v).confidence, 0.0);
    }

    #[test]
    fn normalize_verdict_replaces_empty_reasoning() {
        let v = AutoReviewVerdict {
            verdict: "approved".into(),
            confidence: 0.9,
            reasoning: "   ".into(),
        };
        let out = normalize_verdict(v);
        assert!(out.reasoning.contains("empty reasoning"));
    }

    // --- parse_diff_stat ----------------------------------------------------

    #[test]
    fn parse_diff_stat_typical_line() {
        let txt = "src/foo.rs | 2 +-\n 4 files changed, 120 insertions(+), 30 deletions(-)";
        assert_eq!(parse_diff_stat(txt), (4, 120, 30));
    }

    #[test]
    fn parse_diff_stat_singular_units() {
        let txt = " 1 file changed, 1 insertion(+), 1 deletion(-)";
        assert_eq!(parse_diff_stat(txt), (1, 1, 1));
    }

    #[test]
    fn parse_diff_stat_empty_diff() {
        assert_eq!(parse_diff_stat(""), (0, 0, 0));
    }

    // --- tail_chars ---------------------------------------------------------

    #[test]
    fn tail_chars_keeps_short_strings_verbatim() {
        assert_eq!(tail_chars("hi", 100), "hi");
    }

    #[test]
    fn tail_chars_truncates_with_marker() {
        let s: String = std::iter::repeat('x').take(500).collect();
        let out = tail_chars(&s, 100);
        assert!(out.contains("[…truncated…]"));
        // Marker length + 100 chars, give or take.
        let payload: String = out.chars().filter(|c| *c == 'x').collect();
        assert_eq!(payload.len(), 100);
    }

    #[test]
    fn tail_chars_is_codepoint_safe() {
        // 5 × 4-byte codepoint
        let s: String = "🦀".repeat(5);
        let out = tail_chars(&s, 3);
        // Should keep the last 3 crab emoji
        let crabs: usize = out.chars().filter(|c| *c == '🦀').count();
        assert_eq!(crabs, 3);
    }

    // --- scope_creep_candidates --------------------------------------------

    #[test]
    fn scope_creep_returns_unclaimed_paths() {
        let touched = vec![
            "a.rs".to_string(),
            "b.rs".to_string(),
            "c.rs".to_string(),
        ];
        let claims = vec!["a.rs".to_string(), "b.rs".to_string()];
        let creep = scope_creep_candidates(&touched, &claims);
        assert_eq!(creep, vec!["c.rs".to_string()]);
    }

    #[test]
    fn scope_creep_empty_when_all_claimed() {
        let touched = vec!["a.rs".to_string()];
        let claims = vec!["a.rs".to_string(), "b.rs".to_string()];
        assert!(scope_creep_candidates(&touched, &claims).is_empty());
    }

    // --- compute_plan_hash --------------------------------------------------

    #[test]
    fn plan_hash_is_deterministic() {
        let mut p = std::env::temp_dir();
        p.push(format!("auto_review_test_plan_{}.md", uuid::Uuid::new_v4()));
        std::fs::write(&p, b"# plan body\n\n- step 1\n").expect("write tmp plan");
        let h1 = compute_plan_hash(p.to_str().unwrap()).expect("hash");
        let h2 = compute_plan_hash(p.to_str().unwrap()).expect("hash");
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 64); // sha256 hex
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn plan_hash_differs_on_content_change() {
        let mut p = std::env::temp_dir();
        p.push(format!("auto_review_test_plan_{}.md", uuid::Uuid::new_v4()));
        std::fs::write(&p, b"v1").expect("write");
        let h1 = compute_plan_hash(p.to_str().unwrap()).expect("hash");
        std::fs::write(&p, b"v2").expect("write");
        let h2 = compute_plan_hash(p.to_str().unwrap()).expect("hash");
        assert_ne!(h1, h2);
        let _ = std::fs::remove_file(&p);
    }

    // --- TestResults serde --------------------------------------------------

    #[test]
    fn test_results_serializes_compactly() {
        let t = TestResults {
            cargo_check: Some("ok".to_string()),
            cargo_check_tail: Some("Compiling…".to_string()),
            ..Default::default()
        };
        let v = serde_json::to_value(&t).unwrap();
        assert_eq!(v["cargo_check"], "ok");
        // None fields skipped, empty Vec skipped.
        assert!(v.get("tsc_no_emit").is_none());
        assert!(v.get("skipped").is_none());
    }

    // --- run_command_with_timeout — timeout path ---------------------------
    //
    // Verifies the timeout branch by spawning a sleep that exceeds the cap.
    // Skipped on Windows where `sleep` isn't on PATH by default — the
    // test relies on the timeout BRANCH firing, not the sleep itself.

    #[cfg(not(windows))]
    #[tokio::test]
    async fn run_command_with_timeout_returns_timeout_for_long_running_cmd() {
        let outcome = run_command_with_timeout("sleep", &["10"], ".", 1).await;
        match outcome {
            CommandOutcome::Timeout => {}
            other => panic!("expected Timeout, got {:?}", outcome_kind(&other)),
        }
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn run_command_with_timeout_returns_timeout_for_long_running_cmd() {
        // On Windows we use PowerShell `Start-Sleep` to block for longer
        // than the test's wall-clock cap. PowerShell is universally
        // available on supported Windows versions; `cmd /c timeout` is
        // not reliable in non-interactive contexts (it exits with an
        // error rather than blocking).
        //
        // The test cap is 2s; the inner command sleeps 30s. The
        // `kill_on_drop` setting on the Command ensures the child
        // doesn't outlive the test.
        let outcome = run_command_with_timeout(
            "powershell",
            &["-NoProfile", "-Command", "Start-Sleep", "-Seconds", "30"],
            ".",
            2,
        )
        .await;
        match outcome {
            CommandOutcome::Timeout | CommandOutcome::SpawnError(_) => {
                // Either Timeout (expected) or SpawnError on minimal CI
                // images that lack `powershell` on PATH. Both are
                // non-fatal — the assertion is "we don't hang past the
                // deadline".
            }
            other => panic!("expected Timeout/SpawnError, got {:?}", outcome_kind(&other)),
        }
    }

    fn outcome_kind(o: &CommandOutcome) -> &'static str {
        match o {
            CommandOutcome::Success(_) => "success",
            CommandOutcome::Failure(_, _) => "failure",
            CommandOutcome::Timeout => "timeout",
            CommandOutcome::SpawnError(_) => "spawn-error",
        }
    }

    // --- LlmDisabled stub-row path -----------------------------------------
    //
    // The stub-row path is exercised end-to-end in the integration suite
    // (which has a live PG); here we cover the InsertReviewInput shape so
    // a future schema change to `reviews.reasoning` etc. surfaces as a
    // compile-time error in this test alongside the call site.

    #[test]
    fn stub_review_input_shape_compiles() {
        let _input = InsertReviewInput {
            task_id: "t",
            reviewer_session_id: "r",
            reviewed_session_id: "w",
            verdict: "escalate",
            confidence: 0.0,
            reasoning: "awaiting LLM provider",
            diff_summary: None,
            test_results: None,
        };
    }

    // --- Plan-version-mismatch guard ---------------------------------------

    #[test]
    fn plan_version_mismatch_constructs_with_distinct_hashes() {
        let err = ReviewError::PlanVersionMismatch {
            task_id: "t1".to_string(),
            expected: "abc".to_string(),
            actual: "def".to_string(),
        };
        let s = format!("{}", err);
        assert!(s.contains("plan-version mismatch"));
        assert!(s.contains("abc"));
        assert!(s.contains("def"));
    }

    #[test]
    fn review_error_display_messages_are_distinct() {
        let cases = [
            ReviewError::TaskNotFound("t".into()),
            ReviewError::NoAssignedSession("t".into()),
            ReviewError::PlanVersionMismatch {
                task_id: "t".into(),
                expected: "a".into(),
                actual: "b".into(),
            },
            ReviewError::LlmDisabled("x".into()),
            ReviewError::LlmCallFailed("x".into()),
            ReviewError::Db("x".into()),
            ReviewError::Io("x".into()),
        ];
        let strings: Vec<String> = cases.iter().map(|e| format!("{}", e)).collect();
        let mut sorted = strings.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), 7, "{:?}", strings);
    }
}

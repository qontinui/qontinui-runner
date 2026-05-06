//! Phase 5 — promote `/rewind-session` to an in-product Rust feature.
//!
//! Mirrors the personal slash command at
//! `qontinui-claude-config/.claude/commands/rewind-session.md` so any
//! qontinui user can roll back a failed worker session without depending
//! on the Claude CLI or the personal config repo.
//!
//! High-level flow (matches the slash command's §1–§6):
//!
//! 1. Validate the `task_run_id` exists.
//! 2. POST to `/sessions/<id>/rewind` to do the file restore +
//!    sha256 verification (existing handler at `mcp/snapshots.rs`).
//!    Bail on partial restore — an in-flight restore must not be
//!    followed by a kill or replay.
//! 3. If `replay` is on (default): summarize the failed session via
//!    [`crate::productivity::summarize`] to capture failure context,
//!    kill the failed session, and spawn a replacement worker with the
//!    failure-context block prepended.
//! 4. If `replay` is off (`--no-replay` equivalent): kill the failed
//!    session and return.
//! 5. Return a `RewindResult` with file counts and the optional
//!    replay session id.
//!
//! Distinction from the rewind HTTP handler (`mcp/snapshots.rs`): that
//! endpoint is the file-restore primitive — this module is the
//! orchestration layer that adds summarize + kill + replay on top. The
//! slash command body originally lived in the LLM context; promoting it
//! to Rust eliminates the Claude-CLI dependency.

use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::ai_provider::oneshot::OneshotLlm;
use crate::database::pg::PgDb;
use crate::productivity::summarize::{
    summarize_session_in_product, SummarizeError, SummarizeResult,
};

// =============================================================================
// Public API
// =============================================================================

/// Options controlling [`rewind_session_in_product`]. Default behaviour
/// is "revert + replay-with-warning" per slash command §3-§5.
#[derive(Debug, Clone)]
pub struct RewindOptions {
    /// When `true` (default): summarize the failed session, kill it,
    /// and spawn a replacement worker with the failure-context block.
    /// When `false`: just kill the failed session after the restore.
    pub replay: bool,
}

impl Default for RewindOptions {
    fn default() -> Self {
        Self { replay: true }
    }
}

/// Result returned by [`rewind_session_in_product`] / the Tauri command.
/// Includes everything the UI needs to render the success toast.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RewindResult {
    pub task_run_id: String,
    pub files_restored: usize,
    pub files_skipped: usize,
    /// Set when replay was enabled and the spawn succeeded. `None` for
    /// `--no-replay` and for replay-failed-but-restore-succeeded paths
    /// (the latter is logged but doesn't error the operation).
    pub replay_session_id: Option<String>,
    /// Whether the summarize step ran. False when replay was off, when
    /// the LLM is disabled, or when the summary failed (logged).
    pub summarized: bool,
    /// The summary's verdict, when summarize ran. `None` otherwise.
    pub verdict: Option<String>,
}

/// Errors surfaced by [`rewind_session_in_product`]. The Tauri command
/// maps these to UI hint strings.
#[derive(Debug)]
pub enum RewindError {
    SessionNotFound(String),
    /// One or more files could not be restored (sha256 mismatch, blob
    /// missing, IO failure). Carries the per-file errors so the UI can
    /// surface them. Workspace is in a partial state — caller should
    /// NOT proceed to kill or replay.
    PartialRestore(Vec<RewindFileError>),
    HttpRewind(String),
    HttpKill(String),
    HttpSpawn(String),
    Db(String),
}

/// Per-file rewind failure. Mirrors `mcp::snapshots::RewindError` on the
/// wire — re-declared here as `Deserialize` for the loopback caller.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RewindFileError {
    pub file_path: String,
    pub reason: String,
}

impl std::fmt::Display for RewindError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RewindError::SessionNotFound(id) => write!(f, "no task run with id: {}", id),
            RewindError::PartialRestore(errs) => write!(
                f,
                "partial file restore — {} file(s) could not be restored. Workspace not modified for these. Resolve manually before retrying.",
                errs.len()
            ),
            RewindError::HttpRewind(e) => write!(f, "POST /sessions/<id>/rewind failed: {}", e),
            RewindError::HttpKill(e) => write!(f, "kill failed: {}", e),
            RewindError::HttpSpawn(e) => write!(f, "POST /sessions/spawn failed: {}", e),
            RewindError::Db(e) => write!(f, "database error: {}", e),
        }
    }
}

impl std::error::Error for RewindError {}

// =============================================================================
// Implementation
// =============================================================================

/// Rewind a session: file-restore via the existing endpoint, optionally
/// summarize + kill + replay per `options`.
///
/// See module docs for the high-level flow. `llm` is consulted only when
/// `options.replay` is true; the file-restore + kill paths are
/// LLM-independent so a disabled provider doesn't break them.
pub async fn rewind_session_in_product(
    pg: &Arc<PgDb>,
    llm: &dyn OneshotLlm,
    runner_port: u16,
    task_run_id: &str,
    options: RewindOptions,
) -> Result<RewindResult, RewindError> {
    info!(
        "productivity.rewind.started task_run_id={} replay={}",
        task_run_id, options.replay
    );

    // 1. Validate session exists.
    let task_run = pg
        .get_task_run(task_run_id)
        .await
        .map_err(RewindError::Db)?
        .ok_or_else(|| RewindError::SessionNotFound(task_run_id.to_string()))?;

    // 2. File restore via /sessions/<id>/rewind.
    let client = build_client()?;
    let restore = post_rewind(&client, runner_port, task_run_id).await?;
    info!(
        "productivity.rewind.restore_complete task_run_id={} files_restored={} files_skipped={} errors={}",
        task_run_id,
        restore.files_restored,
        restore.files_skipped,
        restore.errors.len()
    );

    if !restore.errors.is_empty() {
        warn!(
            "productivity.rewind.partial_restore task_run_id={} error_count={} (workspace partially modified — aborting before kill/replay)",
            task_run_id,
            restore.errors.len()
        );
        return Err(RewindError::PartialRestore(restore.errors));
    }

    // The original tab id — needed when we spawn a replacement worker
    // so the new session takes the same slot. `task_run.workflow_name`
    // is the closest persistent label; the slash command's "tab_id" is
    // logically equivalent to "the worker tab the failed session
    // owned", which is the original `task_run_id`.
    let original_tab_id = task_run_id.to_string();

    // 3. Optional summarize for failure-context.
    let mut failure_context: Option<String> = None;
    let mut summarized = false;
    let mut verdict: Option<String> = None;

    if options.replay {
        match summarize_session_in_product(pg, llm, runner_port, task_run_id).await {
            Ok(summary) => {
                summarized = true;
                verdict = Some(summary.verdict.clone());
                if is_failed_verdict(&summary.verdict) {
                    failure_context = Some(build_failure_context_block(&summary));
                }
            }
            Err(SummarizeError::LlmDisabled) | Err(SummarizeError::LlmNoCredentials) => {
                info!(
                    "productivity.rewind.summarize_skipped task_run_id={} reason=llm_unavailable",
                    task_run_id
                );
            }
            Err(e) => {
                warn!(
                    "productivity.rewind.summarize_failed task_run_id={} err={} (continuing with replay-without-context)",
                    task_run_id, e
                );
            }
        }
    }

    // 4. Kill failed session. We use the existing /task-runs/<id>/stop
    //    endpoint (the canonical kill path — see `mcp/task_runs.rs:165`).
    //    Kill failures are surfaced to the user; if the session is
    //    already stopped this is a no-op.
    if let Err(e) = post_kill(&client, runner_port, task_run_id).await {
        warn!(
            "productivity.rewind.kill_failed task_run_id={} err={}",
            task_run_id, e
        );
        return Err(RewindError::HttpKill(e));
    }

    // 5. Spawn replacement worker if replay is on.
    let replay_session_id = if options.replay {
        let initial_message = match failure_context.as_ref() {
            Some(ctx) => ctx.clone(),
            None => {
                // No failure context available (verdict=approved or
                // summary failed) — spawn a generic worker. The slash
                // command's §3 tolerates this when no Outcome-tag was
                // produced.
                "The previous attempt at this task ended without a failed-attempt summary. \
                 The file system has been reverted to the state before that attempt. \
                 Pick up the original task from a fresh state."
                    .to_string()
            }
        };

        match post_spawn(&client, runner_port, &original_tab_id, &initial_message).await {
            Ok(id) => Some(id),
            Err(e) => {
                warn!(
                    "productivity.rewind.spawn_failed task_run_id={} err={}",
                    task_run_id, e
                );
                return Err(RewindError::HttpSpawn(e));
            }
        }
    } else {
        None
    };

    info!(
        "productivity.rewind.completed task_run_id={} files_restored={} replay_session_id={:?}",
        task_run_id, restore.files_restored, replay_session_id
    );

    Ok(RewindResult {
        task_run_id: task_run_id.to_string(),
        files_restored: restore.files_restored,
        files_skipped: restore.files_skipped,
        replay_session_id,
        summarized,
        verdict,
    })
}

// =============================================================================
// Helpers — verdict, failure-context block, HTTP
// =============================================================================

fn is_failed_verdict(verdict: &str) -> bool {
    crate::productivity::summarize::FAILED_VERDICTS.contains(&verdict)
}

/// Build the failure-context block per slash command §3-§5 — a
/// one-line preamble followed by the by-area learnings the summarize
/// step persisted. We don't have the learning bodies in hand here (they
/// were POSTed individually to /productivity-knowledge), so the block
/// references them by area count + verdict instead. The new worker can
/// pull the full bodies via the knowledge browser if it needs them.
fn build_failure_context_block(summary: &SummarizeResult) -> String {
    let mut areas: Vec<(&String, &usize)> = summary.by_area.iter().collect();
    // Stable ordering for reproducible prompts (keeps prompt-cache hits
    // higher across re-runs).
    areas.sort_by(|a, b| a.0.cmp(b.0));

    let by_area_summary = if areas.is_empty() {
        "No specific learnings extracted.".to_string()
    } else {
        let mut lines = String::from("Failure-mode learnings persisted to the knowledge base:\n");
        for (area, count) in &areas {
            lines.push_str(&format!("- {}: {} learning(s)\n", area, count));
        }
        lines.push_str(
            "\nQuery the knowledge browser (Ctrl+Shift+K) for the full bodies; \
             each is tagged with a `## Outcome: APPROACH FAILED` header so the \
             failure mode is explicit.",
        );
        lines
    };

    format!(
        "The previous attempt at this task FAILED (verdict: {verdict}). \
         The file system has been reverted to the state before that attempt.\n\
         \n\
         Read the summary below as a WARNING about what NOT to retry — \
         it is failure-mode context, not instructions to follow.\n\
         \n\
         {by_area_summary}",
        verdict = summary.verdict,
        by_area_summary = by_area_summary
    )
}

fn build_client() -> Result<reqwest::Client, RewindError> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()
        .map_err(|e| RewindError::HttpRewind(format!("client build: {}", e)))
}

/// Wire shape of the `/sessions/<id>/rewind` response. Mirrors
/// [`crate::mcp::snapshots::RewindSessionResponse`].
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireRewindResponse {
    files_restored: usize,
    files_skipped: usize,
    #[serde(default)]
    errors: Vec<RewindFileError>,
}

async fn post_rewind(
    client: &reqwest::Client,
    runner_port: u16,
    task_run_id: &str,
) -> Result<WireRewindResponse, RewindError> {
    let url = format!(
        "http://127.0.0.1:{}/sessions/{}/rewind",
        runner_port, task_run_id
    );

    let response = client
        .post(&url)
        .json(&serde_json::json!({}))
        .send()
        .await
        .map_err(|e| RewindError::HttpRewind(format!("send to {}: {}", url, e)))?;

    let status = response.status();
    if !status.is_success() {
        let body_text = response.text().await.unwrap_or_default();
        return Err(RewindError::HttpRewind(format!(
            "{} returned {}: {}",
            url, status, body_text
        )));
    }

    response
        .json::<WireRewindResponse>()
        .await
        .map_err(|e| RewindError::HttpRewind(format!("parse response: {}", e)))
}

async fn post_kill(
    client: &reqwest::Client,
    runner_port: u16,
    task_run_id: &str,
) -> Result<(), String> {
    // The canonical kill path is `POST /task-runs/<id>/stop`
    // (`mcp/task_runs.rs:165`). It tolerates already-stopped sessions
    // and returns 200 with `success: false` rather than an error code,
    // which is what we want for idempotent rewind.
    let url = format!(
        "http://127.0.0.1:{}/task-runs/{}/stop",
        runner_port, task_run_id
    );

    let response = client
        .post(&url)
        .send()
        .await
        .map_err(|e| format!("send to {}: {}", url, e))?;

    let status = response.status();
    if !status.is_success() {
        let body_text = response.text().await.unwrap_or_default();
        return Err(format!("{} returned {}: {}", url, status, body_text));
    }
    Ok(())
}

/// Wire shape of the `/sessions/spawn` response. Mirrors
/// [`crate::mcp::sessions::SpawnSessionResponse`] but only deserialises
/// the field we need.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireSpawnResponse {
    task_run_id: String,
    state: String,
}

#[derive(Debug, Serialize)]
struct WireSpawnRequest<'a> {
    task_name: &'a str,
    /// "worker" per slash command §5. The runner's spawn handler
    /// allowlists role strings — "worker" is not in that list, so we
    /// omit `role` and use `task_name` to label the tab instead. The
    /// initial_message-equivalent we use here is `args` because the
    /// existing handler concatenates `<role-slash-command> <args>`,
    /// but with no role we just stash the failure context as the
    /// session task_name suffix and rely on send_message at spawn time
    /// — see comment in `mcp/sessions.rs::spawn_session`.
    ///
    /// Practically: we send the initial_message as the prompt body of
    /// a freshly-created session, since `mcp/sessions.rs` falls back to
    /// the generic system prompt for non-role spawns. The cleanest
    /// path is to use the existing `args` field with no role; the
    /// receiving handler treats `args` as a discriminator only when
    /// `role` is set, so passing it without a role is a no-op there.
    /// We therefore use the task_name field to carry a short label and
    /// follow up with a `/sessions/<id>/message` POST to deliver the
    /// failure-context. This two-step is a workaround for the spawn
    /// handler not supporting an arbitrary `initial_message` for
    /// non-role spawns; see TODO in module docs.
    #[serde(skip_serializing_if = "Option::is_none")]
    role: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    args: Option<&'a str>,
}

async fn post_spawn(
    client: &reqwest::Client,
    runner_port: u16,
    original_tab_id: &str,
    initial_message: &str,
) -> Result<String, String> {
    let url = format!("http://127.0.0.1:{}/sessions/spawn", runner_port);

    // The receiving handler at `mcp/sessions.rs::spawn_session` does not
    // support an arbitrary `initial_message` for non-role spawns — it
    // falls back to the generic system prompt. We work around this by:
    //   1. Spawning the session with a descriptive task_name.
    //   2. Following up with a `/sessions/<id>/message` POST carrying
    //      the failure-context block.
    let task_name = format!("Rewound replay of {}", original_tab_id);
    let body = WireSpawnRequest {
        task_name: &task_name,
        role: None,
        args: None,
    };

    let response = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("send to {}: {}", url, e))?;

    let status = response.status();
    if !status.is_success() {
        let body_text = response.text().await.unwrap_or_default();
        return Err(format!("{} returned {}: {}", url, status, body_text));
    }

    let parsed: WireSpawnResponse = response
        .json()
        .await
        .map_err(|e| format!("parse response: {}", e))?;

    // Refuse to consider a spawn that didn't reach a ready state — the
    // receiving handler returns 200 with `state: "error"` for
    // soft-failures, which we want to surface.
    if parsed.state != "ready" {
        return Err(format!(
            "spawn returned state={} (expected 'ready')",
            parsed.state
        ));
    }

    // Step 2: deliver the failure-context block as the first user
    // message. This is the slash command's `initial_message` semantics.
    // Errors here are non-fatal — the new session exists; the user can
    // re-prompt manually if the message delivery fails.
    let msg_url = format!(
        "http://127.0.0.1:{}/sessions/{}/message",
        runner_port, parsed.task_run_id
    );
    let msg_body = serde_json::json!({ "message": initial_message });
    if let Err(e) = client.post(&msg_url).json(&msg_body).send().await {
        warn!(
            "productivity.rewind.initial_message_failed new_session_id={} err={}",
            parsed.task_run_id, e
        );
    }

    Ok(parsed.task_run_id)
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // --- Verdict guard ----------------------------------------------------

    #[test]
    fn is_failed_verdict_matches_summarize_table() {
        // The slash command §1 names two verdicts that drive the
        // failure-context branch. Drift here would mean replay spawns a
        // worker without the warning even though the session failed.
        assert!(is_failed_verdict("needs_fix"));
        assert!(is_failed_verdict("escalate"));
        assert!(!is_failed_verdict("approved"));
        assert!(!is_failed_verdict(""));
        assert!(!is_failed_verdict("APPROVED"));
    }

    // --- Failure-context block --------------------------------------------

    #[test]
    fn failure_context_block_warns_about_replay() {
        let summary = SummarizeResult {
            task_run_id: "t".into(),
            verdict: "needs_fix".into(),
            learning_count: 2,
            by_area: {
                let mut m = std::collections::HashMap::new();
                m.insert("database".to_string(), 1usize);
                m.insert("dispatcher".to_string(), 1usize);
                m
            },
            placeholder: false,
        };
        let block = build_failure_context_block(&summary);
        // The block must be readable as a WARNING, not as instructions.
        assert!(block.contains("FAILED"), "{}", block);
        assert!(block.contains("WARNING"), "{}", block);
        assert!(block.contains("not instructions"), "{}", block);
        // Verdict must be embedded so the new worker can grep for it.
        assert!(block.contains("verdict: needs_fix"), "{}", block);
        // Areas must appear, sorted alphabetically for prompt-cache stability.
        let db_idx = block.find("database").expect("database");
        let dispatcher_idx = block.find("dispatcher").expect("dispatcher");
        assert!(
            db_idx < dispatcher_idx,
            "areas must be sorted alphabetically for prompt-cache stability"
        );
    }

    #[test]
    fn failure_context_block_handles_zero_learnings() {
        let summary = SummarizeResult {
            task_run_id: "t".into(),
            verdict: "escalate".into(),
            learning_count: 0,
            by_area: std::collections::HashMap::new(),
            placeholder: false,
        };
        let block = build_failure_context_block(&summary);
        // Empty by_area still produces a useful warning preamble.
        assert!(block.contains("FAILED"), "{}", block);
        assert!(block.contains("verdict: escalate"), "{}", block);
        assert!(
            block.contains("No specific learnings extracted"),
            "{}",
            block
        );
    }

    // --- RewindOptions defaults -------------------------------------------

    #[test]
    fn rewind_options_default_to_replay_on() {
        // Slash command §6 rule: "Default to replay" — only honour
        // `--no-replay` when the user explicitly typed it.
        let opts = RewindOptions::default();
        assert!(opts.replay);
    }

    // --- Error display ----------------------------------------------------

    #[test]
    fn rewind_error_display_messages_are_actionable() {
        let cases = [
            RewindError::SessionNotFound("abc".into()),
            RewindError::PartialRestore(vec![]),
            RewindError::HttpRewind("connreset".into()),
            RewindError::HttpKill("404".into()),
            RewindError::HttpSpawn("timeout".into()),
            RewindError::Db("pool".into()),
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

    // --- Partial-restore short-circuit ------------------------------------

    /// Regression guard: if any later edit drops the partial-restore
    /// short-circuit, an in-flight workspace gets killed + replayed
    /// while still in a half-restored state — silently corrupts the
    /// next session. Encoded as a behaviour test on the error variant
    /// since the orchestration path needs PG + an HTTP server.
    #[test]
    fn partial_restore_carries_per_file_errors() {
        let errs = vec![
            RewindFileError {
                file_path: "src/foo.rs".into(),
                reason: "blob sha256 mismatch".into(),
            },
            RewindFileError {
                file_path: "src/bar.rs".into(),
                reason: "blob unreadable".into(),
            },
        ];
        let err = RewindError::PartialRestore(errs.clone());
        let s = format!("{}", err);
        assert!(s.contains("partial file restore"), "{}", s);
        assert!(s.contains("Workspace not modified"), "{}", s);
        // The variant carries the per-file errors so the UI can render
        // a list — guard against accidental flattening to a single
        // string.
        match err {
            RewindError::PartialRestore(carried) => assert_eq!(carried.len(), 2),
            _ => unreachable!(),
        }
    }
}

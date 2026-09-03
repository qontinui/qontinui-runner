//! Fix agent: spawns Claude CLI to implement reflection fixes.
//!
//! Ported from the supervisor's pipeline mode fix agent.

use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::watch;
use tracing::{error, info, warn};

/// Fix types that indicate the workflow itself needs structural changes (rebuild).
const REBUILD_FIX_TYPES: &[&str] = &[
    "workflow_step_rewrite",
    "instruction_clarification",
    "context_addition",
];

/// Check if any fixes require a workflow rebuild.
pub fn should_rebuild(fixes: &[serde_json::Value]) -> bool {
    for fix in fixes {
        let fix_type = fix
            .get("fix_type")
            .or_else(|| fix.get("type"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if REBUILD_FIX_TYPES.iter().any(|&rt| fix_type.contains(rt)) {
            return true;
        }
    }
    false
}

/// Build the fix agent's `claude --print` command, fully env-prepared.
///
/// The credential scrub is the LAST env mutation: nothing between here and
/// [`run_fix_agent`]'s `cmd.spawn()` touches the env (only the Windows
/// creation-flag, which is not env). See
/// `crate::terminal::CREDENTIAL_VALUE_ENV_VARS` — this agent runs
/// `bypassPermissions`, and its stdout/stderr are streamed into the runner's
/// logs, so an `env` dump here is doubly persisted.
///
/// Returned by value so the constructed environment is unit-testable without
/// spawning `claude`.
pub(crate) fn build_fix_agent_command(prompt_file: &str, model: &str) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new("claude");
    cmd.args([
        "--print",
        prompt_file,
        "--permission-mode",
        "bypassPermissions",
        "--output-format",
        "text",
        "--model",
        model,
    ])
    .env_remove("CLAUDECODE")
    // Same rule, sibling marker — see `session::transport::claude_cli` docs.
    .env_remove(qontinui_runner_lib::claude_env::CLAUDE_CHILD_SESSION_ENV)
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());

    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW

    // Full non-interactive git credential posture — this seam spawns a
    // `claude` that can run `git push`, so without it a credential prompt
    // is an infinite silent hang. One shared list, eight seams; see
    // `credential_helper::non_interactive_git_env`.
    crate::credential_helper::apply_non_interactive_git_env_tokio(&mut cmd);
    crate::terminal::scrub_credential_env_tokio(&mut cmd);

    cmd
}

/// Build a prompt for Claude CLI to implement fixes.
pub fn build_fix_prompt(fixes: &[serde_json::Value], additional_context: Option<&str>) -> String {
    let mut prompt = String::from("# Implement Reflection Fixes\n\n");
    prompt.push_str("The following issues were found during workflow reflection. Fix them.\n\n");

    for (i, fix) in fixes.iter().enumerate() {
        let fix_type = fix
            .get("fix_type")
            .or_else(|| fix.get("type"))
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let description = fix
            .get("description")
            .or_else(|| fix.get("message"))
            .and_then(|v| v.as_str())
            .unwrap_or("No description");
        let file = fix
            .get("file")
            .or_else(|| fix.get("file_path"))
            .and_then(|v| v.as_str());

        prompt.push_str(&format!("## Fix {} — {}\n\n", i + 1, fix_type));
        prompt.push_str(&format!("{}\n", description));
        if let Some(f) = file {
            prompt.push_str(&format!("\n**File:** `{}`\n", f));
        }
        prompt.push('\n');
    }

    prompt.push_str("## Instructions\n\n");
    prompt.push_str("1. Read the relevant files mentioned above\n");
    prompt.push_str("2. Make minimal, targeted fixes for each issue\n");
    prompt.push_str("3. Do NOT refactor or change unrelated code\n");
    prompt.push_str("4. Do NOT add new features\n");

    if let Some(ctx) = additional_context {
        prompt.push_str(&format!("\n## Additional Context\n\n{}\n", ctx));
    }

    prompt
}

/// Spawn Claude CLI to implement fixes. Returns Ok(true) on success.
pub async fn run_fix_agent(
    prompt: &str,
    model: &str,
    timeout_secs: u64,
    stop_rx: &watch::Receiver<bool>,
) -> Result<bool, String> {
    // Agent-registry spawn authorization (plan
    // `2026-07-28-migrate-claude-md-into-qontinui.md` Phase 4c, served clause
    // `agent-spawn-authorization`). This launches the `claude` CLI on the
    // user's own AI account from an autonomous loop. It is `--print` — a
    // one-shot that dies with the step that asked for it — so it is an
    // `in_session_subagent` (implied-by-task, bounded), not a standing spawn.
    // Checked before the prompt file is written so a refusal leaves no
    // artifacts behind.
    let authz = crate::agent_authorization::authorize_spawn(
        Some("orchestration-fix-agent"),
        crate::agent_authorization::SpawnPath::InSessionSubagent,
    )
    .await;
    if let Some(refusal) = authz.refusal() {
        return Err(refusal);
    }

    // Write prompt to temp file
    let prompt_file = std::env::temp_dir().join("qontinui-orchestration-fix-prompt.md");
    tokio::fs::write(&prompt_file, prompt)
        .await
        .map_err(|e| format!("Failed to write prompt file: {}", e))?;

    info!(
        "Fix agent: spawning Claude CLI (model={}, timeout={}s)",
        model, timeout_secs
    );

    let mut cmd = build_fix_agent_command(&prompt_file.display().to_string(), model);

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("Failed to spawn Claude CLI: {}", e))?;

    // Log stdout/stderr
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let log_handle = tokio::spawn(async move {
        if let Some(stdout) = stdout {
            let reader = BufReader::new(stdout);
            let mut lines = reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                info!("Fix agent stdout: {}", line);
            }
        }
    });

    let stderr_handle = tokio::spawn(async move {
        if let Some(stderr) = stderr {
            let reader = BufReader::new(stderr);
            let mut lines = reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                warn!("Fix agent stderr: {}", line);
            }
        }
    });

    // Wait with timeout, checking stop signal periodically
    let timeout = if timeout_secs == 0 {
        Duration::from_secs(3600) // 1 hour max if no timeout
    } else {
        Duration::from_secs(timeout_secs)
    };

    let result = tokio::select! {
        result = tokio::time::timeout(timeout, child.wait()) => {
            match result {
                Ok(Ok(status)) => {
                    let _ = log_handle.await;
                    let _ = stderr_handle.await;
                    if status.success() {
                        info!("Fix agent: completed successfully");
                        Ok(true)
                    } else {
                        warn!("Fix agent: Claude CLI exited with status: {}", status);
                        Ok(false)
                    }
                }
                Ok(Err(e)) => {
                    let _ = child.kill().await;
                    Err(format!("Claude CLI process error: {}", e))
                }
                Err(_) => {
                    let _ = child.kill().await;
                    error!("Fix agent: timed out after {}s", timeout_secs);
                    Ok(false)
                }
            }
        }
        _ = async {
            loop {
                tokio::time::sleep(Duration::from_secs(2)).await;
                if *stop_rx.borrow() {
                    return;
                }
            }
        } => {
            let _ = child.kill().await;
            info!("Fix agent: stopped by user");
            Ok(false)
        }
    };

    // Clean up prompt file
    let _ = tokio::fs::remove_file(&prompt_file).await;

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The eighth Claude-spawning seam. It carries no credential-scrub test of
    /// its own, so this is the only per-seam assertion that this spawn path
    /// cannot reach a credential prompt. Removing the
    /// `apply_non_interactive_git_env_tokio` call from
    /// [`build_fix_agent_command`] reddens it.
    #[test]
    fn fix_agent_command_applies_non_interactive_git_posture() {
        let cmd = build_fix_agent_command("/tmp/prompt.txt", "sonnet");
        crate::credential_helper::assert_non_interactive_git_posture_tokio(
            &cmd,
            "build_fix_agent_command",
        );
    }
}

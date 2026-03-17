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

/// Build a prompt for Claude CLI to implement fixes.
pub fn build_fix_prompt(
    fixes: &[serde_json::Value],
    additional_context: Option<&str>,
) -> String {
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
    // Write prompt to temp file
    let prompt_file = std::env::temp_dir().join("qontinui-orchestration-fix-prompt.md");
    tokio::fs::write(&prompt_file, prompt)
        .await
        .map_err(|e| format!("Failed to write prompt file: {}", e))?;

    info!(
        "Fix agent: spawning Claude CLI (model={}, timeout={}s)",
        model, timeout_secs
    );

    let mut cmd = tokio::process::Command::new("claude");
    cmd.args([
        "--print",
        &prompt_file.display().to_string(),
        "--permission-mode",
        "bypassPermissions",
        "--output-format",
        "text",
        "--model",
        model,
    ])
    .env_remove("CLAUDECODE")
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());

    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW

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

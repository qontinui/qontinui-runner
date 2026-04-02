//! Saga compensation for Restate durable workflows.
//!
//! Implements a LIFO compensation stack that records undo actions at phase
//! boundaries and executes them in reverse order on workflow failure.

use serde::{Deserialize, Serialize};
use tracing::{error, info, warn};

/// A compensation action that can be executed to undo a side effect.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompensationAction {
    /// Unique identifier for this compensation
    pub id: String,
    /// Phase that created this compensation
    pub phase: String,
    /// Iteration number (if applicable)
    pub iteration: Option<u32>,
    /// The type and parameters of the compensation
    pub action_type: CompensationType,
    /// When this compensation was recorded
    pub recorded_at: String,
    /// Human-readable description
    pub description: String,
}

/// Types of compensation actions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CompensationType {
    /// Reset a git repository to a specific commit
    GitReset {
        commit_hash: String,
        repo_path: String,
    },
    /// Remove specific files created during execution
    FileCleanup {
        paths: Vec<String>,
    },
    /// Remove a git worktree
    WorktreeRemove {
        worktree_path: String,
        branch_name: Option<String>,
    },
    /// Kill a spawned process
    ProcessKill {
        pid: u32,
    },
    /// Run a custom shell command for cleanup
    CustomCommand {
        command: String,
        args: Vec<String>,
        cwd: String,
    },
}

/// Result of executing a single compensation action.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompensationResult {
    pub action_id: String,
    pub success: bool,
    pub error: Option<String>,
    pub duration_ms: u64,
}

/// Execute a single compensation action.
pub async fn execute_compensation(action: &CompensationAction) -> CompensationResult {
    let start = std::time::Instant::now();
    info!(
        "Executing compensation [{}]: {} ({})",
        action.id, action.description, action.phase
    );

    let result = match &action.action_type {
        CompensationType::GitReset {
            commit_hash,
            repo_path,
        } => execute_git_reset(commit_hash, repo_path).await,

        CompensationType::FileCleanup { paths } => execute_file_cleanup(paths).await,

        CompensationType::WorktreeRemove {
            worktree_path,
            branch_name,
        } => execute_worktree_remove(worktree_path, branch_name.as_deref()).await,

        CompensationType::ProcessKill { pid } => execute_process_kill(*pid).await,

        CompensationType::CustomCommand { command, args, cwd } => {
            execute_custom_command(command, args, cwd).await
        }
    };

    let duration_ms = start.elapsed().as_millis() as u64;
    match &result {
        Ok(()) => info!(
            "Compensation [{}] succeeded in {}ms",
            action.id, duration_ms
        ),
        Err(e) => error!(
            "Compensation [{}] failed in {}ms: {}",
            action.id, duration_ms, e
        ),
    }

    CompensationResult {
        action_id: action.id.clone(),
        success: result.is_ok(),
        error: result.err(),
        duration_ms,
    }
}

/// Execute all compensation actions in LIFO (reverse) order.
/// Continues executing even if individual compensations fail.
pub async fn execute_all_compensations(
    actions: &[CompensationAction],
) -> Vec<CompensationResult> {
    let mut results = Vec::with_capacity(actions.len());

    // Execute in reverse order (LIFO)
    for action in actions.iter().rev() {
        let result = execute_compensation(action).await;
        results.push(result);
    }

    let succeeded = results.iter().filter(|r| r.success).count();
    let failed = results.iter().filter(|r| !r.success).count();
    info!(
        "Compensation complete: {}/{} succeeded, {} failed",
        succeeded,
        actions.len(),
        failed
    );

    results
}

async fn execute_git_reset(commit_hash: &str, repo_path: &str) -> Result<(), String> {
    let output = tokio::process::Command::new("git")
        .args(["reset", "--hard", commit_hash])
        .current_dir(repo_path)
        .output()
        .await
        .map_err(|e| format!("Failed to run git reset: {}", e))?;

    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "git reset failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ))
    }
}

async fn execute_file_cleanup(paths: &[String]) -> Result<(), String> {
    let mut errors = Vec::new();
    for path in paths {
        if let Err(e) = tokio::fs::remove_file(path).await {
            if e.kind() != std::io::ErrorKind::NotFound {
                errors.push(format!("{}: {}", path, e));
            }
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(format!("File cleanup errors: {}", errors.join(", ")))
    }
}

async fn execute_worktree_remove(
    worktree_path: &str,
    _branch_name: Option<&str>,
) -> Result<(), String> {
    let output = tokio::process::Command::new("git")
        .args(["worktree", "remove", "--force", worktree_path])
        .output()
        .await
        .map_err(|e| format!("Failed to run git worktree remove: {}", e))?;

    if output.status.success() {
        Ok(())
    } else {
        // Fallback: just delete the directory
        warn!(
            "git worktree remove failed, falling back to directory removal: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        tokio::fs::remove_dir_all(worktree_path)
            .await
            .map_err(|e| format!("Failed to remove worktree directory: {}", e))
    }
}

async fn execute_process_kill(pid: u32) -> Result<(), String> {
    #[cfg(windows)]
    {
        let output = tokio::process::Command::new("taskkill")
            .args(["/F", "/T", "/PID", &pid.to_string()])
            .output()
            .await
            .map_err(|e| format!("taskkill failed: {}", e))?;

        if output.status.success() {
            Ok(())
        } else {
            Err(format!(
                "taskkill failed: {}",
                String::from_utf8_lossy(&output.stderr)
            ))
        }
    }

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let result = unsafe { libc::kill(pid as i32, libc::SIGTERM) };
        if result == 0 {
            Ok(())
        } else {
            Err(format!("kill({}) failed with errno", pid))
        }
    }
}

async fn execute_custom_command(
    command: &str,
    args: &[String],
    cwd: &str,
) -> Result<(), String> {
    let output = tokio::process::Command::new(command)
        .args(args)
        .current_dir(cwd)
        .output()
        .await
        .map_err(|e| format!("Failed to run command '{}': {}", command, e))?;

    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "Command '{}' failed: {}",
            command,
            String::from_utf8_lossy(&output.stderr)
        ))
    }
}

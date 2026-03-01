//! Git event watcher: monitors .git/ refs for commits, branch switches, tags.
//!
//! Uses file watching on specific .git/ paths rather than polling git CLI.

use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use super::super::types::TriggerEvent;

/// Start a git event watcher for a trigger.
///
/// Watches:
/// - `.git/HEAD` -- branch switches
/// - `.git/refs/heads/` -- new commits
/// - `.git/refs/tags/` -- new tags
pub fn start_git_watcher(
    trigger_id: String,
    repo_path: String,
    events: Vec<String>,
    branch_filter: Option<String>,
    tx: mpsc::Sender<TriggerEvent>,
    stop_signal: Arc<AtomicBool>,
) -> Result<RecommendedWatcher, String> {
    let git_dir = PathBuf::from(&repo_path).join(".git");
    if !git_dir.exists() {
        return Err(format!(
            "Not a git repository: {} (.git not found)",
            repo_path
        ));
    }

    let trigger_id_clone = trigger_id.clone();
    let events_clone = events.clone();
    let branch_filter_clone = branch_filter.clone();
    let repo_path_clone = repo_path.clone();

    let mut watcher = notify::recommended_watcher(move |result: Result<Event, notify::Error>| {
        if stop_signal.load(Ordering::SeqCst) {
            return;
        }

        match result {
            Ok(event) => {
                match event.kind {
                    EventKind::Create(_) | EventKind::Modify(_) => {}
                    _ => return,
                }

                for path in &event.paths {
                    let path_str = path.to_string_lossy().to_string();
                    let path_normalized = path_str.replace('\\', "/");

                    let (event_type, details) = if path_normalized.contains(".git/HEAD")
                        || path_normalized.ends_with("HEAD")
                    {
                        if !events_clone.contains(&"branch_switch".to_string()) {
                            continue;
                        }
                        // Read current branch from HEAD
                        let branch = read_current_branch(&repo_path_clone);
                        ("branch_switch", branch)
                    } else if path_normalized.contains("refs/heads/") {
                        if !events_clone.contains(&"commit".to_string()) {
                            continue;
                        }
                        // Extract branch name from path
                        let branch = path_normalized
                            .split("refs/heads/")
                            .last()
                            .unwrap_or("unknown")
                            .to_string();
                        ("commit", branch)
                    } else if path_normalized.contains("refs/tags/") {
                        if !events_clone.contains(&"tag".to_string()) {
                            continue;
                        }
                        let tag = path_normalized
                            .split("refs/tags/")
                            .last()
                            .unwrap_or("unknown")
                            .to_string();
                        ("tag", tag)
                    } else {
                        continue;
                    };

                    // Apply branch filter
                    if let Some(ref filter) = branch_filter_clone {
                        if let Ok(re) = regex::Regex::new(filter) {
                            if !re.is_match(&details) {
                                debug!(
                                    "Git watcher: '{}' doesn't match filter '{}'",
                                    details, filter
                                );
                                continue;
                            }
                        }
                    }

                    let mut variables = HashMap::new();
                    variables.insert("git_event".to_string(), event_type.to_string());
                    variables.insert("branch".to_string(), details.clone());
                    variables.insert("repo_path".to_string(), repo_path_clone.clone());

                    let trigger_event = TriggerEvent {
                        trigger_id: trigger_id_clone.clone(),
                        event_type: format!("git_{}", event_type),
                        event_data: serde_json::json!({
                            "event": event_type,
                            "detail": details,
                            "repo_path": repo_path_clone,
                        }),
                        variables,
                        chain_depth: 0,
                    };

                    if let Err(e) = tx.try_send(trigger_event) {
                        warn!("Failed to send git event: {}", e);
                    }
                }
            }
            Err(e) => {
                warn!("Git watcher error: {}", e);
            }
        }
    })
    .map_err(|e| format!("Failed to create git watcher: {}", e))?;

    // Watch HEAD for branch switches
    if events.contains(&"branch_switch".to_string()) {
        let head_path = git_dir.join("HEAD");
        if head_path.exists() {
            watcher
                .watch(&head_path, RecursiveMode::NonRecursive)
                .map_err(|e| format!("Failed to watch .git/HEAD: {}", e))?;
        }
    }

    // Watch refs/heads for commits
    if events.contains(&"commit".to_string()) {
        let refs_heads = git_dir.join("refs").join("heads");
        if refs_heads.exists() {
            watcher
                .watch(&refs_heads, RecursiveMode::Recursive)
                .map_err(|e| format!("Failed to watch .git/refs/heads: {}", e))?;
        }
    }

    // Watch refs/tags for tags
    if events.contains(&"tag".to_string()) {
        let refs_tags = git_dir.join("refs").join("tags");
        if refs_tags.exists() {
            watcher
                .watch(&refs_tags, RecursiveMode::Recursive)
                .map_err(|e| format!("Failed to watch .git/refs/tags: {}", e))?;
        }
    }

    info!(
        "Git watcher: watching '{}' for events: {:?}",
        repo_path, events
    );

    Ok(watcher)
}

/// Read the current branch from .git/HEAD.
fn read_current_branch(repo_path: &str) -> String {
    let head_path = PathBuf::from(repo_path).join(".git").join("HEAD");
    match std::fs::read_to_string(&head_path) {
        Ok(content) => {
            let content = content.trim();
            if let Some(branch) = content.strip_prefix("ref: refs/heads/") {
                branch.to_string()
            } else {
                // Detached HEAD -- return the commit hash
                content[..8.min(content.len())].to_string()
            }
        }
        Err(_) => "unknown".to_string(),
    }
}

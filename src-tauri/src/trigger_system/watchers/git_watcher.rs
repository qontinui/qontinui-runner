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

                    // Base payload: always populated, even if git2 enrichment fails.
                    // This preserves the pre-D5-Phase-1 shape so trigger configs
                    // that depend on the minimal payload continue to work.
                    let mut event_data = serde_json::json!({
                        "event": event_type,
                        "detail": details,
                        "repo_path": repo_path_clone,
                    });

                    // D5 Phase 1: enrich `commit` events with libgit2 metadata
                    // (sha, message, author, timestamp, changed_files). If
                    // git2 fails for any reason — repo locked, libgit2 error,
                    // partial commit on disk — we silently fall back to the
                    // base payload so the supervision channel never breaks
                    // the existing trigger pipeline.
                    if event_type == "commit" {
                        if let Some(commit_meta) = enrich_commit_metadata(&repo_path_clone) {
                            if let Some(obj) = event_data.as_object_mut() {
                                for (k, v) in commit_meta.as_object().into_iter().flatten() {
                                    obj.insert(k.clone(), v.clone());
                                }
                            }
                            // Surface SHA + author into template variables so
                            // workflow prompts (and supervision payloads) can
                            // reference {{sha}} / {{author_name}}.
                            if let Some(sha) = commit_meta.get("sha").and_then(|v| v.as_str()) {
                                variables.insert("sha".to_string(), sha.to_string());
                            }
                            if let Some(author) =
                                commit_meta.get("author_name").and_then(|v| v.as_str())
                            {
                                variables.insert("author_name".to_string(), author.to_string());
                            }
                        }
                    }

                    let trigger_event = TriggerEvent {
                        trigger_id: trigger_id_clone.clone(),
                        event_type: format!("git_{}", event_type),
                        event_data,
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

/// Enrich a `commit` event payload with libgit2-derived metadata.
///
/// Returns a `serde_json::Value` (object) with keys:
///   - `sha`: full commit SHA (string)
///   - `message`: commit message (string, trimmed)
///   - `author_name`: commit author name (string)
///   - `author_email`: commit author email (string)
///   - `timestamp`: commit author timestamp (unix seconds, i64)
///   - `changed_files`: array of `{path, status}` objects (status is one of
///     "added" | "modified" | "deleted" | "renamed" | "typechange" | "other")
///
/// Returns `None` if the repository can't be opened, HEAD can't be peeled
/// to a commit, or any other git2 error occurs. Callers should fall back
/// to the minimal pre-enrichment payload in that case.
fn enrich_commit_metadata(repo_path: &str) -> Option<serde_json::Value> {
    let repo = git2::Repository::open(repo_path).ok()?;
    let head = repo.head().ok()?;
    let commit = head.peel_to_commit().ok()?;

    let sha = commit.id().to_string();
    let message = commit.message().unwrap_or("").trim().to_string();
    let author = commit.author();
    let author_name = author.name().unwrap_or("").to_string();
    let author_email = author.email().unwrap_or("").to_string();
    let timestamp = commit.time().seconds();

    // First-parent SHA (null for a root commit). The tree diff below already
    // peels the first parent; this surfaces it on the payload so downstream
    // consumers (the coord commit forwarder) can record the lineage edge.
    let parent_sha = if commit.parent_count() > 0 {
        commit.parent(0).ok().map(|p| p.id().to_string())
    } else {
        None
    };

    // Compute changed files by diffing this commit's tree against its first
    // parent's tree. Initial commits (no parents) report every file in the
    // tree as added. If the diff fails for any reason, omit the field.
    let changed_files = collect_changed_files(&repo, &commit);

    Some(serde_json::json!({
        "sha": sha,
        "message": message,
        "author_name": author_name,
        "author_email": author_email,
        "timestamp": timestamp,
        "parent_sha": parent_sha,
        "changed_files": changed_files,
    }))
}

/// Diff a commit against its first parent (or an empty tree for the initial
/// commit) and return the resulting per-file `{path, status}` list. Returns
/// an empty vec on diff failure rather than propagating the error — the
/// commit payload is still useful without the file list.
fn collect_changed_files(
    repo: &git2::Repository,
    commit: &git2::Commit<'_>,
) -> Vec<serde_json::Value> {
    let new_tree = match commit.tree() {
        Ok(t) => t,
        Err(_) => return Vec::new(),
    };

    let parent_tree = if commit.parent_count() > 0 {
        match commit.parent(0).and_then(|p| p.tree()) {
            Ok(t) => Some(t),
            Err(_) => return Vec::new(),
        }
    } else {
        None
    };

    let diff = match repo.diff_tree_to_tree(parent_tree.as_ref(), Some(&new_tree), None) {
        Ok(d) => d,
        Err(_) => return Vec::new(),
    };

    let mut out: Vec<serde_json::Value> = Vec::new();
    for delta in diff.deltas() {
        let status = match delta.status() {
            git2::Delta::Added => "added",
            git2::Delta::Deleted => "deleted",
            git2::Delta::Modified => "modified",
            git2::Delta::Renamed => "renamed",
            git2::Delta::Typechange => "typechange",
            git2::Delta::Copied => "copied",
            _ => "other",
        };
        // Prefer new_file().path() (post-change); fall back to old_file().path()
        // for deletions where new_file is empty.
        let path = delta
            .new_file()
            .path()
            .or_else(|| delta.old_file().path())
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        out.push(serde_json::json!({ "path": path, "status": status }));
    }
    out
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

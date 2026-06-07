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

                    let Some((event_type, details)) =
                        classify_ref_path(&path_normalized, &repo_path_clone)
                    else {
                        continue;
                    };
                    if !events_clone.iter().any(|e| e == event_type) {
                        continue;
                    }

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
                        if let Some(commit_meta) =
                            enrich_commit_metadata(&repo_path_clone, Some(&details))
                        {
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

/// Classify a normalized (forward-slash) filesystem path under `.git/` into
/// a `(event_type, detail)` pair, or `None` for paths the watcher ignores.
///
/// Lock files are ignored explicitly: git updates refs via a lock-file dance
/// (write `refs/heads/<branch>.lock`, then rename it onto
/// `refs/heads/<branch>`). The lock event fires first, so it used to win the
/// trigger debounce window — events carried a phantom `<branch>.lock` branch
/// and enrichment ran before the ref was updated. The rename-target event for
/// the real ref follows immediately and is the one that drives the pipeline.
///
/// Directories are ignored too: creating a namespaced branch (`fix/<name>`)
/// also creates the `refs/heads/fix/` directory, and that event used to
/// classify as a commit on the phantom branch `fix` (enriched from HEAD —
/// the wrong commit). A real loose ref is always a file, never a directory.
fn classify_ref_path(path_normalized: &str, repo_path: &str) -> Option<(&'static str, String)> {
    if path_normalized.ends_with(".lock") {
        return None;
    }
    if std::path::Path::new(path_normalized).is_dir() {
        return None;
    }
    if path_normalized.contains(".git/HEAD") || path_normalized.ends_with("HEAD") {
        // Read current branch from HEAD
        Some(("branch_switch", read_current_branch(repo_path)))
    } else if path_normalized.contains("refs/heads/") {
        // Extract branch name from path
        let branch = path_normalized
            .split("refs/heads/")
            .last()
            .unwrap_or("unknown")
            .to_string();
        Some(("commit", branch))
    } else if path_normalized.contains("refs/tags/") {
        let tag = path_normalized
            .split("refs/tags/")
            .last()
            .unwrap_or("unknown")
            .to_string();
        Some(("tag", tag))
    } else {
        None
    }
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
/// `branch` is the name of the changed ref when known. The commit is resolved
/// from `refs/heads/<branch>` rather than HEAD: linked-worktree commits update
/// the shared branch ref while the main checkout's HEAD points elsewhere, so
/// peeling HEAD mis-attributed them to whatever the main checkout had checked
/// out. HEAD is used only when no branch is known; a known-but-unresolvable
/// ref (deleted between the event and enrichment) yields `None` rather than
/// a HEAD mis-attribution.
///
/// Returns `None` if the repository can't be opened, no commit can be
/// resolved, or any other git2 error occurs. Callers should fall back
/// to the minimal pre-enrichment payload in that case.
fn enrich_commit_metadata(repo_path: &str, branch: Option<&str>) -> Option<serde_json::Value> {
    let repo = git2::Repository::open(repo_path).ok()?;
    let commit = resolve_event_commit(&repo, branch)?;

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

/// Resolve the commit a watcher event refers to: the tip of the changed
/// branch ref when known, else the repo HEAD.
fn resolve_event_commit<'r>(
    repo: &'r git2::Repository,
    branch: Option<&str>,
) -> Option<git2::Commit<'r>> {
    if let Some(branch) = branch {
        // The changed ref is known: resolve it or give up. Falling back to
        // HEAD here would attribute the event to whatever the main checkout
        // has checked out — a different commit entirely (observed live as
        // wrong-sha rows in coord.commit_observations). An unresolvable
        // known ref (deleted mid-race, or a phantom name) yields None and
        // the event goes out un-enriched instead of mis-attributed.
        let oid = repo.refname_to_id(&format!("refs/heads/{branch}")).ok()?;
        return repo.find_commit(oid).ok();
    }
    repo.head().ok()?.peel_to_commit().ok()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_ignores_ref_lock_files() {
        // Git's ref-update lock-file dance must not produce events: the
        // phantom "<branch>.lock" branch polluted commit observations and
        // won the debounce window from the real ref event.
        assert!(classify_ref_path("D:/repo/.git/refs/heads/main.lock", ".").is_none());
        assert!(classify_ref_path("D:/repo/.git/refs/heads/feat/x.lock", ".").is_none());
        assert!(classify_ref_path("D:/repo/.git/refs/tags/v1.0.lock", ".").is_none());
        assert!(classify_ref_path("D:/repo/.git/HEAD.lock", ".").is_none());
    }

    #[test]
    fn classify_extracts_branch_and_tag() {
        assert_eq!(
            classify_ref_path("D:/repo/.git/refs/heads/main", "."),
            Some(("commit", "main".to_string()))
        );
        assert_eq!(
            classify_ref_path("D:/repo/.git/refs/heads/feat/nested-branch", "."),
            Some(("commit", "feat/nested-branch".to_string()))
        );
        assert_eq!(
            classify_ref_path("D:/repo/.git/refs/tags/v1.2.3", "."),
            Some(("tag", "v1.2.3".to_string()))
        );
        // Unrelated .git internals stay ignored.
        assert!(classify_ref_path("D:/repo/.git/config", ".").is_none());
        assert!(classify_ref_path("D:/repo/.git/index", ".").is_none());
    }

    /// Stage `name` with `content` and commit it on HEAD; returns the new OID.
    fn commit_file(repo: &git2::Repository, name: &str, content: &str, msg: &str) -> git2::Oid {
        let workdir = repo.workdir().unwrap();
        std::fs::write(workdir.join(name), content).unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new(name)).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let sig = git2::Signature::now("Watcher Test", "watcher@test.local").unwrap();
        let parent = repo.head().ok().and_then(|h| h.peel_to_commit().ok());
        let parents: Vec<&git2::Commit> = parent.iter().collect();
        repo.commit(Some("HEAD"), &sig, &sig, msg, &tree, &parents)
            .unwrap()
    }

    #[test]
    fn enrich_resolves_changed_ref_not_head() {
        let dir = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();
        let first = commit_file(&repo, "a.txt", "one", "first");
        let side_tip = repo.find_commit(first).unwrap();
        repo.branch("side", &side_tip, false).unwrap();
        let second = commit_file(&repo, "a.txt", "two", "second");
        let path = dir.path().to_string_lossy().to_string();

        // Changed-ref resolution: the named branch's tip, not HEAD. This is
        // the linked-worktree case — a commit on another branch updates the
        // shared ref while this checkout's HEAD points elsewhere.
        let meta = enrich_commit_metadata(&path, Some("side")).expect("enrich side");
        assert_eq!(meta["sha"], first.to_string());
        assert_eq!(meta["message"], "first");

        // No branch known → HEAD (pre-existing behavior).
        let meta = enrich_commit_metadata(&path, None).expect("enrich head");
        assert_eq!(meta["sha"], second.to_string());

        // Known-but-unresolvable branch → None (un-enriched), NOT a HEAD
        // mis-attribution. Phantom branch names from namespaced-dir events
        // and mid-race ref deletions both land here.
        assert!(enrich_commit_metadata(&path, Some("does-not-exist")).is_none());
    }

    #[test]
    fn classify_ignores_ref_directories() {
        // Creating a namespaced branch (fix/<name>) also creates the
        // refs/heads/fix/ directory; that event must not classify as a
        // commit on phantom branch "fix".
        let dir = tempfile::tempdir().unwrap();
        let ns = dir
            .path()
            .join(".git")
            .join("refs")
            .join("heads")
            .join("fix");
        std::fs::create_dir_all(&ns).unwrap();
        let ns_norm = ns.to_string_lossy().replace('\\', "/");
        assert!(classify_ref_path(&ns_norm, ".").is_none());

        // A real loose-ref FILE at a namespaced path still classifies.
        let leaf = ns.join("real-branch");
        std::fs::write(&leaf, "0000000000000000000000000000000000000000\n").unwrap();
        let leaf_norm = leaf.to_string_lossy().replace('\\', "/");
        assert_eq!(
            classify_ref_path(&leaf_norm, "."),
            Some(("commit", "fix/real-branch".to_string()))
        );
    }
}

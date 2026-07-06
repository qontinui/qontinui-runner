//! Layer 3 of `plans/2026-06-03-shared-checkout-coordination-gap-fix.md` —
//! soft warning for branch-mutating git typed into the runner's Terminal
//! PTY.
//!
//! Branch-mutating git typed directly into a Terminal PTY (`git switch`,
//! `git checkout <branch>`, `git reset --hard`) bypasses every Claude Code
//! hook, so a session can clobber a *peer's* in-flight work in a shared
//! checkout with no warning. This module:
//!
//! 1. Recognizes a completed input line as branch-mutating git
//!    ([`is_branch_mutating_git`]) — cheap, only runs when a line is
//!    submitted (CR/LF), never per keystroke.
//! 2. If so, asynchronously asks coord whether a *peer* holds the
//!    `worktree` claim on this session's repo and, if so, emits a soft
//!    `terminal-coord-warning` Tauri event for the frontend to surface a
//!    dismissible banner ([`spawn_check_and_warn`]).
//!
//! Everything here is best-effort and SOFT: it never blocks input, never
//! touches terminal output, and swallows all failures (coord unreachable,
//! parse errors, missing repo). The frontend banner is dismissible and
//! advisory only.

use std::path::{Path, PathBuf};

use once_cell::sync::Lazy;
use regex::Regex;
use serde::Serialize;
use tauri::{AppHandle, Emitter};
use tracing::debug;

/// Branch-mutating git matcher. Matches (optionally prefixed by a
/// `cd <dir> &&`):
///   - `git [-C <dir>] switch ...`
///   - `git [-C <dir>] checkout <branch>` but NOT `git checkout -- <file>`
///   - `git [-C <dir>] reset --hard ...`
///
/// This is a soft heuristic, not a shell parser — false negatives are
/// acceptable (a missed warning is no worse than today); false positives
/// only ever produce an advisory banner the user can dismiss.
static BRANCH_MUTATING_GIT: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"^\s*(cd\s+[^&]+&&\s*)?git\s+(-C\s+\S+\s+)?(switch\b|checkout\b|reset\s+--hard\b)")
        .expect("BRANCH_MUTATING_GIT regex is a compile-time constant")
});

/// Excludes `git checkout -- <file>` (a working-tree file restore, which is
/// NOT branch-mutating). The Rust `regex` crate (RE2) has no look-around, so
/// this case is filtered in [`is_branch_mutating_git`] rather than inline in
/// [`BRANCH_MUTATING_GIT`]. Matches a `checkout` whose first argument is the
/// `--` path separator (`--` followed by whitespace or end-of-line); a
/// double-dash option like `--force` is NOT matched and stays branch-mutating.
static CHECKOUT_FILE_RESTORE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"\bcheckout\s+--(\s|$)")
        .expect("CHECKOUT_FILE_RESTORE regex is a compile-time constant")
});

/// Returns true iff `line` is a branch-mutating git command per
/// [`BRANCH_MUTATING_GIT`], excluding the `git checkout -- <file>` file-restore
/// form (see [`CHECKOUT_FILE_RESTORE`]).
pub fn is_branch_mutating_git(line: &str) -> bool {
    BRANCH_MUTATING_GIT.is_match(line) && !CHECKOUT_FILE_RESTORE.is_match(line)
}

/// Payload for the `terminal-coord-warning` Tauri event.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoordWarning {
    pub terminal_id: String,
    /// Repo slug (canonical-path basename) the warning is about.
    pub repo: String,
    /// `machine_id` of the peer holding the worktree claim.
    pub holder_machine_id: String,
    /// `session_id` of the holder, when coord reports one.
    pub holder_session_id: Option<String>,
    /// The branch-mutating command line that triggered the warning.
    pub command: String,
}

/// Best-effort: resolve the session's repo, ask coord for the live
/// `worktree` claim holder, and — if a PEER holds it — emit a soft
/// `terminal-coord-warning` event. Spawns a detached async task so the
/// caller (the PTY write hot path) never blocks. All failures are
/// swallowed.
///
/// Uses `tauri::async_runtime::spawn` (not bare `tokio::spawn`) because the
/// observer lives inside `TerminalSession::write` (the single input funnel),
/// which is reachable from bare OS threads — `tokio::spawn` would panic there.
///
/// `our_session_id` is this terminal's coord session id (if wired); it is
/// used to distinguish a same-machine peer (different session) from
/// ourselves.
pub fn spawn_check_and_warn(
    app_handle: AppHandle,
    terminal_id: String,
    working_dir: String,
    our_session_id: Option<uuid::Uuid>,
    command: String,
) {
    tauri::async_runtime::spawn(async move {
        if let Err(e) = check_and_warn(
            &app_handle,
            &terminal_id,
            &working_dir,
            our_session_id,
            &command,
        )
        .await
        {
            // Soft + best-effort: debug-only, never surfaced to the user.
            debug!(
                terminal_id = %terminal_id,
                error = %e,
                "terminal coord-warn: lookup skipped"
            );
        }
    });
}

/// Inner fallible body of [`spawn_check_and_warn`]. A returned `Err` is
/// purely observability — the caller logs it at debug and moves on.
async fn check_and_warn(
    app_handle: &AppHandle,
    terminal_id: &str,
    working_dir: &str,
    our_session_id: Option<uuid::Uuid>,
    command: &str,
) -> Result<(), String> {
    // 1. Resolve the repo's canonical path → the coord `worktree`
    //    resource key.
    let (repo_slug, resource_key) = resolve_repo(working_dir)
        .ok_or_else(|| "working_dir not under a known repo / no git toplevel".to_string())?;

    // 2. Resolve coord HTTP base + our device id.
    let coord_base = crate::mcp::agent_worktrees::coord_http_base()
        .map_err(|e| format!("coord URL not configured: {e}"))?;
    let our_machine_id = read_device_id().map_err(|e| format!("device_id unavailable: {e}"))?;

    // 3. GET the live worktree-claim holder for this repo.
    let url = format!(
        "{}/coord/claims/by-resource?kind=worktree&key={}",
        coord_base.trim_end_matches('/'),
        urlencoding::encode(&resource_key),
    );
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| format!("build client: {e}"))?;
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("GET {url}: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("GET by-resource {}", resp.status().as_u16()));
    }
    let body: serde_json::Value = resp.json().await.map_err(|e| format!("parse body: {e}"))?;

    // `null` => no holder; nothing to warn about.
    if body.is_null() {
        return Ok(());
    }

    let holder_machine = body
        .get("machine_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "holder missing machine_id".to_string())?;
    let holder_session = body
        .get("session_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    // 4. Decide whether the holder is a PEER (someone else) or us.
    //    - Foreign machine => always a peer.
    //    - Same machine but a different coord session => a same-machine
    //      peer (another terminal/agent on this host).
    //    - Same machine + same session (or our session unknown but the
    //      machine is ours with no session distinction) => it's us; no
    //      warning.
    let our_machine_str = our_machine_id.to_string();
    let is_peer = if holder_machine != our_machine_str {
        true
    } else {
        match (our_session_id, holder_session.as_deref()) {
            (Some(ours), Some(theirs)) => ours.to_string() != theirs,
            // Same machine, holder reports no session: can't prove it's a
            // peer, so stay quiet (avoid warning on our own claim).
            (Some(_), None) => false,
            // We have no session id: same-machine holder is ambiguous —
            // treat as us to avoid false positives.
            (None, _) => false,
        }
    };

    if !is_peer {
        return Ok(());
    }

    // 5. Emit the soft warning event.
    let payload = CoordWarning {
        terminal_id: terminal_id.to_string(),
        repo: repo_slug,
        holder_machine_id: holder_machine.to_string(),
        holder_session_id: holder_session,
        command: command.to_string(),
    };
    app_handle
        .emit("terminal-coord-warning", &payload)
        .map_err(|e| format!("emit terminal-coord-warning: {e}"))?;
    Ok(())
}

/// Resolve `working_dir` to `(repo_slug, resource_key)` where
/// `resource_key` is the canonical-path string coord stores for the
/// `worktree` claim.
///
/// Preference order:
///   1. If `working_dir` maps to a known canonical repo (L2 helper), use
///      that repo's canonical path (slug = repo name).
///   2. Otherwise fall back to `git rev-parse --show-toplevel` and use the
///      toplevel path as the key (slug = its basename).
fn resolve_repo(working_dir: &str) -> Option<(String, String)> {
    let wd = Path::new(working_dir);
    if let Some(slug) = crate::agent_worktree::canonical_paths::repo_slug_for_path(wd) {
        if let Ok(canonical) = crate::agent_worktree::canonical_paths::default_canonical_path(&slug)
        {
            return Some((slug, canonical.to_string_lossy().to_string()));
        }
    }
    let toplevel = git_toplevel(wd)?;
    let slug = toplevel
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_string();
    Some((slug, toplevel.to_string_lossy().to_string()))
}

/// `git -C <dir> rev-parse --show-toplevel`, or `None` if `dir` isn't in a
/// git repo / git is unavailable.
fn git_toplevel(dir: &Path) -> Option<PathBuf> {
    let output = crate::process_helpers::no_window("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(PathBuf::from(s))
    }
}

/// Read the device UUID from `~/.qontinui/machine.json`. Mirrors
/// `agent_worktree::isolated_edit::read_device_id` (which is private to
/// that module) — accepts `device_id` and falls back to legacy
/// `machine_id`.
fn read_device_id() -> Result<uuid::Uuid, String> {
    let path = dirs::home_dir()
        .map(|h| h.join(".qontinui").join("machine.json"))
        .ok_or_else(|| "no HOME dir".to_string())?;
    let bytes = std::fs::read(&path).map_err(|e| format!("read {}: {}", path.display(), e))?;
    let v: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|e| format!("parse {}: {}", path.display(), e))?;
    let id_str = v
        .get("device_id")
        .or_else(|| v.get("machine_id"))
        .and_then(|s| s.as_str())
        .ok_or_else(|| format!("{}: missing device_id (or machine_id)", path.display()))?;
    uuid::Uuid::parse_str(id_str).map_err(|e| format!("invalid UUID: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_git_switch() {
        assert!(is_branch_mutating_git("git switch main"));
        assert!(is_branch_mutating_git("  git switch -c feature"));
    }

    #[test]
    fn matches_git_checkout_branch() {
        assert!(is_branch_mutating_git("git checkout main"));
        assert!(is_branch_mutating_git("git checkout -b feature/x"));
    }

    #[test]
    fn does_not_match_checkout_file() {
        // `git checkout -- <file>` restores a file; not branch-mutating.
        assert!(!is_branch_mutating_git("git checkout -- src/main.rs"));
    }

    #[test]
    fn matches_reset_hard() {
        assert!(is_branch_mutating_git("git reset --hard origin/main"));
    }

    #[test]
    fn does_not_match_soft_reset() {
        assert!(!is_branch_mutating_git("git reset --soft HEAD~1"));
        assert!(!is_branch_mutating_git("git reset HEAD~1"));
    }

    #[test]
    fn matches_cd_prefixed() {
        assert!(is_branch_mutating_git(
            "cd /d/qontinui-root/qontinui-runner && git switch main"
        ));
    }

    #[test]
    fn matches_dash_c_form() {
        assert!(is_branch_mutating_git("git -C /repo switch main"));
        assert!(is_branch_mutating_git("git -C /repo reset --hard HEAD"));
    }

    #[test]
    fn does_not_match_unrelated() {
        assert!(!is_branch_mutating_git("git status"));
        assert!(!is_branch_mutating_git("git log --oneline"));
        assert!(!is_branch_mutating_git("ls -la"));
        assert!(!is_branch_mutating_git("git commit -m 'switch things up'"));
    }
}

//! Commit ↔ session lineage push-report trigger (Population path 2 of plan
//! `2026-06-07-coord-commit-session-lineage.md`).
//!
//! ## Trigger
//!
//! The most robust runner-observed signal that a session produced commits is a
//! **successful `git push`** in the session's working directory. A push means
//! SHAs left the machine — exactly the lineage event coord wants to record. We
//! piggyback on the existing [`super::transcript_watcher`] tail loop, which
//! already parses every new JSONL line of each interactive Claude session for
//! `tool_use` blocks. Here we parse `Bash` `tool_use` blocks instead of
//! Edit/Write/MultiEdit, detect `git push` invocations, and (best-effort)
//! enumerate the pushed SHAs to enqueue a coord outbox report.
//!
//! Why a push and not a commit: a commit can be amended/rebased/dropped before
//! it ever reaches a remote, so committing is a noisier, less durable signal. A
//! push is the point at which a SHA becomes a shared, attributable fact — which
//! is precisely what `coord.commit_lineage` records.
//!
//! ## Wire path
//!
//! Detection enqueues a `commit_report` row on the shared
//! [`crate::session::local_store::OutboxWriter`] (via
//! [`crate::claude_session::coord_register::AiCoordRegistrar::report_commits`]).
//! The existing `CoordSync` drain loop POSTs it to
//! `POST /coord/commits/report {repo, branch, shas[]}`. Coord resolves the
//! session **server-side** from `(repo, branch)`; the body carries NO session
//! id (plan §Population path 2).
//!
//! ## Dedup
//!
//! Re-reporting the same SHAs is harmless (coord is `ON CONFLICT DO NOTHING`),
//! but we still suppress no-op work: a process-global cache remembers the last
//! HEAD SHA reported per `(repo, branch)`. A subsequent push that hasn't moved
//! HEAD enqueues nothing.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

use once_cell::sync::Lazy;
use tracing::debug;

/// Default-ON env gate (mirrors `coord_register::registration_enabled`). Any of
/// `0` / `false` / `off` (case-insensitive) disables push-report; anything else
/// (including unset) leaves it ON.
pub fn report_enabled() -> bool {
    match std::env::var("QONTINUI_COMMIT_LINEAGE_REPORT") {
        Ok(v) => !matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "off"
        ),
        Err(_) => true,
    }
}

/// How many recent SHAs to enumerate + report per push. Coord dedups, so an
/// upper bound here just caps the body size; the branch's most recent commits
/// are the ones a push most likely just delivered.
const SHA_WINDOW: usize = 25;

/// A detected `git push` observation extracted from a single Bash `tool_use`
/// block. Pure parse output — no git has been run yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushObservation {
    /// Working directory to run git in. Resolution order: a `cd "<dir>" &&`
    /// prefix in the command (overrides), else the transcript record's
    /// top-level `cwd`.
    pub working_dir: String,
}

/// Process-global dedup cache: `(repo, branch)` → last reported HEAD SHA.
static LAST_REPORTED: Lazy<Mutex<HashMap<(String, String), String>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

// ── Pure parsing ─────────────────────────────────────────────────────────────

/// Return true if a shell command string contains a `git push` invocation.
///
/// Tolerant of the common shapes the agent emits: `git push`,
/// `git push -u origin x`, `cd "<dir>" && git push …`, `git -C <dir> push`,
/// piped/redirected (`… | tail`, `2>&1`). Conservative: requires the literal
/// token sequence `git … push` so `git pushd`-style false positives and
/// `git log` don't match. Does NOT match `--dry-run` pushes.
pub fn command_is_git_push(command: &str) -> bool {
    // Split on shell separators so each sub-command is checked independently
    // (`cd x && git push` → ["cd x", "git push"]).
    for segment in command.split(['&', ';', '|', '\n']) {
        let toks: Vec<&str> = segment.split_whitespace().collect();
        // `git` must be the FIRST token of the segment — otherwise `echo git
        // push` or `git push` mentioned as an argument would match. A leading
        // env-assignment (`FOO=bar git push`) is tolerated by skipping tokens
        // that contain `=` and no `/` before they could be a path.
        let mut start = 0;
        while start < toks.len() && toks[start].contains('=') {
            start += 1;
        }
        if toks.get(start) != Some(&"git") {
            continue;
        }
        let git_idx = start;
        // Find the first non-flag, non-`-C <path>` token after `git`.
        let mut i = git_idx + 1;
        while i < toks.len() {
            let t = toks[i];
            if t == "-C" {
                i += 2; // skip `-C <path>`
                continue;
            }
            if t.starts_with('-') {
                i += 1;
                continue;
            }
            // First subcommand token.
            if t == "push" {
                // Exclude dry-runs — they push nothing.
                if segment.contains("--dry-run") {
                    return false;
                }
                return true;
            }
            break;
        }
    }
    false
}

/// Extract the working directory from a shell command, preferring a leading
/// `cd "<dir>" &&` (or `cd <dir> &&`) prefix, else `git -C <dir>`, else falling
/// back to the supplied transcript `cwd`. Strips surrounding quotes.
pub fn resolve_working_dir(command: &str, transcript_cwd: &str) -> String {
    // 1. `cd <dir> &&` prefix.
    for segment in command.split("&&") {
        let seg = segment.trim();
        if let Some(rest) = seg.strip_prefix("cd ") {
            let dir = unquote(rest.trim());
            if !dir.is_empty() {
                return dir;
            }
        }
    }
    // 2. `git -C <dir>`.
    let toks: Vec<&str> = command.split_whitespace().collect();
    if let Some(idx) = toks.iter().position(|t| *t == "-C") {
        if let Some(dir) = toks.get(idx + 1) {
            let d = unquote(dir);
            if !d.is_empty() {
                return d;
            }
        }
    }
    // 3. Transcript cwd fallback.
    transcript_cwd.to_string()
}

/// Strip one layer of matching single/double quotes.
fn unquote(s: &str) -> String {
    let s = s.trim();
    let bytes = s.as_bytes();
    if bytes.len() >= 2
        && ((bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[bytes.len() - 1] == b'\''))
    {
        return s[1..s.len() - 1].to_string();
    }
    s.to_string()
}

/// Walk a parsed `{type:"assistant"}` JSONL record and emit a [`PushObservation`]
/// for each Bash `tool_use` block whose command is a `git push`. Pure (no I/O).
/// Uses the record's top-level `cwd` as the working-dir fallback.
pub fn extract_push_observations(record: &serde_json::Value) -> Vec<PushObservation> {
    let transcript_cwd = record.get("cwd").and_then(|c| c.as_str()).unwrap_or("");

    let Some(content) = record
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_array())
    else {
        return Vec::new();
    };

    let mut out = Vec::new();
    for block in content {
        if block.get("type").and_then(|t| t.as_str()) != Some("tool_use") {
            continue;
        }
        if block.get("name").and_then(|n| n.as_str()) != Some("Bash") {
            continue;
        }
        let Some(command) = block
            .get("input")
            .and_then(|i| i.get("command"))
            .and_then(|c| c.as_str())
        else {
            continue;
        };
        if command_is_git_push(command) {
            out.push(PushObservation {
                working_dir: resolve_working_dir(command, transcript_cwd),
            });
        }
    }
    out
}

/// Convenience: parse one JSONL line and return push observations (empty for
/// non-assistant records / malformed JSON — never panics).
pub fn parse_line_for_pushes(line: &str) -> Vec<PushObservation> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    let Ok(record) = serde_json::from_str::<serde_json::Value>(trimmed) else {
        return Vec::new();
    };
    if record.get("type").and_then(|t| t.as_str()) != Some("assistant") {
        return Vec::new();
    }
    extract_push_observations(&record)
}

// ── Git enumeration (impure) ──────────────────────────────────────────────────

/// Resolved facts about a pushed branch, ready to hand to the coord report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedPush {
    /// `owner/repo` derived from `git remote get-url origin`.
    pub repo: String,
    /// Current branch name.
    pub branch: String,
    /// Recent SHAs on the branch (newest first), capped at [`SHA_WINDOW`].
    pub shas: Vec<String>,
}

/// Normalize a `git remote get-url origin` value to `owner/repo`. Handles both
/// SSH (`git@github.com:owner/repo.git`) and HTTPS
/// (`https://github.com/owner/repo.git`) forms; strips a trailing `.git`.
/// Returns `None` for shapes we don't recognise.
pub fn parse_repo_full_name(remote_url: &str) -> Option<String> {
    let url = remote_url.trim();
    let tail = if let Some(idx) = url.find('@') {
        // SSH: git@github.com:owner/repo.git
        let after_at = &url[idx + 1..];
        after_at.split_once(':').map(|(_, p)| p)?
    } else if let Some(pos) = url.find("://") {
        // HTTPS: https://github.com/owner/repo.git
        let after_scheme = &url[pos + 3..];
        let path = after_scheme.split_once('/').map(|(_, p)| p)?;
        path
    } else {
        // scp-like without scheme, or bare path: host:owner/repo
        url.split_once(':').map(|(_, p)| p).unwrap_or(url)
    };
    let cleaned = tail.trim_end_matches('/').trim_end_matches(".git");
    // Require at least `owner/repo`.
    let parts: Vec<&str> = cleaned.split('/').filter(|s| !s.is_empty()).collect();
    if parts.len() < 2 {
        return None;
    }
    let n = parts.len();
    Some(format!("{}/{}", parts[n - 2], parts[n - 1]))
}

/// Run a git subcommand in `dir`, returning trimmed stdout on success.
fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let output = crate::process_helpers::no_window("git")
        .current_dir(dir)
        .args(args)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Resolve repo full_name, branch, and the recent-SHA window for a working dir.
/// Returns `None` if the dir isn't a git repo, has no origin remote, or is in a
/// detached-HEAD / unparseable state.
pub fn resolve_push(working_dir: &str) -> Option<ResolvedPush> {
    let dir = PathBuf::from(working_dir);
    if working_dir.trim().is_empty() {
        return None;
    }
    let remote = git(&dir, &["remote", "get-url", "origin"])?;
    let repo = parse_repo_full_name(&remote)?;

    let branch = git(&dir, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    if branch.is_empty() || branch == "HEAD" {
        // Detached HEAD — no branch to attribute against. Skip.
        return None;
    }

    let log = git(
        &dir,
        &["log", "--format=%H", &format!("-n{SHA_WINDOW}"), &branch],
    )?;
    let shas: Vec<String> = log
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    if shas.is_empty() {
        return None;
    }

    Some(ResolvedPush { repo, branch, shas })
}

/// Dedup gate: returns `true` (report) only if the branch's HEAD SHA has moved
/// since the last report for this `(repo, branch)`. Records the new HEAD on a
/// `true` so the next identical push is a no-op.
pub fn should_report(repo: &str, branch: &str, head_sha: &str) -> bool {
    let mut cache = LAST_REPORTED.lock().unwrap_or_else(|e| e.into_inner());
    let key = (repo.to_string(), branch.to_string());
    match cache.get(&key) {
        Some(prev) if prev == head_sha => false,
        _ => {
            cache.insert(key, head_sha.to_string());
            true
        }
    }
}

/// Test-only: clear the dedup cache so tests don't bleed into each other.
#[cfg(test)]
pub fn reset_dedup_for_test() {
    LAST_REPORTED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear();
}

/// Full pipeline for one push observation: resolve git facts, apply dedup, and
/// (when warranted) report via the registrar. Best-effort — logs and returns on
/// any miss. Synchronous git calls are cheap and run on the tail task's thread.
pub fn handle_push_observation(
    obs: &PushObservation,
    registrar: &crate::claude_session::coord_register::AiCoordRegistrar,
) {
    if !report_enabled() {
        return;
    }
    let Some(resolved) = resolve_push(&obs.working_dir) else {
        debug!(
            "commit_report: could not resolve git push in {} — skipping",
            obs.working_dir
        );
        return;
    };
    let head = &resolved.shas[0];
    if !should_report(&resolved.repo, &resolved.branch, head) {
        debug!(
            "commit_report: {}@{} HEAD {} already reported — no-op",
            resolved.repo, resolved.branch, head
        );
        return;
    }
    registrar.report_commits(&resolved.repo, &resolved.branch, resolved.shas);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn detects_plain_git_push() {
        assert!(command_is_git_push("git push"));
        assert!(command_is_git_push("git push -u origin feat/x"));
        assert!(command_is_git_push(
            "cd \"C:/repo\" && git push -u origin feat/x 2>&1 | tail -10"
        ));
        assert!(command_is_git_push("git -C /some/dir push origin main"));
    }

    #[test]
    fn ignores_non_push_git() {
        assert!(!command_is_git_push("git log --oneline"));
        assert!(!command_is_git_push("git commit -m 'x'"));
        assert!(!command_is_git_push("git status && echo done"));
        // pushd is not push
        assert!(!command_is_git_push("pushd /tmp"));
        // dry-run pushes nothing
        assert!(!command_is_git_push("git push --dry-run origin main"));
    }

    #[test]
    fn ignores_non_git_commands() {
        assert!(!command_is_git_push("npm run build"));
        assert!(!command_is_git_push("echo git push"));
    }

    #[test]
    fn resolve_dir_prefers_cd_prefix() {
        assert_eq!(
            resolve_working_dir("cd \"C:/repo/x\" && git push", "C:/fallback"),
            "C:/repo/x"
        );
        assert_eq!(
            resolve_working_dir("cd /home/u/proj && git push", "/fallback"),
            "/home/u/proj"
        );
    }

    #[test]
    fn resolve_dir_uses_dash_c() {
        assert_eq!(
            resolve_working_dir("git -C /repo/y push origin main", "/fallback"),
            "/repo/y"
        );
    }

    #[test]
    fn resolve_dir_falls_back_to_cwd() {
        assert_eq!(resolve_working_dir("git push", "C:/the/cwd"), "C:/the/cwd");
    }

    #[test]
    fn extract_pushes_from_assistant_record() {
        let record = json!({
            "type": "assistant",
            "cwd": "C:/Users/x/repo",
            "message": {
                "content": [
                    {"type": "text", "text": "pushing now"},
                    {"type": "tool_use", "name": "Bash", "input": {"command": "git push -u origin feat/y"}},
                    {"type": "tool_use", "name": "Read", "input": {"file_path": "/x"}},
                    {"type": "tool_use", "name": "Bash", "input": {"command": "git status"}}
                ]
            }
        });
        let pushes = extract_push_observations(&record);
        assert_eq!(pushes.len(), 1);
        assert_eq!(pushes[0].working_dir, "C:/Users/x/repo");
    }

    #[test]
    fn extract_pushes_honors_cd_prefix_over_cwd() {
        let record = json!({
            "type": "assistant",
            "cwd": "C:/wrong",
            "message": {
                "content": [
                    {"type": "tool_use", "name": "Bash",
                     "input": {"command": "cd \"C:/right/repo\" && git push 2>&1 | tail -5"}}
                ]
            }
        });
        let pushes = extract_push_observations(&record);
        assert_eq!(pushes.len(), 1);
        assert_eq!(pushes[0].working_dir, "C:/right/repo");
    }

    #[test]
    fn parse_line_ignores_user_and_malformed() {
        assert!(parse_line_for_pushes("not json").is_empty());
        assert!(
            parse_line_for_pushes(r#"{"type":"user","message":{"content":"git push"}}"#).is_empty()
        );
        assert!(parse_line_for_pushes("").is_empty());
    }

    #[test]
    fn parse_repo_full_name_ssh_and_https() {
        assert_eq!(
            parse_repo_full_name("git@github.com:qontinui/qontinui-runner.git").as_deref(),
            Some("qontinui/qontinui-runner")
        );
        assert_eq!(
            parse_repo_full_name("https://github.com/qontinui/qontinui-coord.git").as_deref(),
            Some("qontinui/qontinui-coord")
        );
        assert_eq!(
            parse_repo_full_name("https://github.com/qontinui/qontinui-web").as_deref(),
            Some("qontinui/qontinui-web")
        );
        assert_eq!(parse_repo_full_name("not-a-url").as_deref(), None);
        assert_eq!(
            parse_repo_full_name("https://host/onlyone").as_deref(),
            None
        );
    }

    #[test]
    fn dedup_suppresses_repeat_same_head() {
        reset_dedup_for_test();
        assert!(should_report("o/r", "main", "sha1"), "first report fires");
        assert!(
            !should_report("o/r", "main", "sha1"),
            "same HEAD is a no-op"
        );
        assert!(
            should_report("o/r", "main", "sha2"),
            "moved HEAD reports again"
        );
        // Different branch is independent.
        assert!(should_report("o/r", "feat/x", "sha2"));
    }
}

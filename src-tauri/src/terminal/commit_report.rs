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
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use once_cell::sync::Lazy;
use tracing::{debug, warn};

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

/// Default hard cap on a single `git` invocation. Overridable via
/// `QONTINUI_COMMIT_LINEAGE_GIT_TIMEOUT_SECS` (clamped to 1..=120).
const GIT_TIMEOUT_DEFAULT_SECS: u64 = 10;

/// Resolve the per-invocation git timeout.
fn git_timeout() -> Duration {
    let secs = std::env::var("QONTINUI_COMMIT_LINEAGE_GIT_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(GIT_TIMEOUT_DEFAULT_SECS)
        .clamp(1, 120);
    Duration::from_secs(secs)
}

/// Run a git subcommand in `dir`, returning trimmed stdout on success.
///
/// **Time-bounded by contract.** `Command::output()` used to be called here
/// with no timeout at all, so a `git` that never returns — an `index.lock`
/// held by another process, a credential prompt, an unreachable remote — held
/// the calling thread forever. Every caller of this function runs on a pool
/// thread, so "forever" meant one fewer thread in that pool per hung push, and
/// the 2026-08-23 wedge started exactly there. A timed-out git now returns
/// `None` (same as any other failure) after the child has been killed and
/// reaped.
fn git(dir: &Path, args: &[&str]) -> Option<String> {
    git_with_timeout(dir, args, git_timeout())
}

/// [`git`] with an explicit budget.
fn git_with_timeout(dir: &Path, args: &[&str], timeout: Duration) -> Option<String> {
    let mut cmd = crate::process_helpers::no_window("git");
    cmd.current_dir(dir).args(args);
    run_git_command(cmd, dir, args, timeout)
}

/// Execute an already-built git `Command` under `timeout`, mapping every
/// failure mode (non-zero exit, spawn error, timeout) to `None`.
///
/// Split out from [`git_with_timeout`] so a test can hand it a command that
/// genuinely never returns — the hang this function exists to survive.
fn run_git_command(
    cmd: std::process::Command,
    dir: &Path,
    args: &[&str],
    timeout: Duration,
) -> Option<String> {
    match crate::process_helpers::run_with_timeout(cmd, timeout) {
        Ok(crate::process_helpers::TimedOutput::Completed(output)) => {
            if !output.status.success() {
                return None;
            }
            Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
        }
        Ok(crate::process_helpers::TimedOutput::TimedOut { pid, reaped }) => {
            // WARN, not debug: a silent timeout would just relocate the
            // mystery. The dir + argv are what make it actionable.
            warn!(
                dir = %dir.display(),
                args = ?args,
                timeout_secs = timeout.as_secs(),
                child_pid = pid,
                reaped,
                "commit_report: git timed out and was killed — treating as a failed lookup"
            );
            None
        }
        Err(e) => {
            debug!(
                "commit_report: git {:?} in {} could not run: {}",
                args,
                dir.display(),
                e
            );
            None
        }
    }
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

// ── Bounded fan-out ──────────────────────────────────────────────────────────
//
// **The problem.** The transcript tail loop used to do
// `for obs in pushes { spawn_blocking(|| handle_push_observation(..)) }` — one
// blocking-pool task per transcript line, with no cap of any kind. The bound
// was the transcript's line rate, i.e. none. Combined with an untimed `git`
// (fixed above) that is a direct route to blocking-pool exhaustion, which is
// stage 1 of the 2026-08-23 wedge.
//
// **The choice: one dedicated OS thread behind a bounded queue** — NOT a
// semaphore over `spawn_blocking`, and NOT tokio's blocking pool at all.
//
//   * *Why off the blocking pool.* The pool is shared with the transcript
//     scan, `tokio::fs`, and every other `spawn_blocking` in the process.
//     Anything that can hang must not be able to consume it. A private thread
//     caps the blast radius of a pathological git at exactly one thread.
//   * *Why one worker and not N.* Git enumeration is inherently serial per
//     repo and the event rate is one burst per `git push` — a human-scale
//     event. Serial costs nothing real, and with the 10s cap the worst case
//     for the queue is `depth × 3 × 10s` of lag on a best-effort report.
//   * *Why a BOUNDED queue with `try_send` (drop) rather than blocking send.*
//     A blocking send would push the back-pressure right back onto the caller
//     — the transcript tail loop — which is what we are protecting. Dropping
//     is safe here: `report_commits` is best-effort and coord dedups, so a
//     dropped observation costs at most one lineage row that the next push
//     re-reports. Drops are counted and WARNed, never silent.

/// Queue depth for pending git enumerations. Small on purpose: a backlog this
/// deep already means git is pathological, and queueing more just delays the
/// WARN that says so.
const PUSH_QUEUE_CAPACITY: usize = 32;

type Job = Box<dyn FnOnce() + Send + 'static>;

/// Bounded, single-worker dispatcher for the blocking git enumeration.
pub struct PushDispatcher {
    tx: SyncSender<Job>,
    accepted: Arc<AtomicU64>,
    dropped: Arc<AtomicU64>,
}

impl PushDispatcher {
    /// Start a dispatcher with `capacity` queue slots and exactly one worker
    /// thread. Fail-open: if the thread cannot be spawned the receiver is
    /// dropped, every dispatch is counted as dropped, and nothing hangs.
    pub fn new(capacity: usize) -> Self {
        let (tx, rx) = sync_channel::<Job>(capacity);
        let spawned = std::thread::Builder::new()
            .name("commit-report-git".to_string())
            .spawn(move || {
                while let Ok(job) = rx.recv() {
                    job();
                }
            });
        if let Err(e) = spawned {
            warn!("commit_report: could not start the git worker thread ({e}) — push reports disabled");
        }
        Self {
            tx,
            accepted: Arc::new(AtomicU64::new(0)),
            dropped: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Enqueue `job` for the worker. NEVER blocks: a full queue (or a dead
    /// worker) drops the job and returns `false`.
    pub fn try_dispatch(&self, job: impl FnOnce() + Send + 'static) -> bool {
        match self.tx.try_send(Box::new(job)) {
            Ok(()) => {
                self.accepted.fetch_add(1, Ordering::Relaxed);
                true
            }
            Err(TrySendError::Full(_)) => {
                let n = self.dropped.fetch_add(1, Ordering::Relaxed) + 1;
                warn!(
                    queue_capacity = PUSH_QUEUE_CAPACITY,
                    dropped_total = n,
                    "commit_report: git enumeration queue full — dropping a push observation                      (coord dedups; the next push re-reports it)"
                );
                false
            }
            Err(TrySendError::Disconnected(_)) => {
                self.dropped.fetch_add(1, Ordering::Relaxed);
                false
            }
        }
    }

    /// Jobs handed to the worker.
    pub fn accepted(&self) -> u64 {
        self.accepted.load(Ordering::Relaxed)
    }

    /// Jobs refused because the queue was full (or the worker is gone).
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

/// The process-wide dispatcher. Lazily started on the first observed push.
static PUSH_DISPATCHER: Lazy<PushDispatcher> =
    Lazy::new(|| PushDispatcher::new(PUSH_QUEUE_CAPACITY));

/// Hand one push observation to the bounded git worker.
///
/// This is what the transcript tail loop calls, in place of an unbounded
/// `spawn_blocking` per line. Returns whether the observation was queued.
pub fn dispatch_push_observation(
    obs: PushObservation,
    registrar: Arc<crate::claude_session::coord_register::AiCoordRegistrar>,
) -> bool {
    PUSH_DISPATCHER.try_dispatch(move || {
        handle_push_observation(&obs, &registrar);
    })
}

/// Observability for the process-wide dispatcher.
pub fn dispatch_stats() -> (u64, u64) {
    (PUSH_DISPATCHER.accepted(), PUSH_DISPATCHER.dropped())
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
    // ── Item 1: bounded + time-bounded git fan-out ───────────────────────

    /// The fan-out cap. Feeding far more observations than the queue can hold
    /// must NOT block the caller and must NOT create a task per observation:
    /// everything beyond `capacity` (+ the one job the worker has in hand) is
    /// refused and counted. With an unbounded fan-out this assertion fails
    /// because nothing is ever dropped.
    #[test]
    fn dispatch_is_bounded_and_never_blocks_the_caller() {
        const CAPACITY: usize = 4;
        const OFFERED: u64 = 500;

        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let release_rx = Arc::new(Mutex::new(release_rx));
        let d = PushDispatcher::new(CAPACITY);

        let started = std::time::Instant::now();
        for _ in 0..OFFERED {
            let rx = release_rx.clone();
            // Every accepted job parks until the test releases it, so the
            // queue really does fill.
            d.try_dispatch(move || {
                let _ = rx.lock().unwrap_or_else(|e| e.into_inner()).recv();
            });
        }
        let elapsed = started.elapsed();

        assert!(
            elapsed < std::time::Duration::from_secs(5),
            "offering {OFFERED} observations blocked the caller for {elapsed:?} —              try_dispatch must never block"
        );
        assert_eq!(
            d.accepted() + d.dropped(),
            OFFERED,
            "every offer must be accounted for"
        );
        // Worker holds at most one job; the channel holds at most CAPACITY.
        assert!(
            d.accepted() <= CAPACITY as u64 + 1,
            "fan-out cap breached: {} accepted with capacity {CAPACITY}",
            d.accepted()
        );
        assert!(
            d.dropped() >= OFFERED - (CAPACITY as u64 + 1),
            "the overflow must be dropped and counted, got {}",
            d.dropped()
        );

        // Let the parked jobs go so the worker thread can exit with the sender.
        for _ in 0..(CAPACITY + 1) {
            let _ = release_tx.send(());
        }
    }

    /// A stand-in for a `git` that never returns (index.lock, credential
    /// prompt, unreachable remote).
    fn hung_git() -> std::process::Command {
        #[cfg(target_os = "windows")]
        {
            let mut c = crate::process_helpers::no_window("cmd.exe");
            c.args(["/C", "ping -n 60 127.0.0.1"]);
            c
        }
        #[cfg(not(target_os = "windows"))]
        {
            let mut c = crate::process_helpers::no_window("sh");
            c.args(["-c", "sleep 60"]);
            c
        }
    }

    /// The stage-1 contract: a git that hangs must surface as a failed lookup
    /// inside the budget, NOT as a parked thread. Remove the timeout from
    /// `run_git_command` and this test blocks for ~59s, blowing the elapsed
    /// assertion.
    #[test]
    fn a_hung_git_returns_none_inside_the_budget() {
        let budget = Duration::from_millis(400);
        let dir = std::env::temp_dir();
        let started = std::time::Instant::now();
        let out = run_git_command(hung_git(), &dir, &["rev-parse", "HEAD"], budget);
        let elapsed = started.elapsed();

        assert!(out.is_none(), "a timed-out git must not report a result");
        assert!(
            elapsed < budget * 8,
            "run_git_command blocked for {elapsed:?} against a {budget:?} budget"
        );
    }

    /// The wrapper must not have broken the happy path.
    #[test]
    fn a_real_git_still_answers_through_the_wrapper() {
        let dir = std::env::temp_dir();
        let v = git_with_timeout(&dir, &["--version"], Duration::from_secs(30));
        assert!(
            v.is_some_and(|v| v.to_lowercase().contains("git version")),
            "a trivial git must still succeed through the timeout wrapper"
        );
    }

    #[test]
    fn git_timeout_defaults_to_the_documented_budget() {
        if std::env::var("QONTINUI_COMMIT_LINEAGE_GIT_TIMEOUT_SECS").is_ok() {
            return; // ambient override — nothing to pin
        }
        assert_eq!(git_timeout(), Duration::from_secs(GIT_TIMEOUT_DEFAULT_SECS));
    }
}

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
//!
//! ## Sensitive agent actions (Phase 9)
//!
//! The same tail also classifies the Bash commands that act OUTSIDE coord —
//! force-pushes, ref deletions, `gh release create|delete`, `npm publish`,
//! `cargo publish` — and, once their `tool_result` shows they succeeded,
//! notifies coord (`POST /coord/agent-notifications`). See the "Sensitive
//! agent actions" section below (plan
//! `2026-09-18-notifications-are-agent-actions-and-alerts-are-agent-work`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

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

// ── Sensitive agent actions (Phase 9) ─────────────────────────────────────────
//
// Plan `2026-09-18-notifications-are-agent-actions-and-alerts-are-agent-work`
// Phase 9 — "actions agents take outside coord still notify".
//
// Coord emits `agent_took_sensitive_action` for the sensitive steps that pass
// THROUGH it (its own force-pushes, force-merges, dial loosenings…). An agent
// that runs `git push --force`, `git push --delete`, `gh release create`,
// `npm publish` or `cargo publish` in its own shell goes straight to GitHub or
// a registry, and coord never sees it. This section is the runner's half of
// the notify-after-action contract for exactly those commands:
//
// 1. **Classify** the Bash `tool_use` command ([`classify_sensitive_command`]),
//    with the same first-word-of-a-segment rule [`command_is_git_push`] uses,
//    so `echo "git push --force"` is text, not a push.
// 2. **Hold** the classification in a bounded pending map keyed on the
//    `tool_use_id` ([`SensitiveActionTracker`]). The transcript parser is
//    stateless per record and the `tool_result` arrives in a LATER `user`
//    record, so something has to remember the command in between. The map is
//    owned by the transcript watcher's per-session tail loop, capped at
//    [`PENDING_ACTION_CAPACITY`] entries and expires entries after
//    [`PENDING_ACTION_TTL`].
// 3. **Emit only what happened**: positive evidence in the output (a
//    `(forced update)` / `[deleted]` line inside a `To <url>` block, npm's
//    `+ name@ver`, cargo's `Published`) always counts; anything inferred
//    from the command alone counts only when the result is not `is_error`,
//    was not interrupted and did not run in the background. The output is
//    read ONCE per tool call, and a `From <url>` (fetch) table never counts.
// 4. **Filter noise (plan design decision D8)**: a force-push or delete of
//    the agent's OWN non-default working branch is routine and is not
//    notified; the default branch, tags, `release/*`/`hotfix/*` and any other
//    branch are. See [`ref_action_is_notable`]. Unknown never suppresses.
// 5. **POST** through the same session outbox → `CoordSync` drain → device-JWT
//    path the commit report uses, to `POST /coord/agent-notifications` (the
//    HTTP twin of `coord_notify_sensitive_action`). The drain arm lives in
//    `session::coord_sync` (`agent_notification`).

/// Default-ON env gate for the sensitive-action notifier, same shape as
/// [`report_enabled`]. Any of `0` / `false` / `off` (case-insensitive) in
/// `QONTINUI_AGENT_ACTION_NOTIFY` disables it; anything else leaves it ON.
pub fn action_notify_enabled() -> bool {
    action_notify_enabled_from(
        std::env::var("QONTINUI_AGENT_ACTION_NOTIFY")
            .ok()
            .as_deref(),
    )
}

/// Log the notifier gate once, at transcript-watcher start, so a box whose
/// notifications never arrive shows in its log whether the gate was off.
pub fn log_action_notify_gate() {
    let raw = std::env::var("QONTINUI_AGENT_ACTION_NOTIFY").ok();
    tracing::info!(
        env_value = raw.as_deref().unwrap_or("<unset>"),
        enabled = action_notify_enabled_from(raw.as_deref()),
        "commit_report: sensitive-action notifier gate (QONTINUI_AGENT_ACTION_NOTIFY)"
    );
}

fn action_notify_enabled_from(value: Option<&str>) -> bool {
    match value {
        Some(v) => !matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "off"
        ),
        None => true,
    }
}

/// At most this many unresolved sensitive `tool_use`s are remembered per
/// transcript. A session has at most a handful in flight; 256 only binds for a
/// transcript whose results never arrive (a killed CLI), and then the oldest
/// entry is evicted rather than the map growing.
pub const PENDING_ACTION_CAPACITY: usize = 256;

/// A pending `tool_use` whose `tool_result` has not arrived after this long is
/// dropped: the command was interrupted, the session died, or the result line
/// was never written. Ten minutes comfortably covers a slow `cargo publish`
/// (which verifies by building) without holding a dead entry forever.
pub const PENDING_ACTION_TTL: Duration = Duration::from_secs(10 * 60);

/// How many emitted `tool_use_id`s the process remembers (see
/// `EMITTED_TOOL_USES`), so one `tool_use` seen in two transcript files — or
/// in one transcript re-read from offset 0 — notifies once.
const EMITTED_MEMORY: usize = 4096;

/// Longest `artifact` / `undo` string sent. Coord caps posted fields at 2000
/// chars; the runner stays well under so a pathological ref name cannot turn
/// the notification into a 400 (which would LOSE it).
const MAX_NOTIFICATION_FIELD_CHARS: usize = 500;

/// Coord's `detail.action` discriminator, restricted to the three verbs a
/// shell command can be classified as.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SensitiveAction {
    /// A ref was force-updated, discarding commits reachable before.
    ForcePush,
    /// A remote ref (or a GitHub release) was deleted.
    Delete,
    /// An artifact was published outward — a GitHub release, an npm version,
    /// a crate version.
    Publish,
}

impl SensitiveAction {
    /// Coord's wire string (`AgentAction::as_str`).
    pub fn as_str(self) -> &'static str {
        match self {
            SensitiveAction::ForcePush => "force_push",
            SensitiveAction::Delete => "delete",
            SensitiveAction::Publish => "publish",
        }
    }
}

/// Coord's `detail.reversible` wire strings (`Reversibility::as_str`).
pub mod reversibility {
    /// Cannot be undone — the prior state is gone.
    pub const NO: &str = "no";
    /// Undone by moving forward (re-create, re-publish).
    pub const ROLL_FORWARD: &str = "roll-forward";
    /// Undone by restoring the prior state from something that still holds it.
    pub const RESTORE: &str = "restore";
}

/// `git push` with a force and/or delete component, parsed from the command.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GitPushIntent {
    /// First positional argument — a remote name or a URL. `None` means
    /// git's default remote.
    pub remote: Option<String>,
    /// `Some(refs)` when the push force-updates: the DESTINATION refs named by
    /// `+refspec`s or covered by a `--force`/`-f`/`--force-with-lease` flag.
    /// `Some(vec![])` means "forced, refs unnamed" (the current branch under
    /// `push.default`).
    pub force: Option<Vec<String>>,
    /// `Some(refs)` when the push deletes: `--delete`/`-d` refs, or `:ref`
    /// refspecs. `Some(vec![])` means "deletes, refs unnamed".
    pub delete: Option<Vec<String>>,
    /// `--tags` / `--mirror` — the push covers tags, which the D8 noise rule
    /// always notifies.
    pub tags: bool,
}

/// One classified sensitive command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SensitiveCommand {
    /// `git push` that force-updates and/or deletes a ref.
    GitPush(GitPushIntent),
    /// `gh release create` (non-draft) or `gh release delete`.
    GhRelease {
        /// `true` for `delete`, `false` for `create`.
        delete: bool,
        /// The release tag (first positional), when given.
        tag: Option<String>,
        /// `-R/--repo owner/repo`, when given.
        repo: Option<String>,
        /// `gh release delete --cleanup-tag` also deletes the git tag.
        cleanup_tag: bool,
    },
    /// `npm publish` (not `--dry-run`).
    NpmPublish {
        /// Positional `<folder|tarball>`, when given.
        spec: Option<String>,
    },
    /// `cargo publish` (not `--dry-run`).
    CargoPublish {
        /// `-p/--package`, when given.
        package: Option<String>,
    },
}

// ── Shell tokenizing ─────────────────────────────────────────────────────────

/// Split a shell command into segments of words — the quote-aware form of the
/// split [`command_is_git_push`] does.
///
/// The RULE is the same one that detector applies: a command is only what
/// starts a segment (`;`, `&`, `&&`, `|`, `||`, newline, `(`, `)`), so
/// `echo git push --force` is an `echo`. The difference is that quoting is
/// honoured — a separator inside quotes does not start a segment and quotes
/// are stripped from words — because the commands this classifier looks for
/// are routinely written inside commit messages, PR bodies and `echo`s, where
/// the unquoted split would find a `git push --force` that never ran. For the
/// same reason heredoc bodies are skipped, and `#` comments are dropped.
/// Redirections stay attached to their word (`2>&1` is one word, not a
/// segment break).
pub(crate) fn shell_segments(command: &str) -> Vec<Vec<String>> {
    struct Lexer {
        segments: Vec<Vec<String>>,
        words: Vec<String>,
        cur: String,
        in_word: bool,
    }
    impl Lexer {
        fn end_word(&mut self) {
            if self.in_word {
                self.words.push(std::mem::take(&mut self.cur));
                self.in_word = false;
            }
        }
        fn end_segment(&mut self) {
            self.end_word();
            if !self.words.is_empty() {
                self.segments.push(std::mem::take(&mut self.words));
            }
        }
    }

    let chars: Vec<char> = command.chars().collect();
    let n = chars.len();
    let mut lx = Lexer {
        segments: Vec::new(),
        words: Vec::new(),
        cur: String::new(),
        in_word: false,
    };
    // Heredoc delimiters opened on the current line: (delimiter, strip_tabs).
    let mut heredocs: Vec<(String, bool)> = Vec::new();
    let mut i = 0;
    while i < n {
        let c = chars[i];
        match c {
            '\'' => {
                lx.in_word = true;
                i += 1;
                while i < n && chars[i] != '\'' {
                    lx.cur.push(chars[i]);
                    i += 1;
                }
                i += 1; // closing quote (or end)
            }
            '"' => {
                lx.in_word = true;
                i += 1;
                while i < n && chars[i] != '"' {
                    if chars[i] == '\\'
                        && i + 1 < n
                        && matches!(chars[i + 1], '"' | '\\' | '$' | '`')
                    {
                        lx.cur.push(chars[i + 1]);
                        i += 2;
                        continue;
                    }
                    lx.cur.push(chars[i]);
                    i += 1;
                }
                i += 1;
            }
            '\\' => {
                if i + 1 < n {
                    if chars[i + 1] != '\n' {
                        lx.cur.push(chars[i + 1]);
                        lx.in_word = true;
                    }
                    i += 2; // backslash-newline is a line continuation
                } else {
                    i += 1;
                }
            }
            ' ' | '\t' | '\r' => {
                lx.end_word();
                i += 1;
            }
            '\n' => {
                lx.end_segment();
                i += 1;
                // Skip the bodies of any heredocs opened on the line just ended.
                for (delim, strip_tabs) in std::mem::take(&mut heredocs) {
                    loop {
                        if i >= n {
                            break;
                        }
                        let start = i;
                        while i < n && chars[i] != '\n' {
                            i += 1;
                        }
                        let line: String = chars[start..i].iter().collect();
                        i += 1; // the newline
                        let line = line.trim_end_matches('\r');
                        let line = if strip_tabs {
                            line.trim_start_matches('\t')
                        } else {
                            line
                        };
                        if line == delim {
                            break;
                        }
                    }
                }
            }
            ';' | '|' | '(' | ')' => {
                lx.end_segment();
                i += 1;
            }
            '&' => {
                let redirect_target =
                    lx.in_word && (lx.cur.ends_with('>') || lx.cur.ends_with('<'));
                let redirect_both = i + 1 < n && chars[i + 1] == '>';
                if redirect_target || redirect_both {
                    // `2>&1` / `&>file` — part of a redirection word.
                    lx.cur.push('&');
                    lx.in_word = true;
                } else {
                    lx.end_segment();
                }
                i += 1;
            }
            '#' if !lx.in_word => {
                while i < n && chars[i] != '\n' {
                    i += 1;
                }
            }
            '<' if i + 2 < n && chars[i + 1] == '<' && chars[i + 2] == '<' => {
                // `<<<word` is a here-STRING: a redirection word, not a
                // heredoc. Consume all three `<` so the guard below never
                // sees its tail as `<<`.
                lx.cur.push_str("<<<");
                lx.in_word = true;
                i += 3;
            }
            '<' if i + 1 < n && chars[i + 1] == '<' => {
                // Heredoc operator `<<DELIM` / `<<-DELIM` / `<<'DELIM'`.
                lx.end_word();
                i += 2;
                let mut strip_tabs = false;
                if i < n && chars[i] == '-' {
                    strip_tabs = true;
                    i += 1;
                }
                while i < n && (chars[i] == ' ' || chars[i] == '\t') {
                    i += 1;
                }
                let mut delim = String::new();
                while i < n && !matches!(chars[i], ' ' | '\t' | '\n' | ';' | '&' | '|' | ')') {
                    if !matches!(chars[i], '\'' | '"') {
                        delim.push(chars[i]);
                    }
                    i += 1;
                }
                if !delim.is_empty() {
                    heredocs.push((delim, strip_tabs));
                }
            }
            _ => {
                lx.cur.push(c);
                lx.in_word = true;
                i += 1;
            }
        }
    }
    lx.end_segment();
    lx.segments
}

/// `NAME=value` — a leading environment assignment, skipped when finding a
/// segment's program (the same tolerance [`command_is_git_push`] has).
fn is_env_assignment(word: &str) -> bool {
    match word.split_once('=') {
        Some((name, _)) => {
            !name.is_empty()
                && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                && !name.starts_with(|c: char| c.is_ascii_digit())
        }
        None => false,
    }
}

/// Whether `word` names the program `name`, tolerating a path prefix and a
/// Windows `.exe` / `.cmd` suffix (`/usr/bin/git`, `git.exe`, `npm.cmd`).
fn program_is(word: &str, name: &str) -> bool {
    let base = word.rsplit(['/', '\\']).next().unwrap_or(word);
    let base = base
        .strip_suffix(".exe")
        .or_else(|| base.strip_suffix(".cmd"))
        .unwrap_or(base);
    base == name
}

/// A redirection word: `>f`, `2>&1`, `&>f`, `<f`, `>>f`.
fn is_redirection(word: &str) -> bool {
    let rest = word.trim_start_matches(|c: char| c.is_ascii_digit());
    rest.starts_with('>') || rest.starts_with('<') || word.starts_with("&>")
}

/// A redirection OPERATOR with its target in the next word (`>` then `file`).
fn redirection_takes_next(word: &str) -> bool {
    is_redirection(word) && (word.ends_with('>') || word.ends_with('<'))
}

/// The words of a segment from its PROGRAM on: leading env assignments,
/// shell keywords (`if`, `then`, `do`, `else`, `{`, `!`, `time`, …) and
/// transparent wrappers with their own arguments (`timeout <dur>`,
/// `env VAR=… [-i] [-u NAME]`, `nohup`, `command`, `exec`) are skipped, so
/// `if git push -f; then …` and `timeout 60 git push -f` are pushes.
fn command_words(segment: &[String]) -> &[String] {
    let n = segment.len();
    let mut i = 0;
    loop {
        while i < n && is_env_assignment(&segment[i]) {
            i += 1;
        }
        let Some(w) = segment.get(i).map(String::as_str) else {
            break;
        };
        match w {
            "if" | "then" | "do" | "else" | "elif" | "while" | "until" | "{" | "!" => i += 1,
            "time" | "nohup" | "command" | "exec" => {
                i += 1;
                while i < n && segment[i].starts_with('-') {
                    i += 1;
                }
            }
            "env" => {
                i += 1;
                while i < n {
                    let a = segment[i].as_str();
                    if a == "-u" || a == "--unset" || a == "-C" || a == "--chdir" {
                        i += 2;
                    } else if a.starts_with('-') || is_env_assignment(a) {
                        i += 1;
                    } else {
                        break;
                    }
                }
            }
            "timeout" => {
                i += 1;
                while i < n && segment[i].starts_with('-') {
                    let a = segment[i].as_str();
                    // `-s SIG` / `-k DUR` take a value; `--x=y` and bare
                    // switches do not.
                    i += if a == "-s" || a == "-k" || a == "--signal" || a == "--kill-after" {
                        2
                    } else {
                        1
                    };
                }
                i += 1; // the duration
            }
            _ => break,
        }
    }
    &segment[i.min(n)..]
}

/// Iterate a command's arguments with redirections removed.
fn args_without_redirections(args: &[String]) -> Vec<&str> {
    let mut out = Vec::new();
    let mut j = 0;
    while j < args.len() {
        let w = args[j].as_str();
        if is_redirection(w) {
            j += if redirection_takes_next(w) { 2 } else { 1 };
            continue;
        }
        out.push(w);
        j += 1;
    }
    out
}

// ── Classification ───────────────────────────────────────────────────────────

/// Classify every sensitive command in a shell command string.
///
/// Returns one entry per segment that is one of the five shapes: a `git push`
/// that force-updates (`--force`, `-f`, `--force-with-lease[=…]`, `--mirror`,
/// or a `+refspec`) and/or deletes (`--delete`, `-d`, `:ref`); a non-draft
/// `gh release create` or a `gh release delete`; `npm publish`; and
/// `cargo publish`. A `--dry-run` of any of them publishes nothing and is not
/// classified, and neither is a plain push.
pub fn classify_sensitive_command(command: &str) -> Vec<SensitiveCommand> {
    shell_segments(command)
        .iter()
        .filter_map(|seg| classify_segment(command_words(seg)))
        .collect()
}

fn classify_segment(words: &[String]) -> Option<SensitiveCommand> {
    let program = words.first()?;
    let rest = &words[1..];
    if program_is(program, "git") {
        classify_git(rest)
    } else if program_is(program, "gh") {
        classify_gh(rest)
    } else if program_is(program, "npm") {
        classify_npm(rest)
    } else if program_is(program, "cargo") {
        classify_cargo(rest)
    } else {
        None
    }
}

fn classify_git(args: &[String]) -> Option<SensitiveCommand> {
    let args = args_without_redirections(args);
    // Global options before the subcommand, some of which take a value.
    let mut i = 0;
    while i < args.len() {
        let a = args[i];
        if matches!(a, "-C" | "-c" | "--git-dir" | "--work-tree" | "--namespace") {
            i += 2;
            continue;
        }
        if a.starts_with('-') {
            i += 1;
            continue;
        }
        break;
    }
    if args.get(i) != Some(&"push") {
        return None;
    }
    parse_git_push_args(&args[i + 1..]).map(SensitiveCommand::GitPush)
}

/// Parse the arguments after `push`. `None` for a dry-run or a push that
/// neither forces nor deletes.
fn parse_git_push_args(args: &[&str]) -> Option<GitPushIntent> {
    let mut remote: Option<String> = None;
    let mut refspecs: Vec<String> = Vec::new();
    let mut force_flag = false;
    let mut delete_flag = false;
    let mut tags = false;
    let mut only_positional = false;
    let mut j = 0;
    while j < args.len() {
        let a = args[j];
        if !only_positional && a.starts_with('-') && a.len() > 1 {
            match a {
                "--" => only_positional = true,
                "--force" | "--force-with-lease" => force_flag = true,
                "--mirror" => {
                    force_flag = true;
                    tags = true;
                }
                "--tags" => tags = true,
                "--delete" => delete_flag = true,
                "--dry-run" => return None,
                "-o" | "--push-option" | "--repo" | "--receive-pack" | "--exec" => j += 1,
                s if s.starts_with("--force-with-lease=") => force_flag = true,
                s if s.starts_with("--") => {}
                s => {
                    // A short-flag cluster: `-f`, `-uf`, `-d`, `-n`.
                    let flags = &s[1..];
                    if flags.contains('n') {
                        return None; // -n is --dry-run
                    }
                    if flags.contains('f') {
                        force_flag = true;
                    }
                    if flags.contains('d') {
                        delete_flag = true;
                    }
                    if flags.ends_with('o') {
                        j += 1; // `-o <option>`
                    }
                }
            }
            j += 1;
            continue;
        }
        if remote.is_none() {
            // A URL remote can carry a credential in its userinfo
            // (`https://x-access-token:<tok>@github.com/…`). Strip it here, so
            // nothing downstream — artifact, repo parse, log — ever sees it.
            remote = Some(strip_userinfo(a));
        } else {
            refspecs.push(a.to_string());
        }
        j += 1;
    }

    let mut forced: Vec<String> = Vec::new();
    let mut deleted: Vec<String> = Vec::new();
    let mut unforced_named = 0usize;
    for spec in &refspecs {
        let (plus, body) = match spec.strip_prefix('+') {
            Some(b) => (true, b),
            None => (false, spec.as_str()),
        };
        if delete_flag {
            let r = body.trim_start_matches(':');
            if !r.is_empty() {
                deleted.push(r.to_string());
            }
            continue;
        }
        // `:ref` deletes `ref`; a bare `:` is the MATCHING refspec (push every
        // branch that exists on both sides), not a deletion of "".
        if let Some(dst) = body.strip_prefix(':') {
            if dst.is_empty() {
                if plus || force_flag {
                    forced.push(String::new());
                } else {
                    unforced_named += 1;
                }
            } else {
                deleted.push(dst.to_string());
            }
            continue;
        }
        let dst = body.rsplit_once(':').map(|(_, d)| d).unwrap_or(body);
        if plus || force_flag {
            forced.push(dst.to_string());
        } else {
            unforced_named += 1;
        }
    }

    // A force flag with no non-delete refspec forces the unnamed default
    // (the current branch). A force flag beside ONLY `:ref` deletions forces
    // nothing — there is no ref for it to apply to.
    // `+:` forces every matching branch: unnamed, like a bare `--force`.
    let forced_all = forced.iter().any(String::is_empty);
    forced.retain(|r| !r.is_empty());
    let force = if forced_all {
        Some(Vec::new())
    } else if !forced.is_empty() {
        Some(forced)
    } else if force_flag && !delete_flag && deleted.is_empty() && unforced_named == 0 {
        Some(Vec::new())
    } else {
        None
    };
    let delete = if !deleted.is_empty() || delete_flag {
        Some(deleted)
    } else {
        None
    };
    if force.is_none() && delete.is_none() {
        return None;
    }
    Some(GitPushIntent {
        remote,
        force,
        delete,
        tags,
    })
}

fn classify_gh(args: &[String]) -> Option<SensitiveCommand> {
    let args = args_without_redirections(args);
    if args.first() != Some(&"release") {
        return None;
    }
    let delete = match args.get(1) {
        Some(&"create") => false,
        Some(&"delete") => true,
        _ => return None,
    };
    let mut tag: Option<String> = None;
    let mut repo: Option<String> = None;
    let mut cleanup_tag = false;
    let mut j = 2;
    while j < args.len() {
        let a = args[j];
        if a.starts_with('-') && a.len() > 1 {
            match a {
                "-R" | "--repo" => {
                    repo = args.get(j + 1).map(|s| s.to_string());
                    j += 2;
                    continue;
                }
                s if s.starts_with("--repo=") => repo = Some(s["--repo=".len()..].to_string()),
                // A draft release is not published — nobody outside the repo
                // can see it until it is edited to non-draft.
                "-d" | "--draft" if !delete => return None,
                s if s.starts_with("--draft=") && s != "--draft=false" && !delete => return None,
                "--cleanup-tag" => cleanup_tag = true,
                // Value-taking flags of `gh release create`.
                "-t"
                | "--title"
                | "-n"
                | "--notes"
                | "-F"
                | "--notes-file"
                | "--target"
                | "--discussion-category"
                | "--notes-start-tag"
                | "--notes-from-tag" => {
                    j += 2;
                    continue;
                }
                _ => {}
            }
            j += 1;
            continue;
        }
        if tag.is_none() {
            tag = Some(a.to_string());
        }
        j += 1;
    }
    Some(SensitiveCommand::GhRelease {
        delete,
        tag,
        repo,
        cleanup_tag,
    })
}

fn classify_npm(args: &[String]) -> Option<SensitiveCommand> {
    let args = args_without_redirections(args);
    // The subcommand is the first non-flag word.
    let sub = args.iter().position(|a| !a.starts_with('-'))?;
    if args[sub] != "publish" {
        return None;
    }
    let mut spec: Option<String> = None;
    let mut j = sub + 1;
    while j < args.len() {
        let a = args[j];
        if a.starts_with('-') && a.len() > 1 {
            match a {
                "--dry-run" => return None,
                s if s.starts_with("--dry-run=") && s != "--dry-run=false" => return None,
                "--tag" | "--access" | "--otp" | "--registry" | "-w" | "--workspace" => {
                    j += 2;
                    continue;
                }
                _ => {}
            }
            j += 1;
            continue;
        }
        if spec.is_none() {
            spec = Some(a.to_string());
        }
        j += 1;
    }
    Some(SensitiveCommand::NpmPublish { spec })
}

fn classify_cargo(args: &[String]) -> Option<SensitiveCommand> {
    let args = args_without_redirections(args);
    // `cargo +nightly publish` — skip a toolchain selector and global flags.
    let sub = args
        .iter()
        .position(|a| !a.starts_with('-') && !a.starts_with('+'))?;
    if args[sub] != "publish" {
        return None;
    }
    let mut package: Option<String> = None;
    let mut j = sub + 1;
    while j < args.len() {
        let a = args[j];
        match a {
            "--dry-run" | "-n" => return None,
            "-p" | "--package" => {
                package = args.get(j + 1).map(|s| s.to_string());
                j += 2;
                continue;
            }
            s if s.starts_with("--package=") => package = Some(s["--package=".len()..].to_string()),
            "--registry" | "--index" | "--token" | "--manifest-path" | "--target"
            | "--target-dir" | "-j" | "--jobs" | "-F" | "--features" | "--config" | "-Z" => {
                j += 2;
                continue;
            }
            _ => {}
        }
        j += 1;
    }
    Some(SensitiveCommand::CargoPublish { package })
}

// ── Result interpretation ────────────────────────────────────────────────────

/// Strip a URL's userinfo: `scheme://user:pass@host/…` → `scheme://host/…`.
///
/// A push remote given as a URL routinely carries a token
/// (`https://x-access-token:<tok>@github.com/o/r.git`), and so does the
/// `To <url>` line git echoes back. Everything that reaches an artifact, a
/// repo parse or a log line passes through here first. The scp form
/// (`git@github.com:o/r`) has a user but no secret and is left alone.
pub fn strip_userinfo(url: &str) -> String {
    if let Some(pos) = url.find("://") {
        let (scheme, rest) = url.split_at(pos + 3);
        let authority_end = rest.find('/').unwrap_or(rest.len());
        if let Some(at) = rest[..authority_end].rfind('@') {
            return format!("{scheme}{}", &rest[at + 1..]);
        }
    }
    url.to_string()
}

/// Whether a remote URL names a HOSTED repository — `scheme://host/…` (not
/// `file://`) or scp-style `[user@]host:path` — rather than a local path
/// (`/srv/r.git`, `../r`, `C:/r`). A push to a local path is not an action
/// anyone else can see, so it is not notified.
pub fn is_hosted_remote(url: &str) -> bool {
    let u = url.trim();
    if u.is_empty() {
        return false;
    }
    if let Some((scheme, _)) = u.split_once("://") {
        return !scheme.eq_ignore_ascii_case("file");
    }
    match u.split_once(':') {
        Some((host, _)) => {
            let host = host.rsplit('@').next().unwrap_or(host);
            // A one-letter "host" is a Windows drive (`C:/repo`).
            host.len() > 1 && !host.contains('/') && !host.contains('\\') && !host.starts_with('.')
        }
        None => false,
    }
}

/// The destination of a force-push or ref deletion. Carried to the
/// dispatcher worker, where the D8 noise rule ([`ref_action_is_notable`]) is
/// evaluated against git.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefTarget {
    /// The destination ref as the output (or, when the output was silent,
    /// the command) named it. `None` means "the current branch" — a force
    /// with no refspec under `push.default`.
    pub dest: Option<String>,
    /// The remote NAME, or a userinfo-stripped URL when the command gave one.
    pub remote: String,
    /// The userinfo-stripped URL the output's `To <url>` line named, when it
    /// named one.
    pub remote_url: Option<String>,
    /// `--tags` / `--mirror`.
    pub tags_or_mirror: bool,
}

/// A sensitive action that HAPPENED. Everything needed for the
/// `POST /coord/agent-notifications` body, plus what the dispatcher worker
/// still has to settle against git ([`finalize_detected_action`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectedAction {
    pub action: SensitiveAction,
    /// What was acted on: a ref, a release tag, a package version. Never a
    /// remote URL.
    pub artifact: String,
    /// One of [`reversibility`]'s wire strings.
    pub reversible: &'static str,
    /// The concrete revert handle — the prior sha of a force-updated ref, when
    /// the push output names it.
    pub undo: Option<String>,
    /// `owner/repo`, when the command or its output named it.
    pub repo: Option<String>,
    /// When the action IS repo-scoped and its remote URL is still unknown:
    /// the git remote (name) whose URL names the repo. `None` for a registry
    /// publish, or when the URL is already known.
    pub repo_from_remote: Option<String>,
    /// Where to run git for [`Self::repo_from_remote`] and the noise rule.
    pub working_dir: String,
    /// Set for a git force-push / ref deletion — the D8 noise rule applies.
    pub ref_target: Option<RefTarget>,
}

fn cap_field(s: &str) -> String {
    if s.chars().count() <= MAX_NOTIFICATION_FIELD_CHARS {
        return s.to_string();
    }
    let mut out: String = s.chars().take(MAX_NOTIFICATION_FIELD_CHARS - 1).collect();
    out.push('…');
    out
}

impl DetectedAction {
    /// The `POST /coord/agent-notifications` body (coord's
    /// `AgentNotificationBody`). Optional fields are OMITTED rather than sent
    /// as `null`: coord's body is `deny_unknown_fields`, and `undo` in
    /// particular is unknown to a coord that predates the plan's Phase 4.
    pub fn body(&self) -> serde_json::Value {
        let mut body = serde_json::json!({
            "action": self.action.as_str(),
            "artifact": cap_field(&self.artifact),
            "reversible": self.reversible,
        });
        if let Some(repo) = &self.repo {
            body["repo"] = serde_json::Value::String(repo.clone());
        }
        if let Some(undo) = &self.undo {
            body["undo"] = serde_json::Value::String(cap_field(undo));
        }
        body
    }
}

/// One `To <url>` block of `git push` output: the ref table git prints for
/// ONE push.
#[derive(Debug, Default, PartialEq, Eq)]
struct PushBlock {
    /// The userinfo-stripped URL from the `To` line.
    url: String,
    /// `(destination ref, prior sha)` per ` + old...new src -> dst (forced update)`.
    forced: Vec<(String, String)>,
    /// Ref per ` - [deleted]  ref`.
    deleted: Vec<String>,
}

/// What a tool call's output says its `git push`(es) did.
#[derive(Debug, Default, PartialEq, Eq)]
struct PushOutput {
    blocks: Vec<PushBlock>,
    /// `Everything up-to-date`, a fast-forward or `[new …]` line, or a
    /// rejection: the output SPOKE about refs without reporting a forced or
    /// deleted one.
    spoke: bool,
    /// `fatal:` / `error: failed to push`.
    failed: bool,
}

/// A line of git's push ref table (`<flag> <summary> <from> -> <to>`).
fn is_ref_table_line(t: &str) -> bool {
    t.starts_with("+ ")
        || t.starts_with("- [deleted]")
        || t.starts_with("* [new")
        || t.starts_with("! [")
        || t.starts_with("= [up to date]")
        || t.contains(" -> ")
}

/// Parse push output, counting ref-table lines ONLY inside a `To <url>`
/// block. `git fetch` / `git pull` in the same Bash call print the SAME
/// table shape under a `From <url>` header — `+ a...b main -> origin/main
/// (forced update)` — which is a fetch observing someone else's force-push,
/// not this agent's action. A block closes on `From `, on any non-table
/// line, or on the next `To `.
fn parse_push_output(output: &str) -> PushOutput {
    let mut out = PushOutput::default();
    let mut open: Option<PushBlock> = None;
    for raw in output.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(url) = line.strip_prefix("To ") {
            out.blocks.extend(open.take());
            open = Some(PushBlock {
                url: strip_userinfo(url.trim()),
                ..PushBlock::default()
            });
            continue;
        }
        if line.starts_with("fatal:") || line.starts_with("error: failed to push") {
            out.failed = true;
            out.blocks.extend(open.take());
            continue;
        }
        if line == "Everything up-to-date" {
            out.spoke = true;
            out.blocks.extend(open.take());
            continue;
        }
        let Some(block) = open.as_mut() else {
            continue; // outside a To-block: fetch output, remote: lines, hints
        };
        if !is_ref_table_line(line) {
            out.blocks.extend(open.take());
            continue;
        }
        if line.contains("(forced update)") {
            let toks: Vec<&str> = line.split_whitespace().collect();
            // ["+", "old...new", "src", "->", "dst", "(forced", "update)"]
            let prior = toks
                .iter()
                .find(|t| t.contains("..."))
                .and_then(|t| t.split("...").next())
                .unwrap_or("")
                .to_string();
            let dst = toks
                .iter()
                .position(|t| *t == "->")
                .and_then(|p| toks.get(p + 1))
                .map(|s| s.to_string())
                .unwrap_or_default();
            if !dst.is_empty() {
                block.forced.push((dst, prior));
            }
        } else if line.starts_with("- [deleted]") {
            if let Some(r) = line.split_whitespace().last() {
                block.deleted.push(r.to_string());
            }
        } else {
            out.spoke = true;
        }
    }
    out.blocks.extend(open.take());
    out
}

/// Case-insensitive "any line starts with one of `prefixes`".
fn any_line_starts_with(output: &str, prefixes: &[&str]) -> bool {
    output.lines().any(|l| {
        let l = l.trim_start().to_ascii_lowercase();
        prefixes.iter().any(|p| l.starts_with(p))
    })
}

/// A ref name as compared across command and output: `refs/heads/` and
/// `refs/tags/` stripped (git's table prints short names).
fn short_ref(r: &str) -> &str {
    r.strip_prefix("refs/heads/")
        .or_else(|| r.strip_prefix("refs/tags/"))
        .unwrap_or(r)
}

/// The destinations a set of intents declared for one arm (force or delete).
/// `None` = unconstrained: some intent left the ref unnamed (or named `HEAD`,
/// whose destination only the output knows), so every reported ref counts.
fn declared_dests<'a>(
    intents: &[&'a GitPushIntent],
    arm: impl Fn(&'a GitPushIntent) -> &'a Option<Vec<String>>,
) -> Option<Vec<String>> {
    let mut names = Vec::new();
    for i in intents {
        if let Some(refs) = arm(i) {
            if refs.is_empty() || refs.iter().any(|r| r == "HEAD" || r == "@") {
                return None;
            }
            names.extend(refs.iter().map(|r| short_ref(r).to_string()));
        }
    }
    Some(names)
}

/// Turn ONE tool call — every sensitive command it classified, and its one
/// output — into the actions that happened, de-duplicated by
/// `(action, artifact)`.
///
/// Interpreting the output once per tool call (not once per classified
/// segment) is what keeps `git push -f a && git push -f b` from reporting
/// every forced line twice.
///
/// `failed` is `is_error` (or an interrupted command). It does not veto
/// POSITIVE evidence — a `(forced update)` / `[deleted]` line inside a
/// `To` block, npm's `+ name@ver`, cargo's `Published` — because a push that
/// updated one ref and was rejected on another exits non-zero after the
/// first ref already moved. It does veto everything inferred from the
/// command alone.
pub fn interpret_tool_use(
    commands: &[SensitiveCommand],
    output: &str,
    working_dir: &str,
    failed: bool,
) -> Vec<DetectedAction> {
    let mut out = Vec::new();
    let pushes: Vec<&GitPushIntent> = commands
        .iter()
        .filter_map(|c| match c {
            SensitiveCommand::GitPush(i) => Some(i),
            _ => None,
        })
        .collect();
    if !pushes.is_empty() {
        out.extend(interpret_git_pushes(&pushes, output, working_dir, failed));
    }
    for cmd in commands {
        match cmd {
            SensitiveCommand::GitPush(_) => {}
            other => out.extend(interpret_other(other, output, working_dir, failed)),
        }
    }
    let mut seen = std::collections::HashSet::new();
    out.retain(|a| seen.insert((a.action, a.artifact.clone())));
    out
}

fn interpret_git_pushes(
    intents: &[&GitPushIntent],
    output: &str,
    working_dir: &str,
    failed: bool,
) -> Vec<DetectedAction> {
    let parsed = parse_push_output(output);
    let remote = intents
        .iter()
        .find_map(|i| i.remote.clone())
        .unwrap_or_else(|| "origin".to_string());
    let tags_or_mirror = intents.iter().any(|i| i.tags);
    let any_force = intents.iter().any(|i| i.force.is_some());
    let any_delete = intents.iter().any(|i| i.delete.is_some());
    let force_dests = declared_dests(intents, |i| &i.force);
    let delete_dests = declared_dests(intents, |i| &i.delete);
    let matches = |declared: &Option<Vec<String>>, r: &str| match declared {
        None => true,
        Some(names) => names.iter().any(|n| n == short_ref(r)),
    };

    let mut out = Vec::new();
    for block in &parsed.blocks {
        // A push to a local path is not an outward action.
        if !is_hosted_remote(&block.url) {
            continue;
        }
        let repo = parse_repo_full_name(&block.url);
        let make = |action, artifact: &str, reversible, undo| DetectedAction {
            action,
            artifact: artifact.to_string(),
            reversible,
            undo,
            repo: repo.clone(),
            repo_from_remote: None,
            working_dir: working_dir.to_string(),
            ref_target: Some(RefTarget {
                dest: Some(artifact.to_string()),
                remote: remote.clone(),
                remote_url: Some(block.url.clone()),
                tags_or_mirror,
            }),
        };
        if any_force {
            for (dst, prior) in &block.forced {
                if !matches(&force_dests, dst) {
                    continue;
                }
                let (reversible, undo) = if prior.is_empty() {
                    (reversibility::NO, None)
                } else {
                    (reversibility::RESTORE, Some(prior.clone()))
                };
                out.push(make(SensitiveAction::ForcePush, dst, reversible, undo));
            }
        }
        if any_delete {
            for r in &block.deleted {
                if matches(&delete_dests, r) {
                    out.push(make(
                        SensitiveAction::Delete,
                        r,
                        reversibility::RESTORE,
                        None,
                    ));
                }
            }
        }
    }

    // The output said nothing about refs at all (`-q`, redirected to a file):
    // report what the command DECLARED — but only for a call that did not
    // fail, since there is no positive evidence either way.
    let silent = parsed.blocks.is_empty() && !parsed.spoke && !parsed.failed;
    if silent && !failed {
        let url = url_remote(&remote);
        if url.as_deref().is_some_and(|u| !is_hosted_remote(u)) {
            return out;
        }
        let repo = url.as_deref().and_then(parse_repo_full_name);
        let fallback = |action, dest: Option<&str>, reversible| DetectedAction {
            action,
            artifact: dest.unwrap_or(CURRENT_BRANCH_ARTIFACT).to_string(),
            reversible,
            undo: None,
            repo: repo.clone(),
            repo_from_remote: if url.is_none() {
                Some(remote.clone())
            } else {
                None
            },
            working_dir: working_dir.to_string(),
            ref_target: Some(RefTarget {
                dest: dest.map(str::to_string),
                remote: remote.clone(),
                remote_url: url.clone(),
                tags_or_mirror,
            }),
        };
        for intent in intents {
            if let Some(declared) = &intent.force {
                // No prior sha is known, so the notification cannot name
                // anything to restore FROM.
                if declared.is_empty() {
                    out.push(fallback(
                        SensitiveAction::ForcePush,
                        None,
                        reversibility::NO,
                    ));
                }
                for r in declared {
                    out.push(fallback(
                        SensitiveAction::ForcePush,
                        Some(r.as_str()),
                        reversibility::NO,
                    ));
                }
            }
            if let Some(declared) = &intent.delete {
                for r in declared {
                    out.push(fallback(
                        SensitiveAction::Delete,
                        Some(r.as_str()),
                        reversibility::RESTORE,
                    ));
                }
            }
        }
    }
    out
}

/// The artifact of a force whose destination only git knows. The dispatcher
/// worker replaces it with the working dir's current branch name.
const CURRENT_BRANCH_ARTIFACT: &str = "the current branch";

/// `Some(url)` when a remote was given as a URL (userinfo already stripped at
/// parse), `None` for a remote NAME.
fn url_remote(remote: &str) -> Option<String> {
    if remote.contains("://") || remote.contains('@') || remote.ends_with(".git") {
        Some(strip_userinfo(remote))
    } else {
        None
    }
}

/// The last path component — a directory NAME, never the full path (which
/// on Windows carries the user name).
fn dir_basename(path: &str) -> &str {
    path.trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("")
}

fn interpret_other(
    cmd: &SensitiveCommand,
    output: &str,
    working_dir: &str,
    failed: bool,
) -> Vec<DetectedAction> {
    match cmd {
        SensitiveCommand::GitPush(_) => Vec::new(),
        SensitiveCommand::GhRelease {
            delete,
            tag,
            repo,
            cleanup_tag,
        } => {
            // gh prints no positive evidence a failure could contradict, so a
            // failed call is simply not reported.
            if failed
                || output.contains("HTTP 4")
                || output.contains("HTTP 5")
                || output.to_ascii_lowercase().contains("release not found")
            {
                return Vec::new();
            }
            let tag_txt = tag.clone().unwrap_or_else(|| "(unnamed tag)".to_string());
            let repo = repo.clone().or_else(|| {
                // `gh release create` prints the release URL:
                // https://github.com/<owner>/<repo>/releases/tag/<tag>
                output
                    .split_whitespace()
                    .find_map(|w| w.split_once("/releases/").map(|(base, _)| base))
                    .and_then(parse_repo_full_name)
            });
            let repo_from_remote = if repo.is_none() {
                Some("origin".to_string())
            } else {
                None
            };
            let (action, artifact, reversible) = if *delete {
                let artifact = if *cleanup_tag {
                    format!("release {tag_txt} and its tag")
                } else {
                    format!("release {tag_txt}")
                };
                // A deleted GitHub release keeps nothing to restore it FROM —
                // its notes and uploaded assets are gone. It is recreated,
                // which is coord's `roll-forward`, not `restore`.
                (
                    SensitiveAction::Delete,
                    artifact,
                    reversibility::ROLL_FORWARD,
                )
            } else {
                (
                    SensitiveAction::Publish,
                    format!("release {tag_txt}"),
                    reversibility::NO,
                )
            };
            vec![DetectedAction {
                action,
                artifact,
                reversible,
                undo: None,
                repo,
                repo_from_remote,
                working_dir: working_dir.to_string(),
                ref_target: None,
            }]
        }
        SensitiveCommand::NpmPublish { spec } => {
            // npm prints `+ <name>@<version>` on success — positive evidence.
            let published = output.lines().find_map(|l| {
                l.trim()
                    .strip_prefix("+ ")
                    .filter(|rest| rest.contains('@'))
                    .map(|rest| rest.trim().to_string())
            });
            if let Some(p) = published {
                return vec![registry_publish(p, working_dir)];
            }
            if failed || any_line_starts_with(output, &["npm err!", "npm error"]) {
                return Vec::new();
            }
            let artifact = match spec {
                Some(s) => format!("npm package {}", dir_basename(s)),
                None => format!("npm package in ./{}", dir_basename(working_dir)),
            };
            vec![registry_publish(artifact, working_dir)]
        }
        SensitiveCommand::CargoPublish { package } => {
            // cargo prints `Published <name> v<version> at registry …` once
            // the upload is accepted — positive evidence.
            let published = output.lines().find_map(|l| {
                let rest = l.trim().strip_prefix("Published ")?;
                let mut it = rest.split_whitespace();
                let name = it.next()?;
                let version = it.next()?;
                Some(format!("crate {name} {version}"))
            });
            if let Some(p) = published {
                return vec![registry_publish(p, working_dir)];
            }
            if failed || any_line_starts_with(output, &["error:", "error["]) {
                return Vec::new();
            }
            // Older cargo prints only `Uploading <name> v<version>`.
            let uploading = output.lines().find_map(|l| {
                let rest = l.trim().strip_prefix("Uploading ")?;
                let mut it = rest.split_whitespace();
                Some(format!("crate {} {}", it.next()?, it.next()?))
            });
            let artifact = uploading.unwrap_or_else(|| match package {
                Some(p) => format!("crate {p}"),
                None => format!("crate in ./{}", dir_basename(working_dir)),
            });
            vec![registry_publish(artifact, working_dir)]
        }
    }
}

fn registry_publish(artifact: String, working_dir: &str) -> DetectedAction {
    DetectedAction {
        action: SensitiveAction::Publish,
        artifact,
        // A registry version cannot be un-published into reuse: npm forbids
        // re-publishing a version number, crates.io only yanks.
        reversible: reversibility::NO,
        undo: None,
        repo: None,
        repo_from_remote: None,
        working_dir: working_dir.to_string(),
        ref_target: None,
    }
}

// ── The D8 noise rule ────────────────────────────────────────────────────────

/// What git says about a ref action's surroundings. Every field is `None`
/// when git could not answer, and an unanswered question never suppresses a
/// notification.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RefFacts {
    /// The working dir's checked-out branch (`git rev-parse --abbrev-ref
    /// HEAD`); `None` on failure or a detached HEAD.
    pub current_branch: Option<String>,
    /// The remote's default branch (`git symbolic-ref --short
    /// refs/remotes/<remote>/HEAD`, remote prefix stripped).
    pub default_branch: Option<String>,
    /// Whether the destination names a local tag (`git tag --list <dest>`).
    pub dest_is_tag: Option<bool>,
}

/// Plan design decision D8 — which force-pushes and ref deletions are worth
/// the operator's attention. Notify ONLY when the destination (with
/// `refs/heads/` stripped) is:
///
/// 1. the remote's default branch (`main`/`master` when git cannot say);
/// 2. a tag (`refs/tags/*`, a local tag of that name, or a `--tags` /
///    `--mirror` push);
/// 3. `release/*` or `hotfix/*`;
/// 4. a branch that is NOT the working dir's current branch — someone
///    else's branch, or at least not the one this agent is working on.
///
/// A force-push or delete of the agent's OWN non-default working branch is
/// routine (rebase, amend, cleanup) and is suppressed. Anything the rule
/// cannot evaluate — no current branch, no destination — notifies: unknown
/// must never suppress. `publish` never reaches this function; it always
/// notifies.
pub fn ref_action_is_notable(dest: Option<&str>, tags_or_mirror: bool, facts: &RefFacts) -> bool {
    if tags_or_mirror {
        return true;
    }
    let dest = match dest {
        Some(d) => d,
        None => match facts.current_branch.as_deref() {
            Some(cb) => cb,
            None => return true,
        },
    };
    if dest.starts_with("refs/tags/") || facts.dest_is_tag == Some(true) {
        return true;
    }
    let d = dest.strip_prefix("refs/heads/").unwrap_or(dest);
    if d.starts_with("release/") || d.starts_with("hotfix/") {
        return true;
    }
    let is_default = match facts.default_branch.as_deref() {
        Some(db) => d == db,
        None => d == "main" || d == "master",
    };
    if is_default {
        return true;
    }
    match facts.current_branch.as_deref() {
        Some(cb) => d != cb,
        None => true,
    }
}

/// Ask git the questions [`ref_action_is_notable`] needs. Impure — run on
/// the dispatcher worker, never the tail loop. Each failure leaves its field
/// `None`.
fn gather_ref_facts(working_dir: &str, target: &RefTarget) -> RefFacts {
    if working_dir.trim().is_empty() {
        return RefFacts::default();
    }
    let dir = PathBuf::from(working_dir);
    let current_branch =
        git(&dir, &["rev-parse", "--abbrev-ref", "HEAD"]).filter(|b| !b.is_empty() && b != "HEAD");
    // Only a remote NAME has a refs/remotes/<name>/HEAD.
    let default_branch = if url_remote(&target.remote).is_none() {
        let head_ref = format!("refs/remotes/{}/HEAD", target.remote);
        git(&dir, &["symbolic-ref", "--short", &head_ref])
            .map(|s| {
                s.strip_prefix(&format!("{}/", target.remote))
                    .unwrap_or(&s)
                    .to_string()
            })
            .filter(|s| !s.is_empty())
    } else {
        None
    };
    let dest_is_tag = target.dest.as_deref().and_then(|d| {
        let name = d.strip_prefix("refs/tags/").unwrap_or(d);
        git(&dir, &["tag", "--list", name]).map(|out| out.lines().any(|l| l.trim() == name))
    });
    RefFacts {
        current_branch,
        default_branch,
        dest_is_tag,
    }
}

/// Settle what a detected action still needs from git, on the dispatcher
/// worker: the remote URL (dropping a push to a local path), the repo, and —
/// for a force-push or delete — the D8 noise rule. `None` means "do not
/// notify".
pub fn finalize_detected_action(mut action: DetectedAction) -> Option<DetectedAction> {
    let url = action
        .ref_target
        .as_ref()
        .and_then(|t| t.remote_url.clone())
        .or_else(|| {
            let remote = action.repo_from_remote.as_deref()?;
            if action.working_dir.trim().is_empty() {
                return None;
            }
            git(
                &PathBuf::from(&action.working_dir),
                &["remote", "get-url", remote],
            )
            .map(|u| strip_userinfo(&u))
        });
    if let Some(u) = url.as_deref() {
        if !is_hosted_remote(u) {
            debug!("commit_report: sensitive action targets a local-path remote — not notified");
            return None;
        }
        if action.repo.is_none() {
            action.repo = parse_repo_full_name(u);
        }
    }
    if let Some(target) = action.ref_target.clone() {
        let facts = gather_ref_facts(&action.working_dir, &target);
        if target.dest.is_none() {
            if let Some(cb) = &facts.current_branch {
                action.artifact = cb.clone();
            }
        }
        if !ref_action_is_notable(target.dest.as_deref(), target.tags_or_mirror, &facts) {
            debug!(
                action = action.action.as_str(),
                artifact = %action.artifact,
                "commit_report: force-push/delete of the working branch — suppressed by D8"
            );
            return None;
        }
    }
    Some(action)
}

// ── The pending map ──────────────────────────────────────────────────────────

struct PendingAction {
    commands: Vec<SensitiveCommand>,
    working_dir: String,
    observed_at: Instant,
}

/// PROCESS-GLOBAL memory of emitted `tool_use_id`s, bounded to
/// [`EMITTED_MEMORY`]. Claude `toolu_…` ids are globally unique, and the same
/// `tool_use` can appear in two transcript files (a subagent's sidechain and
/// its parent, a resumed session) tailed by two trackers — one notification
/// per id, not one per file. It also covers a truncated transcript re-read
/// from offset 0.
static EMITTED_TOOL_USES: Lazy<
    Mutex<(
        std::collections::VecDeque<String>,
        std::collections::HashSet<String>,
    )>,
> = Lazy::new(|| {
    Mutex::new((
        std::collections::VecDeque::new(),
        std::collections::HashSet::new(),
    ))
});

/// Record `id` as emitted. `false` when it already was.
fn claim_emission(id: &str) -> bool {
    let mut g = EMITTED_TOOL_USES.lock().unwrap_or_else(|e| e.into_inner());
    let (order, set) = &mut *g;
    if set.contains(id) {
        return false;
    }
    if order.len() >= EMITTED_MEMORY {
        if let Some(old) = order.pop_front() {
            set.remove(&old);
        }
    }
    order.push_back(id.to_string());
    set.insert(id.to_string());
    true
}

/// Per-transcript memory of sensitive `tool_use`s awaiting their `tool_result`.
///
/// Owned by one transcript tail loop (`transcript_watcher::tail_session`), so
/// it needs no lock and dies with the tail. Bounded two ways: at most
/// [`PENDING_ACTION_CAPACITY`] entries (the oldest is evicted to admit a new
/// one), and entries older than [`PENDING_ACTION_TTL`] are dropped before any
/// lookup — so a result that arrives after the TTL is NOT emitted. Both drops
/// are counted.
pub struct SensitiveActionTracker {
    pending: HashMap<String, PendingAction>,
    expired: u64,
    evicted: u64,
}

impl Default for SensitiveActionTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl SensitiveActionTracker {
    pub fn new() -> Self {
        Self {
            pending: HashMap::new(),
            expired: 0,
            evicted: 0,
        }
    }

    /// Entries awaiting a result.
    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    /// Entries dropped by the TTL.
    pub fn expired(&self) -> u64 {
        self.expired
    }

    /// Entries evicted by the capacity bound.
    pub fn evicted(&self) -> u64 {
        self.evicted
    }

    fn sweep(&mut self, now: Instant) {
        let before = self.pending.len();
        self.pending
            .retain(|_, p| now.saturating_duration_since(p.observed_at) < PENDING_ACTION_TTL);
        let dropped = (before - self.pending.len()) as u64;
        if dropped > 0 {
            self.expired += dropped;
            debug!(
                dropped,
                expired_total = self.expired,
                "commit_report: sensitive tool_use(s) expired with no tool_result — not notified"
            );
        }
    }

    fn insert(&mut self, id: String, entry: PendingAction) {
        if !self.pending.contains_key(&id) && self.pending.len() >= PENDING_ACTION_CAPACITY {
            if let Some(oldest) = self
                .pending
                .iter()
                .min_by_key(|(_, p)| p.observed_at)
                .map(|(k, _)| k.clone())
            {
                self.pending.remove(&oldest);
                self.evicted += 1;
                warn!(
                    capacity = PENDING_ACTION_CAPACITY,
                    evicted_total = self.evicted,
                    "commit_report: sensitive-action pending map full — evicted the oldest entry"
                );
            }
        }
        self.pending.insert(id, entry);
    }

    /// Feed one transcript line. Returns the actions whose result this line
    /// carried. Pure apart from `now` and the process-global emitted-id set —
    /// never runs git or I/O.
    pub fn observe_line(&mut self, line: &str, now: Instant) -> Vec<DetectedAction> {
        // Cheap pre-filters: this runs on every transcript line, and almost no
        // line is a Bash tool_use or a result we are waiting for.
        let maybe_use = line.contains("\"tool_use\"") && line.contains("\"Bash\"");
        let maybe_result = !self.pending.is_empty() && line.contains("\"tool_result\"");
        if !maybe_use && !maybe_result {
            return Vec::new();
        }
        let Ok(record) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
            return Vec::new();
        };
        self.sweep(now);
        match record.get("type").and_then(|t| t.as_str()) {
            Some("assistant") => {
                self.observe_tool_uses(&record, now);
                Vec::new()
            }
            Some("user") => self.observe_tool_results(&record),
            _ => Vec::new(),
        }
    }

    fn observe_tool_uses(&mut self, record: &serde_json::Value, now: Instant) {
        let transcript_cwd = record.get("cwd").and_then(|c| c.as_str()).unwrap_or("");
        let Some(content) = record
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_array())
        else {
            return;
        };
        for block in content {
            if block.get("type").and_then(|t| t.as_str()) != Some("tool_use")
                || block.get("name").and_then(|n| n.as_str()) != Some("Bash")
            {
                continue;
            }
            let (Some(id), Some(command)) = (
                block.get("id").and_then(|i| i.as_str()),
                block
                    .get("input")
                    .and_then(|i| i.get("command"))
                    .and_then(|c| c.as_str()),
            ) else {
                continue;
            };
            let commands = classify_sensitive_command(command);
            if commands.is_empty() {
                continue;
            }
            self.insert(
                id.to_string(),
                PendingAction {
                    commands,
                    working_dir: resolve_working_dir(command, transcript_cwd),
                    observed_at: now,
                },
            );
        }
    }

    fn observe_tool_results(&mut self, record: &serde_json::Value) -> Vec<DetectedAction> {
        let Some(content) = record
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_array())
        else {
            return Vec::new();
        };
        let results: Vec<&serde_json::Value> = content
            .iter()
            .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("tool_result"))
            .collect();
        // The Claude Code record's `toolUseResult` describes its single
        // result; with several results it cannot be attributed.
        let side = if results.len() == 1 {
            record.get("toolUseResult")
        } else {
            None
        };
        let mut out = Vec::new();
        for block in &results {
            let Some(id) = block.get("tool_use_id").and_then(|i| i.as_str()) else {
                continue;
            };
            let Some(pending) = self.pending.remove(id) else {
                continue;
            };
            // A `run_in_background` call returns at once with a task id; its
            // result says the command STARTED, not that it did anything.
            if side
                .and_then(|s| s.get("backgroundTaskId"))
                .is_some_and(|v| !v.is_null())
            {
                debug!(
                    tool_use_id = id,
                    "commit_report: sensitive command ran in the background — outcome unknown, \
                     not notified"
                );
                continue;
            }
            if record_is_stale(record) {
                debug!(
                    tool_use_id = id,
                    "commit_report: tool_result older than the pending TTL — not notified"
                );
                continue;
            }
            let is_error = block.get("is_error").and_then(|e| e.as_bool()) == Some(true);
            let interrupted = side
                .and_then(|s| s.get("interrupted"))
                .and_then(|i| i.as_bool())
                == Some(true);
            let output = result_output(block, side);
            let actions = interpret_tool_use(
                &pending.commands,
                &output,
                &pending.working_dir,
                is_error || interrupted,
            );
            if actions.is_empty() || !claim_emission(id) {
                continue;
            }
            out.extend(actions);
        }
        out
    }
}

/// Whether a record's own `timestamp` is older than [`PENDING_ACTION_TTL`] —
/// a line from a transcript being re-read long after the fact. A missing or
/// unparseable timestamp is NOT stale.
fn record_is_stale(record: &serde_json::Value) -> bool {
    let Some(ts) = record
        .get("timestamp")
        .and_then(|t| t.as_str())
        .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
    else {
        return false;
    };
    let age = chrono::Utc::now().signed_duration_since(ts.with_timezone(&chrono::Utc));
    age.to_std().is_ok_and(|a| a > PENDING_ACTION_TTL)
}

/// The output of one Bash call. Claude Code records it TWICE: as the
/// `tool_result` text the model saw, and as raw `stdout` / `stderr` under
/// `toolUseResult` (git writes its ref table to stderr). Joining the two
/// would parse every ref line twice, so the raw streams win whenever either
/// is non-empty, and the result text is only the fallback.
fn result_output(block: &serde_json::Value, side: Option<&serde_json::Value>) -> String {
    if let Some(side) = side {
        let stdout = side.get("stdout").and_then(|v| v.as_str()).unwrap_or("");
        let stderr = side.get("stderr").and_then(|v| v.as_str()).unwrap_or("");
        if !stdout.trim().is_empty() || !stderr.trim().is_empty() {
            return format!("{stdout}\n{stderr}");
        }
    }
    tool_result_text(block)
}

/// The text of a `tool_result` block: a string, or an array of `{type:"text"}`.
fn tool_result_text(block: &serde_json::Value) -> String {
    match block.get("content") {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Array(parts)) => parts
            .iter()
            .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// Hand one detected action to the bounded git worker, which settles it
/// against git ([`finalize_detected_action`]) and enqueues the notification
/// on the session outbox. Non-blocking, same queue and drop posture as
/// [`dispatch_push_observation`].
pub fn dispatch_detected_action(
    action: DetectedAction,
    lane: uuid::Uuid,
    registrar: Arc<crate::claude_session::coord_register::AiCoordRegistrar>,
) -> bool {
    PUSH_DISPATCHER.try_dispatch(move || {
        if let Some(action) = finalize_detected_action(action) {
            registrar.report_agent_notification(lane, action.body());
        }
    })
}

/// The outbox seq lane for a transcript's notifications: deterministic per
/// transcript session so its notifications drain in order, and distinct from
/// any coord session id (coord never reads it).
pub fn agent_notification_lane(transcript_session_id: &str) -> uuid::Uuid {
    uuid::Uuid::new_v5(
        &uuid::Uuid::NAMESPACE_URL,
        format!("agent-notification:{transcript_session_id}").as_bytes(),
    )
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

/// Phase 9 of plan `2026-09-18-notifications-are-agent-actions-and-alerts-are-agent-work`:
/// the sensitive-action classifier, the result interpreter, and the pending
/// map, driven by fixture transcripts.
#[cfg(test)]
mod sensitive_action_tests {
    use super::*;
    use serde_json::json;

    // ── Classifier: positive shapes ──────────────────────────────────────

    fn git_push(cmd: &str) -> GitPushIntent {
        let got = classify_sensitive_command(cmd);
        assert_eq!(got.len(), 1, "{cmd:?} should classify once, got {got:?}");
        match &got[0] {
            SensitiveCommand::GitPush(i) => i.clone(),
            other => panic!("{cmd:?} classified as {other:?}, not a git push"),
        }
    }

    #[test]
    fn force_push_flag_shapes() {
        for cmd in [
            "git push --force",
            "git push -f",
            "git push --force-with-lease",
            "git push --mirror origin",
            "cd \"C:/repo\" && git push --force 2>&1 | tail -5",
            "git -C /some/dir push -f",
            "FOO=bar git push --force",
            "/usr/bin/git push --force",
            "git.exe push -f",
        ] {
            let i = git_push(cmd);
            assert_eq!(i.force, Some(vec![]), "{cmd:?}: forced, refs unnamed");
            assert_eq!(i.delete, None, "{cmd:?}");
        }
    }

    #[test]
    fn force_push_named_refs() {
        let i = git_push("git push -f origin feat/x");
        assert_eq!(i.remote.as_deref(), Some("origin"));
        assert_eq!(i.force, Some(vec!["feat/x".to_string()]));

        let i = git_push("git push --force-with-lease=main:abc123 origin main");
        assert_eq!(i.force, Some(vec!["main".to_string()]));

        let i = git_push("git push -uf origin feat/y");
        assert_eq!(i.force, Some(vec!["feat/y".to_string()]));

        // A `+refspec` forces only that ref; its destination is reported.
        let i = git_push("git push origin +HEAD:refs/heads/feat/z main");
        assert_eq!(i.force, Some(vec!["refs/heads/feat/z".to_string()]));

        let i = git_push("git push origin +main");
        assert_eq!(i.force, Some(vec!["main".to_string()]));
    }

    #[test]
    fn ref_deletion_shapes() {
        let i = git_push("git push --delete origin old-a old-b");
        assert_eq!(
            i.delete,
            Some(vec!["old-a".to_string(), "old-b".to_string()])
        );
        assert_eq!(i.force, None);

        let i = git_push("git push -d origin old");
        assert_eq!(i.delete, Some(vec!["old".to_string()]));

        let i = git_push("git push origin :old");
        assert_eq!(i.delete, Some(vec!["old".to_string()]));
        assert_eq!(i.force, None);

        // A force flag beside ONLY a deletion forces nothing.
        let i = git_push("git push -f origin :old");
        assert_eq!(i.force, None);
        assert_eq!(i.delete, Some(vec!["old".to_string()]));
    }

    #[test]
    fn gh_release_shapes() {
        let got = classify_sensitive_command(
            "gh release create v1.2.3 --title \"v1.2.3\" --notes \"fixes; see git push --force\"",
        );
        assert_eq!(
            got,
            vec![SensitiveCommand::GhRelease {
                delete: false,
                tag: Some("v1.2.3".to_string()),
                repo: None,
                cleanup_tag: false,
            }],
            "the quoted notes are ONE argument — no phantom git push, no wrong tag"
        );

        let got = classify_sensitive_command("gh release delete v1.0.0 -y --cleanup-tag -R o/r");
        assert_eq!(
            got,
            vec![SensitiveCommand::GhRelease {
                delete: true,
                tag: Some("v1.0.0".to_string()),
                repo: Some("o/r".to_string()),
                cleanup_tag: true,
            }]
        );
    }

    #[test]
    fn registry_publish_shapes() {
        assert_eq!(
            classify_sensitive_command("npm publish"),
            vec![SensitiveCommand::NpmPublish { spec: None }]
        );
        assert_eq!(
            classify_sensitive_command("cd packages/ui && npm publish --access public"),
            vec![SensitiveCommand::NpmPublish { spec: None }]
        );
        assert_eq!(
            classify_sensitive_command("cargo publish"),
            vec![SensitiveCommand::CargoPublish { package: None }]
        );
        assert_eq!(
            classify_sensitive_command("cargo +stable publish -p qontinui-schemas"),
            vec![SensitiveCommand::CargoPublish {
                package: Some("qontinui-schemas".to_string())
            }]
        );
    }

    // ── Classifier: negative shapes ──────────────────────────────────────

    #[test]
    fn plain_and_dry_run_pushes_are_not_sensitive() {
        for cmd in [
            "git push",
            "git push -u origin feat/x",
            "git push origin HEAD",
            "git push --force --dry-run",
            "git push -n -f origin x",
            "git push -fn origin x",
            "git push --force-if-includes origin x",
        ] {
            assert!(
                classify_sensitive_command(cmd).is_empty(),
                "{cmd:?} must not classify"
            );
        }
    }

    /// The same first-word-of-a-segment rule [`command_is_git_push`] applies:
    /// a push MENTIONED as text is not a push. Both detectors agree on every
    /// shape here.
    #[test]
    fn mentioned_commands_are_text_not_actions() {
        for cmd in [
            "echo \"git push --force-with-lease\"",
            "echo git push --force",
            "printf '%s' 'npm publish'",
            "git commit -m \"revert the git push --force\"",
            "# git push --force\ngit status",
        ] {
            assert!(
                classify_sensitive_command(cmd).is_empty(),
                "{cmd:?} must not classify"
            );
        }
        // The git-push detector agrees on the unquoted echo.
        assert!(!command_is_git_push("echo git push --force"));
        // And both see the real push behind `&&`.
        assert!(command_is_git_push("cd x && git push -f"));
        assert_eq!(classify_sensitive_command("cd x && git push -f").len(), 1);
    }

    #[test]
    fn heredoc_bodies_are_not_commands() {
        let cmd =
            "git commit -F - <<'EOF'\nfix: undo it\ngit push --force\nnpm publish\nEOF\ngit log -1";
        assert!(classify_sensitive_command(cmd).is_empty());
        // A real command AFTER the heredoc still classifies.
        let cmd = "cat <<EOF > notes.txt\ngit push --force\nEOF\ngit push -f origin x";
        assert_eq!(classify_sensitive_command(cmd).len(), 1);
    }

    #[test]
    fn non_publish_subcommands_are_not_sensitive() {
        for cmd in [
            "npm run publish",
            "npm publish --dry-run",
            "cargo publish --dry-run",
            "cargo publish -n",
            "cargo build --release",
            "gh release create v1 --draft",
            "gh release view v1",
            "gh pr create --title x",
            "git log --grep=force",
        ] {
            assert!(
                classify_sensitive_command(cmd).is_empty(),
                "{cmd:?} must not classify"
            );
        }
    }

    #[test]
    fn shell_segments_keeps_redirections_attached() {
        assert_eq!(
            shell_segments("git push -f 2>&1 | tail -3"),
            vec![vec!["git", "push", "-f", "2>&1"], vec!["tail", "-3"],]
                .into_iter()
                .map(|v| v.into_iter().map(String::from).collect::<Vec<_>>())
                .collect::<Vec<_>>()
        );
    }

    /// `<<<` is a here-STRING, not a heredoc: the word after it must not be
    /// taken as a heredoc delimiter that swallows the following lines.
    #[test]
    fn here_strings_are_not_heredocs() {
        let cmd = "cat <<< EOF\ngit push -f origin other-branch";
        assert_eq!(classify_sensitive_command(cmd).len(), 1);
    }

    #[test]
    fn keywords_and_wrappers_do_not_hide_the_program() {
        for cmd in [
            "if git push -f origin x; then echo ok; fi",
            "for r in a; do git push -f origin x; done",
            "timeout 60 git push -f origin x",
            "timeout -s KILL 60 git push -f origin x",
            "env GIT_TRACE=1 git push -f origin x",
            "env -i HOME=/h git push -f origin x",
            "nohup git push -f origin x",
            "command git push -f origin x",
            "exec git push -f origin x",
            "time git push -f origin x",
            "! git push -f origin x",
            "{ git push -f origin x; }",
        ] {
            assert_eq!(
                classify_sensitive_command(cmd).len(),
                1,
                "{cmd:?} should classify as one push"
            );
        }
    }

    #[test]
    fn the_matching_refspec_is_not_a_deletion() {
        assert!(classify_sensitive_command("git push origin :").is_empty());
        // Forced matching push: every matching branch, unnamed.
        assert_eq!(git_push("git push origin +:").force, Some(vec![]));
        assert_eq!(git_push("git push -f origin :").force, Some(vec![]));
        assert_eq!(git_push("git push -f origin :").delete, None);
    }

    #[test]
    fn tags_and_mirror_pushes_are_marked() {
        assert!(git_push("git push --force --tags origin").tags);
        assert!(git_push("git push --mirror origin").tags);
        assert!(!git_push("git push -f origin x").tags);
    }

    // ── URL hygiene ──────────────────────────────────────────────────────

    #[test]
    fn userinfo_is_stripped_before_anything_sees_the_remote() {
        let tok = "https://x-access-token:ghs_SECRET123@github.com/o/r.git";
        assert_eq!(strip_userinfo(tok), "https://github.com/o/r.git");
        assert_eq!(
            strip_userinfo("git@github.com:o/r.git"),
            "git@github.com:o/r.git",
            "scp form carries no secret"
        );

        let intent = git_push(&format!("git push -f {tok} main"));
        assert_eq!(intent.remote.as_deref(), Some("https://github.com/o/r.git"));
        // Silent output → the fallback path, which reads the remote.
        let got = interpret_tool_use(&[SensitiveCommand::GitPush(intent)], "", "C:/r", false);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].repo.as_deref(), Some("o/r"));
        let body = got[0].body().to_string();
        assert!(!body.contains("SECRET"), "credential leaked: {body}");
        assert!(
            !got[0].artifact.contains("github.com"),
            "never a URL artifact"
        );

        // The To-line echo is stripped too.
        let out = format!("To {tok}\n + 1a2b3c4...5d6e7f8 main -> main (forced update)\n");
        let got = interpret_tool_use(
            &[SensitiveCommand::GitPush(git_push(
                "git push -f origin main",
            ))],
            &out,
            "",
            false,
        );
        assert_eq!(got.len(), 1);
        let t = got[0].ref_target.as_ref().unwrap();
        assert_eq!(t.remote_url.as_deref(), Some("https://github.com/o/r.git"));
        assert!(!format!("{:?}", got[0]).contains("SECRET"));
    }

    #[test]
    fn hosted_remotes_vs_local_paths() {
        for hosted in [
            "https://github.com/o/r.git",
            "ssh://git@github.com/o/r",
            "git@github.com:o/r.git",
            "gitlab.example.com:group/r",
        ] {
            assert!(is_hosted_remote(hosted), "{hosted}");
        }
        for local in [
            "/srv/git/r.git",
            "../r",
            "C:/work/r",
            "D:\\work\\r",
            "file:///srv/r.git",
            "",
        ] {
            assert!(!is_hosted_remote(local), "{local}");
        }
        // A push to a local path produces nothing.
        let out = "To /srv/git/r.git\n + 1a2b3c4...5d6e7f8 main -> main (forced update)\n";
        let got = interpret_tool_use(
            &[SensitiveCommand::GitPush(git_push(
                "git push -f /srv/git/r.git main",
            ))],
            out,
            "",
            false,
        );
        assert!(got.is_empty());
    }

    // ── Result interpretation ────────────────────────────────────────────

    fn push_cmds(cmd: &str) -> Vec<SensitiveCommand> {
        classify_sensitive_command(cmd)
    }

    #[test]
    fn forced_update_names_ref_prior_sha_and_repo() {
        let out = "To github.com:qontinui/qontinui-runner.git\n + 1a2b3c4...5d6e7f8 feat/x -> feat/x (forced update)\n";
        let got = interpret_tool_use(
            &push_cmds("git push --force-with-lease origin feat/x"),
            out,
            "C:/r",
            false,
        );
        assert_eq!(got.len(), 1);
        let a = &got[0];
        assert_eq!(a.action, SensitiveAction::ForcePush);
        assert_eq!(a.artifact, "feat/x");
        assert_eq!(a.reversible, reversibility::RESTORE);
        assert_eq!(a.undo.as_deref(), Some("1a2b3c4"));
        assert_eq!(a.repo.as_deref(), Some("qontinui/qontinui-runner"));
        assert_eq!(a.repo_from_remote, None, "the output named the repo");
        let t = a.ref_target.as_ref().unwrap();
        assert_eq!(t.dest.as_deref(), Some("feat/x"));
        assert_eq!(t.remote, "origin");
    }

    #[test]
    fn a_force_that_only_fast_forwarded_discarded_nothing() {
        for out in [
            "To github.com:o/r.git\n   1a2b3c4..5d6e7f8  main -> main\n",
            "Everything up-to-date\n",
        ] {
            let got = interpret_tool_use(&push_cmds("git push -f origin main"), out, "", false);
            assert!(got.is_empty(), "{out:?} discarded nothing: {got:?}");
        }
    }

    #[test]
    fn silent_output_reports_the_declared_force_without_undo() {
        let got = interpret_tool_use(&push_cmds("git push -q --force"), "", "C:/r", false);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].artifact, CURRENT_BRANCH_ARTIFACT);
        assert_eq!(
            got[0].reversible,
            reversibility::NO,
            "no prior sha is known"
        );
        assert_eq!(got[0].undo, None);
        assert_eq!(got[0].repo, None);
        assert_eq!(got[0].repo_from_remote.as_deref(), Some("origin"));
        assert_eq!(got[0].ref_target.as_ref().unwrap().dest, None);
        // …but never for a call that failed: nothing proves it happened.
        assert!(interpret_tool_use(&push_cmds("git push -q --force"), "", "C:/r", true).is_empty());
    }

    #[test]
    fn deleted_refs_are_read_from_the_output() {
        let out = "To https://github.com/o/r.git\n - [deleted]         old-a\n";
        let got = interpret_tool_use(&push_cmds("git push origin --delete old-a"), out, "", false);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].action, SensitiveAction::Delete);
        assert_eq!(got[0].artifact, "old-a");
        assert_eq!(got[0].reversible, reversibility::RESTORE);
        assert_eq!(got[0].repo.as_deref(), Some("o/r"));
    }

    #[test]
    fn a_rejected_push_piped_to_tail_reports_nothing() {
        // `| tail` makes the exit status 0, so is_error is false — the output
        // is the only evidence that nothing happened.
        let out = "To github.com:o/r.git\n ! [rejected]        main -> main (stale info)\nerror: failed to push some refs to 'github.com:o/r.git'\n";
        let got = interpret_tool_use(
            &push_cmds("git push -f origin main 2>&1 | tail -3"),
            out,
            "",
            false,
        );
        assert!(got.is_empty());
    }

    /// B2: `git fetch` / `git pull` print the same table shape under `From`.
    /// A fetch observing someone else's force-push is not this agent's action.
    #[test]
    fn fetch_output_is_never_read_as_push_output() {
        let out = "From github.com:o/r
 + 1111111...2222222 main       -> origin/main  (forced update)
";
        let got = interpret_tool_use(
            &push_cmds("git fetch origin && git push -f origin feat/x"),
            out,
            "",
            false,
        );
        // The fetch line names `main` and prior sha 1111111 — neither may
        // surface. (The push itself printed no To-block, so at most its own
        // DECLARED ref is reported, without an undo handle.)
        assert!(
            got.iter()
                .all(|a| a.artifact == "feat/x" && a.undo.is_none()),
            "a fetch-only forced line is not a push: {got:?}"
        );

        // Fetch then push in one call: only the To-block counts.
        let out = "From github.com:o/r\n + 1111111...2222222 main -> origin/main (forced update)\nTo github.com:o/r.git\n + 3333333...4444444 feat/x -> feat/x (forced update)\n";
        let got = interpret_tool_use(
            &push_cmds("git pull --rebase && git push -f origin feat/x"),
            out,
            "",
            false,
        );
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].artifact, "feat/x");
        assert_eq!(got[0].undo.as_deref(), Some("3333333"));
    }

    /// A push block followed by a fetch: the `From` line closes the block.
    #[test]
    fn a_from_line_closes_the_push_block() {
        let out = "To github.com:o/r.git\n + 3333333...4444444 feat/x -> feat/x (forced update)\nFrom github.com:o/r\n + 1111111...2222222 main -> origin/main (forced update)\n";
        let got = interpret_tool_use(
            &push_cmds("git push -f origin feat/x; git fetch"),
            out,
            "",
            false,
        );
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].artifact, "feat/x");
    }

    /// B2: output is interpreted once per tool call, and a declared
    /// destination filters what the output reports. Two force-pushes in one
    /// call report two refs, not four.
    #[test]
    fn several_pushes_in_one_call_report_each_ref_once() {
        let out = "To github.com:o/r.git\n + aaaaaaa...bbbbbbb feat/a -> feat/a (forced update)\nTo github.com:o/r.git\n + ccccccc...ddddddd feat/b -> feat/b (forced update)\n";
        let cmds = push_cmds("git push -f origin feat/a && git push -f origin feat/b");
        assert_eq!(cmds.len(), 2);
        let got = interpret_tool_use(&cmds, out, "", false);
        let arts: Vec<&str> = got.iter().map(|a| a.artifact.as_str()).collect();
        assert_eq!(arts, vec!["feat/a", "feat/b"]);
    }

    #[test]
    fn a_declared_destination_filters_the_reported_refs() {
        // `-f origin feat/a` plus a plain `origin main` in one call: the
        // table shows main fast-forwarded and — say, from a mirror config — a
        // forced line for an undeclared ref. Only the declared one counts.
        let out = "To github.com:o/r.git\n + aaaaaaa...bbbbbbb feat/a -> feat/a (forced update)\n + ccccccc...ddddddd other -> other (forced update)\n";
        let got = interpret_tool_use(&push_cmds("git push -f origin feat/a"), out, "", false);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].artifact, "feat/a");
        // `+HEAD:` names nothing the output can match: unconstrained.
        let out = "To github.com:o/r.git\n + aaaaaaa...bbbbbbb HEAD -> feat/z (forced update)\n";
        let got = interpret_tool_use(&push_cmds("git push origin +HEAD"), out, "", false);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].artifact, "feat/z");
    }

    /// S5: a push that moved one ref and was rejected on another exits
    /// non-zero — the forced line is still proof the first ref moved.
    #[test]
    fn positive_evidence_wins_over_is_error() {
        let out = "To github.com:o/r.git\n + aaaaaaa...bbbbbbb feat/a -> feat/a (forced update)\n ! [rejected]        main -> main (fetch first)\nerror: failed to push some refs\n";
        let got = interpret_tool_use(&push_cmds("git push -f origin feat/a main"), out, "", true);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].artifact, "feat/a");

        let npm = interpret_tool_use(
            &[SensitiveCommand::NpmPublish { spec: None }],
            "+ @q/ui@1.0.0\nnpm warn exit handler\n",
            "C:/r",
            true,
        );
        assert_eq!(npm.len(), 1);
        let cargo = interpret_tool_use(
            &[SensitiveCommand::CargoPublish { package: None }],
            "   Published q-x v0.1.0 at registry `crates-io`\nerror: something after\n",
            "C:/r",
            true,
        );
        assert_eq!(cargo.len(), 1);
        // No positive evidence + failed → nothing.
        assert!(interpret_tool_use(
            &[SensitiveCommand::NpmPublish { spec: None }],
            "",
            "C:/r",
            true
        )
        .is_empty());
    }

    #[test]
    fn gh_release_create_reads_repo_from_the_release_url() {
        let cmd = classify_sensitive_command("gh release create v2.0.0 --generate-notes");
        let got = interpret_tool_use(
            &cmd,
            "https://github.com/qontinui/qontinui-web/releases/tag/v2.0.0\n",
            "",
            false,
        );
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].action, SensitiveAction::Publish);
        assert_eq!(got[0].artifact, "release v2.0.0");
        assert_eq!(got[0].reversible, reversibility::NO);
        assert_eq!(got[0].repo.as_deref(), Some("qontinui/qontinui-web"));
        assert_eq!(got[0].ref_target, None, "publish is never noise-filtered");
    }

    #[test]
    fn gh_release_delete_is_roll_forward() {
        let cmd = classify_sensitive_command("gh release delete v1 --yes -R o/r");
        let got = interpret_tool_use(&cmd, "", "", false);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].action, SensitiveAction::Delete);
        assert_eq!(got[0].reversible, reversibility::ROLL_FORWARD);
        assert_eq!(got[0].repo.as_deref(), Some("o/r"));
    }

    #[test]
    fn registry_publishes_read_the_version_from_the_output() {
        let npm = interpret_tool_use(
            &[SensitiveCommand::NpmPublish { spec: None }],
            "npm notice Publishing to https://registry.npmjs.org/\n+ @qontinui/ui-bridge@0.27.0\n",
            "C:/r",
            false,
        );
        assert_eq!(npm.len(), 1);
        assert_eq!(npm[0].artifact, "@qontinui/ui-bridge@0.27.0");
        assert_eq!(npm[0].reversible, reversibility::NO);
        assert_eq!(
            npm[0].repo_from_remote, None,
            "a registry publish is not repo-scoped"
        );

        let cargo = interpret_tool_use(
            &[SensitiveCommand::CargoPublish { package: None }],
            "   Uploading qontinui-schemas v0.4.1 (C:/r)\n   Published qontinui-schemas v0.4.1 at registry `crates-io`\n",
            "C:/r",
            false,
        );
        assert_eq!(cargo[0].artifact, "crate qontinui-schemas v0.4.1");

        let failed = interpret_tool_use(
            &[SensitiveCommand::NpmPublish { spec: None }],
            "npm error code E403\n",
            "C:/r",
            false,
        );
        assert!(failed.is_empty());
    }

    /// The fallback artifact names a directory, never a full local path (a
    /// Windows path carries the user name).
    #[test]
    fn registry_fallback_artifacts_carry_no_local_path() {
        let npm = interpret_tool_use(
            &[SensitiveCommand::NpmPublish { spec: None }],
            "",
            "C:/Users/someone/work/ui-kit",
            false,
        );
        assert_eq!(npm[0].artifact, "npm package in ./ui-kit");
        let cargo = interpret_tool_use(
            &[SensitiveCommand::CargoPublish { package: None }],
            "",
            "C:\\Users\\someone\\work\\crate-x\\",
            false,
        );
        assert_eq!(cargo[0].artifact, "crate in ./crate-x");
        assert!(!npm[0].body().to_string().contains("someone"));
    }

    #[test]
    fn body_omits_absent_optionals_and_carries_undo_when_known() {
        let a = DetectedAction {
            action: SensitiveAction::ForcePush,
            artifact: "feat/x".into(),
            reversible: reversibility::RESTORE,
            undo: Some("1a2b3c4".into()),
            repo: Some("o/r".into()),
            repo_from_remote: None,
            working_dir: String::new(),
            ref_target: None,
        };
        assert_eq!(
            a.body(),
            json!({
                "action": "force_push",
                "artifact": "feat/x",
                "reversible": "restore",
                "repo": "o/r",
                "undo": "1a2b3c4",
            })
        );
        let b = DetectedAction {
            undo: None,
            repo: None,
            ..a
        };
        let body = b.body();
        assert!(body.get("undo").is_none(), "never `undo: null`");
        assert!(body.get("repo").is_none());
    }

    // ── The D8 noise rule ────────────────────────────────────────────────

    fn facts(current: Option<&str>, default: Option<&str>, tag: Option<bool>) -> RefFacts {
        RefFacts {
            current_branch: current.map(String::from),
            default_branch: default.map(String::from),
            dest_is_tag: tag,
        }
    }

    /// The D8 table. Each row: (dest, tags_or_mirror, facts, notify?, why).
    #[test]
    fn d8_noise_rule_table() {
        let rows: Vec<(Option<&str>, bool, RefFacts, bool, &str)> = vec![
            // Own non-default working branch → suppressed.
            (
                Some("feat/x"),
                false,
                facts(Some("feat/x"), Some("main"), Some(false)),
                false,
                "own branch",
            ),
            (
                Some("refs/heads/feat/x"),
                false,
                facts(Some("feat/x"), Some("main"), None),
                false,
                "own branch, full ref",
            ),
            (
                None,
                false,
                facts(Some("feat/x"), Some("main"), None),
                false,
                "unnamed = own branch",
            ),
            // (1) the default branch.
            (
                Some("main"),
                false,
                facts(Some("main"), Some("main"), Some(false)),
                true,
                "default, even when checked out",
            ),
            (
                Some("develop"),
                false,
                facts(Some("develop"), Some("develop"), None),
                true,
                "non-main default",
            ),
            (
                Some("master"),
                false,
                facts(Some("master"), None, None),
                true,
                "default unknown → master counts",
            ),
            (
                Some("main"),
                false,
                facts(Some("main"), None, None),
                true,
                "default unknown → main counts",
            ),
            (
                None,
                false,
                facts(Some("main"), Some("main"), None),
                true,
                "unnamed on the default branch",
            ),
            // (2) tags.
            (
                Some("refs/tags/v1"),
                false,
                facts(Some("v1"), Some("main"), None),
                true,
                "refs/tags/*",
            ),
            (
                Some("v1"),
                false,
                facts(Some("v1"), Some("main"), Some(true)),
                true,
                "a local tag of that name",
            ),
            (
                Some("feat/x"),
                true,
                facts(Some("feat/x"), Some("main"), None),
                true,
                "--tags / --mirror",
            ),
            // (3) release/* and hotfix/*.
            (
                Some("release/2.0"),
                false,
                facts(Some("release/2.0"), Some("main"), None),
                true,
                "release/*",
            ),
            (
                Some("hotfix/boom"),
                false,
                facts(Some("hotfix/boom"), Some("main"), None),
                true,
                "hotfix/*",
            ),
            // (4) a branch that is not the working dir's current branch.
            (
                Some("feat/other"),
                false,
                facts(Some("feat/x"), Some("main"), Some(false)),
                true,
                "someone else's branch",
            ),
            // Unknown never suppresses.
            (
                Some("feat/x"),
                false,
                facts(None, Some("main"), None),
                true,
                "current branch unknown",
            ),
            (None, false, facts(None, None, None), true, "nothing known"),
        ];
        for (dest, tags, f, want, why) in rows {
            assert_eq!(
                ref_action_is_notable(dest, tags, &f),
                want,
                "{why}: dest={dest:?} tags={tags} facts={f:?}"
            );
        }
    }

    // ── Fixture transcripts through the pending map ──────────────────────

    fn tool_use_line(id: &str, command: &str) -> String {
        json!({
            "type": "assistant",
            "cwd": "C:/work/qontinui-runner",
            "message": {"content": [
                {"type": "text", "text": "pushing"},
                {"type": "tool_use", "id": id, "name": "Bash", "input": {"command": command}},
            ]},
        })
        .to_string()
    }

    /// A result as Claude Code writes it: the text the model saw AND the raw
    /// streams under `toolUseResult` — the same output, twice.
    fn tool_result_line(id: &str, is_error: bool, content: &str) -> String {
        json!({
            "type": "user",
            "message": {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": id, "is_error": is_error, "content": content},
            ]},
            "toolUseResult": {"stdout": "", "stderr": content, "interrupted": false},
        })
        .to_string()
    }

    const FORCED_OUTPUT: &str = "To github.com:qontinui/qontinui-runner.git\n + 1a2b3c4...5d6e7f8 feat/x -> feat/x (forced update)";

    /// Unique ids per test: emitted ids are remembered PROCESS-wide.
    fn uid(tag: &str) -> String {
        format!("toolu_{tag}_{}", uuid::Uuid::new_v4().simple())
    }

    #[test]
    fn fixture_successful_force_push_emits() {
        let id = uid("ok");
        let mut t = SensitiveActionTracker::new();
        let t0 = Instant::now();
        assert!(t
            .observe_line(
                &tool_use_line(&id, "git push --force-with-lease origin feat/x"),
                t0
            )
            .is_empty());
        assert_eq!(t.pending_len(), 1, "the tool_use is parked");

        let got = t.observe_line(
            &tool_result_line(&id, false, FORCED_OUTPUT),
            t0 + Duration::from_secs(3),
        );
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].action, SensitiveAction::ForcePush);
        assert_eq!(got[0].undo.as_deref(), Some("1a2b3c4"));
        assert_eq!(got[0].working_dir, "C:/work/qontinui-runner");
        assert_eq!(got[0].body()["repo"], json!("qontinui/qontinui-runner"));
        assert_eq!(t.pending_len(), 0, "the result resolves the entry");
    }

    /// B1: the result text and `toolUseResult.stderr` carry the SAME ref
    /// table. Joining them would report the force-push twice.
    #[test]
    fn fixture_output_recorded_twice_emits_once() {
        let id = uid("twice");
        let mut t = SensitiveActionTracker::new();
        let t0 = Instant::now();
        t.observe_line(&tool_use_line(&id, "git push -f origin feat/x"), t0);
        let line = json!({
            "type": "user",
            "message": {"content": [
                {"type": "tool_result", "tool_use_id": id, "is_error": false,
                 "content": FORCED_OUTPUT},
            ]},
            "toolUseResult": {"stdout": "", "stderr": FORCED_OUTPUT, "interrupted": false},
        })
        .to_string();
        let got = t.observe_line(&line, t0);
        assert_eq!(got.len(), 1, "one forced line → one notification: {got:?}");
    }

    #[test]
    fn fixture_failed_force_push_does_not_emit() {
        let id = uid("fail");
        let mut t = SensitiveActionTracker::new();
        let t0 = Instant::now();
        t.observe_line(&tool_use_line(&id, "git push -f origin main"), t0);
        let got = t.observe_line(
            &tool_result_line(
                &id,
                true,
                "To github.com:o/r.git\n ! [rejected]        main -> main (fetch first)\nerror: failed to push some refs",
            ),
            t0,
        );
        assert!(got.is_empty(), "is_error with no positive evidence");
        assert_eq!(
            t.pending_len(),
            0,
            "the failed entry is released, not leaked"
        );
    }

    #[test]
    fn fixture_plain_push_does_not_emit() {
        let id = uid("plain");
        let mut t = SensitiveActionTracker::new();
        let t0 = Instant::now();
        t.observe_line(&tool_use_line(&id, "git push -u origin feat/x"), t0);
        assert_eq!(t.pending_len(), 0, "a plain push is never parked");
        let got = t.observe_line(
            &tool_result_line(
                &id,
                false,
                "To github.com:o/r.git\n * [new branch] feat/x -> feat/x",
            ),
            t0,
        );
        assert!(got.is_empty());
    }

    #[test]
    fn fixture_result_that_never_arrives_expires_without_emitting() {
        let id = uid("expire");
        let mut t = SensitiveActionTracker::new();
        let t0 = Instant::now();
        t.observe_line(&tool_use_line(&id, "git push --force"), t0);
        assert_eq!(t.pending_len(), 1);

        // Any later line past the TTL sweeps it — here an unrelated Bash use.
        t.observe_line(
            &tool_use_line(&uid("other"), "git status"),
            t0 + PENDING_ACTION_TTL + Duration::from_secs(1),
        );
        assert_eq!(t.pending_len(), 0, "expired");
        assert_eq!(t.expired(), 1);

        // A result that straggles in after expiry is NOT emitted.
        let got = t.observe_line(
            &tool_result_line(&id, false, FORCED_OUTPUT),
            t0 + PENDING_ACTION_TTL + Duration::from_secs(2),
        );
        assert!(got.is_empty());
    }

    #[test]
    fn a_result_inside_the_ttl_still_emits() {
        let id = uid("ttl");
        let mut t = SensitiveActionTracker::new();
        let t0 = Instant::now();
        t.observe_line(&tool_use_line(&id, "git push --force"), t0);
        let got = t.observe_line(
            &tool_result_line(&id, false, FORCED_OUTPUT),
            t0 + PENDING_ACTION_TTL - Duration::from_secs(1),
        );
        assert_eq!(got.len(), 1);
    }

    #[test]
    fn the_pending_map_is_bounded() {
        let tag = uid("bound");
        let mut t = SensitiveActionTracker::new();
        let t0 = Instant::now();
        let offered = PENDING_ACTION_CAPACITY + 44;
        for n in 0..offered {
            t.observe_line(
                &tool_use_line(&format!("{tag}_{n}"), "git push --force"),
                t0 + Duration::from_millis(n as u64),
            );
        }
        assert_eq!(t.pending_len(), PENDING_ACTION_CAPACITY);
        assert_eq!(t.evicted(), 44);
        // The OLDEST were evicted: the first id is gone, the last is held.
        assert!(t
            .observe_line(
                &tool_result_line(&format!("{tag}_0"), false, FORCED_OUTPUT),
                t0
            )
            .is_empty());
        assert_eq!(
            t.observe_line(
                &tool_result_line(&format!("{tag}_{}", offered - 1), false, FORCED_OUTPUT),
                t0,
            )
            .len(),
            1
        );
    }

    #[test]
    fn a_reread_transcript_does_not_notify_twice() {
        let id = uid("reread");
        let mut t = SensitiveActionTracker::new();
        let t0 = Instant::now();
        let use_line = tool_use_line(&id, "git push --force");
        let result = tool_result_line(&id, false, FORCED_OUTPUT);
        t.observe_line(&use_line, t0);
        assert_eq!(t.observe_line(&result, t0).len(), 1);
        // Truncation → the tail re-reads from offset 0.
        t.observe_line(&use_line, t0);
        assert!(t.observe_line(&result, t0).is_empty());
    }

    /// S3: the same `tool_use` in two transcript files (a subagent sidechain
    /// and its parent) is tailed by two trackers — it notifies once.
    #[test]
    fn one_tool_use_in_two_transcripts_notifies_once() {
        let id = uid("twofiles");
        let t0 = Instant::now();
        let use_line = tool_use_line(&id, "git push --force");
        let result = tool_result_line(&id, false, FORCED_OUTPUT);
        let mut a = SensitiveActionTracker::new();
        let mut b = SensitiveActionTracker::new();
        a.observe_line(&use_line, t0);
        b.observe_line(&use_line, t0);
        assert_eq!(a.observe_line(&result, t0).len(), 1);
        assert!(b.observe_line(&result, t0).is_empty());
    }

    #[test]
    fn a_stale_record_timestamp_does_not_emit() {
        let id = uid("stale");
        let mut t = SensitiveActionTracker::new();
        let t0 = Instant::now();
        t.observe_line(&tool_use_line(&id, "git push --force"), t0);
        let mut v: serde_json::Value =
            serde_json::from_str(&tool_result_line(&id, false, FORCED_OUTPUT)).unwrap();
        v["timestamp"] = json!("2020-01-01T00:00:00Z");
        assert!(t.observe_line(&v.to_string(), t0).is_empty());
    }

    #[test]
    fn an_interrupted_command_without_evidence_does_not_emit() {
        let id = uid("intr");
        let mut t = SensitiveActionTracker::new();
        let t0 = Instant::now();
        t.observe_line(&tool_use_line(&id, "cargo publish"), t0);
        let line = json!({
            "type": "user",
            "message": {"content": [
                {"type": "tool_result", "tool_use_id": id, "is_error": false, "content": ""},
            ]},
            "toolUseResult": {"stdout": "", "stderr": "", "interrupted": true},
        })
        .to_string();
        assert!(t.observe_line(&line, t0).is_empty());
    }

    /// S1: a `run_in_background` call's result only says it STARTED.
    #[test]
    fn a_background_command_is_never_reported_as_done() {
        let id = uid("bg");
        let mut t = SensitiveActionTracker::new();
        let t0 = Instant::now();
        t.observe_line(&tool_use_line(&id, "git push --force"), t0);
        let line = json!({
            "type": "user",
            "message": {"content": [
                {"type": "tool_result", "tool_use_id": id, "is_error": false,
                 "content": "Command running in background with ID: b12345"},
            ]},
            "toolUseResult": {"stdout": "", "stderr": "", "interrupted": false,
                              "backgroundTaskId": "b12345"},
        })
        .to_string();
        assert!(t.observe_line(&line, t0).is_empty());
        assert_eq!(t.pending_len(), 0);
    }

    #[test]
    fn the_ref_table_is_read_from_stderr_when_content_omits_it() {
        let id = uid("stderr");
        let mut t = SensitiveActionTracker::new();
        let t0 = Instant::now();
        t.observe_line(&tool_use_line(&id, "git push -f origin feat/x"), t0);
        let line = json!({
            "type": "user",
            "message": {"content": [
                {"type": "tool_result", "tool_use_id": id, "is_error": false,
                 "content": [{"type": "text", "text": ""}]},
            ]},
            "toolUseResult": {"stdout": "", "stderr": FORCED_OUTPUT, "interrupted": false},
        })
        .to_string();
        let got = t.observe_line(&line, t0);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].undo.as_deref(), Some("1a2b3c4"));
    }

    #[test]
    fn the_result_text_is_the_fallback_when_the_streams_are_empty() {
        let id = uid("textonly");
        let mut t = SensitiveActionTracker::new();
        let t0 = Instant::now();
        t.observe_line(&tool_use_line(&id, "git push -f origin feat/x"), t0);
        let line = json!({
            "type": "user",
            "message": {"content": [
                {"type": "tool_result", "tool_use_id": id, "is_error": false,
                 "content": FORCED_OUTPUT},
            ]},
        })
        .to_string();
        assert_eq!(t.observe_line(&line, t0).len(), 1);
    }

    #[test]
    fn notification_lane_is_deterministic_per_transcript() {
        let a = agent_notification_lane("0f6c1c4e-0000-4000-8000-000000000001");
        assert_eq!(
            a,
            agent_notification_lane("0f6c1c4e-0000-4000-8000-000000000001")
        );
        assert_ne!(
            a,
            agent_notification_lane("0f6c1c4e-0000-4000-8000-000000000002")
        );
    }
}

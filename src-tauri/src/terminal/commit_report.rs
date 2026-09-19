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
                // A relative `cd` is relative to the SESSION's cwd, never the
                // runner's own.
                return join_dir(transcript_cwd, &dir);
            }
        }
    }
    // 2. `git -C <dir>`.
    let toks: Vec<&str> = command.split_whitespace().collect();
    if let Some(idx) = toks.iter().position(|t| *t == "-C") {
        if let Some(dir) = toks.get(idx + 1) {
            let d = unquote(dir);
            if !d.is_empty() {
                return join_dir(transcript_cwd, &d);
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
                // Counted here; WARNed by the caller, which knows what the
                // drop costs (a re-reportable lineage row vs. a lost
                // notification).
                self.dropped.fetch_add(1, Ordering::Relaxed);
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
    let accepted = PUSH_DISPATCHER.try_dispatch(move || {
        handle_push_observation(&obs, &registrar);
    });
    if !accepted {
        warn!(
            queue_capacity = PUSH_QUEUE_CAPACITY,
            dropped_total = PUSH_DISPATCHER.dropped(),
            "commit_report: git worker queue full — dropping a push observation (coord dedups; \
             the next push re-reports it)"
        );
    }
    accepted
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
// 1. **Classify** the Bash `tool_use` command ([`classify_commands`]), with
//    the same first-word-of-a-segment rule [`command_is_git_push`] uses, so
//    `echo "git push --force"` is text, not a push. Each segment carries its
//    own working directory (`cd`, `git -C`), and `bash -c "…"`, `xargs`,
//    `sudo`, `timeout`, `env` … are looked through.
// 2. **Hold** the classification in a bounded pending map keyed on the
//    `tool_use_id` ([`SensitiveActionTracker`]). The transcript parser is
//    stateless per record and the `tool_result` arrives in a LATER `user`
//    record, so something has to remember the command in between. The map is
//    owned by the transcript watcher's per-session tail loop, capped at
//    [`PENDING_ACTION_CAPACITY`] entries and expires entries after
//    [`PENDING_ACTION_TTL`].
// 3. **Emit only what happened**: positive evidence in the output (a
//    `(forced update)` / `[deleted]` line inside a `To <url>` block — human
//    or `--porcelain` — npm's `+ name@ver`, cargo's `Published`) always
//    counts; anything inferred from the command alone counts only when the
//    result is not `is_error`, was not interrupted and did not run in the
//    background. The output is read ONCE per tool call, its `To` blocks are
//    attributed to the push segments that printed them, and a `From <url>`
//    (fetch) table never counts.
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
    /// `--dry-run` / `-n` — pushes nothing. Kept (not dropped) so its output
    /// block can be attributed to it rather than to a real push.
    pub dry_run: bool,
}

/// One classified command. A `GitPush` may be plain or a dry-run (see
/// [`is_sensitive`]); the other kinds are only ever sensitive ones.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SensitiveCommand {
    /// `git push` — see [`GitPushIntent`].
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
/// The words of a segment from its PROGRAM on. Skipped in front of it:
/// leading env assignments, shell keywords (`if`, `then`, `do`, `else`, `{`,
/// `!`, …) and transparent wrappers together with their own arguments —
/// `time`, `nohup`, `command`, `exec`, `env VAR=… [-i] [-u NAME]`,
/// `timeout [-s SIG] <dur>`, `sudo [-u USER]`, `nice [-n N]`,
/// `stdbuf -oL`, and `xargs [-I R] [-n N] …` (whose command is the one that
/// runs). So `if git push -f; then …`, `timeout 60 git push -f` and
/// `… | xargs git push origin --delete` are pushes.
fn command_words(segment: &[String]) -> &[String] {
    // Flags of each wrapper that take a SEPARATE value word.
    fn skip_flags(segment: &[String], mut i: usize, valued: &[&str]) -> usize {
        while i < segment.len() {
            let a = segment[i].as_str();
            if a == "--" {
                return i + 1;
            }
            if !a.starts_with('-') || a == "-" {
                break;
            }
            i += if valued.contains(&a) { 2 } else { 1 };
        }
        i
    }
    let n = segment.len();
    let mut i = 0;
    loop {
        while i < n && is_env_assignment(&segment[i]) {
            i += 1;
        }
        let Some(w) = segment.get(i).map(String::as_str) else {
            break;
        };
        let base = w.rsplit(['/', '\\']).next().unwrap_or(w);
        match base {
            "if" | "then" | "do" | "else" | "elif" | "while" | "until" | "{" | "!" => i += 1,
            "time" | "nohup" | "command" | "exec" => i = skip_flags(segment, i + 1, &[]),
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
                i = skip_flags(segment, i + 1, &["-s", "-k", "--signal", "--kill-after"]);
                i += 1; // the duration
            }
            "sudo" => {
                i = skip_flags(
                    segment,
                    i + 1,
                    &["-u", "-g", "-C", "-D", "-h", "-p", "-r", "-t", "-U", "-T"],
                );
            }
            "nice" => i = skip_flags(segment, i + 1, &["-n"]),
            "stdbuf" => i = skip_flags(segment, i + 1, &["-i", "-o", "-e"]),
            "xargs" => {
                i = skip_flags(
                    segment,
                    i + 1,
                    &["-I", "-n", "-P", "-L", "-d", "-E", "-s", "-a"],
                );
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

/// One classified command and the directory it runs in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassifiedCommand {
    pub cmd: SensitiveCommand,
    /// The segment's own working directory: the transcript `cwd`, moved by
    /// every `cd`/`pushd` segment before it (a relative one resolved against
    /// the directory in effect, never against the runner's own cwd) and by
    /// the segment's own `git -C`. Empty = unknown (`cd -`, `cd ~`, a
    /// relative `cd` with no base) — which makes the D8 rule notify.
    ///
    /// Known limit: a subshell `( cd x && … )` does not restore the directory
    /// afterwards, because segments are not scoped. The ownership check in
    /// [`gather_ref_facts`] is the backstop — a push whose remote does not
    /// match this directory's remote is treated as not-the-working-branch.
    pub dir: String,
}

/// Classify every command in a shell string, with each one's directory.
///
/// EVERY `git push` is returned — plain and dry-run ones included — because
/// the output's `To` blocks are attributed to push segments by position (see
/// [`interpret_tool_use`]), and a block from a plain push must not be taken
/// for a forced one. The other kinds are returned only when sensitive.
/// `bash -c "…"` / `sh -c "…"` are classified by their inner script.
pub fn classify_commands(command: &str, cwd: &str) -> Vec<ClassifiedCommand> {
    classify_commands_depth(command, cwd, 0)
}

fn classify_commands_depth(command: &str, cwd: &str, depth: u8) -> Vec<ClassifiedCommand> {
    let mut dir = cwd.to_string();
    let mut out = Vec::new();
    for seg in shell_segments(command) {
        let words = command_words(&seg);
        let Some(program) = words.first() else {
            continue;
        };
        if program_is(program, "cd") || program_is(program, "pushd") {
            let target = words[1..]
                .iter()
                .find(|w| !(w.starts_with('-') && w.len() > 1));
            dir = match target {
                Some(t) if t != "-" => join_dir(&dir, t),
                _ => String::new(), // `cd`, `cd -`: somewhere we cannot see
            };
            continue;
        }
        if ["bash", "sh", "zsh", "dash"]
            .iter()
            .any(|s| program_is(program, s))
        {
            if depth < 2 {
                if let Some(script) = shell_c_script(&words[1..]) {
                    out.extend(classify_commands_depth(script, &dir, depth + 1));
                }
            }
            continue;
        }
        if let Some(cmd) = classify_segment(words) {
            let seg_dir = if program_is(program, "git") {
                git_dash_c_dir(&words[1..], &dir)
            } else {
                dir.clone()
            };
            out.push(ClassifiedCommand { cmd, dir: seg_dir });
        }
    }
    out
}

/// The script of `bash -c '<script>'` (also `-lc`, `-ec`, `-xc` clusters).
fn shell_c_script(args: &[String]) -> Option<&str> {
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if a.starts_with('-') && !a.starts_with("--") && a[1..].contains('c') {
            return args.get(i + 1).map(String::as_str);
        }
        if !a.starts_with('-') {
            return None; // `bash script.sh` — a file we cannot see
        }
        i += 1;
    }
    None
}

/// The directory `git [-C a] [-C b] …` runs in: each `-C` resolved against
/// the previous one, exactly as git does.
fn git_dash_c_dir(args: &[String], base: &str) -> String {
    let mut dir = base.to_string();
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if a == "-C" {
            if let Some(d) = args.get(i + 1) {
                dir = join_dir(&dir, d);
            }
            i += 2;
            continue;
        }
        if matches!(a, "-c" | "--git-dir" | "--work-tree" | "--namespace") {
            i += 2;
            continue;
        }
        if a.starts_with('-') {
            i += 1;
            continue;
        }
        break;
    }
    dir
}

/// Resolve `rel` against `base`. Absolute paths (`/x`, `\x`, `C:/x`) stand on
/// their own; `~…` and a relative path with no base are unknown (empty).
fn join_dir(base: &str, rel: &str) -> String {
    let rel = rel.trim();
    if rel.is_empty() {
        return base.to_string();
    }
    if rel.starts_with('~') || rel.contains('$') {
        return String::new();
    }
    let bytes = rel.as_bytes();
    let absolute = rel.starts_with('/')
        || rel.starts_with('\\')
        || (bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic());
    if absolute {
        return rel.to_string();
    }
    if base.is_empty() {
        return String::new();
    }
    format!("{}/{}", base.trim_end_matches(['/', '\\']), rel)
}

/// Whether a classified command is one this notifier reports: a push that
/// really forces or deletes (not a dry-run), or any of the other kinds.
pub fn is_sensitive(cmd: &SensitiveCommand) -> bool {
    match cmd {
        SensitiveCommand::GitPush(i) => !i.dry_run && (i.force.is_some() || i.delete.is_some()),
        _ => true,
    }
}

/// Classify the SENSITIVE commands in a shell command string, without
/// directories — a convenience over [`classify_commands`].
///
/// A `git push` that force-updates (`--force`, `-f`, `--force-with-lease[=…]`,
/// `--mirror`, or a `+refspec`) and/or deletes (`--delete`, `-d`, `:ref`); a
/// non-draft `gh release create` or a `gh release delete`; `npm publish`; and
/// `cargo publish`. A `--dry-run` of any of them publishes nothing and is not
/// sensitive, and neither is a plain push.
pub fn classify_sensitive_command(command: &str) -> Vec<SensitiveCommand> {
    classify_commands(command, "")
        .into_iter()
        .filter(|c| is_sensitive(&c.cmd))
        .map(|c| c.cmd)
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
    Some(SensitiveCommand::GitPush(parse_git_push_args(
        &args[i + 1..],
    )))
}

/// Parse the arguments after `push`. Always an intent — a plain or dry-run
/// push has `force`/`delete` unset or `dry_run` set; see [`is_sensitive`].
fn parse_git_push_args(args: &[&str]) -> GitPushIntent {
    let mut remote: Option<String> = None;
    let mut refspecs: Vec<String> = Vec::new();
    let mut force_flag = false;
    let mut delete_flag = false;
    let mut tags = false;
    let mut dry_run = false;
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
                "--dry-run" => dry_run = true,
                "-o" | "--push-option" | "--repo" | "--receive-pack" | "--exec" => j += 1,
                s if s.starts_with("--force-with-lease=") => force_flag = true,
                s if s.starts_with("--") => {}
                s => {
                    // A short-flag cluster: `-f`, `-uf`, `-d`, `-n`.
                    let flags = &s[1..];
                    if flags.contains('n') {
                        dry_run = true; // -n is --dry-run
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

    // `+:` forces every matching branch: unnamed, like a bare `--force`.
    let forced_all = forced.iter().any(String::is_empty);
    forced.retain(|r| !r.is_empty());
    // A force flag with no non-delete refspec forces the unnamed default
    // (the current branch). A force flag beside ONLY `:ref` deletions forces
    // nothing — there is no ref for it to apply to.
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
    GitPushIntent {
        remote,
        force,
        delete,
        tags,
        dry_run,
    }
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

/// The destination of a force-push or ref deletion, as far as the
/// transcript can name it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DestRef {
    /// A plain ref name — from the output's ref table, or a literal in the
    /// command.
    Named(String),
    /// `HEAD` / `@`: the working dir's current branch, pushed to the
    /// same-named remote branch.
    CurrentBranch,
    /// No refspec: whatever `push.default` picks — `@{push}`.
    PushDefault,
    /// A ref only the shell knew — `$B`, `$(git branch --show-current)`, a
    /// glob. Never resolved, so it always notifies.
    Unresolvable(String),
}

/// Whether a declared ref is a PLAIN ref name. Anything the shell expands or
/// git pattern-matches (`$`, backquote, `*`, `?`, `[`, `{`, `~`, `^`, `:`)
/// is not: comparing it as text against the output would drop real refs.
pub fn is_plain_ref(r: &str) -> bool {
    !r.is_empty()
        && !r.contains(['$', '`', '*', '?', '[', '{', '~', '^', ':'])
        && r != "HEAD"
        && r != "@"
}

fn declared_ref(r: &str) -> DestRef {
    if r == "HEAD" || r == "@" {
        DestRef::CurrentBranch
    } else if is_plain_ref(r) {
        DestRef::Named(r.to_string())
    } else {
        DestRef::Unresolvable(r.to_string())
    }
}

/// The destination of a force-push or ref deletion. Carried to the
/// dispatcher worker, where the D8 noise rule ([`ref_action_is_notable`]) is
/// evaluated against git.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefTarget {
    pub dest: DestRef,
    /// The remote NAME the command used (`origin` by default), or a
    /// userinfo-stripped URL when the command gave one. Empty when several
    /// push segments disagreed and the block could not be attributed.
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
    /// The directory the command ran in (see [`ClassifiedCommand::dir`]).
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
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct PushBlock {
    /// The userinfo-stripped URL from the `To` line.
    url: String,
    /// `(destination ref, prior sha)` per forced update.
    forced: Vec<(String, String)>,
    /// Ref per deletion.
    deleted: Vec<String>,
}

/// What one `git push` printed: a ref table, or `Everything up-to-date`.
#[derive(Debug, Clone, PartialEq, Eq)]
enum PushUnit {
    Block(PushBlock),
    UpToDate,
}

/// What a tool call's output says its `git push`(es) did, in order.
#[derive(Debug, Default, PartialEq, Eq)]
struct PushOutput {
    units: Vec<PushUnit>,
    /// `fatal:` / `error: failed to push`.
    failed: bool,
}

/// A `--porcelain` ref-table line: `<flag>\t<from>:<to>\t<summary>`.
fn porcelain_fields(raw: &str) -> Option<(char, &str, &str)> {
    let mut chars = raw.chars();
    let flag = chars.next()?;
    if !matches!(flag, '+' | '-' | ' ' | '*' | '=' | '!') || chars.next() != Some('\t') {
        return None;
    }
    let mut parts = raw[2..].splitn(2, '\t');
    let refs = parts.next()?;
    let summary = parts.next().unwrap_or("");
    Some((flag, refs, summary))
}

/// A line of git's human ref table (`<flag> <summary> <from> -> <to>`).
fn is_ref_table_line(t: &str) -> bool {
    t.starts_with("+ ")
        || t.starts_with("- [deleted]")
        || t.starts_with("* [new")
        || t.starts_with("! [")
        || t.starts_with("= [up to date]")
        || t.contains(" -> ")
}

/// A ref name as the artifact names it: `refs/heads/` stripped (tags keep
/// their `refs/tags/` prefix, which the D8 rule reads).
fn artifact_ref(r: &str) -> String {
    r.strip_prefix("refs/heads/").unwrap_or(r).to_string()
}

/// Parse push output, counting ref-table lines ONLY inside a `To <url>`
/// block. `git fetch` / `git pull` in the same Bash call print the SAME
/// table shape under a `From <url>` header — `+ a...b main -> origin/main
/// (forced update)` — which is a fetch observing someone else's force-push,
/// not this agent's action. A block closes on `From `, on any non-table
/// line, or on the next `To `. Both the human and the `--porcelain` table
/// formats are read.
fn parse_push_output(output: &str) -> PushOutput {
    let mut out = PushOutput::default();
    let mut open: Option<PushBlock> = None;
    let close = |open: &mut Option<PushBlock>, out: &mut PushOutput| {
        if let Some(b) = open.take() {
            out.units.push(PushUnit::Block(b));
        }
    };
    for raw in output.lines() {
        let raw = raw.trim_end_matches('\r');
        if let (Some(block), Some((flag, refs, summary))) = (open.as_mut(), porcelain_fields(raw)) {
            let dst = refs.rsplit_once(':').map(|(_, d)| d).unwrap_or(refs);
            match flag {
                '+' => {
                    let prior = summary
                        .split_whitespace()
                        .next()
                        .and_then(|s| s.split("...").next())
                        .unwrap_or("")
                        .to_string();
                    block.forced.push((artifact_ref(dst), prior));
                }
                '-' => block.deleted.push(artifact_ref(dst)),
                _ => {}
            }
            continue;
        }
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(url) = line.strip_prefix("To ") {
            close(&mut open, &mut out);
            open = Some(PushBlock {
                url: strip_userinfo(url.trim()),
                ..PushBlock::default()
            });
            continue;
        }
        if line.starts_with("fatal:") || line.starts_with("error: failed to push") {
            out.failed = true;
            close(&mut open, &mut out);
            continue;
        }
        if line == "Everything up-to-date" {
            close(&mut open, &mut out);
            out.units.push(PushUnit::UpToDate);
            continue;
        }
        let Some(block) = open.as_mut() else {
            continue; // outside a To-block: fetch output, remote: lines, hints
        };
        if !is_ref_table_line(line) {
            close(&mut open, &mut out);
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
                .map(|s| artifact_ref(s))
                .unwrap_or_default();
            if !dst.is_empty() {
                block.forced.push((dst, prior));
            }
        } else if line.starts_with("- [deleted]") {
            if let Some(r) = line.split_whitespace().last() {
                block.deleted.push(artifact_ref(r));
            }
        }
    }
    close(&mut open, &mut out);
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
/// `refs/tags/` stripped (git's human table prints short names).
fn short_ref(r: &str) -> &str {
    r.strip_prefix("refs/heads/")
        .or_else(|| r.strip_prefix("refs/tags/"))
        .unwrap_or(r)
}

/// Which destinations a set of intents declared for one arm (force or
/// delete). `None` = UNCONSTRAINED: some intent left the ref unnamed, named
/// `HEAD`, or named something only the shell could expand (`$B`,
/// `$(git branch --show-current)`, a glob) — so every ref the output reports
/// counts. Only plain names constrain.
fn declared_dests(
    intents: &[&GitPushIntent],
    arm: impl Fn(&GitPushIntent) -> Option<&Vec<String>>,
) -> Option<Vec<String>> {
    let mut names = Vec::new();
    for i in intents {
        if let Some(refs) = arm(i) {
            if refs.is_empty() || refs.iter().any(|r| !is_plain_ref(r)) {
                return None;
            }
            names.extend(refs.iter().map(|r| short_ref(r).to_string()));
        }
    }
    Some(names)
}

/// Turn ONE tool call — every command it classified (with each one's
/// directory), and its one output — into the actions that happened,
/// de-duplicated by `(action, artifact)`.
///
/// `failed` is `is_error` (or an interrupted command). It does not veto
/// POSITIVE evidence — a forced/deleted line inside a `To` block, npm's
/// `+ name@ver`, cargo's `Published` — because a push that updated one ref
/// and was rejected on another exits non-zero after the first ref already
/// moved. It does veto everything inferred from the command alone.
pub fn interpret_tool_use(
    commands: &[ClassifiedCommand],
    output: &str,
    failed: bool,
) -> Vec<DetectedAction> {
    let mut out = Vec::new();
    let pushes: Vec<(&GitPushIntent, &str)> = commands
        .iter()
        .filter_map(|c| match &c.cmd {
            SensitiveCommand::GitPush(i) => Some((i, c.dir.as_str())),
            _ => None,
        })
        .collect();
    out.extend(interpret_git_pushes(&pushes, output, failed));
    for c in commands {
        if !matches!(c.cmd, SensitiveCommand::GitPush(_)) {
            out.extend(interpret_other(&c.cmd, output, &c.dir, failed));
        }
    }
    let mut seen = std::collections::HashSet::new();
    out.retain(|a| seen.insert((a.action, a.artifact.clone())));
    out
}

fn intent_is_sensitive(i: &GitPushIntent) -> bool {
    !i.dry_run && (i.force.is_some() || i.delete.is_some())
}

fn intent_remote(i: &GitPushIntent) -> String {
    i.remote.clone().unwrap_or_else(|| "origin".to_string())
}

/// The actions one push block evidences, filtered by what `intents`
/// declared.
fn block_actions(
    block: &PushBlock,
    intents: &[&GitPushIntent],
    dir: &str,
    remote: &str,
) -> Vec<DetectedAction> {
    // A push to a local path is not an outward action.
    if !is_hosted_remote(&block.url) {
        return Vec::new();
    }
    let any_force = intents.iter().any(|i| i.force.is_some());
    let any_delete = intents.iter().any(|i| i.delete.is_some());
    let tags_or_mirror = intents.iter().any(|i| i.tags);
    let force_dests = declared_dests(intents, |i| i.force.as_ref());
    let delete_dests = declared_dests(intents, |i| i.delete.as_ref());
    let matches = |declared: &Option<Vec<String>>, r: &str| match declared {
        None => true,
        Some(names) => names.iter().any(|n| n == short_ref(r)),
    };
    let repo = parse_repo_full_name(&block.url);
    let make = |action, artifact: &str, reversible, undo| DetectedAction {
        action,
        artifact: artifact.to_string(),
        reversible,
        undo,
        repo: repo.clone(),
        repo_from_remote: None,
        working_dir: dir.to_string(),
        ref_target: Some(RefTarget {
            dest: DestRef::Named(artifact.to_string()),
            remote: remote.to_string(),
            remote_url: Some(block.url.clone()),
            tags_or_mirror,
        }),
    };
    let mut out = Vec::new();
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
    out
}

/// What a sensitive push DECLARED, for when its output cannot be read: the
/// output was silent (`-q`, redirected), or could not be told apart from a
/// dry-run's. Only ever used for a call that did not fail.
fn declared_actions(intent: &GitPushIntent, dir: &str) -> Vec<DetectedAction> {
    let remote = intent_remote(intent);
    let url = url_remote(&remote);
    if url.as_deref().is_some_and(|u| !is_hosted_remote(u)) {
        return Vec::new();
    }
    let repo = url.as_deref().and_then(parse_repo_full_name);
    let make = |action, dest: DestRef, reversible| {
        let artifact = match &dest {
            DestRef::Named(n) => n.clone(),
            DestRef::Unresolvable(r) => r.clone(),
            DestRef::CurrentBranch | DestRef::PushDefault => CURRENT_BRANCH_ARTIFACT.to_string(),
        };
        DetectedAction {
            action,
            artifact,
            reversible,
            undo: None,
            repo: repo.clone(),
            repo_from_remote: if url.is_none() {
                Some(remote.clone())
            } else {
                None
            },
            working_dir: dir.to_string(),
            ref_target: Some(RefTarget {
                dest,
                remote: remote.clone(),
                remote_url: url.clone(),
                tags_or_mirror: intent.tags,
            }),
        }
    };
    let mut out = Vec::new();
    if let Some(declared) = &intent.force {
        // No prior sha is known, so the notification cannot name anything to
        // restore FROM.
        if declared.is_empty() {
            out.push(make(
                SensitiveAction::ForcePush,
                DestRef::PushDefault,
                reversibility::NO,
            ));
        }
        for r in declared {
            out.push(make(
                SensitiveAction::ForcePush,
                declared_ref(r),
                reversibility::NO,
            ));
        }
    }
    if let Some(declared) = &intent.delete {
        if declared.is_empty() {
            // `xargs git push origin --delete`: the refs came from stdin.
            out.push(make(
                SensitiveAction::Delete,
                DestRef::Unresolvable(format!("ref(s) on {remote}")),
                reversibility::RESTORE,
            ));
        }
        for r in declared {
            out.push(make(
                SensitiveAction::Delete,
                declared_ref(r),
                reversibility::RESTORE,
            ));
        }
    }
    out
}

/// Attribute the output's push units to the push segments that printed them.
///
/// When the number of units equals the number of push segments (every `git
/// push` in the call — plain and dry-run ones included — prints exactly one
/// `To` block or `Everything up-to-date`), unit *k* belongs to segment *k*:
/// its declared destinations, its directory and its remote apply, and a
/// dry-run's or a plain push's block is ignored. Otherwise the blocks are
/// read against the union of the sensitive segments — except when a dry-run
/// is present and the sensitive side is unconstrained, where a dry-run's
/// forced lines could not be told apart; then the output is excluded and the
/// declarations are reported instead.
fn interpret_git_pushes(
    pushes: &[(&GitPushIntent, &str)],
    output: &str,
    failed: bool,
) -> Vec<DetectedAction> {
    let sensitive: Vec<(&GitPushIntent, &str)> = pushes
        .iter()
        .copied()
        .filter(|(i, _)| intent_is_sensitive(i))
        .collect();
    if sensitive.is_empty() {
        return Vec::new();
    }
    let parsed = parse_push_output(output);
    let declared = || -> Vec<DetectedAction> {
        sensitive
            .iter()
            .flat_map(|(i, d)| declared_actions(i, d))
            .collect()
    };
    let mut out = Vec::new();
    if parsed.units.is_empty() {
        // Silent output: report the declarations — never for a failed call.
        if !parsed.failed && !failed {
            out.extend(declared());
        }
        return out;
    }
    if parsed.units.len() == pushes.len() {
        for (unit, (intent, dir)) in parsed.units.iter().zip(pushes) {
            if let PushUnit::Block(b) = unit {
                if intent_is_sensitive(intent) {
                    out.extend(block_actions(b, &[*intent], dir, &intent_remote(intent)));
                }
            }
        }
        return out;
    }
    let intents: Vec<&GitPushIntent> = sensitive.iter().map(|(i, _)| *i).collect();
    let has_dry_run = pushes.iter().any(|(i, _)| i.dry_run);
    let force_open = intents.iter().any(|i| i.force.is_some())
        && declared_dests(&intents, |i| i.force.as_ref()).is_none();
    let delete_open = intents.iter().any(|i| i.delete.is_some())
        && declared_dests(&intents, |i| i.delete.as_ref()).is_none();
    if has_dry_run && (force_open || delete_open) {
        if !failed {
            out.extend(declared());
        }
        return out;
    }
    // One directory and one remote only when every sensitive segment agrees;
    // otherwise unknown, which makes the D8 rule notify.
    let dir = if sensitive.iter().all(|(_, d)| *d == sensitive[0].1) {
        sensitive[0].1
    } else {
        ""
    };
    let remotes: Vec<String> = intents.iter().map(|i| intent_remote(i)).collect();
    for unit in &parsed.units {
        let PushUnit::Block(b) = unit else { continue };
        let remote = remotes
            .iter()
            .find(|r| url_remote(r).as_deref() == Some(b.url.as_str()))
            .cloned()
            .or_else(|| {
                remotes
                    .iter()
                    .all(|r| *r == remotes[0])
                    .then(|| remotes[0].clone())
            })
            .unwrap_or_default();
        out.extend(block_actions(b, &intents, dir, &remote));
    }
    out
}

/// The artifact of a force whose destination only git knows. The dispatcher
/// worker replaces it with the resolved branch name.
const CURRENT_BRANCH_ARTIFACT: &str = "the current branch";

/// `Some(url)` when a remote was given as a URL (userinfo stripped), `None`
/// for a remote NAME.
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
            // npm prints `+ <name>@<version>` per published package (one per
            // workspace with `--workspaces`) — positive evidence, every line.
            let published: Vec<String> = output
                .lines()
                .filter_map(|l| {
                    l.trim()
                        .strip_prefix("+ ")
                        .filter(|rest| rest.contains('@'))
                        .map(|rest| rest.trim().to_string())
                })
                .collect();
            if !published.is_empty() {
                return published
                    .into_iter()
                    .map(|p| registry_publish(p, working_dir))
                    .collect();
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
            // cargo prints `Published <name> v<version> at registry …` per
            // crate once the upload is accepted (several with `--workspace`)
            // — positive evidence, every line.
            let crate_line = |prefix: &str| -> Vec<String> {
                output
                    .lines()
                    .filter_map(|l| {
                        let rest = l.trim().strip_prefix(prefix)?;
                        let mut it = rest.split_whitespace();
                        let name = it.next()?;
                        let version = it.next()?;
                        Some(format!("crate {name} {version}"))
                    })
                    .collect()
            };
            let published = crate_line("Published ");
            if !published.is_empty() {
                return published
                    .into_iter()
                    .map(|p| registry_publish(p, working_dir))
                    .collect();
            }
            if failed || any_line_starts_with(output, &["error:", "error["]) {
                return Vec::new();
            }
            // Older cargo prints only `Uploading <name> v<version>`.
            let uploading = crate_line("Uploading ");
            if !uploading.is_empty() {
                return uploading
                    .into_iter()
                    .map(|p| registry_publish(p, working_dir))
                    .collect();
            }
            let artifact = match package {
                Some(p) => format!("crate {p}"),
                None => format!("crate in ./{}", dir_basename(working_dir)),
            };
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
/// when git could not answer — or, for the working-dir fields, when the
/// pushed repo is not provably this working dir's — and an unanswered
/// question never suppresses a notification.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RefFacts {
    /// The working dir's checked-out branch (`git rev-parse --abbrev-ref
    /// HEAD`); `None` on failure, a detached HEAD, or an ownership mismatch.
    pub current_branch: Option<String>,
    /// The remote's default branch (`git symbolic-ref --short
    /// refs/remotes/<remote>/HEAD`, remote prefix stripped).
    pub default_branch: Option<String>,
    /// Whether the destination names a local tag (`git tag --list <dest>`).
    pub dest_is_tag: Option<bool>,
    /// Where an unnamed push goes (`git rev-parse --abbrev-ref
    /// --symbolic-full-name @{push}`, remote prefix stripped).
    pub push_dest: Option<String>,
}

/// Plan design decision D8 — which force-pushes and ref deletions are worth
/// the operator's attention. Notify ONLY when the destination (with
/// `refs/heads/` stripped) is:
///
/// 1. the remote's default branch, and `main` / `master` ALWAYS — a stale
///    `refs/remotes/<remote>/HEAD` must not hide `main`;
/// 2. a tag (`refs/tags/*`, a local tag of that name, or a `--tags` /
///    `--mirror` push);
/// 3. `release/*` or `hotfix/*`;
/// 4. a branch that is NOT the working dir's current branch — someone
///    else's branch, or at least not the one this agent is working on.
///
/// A force-push or delete of the agent's OWN non-default working branch is
/// routine (rebase, amend, cleanup) and is suppressed. `dest` is `None` when
/// the destination could not be resolved, and anything the rule cannot
/// evaluate notifies: unknown must never suppress. `publish` never reaches
/// this function; it always notifies.
///
/// Accepted limitations:
/// - The current branch is read on the dispatcher worker AFTER the command
///   ran, not at push time; a session that switched branches in the same
///   Bash call is compared against the branch it ended on.
/// - A detached HEAD has no current branch, so every force-push from one
///   notifies. That is noise, not loss, and is accepted.
pub fn ref_action_is_notable(dest: Option<&str>, tags_or_mirror: bool, facts: &RefFacts) -> bool {
    if tags_or_mirror {
        return true;
    }
    let Some(dest) = dest else {
        return true;
    };
    if dest.starts_with("refs/tags/") || facts.dest_is_tag == Some(true) {
        return true;
    }
    let d = dest.strip_prefix("refs/heads/").unwrap_or(dest);
    if d.starts_with("release/") || d.starts_with("hotfix/") {
        return true;
    }
    if d == "main" || d == "master" || facts.default_branch.as_deref() == Some(d) {
        return true;
    }
    match facts.current_branch.as_deref() {
        Some(cb) => d != cb,
        None => true,
    }
}

/// The concrete destination of `dest`, given what git said. `None` =
/// unresolved (notify).
pub fn resolve_dest(dest: &DestRef, facts: &RefFacts) -> Option<String> {
    match dest {
        DestRef::Named(n) => Some(n.clone()),
        DestRef::CurrentBranch => facts.current_branch.clone(),
        DestRef::PushDefault => facts.push_dest.clone(),
        DestRef::Unresolvable(_) => None,
    }
}

/// Ask git the questions [`ref_action_is_notable`] needs. Impure — run on
/// the dispatcher worker, never the tail loop.
///
/// OWNERSHIP: the working-dir facts (current branch, `@{push}`) describe the
/// pushed repo only if the push went to this working dir's `<remote>`. When
/// the output's `To` URL names a different owner/repo than `git remote
/// get-url <remote>` in the working dir — a `cd` the segment tracking could
/// not follow, a subshell, a URL remote, a fork — they are withheld, so the
/// rule notifies.
fn gather_ref_facts(working_dir: &str, target: &RefTarget) -> RefFacts {
    if working_dir.trim().is_empty() || target.remote.is_empty() {
        return RefFacts::default();
    }
    let dir = PathBuf::from(working_dir);
    let remote_is_name = url_remote(&target.remote).is_none();
    let owned = match &target.remote_url {
        Some(pushed_url) => {
            let pushed = parse_repo_full_name(pushed_url);
            let local = remote_is_name
                .then(|| git(&dir, &["remote", "get-url", &target.remote]))
                .flatten()
                .and_then(|u| parse_repo_full_name(&strip_userinfo(&u)));
            pushed.is_some() && pushed == local
        }
        // No `To` line (quiet push): the command ran in this dir against
        // this dir's own remote, which is all a quiet push can tell us.
        None => remote_is_name,
    };
    let current_branch = owned
        .then(|| git(&dir, &["rev-parse", "--abbrev-ref", "HEAD"]))
        .flatten()
        .filter(|b| !b.is_empty() && b != "HEAD");
    let remote_prefix = format!("{}/", target.remote);
    let default_branch = (owned && remote_is_name)
        .then(|| {
            let head_ref = format!("refs/remotes/{}/HEAD", target.remote);
            git(&dir, &["symbolic-ref", "--short", &head_ref])
        })
        .flatten()
        .map(|s| s.strip_prefix(&remote_prefix).unwrap_or(&s).to_string())
        .filter(|s| !s.is_empty());
    let push_dest = (owned && remote_is_name)
        .then(|| {
            git(
                &dir,
                &[
                    "rev-parse",
                    "--abbrev-ref",
                    "--symbolic-full-name",
                    "@{push}",
                ],
            )
        })
        .flatten()
        // `@{push}` on another remote is not where THIS push went.
        .and_then(|s| s.strip_prefix(&remote_prefix).map(str::to_string))
        .filter(|s| !s.is_empty());
    let dest_is_tag = match &target.dest {
        DestRef::Named(d) => {
            let name = d.strip_prefix("refs/tags/").unwrap_or(d);
            git(&dir, &["tag", "--list", name]).map(|out| out.lines().any(|l| l.trim() == name))
        }
        _ => None,
    };
    RefFacts {
        current_branch,
        default_branch,
        dest_is_tag,
        push_dest,
    }
}

/// Settle what a detected action still needs from git, on the dispatcher
/// worker: the remote URL (dropping a push to a local path), the repo, the
/// resolved destination, and — for a force-push or delete — the D8 noise
/// rule. `None` means "do not notify".
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
        let dest = resolve_dest(&target.dest, &facts);
        if matches!(target.dest, DestRef::CurrentBranch | DestRef::PushDefault) {
            if let Some(d) = &dest {
                action.artifact = d.clone();
            }
        }
        if !ref_action_is_notable(dest.as_deref(), target.tags_or_mirror, &facts) {
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
    commands: Vec<ClassifiedCommand>,
    observed_at: Instant,
}

/// One tool call's detected actions, keyed by its `tool_use_id`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectedToolUse {
    pub tool_use_id: String,
    pub actions: Vec<DetectedAction>,
}

/// PROCESS-GLOBAL memory of `tool_use_id`s whose notifications were handed
/// to the worker, bounded to [`EMITTED_MEMORY`]. Claude `toolu_…` ids are
/// globally unique, and the same `tool_use` can appear in two transcript
/// files (a subagent's sidechain and its parent, a resumed session) tailed
/// by two trackers — one notification per id, not one per file. It also
/// covers a truncated transcript re-read from offset 0.
///
/// An id is recorded only AFTER its job was accepted by the worker queue
/// ([`dispatch_detected_tool_use`]), so a dropped dispatch leaves the id
/// free for another transcript to report.
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

/// Whether `id`'s notifications were already handed to the worker.
pub fn emission_recorded(id: &str) -> bool {
    EMITTED_TOOL_USES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .1
        .contains(id)
}

/// Record `id` as handed to the worker.
pub(crate) fn record_emission(id: &str) {
    let mut g = EMITTED_TOOL_USES.lock().unwrap_or_else(|e| e.into_inner());
    let (order, set) = &mut *g;
    if !set.insert(id.to_string()) {
        return;
    }
    if order.len() >= EMITTED_MEMORY {
        if let Some(old) = order.pop_front() {
            set.remove(&old);
        }
    }
    order.push_back(id.to_string());
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

    /// Feed one transcript line. Returns the tool calls whose result this
    /// line carried, with the actions each evidences. Pure apart from `now`
    /// and a read of the process-global emitted-id set — never runs git or
    /// I/O. The caller hands each result to [`dispatch_detected_tool_use`].
    pub fn observe_line(&mut self, line: &str, now: Instant) -> Vec<DetectedToolUse> {
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
            let commands = classify_commands(command, transcript_cwd);
            if !commands.iter().any(|c| is_sensitive(&c.cmd)) {
                continue;
            }
            self.insert(
                id.to_string(),
                PendingAction {
                    commands,
                    observed_at: now,
                },
            );
        }
    }

    fn observe_tool_results(&mut self, record: &serde_json::Value) -> Vec<DetectedToolUse> {
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
            if emission_recorded(id) {
                continue; // another transcript (or a re-read) already reported it
            }
            // A `run_in_background` call returns at once with a task id; its
            // result says the command STARTED, not that it did anything.
            // Without `toolUseResult` the result text is the only witness.
            let backgrounded = match side {
                Some(s) => s.get("backgroundTaskId").is_some_and(|v| !v.is_null()),
                None => tool_result_text(block).contains("Command running in background"),
            };
            if backgrounded {
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
            let actions = interpret_tool_use(&pending.commands, &output, is_error || interrupted);
            if actions.is_empty() {
                continue;
            }
            out.push(DetectedToolUse {
                tool_use_id: id.to_string(),
                actions,
            });
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

/// Hand one tool call's detected actions to the bounded git worker as ONE
/// job, which settles each against git ([`finalize_detected_action`]) and
/// enqueues the notifications on the session outbox. Non-blocking.
///
/// The `tool_use_id` is recorded as emitted only once the worker queue
/// ACCEPTED the job. A refused job is a lost notification for an action that
/// already happened — unlike a dropped commit-lineage report, nothing will
/// re-report it — so it is WARNed as such, and the id stays free for another
/// transcript carrying the same `tool_use` to report.
pub fn dispatch_detected_tool_use(
    detected: DetectedToolUse,
    lane: uuid::Uuid,
    registrar: Arc<crate::claude_session::coord_register::AiCoordRegistrar>,
) -> bool {
    if emission_recorded(&detected.tool_use_id) {
        return true;
    }
    let id = detected.tool_use_id.clone();
    let count = detected.actions.len();
    let accepted = PUSH_DISPATCHER.try_dispatch(move || {
        for action in detected.actions {
            if let Some(action) = finalize_detected_action(action) {
                registrar.report_agent_notification(lane, action.body());
            }
        }
    });
    if accepted {
        record_emission(&id);
    } else {
        warn!(
            tool_use_id = %id,
            actions = count,
            queue_capacity = PUSH_QUEUE_CAPACITY,
            dropped_total = PUSH_DISPATCHER.dropped(),
            "commit_report: git worker queue full — DROPPED a sensitive-action notification; \
             the action already happened and nothing will re-report it"
        );
    }
    accepted
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
/// the sensitive-action classifier, the result interpreter, the D8 noise rule
/// and the pending map, driven by fixture transcripts.
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
                "{cmd:?} must not classify as sensitive"
            );
        }
        // …but every push is still RETURNED, for block attribution.
        let all = classify_commands("git push -n -f origin x && git push origin y", "C:/w");
        assert_eq!(all.len(), 2);
        assert!(matches!(&all[0].cmd, SensitiveCommand::GitPush(i) if i.dry_run));
    }

    /// The same first-word-of-a-segment rule [`command_is_git_push`] applies:
    /// a push MENTIONED as text is not a push.
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
        assert!(!command_is_git_push("echo git push --force"));
        assert!(command_is_git_push("cd x && git push -f"));
        assert_eq!(classify_sensitive_command("cd x && git push -f").len(), 1);
    }

    #[test]
    fn heredoc_bodies_are_not_commands() {
        let cmd =
            "git commit -F - <<'EOF'\nfix: undo it\ngit push --force\nnpm publish\nEOF\ngit log -1";
        assert!(classify_sensitive_command(cmd).is_empty());
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
            "bash script.sh",
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
            "sudo -u deploy git push -f origin x",
            "nice -n 10 git push -f origin x",
            "stdbuf -oL git push -f origin x",
            "echo old | xargs -n 1 git push origin --delete",
            "bash -c 'git push -f origin x'",
            "sh -c \"cd sub && git push -f origin x\"",
            "bash -lc 'git push -f origin x'",
        ] {
            assert_eq!(
                classify_sensitive_command(cmd).len(),
                1,
                "{cmd:?} should classify as one sensitive command"
            );
        }
    }

    #[test]
    fn the_matching_refspec_is_not_a_deletion() {
        assert!(classify_sensitive_command("git push origin :").is_empty());
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

    // ── Per-segment working directories ──────────────────────────────────

    fn dirs(cmd: &str, cwd: &str) -> Vec<String> {
        classify_commands(cmd, cwd)
            .into_iter()
            .map(|c| c.dir)
            .collect()
    }

    #[test]
    fn each_segment_carries_its_own_directory() {
        assert_eq!(dirs("git push -f", "C:/w"), vec!["C:/w"]);
        assert_eq!(dirs("cd sub && git push -f", "C:/w"), vec!["C:/w/sub"]);
        assert_eq!(dirs("cd /abs/r && git push -f", "C:/w"), vec!["/abs/r"]);
        assert_eq!(dirs("cd D:/r && git push -f", "C:/w"), vec!["D:/r"]);
        assert_eq!(
            dirs("cd a && git push -f; cd b && git push -f", "C:/w"),
            vec!["C:/w/a", "C:/w/a/b"],
            "every cd moves the directory for what follows"
        );
        assert_eq!(
            dirs("git -C a -C b push -f", "C:/w"),
            vec!["C:/w/a/b"],
            "each -C resolves against the previous one"
        );
        assert_eq!(
            dirs("git -C first push -f && git -C second push -f", "C:/w"),
            vec!["C:/w/first", "C:/w/second"],
            "the first -C does not win for the second push"
        );
        assert_eq!(dirs("cd - && git push -f", "C:/w"), vec![""]);
        assert_eq!(dirs("cd ~/x && git push -f", "C:/w"), vec![""]);
        assert_eq!(dirs("cd sub && git push -f", ""), vec![""]);
        assert_eq!(
            dirs("sh -c 'cd sub && git push -f'", "C:/w"),
            vec!["C:/w/sub"]
        );
    }

    #[test]
    fn resolve_working_dir_joins_a_relative_cd_to_the_session_cwd() {
        assert_eq!(
            resolve_working_dir("cd sub && git push", "C:/w"),
            "C:/w/sub"
        );
        assert_eq!(
            resolve_working_dir("git -C rel push", "/home/u"),
            "/home/u/rel"
        );
        assert_eq!(resolve_working_dir("cd /abs && git push", "C:/w"), "/abs");
    }

    // ── URL hygiene ──────────────────────────────────────────────────────

    fn at(cmd: &str) -> Vec<ClassifiedCommand> {
        classify_commands(cmd, "C:/r")
    }

    fn one(cmd: SensitiveCommand, dir: &str) -> Vec<ClassifiedCommand> {
        vec![ClassifiedCommand {
            cmd,
            dir: dir.to_string(),
        }]
    }

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
        let got = interpret_tool_use(&at(&format!("git push -f {tok} main")), "", false);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].repo.as_deref(), Some("o/r"));
        let body = got[0].body().to_string();
        assert!(!body.contains("SECRET"), "credential leaked: {body}");
        assert!(
            !got[0].artifact.contains("github.com"),
            "never a URL artifact"
        );

        let out = format!("To {tok}\n + 1a2b3c4...5d6e7f8 main -> main (forced update)\n");
        let got = interpret_tool_use(&at("git push -f origin main"), &out, false);
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
        let out = "To /srv/git/r.git\n + 1a2b3c4...5d6e7f8 main -> main (forced update)\n";
        let got = interpret_tool_use(&at("git push -f /srv/git/r.git main"), out, false);
        assert!(got.is_empty());
    }

    // ── Result interpretation ────────────────────────────────────────────

    #[test]
    fn forced_update_names_ref_prior_sha_and_repo() {
        let out = "To github.com:qontinui/qontinui-runner.git\n + 1a2b3c4...5d6e7f8 feat/x -> feat/x (forced update)\n";
        let got = interpret_tool_use(&at("git push --force-with-lease origin feat/x"), out, false);
        assert_eq!(got.len(), 1);
        let a = &got[0];
        assert_eq!(a.action, SensitiveAction::ForcePush);
        assert_eq!(a.artifact, "feat/x");
        assert_eq!(a.reversible, reversibility::RESTORE);
        assert_eq!(a.undo.as_deref(), Some("1a2b3c4"));
        assert_eq!(a.repo.as_deref(), Some("qontinui/qontinui-runner"));
        assert_eq!(a.working_dir, "C:/r");
        let t = a.ref_target.as_ref().unwrap();
        assert_eq!(t.dest, DestRef::Named("feat/x".into()));
        assert_eq!(t.remote, "origin");
    }

    #[test]
    fn porcelain_ref_tables_are_read() {
        let out = "To github.com:o/r.git\n+\trefs/heads/feat/x:refs/heads/feat/x\t1a2b3c4...5d6e7f8 (forced update)\n-\t:refs/heads/old\t[deleted]\nDone\n";
        let got = interpret_tool_use(
            &at("git push --porcelain -f origin feat/x :old"),
            out,
            false,
        );
        let arts: Vec<(SensitiveAction, &str, Option<&str>)> = got
            .iter()
            .map(|a| (a.action, a.artifact.as_str(), a.undo.as_deref()))
            .collect();
        assert_eq!(
            arts,
            vec![
                (SensitiveAction::ForcePush, "feat/x", Some("1a2b3c4")),
                (SensitiveAction::Delete, "old", None),
            ]
        );
    }

    #[test]
    fn a_force_that_only_fast_forwarded_discarded_nothing() {
        for out in [
            "To github.com:o/r.git\n   1a2b3c4..5d6e7f8  main -> main\n",
            "Everything up-to-date\n",
        ] {
            let got = interpret_tool_use(&at("git push -f origin main"), out, false);
            assert!(got.is_empty(), "{out:?} discarded nothing: {got:?}");
        }
    }

    #[test]
    fn silent_output_reports_the_declared_force_without_undo() {
        let got = interpret_tool_use(&at("git push -q --force"), "", false);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].artifact, CURRENT_BRANCH_ARTIFACT);
        assert_eq!(
            got[0].reversible,
            reversibility::NO,
            "no prior sha is known"
        );
        assert_eq!(got[0].undo, None);
        assert_eq!(got[0].repo_from_remote.as_deref(), Some("origin"));
        assert_eq!(
            got[0].ref_target.as_ref().unwrap().dest,
            DestRef::PushDefault
        );
        assert!(interpret_tool_use(&at("git push -q --force"), "", true).is_empty());
    }

    #[test]
    fn a_quiet_head_or_expanded_destination_is_not_taken_literally() {
        let dest = |cmd: &str| {
            interpret_tool_use(&at(cmd), "", false)[0]
                .ref_target
                .as_ref()
                .unwrap()
                .dest
                .clone()
        };
        assert_eq!(dest("git push -q -f origin HEAD"), DestRef::CurrentBranch);
        assert_eq!(
            dest("git push -q -f origin \"$B\""),
            DestRef::Unresolvable("$B".into())
        );
        assert_eq!(
            dest("git push -q -f origin main"),
            DestRef::Named("main".into())
        );
    }

    #[test]
    fn deleted_refs_are_read_from_the_output() {
        let out = "To https://github.com/o/r.git\n - [deleted]         old-a\n";
        let got = interpret_tool_use(&at("git push origin --delete old-a"), out, false);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].action, SensitiveAction::Delete);
        assert_eq!(got[0].artifact, "old-a");
        assert_eq!(got[0].reversible, reversibility::RESTORE);
        assert_eq!(got[0].repo.as_deref(), Some("o/r"));
    }

    #[test]
    fn a_rejected_push_piped_to_tail_reports_nothing() {
        let out = "To github.com:o/r.git\n ! [rejected]        main -> main (stale info)\nerror: failed to push some refs to 'github.com:o/r.git'\n";
        let got = interpret_tool_use(&at("git push -f origin main 2>&1 | tail -3"), out, false);
        assert!(got.is_empty());
    }

    // ── Round-2 blocking #1: declared destinations that are not plain names ──

    #[test]
    fn plain_ref_names() {
        for plain in ["main", "feat/x", "refs/heads/feat/x", "v1.2.3"] {
            assert!(is_plain_ref(plain), "{plain}");
        }
        for not_plain in [
            "$B",
            "$",
            "$(git branch --show-current)",
            "`git branch --show-current`",
            "refs/heads/*",
            "feat/?",
            "feat/[ab]",
            "{a,b}",
            "main~1",
            "main^",
            "a:b",
            "HEAD",
            "@",
            "",
        ] {
            assert!(!is_plain_ref(not_plain), "{not_plain}");
        }
    }

    #[test]
    fn a_command_substituted_destination_does_not_drop_the_forced_ref() {
        let out = "To github.com:o/r.git\n + 1111111...2222222 main -> main (forced update)\n";
        // Quoted: one word, `$(…)`.
        let got = interpret_tool_use(
            &at("git push -f origin \"$(git branch --show-current)\""),
            out,
            false,
        );
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].artifact, "main");
        // Unquoted: the lexer leaves a bare `$` fragment.
        let got = interpret_tool_use(
            &at("git push -f origin $(git branch --show-current)"),
            out,
            false,
        );
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].artifact, "main");
    }

    #[test]
    fn a_for_loop_delete_does_not_drop_the_deleted_refs() {
        let out = "To github.com:o/r.git\n - [deleted]         old-a\nTo github.com:o/r.git\n - [deleted]         old-b\n";
        let got = interpret_tool_use(
            &at("for b in old-a old-b; do git push origin --delete $b; done"),
            out,
            false,
        );
        let arts: Vec<&str> = got.iter().map(|a| a.artifact.as_str()).collect();
        assert_eq!(arts, vec!["old-a", "old-b"]);
    }

    #[test]
    fn a_glob_refspec_does_not_drop_the_forced_refs() {
        let out = "To github.com:o/r.git\n + 1111111...2222222 main -> main (forced update)\n + 3333333...4444444 dev -> dev (forced update)\n";
        let got = interpret_tool_use(
            &at("git push -f origin 'refs/heads/*:refs/heads/*'"),
            out,
            false,
        );
        let arts: Vec<&str> = got.iter().map(|a| a.artifact.as_str()).collect();
        assert_eq!(arts, vec!["main", "dev"]);
    }

    // ── Block attribution ────────────────────────────────────────────────

    #[test]
    fn fetch_output_is_never_read_as_push_output() {
        let out = "From github.com:o/r\n + 1111111...2222222 main       -> origin/main  (forced update)\n";
        let got = interpret_tool_use(
            &at("git fetch origin && git push -f origin feat/x"),
            out,
            false,
        );
        assert!(
            got.iter()
                .all(|a| a.artifact == "feat/x" && a.undo.is_none()),
            "a fetch-only forced line is not a push: {got:?}"
        );

        let out = "From github.com:o/r\n + 1111111...2222222 main -> origin/main (forced update)\nTo github.com:o/r.git\n + 3333333...4444444 feat/x -> feat/x (forced update)\n";
        let got = interpret_tool_use(
            &at("git pull --rebase && git push -f origin feat/x"),
            out,
            false,
        );
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].artifact, "feat/x");
        assert_eq!(got[0].undo.as_deref(), Some("3333333"));
    }

    #[test]
    fn a_from_line_closes_the_push_block() {
        let out = "To github.com:o/r.git\n + 3333333...4444444 feat/x -> feat/x (forced update)\nFrom github.com:o/r\n + 1111111...2222222 main -> origin/main (forced update)\n";
        let got = interpret_tool_use(&at("git push -f origin feat/x; git fetch"), out, false);
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].artifact, "feat/x");
    }

    #[test]
    fn several_pushes_in_one_call_report_each_ref_once_in_its_own_dir() {
        let out = "To github.com:o/r.git\n + aaaaaaa...bbbbbbb feat/a -> feat/a (forced update)\nTo github.com:o/r.git\n + ccccccc...ddddddd feat/b -> feat/b (forced update)\n";
        let got = interpret_tool_use(
            &at("git -C one push -f origin feat/a && git -C two push -f origin feat/b"),
            out,
            false,
        );
        let arts: Vec<(&str, &str)> = got
            .iter()
            .map(|a| (a.artifact.as_str(), a.working_dir.as_str()))
            .collect();
        assert_eq!(arts, vec![("feat/a", "C:/r/one"), ("feat/b", "C:/r/two")]);
    }

    /// #13: with one block per push, a dry-run's block is attributed to the
    /// dry-run and ignored.
    #[test]
    fn a_dry_runs_forced_lines_are_never_reported() {
        let out = "To github.com:o/r.git\n + 1111111...2222222 main -> main (forced update)\nTo github.com:o/r.git\n + 3333333...4444444 feat/x -> feat/x (forced update)\n";
        let got = interpret_tool_use(
            &at("git push -n -f origin main && git push -f origin feat/x"),
            out,
            false,
        );
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].artifact, "feat/x");

        // Unattributable (one block for two pushes) + unconstrained real
        // push beside a dry-run: the output is excluded, the declaration is
        // reported instead.
        let out = "To github.com:o/r.git\n + 1111111...2222222 main -> main (forced update)\n";
        let got = interpret_tool_use(
            &at("git push -n -f origin main; git push -q -f"),
            out,
            false,
        );
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].undo, None, "no sha taken from the dry-run's table");
        assert_eq!(
            got[0].ref_target.as_ref().unwrap().dest,
            DestRef::PushDefault
        );
    }

    #[test]
    fn a_declared_destination_filters_the_reported_refs() {
        let out = "To github.com:o/r.git\n + aaaaaaa...bbbbbbb feat/a -> feat/a (forced update)\n + ccccccc...ddddddd other -> other (forced update)\n";
        let got = interpret_tool_use(&at("git push -f origin feat/a"), out, false);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].artifact, "feat/a");
        let out = "To github.com:o/r.git\n + aaaaaaa...bbbbbbb HEAD -> feat/z (forced update)\n";
        let got = interpret_tool_use(&at("git push origin +HEAD"), out, false);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].artifact, "feat/z");
    }

    #[test]
    fn positive_evidence_wins_over_is_error() {
        let out = "To github.com:o/r.git\n + aaaaaaa...bbbbbbb feat/a -> feat/a (forced update)\n ! [rejected]        main -> main (fetch first)\nerror: failed to push some refs\n";
        let got = interpret_tool_use(&at("git push -f origin feat/a main"), out, true);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].artifact, "feat/a");

        let npm = interpret_tool_use(
            &one(SensitiveCommand::NpmPublish { spec: None }, "C:/r"),
            "+ @q/ui@1.0.0\nnpm warn exit handler\n",
            true,
        );
        assert_eq!(npm.len(), 1);
        let cargo = interpret_tool_use(
            &one(SensitiveCommand::CargoPublish { package: None }, "C:/r"),
            "   Published q-x v0.1.0 at registry `crates-io`\nerror: something after\n",
            true,
        );
        assert_eq!(cargo.len(), 1);
        assert!(interpret_tool_use(
            &one(SensitiveCommand::NpmPublish { spec: None }, "C:/r"),
            "",
            true
        )
        .is_empty());
    }

    /// #9: a workspace publish reports every package, not the first.
    #[test]
    fn every_published_package_emits() {
        let npm = interpret_tool_use(
            &one(SensitiveCommand::NpmPublish { spec: None }, "C:/r"),
            "+ @q/a@1.0.0\n+ @q/b@2.0.0\n",
            false,
        );
        let arts: Vec<&str> = npm.iter().map(|a| a.artifact.as_str()).collect();
        assert_eq!(arts, vec!["@q/a@1.0.0", "@q/b@2.0.0"]);
        let cargo = interpret_tool_use(
            &one(SensitiveCommand::CargoPublish { package: None }, "C:/r"),
            "   Published a v0.1.0 at registry `crates-io`\n   Published b v0.2.0 at registry `crates-io`\n",
            false,
        );
        let arts: Vec<&str> = cargo.iter().map(|a| a.artifact.as_str()).collect();
        assert_eq!(arts, vec!["crate a v0.1.0", "crate b v0.2.0"]);
    }

    #[test]
    fn gh_release_create_reads_repo_from_the_release_url() {
        let got = interpret_tool_use(
            &at("gh release create v2.0.0 --generate-notes"),
            "https://github.com/qontinui/qontinui-web/releases/tag/v2.0.0\n",
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
        let got = interpret_tool_use(&at("gh release delete v1 --yes -R o/r"), "", false);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].action, SensitiveAction::Delete);
        assert_eq!(got[0].reversible, reversibility::ROLL_FORWARD);
        assert_eq!(got[0].repo.as_deref(), Some("o/r"));
    }

    #[test]
    fn registry_publishes_read_the_version_from_the_output() {
        let npm = interpret_tool_use(
            &one(SensitiveCommand::NpmPublish { spec: None }, "C:/r"),
            "npm notice Publishing to https://registry.npmjs.org/\n+ @qontinui/ui-bridge@0.27.0\n",
            false,
        );
        assert_eq!(npm.len(), 1);
        assert_eq!(npm[0].artifact, "@qontinui/ui-bridge@0.27.0");
        assert_eq!(npm[0].repo_from_remote, None);

        let cargo = interpret_tool_use(
            &one(SensitiveCommand::CargoPublish { package: None }, "C:/r"),
            "   Uploading qontinui-schemas v0.4.1 (C:/r)\n   Published qontinui-schemas v0.4.1 at registry `crates-io`\n",
            false,
        );
        assert_eq!(cargo.len(), 1);
        assert_eq!(cargo[0].artifact, "crate qontinui-schemas v0.4.1");

        let failed = interpret_tool_use(
            &one(SensitiveCommand::NpmPublish { spec: None }, "C:/r"),
            "npm error code E403\n",
            false,
        );
        assert!(failed.is_empty());
    }

    #[test]
    fn registry_fallback_artifacts_carry_no_local_path() {
        let npm = interpret_tool_use(
            &one(
                SensitiveCommand::NpmPublish { spec: None },
                "C:/Users/someone/work/ui-kit",
            ),
            "",
            false,
        );
        assert_eq!(npm[0].artifact, "npm package in ./ui-kit");
        let cargo = interpret_tool_use(
            &one(
                SensitiveCommand::CargoPublish { package: None },
                "C:\\Users\\someone\\work\\crate-x\\",
            ),
            "",
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
            push_dest: None,
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
            // (1) the default branch — and main/master ALWAYS.
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
                Some("main"),
                false,
                facts(Some("main"), Some("develop"), None),
                true,
                "main even when origin/HEAD says develop (stale)",
            ),
            (
                Some("master"),
                false,
                facts(Some("master"), Some("trunk"), None),
                true,
                "master even when origin/HEAD says otherwise",
            ),
            (
                Some("master"),
                false,
                facts(Some("master"), None, None),
                true,
                "default unknown",
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
                "current branch unknown / ownership mismatch",
            ),
            (
                None,
                false,
                facts(Some("feat/x"), Some("main"), None),
                true,
                "destination unresolved",
            ),
        ];
        for (dest, tags, f, want, why) in rows {
            assert_eq!(
                ref_action_is_notable(dest, tags, &f),
                want,
                "{why}: dest={dest:?} tags={tags} facts={f:?}"
            );
        }
    }

    #[test]
    fn destinations_resolve_from_git_facts() {
        let f = RefFacts {
            current_branch: Some("feat/x".into()),
            push_dest: Some("feat/up".into()),
            ..RefFacts::default()
        };
        assert_eq!(
            resolve_dest(&DestRef::Named("main".into()), &f).as_deref(),
            Some("main")
        );
        assert_eq!(
            resolve_dest(&DestRef::CurrentBranch, &f).as_deref(),
            Some("feat/x")
        );
        assert_eq!(
            resolve_dest(&DestRef::PushDefault, &f).as_deref(),
            Some("feat/up"),
            "push.default=upstream can push elsewhere than the branch name"
        );
        assert_eq!(resolve_dest(&DestRef::Unresolvable("$B".into()), &f), None);
        assert_eq!(
            resolve_dest(&DestRef::PushDefault, &RefFacts::default()),
            None
        );
    }

    // ── D8 end to end, against a real repository ─────────────────────────

    fn run_git(dir: &std::path::Path, args: &[&str]) {
        let ok = std::process::Command::new("git")
            .current_dir(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        assert!(ok, "git {args:?} failed in {}", dir.display());
    }

    /// A repo on branch `feat/x` whose `origin` is github.com/o/r.
    fn repo_on_feat_x() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path();
        run_git(d, &["init", "-q"]);
        run_git(d, &["commit", "-q", "--allow-empty", "-m", "x"]);
        run_git(d, &["checkout", "-q", "-b", "feat/x"]);
        run_git(
            d,
            &["remote", "add", "origin", "https://github.com/o/r.git"],
        );
        tmp
    }

    fn forced(dest: DestRef, url: &str, dir: &str) -> DetectedAction {
        DetectedAction {
            action: SensitiveAction::ForcePush,
            artifact: CURRENT_BRANCH_ARTIFACT.into(),
            reversible: reversibility::NO,
            undo: None,
            repo: None,
            repo_from_remote: None,
            working_dir: dir.into(),
            ref_target: Some(RefTarget {
                dest,
                remote: "origin".into(),
                remote_url: Some(url.into()),
                tags_or_mirror: false,
            }),
        }
    }

    #[test]
    fn finalize_suppresses_only_the_owned_working_branch() {
        let repo = repo_on_feat_x();
        let dir = repo.path().to_string_lossy().to_string();
        let own = "https://github.com/o/r.git";
        // Own branch, own repo → suppressed.
        assert!(
            finalize_detected_action(forced(DestRef::Named("feat/x".into()), own, &dir)).is_none()
        );
        // `HEAD` resolves to the current branch — suppressed, and named.
        assert!(finalize_detected_action(forced(DestRef::CurrentBranch, own, &dir)).is_none());
        // main always notifies.
        assert!(
            finalize_detected_action(forced(DestRef::Named("main".into()), own, &dir)).is_some()
        );
        // Same branch name, DIFFERENT repo (a cd the tracking missed, a fork
        // URL): ownership fails, so it notifies.
        assert!(finalize_detected_action(forced(
            DestRef::Named("feat/x".into()),
            "https://github.com/someone-else/r.git",
            &dir
        ))
        .is_some());
        // Unnamed push with no upstream: `@{push}` is unanswerable → notify.
        assert!(finalize_detected_action(forced(DestRef::PushDefault, own, &dir)).is_some());
        // Unresolvable (`$B`) → notify.
        assert!(
            finalize_detected_action(forced(DestRef::Unresolvable("$B".into()), own, &dir))
                .is_some()
        );
        // Unknown directory → notify.
        assert!(
            finalize_detected_action(forced(DestRef::Named("feat/x".into()), own, "")).is_some()
        );
    }

    #[test]
    fn a_quiet_head_push_is_named_after_the_current_branch() {
        let repo = repo_on_feat_x();
        let dir = repo.path().to_string_lossy().to_string();
        let mut a = forced(DestRef::CurrentBranch, "https://github.com/o/r.git", &dir);
        // Make it notable regardless of ownership, to observe the artifact.
        a.ref_target.as_mut().unwrap().tags_or_mirror = true;
        let got = finalize_detected_action(a).expect("tags always notify");
        assert_eq!(got.artifact, "feat/x");
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

    fn uid(tag: &str) -> String {
        format!("toolu_{tag}_{}", uuid::Uuid::new_v4().simple())
    }

    fn actions(got: &[DetectedToolUse]) -> Vec<&DetectedAction> {
        got.iter().flat_map(|d| d.actions.iter()).collect()
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
        assert_eq!(got[0].tool_use_id, id);
        let a = actions(&got);
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].action, SensitiveAction::ForcePush);
        assert_eq!(a[0].undo.as_deref(), Some("1a2b3c4"));
        assert_eq!(a[0].working_dir, "C:/work/qontinui-runner");
        assert_eq!(a[0].body()["repo"], json!("qontinui/qontinui-runner"));
        assert_eq!(t.pending_len(), 0, "the result resolves the entry");
    }

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
        assert_eq!(actions(&got).len(), 1, "one forced line → one notification");
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
        t.observe_line(
            &tool_use_line(&uid("other"), "git status"),
            t0 + PENDING_ACTION_TTL + Duration::from_secs(1),
        );
        assert_eq!(t.pending_len(), 0, "expired");
        assert_eq!(t.expired(), 1);
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

    /// The emitted-id record is written by a SUCCESSFUL dispatch
    /// ([`dispatch_detected_tool_use`]); once it is, a re-read transcript
    /// reports nothing.
    #[test]
    fn a_reread_transcript_does_not_notify_twice() {
        let id = uid("reread");
        let mut t = SensitiveActionTracker::new();
        let t0 = Instant::now();
        let use_line = tool_use_line(&id, "git push --force");
        let result = tool_result_line(&id, false, FORCED_OUTPUT);
        t.observe_line(&use_line, t0);
        assert_eq!(t.observe_line(&result, t0).len(), 1);
        record_emission(&id); // what an accepted dispatch does
        t.observe_line(&use_line, t0);
        assert!(t.observe_line(&result, t0).is_empty());
    }

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
        record_emission(&id);
        assert!(b.observe_line(&result, t0).is_empty());
    }

    /// #5: an id is recorded only after an ACCEPTED dispatch — if the first
    /// transcript's dispatch was dropped (nothing recorded), the second
    /// transcript still reports it.
    #[test]
    fn a_dropped_dispatch_leaves_the_id_free_for_another_transcript() {
        let id = uid("dropped");
        let t0 = Instant::now();
        let use_line = tool_use_line(&id, "git push --force");
        let result = tool_result_line(&id, false, FORCED_OUTPUT);
        let mut a = SensitiveActionTracker::new();
        let mut b = SensitiveActionTracker::new();
        a.observe_line(&use_line, t0);
        b.observe_line(&use_line, t0);
        assert_eq!(a.observe_line(&result, t0).len(), 1);
        // No record_emission: the dispatch was refused.
        assert!(!emission_recorded(&id));
        assert_eq!(b.observe_line(&result, t0).len(), 1);
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

    /// #11: without `toolUseResult`, the result text is the only witness.
    #[test]
    fn a_background_command_without_tool_use_result_is_not_reported() {
        let id = uid("bgtext");
        let mut t = SensitiveActionTracker::new();
        let t0 = Instant::now();
        t.observe_line(&tool_use_line(&id, "git push --force"), t0);
        let line = json!({
            "type": "user",
            "message": {"content": [
                {"type": "tool_result", "tool_use_id": id, "is_error": false,
                 "content": "Command running in background with ID: b999. Output is being written to: x"},
            ]},
        })
        .to_string();
        assert!(t.observe_line(&line, t0).is_empty());
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
        let a = actions(&got);
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].undo.as_deref(), Some("1a2b3c4"));
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

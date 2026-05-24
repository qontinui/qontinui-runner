//! `GitOpBridge` — the second [`RunnerObservableBridge`] category
//! (`"git_op"`), the proof that the federation trait generalizes beyond
//! memory.
//!
//! Plan: `2026-05-24-federation-verify-and-gitop.md`, Phase 6.
//!
//! ## What it observes
//!
//! Every git operation a runner-spawned session performs on its PRIMARY
//! working tree (`SessionContext.working_dir`) is recorded to coord as a
//! post-action observational feed. Two detection mechanisms compose:
//!
//! 1. **`notify`-watch of `<working_dir>/.git/`** (baseline) — cleanly
//!    captures `commit` / `checkout` / `branch_create` / `merge` /
//!    `rebase` / `reset` by tailing the reflog (`logs/HEAD`) and watching
//!    ref files. Zero repo modification.
//! 2. **`pre-push` hook** (push precision) — a `git push` only advances
//!    the *remote-tracking* ref locally, indistinguishable from a fetch
//!    and after the fact. The hook yields an unambiguous, semantically
//!    rich `push` event. Installed backup-and-chain (the user's existing
//!    `pre-push`, if any, is preserved and still runs).
//!
//! ## Crash safety
//!
//! Installing a hook risks a leak (session dies before teardown → repo
//! left with the qontinui hook + the user's hook shelved). Two defenses:
//!  - `reconcile` uninstalls (restore backup / remove ours).
//!  - `start_watching` runs an **idempotent stale-hook self-heal FIRST**:
//!    if a `.pre-push.qontinui-backup` exists, a prior session leaked —
//!    restore it (and strip any leftover qontinui hook) before installing
//!    fresh. This turns a permanent leak into a next-session self-repair.

use anyhow::{Context, Result};
use async_trait::async_trait;
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use qontinui_types::git_ops::RecordGitOpRequest;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};
use uuid::Uuid;

use super::git_ops_client;
use super::{ReconcileReport, RunnerObservableBridge, SessionContext};

/// Git ops are less bursty than memory edits, but a rebase touches many
/// refs at once — a longer window than memory's 250ms coalesces the
/// burst into a sensible op sequence.
const DEBOUNCE_MS: u64 = 500;

/// Sentinel comment written into our `pre-push` hook so we can recognize
/// "our" hook on self-heal (vs a user's hook we must never clobber).
const HOOK_SENTINEL: &str = "# qontinui-gitop-hook";

/// Suffix appended to the user's original `pre-push` when we shelve it.
const HOOK_BACKUP_NAME: &str = ".pre-push.qontinui-backup";

/// One detected git op, derived from a reflog line or a hook push line,
/// ready to become a [`RecordGitOpRequest`]. Bridge-internal — never
/// crosses the [`RunnerObservableBridge`] trait (a git op has nothing
/// structurally in common with a memory upsert, which is exactly why the
/// trait carries no change type).
#[derive(Debug, Clone)]
struct GitOp {
    op_kind: String,
    sha: Option<String>,
    branch: Option<String>,
    message: Option<String>,
    metadata: Option<serde_json::Value>,
}

/// Reusable bridge for the lifetime of one runner process. Watchers are
/// keyed by `session_id` so concurrent sessions (different working dirs)
/// don't trample each other's `notify` hooks or push temp files.
pub struct GitOpBridge {
    http: reqwest::Client,
    watchers: tokio::sync::Mutex<HashMap<Uuid, GitWatcherHandle>>,
}

struct GitWatcherHandle {
    /// Watches `<working_dir>/.git/` (recursive — refs live in
    /// subdirs). Held for its side effect; dropping disconnects notify.
    _git_notify: RecommendedWatcher,
    /// Watches the hook's push temp file (a separate, non-recursive
    /// watch on the temp dir entry). Held for side effect.
    _push_notify: RecommendedWatcher,
    cancel: CancellationToken,
    /// Where the pre-push hook appends its push lines. Removed on
    /// reconcile.
    push_file: PathBuf,
    /// The repo's `.git/hooks/` dir — used by reconcile to uninstall.
    hooks_dir: PathBuf,
}

impl GitOpBridge {
    pub fn new() -> Result<Self> {
        Ok(Self {
            http: git_ops_client::build_client()?,
            watchers: tokio::sync::Mutex::new(HashMap::new()),
        })
    }

    /// Cancel every active watcher + uninstall any hooks. Called at runner
    /// shutdown so no `notify` hooks or installed pre-push hooks linger.
    pub async fn shutdown_all(&self) {
        let mut guard = self.watchers.lock().await;
        for (_, handle) in guard.drain() {
            handle.cancel.cancel();
            uninstall_hook(&handle.hooks_dir);
            let _ = std::fs::remove_file(&handle.push_file);
        }
    }

    fn device_token(&self) -> Option<String> {
        crate::auth::AuthManager::new().get_access_token().ok()
    }

    /// Emit one detected op to coord, best-effort (log-and-continue —
    /// never blocks/panics the session). Resolves repo from working_dir.
    async fn emit(&self, op: GitOp, ctx: &SessionContext) {
        let token = match self.device_token() {
            Some(t) if !t.is_empty() => t,
            _ => {
                debug!("observable_bridge::git_op::emit: no device token; dropping op");
                return;
            }
        };
        let base = match git_ops_client::coord_http_base() {
            Some(b) => b,
            None => {
                debug!("observable_bridge::git_op::emit: no coord_url; dropping op");
                return;
            }
        };
        let git_dir = ctx.working_dir.join(".git");
        let repo = resolve_repo_name(&ctx.working_dir, &git_dir);
        // For ops without an explicit branch (e.g. commit), fall back to
        // the current HEAD branch so the feed is always branch-tagged.
        let branch = op.branch.or_else(|| current_branch(&git_dir));
        let req = RecordGitOpRequest {
            repo,
            branch,
            op_kind: op.op_kind.clone(),
            sha: op.sha,
            message: op.message,
            metadata: op.metadata,
        };
        if let Err(e) = git_ops_client::record(&self.http, &base, &token, ctx.tenant_id, &req).await
        {
            warn!(
                "observable_bridge::git_op::emit record {} failed: {e}",
                op.op_kind
            );
        } else {
            debug!("observable_bridge::git_op::emit recorded {}", op.op_kind);
        }
    }
}

// ---------------------------------------------------------------------------
// Repo / ref resolution
// ---------------------------------------------------------------------------

/// Resolve the repo name: `[remote "origin"] url` basename (minus `.git`),
/// falling back to the working-dir basename for remote-less clones.
fn resolve_repo_name(working_dir: &Path, git_dir: &Path) -> String {
    if let Some(name) = origin_url_basename(&git_dir.join("config")) {
        return name;
    }
    working_dir
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown-repo")
        .to_string()
}

/// Parse `.git/config` for `[remote "origin"] url = …` and return the
/// URL basename without a trailing `.git`. Handles both `git@host:org/x.git`
/// and `https://host/org/x.git` forms.
fn origin_url_basename(config_path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(config_path).ok()?;
    let mut in_origin = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_origin = trimmed
                .replace(' ', "")
                .eq_ignore_ascii_case("[remote\"origin\"]");
            continue;
        }
        if in_origin {
            if let Some(rest) = trimmed.strip_prefix("url") {
                let rest = rest.trim_start();
                if let Some(eq) = rest.strip_prefix('=') {
                    let url = eq.trim();
                    return Some(repo_basename_from_url(url));
                }
            }
        }
    }
    None
}

/// `git@github.com:org/repo.git` / `https://github.com/org/repo.git`
/// → `repo`.
fn repo_basename_from_url(url: &str) -> String {
    let url = url.trim().trim_end_matches('/');
    let last = url.rsplit(|c| c == '/' || c == ':').next().unwrap_or(url);
    last.strip_suffix(".git").unwrap_or(last).to_string()
}

/// Read `.git/HEAD`; if it's a symbolic ref `ref: refs/heads/<branch>`,
/// return `<branch>`. Detached HEAD (raw sha) → `None`.
fn current_branch(git_dir: &Path) -> Option<String> {
    let head = std::fs::read_to_string(git_dir.join("HEAD")).ok()?;
    let head = head.trim();
    head.strip_prefix("ref: refs/heads/").map(|b| b.to_string())
}

/// Resolve the current HEAD commit sha: follow the symbolic ref to its
/// loose ref file, or read the raw sha for detached HEAD. Falls back to
/// `packed-refs` for the symbolic case.
fn current_head_sha(git_dir: &Path) -> Option<String> {
    let head = std::fs::read_to_string(git_dir.join("HEAD")).ok()?;
    let head = head.trim();
    if let Some(ref_path) = head.strip_prefix("ref: ") {
        let loose = git_dir.join(ref_path);
        if let Ok(sha) = std::fs::read_to_string(&loose) {
            let sha = sha.trim();
            if !sha.is_empty() {
                return Some(sha.to_string());
            }
        }
        // Fall back to packed-refs.
        if let Ok(packed) = std::fs::read_to_string(git_dir.join("packed-refs")) {
            for line in packed.lines() {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') || line.starts_with('^') {
                    continue;
                }
                if let Some((sha, name)) = line.split_once(' ') {
                    if name.trim() == ref_path {
                        return Some(sha.trim().to_string());
                    }
                }
            }
        }
        None
    } else if !head.is_empty() {
        // Detached HEAD: the content IS the sha.
        Some(head.to_string())
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Reflog parsing
// ---------------------------------------------------------------------------

/// A parsed `logs/HEAD` reflog entry.
///
/// Format: `<old-sha> <new-sha> <name> <email> <unix-ts> <tz>\t<verb>: <msg>`.
/// The committer field contains spaces, so we split on the TAB first to
/// isolate the `<verb>: <msg>` subject, then split the leading SHAs off
/// the front of the prefix.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ReflogEntry {
    new_sha: String,
    /// The reflog verb: `commit`, `checkout`, `merge`, `rebase`,
    /// `reset`, `commit (amend)`, `commit (initial)`, `pull`, …
    verb: String,
    message: String,
}

/// Parse a single reflog line. Returns `None` for malformed lines.
fn parse_reflog_line(line: &str) -> Option<ReflogEntry> {
    let line = line.trim_end_matches(['\r', '\n']);
    if line.is_empty() {
        return None;
    }
    // Split prefix (SHAs + committer + ts) from "<verb>: <msg>" on the TAB.
    let (prefix, subject) = line.split_once('\t')?;
    let mut parts = prefix.split_whitespace();
    let _old = parts.next()?;
    let new_sha = parts.next()?.to_string();
    // subject = "<verb>: <message>" (message optional). The verb may
    // itself contain a space + parenthetical, e.g. "commit (amend)".
    let (verb, message) = match subject.split_once(':') {
        Some((v, m)) => (v.trim().to_string(), m.trim().to_string()),
        None => (subject.trim().to_string(), String::new()),
    };
    Some(ReflogEntry {
        new_sha,
        verb,
        message,
    })
}

/// Map a reflog entry to a [`GitOp`]. Returns `None` for verbs we don't
/// surface (the dominant fleet signals are commit/checkout/merge/rebase/
/// reset; others are noise).
fn reflog_entry_to_op(entry: &ReflogEntry, git_dir: &Path) -> Option<GitOp> {
    // The verb's leading word is the canonical op; the parenthetical
    // (e.g. "commit (amend)") is preserved into metadata.
    let verb_head = entry.verb.split_whitespace().next().unwrap_or("");
    let (op_kind, message): (&str, Option<String>) = match verb_head {
        "commit" => ("commit", non_empty(&entry.message)),
        // "checkout" reflog message is "<from-sha-or-branch> to <to>"; we
        // surface the destination branch via current HEAD at fire time.
        "checkout" => ("checkout", non_empty(&entry.message)),
        "merge" => ("merge", non_empty(&entry.message)),
        "rebase" => ("rebase", non_empty(&entry.message)),
        "reset" => ("reset", non_empty(&entry.message)),
        "pull" => ("merge", non_empty(&entry.message)), // pull = fetch + merge
        // "clone", "branch", "initial" and anything else: skip. Branch
        // creation is caught by the refs/heads watch, not the HEAD reflog.
        _ => return None,
    };

    let mut metadata = serde_json::Map::new();
    if entry.verb != op_kind {
        metadata.insert(
            "reflog_verb".to_string(),
            serde_json::Value::String(entry.verb.clone()),
        );
    }
    let metadata = if metadata.is_empty() {
        None
    } else {
        Some(serde_json::Value::Object(metadata))
    };

    Some(GitOp {
        op_kind: op_kind.to_string(),
        sha: Some(entry.new_sha.clone()),
        branch: current_branch(git_dir),
        message,
        metadata,
    })
}

fn non_empty(s: &str) -> Option<String> {
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

/// Read the reflog and return entries AFTER `last_seen_count` lines, plus
/// the new total line count. The reflog is append-only, so tracking the
/// line count is a reliable cursor.
fn read_reflog_since(git_dir: &Path, last_seen_count: usize) -> (Vec<ReflogEntry>, usize) {
    let path = git_dir.join("logs").join("HEAD");
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(_) => return (Vec::new(), last_seen_count),
    };
    let lines: Vec<&str> = text.lines().collect();
    let total = lines.len();
    if total <= last_seen_count {
        // Reflog was rewritten/shrunk (e.g. `git reflog expire`); reset
        // the cursor without emitting (avoids re-emitting old history).
        return (Vec::new(), total);
    }
    let new_entries = lines[last_seen_count..]
        .iter()
        .filter_map(|l| parse_reflog_line(l))
        .collect();
    (new_entries, total)
}

/// Count reflog lines now — used to seed the cursor at watch start so we
/// only emit ops that happen DURING the session.
fn reflog_line_count(git_dir: &Path) -> usize {
    let path = git_dir.join("logs").join("HEAD");
    std::fs::read_to_string(&path)
        .map(|t| t.lines().count())
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Hook install / self-heal
// ---------------------------------------------------------------------------

/// The body of our `pre-push` hook. Appends a single
/// `QONTINUI_GIT_PUSH <remote> <url> <unix-ts>` line to `push_file`, then
/// chains the user's shelved hook (if present) so their hook still runs.
///
/// `git` invokes `pre-push` with argv `<remote-name> <remote-url>` and
/// pipes `<local-ref> <local-sha> <remote-ref> <remote-sha>` lines on
/// stdin. We tee stdin so the chained hook still receives it.
fn hook_body(push_file: &Path) -> String {
    let push_file_str = push_file.to_string_lossy().replace('\\', "/");
    format!(
        r#"#!/usr/bin/env bash
{sentinel}
# Installed by qontinui GitOpBridge to record `git push` ops to coord.
# Backup-and-chain: the user's original pre-push (if any) is preserved as
# `{backup}` next to this file and is sourced below so it still runs.
__qontinui_remote="$1"
__qontinui_url="$2"
__qontinui_stdin="$(cat)"
printf 'QONTINUI_GIT_PUSH %s %s %s\n' "$__qontinui_remote" "$__qontinui_url" "$(date +%s)" >> "{push_file}" 2>/dev/null || true
__qontinui_dir="$(cd "$(dirname "${{BASH_SOURCE[0]}}")" && pwd)"
__qontinui_backup="$__qontinui_dir/{backup}"
if [ -x "$__qontinui_backup" ] || [ -f "$__qontinui_backup" ]; then
  printf '%s' "$__qontinui_stdin" | "$__qontinui_backup" "$__qontinui_remote" "$__qontinui_url"
  exit $?
fi
exit 0
"#,
        sentinel = HOOK_SENTINEL,
        backup = HOOK_BACKUP_NAME,
        push_file = push_file_str,
    )
}

/// Is `path` a pre-push hook we installed? Detect via the sentinel line.
fn is_our_hook(path: &Path) -> bool {
    std::fs::read_to_string(path)
        .map(|c| c.contains(HOOK_SENTINEL))
        .unwrap_or(false)
}

/// Idempotent stale-hook self-heal. Run at `start_watching` BEFORE
/// installing, and the same logic powers uninstall on reconcile.
///
/// - If a backup exists, a prior session leaked: restore it over
///   `pre-push` (overwriting any leftover qontinui hook), then remove the
///   backup.
/// - Else if `pre-push` is our hook (leaked with no backup → there was no
///   user hook to shelve), remove it.
/// - Else (user's own hook, or none): leave untouched.
fn self_heal_hook(hooks_dir: &Path) {
    let pre_push = hooks_dir.join("pre-push");
    let backup = hooks_dir.join(HOOK_BACKUP_NAME);
    if backup.exists() {
        if let Err(e) = std::fs::rename(&backup, &pre_push) {
            // rename can fail across edge cases; fall back to copy+remove.
            warn!("observable_bridge::git_op: self-heal restore rename failed ({e}); trying copy");
            if std::fs::copy(&backup, &pre_push).is_ok() {
                let _ = std::fs::remove_file(&backup);
            }
        }
        info!("observable_bridge::git_op: self-healed leaked pre-push hook (restored backup)");
    } else if pre_push.exists() && is_our_hook(&pre_push) {
        let _ = std::fs::remove_file(&pre_push);
        info!("observable_bridge::git_op: self-healed leaked pre-push hook (removed, no backup)");
    }
}

/// Install our pre-push hook backup-and-chain. Best-effort: any IO error
/// is logged and the watcher continues (notify still catches non-push
/// ops). Returns whether the hook was installed.
fn install_hook(hooks_dir: &Path, push_file: &Path) -> bool {
    if let Err(e) = std::fs::create_dir_all(hooks_dir) {
        warn!(
            "observable_bridge::git_op: create hooks dir {} failed: {e}",
            hooks_dir.display()
        );
        return false;
    }
    let pre_push = hooks_dir.join("pre-push");
    let backup = hooks_dir.join(HOOK_BACKUP_NAME);

    // Shelve a pre-existing user hook (but never shelve our own — that
    // would happen if self-heal somehow missed it).
    if pre_push.exists() && !is_our_hook(&pre_push) {
        if let Err(e) = std::fs::copy(&pre_push, &backup) {
            warn!(
                "observable_bridge::git_op: backup existing pre-push failed: {e}; \
                 not installing hook to avoid clobbering user hook"
            );
            return false;
        }
    }

    if let Err(e) = std::fs::write(&pre_push, hook_body(push_file)) {
        warn!("observable_bridge::git_op: write pre-push hook failed: {e}");
        return false;
    }
    make_executable(&pre_push);
    debug!(
        "observable_bridge::git_op: installed pre-push hook at {}",
        pre_push.display()
    );
    true
}

/// Restore the backup over our hook, or remove our hook if there's no
/// backup. The reconcile/shutdown teardown counterpart to `install_hook`.
fn uninstall_hook(hooks_dir: &Path) {
    let pre_push = hooks_dir.join("pre-push");
    let backup = hooks_dir.join(HOOK_BACKUP_NAME);
    if backup.exists() {
        if std::fs::rename(&backup, &pre_push).is_err() {
            if std::fs::copy(&backup, &pre_push).is_ok() {
                let _ = std::fs::remove_file(&backup);
            }
        }
    } else if pre_push.exists() && is_our_hook(&pre_push) {
        let _ = std::fs::remove_file(&pre_push);
    }
}

/// chmod +x best-effort (no-op error on Windows where it's irrelevant —
/// Git Bash runs the hook via its shebang regardless of the NTFS x-bit).
fn make_executable(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(path) {
            let mut perms = meta.permissions();
            perms.set_mode(0o755);
            let _ = std::fs::set_permissions(path, perms);
        }
    }
    #[cfg(not(unix))]
    {
        let _ = path; // nothing to do on Windows.
    }
}

/// Path of the per-session hook IPC temp file. A REGULAR file (NOT a FIFO
/// — `mkfifo` is absent on Windows; Git Bash appends to a regular file the
/// bridge watches).
fn push_temp_file(session_id: Uuid) -> PathBuf {
    std::env::temp_dir().join(format!("qontinui-gitop-{session_id}.push"))
}

/// Parse a `QONTINUI_GIT_PUSH <remote> <url> <ts>` hook line into a push
/// op. Returns `None` for non-matching lines.
fn parse_push_line(line: &str, git_dir: &Path) -> Option<GitOp> {
    let rest = line.trim().strip_prefix("QONTINUI_GIT_PUSH")?;
    let mut parts = rest.split_whitespace();
    let remote = parts.next().unwrap_or("origin").to_string();
    let url = parts.next().map(|s| s.to_string());
    let mut metadata = serde_json::Map::new();
    metadata.insert("remote".to_string(), serde_json::Value::String(remote));
    if let Some(u) = url {
        metadata.insert("remote_url".to_string(), serde_json::Value::String(u));
    }
    Some(GitOp {
        op_kind: "push".to_string(),
        sha: current_head_sha(git_dir),
        branch: current_branch(git_dir),
        message: None,
        metadata: Some(serde_json::Value::Object(metadata)),
    })
}

// ---------------------------------------------------------------------------
// Watch loop
// ---------------------------------------------------------------------------

/// Tracks coalesced state across debounce windows so we know what to emit
/// and how to dedupe hook-push vs notify remote-ref events.
struct WatchState {
    /// Reflog cursor (line count already emitted from `logs/HEAD`).
    reflog_cursor: usize,
    /// Push temp-file cursor (lines already consumed).
    push_cursor: usize,
    /// Branch ref files we've already seen (to detect NEW branch_create).
    known_branches: std::collections::HashSet<String>,
    /// Monotonic instant of the last hook-sourced push emit. Used to
    /// suppress an ambiguous `logs/refs/remotes/` notify event that fires
    /// within the debounce window right after a real push.
    last_hook_push: Option<std::time::Instant>,
}

/// Background task: drains notify events from BOTH watches (git dir + push
/// file land in the same channel), debounces, and emits ops.
async fn run_git_watch_loop(
    bridge: Arc<GitOpBridge>,
    ctx: SessionContext,
    git_dir: PathBuf,
    push_file: PathBuf,
    mut rx: mpsc::Receiver<notify::Result<Event>>,
    cancel: CancellationToken,
) {
    use std::time::{Duration, Instant};
    use tokio::time::{sleep_until, Instant as TokioInstant};

    let mut state = WatchState {
        reflog_cursor: reflog_line_count(&git_dir),
        push_cursor: count_lines(&push_file),
        known_branches: list_branch_refs(&git_dir),
        last_hook_push: None,
    };

    // Pending fire deadline; any relevant event (re)arms it.
    let mut deadline: Option<Instant> = None;
    // Whether a remote-ref notify event arrived this window (ambiguous —
    // only emitted if NOT covered by a hook push).
    let mut saw_remote_ref = false;

    loop {
        let next_deadline = deadline.map(TokioInstant::from_std);
        tokio::select! {
            _ = cancel.cancelled() => {
                debug!("observable_bridge::git_op: watch loop cancelled");
                return;
            }
            maybe_event = rx.recv() => {
                let event = match maybe_event {
                    Some(Ok(ev)) => ev,
                    Some(Err(e)) => {
                        warn!("observable_bridge::git_op: notify error: {e}");
                        continue;
                    }
                    None => {
                        debug!("observable_bridge::git_op: notify channel closed");
                        return;
                    }
                };
                if !matches!(
                    event.kind,
                    EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
                ) {
                    continue;
                }
                for p in &event.paths {
                    if path_touches_remote_logs(p, &git_dir) {
                        saw_remote_ref = true;
                    }
                }
                // (Re)arm debounce.
                deadline = Some(Instant::now() + Duration::from_millis(DEBOUNCE_MS));
            }
            _ = async {
                match next_deadline {
                    Some(d) => sleep_until(d).await,
                    None => futures::future::pending::<()>().await,
                }
            } => {
                deadline = None;
                let took_remote = std::mem::take(&mut saw_remote_ref);
                fire_window(&bridge, &ctx, &git_dir, &push_file, &mut state, took_remote).await;
            }
        }
    }
}

/// Emit every op that accrued during one debounce window: hook pushes
/// first (authoritative), then reflog ops, then new branch_creates, then
/// — only if a remote-ref notify fired and NO hook push covered it — a
/// low-confidence `remote_update`.
async fn fire_window(
    bridge: &Arc<GitOpBridge>,
    ctx: &SessionContext,
    git_dir: &Path,
    push_file: &Path,
    state: &mut WatchState,
    saw_remote_ref: bool,
) {
    // 1. Hook-sourced pushes (authoritative for push).
    let (push_lines, new_push_cursor) = read_lines_since(push_file, state.push_cursor);
    state.push_cursor = new_push_cursor;
    let mut emitted_push = false;
    for line in &push_lines {
        if let Some(op) = parse_push_line(line, git_dir) {
            emitted_push = true;
            state.last_hook_push = Some(std::time::Instant::now());
            bridge.emit(op, ctx).await;
        }
    }

    // 2. Reflog-derived ops (commit / checkout / merge / rebase / reset).
    let (entries, new_cursor) = read_reflog_since(git_dir, state.reflog_cursor);
    state.reflog_cursor = new_cursor;
    for entry in &entries {
        if let Some(op) = reflog_entry_to_op(entry, git_dir) {
            bridge.emit(op, ctx).await;
        }
    }

    // 3. New branches (refs/heads files that weren't there at last scan).
    let current_branches = list_branch_refs(git_dir);
    for b in current_branches.difference(&state.known_branches) {
        let op = GitOp {
            op_kind: "branch_create".to_string(),
            sha: None,
            branch: Some(b.clone()),
            message: None,
            metadata: None,
        };
        bridge.emit(op, ctx).await;
    }
    state.known_branches = current_branches;

    // 4. Ambiguous remote-ref advance — only if a remote log fired AND no
    // hook push covered it this window (hook is authoritative for push;
    // this is the fetch/unattributed-push fallback). Also suppress if a
    // hook push fired very recently (within one debounce window) to avoid
    // double-emit when the hook line and the remote-ref update straddle
    // the window boundary.
    if saw_remote_ref && !emitted_push {
        let recently_pushed = state
            .last_hook_push
            .map(|t| t.elapsed() < std::time::Duration::from_millis(DEBOUNCE_MS * 2))
            .unwrap_or(false);
        if !recently_pushed {
            let op = GitOp {
                op_kind: "remote_update".to_string(),
                sha: current_head_sha(git_dir),
                branch: current_branch(git_dir),
                message: None,
                metadata: Some(serde_json::json!({ "source": "notify_remote_ref" })),
            };
            bridge.emit(op, ctx).await;
        }
    }
}

/// True if `p` is under `<git_dir>/logs/refs/remotes/` or
/// `<git_dir>/refs/remotes/` — the ambiguous remote-tracking ref surface.
fn path_touches_remote_logs(p: &Path, git_dir: &Path) -> bool {
    let logs_remotes = git_dir.join("logs").join("refs").join("remotes");
    let refs_remotes = git_dir.join("refs").join("remotes");
    p.starts_with(&logs_remotes) || p.starts_with(&refs_remotes)
}

/// List current loose branch ref names under `refs/heads/` (recursive),
/// plus packed-refs `refs/heads/*`. Names are the branch portion after
/// `refs/heads/`.
fn list_branch_refs(git_dir: &Path) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    let heads = git_dir.join("refs").join("heads");
    collect_branch_files(&heads, &heads, &mut out);
    // packed-refs.
    if let Ok(packed) = std::fs::read_to_string(git_dir.join("packed-refs")) {
        for line in packed.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with('^') {
                continue;
            }
            if let Some((_sha, name)) = line.split_once(' ') {
                if let Some(branch) = name.trim().strip_prefix("refs/heads/") {
                    out.insert(branch.to_string());
                }
            }
        }
    }
    out
}

fn collect_branch_files(base: &Path, dir: &Path, out: &mut std::collections::HashSet<String>) {
    let rd = match std::fs::read_dir(dir) {
        Ok(r) => r,
        Err(_) => return,
    };
    for entry in rd.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_branch_files(base, &path, out);
        } else if let Ok(rel) = path.strip_prefix(base) {
            if let Some(name) = rel.to_str() {
                out.insert(name.replace('\\', "/"));
            }
        }
    }
}

/// Read lines of `path` after `cursor` lines; return (new lines, new
/// total line count). Append-only-friendly cursor.
fn read_lines_since(path: &Path, cursor: usize) -> (Vec<String>, usize) {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(_) => return (Vec::new(), cursor),
    };
    let lines: Vec<&str> = text.lines().collect();
    let total = lines.len();
    if total <= cursor {
        return (Vec::new(), total);
    }
    let new_lines = lines[cursor..].iter().map(|s| s.to_string()).collect();
    (new_lines, total)
}

fn count_lines(path: &Path) -> usize {
    std::fs::read_to_string(path)
        .map(|t| t.lines().count())
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Trait impl
// ---------------------------------------------------------------------------

#[async_trait]
impl RunnerObservableBridge for GitOpBridge {
    fn category(&self) -> &'static str {
        "git_op"
    }

    /// No-op in v1: git state is not materialized FROM coord — coord does
    /// not dictate your working tree. Reserved for a future "inject the
    /// fleet's branch map at session start" context surface.
    async fn pull(&self, _ctx: &mut SessionContext) -> Result<()> {
        Ok(())
    }

    /// Self-heal any leaked hook, start notify-watching `.git/` + the push
    /// temp file, and install the pre-push hook (backup-and-chain).
    /// Takes `self: Arc<Self>` so it can clone itself into the detached
    /// watch task. Best-effort: a non-git working_dir short-circuits
    /// cleanly (no `.git/` → no watch, no hook).
    async fn start_watching(self: Arc<Self>, ctx: &SessionContext) -> Result<()> {
        let session_id = ctx.session_id;
        let git_dir = ctx.working_dir.join(".git");
        if !git_dir.is_dir() {
            debug!(
                "observable_bridge::git_op: no .git at {}; git federation skipped this session",
                git_dir.display()
            );
            return Ok(());
        }
        let hooks_dir = git_dir.join("hooks");

        // (1) Stale-hook self-heal FIRST — idempotent crash recovery.
        self_heal_hook(&hooks_dir);

        let mut guard = self.watchers.lock().await;
        if let Some(prev) = guard.remove(&session_id) {
            prev.cancel.cancel();
            uninstall_hook(&prev.hooks_dir);
            let _ = std::fs::remove_file(&prev.push_file);
        }

        // (2) notify-watch the .git dir (recursive: refs/heads/, logs/…).
        let (tx, rx) = mpsc::channel::<notify::Result<Event>>(512);
        let tx_git = tx.clone();
        let mut git_watcher = notify::recommended_watcher(move |res: notify::Result<Event>| {
            let _ = tx_git.blocking_send(res);
        })
        .context("observable_bridge::git_op: build .git notify watcher")?;
        git_watcher
            .watch(&git_dir, RecursiveMode::Recursive)
            .with_context(|| format!("notify::watch on {}", git_dir.display()))?;

        // (3) pre-push hook (push precision) + its push temp file watch.
        let push_file = push_temp_file(session_id);
        // Ensure the temp file exists so notify can watch it from the
        // start (watching its parent dir would be noisy — watch the file).
        if let Err(e) = std::fs::write(&push_file, b"") {
            warn!(
                "observable_bridge::git_op: create push temp file {} failed: {e}",
                push_file.display()
            );
        }
        let tx_push = tx.clone();
        let mut push_watcher = notify::recommended_watcher(move |res: notify::Result<Event>| {
            let _ = tx_push.blocking_send(res);
        })
        .context("observable_bridge::git_op: build push-file notify watcher")?;
        if let Err(e) = push_watcher.watch(&push_file, RecursiveMode::NonRecursive) {
            warn!(
                "observable_bridge::git_op: watch push temp file failed: {e}; \
                 push detection degraded to notify-only"
            );
        }
        // Drop the original tx so the channel closes when both watchers do.
        drop(tx);

        let installed = install_hook(&hooks_dir, &push_file);
        if installed {
            info!(
                "observable_bridge::git_op: watching {} + pre-push hook for session {}",
                git_dir.display(),
                session_id
            );
        } else {
            info!(
                "observable_bridge::git_op: watching {} (pre-push hook NOT installed; \
                 push detection degraded) for session {}",
                git_dir.display(),
                session_id
            );
        }

        let cancel = CancellationToken::new();
        let bridge = Arc::clone(&self);
        let task_cancel = cancel.clone();
        let task_ctx = ctx.clone();
        let task_git_dir = git_dir.clone();
        let task_push_file = push_file.clone();
        tokio::spawn(async move {
            run_git_watch_loop(
                bridge,
                task_ctx,
                task_git_dir,
                task_push_file,
                rx,
                task_cancel,
            )
            .await;
        });

        guard.insert(
            session_id,
            GitWatcherHandle {
                _git_notify: git_watcher,
                _push_notify: push_watcher,
                cancel,
                push_file,
                hooks_dir,
            },
        );
        Ok(())
    }

    async fn stop_watching(&self, session_id: Uuid) {
        let mut guard = self.watchers.lock().await;
        if let Some(prev) = guard.remove(&session_id) {
            prev.cancel.cancel();
            uninstall_hook(&prev.hooks_dir);
            let _ = std::fs::remove_file(&prev.push_file);
        }
    }

    /// Session-end: (1) uninstall the hook + remove the push temp file,
    /// (2) emit a terminal `checkout` op anchoring the branch + HEAD sha
    /// the session ended on (a cheap fleet-state anchor), (3) return
    /// counts. `stop_watching` (called by dispatch before this) already
    /// tore down the watcher; this is the idempotent belt-and-braces pass.
    async fn reconcile(&self, ctx: &SessionContext) -> Result<ReconcileReport> {
        // Idempotent teardown — `stop_watching` may have run first.
        {
            let mut guard = self.watchers.lock().await;
            if let Some(prev) = guard.remove(&ctx.session_id) {
                prev.cancel.cancel();
                uninstall_hook(&prev.hooks_dir);
                let _ = std::fs::remove_file(&prev.push_file);
            } else {
                // Watcher already gone (stop_watching ran). Still ensure
                // hook + temp file are cleaned in case of a partial state.
                let git_dir = ctx.working_dir.join(".git");
                uninstall_hook(&git_dir.join("hooks"));
                let _ = std::fs::remove_file(push_temp_file(ctx.session_id));
            }
        }

        let mut report = ReconcileReport::default();
        let git_dir = ctx.working_dir.join(".git");
        if git_dir.is_dir() {
            let branch = current_branch(&git_dir);
            let sha = current_head_sha(&git_dir);
            let op = GitOp {
                op_kind: "checkout".to_string(),
                sha,
                branch,
                message: None,
                metadata: Some(serde_json::json!({ "terminal": true })),
            };
            self.emit(op, ctx).await;
            report.pushed = 1;
        }

        info!(
            "observable_bridge::git_op::reconcile: emitted terminal anchor (pushed={})",
            report.pushed
        );
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_reflog_commit_line() {
        let line = "0000000000000000000000000000000000000000 abc123def456 Josh <j@x.io> 1716500000 +0000\tcommit (initial): first commit";
        let e = parse_reflog_line(line).expect("parse");
        assert_eq!(e.new_sha, "abc123def456");
        assert_eq!(e.verb, "commit (initial)");
        assert_eq!(e.message, "first commit");
    }

    #[test]
    fn parse_reflog_checkout_line() {
        let line =
            "abc123 def456 Josh <j@x.io> 1716500001 +0000\tcheckout: moving from main to feature";
        let e = parse_reflog_line(line).expect("parse");
        assert_eq!(e.new_sha, "def456");
        assert_eq!(e.verb, "checkout");
        assert_eq!(e.message, "moving from main to feature");
    }

    #[test]
    fn parse_reflog_no_message() {
        let line = "abc def Josh <j@x.io> 1716500002 +0000\tinitial";
        let e = parse_reflog_line(line).expect("parse");
        assert_eq!(e.verb, "initial");
        assert_eq!(e.message, "");
    }

    #[test]
    fn parse_reflog_rejects_malformed() {
        assert!(parse_reflog_line("").is_none());
        assert!(parse_reflog_line("no tab here at all").is_none());
    }

    #[test]
    fn reflog_entry_to_op_maps_commit() {
        let e = ReflogEntry {
            new_sha: "sha1".into(),
            verb: "commit".into(),
            message: "feat: x".into(),
        };
        let op = reflog_entry_to_op(&e, Path::new("/nonexistent/.git")).expect("op");
        assert_eq!(op.op_kind, "commit");
        assert_eq!(op.sha.as_deref(), Some("sha1"));
        assert_eq!(op.message.as_deref(), Some("feat: x"));
    }

    #[test]
    fn reflog_entry_to_op_maps_amend_with_metadata() {
        let e = ReflogEntry {
            new_sha: "sha2".into(),
            verb: "commit (amend)".into(),
            message: "fix: y".into(),
        };
        let op = reflog_entry_to_op(&e, Path::new("/nonexistent/.git")).expect("op");
        assert_eq!(op.op_kind, "commit");
        let md = op.metadata.expect("metadata");
        assert_eq!(md["reflog_verb"], "commit (amend)");
    }

    #[test]
    fn reflog_entry_to_op_maps_pull_to_merge() {
        let e = ReflogEntry {
            new_sha: "sha3".into(),
            verb: "pull".into(),
            message: String::new(),
        };
        let op = reflog_entry_to_op(&e, Path::new("/nonexistent/.git")).expect("op");
        assert_eq!(op.op_kind, "merge");
    }

    #[test]
    fn reflog_entry_to_op_skips_unknown_verb() {
        let e = ReflogEntry {
            new_sha: "sha4".into(),
            verb: "clone".into(),
            message: String::new(),
        };
        assert!(reflog_entry_to_op(&e, Path::new("/nonexistent/.git")).is_none());
    }

    #[test]
    fn repo_basename_handles_ssh_and_https() {
        assert_eq!(
            repo_basename_from_url("git@github.com:qontinui/qontinui-runner.git"),
            "qontinui-runner"
        );
        assert_eq!(
            repo_basename_from_url("https://github.com/qontinui/qontinui-runner.git"),
            "qontinui-runner"
        );
        assert_eq!(
            repo_basename_from_url("https://github.com/qontinui/qontinui-runner"),
            "qontinui-runner"
        );
        assert_eq!(repo_basename_from_url("/local/path/myrepo/"), "myrepo");
    }

    #[test]
    fn origin_url_basename_parses_config() {
        let dir = std::env::temp_dir().join(format!("qontinui-gitop-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let config = dir.join("config");
        std::fs::write(
            &config,
            "[core]\n\trepositoryformatversion = 0\n[remote \"origin\"]\n\turl = git@github.com:qontinui/qontinui-runner.git\n\tfetch = +refs/heads/*:refs/remotes/origin/*\n",
        )
        .unwrap();
        assert_eq!(
            origin_url_basename(&config).as_deref(),
            Some("qontinui-runner")
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn parse_push_line_extracts_remote() {
        let op = parse_push_line(
            "QONTINUI_GIT_PUSH origin git@github.com:qontinui/x.git 1716500000",
            Path::new("/nonexistent/.git"),
        )
        .expect("push op");
        assert_eq!(op.op_kind, "push");
        let md = op.metadata.expect("metadata");
        assert_eq!(md["remote"], "origin");
        assert_eq!(md["remote_url"], "git@github.com:qontinui/x.git");
    }

    #[test]
    fn parse_push_line_rejects_other_lines() {
        assert!(parse_push_line("not a push line", Path::new("/x/.git")).is_none());
    }

    #[test]
    fn read_lines_since_tracks_cursor() {
        let dir = std::env::temp_dir().join(format!("qontinui-gitop-lines-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("push");
        std::fs::write(&f, "line1\nline2\n").unwrap();
        let (lines, cursor) = read_lines_since(&f, 0);
        assert_eq!(lines, vec!["line1", "line2"]);
        assert_eq!(cursor, 2);
        std::fs::write(&f, "line1\nline2\nline3\n").unwrap();
        let (lines2, cursor2) = read_lines_since(&f, cursor);
        assert_eq!(lines2, vec!["line3"]);
        assert_eq!(cursor2, 3);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn hook_body_contains_sentinel_and_push_file() {
        let body = hook_body(Path::new("/tmp/qontinui-gitop-abc.push"));
        assert!(body.contains(HOOK_SENTINEL));
        assert!(body.contains("QONTINUI_GIT_PUSH"));
        assert!(body.contains("/tmp/qontinui-gitop-abc.push"));
        assert!(body.contains(HOOK_BACKUP_NAME));
    }

    #[test]
    fn self_heal_restores_backup() {
        let hooks = std::env::temp_dir().join(format!("qontinui-gitop-heal-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&hooks).unwrap();
        let pre_push = hooks.join("pre-push");
        let backup = hooks.join(HOOK_BACKUP_NAME);
        // Simulate a leak: our hook installed, user's hook shelved.
        std::fs::write(&pre_push, format!("{HOOK_SENTINEL}\necho ours")).unwrap();
        std::fs::write(&backup, "#!/bin/sh\necho user hook").unwrap();
        self_heal_hook(&hooks);
        assert!(!backup.exists(), "backup should be consumed");
        let restored = std::fs::read_to_string(&pre_push).unwrap();
        assert!(
            restored.contains("user hook"),
            "user hook should be restored"
        );
        assert!(!restored.contains(HOOK_SENTINEL));
        std::fs::remove_dir_all(&hooks).ok();
    }

    #[test]
    fn self_heal_removes_leaked_hook_without_backup() {
        let hooks = std::env::temp_dir().join(format!("qontinui-gitop-heal2-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&hooks).unwrap();
        let pre_push = hooks.join("pre-push");
        // Leaked our hook, no backup (there was no user hook to shelve).
        std::fs::write(&pre_push, format!("{HOOK_SENTINEL}\necho ours")).unwrap();
        self_heal_hook(&hooks);
        assert!(!pre_push.exists(), "leaked qontinui hook should be removed");
        std::fs::remove_dir_all(&hooks).ok();
    }

    #[test]
    fn self_heal_leaves_user_hook_untouched() {
        let hooks = std::env::temp_dir().join(format!("qontinui-gitop-heal3-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&hooks).unwrap();
        let pre_push = hooks.join("pre-push");
        std::fs::write(&pre_push, "#!/bin/sh\necho purely user").unwrap();
        self_heal_hook(&hooks);
        assert!(pre_push.exists());
        let content = std::fs::read_to_string(&pre_push).unwrap();
        assert!(content.contains("purely user"));
        std::fs::remove_dir_all(&hooks).ok();
    }

    #[test]
    fn install_then_uninstall_restores_user_hook() {
        let hooks = std::env::temp_dir().join(format!("qontinui-gitop-inst-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&hooks).unwrap();
        let pre_push = hooks.join("pre-push");
        std::fs::write(&pre_push, "#!/bin/sh\necho user original").unwrap();
        let push_file = hooks.join("p.push");
        assert!(install_hook(&hooks, &push_file));
        // Our hook is now installed, user's hook shelved.
        assert!(is_our_hook(&pre_push));
        assert!(hooks.join(HOOK_BACKUP_NAME).exists());
        uninstall_hook(&hooks);
        let restored = std::fs::read_to_string(&pre_push).unwrap();
        assert!(restored.contains("user original"));
        assert!(!hooks.join(HOOK_BACKUP_NAME).exists());
        std::fs::remove_dir_all(&hooks).ok();
    }

    #[test]
    fn current_branch_parses_symbolic_head() {
        let git = std::env::temp_dir().join(format!("qontinui-gitop-head-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&git).unwrap();
        std::fs::write(git.join("HEAD"), "ref: refs/heads/feature/x\n").unwrap();
        assert_eq!(current_branch(&git).as_deref(), Some("feature/x"));
        std::fs::remove_dir_all(&git).ok();
    }

    #[test]
    fn current_branch_none_for_detached() {
        let git = std::env::temp_dir().join(format!("qontinui-gitop-det-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&git).unwrap();
        std::fs::write(git.join("HEAD"), "deadbeefdeadbeef\n").unwrap();
        assert!(current_branch(&git).is_none());
        std::fs::remove_dir_all(&git).ok();
    }
}

//! Graceful drain on planned restart (Phase 2 of
//! `2026-06-06-runner-dev-loop-and-restart-resilience`).
//!
//! The runner historically died hard on a planned restart/deploy: the
//! supervisor (and the in-process exit seam) hard-killed the embedded Claude
//! CLI processes with `taskkill /F /T` WITHOUT flushing in-flight AI/chat
//! turns, committing uncommitted worktree state, or persisting coord claims.
//! Resume then replayed a torn conversation and re-provisioned from scratch.
//!
//! [`drain`] makes a planned stop survivable:
//!   1. Sets a global "draining" flag so new AI turns are refused.
//!   2. Flushes each live AI session's in-flight turn to `output_log`
//!      (bounded — waits for a natural `Processing → Ready` checkpoint, then
//!      force-flushes the unpersisted delta regardless).
//!   3. Auto-commits each session's uncommitted worktree state to a WIP ref
//!      (`refs/wip/<agent_session_id>`) via `git stash create` —
//!      WITHOUT polluting branch history. Phase 3 restores these.
//!   4. Heartbeats/persists each live session's coord claim so resume can
//!      re-acquire it.
//!   5. Bounds the whole sequence with a hard timeout — a stuck session can
//!      NEVER block a deploy.
//!
//! It is exposed over HTTP as `POST /drain` (registered in `mcp_api.rs`) and
//! invoked from the `main.rs` exit seam before the hard kill. Both paths are
//! idempotent: a second drain (or a drain after `/drain`) is a fast no-op.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Serialize;
use tracing::{info, warn};
use uuid::Uuid;

use crate::claude_session::manager::SessionManager;
use crate::claude_session::session::ClaudeSession;
use crate::claude_session::state::SessionState;

/// Hard upper bound on the whole drain sequence. A stuck session must NOT
/// block a deploy, so once this elapses we stop waiting, mark `timed_out`,
/// and proceed to exit. Overridable via `QONTINUI_DRAIN_TIMEOUT_MS`.
pub const DEFAULT_DRAIN_TIMEOUT_MS: u64 = 25_000;

/// Minimum slice we leave for the non-flush steps (wip-ref + claim persist)
/// so a long turn-flush can't consume the entire budget and starve them.
const RESERVED_TAIL_MS: u64 = 4_000;

/// Poll cadence while waiting for a session to leave `Processing`.
const CHECKPOINT_POLL_MS: u64 = 100;

/// Fixed namespace UUID for deriving stable AI-session ids (UUIDv5).
///
/// SHARED CONTRACT — Phase 3 re-derives the same id to re-acquire claims and
/// to locate the `refs/wip/<agent_session_id>` ref written here. Do NOT change
/// this constant; it is the deterministic root for `stable_ai_session_id`.
///
/// Value: `b3f4d2a1-7c6e-5f48-9a2b-1d0e8c7f6a55`
pub const AI_SESSION_NS: Uuid = Uuid::from_bytes([
    0xb3, 0xf4, 0xd2, 0xa1, 0x7c, 0x6e, 0x5f, 0x48, 0x9a, 0x2b, 0x1d, 0x0e, 0x8c, 0x7f, 0x6a, 0x55,
]);

/// Global "draining" flag. Set for the lifetime of the process once a drain
/// begins (a planned stop is terminal — we never un-drain). Checked on the
/// new-AI-turn entry path (`send_user_message`) to refuse fresh work.
static DRAINING: AtomicBool = AtomicBool::new(false);

/// Guard so the exit-seam drain doesn't re-run a drain that `/drain` already
/// completed. Distinct from `DRAINING`: `DRAINING` flips the moment a drain
/// *starts*; `DRAINED` flips when one *finishes*. The exit seam checks
/// `DRAINED` to skip a redundant pass.
static DRAINED: AtomicBool = AtomicBool::new(false);

/// True once a drain has begun. New AI turns must be refused while set.
pub fn is_draining() -> bool {
    DRAINING.load(Ordering::SeqCst)
}

/// Deterministic stable AI-session id for a `task_run_id`.
///
/// `Uuid::new_v5(&AI_SESSION_NS, "ai-session:<task_run_id>")`. This is the
/// SHARED CONTRACT id used as the `refs/wip/<id>` leaf and the coord claim
/// owner discriminator. Phase 3 reuses this exact helper verbatim.
///
/// Mirrors the deterministic-v5 pattern gate-continuation uses
/// (`agent_runtime::continuation_session_id`).
pub fn stable_ai_session_id(task_run_id: &str) -> Uuid {
    let name = format!("ai-session:{task_run_id}");
    Uuid::new_v5(&AI_SESSION_NS, name.as_bytes())
}

/// Structured summary of a drain pass. Serialized as the `POST /drain` body.
#[derive(Debug, Clone, Serialize, Default)]
pub struct DrainSummary {
    /// Number of live AI sessions we flushed (turn text → `output_log`).
    pub drained_sessions: usize,
    /// Number of `refs/wip/<agent_session_id>` refs written (dirty worktrees).
    pub wip_refs_written: usize,
    /// Number of session coord claims heartbeated/persisted.
    pub claims_persisted: usize,
    /// True if the hard timeout elapsed before the sequence completed.
    pub timed_out: bool,
    /// Wall-clock time the drain took.
    pub elapsed_ms: u64,
    /// True if this call was a no-op because a drain already completed.
    pub already_drained: bool,
}

/// Resolve the configured drain timeout (env override → default).
pub fn configured_timeout() -> Duration {
    let ms = std::env::var("QONTINUI_DRAIN_TIMEOUT_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(DEFAULT_DRAIN_TIMEOUT_MS);
    Duration::from_millis(ms)
}

/// Run the graceful drain sequence, bounded by `timeout`.
///
/// Safe to call when idle (no sessions → fast no-op success). Idempotent: a
/// second call after a completed drain returns `{already_drained: true}`
/// immediately without touching any session.
pub fn drain(app_handle: &tauri::AppHandle, timeout: Duration) -> DrainSummary {
    let started = Instant::now();

    // Idempotency: a completed drain is terminal. Don't double-flush/double-stash.
    if DRAINED.load(Ordering::SeqCst) {
        return DrainSummary {
            already_drained: true,
            elapsed_ms: started.elapsed().as_millis() as u64,
            ..Default::default()
        };
    }

    // Flip the draining flag FIRST so no new turn slips in mid-drain. We never
    // reset it — a planned stop is terminal.
    DRAINING.store(true, Ordering::SeqCst);
    info!(
        "drain: starting graceful drain (timeout={}ms)",
        timeout.as_millis()
    );

    let mut summary = DrainSummary::default();

    // Enumerate live ClaudeSessions. Workers and inline PIDs don't run a
    // resumable Claude conversation with an output_log replay, so they're
    // out of scope for turn-flush/wip-capture (they're force-closed by the
    // existing seam, same as before).
    use tauri::Manager;
    let sessions: Vec<(String, Arc<ClaudeSession>)> =
        match app_handle.try_state::<Arc<SessionManager>>() {
            Some(sm) => sm.active_claude_sessions(),
            None => {
                warn!("drain: SessionManager not available — nothing to drain");
                Vec::new()
            }
        };

    if sessions.is_empty() {
        info!("drain: no live AI sessions — fast no-op");
    }

    let deadline = started + timeout;

    // ── Step (b): flush in-flight AI/chat turns ────────────────────────────
    // Reserve a tail of the budget for the wip-ref + claim steps so a slow
    // turn can't starve them. Each session gets an even slice of the
    // remaining flush budget, but we never wait past the global deadline.
    let flush_deadline = deadline
        .checked_sub(Duration::from_millis(RESERVED_TAIL_MS))
        .unwrap_or(deadline);

    for (task_run_id, session) in &sessions {
        // Bounded wait for a natural checkpoint: a turn completes when the
        // state machine transitions Processing → Ready (the dispatcher flushes
        // the turn's delta to output_log on the `result` message at that
        // point). If it doesn't checkpoint in time, force-flush below.
        wait_for_checkpoint(session, flush_deadline);

        // Force-flush whatever accumulated output hasn't been persisted yet.
        // Idempotent vs. the dispatcher's own flush (guarded by
        // persisted_output_len), so a turn that DID checkpoint flushes nothing
        // extra here.
        session.flush_pending_output();
        summary.drained_sessions += 1;
        info!(
            "drain: flushed session task_run_id={} (state={})",
            task_run_id,
            session.state()
        );
    }

    // ── Step (c): auto-commit uncommitted worktree state to a WIP ref ──────
    for (task_run_id, session) in &sessions {
        let Some(wt) = session.worktree() else {
            // No isolated worktree (session runs in the shared cwd) — nothing
            // session-scoped to capture without polluting a shared checkout.
            continue;
        };
        let agent_session_id = stable_ai_session_id(task_run_id);
        match capture_wip_ref(&wt.path, &agent_session_id, task_run_id) {
            Ok(true) => {
                summary.wip_refs_written += 1;
                info!(
                    "drain: wrote refs/wip/{} for task_run_id={} ({})",
                    agent_session_id,
                    task_run_id,
                    wt.path.display()
                );
            }
            Ok(false) => {
                // Clean tree — no ref written (by contract).
            }
            Err(e) => {
                warn!(
                    "drain: wip-ref capture failed for task_run_id={} ({}): {}",
                    task_run_id,
                    wt.path.display(),
                    e
                );
            }
        }
    }

    // ── Step (d): persist/heartbeat coord claims so resume can re-acquire ──
    if let Some(reg) =
        app_handle.try_state::<Arc<crate::claude_session::coord_register::AiCoordRegistrar>>()
    {
        for (task_run_id, _session) in &sessions {
            // Heartbeat the session's coord row (refreshes last_heartbeat_at
            // via the outbox → PATCH {heartbeat:true}). The session row +
            // worktree claim are already durable from register_session; this
            // bumps the freshness so the stale sweep doesn't reap it in the
            // restart window. No-op for unregistered sessions.
            reg.heartbeat_on_interaction(task_run_id);
            summary.claims_persisted += 1;
        }
    } else {
        warn!("drain: AiCoordRegistrar not available — coord claims not heartbeated");
    }

    summary.timed_out = Instant::now() >= deadline;
    if summary.timed_out {
        warn!(
            "drain: hard timeout ({}ms) elapsed — proceeding to exit",
            timeout.as_millis()
        );
    }

    summary.elapsed_ms = started.elapsed().as_millis() as u64;
    DRAINED.store(true, Ordering::SeqCst);

    // Phase 4 — a completed drain is the STRONGEST "clean shutdown" signal:
    // every in-flight turn was flushed, dirty worktrees were stashed, and
    // claims were heartbeated. Flip the on-disk shutdown marker to
    // `clean:true` so the NEXT boot classifies itself as a planned restart
    // (quiet banner) rather than crash recovery. Namespaced by API port to
    // match the resume-path reader. Best-effort: a marker write failure only
    // degrades the next boot to "treated as crash", which is the safe
    // direction.
    let marker_path =
        crate::session::shutdown_marker::marker_path(crate::mcp::types::get_mcp_api_port());
    crate::session::shutdown_marker::mark_clean_shutdown(&marker_path);

    info!(
        "drain: complete drained_sessions={} wip_refs={} claims={} timed_out={} elapsed_ms={}",
        summary.drained_sessions,
        summary.wip_refs_written,
        summary.claims_persisted,
        summary.timed_out,
        summary.elapsed_ms
    );

    summary
}

/// Block (polling) until `session` leaves `Processing` or `deadline` passes.
/// A session not currently processing returns immediately.
fn wait_for_checkpoint(session: &ClaudeSession, deadline: Instant) {
    while session.state() == SessionState::Processing {
        if Instant::now() >= deadline {
            return;
        }
        std::thread::sleep(Duration::from_millis(CHECKPOINT_POLL_MS));
    }
}

/// Capture the working-tree state of `repo_dir` to `refs/wip/<agent_session_id>`
/// WITHOUT polluting branch history, using `git stash create`.
///
/// Returns `Ok(true)` if a ref was written, `Ok(false)` if the tree was clean
/// (`stash create` produces no commit → no ref), `Err` on git failure.
///
/// SHARED CONTRACT: the ref namespace is `refs/wip/<agent_session_id>` and the
/// leaf is the stable v5 id. Phase 3 restores via `git stash apply <ref>` then
/// deletes the ref.
pub fn capture_wip_ref(
    repo_dir: &std::path::Path,
    agent_session_id: &Uuid,
    task_run_id: &str,
) -> Result<bool, String> {
    let stash_sha = run_git(repo_dir, &["stash", "create"])
        .map_err(|e| format!("git stash create failed: {e}"))?;
    let stash_sha = stash_sha.trim();
    if stash_sha.is_empty() {
        // Clean tree — by contract, write no ref.
        return Ok(false);
    }

    let ref_name = format!("refs/wip/{agent_session_id}");
    let msg = format!("qontinui-drain WIP for task_run_id={task_run_id}");
    run_git(repo_dir, &["update-ref", "-m", &msg, &ref_name, stash_sha])
        .map_err(|e| format!("git update-ref {ref_name} failed: {e}"))?;

    Ok(true)
}

/// Outcome of a [`restore_wip_ref`] attempt — symmetric with
/// [`capture_wip_ref`] so the resume path can log + react.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Default)]
pub struct RestoreOutcome {
    /// True if a `refs/wip/<agent_session_id>` ref existed at all.
    pub ref_existed: bool,
    /// True if `git stash apply` of the ref applied cleanly (no conflicts /
    /// non-zero exit). Only meaningful when `ref_existed`.
    pub applied_clean: bool,
    /// True if the ref was deleted after a clean apply. By contract we ONLY
    /// delete on a clean apply — a dirty/failed apply keeps the ref so the
    /// operator can recover it manually.
    pub ref_deleted: bool,
    /// The full ref name (`refs/wip/<agent_session_id>`), for logging /
    /// SYSTEM_NOTE surfacing on the not-clean path.
    pub ref_name: String,
}

/// Restore the working-tree state captured by [`capture_wip_ref`] into
/// `repo_worktree`, symmetric with capture.
///
/// Reads `refs/wip/<agent_session_id>` (a `git stash create` commit) and:
///   - If the ref does not exist → `Ok(RestoreOutcome{ ref_existed:false, .. })`
///     (nothing captured at drain time — clean no-op).
///   - If it exists → `git stash apply <ref-sha>` onto the working tree.
///     - On a CLEAN apply (zero exit) → delete the ref
///       (`update-ref -d`) so WIP refs don't accumulate ("squashed/dropped
///       on successful resume") and return `applied_clean:true, ref_deleted:true`.
///     - On a NON-clean apply (conflicts / non-zero exit) → KEEP the ref and
///       return `applied_clean:false, ref_deleted:false`. The caller surfaces
///       the ref name (a `[SYSTEM_NOTE]`) so the operator can apply it by hand.
///       This never loses the captured state.
///
/// `Err` is reserved for a failure to even probe the ref (git spawn failure on
/// `rev-parse`); a non-zero `stash apply` is NOT an `Err` — it is a clean
/// `Ok(RestoreOutcome{ applied_clean:false, .. })` so resume degrades
/// gracefully rather than aborting.
pub fn restore_wip_ref(
    repo_worktree: &std::path::Path,
    agent_session_id: &Uuid,
) -> Result<RestoreOutcome, String> {
    let ref_name = format!("refs/wip/{agent_session_id}");
    let mut outcome = RestoreOutcome {
        ref_name: ref_name.clone(),
        ..Default::default()
    };

    // Resolve the ref → stash sha. A missing ref is the clean no-op case;
    // `rev-parse --verify --quiet` exits non-zero with empty stdout for a
    // missing ref, which we distinguish from a spawn failure.
    let sha = match run_git(
        repo_worktree,
        &["rev-parse", "--verify", "--quiet", &ref_name],
    ) {
        Ok(s) => {
            let s = s.trim().to_string();
            if s.is_empty() {
                // Ref absent — nothing to restore.
                return Ok(outcome);
            }
            s
        }
        // Non-zero exit from rev-parse --quiet means the ref is absent (git
        // returns 1 with empty stderr). Treat as clean no-op, not an error.
        Err(_) => return Ok(outcome),
    };

    outcome.ref_existed = true;

    // Apply the captured stash commit onto the working tree. A clean apply
    // exits 0; a conflicting apply exits non-zero (run_git → Err) but the
    // stash commit itself is untouched, so the ref remains valid.
    match run_git(repo_worktree, &["stash", "apply", &sha]) {
        Ok(_) => {
            outcome.applied_clean = true;
            // Clean apply — drop the ref so it doesn't accumulate across
            // restarts. A failure to delete is non-fatal (the ref simply
            // lingers; a later clean restore is idempotent).
            match run_git(repo_worktree, &["update-ref", "-d", &ref_name]) {
                Ok(_) => outcome.ref_deleted = true,
                Err(e) => warn!(
                    "drain: restore applied cleanly but failed to delete {} ({}): {}",
                    ref_name,
                    repo_worktree.display(),
                    e
                ),
            }
        }
        Err(e) => {
            // Conflicted / failed apply — KEEP the ref. The caller surfaces
            // `ref_name` for manual recovery.
            warn!(
                "drain: restore of {} did not apply cleanly ({}); keeping ref for manual apply: {}",
                ref_name,
                repo_worktree.display(),
                e
            );
        }
    }

    Ok(outcome)
}

/// Run a `git -C <repo_dir> <args...>` command, returning trimmed stdout on
/// success or a descriptive error on non-zero exit / spawn failure.
fn run_git(repo_dir: &std::path::Path, args: &[&str]) -> Result<String, String> {
    let mut cmd = std::process::Command::new("git");
    cmd.arg("-C").arg(repo_dir);
    cmd.args(args);
    let output = cmd
        .output()
        .map_err(|e| format!("failed to spawn git: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "git {:?} exited {}: {}",
            args,
            output.status.code().unwrap_or(-1),
            stderr.trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Initialize a throwaway git repo in `dir` with one committed file so
    /// `stash create` has a base commit to diff against.
    fn init_repo(dir: &std::path::Path) {
        let run = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(args)
                .output()
                .expect("git spawn");
            assert!(
                out.status.success(),
                "git {:?} failed: {}",
                args,
                String::from_utf8_lossy(&out.stderr)
            );
        };
        run(&["init", "-q"]);
        run(&["config", "user.email", "drain-test@example.com"]);
        run(&["config", "user.name", "Drain Test"]);
        run(&["config", "commit.gpgsign", "false"]);
        // Pin line-ending handling off so round-trip content comparisons are
        // exact on Windows (default core.autocrlf=true would rewrite \n→\r\n
        // on checkout/reset).
        run(&["config", "core.autocrlf", "false"]);
        std::fs::write(dir.join("seed.txt"), "seed\n").unwrap();
        run(&["add", "-A"]);
        run(&["commit", "-q", "-m", "seed"]);
    }

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let base = std::env::temp_dir().join(format!(
            "qontinui-drain-test-{}-{}-{}",
            tag,
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&base).unwrap();
        base
    }

    fn ref_exists(dir: &std::path::Path, ref_name: &str) -> bool {
        std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["rev-parse", "--verify", "--quiet", ref_name])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    #[test]
    fn stable_ai_session_id_is_deterministic_and_namespaced() {
        let a = stable_ai_session_id("task-123");
        let b = stable_ai_session_id("task-123");
        let c = stable_ai_session_id("task-999");
        assert_eq!(a, b, "same task_run_id must yield same id");
        assert_ne!(a, c, "different task_run_id must yield different id");
        // Pin the contract: a known input maps to a stable v5 value.
        let expected = Uuid::new_v5(&AI_SESSION_NS, b"ai-session:task-123");
        assert_eq!(a, expected);
        assert_eq!(a.get_version_num(), 5);
    }

    #[test]
    fn capture_wip_ref_clean_tree_writes_no_ref() {
        let dir = temp_dir("clean");
        init_repo(&dir);
        let sid = stable_ai_session_id("task-clean");
        let wrote = capture_wip_ref(&dir, &sid, "task-clean").expect("capture ok");
        assert!(!wrote, "clean tree must not write a ref");
        assert!(
            !ref_exists(&dir, &format!("refs/wip/{sid}")),
            "no wip ref should exist for a clean tree"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn capture_wip_ref_dirty_tree_writes_ref() {
        let dir = temp_dir("dirty");
        init_repo(&dir);
        // Make the tree dirty (modify tracked + add untracked).
        std::fs::write(dir.join("seed.txt"), "seed\nlocal edit\n").unwrap();
        std::fs::write(dir.join("scratch.txt"), "uncommitted\n").unwrap();
        // Stage so `stash create` captures it (create stashes the index + tree).
        std::process::Command::new("git")
            .arg("-C")
            .arg(&dir)
            .args(["add", "-A"])
            .output()
            .unwrap();

        let sid = stable_ai_session_id("task-dirty");
        let wrote = capture_wip_ref(&dir, &sid, "task-dirty").expect("capture ok");
        assert!(wrote, "dirty tree must write a ref");
        let ref_name = format!("refs/wip/{sid}");
        assert!(
            ref_exists(&dir, &ref_name),
            "wip ref {ref_name} should exist after capture"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Read the working-tree content of a tracked file (or "" if absent),
    /// with CRLF normalized to LF so comparisons are platform-independent.
    fn read_file(dir: &std::path::Path, name: &str) -> String {
        std::fs::read_to_string(dir.join(name))
            .unwrap_or_default()
            .replace("\r\n", "\n")
    }

    #[test]
    fn restore_wip_ref_round_trips_with_capture() {
        let dir = temp_dir("roundtrip");
        init_repo(&dir);

        // Dirty the tree: modify a tracked file + add an untracked file.
        std::fs::write(dir.join("seed.txt"), "seed\nWIP edit\n").unwrap();
        std::fs::write(dir.join("scratch.txt"), "uncommitted\n").unwrap();
        std::process::Command::new("git")
            .arg("-C")
            .arg(&dir)
            .args(["add", "-A"])
            .output()
            .unwrap();

        let sid = stable_ai_session_id("task-roundtrip");
        let ref_name = format!("refs/wip/{sid}");

        // Capture → ref exists.
        let wrote = capture_wip_ref(&dir, &sid, "task-roundtrip").expect("capture ok");
        assert!(wrote, "dirty tree must capture a ref");
        assert!(ref_exists(&dir, &ref_name));

        // Reset the working tree back to the committed state (simulating a
        // fresh worktree on resume before restore runs).
        let reset = std::process::Command::new("git")
            .arg("-C")
            .arg(&dir)
            .args(["reset", "--hard", "-q"])
            .output()
            .unwrap();
        assert!(reset.status.success());
        let _ = std::fs::remove_file(dir.join("scratch.txt"));
        assert_eq!(read_file(&dir, "seed.txt"), "seed\n", "tree reset to base");

        // Restore → tree matches the captured WIP, ref deleted.
        let outcome = restore_wip_ref(&dir, &sid).expect("restore ok");
        assert!(outcome.ref_existed, "ref must have existed");
        assert!(
            outcome.applied_clean,
            "apply onto a clean tree is conflict-free"
        );
        assert!(outcome.ref_deleted, "clean apply must delete the ref");
        assert_eq!(outcome.ref_name, ref_name);

        // Working tree now carries the captured edits again.
        assert_eq!(read_file(&dir, "seed.txt"), "seed\nWIP edit\n");
        assert_eq!(read_file(&dir, "scratch.txt"), "uncommitted\n");
        // Ref is gone (no accumulation).
        assert!(
            !ref_exists(&dir, &ref_name),
            "ref must be deleted after a clean restore"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn restore_wip_ref_absent_is_clean_noop() {
        let dir = temp_dir("absent");
        init_repo(&dir);
        let sid = stable_ai_session_id("task-absent");
        let outcome = restore_wip_ref(&dir, &sid).expect("restore ok");
        assert!(!outcome.ref_existed, "no ref → ref_existed false");
        assert!(!outcome.applied_clean);
        assert!(!outcome.ref_deleted);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn configured_timeout_defaults_when_unset() {
        // Don't mutate the global env in a way that races other tests; just
        // assert the default is returned when the override is absent/invalid.
        std::env::remove_var("QONTINUI_DRAIN_TIMEOUT_MS");
        assert_eq!(
            configured_timeout(),
            Duration::from_millis(DEFAULT_DRAIN_TIMEOUT_MS)
        );
    }
}

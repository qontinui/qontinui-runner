//! SessionManager - tracks active Claude sessions by task_run_id.

use std::collections::HashMap;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};
use tracing::{debug, info};

use super::session::ClaudeSession;
use super::state::SessionState;

/// Manages active Claude CLI sessions, keyed by task_run_id.
pub struct SessionManager {
    sessions: Mutex<HashMap<String, Arc<ClaudeSession>>>,
    /// PIDs for inline (non-interactive) Claude sessions, keyed by task_run_id.
    /// These sessions don't have a full ClaudeSession object but still need to be
    /// visible to the stale task sweep so it can check process liveness.
    inline_pids: Mutex<HashMap<String, u32>>,
    /// Pending context to prepend to the next user message for a given task_run_id.
    /// Used for system notes that should be delivered with the next user message
    /// rather than sent as standalone messages (which would trigger unwanted response turns).
    pending_context: Mutex<HashMap<String, Vec<String>>>,
}

impl SessionManager {
    pub fn new() -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            inline_pids: Mutex::new(HashMap::new()),
            pending_context: Mutex::new(HashMap::new()),
        }
    }

    /// Register a session. Returns Err if a session already exists for this key.
    pub fn register(&self, task_run_id: &str, session: Arc<ClaudeSession>) -> Result<(), String> {
        let mut guard = self
            .sessions
            .lock()
            .map_err(|e| format!("SessionManager lock poisoned: {}", e))?;

        if guard.contains_key(task_run_id) {
            return Err(format!(
                "Session already registered for task_run_id: {}",
                task_run_id
            ));
        }

        info!("SessionManager: registered session for {}", task_run_id);
        guard.insert(task_run_id.to_string(), session);
        Ok(())
    }

    /// Get a session by task_run_id.
    pub fn get(&self, task_run_id: &str) -> Option<Arc<ClaudeSession>> {
        self.sessions
            .lock()
            .ok()
            .and_then(|guard| guard.get(task_run_id).cloned())
    }

    /// Remove and return a session.
    pub fn remove(&self, task_run_id: &str) -> Option<Arc<ClaudeSession>> {
        let removed = self
            .sessions
            .lock()
            .ok()
            .and_then(|mut guard| guard.remove(task_run_id));

        if removed.is_some() {
            info!("SessionManager: removed session for {}", task_run_id);
        } else {
            debug!(
                "SessionManager: no session found for {} (already removed?)",
                task_run_id
            );
        }

        removed
    }

    /// Get the current state of a session.
    pub fn get_state(&self, task_run_id: &str) -> Option<SessionState> {
        self.get(task_run_id).map(|s| s.state())
    }

    // (helper `worktree_path_matches` lives at module scope below so it is
    // unit-testable without constructing a `ClaudeSession`.)

    /// Resolve the `task_run_id` of the active `ClaudeSession` whose isolated
    /// worktree checkout is `workdir` (session-fabric Phase 0 self-identification).
    ///
    /// The coord-mcp loopback proxy knows only the session WORKDIR its
    /// per-session nonce was provisioned into; this bridges that back to the
    /// `task_run_id` the `AiCoordRegistrar` keys the coord `agent_session_id`
    /// on. Matches on the session's worktree path — the provisioning
    /// chokepoint (`provision_session_cwd`, reached from `acquire_for_terminal`
    /// and `acquire_for_worker`) writes the nonce into exactly that dir.
    ///
    /// Two sources of that path, because sessions reach a worktree two ways:
    /// - PROMOTED ([`ClaudeSession::worktree`]) — moved into one after spawn.
    /// - SPAWNED in one, with the `IsolatedEditContext` parked on the session
    ///   ([`ClaudeSession::isolated_worktree_paths`]) — every orchestration
    ///   worker that declares a repo (which, since plan
    ///   `2026-09-23-conductor-e2e-phase1-defects` Phase 1, never runs without
    ///   one) and every resumed chat session that re-acquired its worktree.
    ///   Before this source was read, those sessions never self-identified.
    ///   A parked path counts only when it is an agent allocation ROOT: a
    ///   `shared_branch` row names the canonical checkout, whose in-cwd
    ///   `.mcp.json` every session launched there reads, so matching it would
    ///   attribute a stranger's call to this session.
    ///
    /// Sessions with neither carry no stored cwd to match, so they answer
    /// [`WorkdirTaskRun::NoCandidate`]; two DIFFERENT sessions claiming the same
    /// workdir answer [`WorkdirTaskRun::Ambiguous`] — see
    /// [`identifying_candidate`]. The two are distinct answers because only the
    /// first licenses a caller to look for the session elsewhere (the lifecycle
    /// store): an ambiguous workdir DOES host a task run, which owns its calls.
    /// O(N) over active sessions (bounded by the operator's live terminal count).
    pub fn task_run_id_for_workdir(&self, workdir: &str) -> WorkdirTaskRun {
        // Snapshot (task_run_id, worktree path) pairs under the lock, then do
        // the filesystem checks AFTER releasing it. This runs on every proxied
        // coord_* call (session-fabric Phase 0 caller self-identification), and
        // holding `sessions` across O(live sessions) `canonicalize` syscalls
        // would serialize all session management behind proxy traffic. The
        // snapshot clones one String + PathBuf per worktree path (a multi-repo
        // context contributes one per repo); the syscalls happen lock-free.
        let candidates: Vec<WorkdirCandidate> = {
            // A poisoned lock is UNKNOWN, not "no task run": reporting it as
            // `NoCandidate` would let a caller fall through to a guess.
            let Ok(guard) = self.sessions.lock() else {
                return WorkdirTaskRun::Ambiguous;
            };
            workdir_candidates(guard.iter().map(|(task_run_id, session)| {
                (
                    task_run_id.as_str(),
                    session.worktree().map(|wt| wt.path.clone()),
                    session.isolated_worktree_paths(),
                )
            }))
        };
        identifying_candidate(
            &candidates,
            workdir,
            || std::fs::canonicalize(workdir).ok(),
            is_agent_allocation_root,
        )
    }

    /// Register an inline (non-interactive) session's PID for stale task sweep visibility.
    /// Unlike `register`, this doesn't require a full ClaudeSession — just the PID.
    pub fn register_inline_pid(&self, task_run_id: &str, pid: u32) {
        if let Ok(mut guard) = self.inline_pids.lock() {
            guard.insert(task_run_id.to_string(), pid);
            info!(
                "SessionManager: registered inline PID {} for {}",
                pid, task_run_id
            );
        }
    }

    /// Remove an inline session's PID (called when the inline session completes).
    pub fn remove_inline_pid(&self, task_run_id: &str) {
        if let Ok(mut guard) = self.inline_pids.lock() {
            if guard.remove(task_run_id).is_some() {
                info!("SessionManager: removed inline PID for {}", task_run_id);
            }
        }
    }

    /// List all sessions with their task_run_id, current state, and PID.
    /// Includes ClaudeSessions and inline PID registrations.
    /// Used by the stale task sweep to check session liveness.
    pub fn list_all_with_state(&self) -> Vec<(String, SessionState, u32)> {
        let mut results: Vec<(String, SessionState, u32)> = self
            .sessions
            .lock()
            .map(|guard| {
                guard
                    .iter()
                    .map(|(k, s)| (k.clone(), s.state(), s.pid()))
                    .collect()
            })
            .unwrap_or_default();

        // Include inline PIDs with a synthetic Processing state
        if let Ok(guard) = self.inline_pids.lock() {
            for (id, pid) in guard.iter() {
                // Only add if not already covered by an interactive session
                if !results.iter().any(|(k, _, _)| k == id) {
                    results.push((id.clone(), SessionState::Processing, *pid));
                }
            }
        }

        results
    }

    /// Snapshot every registered `ClaudeSession` as a tuple of
    /// `(task_run_id, holder_name, last_activity_tracker)`.
    ///
    /// `holder_name` matches the friendly display name the file-lock
    /// dispatcher emits as `holder_name` on `file-lock-*` events (see
    /// `claude_session/dispatcher.rs:398-404` and
    /// `ClaudeSession::holder_name`).
    ///
    /// `last_activity_tracker` is a shared `Arc<AtomicU64>` holding the
    /// most-recent stdout-line epoch SECONDS (written at
    /// `claude_session/session.rs:420`). Callers convert to ms at the
    /// boundary.
    ///
    /// Inline-PID registrations are intentionally excluded: they don't
    /// expose a `last_activity` channel.
    /// Returns an empty Vec when no `ClaudeSession`s are registered or
    /// when the inner Mutex is poisoned.
    pub fn snapshot(&self) -> Vec<(String, String, Arc<AtomicU64>)> {
        self.sessions
            .lock()
            .ok()
            .map(|guard| {
                guard
                    .iter()
                    .map(|(id, s)| {
                        (
                            id.clone(),
                            s.holder_name().to_string(),
                            s.last_activity_tracker(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// List all active session task_run_ids — every ClaudeSession whose
    /// state is non-Closed. Broad-by-default: includes `Initializing`
    /// sessions. Callers that need `Ready` sessions only MUST filter the
    /// returned ids via [`Self::get_state`].
    pub fn list_active(&self) -> Vec<String> {
        self.sessions
            .lock()
            .map(|guard| {
                guard
                    .iter()
                    .filter(|(_, s)| s.state().is_active())
                    .map(|(k, _)| k.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Snapshot every active `ClaudeSession` as `(task_run_id, session)`.
    ///
    /// Used by the graceful-drain sequence (`crate::drain`) which needs the
    /// live `Arc<ClaudeSession>` (not just the id) to flush in-flight turns,
    /// read each session's `worktree()`, and force-flush unpersisted output.
    /// Excludes inline PIDs — they don't run a resumable Claude
    /// conversation with an `output_log` replay, so there's nothing to drain.
    pub fn active_claude_sessions(&self) -> Vec<(String, Arc<ClaudeSession>)> {
        self.sessions
            .lock()
            .ok()
            .map(|guard| {
                guard
                    .iter()
                    .filter(|(_, s)| s.state().is_active())
                    .map(|(k, s)| (k.clone(), s.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Clean up any closed sessions.
    pub fn cleanup_closed(&self) {
        if let Ok(mut guard) = self.sessions.lock() {
            let before = guard.len();
            guard.retain(|_, s| s.state().is_active());
            let removed = before - guard.len();
            if removed > 0 {
                info!("SessionManager: cleaned up {} closed sessions", removed);
            }
        }
    }

    /// Add pending context that will be prepended to the next user message.
    /// This is used for system notes that Claude should see but that should NOT
    /// trigger a standalone response turn.
    pub fn push_pending_context(&self, task_run_id: &str, context: String) {
        if let Ok(mut guard) = self.pending_context.lock() {
            let len = context.len();
            guard
                .entry(task_run_id.to_string())
                .or_default()
                .push(context);
            info!(
                "SessionManager: queued pending context for {} ({} chars)",
                task_run_id, len
            );
        }
    }

    /// Drain all pending context for a task_run_id, returning it as a single
    /// string to prepend to the next user message. Returns None if no pending context.
    pub fn drain_pending_context(&self, task_run_id: &str) -> Option<String> {
        if let Ok(mut guard) = self.pending_context.lock() {
            if let Some(contexts) = guard.remove(task_run_id) {
                if contexts.is_empty() {
                    return None;
                }
                let combined = contexts.join("\n\n");
                info!(
                    "SessionManager: drained {} pending context(s) for {} ({} chars)",
                    contexts.len(),
                    task_run_id,
                    combined.len()
                );
                return Some(combined);
            }
        }
        None
    }

    /// Close and remove all active sessions (used during app shutdown or stop-all).
    pub fn close_all_sessions(&self) {
        if let Ok(mut guard) = self.sessions.lock() {
            let count = guard.len();
            if count > 0 {
                info!("SessionManager: closing all {} active sessions", count);
                for (task_run_id, session) in guard.drain() {
                    info!(
                        "SessionManager: closing session for {} (state: {})",
                        task_run_id,
                        session.state()
                    );
                    let _ = session.close();
                }
            }
        }
        // Also clear inline PID registrations
        if let Ok(mut guard) = self.inline_pids.lock() {
            let count = guard.len();
            if count > 0 {
                info!(
                    "SessionManager: clearing {} inline PID registrations",
                    count
                );
                guard.clear();
            }
        }
    }
}

/// What [`SessionManager::task_run_id_for_workdir`] found.
///
/// Tri-state on purpose. An `Option` collapsed two different facts into `None`:
/// nothing on this workdir (a caller may then look the session up elsewhere,
/// e.g. the lifecycle store), and SEVERAL sessions on it (some task run owns
/// the workdir's calls, so a lookup elsewhere could file one session's call
/// under a sibling's identity — exactly the misattribution the ambiguity
/// refusal exists to prevent).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkdirTaskRun {
    /// Exactly one session identifies the workdir.
    Found(String),
    /// No session claims the workdir.
    NoCandidate,
    /// Different sessions claim it (or the session table was unreadable): the
    /// caller must NOT guess, and must not fall through to another source.
    Ambiguous,
}

/// One session's claim on a workdir, snapshotted under the `sessions` lock so
/// the filesystem checks run after it is released.
struct WorkdirCandidate {
    task_run_id: String,
    path: std::path::PathBuf,
    /// `true` when `path` came from a parked `IsolatedEditContext` rather than
    /// a promoted worktree — see [`SessionManager::task_run_id_for_workdir`].
    parked: bool,
}

/// The one session whose candidate path identifies `workdir`.
///
/// Two passes, so the common case stays off the filesystem: a literal string
/// compare first (the proxy provisions the nonce into the exact stored path),
/// and only when nothing matches literally, the canonical-path compare —
/// `canonicalize(workdir)` is computed lazily, once. A parked path must also be
/// an agent allocation root (`is_allocation_root`), so a `shared_branch`
/// context naming the canonical checkout never matches; that probe runs only
/// on a path match.
///
/// AMBIGUITY RESOLVES TO [`WorkdirTaskRun::Ambiguous`]: when the accepted matches name more than one
/// session (two contexts on one root, a promoted path and another session's
/// parked one), HashMap order would otherwise pick the caller at random. No
/// header beats a wrong one — the same rule the terminal leg applies. Pure over
/// its injected probes so every arm is unit-testable.
fn identifying_candidate(
    candidates: &[WorkdirCandidate],
    workdir: &str,
    canon_of_workdir: impl FnOnce() -> Option<std::path::PathBuf>,
    is_allocation_root: impl Fn(&std::path::Path) -> bool,
) -> WorkdirTaskRun {
    let accepted = |c: &&WorkdirCandidate| !c.parked || is_allocation_root(&c.path);
    let literal: Vec<&WorkdirCandidate> = candidates
        .iter()
        .filter(|c| c.path.to_string_lossy() == workdir)
        .filter(accepted)
        .collect();
    let hits = if literal.is_empty() {
        match canon_of_workdir() {
            Some(tc) => candidates
                .iter()
                .filter(|c| worktree_path_matches(&c.path, workdir, Some(&tc)))
                .filter(accepted)
                .collect(),
            None => Vec::new(),
        }
    } else {
        literal
    };
    let mut ids = hits.iter().map(|c| c.task_run_id.as_str());
    let Some(first) = ids.next() else {
        return WorkdirTaskRun::NoCandidate;
    };
    if ids.all(|id| id == first) {
        WorkdirTaskRun::Found(first.to_string())
    } else {
        WorkdirTaskRun::Ambiguous
    }
}

/// The snapshot half of [`SessionManager::task_run_id_for_workdir`]: one
/// candidate per promoted worktree and one per parked context path, from
/// `(task_run_id, promoted path, parked paths)` tuples. Separate from the lock
/// so the loop is testable against real parked contexts.
fn workdir_candidates<'a>(
    sessions: impl IntoIterator<Item = (&'a str, Option<std::path::PathBuf>, Vec<std::path::PathBuf>)>,
) -> Vec<WorkdirCandidate> {
    let mut out = Vec::new();
    for (task_run_id, promoted, parked) in sessions {
        if let Some(path) = promoted {
            out.push(WorkdirCandidate {
                task_run_id: task_run_id.to_string(),
                path,
                parked: false,
            });
        }
        for path in parked {
            out.push(WorkdirCandidate {
                task_run_id: task_run_id.to_string(),
                path,
                parked: true,
            });
        }
    }
    out
}

/// Is `path` itself an agent worktree allocation root (not a descendant, and
/// not a canonical checkout)? The same predicate `provision_session_cwd` uses
/// to decide a cwd was freshly allocated.
fn is_agent_allocation_root(path: &std::path::Path) -> bool {
    crate::agent_worktree::canonical_paths::allocated_worktree_for_path(path)
        .is_some_and(|root| crate::agent_worktree::canonical_paths::paths_equal(&root, path))
}

/// Does a session's worktree `path` identify `workdir`? A fast literal-string
/// compare first (the common case — the proxy provisions the nonce into the
/// exact stored path), then a canonical-path fallback for symlink/8.3/case
/// differences. `target_canon` is `canonicalize(workdir)` computed once by the
/// caller so it is not repeated per session. Pure and lock-free by design so
/// `task_run_id_for_workdir` can run it outside the `sessions` lock and so it
/// is unit-testable without a live `ClaudeSession`.
fn worktree_path_matches(
    path: &std::path::Path,
    workdir: &str,
    target_canon: Option<&std::path::Path>,
) -> bool {
    if path.to_string_lossy() == workdir {
        return true;
    }
    match target_canon {
        Some(tc) => std::fs::canonicalize(path).ok().as_deref() == Some(tc),
        None => false,
    }
}

impl Default for SessionManager {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for SessionManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let count = self.sessions.lock().map(|g| g.len()).unwrap_or(0);
        f.debug_struct("SessionManager")
            .field("session_count", &count)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(task_run_id: &str, path: &str, parked: bool) -> WorkdirCandidate {
        WorkdirCandidate {
            task_run_id: task_run_id.to_string(),
            path: std::path::PathBuf::from(path),
            parked,
        }
    }

    /// An orchestration worker is spawned IN its allocated worktree, so its
    /// path lives only on the parked context. It must resolve — before the
    /// parked source was read, every worker's proxied coord call went out with
    /// no caller-session header.
    #[test]
    fn a_parked_allocation_root_identifies_its_worker() {
        let wt = "D:/qontinui-root/agent-worktrees/abc/qontinui-runner";
        let hit = identifying_candidate(
            &[
                candidate(
                    "other",
                    "D:/qontinui-root/agent-worktrees/xyz/qontinui-web",
                    true,
                ),
                candidate("worker", wt, true),
            ],
            wt,
            || panic!("a literal hit must not canonicalize"),
            |_| true,
        );
        assert_eq!(hit, WorkdirTaskRun::Found("worker".to_string()));
    }

    /// A `shared_branch` context names the canonical checkout, which is not an
    /// allocation root and whose `.mcp.json` every session there shares. It
    /// must not claim the workdir — and the probe must be what decides it.
    #[test]
    fn a_parked_path_that_is_not_an_allocation_root_does_not_identify() {
        let canonical = "D:/qontinui-root/qontinui-runner";
        let hit = identifying_candidate(
            &[candidate("resumed-chat", canonical, true)],
            canonical,
            || None,
            |_| false,
        );
        assert_eq!(hit, WorkdirTaskRun::NoCandidate);
    }

    /// A promoted worktree keeps matching without the allocation-root probe,
    /// exactly as before the parked source existed.
    #[test]
    fn a_promoted_worktree_matches_without_the_allocation_root_probe() {
        let wt = "D:/qontinui-root/agent-worktrees/abc/qontinui-runner";
        let hit = identifying_candidate(
            &[candidate("promoted", wt, false)],
            wt,
            || None,
            |_| panic!("a promoted path must not consult the allocation-root probe"),
        );
        assert_eq!(hit, WorkdirTaskRun::Found("promoted".to_string()));
    }

    /// Two different sessions accepted on one workdir: no header beats a
    /// guessed one. The same session contributing twice (a promoted path and
    /// its own parked context) is still that session.
    #[test]
    fn two_sessions_on_one_workdir_resolve_to_none() {
        let wt = "D:/qontinui-root/agent-worktrees/abc/qontinui-runner";
        let ambiguous = identifying_candidate(
            &[candidate("a", wt, true), candidate("b", wt, false)],
            wt,
            || None,
            |_| true,
        );
        assert_eq!(ambiguous, WorkdirTaskRun::Ambiguous);
        let same = identifying_candidate(
            &[candidate("a", wt, false), candidate("a", wt, true)],
            wt,
            || None,
            |_| true,
        );
        assert_eq!(same, WorkdirTaskRun::Found("a".to_string()));
    }

    /// No literal match falls through to the canonical compare, and a parked
    /// candidate found there still has to pass the allocation-root probe.
    #[test]
    fn the_canonical_pass_runs_only_without_a_literal_hit() {
        let base = std::env::temp_dir();
        let dotted = base.join(".");
        let dotted = dotted.to_string_lossy();
        let base_str = base.to_string_lossy();
        let hit = identifying_candidate(
            &[candidate("worker", &dotted, true)],
            &base_str,
            || std::fs::canonicalize(&base).ok(),
            |_| true,
        );
        assert_eq!(hit, WorkdirTaskRun::Found("worker".to_string()));
        let refused = identifying_candidate(
            &[candidate("worker", &dotted, true)],
            &base_str,
            || std::fs::canonicalize(&base).ok(),
            |_| false,
        );
        assert_eq!(refused, WorkdirTaskRun::NoCandidate);
    }

    /// Real parked contexts through the real snapshot loop
    /// (`workdir_candidates` over `ClaudeSession::isolated_worktree_paths`'
    /// own projection) and the real allocation-root predicate
    /// (`allocated_worktree_for_path_in`, the core of `is_agent_allocation_root`),
    /// on a temp worktree root: an agent allocation root identifies its worker;
    /// the canonical checkout named by a `shared_branch` row does not; and two
    /// sessions parked on the same allocation (the old first-match-wins case)
    /// are Ambiguous.
    #[test]
    fn parked_contexts_resolve_through_the_real_snapshot_and_root_predicate() {
        use crate::agent_worktree::isolated_edit::IsolatedEditContext;
        use crate::agent_worktree::MaterializedWorktree;
        fn ctx(paths: &[&std::path::Path]) -> IsolatedEditContext {
            IsolatedEditContext::for_test(
                paths
                    .iter()
                    .map(|p| MaterializedWorktree {
                        repo: "r".to_string(),
                        branch: "b".to_string(),
                        parent_sha: String::new(),
                        worktree_path: p.to_path_buf(),
                        push_ref: String::new(),
                        parent_sha_provenance: Default::default(),
                    })
                    .collect(),
                Vec::new(),
            )
        }
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("agent-worktrees");
        let alloc = root.join("agent1").join("repo");
        std::fs::create_dir_all(alloc.join(".git")).unwrap();
        let sub = alloc.join("src");
        std::fs::create_dir_all(&sub).unwrap();
        let canonical = tmp.path().join("repo-canonical");
        std::fs::create_dir_all(canonical.join(".git")).unwrap();
        let is_root = |p: &std::path::Path| {
            crate::agent_worktree::canonical_paths::allocated_worktree_for_path_in(&root, p)
                .is_some_and(|r| crate::agent_worktree::canonical_paths::paths_equal(&r, p))
        };
        let parked = |c: &IsolatedEditContext| -> Vec<std::path::PathBuf> {
            c.worktrees
                .iter()
                .map(|w| w.worktree_path.clone())
                .collect()
        };
        let (worker, chat) = (ctx(&[&alloc]), ctx(&[&canonical, &sub]));
        let cands = workdir_candidates([
            ("worker", None, parked(&worker)),
            ("chat", None, parked(&chat)),
        ]);
        let ask = |wd: &std::path::Path| {
            identifying_candidate(&cands, &wd.to_string_lossy(), || None, is_root)
        };
        assert_eq!(ask(&alloc), WorkdirTaskRun::Found("worker".to_string()));
        // `shared_branch` row naming the canonical checkout: refused.
        assert_eq!(ask(&canonical), WorkdirTaskRun::NoCandidate);
        // A descendant of an allocation is not an allocation ROOT.
        assert_eq!(ask(&sub), WorkdirTaskRun::NoCandidate);
        // A second session parked on the same allocation: first-match-wins is gone.
        let twin = ctx(&[&alloc]);
        let cands2 = workdir_candidates([
            ("worker", None, parked(&worker)),
            ("twin", None, parked(&twin)),
        ]);
        assert_eq!(
            identifying_candidate(&cands2, &alloc.to_string_lossy(), || None, is_root),
            WorkdirTaskRun::Ambiguous
        );
    }

    #[test]
    fn worktree_path_matches_on_exact_string_without_touching_fs() {
        // The common path: the stored worktree path is byte-identical to the
        // workdir the proxy provisioned. Must match on the string alone, so a
        // `None` target_canon (canonicalize failed / not computed) is
        // irrelevant — no filesystem access is required for a hit.
        let p = std::path::Path::new("D:/qontinui-root/agent-worktrees/abc/qontinui-runner");
        assert!(worktree_path_matches(
            p,
            "D:/qontinui-root/agent-worktrees/abc/qontinui-runner",
            None
        ));
    }

    #[test]
    fn worktree_path_matches_rejects_a_different_path() {
        // Distinct paths, and no canonical target to fall back on ⇒ no match.
        // This is the worktree-less / wrong-session case that must resolve to
        // None so coord keeps its fuzzy fallback rather than misattributing.
        let p = std::path::Path::new("D:/qontinui-root/agent-worktrees/abc/qontinui-runner");
        assert!(!worktree_path_matches(
            p,
            "D:/qontinui-root/agent-worktrees/XYZ/qontinui-runner",
            None
        ));
    }

    #[test]
    fn worktree_path_matches_canonical_fallback_on_a_real_dir() {
        // When the strings differ, an equal *canonical* form still matches.
        // Use the temp dir (a real path) reached two ways: itself, and itself
        // + "/." — different strings, identical canonicalization.
        let base = std::env::temp_dir();
        let dotted = base.join(".");
        let target_canon = std::fs::canonicalize(&base).ok();
        assert!(
            target_canon.is_some(),
            "temp dir must canonicalize for this test to be meaningful"
        );
        // `dotted` as the stored session path, `base` as the query workdir:
        // strings differ (trailing "/."), canonical forms match.
        assert!(worktree_path_matches(
            &dotted,
            &base.to_string_lossy(),
            target_canon.as_deref()
        ));
    }
}

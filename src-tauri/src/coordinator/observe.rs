//! Single-snapshot read of the data the cheap rules consume.
//!
//! Mirrors the body of `crate::mcp::coordinator::get_coordinator_state`
//! (HTTP `GET /coordinator/state`) so the Rust scheduler doesn't have to
//! roundtrip its own runner over HTTP. Same PG calls, same in-memory
//! registry reads, same `SessionManager::list_active` path — just
//! returned as a typed Rust struct rather than JSON.
//!
//! Per the Phase 1b plan: "Reuse the same PG calls; do not add new SQL."

use std::collections::HashMap;
use std::sync::Arc;

use tauri::Manager;

use crate::claude_session::state::SessionState;
use crate::claude_session::SessionManager;
use crate::database::pg::coordinator_decisions::CoordinatorDecisionRow;
use crate::database::pg::reviews::ReviewRow;
use crate::database::pg::tasks::{TaskRow, NON_TERMINAL_STATUSES};
use crate::executor::file_registry::FileRegistryInfo;
use crate::executor::upcoming_file_registry::UpcomingClaim;
use crate::mcp::types::ApiState;

/// Snapshot for one live session — Rule A and Rule B reason about these.
#[derive(Debug, Clone)]
pub(crate) struct LiveSession {
    pub task_run_id: String,
    /// Concrete state from `SessionManager::get_state`. `None` when the
    /// session disappeared between `list_active()` and the per-id lookup.
    pub state: Option<SessionState>,
    /// Task currently assigned to this session (any non-terminal status).
    /// Populated by joining `tasks.assigned_session_id == task_run_id`.
    pub assigned_task_id: Option<String>,
}

/// Heatmap-derived per-session aggregates the Rule B composite tie-break
/// reads. Populated once per `observe()` tick from `PgDb::hot_sessions`
/// and `FileLockManager::info()`.
#[derive(Debug, Clone, Default)]
pub(crate) struct HeatmapSnapshot {
    /// task_run_id → distinct file paths touched in window. Sourced from
    /// `HotSessionRow.distinct_files`. The `coord.session_touched_files`
    /// table UPSERTs on `(task_run_id, file_path)`, so re-edits don't
    /// inflate — this is breadth-of-activity, not raw frequency.
    pub session_distinct_files: HashMap<String, u64>,
    /// task_run_id → count of files this session currently holds locks
    /// on. Derived from `FileLockManager::info()` grouped by
    /// `holder_task_run_id`.
    pub session_held_count: HashMap<String, u64>,
}

/// Single-tick snapshot. Bundles every read the cheap rules need.
#[derive(Debug, Clone)]
pub(crate) struct Observation {
    pub tasks: Vec<TaskRow>,
    pub active_file_registry: Vec<FileRegistryInfo>,
    pub upcoming_file_registry: Vec<UpcomingClaim>,
    pub live_sessions: Vec<LiveSession>,
    pub recent_reviews: Vec<ReviewRow>,
    pub open_escalations: Vec<CoordinatorDecisionRow>,
    /// Files touched by each `task_run_id`. Pre-fetched once per tick so
    /// Rule B's path-overlap check doesn't fan-out to N PG queries inside
    /// the rule.
    pub session_touched_files: HashMap<String, Vec<String>>,
    /// The most-recent N coordinator_decisions rows. Phase 1b reads this
    /// for Rule D's "process new reviews only" boundary and for the
    /// idempotency guards in rules A/B.
    pub recent_decisions: Vec<CoordinatorDecisionRow>,
    /// path → task_run_id of the session currently holding a blocking
    /// file lock on it. Sourced from `FileLockManager::info()`. Distinct
    /// from `active_file_registry` (advisory `FileRegistryManager`) —
    /// only the lock manager tells us "would assigning this task block
    /// the worker's first Edit."
    pub held_locks: HashMap<String, String>,
    /// Per-session aggregates over the heatmap window. Populated only
    /// when `PgDb::hot_sessions` / `FileLockManager::info` succeed —
    /// degrades to `HeatmapSnapshot::default()` on error so rules can
    /// read it unconditionally.
    pub heatmap: HeatmapSnapshot,
}

impl Observation {
    /// Stable SHA-256 hex of the observation snapshot. Used by the
    /// shadow-decisions feature to join shadow rows against live
    /// coordinator_decisions rows by "did both schedulers see the same
    /// world this tick?". Hash is over a deterministic, sorted
    /// representation so two observations with logically-equal contents
    /// produce identical hashes.
    ///
    /// Excludes `recent_decisions` and `recent_reviews` (audit trail —
    /// changes between ticks even when world-state doesn't), and the
    /// session ordering (sorted by task_run_id) so concurrent
    /// `list_active` permutations don't perturb the digest.
    pub fn observation_hash(&self) -> String {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();

        // Tasks: id + status + assigned_session_id + phase_name + sequence_in_phase.
        // Other columns can drift across reads (updated_at, error_message) without
        // changing what the rules see.
        let mut task_rows: Vec<(String, String, Option<String>, String, i32)> = self
            .tasks
            .iter()
            .map(|t| {
                (
                    t.id.clone(),
                    t.status.clone(),
                    t.assigned_session_id.clone(),
                    t.phase_name.clone(),
                    t.sequence_in_phase,
                )
            })
            .collect();
        task_rows.sort_by(|a, b| a.0.cmp(&b.0));
        hasher.update(b"tasks:");
        for (id, status, sess, phase, seq) in &task_rows {
            hasher.update(id.as_bytes());
            hasher.update(b"|");
            hasher.update(status.as_bytes());
            hasher.update(b"|");
            hasher.update(sess.as_deref().unwrap_or("").as_bytes());
            hasher.update(b"|");
            hasher.update(phase.as_bytes());
            hasher.update(b"|");
            hasher.update(seq.to_string().as_bytes());
            hasher.update(b";");
        }

        // Live sessions: sort by task_run_id, hash (id, state, assigned_task_id).
        let mut sess_rows: Vec<(String, Option<String>, Option<String>)> = self
            .live_sessions
            .iter()
            .map(|s| {
                (
                    s.task_run_id.clone(),
                    s.state.as_ref().map(|st| format!("{:?}", st)),
                    s.assigned_task_id.clone(),
                )
            })
            .collect();
        sess_rows.sort_by(|a, b| a.0.cmp(&b.0));
        hasher.update(b"sessions:");
        for (id, state, task) in &sess_rows {
            hasher.update(id.as_bytes());
            hasher.update(b"|");
            hasher.update(state.as_deref().unwrap_or("").as_bytes());
            hasher.update(b"|");
            hasher.update(task.as_deref().unwrap_or("").as_bytes());
            hasher.update(b";");
        }

        // Active file registry: sort by file_path. Hold by holder_task_run_id
        // (the FileRegistryInfo struct's actual field names — `path`/
        // `session_id` were earlier names that got renamed during the
        // worktree-claims refactor).
        let mut active: Vec<(String, String)> = self
            .active_file_registry
            .iter()
            .map(|r| (r.file_path.clone(), r.holder_task_run_id.clone()))
            .collect();
        active.sort();
        hasher.update(b"active_files:");
        for (p, s) in &active {
            hasher.update(p.as_bytes());
            hasher.update(b"|");
            hasher.update(s.as_bytes());
            hasher.update(b";");
        }

        // Upcoming file registry: sort by (task_id, path).
        let mut upcoming: Vec<(String, String)> = self
            .upcoming_file_registry
            .iter()
            .map(|c| (c.task_id.clone(), c.path.clone()))
            .collect();
        upcoming.sort();
        hasher.update(b"upcoming_files:");
        for (t, p) in &upcoming {
            hasher.update(t.as_bytes());
            hasher.update(b"|");
            hasher.update(p.as_bytes());
            hasher.update(b";");
        }

        // Open escalations count alone — content varies but the count is
        // what every rule consults.
        hasher.update(b"open_escalations_count:");
        hasher.update(self.open_escalations.len().to_string().as_bytes());

        // Held locks: sort by path so two observations with equal lock
        // state hash identically regardless of iteration order.
        let mut held: Vec<(&String, &String)> = self.held_locks.iter().collect();
        held.sort_by(|a, b| a.0.cmp(b.0));
        hasher.update(b"held_locks:");
        for (p, holder) in &held {
            hasher.update(p.as_bytes());
            hasher.update(b"|");
            hasher.update(holder.as_bytes());
            hasher.update(b";");
        }

        // Heatmap aggregates: sort by task_run_id for stable hashing.
        let mut breadth: Vec<(&String, &u64)> = self
            .heatmap
            .session_distinct_files
            .iter()
            .collect();
        breadth.sort_by(|a, b| a.0.cmp(b.0));
        hasher.update(b"heatmap_breadth:");
        for (id, n) in &breadth {
            hasher.update(id.as_bytes());
            hasher.update(b"|");
            hasher.update(n.to_string().as_bytes());
            hasher.update(b";");
        }
        let mut held_count: Vec<(&String, &u64)> =
            self.heatmap.session_held_count.iter().collect();
        held_count.sort_by(|a, b| a.0.cmp(b.0));
        hasher.update(b"heatmap_held:");
        for (id, n) in &held_count {
            hasher.update(id.as_bytes());
            hasher.update(b"|");
            hasher.update(n.to_string().as_bytes());
            hasher.update(b";");
        }

        format!("{:x}", hasher.finalize())
    }
}

impl Observation {
    /// True when there's still pending/ready/escalated work that none of
    /// the cheap rules took care of this iteration. Drives the LLM
    /// fallback in `coordinator::scheduler::run_one_iteration` (Phase 2).
    ///
    /// "Unhandled" here is a deliberately loose predicate — the cheap
    /// rules already ran first; if any of them fired we never call this.
    /// So the decision is just "is there something the user might
    /// reasonably expect us to make a call on?"
    pub(crate) fn has_unhandled_work(&self) -> bool {
        let pending_or_ready_tasks = self
            .tasks
            .iter()
            .any(|t| matches!(t.status.as_str(), "pending" | "ready" | "needs_fix"));
        let open_escalation = !self.open_escalations.is_empty();
        pending_or_ready_tasks || open_escalation
    }
}

/// Build one observation snapshot. Errors propagate as `String` —
/// `run_one_iteration` logs and continues.
pub(crate) async fn observe(state: &Arc<ApiState>) -> Result<Observation, String> {
    let app = state.app_state.clone();
    let pg = &app.pg_db;

    let tasks = pg
        .list_tasks_by_status(NON_TERMINAL_STATUSES)
        .await
        .map_err(|e| format!("list_tasks_by_status: {}", e))?;

    let active_file_registry = app.file_registry_manager.info().await;
    let upcoming_file_registry = app.upcoming_file_registry.snapshot().await;

    // Mirror the `LiveSessionSummary` collection in
    // `mcp::coordinator::get_coordinator_state`, but enrich with the
    // concrete `SessionState` and the assigned task id.
    let session_manager: Option<Arc<SessionManager>> = state
        .app_handle
        .try_state::<Arc<SessionManager>>()
        .map(|s| s.inner().clone());

    let assigned_task_by_session: HashMap<String, String> = tasks
        .iter()
        .filter_map(|t| {
            t.assigned_session_id
                .as_ref()
                .map(|sid| (sid.clone(), t.id.clone()))
        })
        .collect();

    // Phase 6 / Plan §Task 2 / Option 2c:
    // Workers go Ready → Processing on assign-task delivery
    // (`WorkerSession::send_user_message`). They go back to Ready when the
    // assigned task transitions to a terminal status (`done` | `cancelled`).
    // Walk the worker registry, look up each Processing worker's currently-
    // assigned task, and flip back to Ready when the task is in a terminal
    // state. Workers without an assigned task (never received assign-task)
    // are also flipped to Ready as a safety net so a stale Processing flag
    // doesn't strand a worker. Idempotent — runs once per observe tick
    // before the live_sessions snapshot is built.
    if let Some(sm) = session_manager.as_ref() {
        flip_idle_workers(sm, &tasks, &assigned_task_by_session);
    }

    let live_sessions = match session_manager {
        Some(sm) => sm
            .list_active()
            .into_iter()
            .map(|task_run_id| {
                let state = sm.get_state(&task_run_id);
                let assigned_task_id = assigned_task_by_session.get(&task_run_id).cloned();
                LiveSession {
                    task_run_id,
                    state,
                    assigned_task_id,
                }
            })
            .collect(),
        None => Vec::new(),
    };

    // Same one-hour lookback the slash command's Rule D consumes.
    let recent_reviews = pg
        .list_recent_reviews(3600, 50)
        .await
        .map_err(|e| format!("list_recent_reviews: {}", e))?;

    let open_escalations = pg
        .list_open_escalations()
        .await
        .map_err(|e| format!("list_open_escalations: {}", e))?;

    // Pre-fetch the touched-files index for every live session so Rule B
    // can do path overlap in pure code.
    let mut session_touched_files: HashMap<String, Vec<String>> = HashMap::new();
    for ls in &live_sessions {
        match pg.get_files_touched(&ls.task_run_id).await {
            Ok(files) => {
                if !files.is_empty() {
                    session_touched_files.insert(ls.task_run_id.clone(), files);
                }
            }
            Err(e) => {
                tracing::debug!(
                    "observe: get_files_touched failed for {}: {}",
                    ls.task_run_id,
                    e
                );
            }
        }
    }

    // Fixed-size context window: 50 most-recent decisions, no filter.
    // Rule-D and idempotency guards walk this in-memory rather than
    // reissuing per-rule queries.
    let recent_decisions = pg
        .list_recent_coordinator_decisions(50, None, None)
        .await
        .map_err(|e| format!("list_recent_coordinator_decisions: {}", e))?;

    // Held-lock snapshot: cheap in-memory read of the blocking lock map.
    // Rule B uses this to avoid assigning tasks whose expected_file_claims
    // would collide with a lock held by a non-holder candidate session.
    let mut held_locks: HashMap<String, String> = HashMap::new();
    let mut session_held_count: HashMap<String, u64> = HashMap::new();
    for entry in app.file_lock_manager.info().await {
        *session_held_count
            .entry(entry.holder_task_run_id.clone())
            .or_insert(0) += 1;
        held_locks.insert(entry.file_path, entry.holder_task_run_id);
    }

    // Heatmap breadth: per-session distinct-file count over the same
    // window the /file-activity/heatmap route exposes (default 3600s,
    // matching mcp/file_registry.rs:403). Plan §9.1 open question:
    // promote to Settings once telemetry justifies it. Limit 200 is
    // generous — there are rarely more than a handful of live sessions.
    let mut session_distinct_files: HashMap<String, u64> = HashMap::new();
    match pg.hot_sessions(3600, 200).await {
        Ok(rows) => {
            for row in rows {
                if row.distinct_files >= 0 {
                    session_distinct_files
                        .insert(row.task_run_id, row.distinct_files as u64);
                }
            }
        }
        Err(e) => {
            tracing::debug!("observe: hot_sessions failed: {}", e);
        }
    }

    let heatmap = HeatmapSnapshot {
        session_distinct_files,
        session_held_count,
    };

    Ok(Observation {
        tasks,
        active_file_registry,
        upcoming_file_registry,
        live_sessions,
        recent_reviews,
        open_escalations,
        session_touched_files,
        recent_decisions,
        held_locks,
        heatmap,
    })
}

/// Walk every registered worker; for each Processing worker decide whether
/// its assigned task has reached a terminal state and flip back to Ready.
/// See the call site for the full rationale (Phase 6 / Option 2c).
fn flip_idle_workers(
    sm: &SessionManager,
    tasks: &[TaskRow],
    assigned_task_by_session: &HashMap<String, String>,
) {
    // Build a quick lookup so we don't scan `tasks` twice per worker.
    let task_status_by_id: HashMap<&str, &str> = tasks
        .iter()
        .map(|t| (t.id.as_str(), t.status.as_str()))
        .collect();

    for task_run_id in sm.list_active() {
        let Some(worker) = sm.get_worker(&task_run_id) else {
            continue;
        };
        if worker.state() != SessionState::Processing {
            continue;
        }
        match assigned_task_by_session.get(&task_run_id) {
            Some(task_id) => {
                let status = task_status_by_id.get(task_id.as_str()).copied();
                let terminal = matches!(status, Some("done") | Some("cancelled") | None);
                if terminal {
                    tracing::debug!(
                        "observe: flipping worker {} Processing → Ready (task {} status={:?})",
                        task_run_id,
                        task_id,
                        status
                    );
                    worker.set_state(SessionState::Ready);
                }
            }
            None => {
                // Processing worker with no assigned task — safety net.
                tracing::debug!(
                    "observe: flipping orphaned Processing worker {} → Ready",
                    task_run_id
                );
                worker.set_state(SessionState::Ready);
            }
        }
    }
}

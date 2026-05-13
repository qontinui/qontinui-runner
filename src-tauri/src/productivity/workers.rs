//! Worker observability — Phase 6 §§ list_workers follow-up.
//!
//! Joins `SessionManager::list_workers` (the registry) with
//! `TerminalManager::list` (titles, in case the worker registration's
//! cached title is stale) and `PgDb::find_task_by_assigned_session` (the
//! coordinator's view of which task this worker is currently driving)
//! into a single structured payload.
//!
//! Two callers share this helper:
//! - `commands::productivity::list_workers` — the Tauri command surfaced
//!   to the React dashboard.
//! - `mcp::coordinator` `GET /workers` — the HTTP route used by external
//!   debugging tools and the UI Bridge invoke proxy.

use serde::Serialize;
use std::collections::HashMap;
use std::sync::Arc;
use tracing::warn;

use crate::claude_session::state::SessionState;
use crate::claude_session::SessionManager;
use crate::database::pg::PgDb;
use crate::terminal::TerminalManager;

/// One worker's observability snapshot. Field naming matches the rest of
/// the productivity stack (`camelCase` over the wire) so the dashboard can
/// consume the same shape from either the Tauri command or the HTTP route.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkerInfo {
    pub task_run_id: String,
    pub terminal_id: String,
    /// Live title from `TerminalManager::list`. `None` when the pty session
    /// has been removed from the manager (the worker registration outlived
    /// its pty — surfaces as a stale row the user can clean up).
    pub terminal_title: Option<String>,
    /// One of `"initializing"`, `"ready"`, `"processing"`, `"closed"`.
    /// Workers never logically enter the other `SessionState` variants;
    /// defensively map anything unexpected to `"closed"` so the dashboard
    /// doesn't have to handle a wider variant set.
    pub state: String,
    /// The `coord.tasks.id` (uuid string) currently assigned to this
    /// worker, if any. `None` when the worker is idle or when the lookup
    /// failed (best-effort — failures are logged and treated as no
    /// assignment so a transient PG hiccup doesn't blank the whole list).
    pub assigned_task_id: Option<String>,
    /// Unix epoch milliseconds. Source is `WorkerSession::created_at_unix`
    /// (seconds, `u64`); this multiplies by 1000 so the dashboard can
    /// pass it straight to `new Date(ms)`.
    pub created_at_ms: i64,
}

/// Map a `SessionState` to the wire-format string. Workers track
/// `Initializing → Ready ⇄ Processing → Closed`; any other variant falls
/// through to `"closed"` so the dashboard handles a closed pseudo-state
/// instead of rendering a foreign string.
fn state_string(state: SessionState) -> &'static str {
    match state {
        SessionState::Initializing => "initializing",
        SessionState::Ready => "ready",
        SessionState::Processing => "processing",
        SessionState::Closed => "closed",
        _ => "closed",
    }
}

/// Build a `Vec<WorkerInfo>` joining the worker registry with terminal
/// titles and per-worker task assignments. The result is ordered by worker
/// creation time (oldest first) — `SessionManager::list_workers` already
/// sorts by `created_at_unix`.
pub async fn build_worker_infos(
    session_manager: &Arc<SessionManager>,
    terminal_manager: &Arc<TerminalManager>,
    pg: &Arc<PgDb>,
) -> Vec<WorkerInfo> {
    // One pass through TerminalManager — small map, avoids an O(workers ×
    // terminals) scan in the loop below.
    let title_by_terminal: HashMap<String, String> = terminal_manager
        .list()
        .into_iter()
        .map(|info| (info.id, info.title))
        .collect();

    let workers = session_manager.list_workers();
    let mut out = Vec::with_capacity(workers.len());
    for (task_run_id, worker) in workers {
        let terminal_id = worker.terminal_id().to_string();
        let terminal_title = title_by_terminal.get(&terminal_id).cloned();

        // Best-effort task lookup: a transient PG error shouldn't blank
        // the whole list, so we log + treat as `None`.
        let assigned_task_id = match pg.find_task_by_assigned_session(&task_run_id).await {
            Ok(opt) => opt.map(|t| t.id),
            Err(e) => {
                warn!(
                    "build_worker_infos: find_task_by_assigned_session({}) failed: {}",
                    task_run_id, e
                );
                None
            }
        };

        out.push(WorkerInfo {
            task_run_id,
            terminal_id,
            terminal_title,
            state: state_string(worker.state()).to_string(),
            assigned_task_id,
            created_at_ms: (worker.created_at_unix() as i64).saturating_mul(1000),
        });
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_string_maps_supported_variants() {
        assert_eq!(state_string(SessionState::Initializing), "initializing");
        assert_eq!(state_string(SessionState::Ready), "ready");
        assert_eq!(state_string(SessionState::Processing), "processing");
        assert_eq!(state_string(SessionState::Closed), "closed");
        // Unsupported variants fall through to "closed" rather than
        // expanding the wire surface.
        assert_eq!(state_string(SessionState::Created), "closed");
        assert_eq!(state_string(SessionState::Interrupting), "closed");
        assert_eq!(state_string(SessionState::Promoting), "closed");
    }
}

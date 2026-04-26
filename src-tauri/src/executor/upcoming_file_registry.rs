//! Upcoming-file registry — pre-conflict layer for the productivity stack.
//!
//! Where the active [`crate::executor::file_registry::FileRegistryManager`]
//! tracks files a session is **currently** editing, this registry tracks files
//! a decomposed task **is expected** to edit in the future. Both surfaces are
//! advisory; the Coordinator queries the union when scheduling.
//!
//! See §3 of `qontinui-dev-notes/plans/productivity-stack.md`.
//!
//! ## Lifecycle
//!
//! - Populated by `/decompose-plan` via [`add_task_claims`].
//! - Drained when a task finishes (`done`/`cancelled`) or its plan is
//!   re-decomposed via [`drop_task_claims`] / [`drop_plan_claims`].
//! - Rehydrated on runner startup via [`rehydrate_from_pg`] from the rows
//!   in `tasks` whose status is non-terminal.

use crate::executor::file_registry::normalize_path;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

/// A single (plan, task, path) claim. The path is stored normalised so
/// queries can match against tokens written with mixed slashes/case
/// (matches the active registry's behaviour).
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpcomingClaim {
    pub plan_id: String,
    pub task_id: String,
    pub path: String,
    pub plan_version_hash: String,
    pub registered_at_ms: i64,
}

/// In-memory map from normalised path → list of (plan, task) claims.
///
/// A single path may be claimed by many upcoming tasks across many plans;
/// the registry intentionally permits overlap so the Coordinator can warn
/// rather than block.
#[derive(Debug, Clone, Default)]
pub struct UpcomingFileRegistry {
    by_path: Arc<RwLock<HashMap<String, Vec<UpcomingClaim>>>>,
}

impl UpcomingFileRegistry {
    /// Construct an empty registry.
    pub fn new() -> Self {
        Self {
            by_path: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Register expected claims for one task. Idempotent on
    /// `(plan_id, task_id, normalized_path)` so re-running `/decompose-plan`
    /// against an unchanged task is a no-op rather than an explosion of
    /// duplicate entries.
    ///
    /// Empty `paths` is a no-op.
    pub async fn add_task_claims(
        &self,
        plan_id: &str,
        task_id: &str,
        plan_version_hash: &str,
        paths: &[String],
    ) {
        if paths.is_empty() {
            return;
        }
        let now = now_millis();
        let mut state = self.by_path.write().await;

        for raw in paths {
            let path = normalize_path(raw);
            let entries = state.entry(path.clone()).or_default();

            // Idempotent — skip if (plan_id, task_id) already registered for
            // this path.
            if entries
                .iter()
                .any(|c| c.plan_id == plan_id && c.task_id == task_id)
            {
                continue;
            }

            entries.push(UpcomingClaim {
                plan_id: plan_id.to_string(),
                task_id: task_id.to_string(),
                path: path.clone(),
                plan_version_hash: plan_version_hash.to_string(),
                registered_at_ms: now,
            });
            debug!(
                "Upcoming claim: plan={} task={} path={}",
                plan_id, task_id, path
            );
        }
    }

    /// Drop all claims belonging to a single task. Called on
    /// `done`/`cancelled` transitions and on plan re-decomposition.
    pub async fn drop_task_claims(&self, plan_id: &str, task_id: &str) {
        let mut state = self.by_path.write().await;
        let mut removed = 0usize;
        for entries in state.values_mut() {
            let before = entries.len();
            entries.retain(|c| !(c.plan_id == plan_id && c.task_id == task_id));
            removed += before - entries.len();
        }
        state.retain(|_, v| !v.is_empty());
        if removed > 0 {
            debug!(
                "Dropped {} upcoming claim(s) for plan={} task={}",
                removed, plan_id, task_id
            );
        }
    }

    /// Drop every claim for a plan. Used on plan abandon or
    /// re-decomposition (the new task set will re-add fresh claims).
    pub async fn drop_plan_claims(&self, plan_id: &str) {
        let mut state = self.by_path.write().await;
        let mut removed = 0usize;
        for entries in state.values_mut() {
            let before = entries.len();
            entries.retain(|c| c.plan_id != plan_id);
            removed += before - entries.len();
        }
        state.retain(|_, v| !v.is_empty());
        if removed > 0 {
            info!("Dropped {} upcoming claim(s) for plan={}", removed, plan_id);
        }
    }

    /// All upcoming claimers for a single path. Empty Vec when the path is
    /// unclaimed.
    pub async fn claimers_for_path(&self, path: &str) -> Vec<UpcomingClaim> {
        let normalized = normalize_path(path);
        let state = self.by_path.read().await;
        state.get(&normalized).cloned().unwrap_or_default()
    }

    /// Bulk lookup: per-input-path list of claimers. Paths with no claims
    /// are present in the result with an empty Vec, so the caller can
    /// always indexed access.
    pub async fn check_paths(&self, paths: &[String]) -> HashMap<String, Vec<UpcomingClaim>> {
        let state = self.by_path.read().await;
        let mut out = HashMap::with_capacity(paths.len());
        for raw in paths {
            let normalized = normalize_path(raw);
            let claimers = state.get(&normalized).cloned().unwrap_or_default();
            out.insert(normalized, claimers);
        }
        out
    }

    /// Snapshot every claim in the registry. For the Tauri command and
    /// dashboard. Order is unspecified.
    pub async fn snapshot(&self) -> Vec<UpcomingClaim> {
        let state = self.by_path.read().await;
        state.values().flat_map(|v| v.iter().cloned()).collect()
    }

    /// Snapshot, filtered to a single plan. Cheap pre-filter to keep the
    /// dashboard's per-plan view from copying every claim across the wire.
    pub async fn snapshot_for_plan(&self, plan_id: &str) -> Vec<UpcomingClaim> {
        let state = self.by_path.read().await;
        state
            .values()
            .flat_map(|v| v.iter().filter(|c| c.plan_id == plan_id).cloned())
            .collect()
    }

    /// Rehydrate the registry from PG. Reads every task in a non-terminal
    /// status and replays its `expected_file_claims`.
    ///
    /// Errors during the read are logged and the registry stays empty —
    /// the registry is advisory, so it must not block runner startup.
    pub async fn rehydrate_from_pg(&self, db: &crate::database::pg::PgDb) {
        use crate::database::pg::tasks::NON_TERMINAL_STATUSES;

        let rows = match db.list_tasks_by_status(NON_TERMINAL_STATUSES).await {
            Ok(rows) => rows,
            Err(e) => {
                warn!(
                    "UpcomingFileRegistry rehydrate: failed to read tasks: {}",
                    e
                );
                return;
            }
        };

        let mut total = 0usize;
        for row in &rows {
            if row.expected_file_claims.is_empty() {
                continue;
            }
            self.add_task_claims(
                &row.plan_id,
                &row.id,
                &row.plan_version_hash,
                &row.expected_file_claims,
            )
            .await;
            total += row.expected_file_claims.len();
        }

        info!(
            "UpcomingFileRegistry rehydrated: {} task(s), {} claim(s)",
            rows.len(),
            total
        );
    }
}

fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh() -> UpcomingFileRegistry {
        UpcomingFileRegistry::new()
    }

    #[tokio::test]
    async fn add_and_lookup_single_claim() {
        let r = fresh();
        r.add_task_claims(
            "plan-1",
            "task-1",
            "hash-a",
            &["src/main.rs".into(), "src/lib.rs".into()],
        )
        .await;

        let snap = r.snapshot().await;
        assert_eq!(snap.len(), 2);

        let lookups = r.claimers_for_path("src/main.rs").await;
        assert_eq!(lookups.len(), 1);
        assert_eq!(lookups[0].task_id, "task-1");
    }

    #[tokio::test]
    async fn add_is_idempotent_per_task_path() {
        let r = fresh();
        r.add_task_claims("plan-1", "task-1", "hash-a", &["src/main.rs".into()])
            .await;
        r.add_task_claims("plan-1", "task-1", "hash-a", &["src/main.rs".into()])
            .await;

        assert_eq!(r.snapshot().await.len(), 1);
    }

    #[tokio::test]
    async fn multiple_tasks_can_claim_same_path() {
        let r = fresh();
        r.add_task_claims("plan-1", "task-A", "hash", &["shared.rs".into()])
            .await;
        r.add_task_claims("plan-1", "task-B", "hash", &["shared.rs".into()])
            .await;
        r.add_task_claims("plan-2", "task-X", "hash2", &["shared.rs".into()])
            .await;

        let claimers = r.claimers_for_path("shared.rs").await;
        assert_eq!(claimers.len(), 3);
    }

    #[tokio::test]
    async fn drop_task_claims_removes_only_that_task() {
        let r = fresh();
        r.add_task_claims("p", "task-A", "h", &["a.rs".into(), "b.rs".into()])
            .await;
        r.add_task_claims("p", "task-B", "h", &["b.rs".into()])
            .await;

        r.drop_task_claims("p", "task-A").await;

        let snap = r.snapshot().await;
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].task_id, "task-B");
        assert_eq!(snap[0].path, "b.rs");
    }

    #[tokio::test]
    async fn drop_plan_claims_cascades() {
        let r = fresh();
        r.add_task_claims("plan-1", "task-A", "h", &["a.rs".into()])
            .await;
        r.add_task_claims("plan-1", "task-B", "h", &["b.rs".into()])
            .await;
        r.add_task_claims("plan-2", "task-C", "h2", &["c.rs".into()])
            .await;

        r.drop_plan_claims("plan-1").await;

        let snap = r.snapshot().await;
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].plan_id, "plan-2");
    }

    #[tokio::test]
    async fn check_paths_returns_entry_per_input() {
        let r = fresh();
        r.add_task_claims("plan-1", "task-A", "h", &["a.rs".into()])
            .await;

        let result = r
            .check_paths(&["a.rs".into(), "b.rs".into(), "c.rs".into()])
            .await;
        assert_eq!(result.len(), 3);
        assert_eq!(result.get("a.rs").map(|v| v.len()), Some(1));
        assert_eq!(result.get("b.rs").map(|v| v.len()), Some(0));
        assert_eq!(result.get("c.rs").map(|v| v.len()), Some(0));
    }

    #[tokio::test]
    async fn snapshot_for_plan_filters() {
        let r = fresh();
        r.add_task_claims("plan-1", "task-A", "h", &["a.rs".into()])
            .await;
        r.add_task_claims("plan-2", "task-B", "h2", &["b.rs".into()])
            .await;

        let snap = r.snapshot_for_plan("plan-1").await;
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].plan_id, "plan-1");
    }

    #[tokio::test]
    async fn empty_paths_is_noop() {
        let r = fresh();
        r.add_task_claims("plan-1", "task-A", "h", &[]).await;
        assert!(r.snapshot().await.is_empty());
    }

    #[cfg(target_os = "windows")]
    #[tokio::test]
    async fn windows_path_normalisation_matches_lookups() {
        let r = fresh();
        r.add_task_claims("p", "t", "h", &["src\\Main.RS".into()])
            .await;
        let claimers = r.claimers_for_path("SRC/main.rs").await;
        assert_eq!(claimers.len(), 1);
    }
}

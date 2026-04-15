//! Step-Level Breakpoint Support
//!
//! Provides breakpoint snapshots and resume management for workflow debugging.
//! When a step is annotated with `breakpoint: true`, the executor captures a
//! snapshot of workflow state after the step completes and suspends execution
//! until an explicit resume signal.
//!
//! ## Storage Backends
//!
//! - **PostgreSQL** (default): Snapshots stored in `breakpoint_snapshots` table,
//!   resume detected via polling (500ms interval, matching existing pause pattern).
//! - **Restate** (when enabled): Snapshots ride on Restate's awakeable mechanism
//!   for durable suspension/resume.
//!
//! ## Excluded from Snapshots
//!
//! The following are explicitly excluded from snapshot capture:
//! - Open file handles (not serializable)
//! - Browser session handles (not transferable)
//! - GPU tensors (not serializable, too large)
//! - OS process state (PIDs, threads)

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::{info, warn};

use crate::commands::AppState;

/// Default staleness threshold in minutes.
/// Resuming a snapshot older than this triggers a warning.
const DEFAULT_STALENESS_THRESHOLD_MINUTES: i64 = 10;

/// Status of a breakpoint snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BreakpointStatus {
    /// Waiting for user to resume
    Waiting,
    /// User has resumed execution
    Resumed,
    /// Snapshot expired (timed out without resume)
    Expired,
}

impl std::fmt::Display for BreakpointStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Waiting => write!(f, "waiting"),
            Self::Resumed => write!(f, "resumed"),
            Self::Expired => write!(f, "expired"),
        }
    }
}

impl std::str::FromStr for BreakpointStatus {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "waiting" => Ok(Self::Waiting),
            "resumed" => Ok(Self::Resumed),
            "expired" => Ok(Self::Expired),
            _ => Err(format!("Unknown breakpoint status: {}", s)),
        }
    }
}

/// A snapshot captured at a breakpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BreakpointSnapshot {
    /// Unique ID (UUID)
    pub id: String,
    /// Task run ID this snapshot belongs to
    pub execution_id: String,
    /// Index of the step that just completed (0-based)
    pub step_index: usize,
    /// Human-readable step name
    pub step_name: Option<String>,
    /// Workflow phase (setup, verification, agentic, completion)
    pub phase: Option<String>,
    /// Iteration number for repeating phases
    pub iteration: Option<u32>,
    /// Serialized RuntimeContext + SharedVariableStore
    pub variables_json: String,
    /// Reference to last captured screenshot (path or URL)
    pub last_screenshot_ref: Option<String>,
    /// Serialized remaining ExecutionStepConfig[]
    pub pending_steps_json: String,
    /// When the snapshot was captured
    pub freshness_ts: DateTime<Utc>,
    /// Current status
    pub status: BreakpointStatus,
    /// When the snapshot was resumed (if applicable)
    pub resumed_at: Option<DateTime<Utc>>,
    /// When the record was created
    pub created_at: DateTime<Utc>,
}

/// Result of a staleness check.
#[derive(Debug)]
pub enum StalenessCheck {
    /// Snapshot is fresh enough
    Fresh,
    /// Snapshot is stale — includes age duration
    Stale(Duration),
}

/// Manages breakpoint snapshot lifecycle: save, wait, resume, staleness.
pub struct BreakpointManager {
    app_state: Arc<AppState>,
    staleness_threshold: Duration,
}

impl BreakpointManager {
    /// Create a new BreakpointManager with default staleness threshold.
    pub fn new(app_state: Arc<AppState>) -> Self {
        Self {
            app_state,
            staleness_threshold: Duration::minutes(DEFAULT_STALENESS_THRESHOLD_MINUTES),
        }
    }

    /// Create with a custom staleness threshold.
    pub fn with_staleness_threshold(app_state: Arc<AppState>, threshold_minutes: i64) -> Self {
        Self {
            app_state,
            staleness_threshold: Duration::minutes(threshold_minutes),
        }
    }

    /// Save a breakpoint snapshot to PostgreSQL.
    pub async fn save_snapshot(&self, snapshot: &BreakpointSnapshot) -> Result<String, String> {
        self.app_state
            .pg_db
            .save_breakpoint_snapshot(snapshot)
            .await
            .map(|_| snapshot.id.clone())
            .map_err(|e| format!("Failed to save breakpoint snapshot: {}", e))
    }

    /// Check whether Restate durable execution is enabled and healthy.
    ///
    /// Uses the same `should_use_restate` check as the rest of the runner.
    /// Returns `false` if settings cannot be loaded or Restate is down.
    async fn should_use_restate(&self) -> bool {
        let settings = crate::settings::load_settings().restate;
        crate::restate::launch::should_use_restate(&settings).await
    }

    /// Wait for resume using the Restate awakeable mechanism.
    ///
    /// 1. Creates an awakeable row in `restate_awakeables` with
    ///    `awakeable_type = "breakpoint"` and `type_data = snapshot_id`.
    /// 2. Polls the awakeable status until it is resolved (or the task is
    ///    stopped/cancelled).
    /// 3. On resolution, also updates the `breakpoint_snapshots` status to
    ///    `resumed` for consistency.
    async fn wait_for_resume_restate(
        &self,
        snapshot_id: &str,
        execution_id: &str,
    ) -> Result<(), String> {
        // Generate a stable awakeable ID derived from the snapshot so the resume
        // endpoint can locate it.  Format: "bp-<snapshot_id>"
        let awakeable_id = format!("bp-{}", snapshot_id);

        // Persist the awakeable row.
        self.app_state
            .pg_db
            .save_restate_awakeable(&awakeable_id, execution_id, "breakpoint", Some(snapshot_id))
            .await
            .map_err(|e| format!("Failed to create breakpoint awakeable: {}", e))?;

        info!(
            awakeable_id = %awakeable_id,
            snapshot_id = %snapshot_id,
            execution_id = %execution_id,
            "Breakpoint awakeable created — polling for resolution"
        );

        loop {
            tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

            // Check if task was stopped (takes priority)
            match self.app_state.pg_db.get_task_run(execution_id).await {
                Ok(Some(task)) if task.status == "stopped" || task.status == "cancelled" => {
                    info!(
                        "Task {} stopped while at breakpoint (restate path)",
                        execution_id
                    );
                    return Ok(());
                }
                _ => {}
            }

            // Check if the awakeable has been resolved
            match self
                .app_state
                .pg_db
                .find_pending_awakeable_by_type_data(execution_id, "breakpoint", snapshot_id)
                .await
            {
                Ok(Some(_)) => {
                    // Still pending — keep polling
                    continue;
                }
                Ok(None) => {
                    // No pending awakeable found — it was resolved (or never existed).
                    // Ensure the breakpoint snapshot is marked resumed too.
                    if let Err(e) = self
                        .app_state
                        .pg_db
                        .resume_breakpoint_snapshot(snapshot_id)
                        .await
                    {
                        // Best-effort: the snapshot may already be resumed via the
                        // normal API path, which is fine.
                        warn!(
                            "Could not mark breakpoint snapshot {} as resumed: {} (may already be resumed)",
                            snapshot_id, e
                        );
                    }
                    info!("Breakpoint {} resumed via Restate awakeable", snapshot_id);
                    return Ok(());
                }
                Err(e) => {
                    warn!(
                        "Error checking awakeable status: {} — continuing to poll",
                        e
                    );
                    continue;
                }
            }
        }
    }

    /// Wait for a resume signal by polling the snapshot status in PG.
    /// Returns when status changes from Waiting to Resumed.
    /// Also returns if the task is stopped (caller should handle).
    ///
    /// When Restate durable execution is enabled and healthy, delegates to
    /// the awakeable-based path (`wait_for_resume_restate`). Otherwise falls
    /// back to direct PG polling of the `breakpoint_snapshots` table.
    pub async fn wait_for_resume(
        &self,
        snapshot_id: &str,
        execution_id: &str,
    ) -> Result<(), String> {
        info!(
            snapshot_id = %snapshot_id,
            execution_id = %execution_id,
            "Breakpoint hit — waiting for resume signal"
        );

        // Delegate to Restate awakeable path when available.
        if self.should_use_restate().await {
            info!(
                "Restate enabled — using awakeable path for breakpoint {}",
                snapshot_id
            );
            return self
                .wait_for_resume_restate(snapshot_id, execution_id)
                .await;
        }

        // Fallback: PG polling
        loop {
            tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

            // Check if task was stopped (takes priority)
            match self.app_state.pg_db.get_task_run(execution_id).await {
                Ok(Some(task)) if task.status == "stopped" || task.status == "cancelled" => {
                    info!("Task {} stopped while at breakpoint", execution_id);
                    return Ok(());
                }
                _ => {}
            }

            // Check if snapshot was resumed
            match self
                .app_state
                .pg_db
                .get_breakpoint_snapshot(snapshot_id)
                .await
            {
                Ok(Some(snap)) => {
                    let status: BreakpointStatus =
                        snap.status.parse().unwrap_or(BreakpointStatus::Waiting);
                    match status {
                        BreakpointStatus::Resumed => {
                            info!("Breakpoint {} resumed", snapshot_id);
                            return Ok(());
                        }
                        BreakpointStatus::Expired => {
                            warn!("Breakpoint {} expired", snapshot_id);
                            return Err("Breakpoint snapshot expired".to_string());
                        }
                        BreakpointStatus::Waiting => continue,
                    }
                }
                Ok(None) => {
                    return Err(format!("Breakpoint snapshot {} not found", snapshot_id));
                }
                Err(e) => {
                    warn!(
                        "Error checking breakpoint status: {} — continuing to poll",
                        e
                    );
                    continue;
                }
            }
        }
    }

    /// Check if a snapshot is stale (older than the threshold).
    pub fn check_staleness(&self, snapshot: &BreakpointSnapshot) -> StalenessCheck {
        let age = Utc::now() - snapshot.freshness_ts;
        if age > self.staleness_threshold {
            StalenessCheck::Stale(age)
        } else {
            StalenessCheck::Fresh
        }
    }

    /// Build a snapshot from current executor state.
    pub fn build_snapshot(
        execution_id: &str,
        step_index: usize,
        step_name: Option<String>,
        phase: Option<String>,
        iteration: Option<u32>,
        variables_json: String,
        last_screenshot_ref: Option<String>,
        pending_steps_json: String,
    ) -> BreakpointSnapshot {
        let now = Utc::now();
        BreakpointSnapshot {
            id: uuid::Uuid::new_v4().to_string(),
            execution_id: execution_id.to_string(),
            step_index,
            step_name,
            phase,
            iteration,
            variables_json,
            last_screenshot_ref,
            pending_steps_json,
            freshness_ts: now,
            status: BreakpointStatus::Waiting,
            resumed_at: None,
            created_at: now,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_breakpoint_status_roundtrip() {
        for status in [
            BreakpointStatus::Waiting,
            BreakpointStatus::Resumed,
            BreakpointStatus::Expired,
        ] {
            let s = status.to_string();
            let parsed: BreakpointStatus = s.parse().unwrap();
            assert_eq!(parsed, status);
        }
    }

    #[test]
    fn test_snapshot_serialization() {
        let snapshot = BreakpointManager::build_snapshot(
            "test-exec-id",
            1,
            Some("Test Step".to_string()),
            Some("verification".to_string()),
            Some(0),
            r#"{"vars": {}}"#.to_string(),
            Some("/screenshots/test.png".to_string()),
            r#"[{"type": "command"}]"#.to_string(),
        );

        let json = serde_json::to_string(&snapshot).unwrap();
        let deserialized: BreakpointSnapshot = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.id, snapshot.id);
        assert_eq!(deserialized.execution_id, "test-exec-id");
        assert_eq!(deserialized.step_index, 1);
        assert_eq!(deserialized.step_name, Some("Test Step".to_string()));
        assert_eq!(deserialized.status, BreakpointStatus::Waiting);
    }

    #[test]
    fn test_staleness_fresh() {
        let snapshot = BreakpointManager::build_snapshot(
            "test",
            0,
            None,
            None,
            None,
            "{}".to_string(),
            None,
            "[]".to_string(),
        );
        // Can't easily test with real AppState, but we can test the check logic directly
        let age = Utc::now() - snapshot.freshness_ts;
        assert!(age < Duration::minutes(1));
    }

    #[test]
    fn test_staleness_stale() {
        let mut snapshot = BreakpointManager::build_snapshot(
            "test-stale",
            0,
            None,
            None,
            None,
            "{}".to_string(),
            None,
            "[]".to_string(),
        );
        // Backdate the freshness timestamp to 30 minutes ago (well past the 10-min threshold)
        snapshot.freshness_ts = Utc::now() - Duration::minutes(30);

        // Manually replicate check_staleness logic (we can't construct BreakpointManager
        // without AppState, but the logic is purely timestamp-based)
        let threshold = Duration::minutes(DEFAULT_STALENESS_THRESHOLD_MINUTES);
        let age = Utc::now() - snapshot.freshness_ts;
        assert!(
            age > threshold,
            "Expected stale: age {:?} should exceed threshold {:?}",
            age,
            threshold
        );
    }

    #[test]
    fn test_build_snapshot_fields() {
        let snapshot = BreakpointManager::build_snapshot(
            "exec-42",
            3,
            Some("Verify login".to_string()),
            Some("verification".to_string()),
            Some(2),
            r#"{"user": "admin"}"#.to_string(),
            Some("/tmp/screenshot.png".to_string()),
            r#"[{"name": "next_step"}]"#.to_string(),
        );

        assert_eq!(snapshot.execution_id, "exec-42");
        assert_eq!(snapshot.step_index, 3);
        assert_eq!(snapshot.step_name.as_deref(), Some("Verify login"));
        assert_eq!(snapshot.phase.as_deref(), Some("verification"));
        assert_eq!(snapshot.iteration, Some(2));
        assert_eq!(snapshot.variables_json, r#"{"user": "admin"}"#);
        assert_eq!(
            snapshot.last_screenshot_ref.as_deref(),
            Some("/tmp/screenshot.png")
        );
        assert_eq!(snapshot.pending_steps_json, r#"[{"name": "next_step"}]"#);
        assert_eq!(snapshot.status, BreakpointStatus::Waiting);
        assert!(snapshot.resumed_at.is_none());
        // ID should be a valid UUID (36 chars with hyphens)
        assert_eq!(snapshot.id.len(), 36);
        // freshness_ts and created_at should be very recent
        let age = Utc::now() - snapshot.freshness_ts;
        assert!(age < Duration::seconds(5));
        assert_eq!(snapshot.freshness_ts, snapshot.created_at);
    }

    #[test]
    fn test_breakpoint_status_display() {
        assert_eq!(BreakpointStatus::Waiting.to_string(), "waiting");
        assert_eq!(BreakpointStatus::Resumed.to_string(), "resumed");
        assert_eq!(BreakpointStatus::Expired.to_string(), "expired");
    }

    #[test]
    fn test_breakpoint_status_parse_invalid() {
        let result: Result<BreakpointStatus, _> = "invalid".parse();
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Unknown breakpoint status"));
    }
}

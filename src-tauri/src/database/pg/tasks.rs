//! PostgreSQL CRUD operations for the `tasks` table (migration v28).
//!
//! See §2 of `qontinui-dev-notes/plans/productivity-stack.md`. Each row is a
//! task decomposed from a plan, with file claims that populate the in-memory
//! `UpcomingFileRegistry`. Status follows the state machine documented in §2:
//!
//! ```text
//! pending  → ready (deps all done) → assigned (coordinator picks worker)
//!         → running (worker started) → review (worker emits done)
//! review   → done | needs_fix | escalated
//! needs_fix → assigned (re-assigned, bounded retries)
//! escalated → done | cancelled (user resolves)
//! ```
//!
//! UUID columns are cast to/from TEXT in queries because the runner does
//! not enable tokio-postgres `with-uuid-1`. UUIDs flow as canonical strings.

use super::PgDb;
use serde::{Deserialize, Serialize};

/// A row from the `tasks` table.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskRow {
    pub id: String,
    pub plan_id: String,
    pub plan_version_hash: String,
    pub phase_name: String,
    pub sequence_in_phase: i32,
    pub description: String,
    pub expected_file_claims: Vec<String>,
    pub expected_dirs: Vec<String>,
    pub depends_on: Vec<String>,
    pub status: String,
    pub assigned_session_id: Option<String>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub notes: Option<String>,
}

/// Input shape for `insert_task`. The DB allocates the `id`. `depends_on`
/// must be the UUIDs of already-inserted prereq tasks (resolved by the
/// caller — see `mcp::plans::POST /plans/decompose`).
#[derive(Debug, Clone)]
pub struct InsertTaskInput<'a> {
    pub plan_id: &'a str,
    pub plan_version_hash: &'a str,
    pub phase_name: &'a str,
    pub sequence_in_phase: i32,
    pub description: &'a str,
    pub expected_file_claims: &'a [String],
    pub expected_dirs: &'a [String],
    pub depends_on: &'a [String],
    pub status: &'a str,
    pub notes: Option<&'a str>,
}

/// Statuses considered non-terminal — these tasks still belong in the
/// `UpcomingFileRegistry` after a runner restart.
pub const NON_TERMINAL_STATUSES: &[&str] = &[
    "pending",
    "ready",
    "assigned",
    "running",
    "review",
    "needs_fix",
    "escalated",
];

fn task_row_from_pg(r: &tokio_postgres::Row) -> TaskRow {
    // `depends_on` is a UUID[] in PG. We round-trip via TEXT[] so a single
    // SELECT works regardless of with-uuid-1.
    let depends_on: Vec<String> = r.get(8);
    TaskRow {
        id: r.get(0),
        plan_id: r.get(1),
        plan_version_hash: r.get(2),
        phase_name: r.get(3),
        sequence_in_phase: r.get(4),
        description: r.get(5),
        expected_file_claims: r.get(6),
        expected_dirs: r.get(7),
        depends_on,
        status: r.get(9),
        assigned_session_id: r.get(10),
        started_at: r.get(11),
        completed_at: r.get(12),
        created_at: r.get(13),
        updated_at: r.get(14),
        notes: r.get(15),
    }
}

const SELECT_TASK_COLUMNS: &str = r#"
    id::text, plan_id::text, plan_version_hash, phase_name, sequence_in_phase,
    description, expected_file_claims, expected_dirs,
    (SELECT COALESCE(array_agg(d::text), '{}') FROM unnest(depends_on) d) AS depends_on_text,
    status, assigned_session_id, started_at::text, completed_at::text,
    created_at::text, updated_at::text, notes
"#;

impl PgDb {
    /// Insert a new task row. The DB allocates the UUID. Callers stitch
    /// `depends_on` from previously-inserted tasks' returned ids.
    pub async fn insert_task(&self, input: &InsertTaskInput<'_>) -> Result<TaskRow, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        // Deps come in as Vec<String>; cast to UUID[] inside the query.
        let depends_on_owned: Vec<String> = input.depends_on.to_vec();
        let row = conn
            .query_one(
                &format!(
                    r#"
                    INSERT INTO tasks (
                        plan_id, plan_version_hash, phase_name, sequence_in_phase,
                        description, expected_file_claims, expected_dirs,
                        depends_on, status, notes
                    )
                    VALUES (
                        $1::uuid, $2, $3, $4, $5, $6, $7,
                        (SELECT COALESCE(array_agg(d::uuid), '{{}}') FROM unnest($8::text[]) d),
                        $9, $10
                    )
                    RETURNING {}
                    "#,
                    SELECT_TASK_COLUMNS
                ),
                &[
                    &input.plan_id,
                    &input.plan_version_hash,
                    &input.phase_name,
                    &input.sequence_in_phase,
                    &input.description,
                    &input.expected_file_claims,
                    &input.expected_dirs,
                    &depends_on_owned,
                    &input.status,
                    &input.notes,
                ],
            )
            .await
            .map_err(|e| format!("Failed to insert task: {}", e))?;

        Ok(task_row_from_pg(&row))
    }

    /// Look up a single task by id.
    pub async fn get_task_by_id(&self, task_id: &str) -> Result<Option<TaskRow>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let row = conn
            .query_opt(
                &format!(
                    "SELECT {} FROM tasks WHERE id = $1::uuid",
                    SELECT_TASK_COLUMNS
                ),
                &[&task_id],
            )
            .await
            .map_err(|e| format!("Failed to get task: {}", e))?;

        Ok(row.as_ref().map(task_row_from_pg))
    }

    /// List all tasks for a plan, ordered by phase then sequence.
    pub async fn list_tasks_for_plan(&self, plan_id: &str) -> Result<Vec<TaskRow>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                &format!(
                    r#"
                    SELECT {}
                    FROM tasks
                    WHERE plan_id = $1::uuid
                    ORDER BY phase_name, sequence_in_phase
                    "#,
                    SELECT_TASK_COLUMNS
                ),
                &[&plan_id],
            )
            .await
            .map_err(|e| format!("Failed to list tasks for plan: {}", e))?;

        Ok(rows.iter().map(task_row_from_pg).collect())
    }

    /// List all tasks in non-terminal states for a plan. Useful for
    /// rendering the live workboard.
    pub async fn list_active_tasks_for_plan(&self, plan_id: &str) -> Result<Vec<TaskRow>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                &format!(
                    r#"
                    SELECT {}
                    FROM tasks
                    WHERE plan_id = $1::uuid
                      AND status = ANY($2::text[])
                    ORDER BY phase_name, sequence_in_phase
                    "#,
                    SELECT_TASK_COLUMNS
                ),
                &[&plan_id, &NON_TERMINAL_STATUSES],
            )
            .await
            .map_err(|e| format!("Failed to list active tasks: {}", e))?;

        Ok(rows.iter().map(task_row_from_pg).collect())
    }

    /// List all tasks across all plans matching one or more statuses.
    /// Used by `UpcomingFileRegistry::rehydrate_from_pg`.
    pub async fn list_tasks_by_status(&self, statuses: &[&str]) -> Result<Vec<TaskRow>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                &format!(
                    r#"
                    SELECT {}
                    FROM tasks
                    WHERE status = ANY($1::text[])
                    ORDER BY created_at ASC
                    "#,
                    SELECT_TASK_COLUMNS
                ),
                &[&statuses],
            )
            .await
            .map_err(|e| format!("Failed to list tasks by status: {}", e))?;

        Ok(rows.iter().map(task_row_from_pg).collect())
    }

    /// Flip pending → ready for any task in the given plan whose deps are
    /// all done. Returns the ids of newly-readied tasks.
    pub async fn mark_ready_for_unblocked(&self, plan_id: &str) -> Result<Vec<String>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        // A pending task is unblocked when every dep id is present in the
        // set of done-tasks in the same plan. Empty depends_on counts as
        // unblocked too.
        let rows = conn
            .query(
                r#"
                UPDATE tasks t
                SET status = 'ready', updated_at = NOW()
                WHERE t.plan_id = $1::uuid
                  AND t.status = 'pending'
                  AND NOT EXISTS (
                      SELECT 1 FROM unnest(t.depends_on) AS dep_id
                      WHERE NOT EXISTS (
                          SELECT 1 FROM tasks d
                          WHERE d.id = dep_id AND d.status = 'done'
                      )
                  )
                RETURNING id::text
                "#,
                &[&plan_id],
            )
            .await
            .map_err(|e| format!("Failed to mark tasks ready: {}", e))?;

        Ok(rows.iter().map(|r| r.get(0)).collect())
    }

    /// Assign a task to a session. Idempotent if the same session is already
    /// assigned. Returns true if the row updated.
    ///
    /// State transition implied: `ready → assigned`. Caller is responsible
    /// for calling [`PgDb::transition_task_status`] separately if a stricter
    /// transition is needed.
    pub async fn assign_task_to_session(
        &self,
        task_id: &str,
        session_id: &str,
    ) -> Result<bool, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let n = conn
            .execute(
                r#"
                UPDATE tasks
                SET assigned_session_id = $2,
                    status = CASE WHEN status IN ('ready', 'needs_fix')
                                  THEN 'assigned' ELSE status END,
                    updated_at = NOW()
                WHERE id = $1::uuid
                "#,
                &[&task_id, &session_id],
            )
            .await
            .map_err(|e| format!("Failed to assign task: {}", e))?;

        Ok(n > 0)
    }

    /// Atomic status transition with from→to validation.
    ///
    /// Refuses (returns `Ok(false)`) if the row's current `status` is not
    /// equal to `from`. This is the safe primitive callers should use to
    /// avoid lost-update races where two coordinators try to flip the same
    /// row.
    pub async fn transition_task_status(
        &self,
        task_id: &str,
        from: &str,
        to: &str,
    ) -> Result<bool, String> {
        if !is_valid_transition(from, to) {
            return Err(format!(
                "Illegal task status transition: {} -> {}",
                from, to
            ));
        }

        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        // Stamp started_at/completed_at on the canonical transitions.
        let sql = match to {
            "running" => {
                r#"
                UPDATE tasks
                SET status = $3,
                    started_at = COALESCE(started_at, NOW()),
                    updated_at = NOW()
                WHERE id = $1::uuid AND status = $2
                "#
            }
            "done" | "cancelled" => {
                r#"
                UPDATE tasks
                SET status = $3,
                    completed_at = NOW(),
                    updated_at = NOW()
                WHERE id = $1::uuid AND status = $2
                "#
            }
            _ => {
                r#"
                UPDATE tasks
                SET status = $3, updated_at = NOW()
                WHERE id = $1::uuid AND status = $2
                "#
            }
        };

        let n = conn
            .execute(sql, &[&task_id, &from, &to])
            .await
            .map_err(|e| format!("Failed to transition task status: {}", e))?;

        Ok(n > 0)
    }
}

/// Whitelist of legal `from -> to` transitions per §2 of the productivity
/// stack plan. Returning `false` means the transition is rejected upstream
/// before any DB work happens.
fn is_valid_transition(from: &str, to: &str) -> bool {
    matches!(
        (from, to),
        ("pending", "ready")
            | ("pending", "cancelled")
            | ("ready", "assigned")
            | ("ready", "cancelled")
            | ("assigned", "running")
            | ("assigned", "cancelled")
            | ("running", "review")
            | ("running", "cancelled")
            | ("review", "done")
            | ("review", "needs_fix")
            | ("review", "escalated")
            | ("needs_fix", "assigned")
            | ("needs_fix", "cancelled")
            | ("escalated", "done")
            | ("escalated", "cancelled")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_transitions_match_state_machine() {
        // Happy path
        assert!(is_valid_transition("pending", "ready"));
        assert!(is_valid_transition("ready", "assigned"));
        assert!(is_valid_transition("assigned", "running"));
        assert!(is_valid_transition("running", "review"));
        assert!(is_valid_transition("review", "done"));

        // Re-assign loop
        assert!(is_valid_transition("review", "needs_fix"));
        assert!(is_valid_transition("needs_fix", "assigned"));

        // Escalation
        assert!(is_valid_transition("review", "escalated"));
        assert!(is_valid_transition("escalated", "done"));
        assert!(is_valid_transition("escalated", "cancelled"));

        // Cancellation from any non-terminal state
        assert!(is_valid_transition("pending", "cancelled"));
        assert!(is_valid_transition("ready", "cancelled"));
        assert!(is_valid_transition("running", "cancelled"));

        // Illegal — terminal-state rebirth
        assert!(!is_valid_transition("done", "ready"));
        assert!(!is_valid_transition("cancelled", "ready"));

        // Illegal — skipping states
        assert!(!is_valid_transition("pending", "running"));
        assert!(!is_valid_transition("ready", "running"));
    }
}

//! Workflow event log — lightweight event-sourced execution log for DAG durable execution.
//!
//! Each workflow step appends an event (started, completed, failed, …) with a monotonically
//! increasing cursor. On crash recovery the executor replays from the latest cursor to
//! reconstruct which nodes have already completed, skipping re-execution of idempotent steps.

use super::optional_column::OptionalColumn;
use super::PgDb;
use serde::{Deserialize, Serialize};
use tracing::{error, warn};

/// `project.workflow_event_log.step_fingerprint` (alembic
/// `wf_resume_fingerprint_01`) may not exist in the live schema yet — the
/// runner can ship ahead of the migration's deploy. Every statement that names
/// it falls back to a column-free form on `42703`, so a missing column costs
/// replay VALIDATION (rows read back with no fingerprint, which is a MISS and
/// re-executes) rather than costing the journal append itself. Losing an append
/// would be strictly worse than the defect being fixed: an unrecorded
/// `completed` event guarantees re-execution AND re-billing on resume.
static EVENT_LOG_FINGERPRINT: OptionalColumn =
    OptionalColumn::new("project.workflow_event_log.step_fingerprint");

// ============================================================================
// Types
// ============================================================================

/// Event types for the workflow event log.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum EventType {
    Started,
    Completed,
    Failed,
    Skipped,
    Retried,
    Cancelled,
}

impl EventType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Started => "started",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Skipped => "skipped",
            Self::Retried => "retried",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "started" => Some(Self::Started),
            "completed" => Some(Self::Completed),
            "failed" => Some(Self::Failed),
            "skipped" => Some(Self::Skipped),
            "retried" => Some(Self::Retried),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }
}

/// A single event in the workflow event log.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowEvent {
    pub id: i64,
    pub execution_id: String,
    pub node_id: String,
    pub event_type: EventType,
    pub event_data: Option<serde_json::Value>,
    pub cursor: i64,
    pub created_at: String,
}

/// The outcome of a DAG replay lookup for one journal key.
///
/// A plain `Option` cannot distinguish "this node never completed here" from
/// "this node completed, but the work has since CHANGED". Both re-execute, but
/// only the second means an edit was honoured, and collapsing them in the logs
/// is how a silently-disabled replay path goes unnoticed.
#[derive(Debug, Clone, PartialEq)]
pub enum NodeReplay {
    /// A `completed` event exists AND its fingerprint matches: reuse the
    /// recorded output instead of executing.
    Hit(serde_json::Value),
    /// No usable `completed` event for this key.
    NoRow,
    /// A `completed` event exists but describes DIFFERENT work. `stored` is
    /// what the row carried (`None` when the row predates the fingerprint
    /// column, or was written against a schema without it).
    FingerprintMismatch { stored: Option<String> },
}

impl NodeReplay {
    /// The cached output, if this is a hit.
    pub fn hit(self) -> Option<serde_json::Value> {
        match self {
            NodeReplay::Hit(v) => Some(v),
            _ => None,
        }
    }
}

// ============================================================================
// Database operations
// ============================================================================

impl PgDb {
    /// Append an event to the log, returning the new cursor value.
    ///
    /// The cursor is a per-execution monotonically increasing integer computed
    /// as `MAX(cursor) + 1` inside the same transaction, so concurrent appends
    /// are serialised naturally by Postgres row-level locking on the index scan.
    ///
    /// `step_fingerprint` is the content hash of the inputs that determined the
    /// node's output (see [`crate::workflow_state::fingerprint`]). It is a
    /// NON-KEY validation column: the replay reader still locates the row by
    /// `(execution_id, node_id)` and then compares. `None` is written when the
    /// caller has none, and by contract that row is a cache MISS on replay —
    /// never a wildcard.
    pub async fn event_log_append(
        &self,
        execution_id: &str,
        node_id: &str,
        event_type: &EventType,
        event_data: Option<&serde_json::Value>,
        step_fingerprint: Option<&str>,
    ) -> Result<i64, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        // Serialise event_data to TEXT (consistent with other TEXT-blob columns).
        let event_data_str: Option<String> =
            event_data.map(|v| serde_json::to_string(v).unwrap_or_else(|_| "null".to_string()));

        let event_type_str = event_type.as_str();
        let fingerprint: Option<String> = step_fingerprint.map(str::to_string);

        let params: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = vec![
            &execution_id,
            &node_id,
            &event_type_str,
            &event_data_str,
            &fingerprint,
        ];

        // Derive the next cursor in-database to avoid races.
        let with_fingerprint = EVENT_LOG_FINGERPRINT.is_believed_present();
        let mut result = if with_fingerprint {
            conn.query_one(EVENT_LOG_APPEND_SQL_WITH_FINGERPRINT, &params)
                .await
        } else {
            conn.query_one(EVENT_LOG_APPEND_SQL, &params[..4]).await
        };
        if let Err(ref e) = result {
            if with_fingerprint && EVENT_LOG_FINGERPRINT.note_error(e) {
                // Live schema predates `wf_resume_fingerprint_01`. Retry
                // without the column: losing the append entirely would
                // guarantee re-execution and re-billing on resume.
                result = conn.query_one(EVENT_LOG_APPEND_SQL, &params[..4]).await;
            }
        }

        let row = result.map_err(|e| {
            error!(
                "event_log_append failed for execution {}: {}",
                execution_id, e
            );
            format!("Failed to append event log: {}", e)
        })?;

        Ok(row.get::<_, i64>(0))
    }

    /// Replay events from a given cursor position (inclusive).
    ///
    /// Returns events ordered by cursor ASC so callers can process them in
    /// emission order to reconstruct execution state.
    pub async fn event_log_replay_from(
        &self,
        execution_id: &str,
        from_cursor: i64,
    ) -> Result<Vec<WorkflowEvent>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                r#"
                SELECT id, execution_id, node_id, event_type, event_data, cursor, created_at::TEXT
                FROM workflow_event_log
                WHERE execution_id = $1 AND cursor >= $2
                ORDER BY cursor ASC
                "#,
                &[&execution_id, &from_cursor],
            )
            .await
            .map_err(|e| {
                error!(
                    "event_log_replay_from failed for execution {}: {}",
                    execution_id, e
                );
                format!("Failed to replay event log: {}", e)
            })?;

        let events = rows
            .iter()
            .map(|r| {
                let event_type_str: String = r.get(3);
                let event_data_raw: Option<String> = r.get(4);
                let event_data = event_data_raw.and_then(|s| {
                    serde_json::from_str(&s)
                        .map_err(|e| {
                            warn!("Failed to parse event_data JSON: {}", e);
                            e
                        })
                        .ok()
                });
                WorkflowEvent {
                    id: r.get::<_, i64>(0),
                    execution_id: r.get::<_, String>(1),
                    node_id: r.get::<_, String>(2),
                    event_type: EventType::from_str(&event_type_str).unwrap_or(EventType::Started),
                    event_data,
                    cursor: r.get::<_, i64>(5),
                    created_at: r.get::<_, String>(6),
                }
            })
            .collect();

        Ok(events)
    }

    /// Check if a node has a completed event whose fingerprint still matches
    /// the work about to run, returning its output data if so.
    ///
    /// Used during crash-recovery replay: [`NodeReplay::Hit`] means the node
    /// can be skipped (its output is already persisted).
    ///
    /// # The NULL contract
    ///
    /// The comparison is plain equality on the supplied value, done in Rust on
    /// the already-located row. A stored fingerprint that is NULL, empty, or
    /// different is a MISS. It is deliberately NOT written as
    /// `step_fingerprint IS NULL OR step_fingerprint = $3` — that is the
    /// natural SQL instinct and it reads a NULL as "matches anything", which
    /// would serve exactly the stale cached outputs the column exists to
    /// prevent. An empty `expected_fingerprint` is likewise a miss, so a caller
    /// that could not compute one re-executes rather than replaying blind.
    pub async fn event_log_node_completed(
        &self,
        execution_id: &str,
        node_id: &str,
        expected_fingerprint: &str,
    ) -> Result<NodeReplay, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let params: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = vec![&execution_id, &node_id];

        let mut with_fingerprint = EVENT_LOG_FINGERPRINT.is_believed_present();
        let mut result = conn
            .query_opt(node_completed_sql(with_fingerprint), &params)
            .await;
        if let Err(ref e) = result {
            if with_fingerprint && EVENT_LOG_FINGERPRINT.note_error(e) {
                with_fingerprint = false;
                result = conn.query_opt(node_completed_sql(false), &params).await;
            }
        }

        let row = result.map_err(|e| {
            error!(
                "event_log_node_completed failed for execution {}, node {}: {}",
                execution_id, node_id, e
            );
            format!("Failed to query node completion: {}", e)
        })?;

        let Some(r) = row else {
            return Ok(NodeReplay::NoRow);
        };

        // Fingerprint first: a row that describes different work must not be
        // reported as "no row", because those two have different meanings for
        // an operator reading the logs.
        let stored: Option<String> = if with_fingerprint {
            r.try_get::<_, Option<String>>(1)
                .unwrap_or_else(|e| {
                    warn!("step_fingerprint decode drift: {}", e);
                    None
                })
                .filter(|f| !f.is_empty())
        } else {
            None
        };
        if expected_fingerprint.is_empty() || stored.as_deref() != Some(expected_fingerprint) {
            return Ok(NodeReplay::FingerprintMismatch { stored });
        }

        let raw: Option<String> = r.get(0);
        Ok(raw
            .and_then(|s| {
                serde_json::from_str(&s)
                    .map_err(|e| {
                        warn!("Failed to parse node completed event_data: {}", e);
                        e
                    })
                    .ok()
            })
            .map(NodeReplay::Hit)
            .unwrap_or(NodeReplay::NoRow))
    }

    /// Prune events before a cursor (inclusive of `before_cursor - 1`) for a
    /// **single node scope** within an execution.
    ///
    /// The scope is `node_id` itself plus every journal key nested under it
    /// (`"<node_id>/…"`) — that nesting is how `dag_driver` keys loop-body
    /// executions (`"<loop_id>/iter<N>/<body_id>"`), so a loop checkpoint can
    /// discard its own superseded iterations without touching any other node's
    /// rows.
    ///
    /// Scoping matters for correctness, not just tidiness: an
    /// execution-wide prune deletes the `completed` records of every *sibling*
    /// node too, so a crash after one checkpoint would re-execute — and
    /// re-bill — the entire upstream DAG.
    ///
    /// The prefix match is spelled with `substr(...) = $2 || '/'` rather than
    /// `LIKE $2 || '/%'` on purpose: `node_id` is workflow-authored text and
    /// `LIKE` would treat any `_` or `%` inside it as a wildcard, silently
    /// widening the delete to sibling nodes.
    ///
    /// Returns the number of rows deleted.
    pub async fn event_log_prune_before(
        &self,
        execution_id: &str,
        node_id: &str,
        before_cursor: i64,
    ) -> Result<u64, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let affected = conn
            .execute(
                r#"
                DELETE FROM workflow_event_log
                WHERE execution_id = $1
                  AND (node_id = $2 OR substr(node_id, 1, length($2) + 1) = $2 || '/')
                  AND cursor < $3
                "#,
                &[&execution_id, &node_id, &before_cursor],
            )
            .await
            .map_err(|e| {
                error!(
                    "event_log_prune_before failed for execution {}, node {}: {}",
                    execution_id, node_id, e
                );
                format!("Failed to prune event log: {}", e)
            })?;

        Ok(affected)
    }

    /// Get the latest cursor for an execution (for resume position on restart).
    ///
    /// Returns 0 if no events exist yet (fresh execution).
    pub async fn event_log_latest_cursor(&self, execution_id: &str) -> Result<i64, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let row = conn
            .query_one(
                "SELECT COALESCE(MAX(cursor), 0) FROM workflow_event_log WHERE execution_id = $1",
                &[&execution_id],
            )
            .await
            .map_err(|e| {
                error!(
                    "event_log_latest_cursor failed for execution {}: {}",
                    execution_id, e
                );
                format!("Failed to get latest cursor: {}", e)
            })?;

        Ok(row.get::<_, i64>(0))
    }

    /// Delete all events for an execution (cleanup after completion or cancellation).
    ///
    /// Returns the number of rows deleted.
    pub async fn event_log_delete_execution(&self, execution_id: &str) -> Result<u64, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let affected = conn
            .execute(
                "DELETE FROM workflow_event_log WHERE execution_id = $1",
                &[&execution_id],
            )
            .await
            .map_err(|e| {
                error!(
                    "event_log_delete_execution failed for execution {}: {}",
                    execution_id, e
                );
                format!("Failed to delete execution events: {}", e)
            })?;

        Ok(affected)
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ── Pure unit tests (always run — no DB, so they cannot be a false green) ──

    #[test]
    fn event_type_round_trips_through_str() {
        for et in [
            EventType::Started,
            EventType::Completed,
            EventType::Failed,
            EventType::Skipped,
            EventType::Retried,
            EventType::Cancelled,
        ] {
            let s = et.as_str();
            assert_eq!(
                EventType::from_str(s),
                Some(et.clone()),
                "{} did not round-trip",
                s
            );
        }
        assert_eq!(EventType::from_str("not_an_event"), None);
    }

    /// The replay predicate keys on the literal string `"completed"`; if
    /// `as_str` ever drifts, `event_log_node_completed`'s hard-coded
    /// `event_type = 'completed'` silently stops matching and every node
    /// re-executes on resume.
    #[test]
    fn completed_serialises_to_the_literal_the_replay_query_matches() {
        assert_eq!(EventType::Completed.as_str(), "completed");
    }

    // ── PG-gated integration tests ───────────────────────────────────────────
    //
    // These follow the repo convention for `database/pg/*`: `#[ignore]` with an
    // explicit reason, so they are NOT run (and cannot report a vacuous green)
    // unless deliberately selected:
    //
    //   cargo test --lib database::pg::event_log -- --ignored --nocapture
    //
    // Every assertion below is an unconditional `assert*` — there is no
    // in-test "skip if no DB" arm, so a missing database fails the test loudly
    // rather than passing empty.

    /// Fingerprint every fixture row is written with. A replay lookup is
    /// fingerprint-gated, so appending rows WITHOUT one would make every
    /// assertion below miss for the wrong reason and pass vacuously.
    const TEST_FP: &str = "sf1:event-log-test-fingerprint";

    async fn test_db() -> PgDb {
        // Deliberately NOT `PgDb::new_blocking_for_test()`: that spins its own
        // tokio runtime and `block_on` panics when called from inside the
        // `#[tokio::test]` runtime.
        let url = std::env::var("DATABASE_URL")
            .unwrap_or_else(|_| "postgres://localhost:5432/qontinui_test".to_string());
        PgDb::new(&url)
            .await
            .expect("PgDb connection for test (set DATABASE_URL)")
    }

    /// Unique per test so concurrent runs on a shared PG never collide.
    fn unique_execution_id(label: &str) -> String {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        format!(
            "test-evlog-{}-{}-{:?}",
            label,
            nanos,
            std::thread::current().id()
        )
    }

    /// `workflow_event_log.execution_id` has an FK onto `task_runs(id)`, so an
    /// execution row must exist before any event can be appended.
    async fn seed_execution(db: &PgDb, execution_id: &str) {
        let conn = db.pool.get().await.expect("PG pool for seed");
        conn.execute(
            "INSERT INTO task_runs (id, task_name) VALUES ($1, $2) ON CONFLICT (id) DO NOTHING",
            &[&execution_id, &"event-log-test"],
        )
        .await
        .expect("seed task_runs row");
    }

    /// `ON DELETE CASCADE` on the FK takes the event rows with it.
    async fn drop_execution(db: &PgDb, execution_id: &str) {
        let conn = db.pool.get().await.expect("PG pool for cleanup");
        conn.execute("DELETE FROM task_runs WHERE id = $1", &[&execution_id])
            .await
            .expect("cleanup task_runs row");
    }

    #[tokio::test]
    #[ignore = "requires PG via DATABASE_URL"]
    async fn append_assigns_monotonic_cursors_and_replay_returns_them_in_order() {
        let db = test_db().await;
        let exec = unique_execution_id("append");
        seed_execution(&db, &exec).await;

        let c1 = db
            .event_log_append(&exec, "node-a", &EventType::Started, None, Some(TEST_FP))
            .await
            .expect("append started");
        let c2 = db
            .event_log_append(
                &exec,
                "node-a",
                &EventType::Completed,
                Some(&serde_json::json!({ "output": { "value": 7 } })),
                Some(TEST_FP),
            )
            .await
            .expect("append completed");
        assert_eq!(c1, 1, "first append starts the per-execution cursor at 1");
        assert_eq!(c2, 2, "cursor increments per execution");

        let events = db
            .event_log_replay_from(&exec, 0)
            .await
            .expect("replay from 0");
        assert_eq!(events.len(), 2, "both appended events are replayed");
        assert_eq!(events[0].cursor, 1);
        assert_eq!(events[0].event_type, EventType::Started);
        assert_eq!(events[1].event_type, EventType::Completed);
        assert_eq!(
            events[1].event_data.as_ref().and_then(|d| d.get("output")),
            Some(&serde_json::json!({ "value": 7 })),
            "event_data survives the TEXT round-trip"
        );

        let tail = db
            .event_log_replay_from(&exec, 2)
            .await
            .expect("replay from 2");
        assert_eq!(tail.len(), 1, "from_cursor is inclusive and filters");

        assert_eq!(
            db.event_log_latest_cursor(&exec)
                .await
                .expect("latest cursor"),
            2
        );

        drop_execution(&db, &exec).await;
    }

    #[tokio::test]
    #[ignore = "requires PG via DATABASE_URL"]
    async fn node_completed_reads_back_the_latest_completion_only() {
        let db = test_db().await;
        let exec = unique_execution_id("completed");
        seed_execution(&db, &exec).await;

        assert!(
            db.event_log_node_completed(&exec, "node-a", TEST_FP)
                .await
                .expect("query uncompleted node")
                == NodeReplay::NoRow,
            "a node with no events has no completion record"
        );

        db.event_log_append(&exec, "node-a", &EventType::Started, None, Some(TEST_FP))
            .await
            .expect("append started");
        assert!(
            db.event_log_node_completed(&exec, "node-a", TEST_FP)
                .await
                .expect("query started-only node")
                == NodeReplay::NoRow,
            "a `started` event must NOT be mistaken for a completion"
        );

        db.event_log_append(
            &exec,
            "node-a",
            &EventType::Completed,
            Some(&serde_json::json!({ "output": "first" })),
            Some(TEST_FP),
        )
        .await
        .expect("append first completion");
        db.event_log_append(
            &exec,
            "node-a",
            &EventType::Completed,
            Some(&serde_json::json!({ "output": "second" })),
            Some(TEST_FP),
        )
        .await
        .expect("append second completion");

        let cached = db
            .event_log_node_completed(&exec, "node-a", TEST_FP)
            .await
            .expect("query completed node")
            .hit()
            .expect("completion record present");
        assert_eq!(
            cached.get("output"),
            Some(&serde_json::json!("second")),
            "the highest-cursor completion wins"
        );

        // Completion is keyed on (execution_id, node_id) — a sibling node is
        // not covered by node-a's completion.
        assert!(
            db.event_log_node_completed(&exec, "node-b", TEST_FP)
                .await
                .expect("query sibling")
                == NodeReplay::NoRow
        );

        drop_execution(&db, &exec).await;
    }

    /// The regression this phase exists to prevent: `execute_loop_node` prunes
    /// at every `commit_interval`, and while that prune was scoped by
    /// `execution_id` alone it deleted every *sibling* node's `completed`
    /// record too — so a crash after one checkpoint re-executed (and re-billed)
    /// the whole upstream DAG.
    #[tokio::test]
    #[ignore = "requires PG via DATABASE_URL"]
    async fn prune_is_scoped_to_the_loop_node_and_siblings_survive() {
        let db = test_db().await;
        let exec = unique_execution_id("prune-scope");
        seed_execution(&db, &exec).await;

        // An upstream sibling that already completed — must survive.
        db.event_log_append(
            &exec,
            "upstream",
            &EventType::Completed,
            Some(&serde_json::json!({ "output": "expensive" })),
            Some(TEST_FP),
        )
        .await
        .expect("append upstream completion");

        // Two loop-body iterations, keyed the way dag_driver keys them.
        db.event_log_append(
            &exec,
            "loop-a/iter0/body",
            &EventType::Completed,
            Some(&serde_json::json!({ "output": 0 })),
            Some(TEST_FP),
        )
        .await
        .expect("append iter0");
        db.event_log_append(
            &exec,
            "loop-a/iter1/body",
            &EventType::Completed,
            Some(&serde_json::json!({ "output": 1 })),
            Some(TEST_FP),
        )
        .await
        .expect("append iter1");

        // A sibling whose id *shares a prefix* with the loop node. It must NOT
        // be swept up: `loop-a-other` is not nested under `loop-a/`.
        db.event_log_append(
            &exec,
            "loop-a-other",
            &EventType::Completed,
            Some(&serde_json::json!({ "output": "sibling" })),
            Some(TEST_FP),
        )
        .await
        .expect("append prefix-sharing sibling");

        let cursor = db
            .event_log_latest_cursor(&exec)
            .await
            .expect("latest cursor");
        assert_eq!(cursor, 4, "four events appended");

        let deleted = db
            .event_log_prune_before(&exec, "loop-a", cursor)
            .await
            .expect("prune loop-a");
        assert_eq!(
            deleted, 2,
            "only the loop's own nested rows are pruned (both iterations sit below the cursor)"
        );

        assert_eq!(
            db.event_log_node_completed(&exec, "upstream", TEST_FP)
                .await
                .expect("query upstream")
                .hit()
                .and_then(|v| v.get("output").cloned()),
            Some(serde_json::json!("expensive")),
            "SIBLING COMPLETION MUST SURVIVE a loop-node prune"
        );
        assert_eq!(
            db.event_log_node_completed(&exec, "loop-a-other", TEST_FP)
                .await
                .expect("query prefix-sharing sibling")
                .hit()
                .and_then(|v| v.get("output").cloned()),
            Some(serde_json::json!("sibling")),
            "prefix-sharing sibling is not nested under `loop-a/` and must survive"
        );
        assert!(
            db.event_log_node_completed(&exec, "loop-a/iter0/body", TEST_FP)
                .await
                .expect("query pruned iteration")
                == NodeReplay::NoRow,
            "the loop's own superseded iterations are gone"
        );

        drop_execution(&db, &exec).await;
    }

    /// `node_id` is workflow-authored text. A `LIKE`-based prefix match would
    /// read `_` as a single-character wildcard and delete a sibling's rows.
    #[tokio::test]
    #[ignore = "requires PG via DATABASE_URL"]
    async fn prune_prefix_match_does_not_treat_node_id_as_a_wildcard_pattern() {
        let db = test_db().await;
        let exec = unique_execution_id("prune-wildcard");
        seed_execution(&db, &exec).await;

        // Under `LIKE 'a_b/%'` the `_` matches any character, so `axb/iter0`
        // would be deleted along with `a_b/iter0`.
        db.event_log_append(
            &exec,
            "a_b/iter0",
            &EventType::Completed,
            None,
            Some(TEST_FP),
        )
        .await
        .expect("append underscore node");
        db.event_log_append(
            &exec,
            "axb/iter0",
            &EventType::Completed,
            None,
            Some(TEST_FP),
        )
        .await
        .expect("append wildcard-collision node");
        db.event_log_append(
            &exec,
            "pct/iter0",
            &EventType::Completed,
            None,
            Some(TEST_FP),
        )
        .await
        .expect("append unrelated node");

        let cursor = db
            .event_log_latest_cursor(&exec)
            .await
            .expect("latest cursor");
        let deleted = db
            .event_log_prune_before(&exec, "a_b", cursor)
            .await
            .expect("prune a_b");
        assert_eq!(deleted, 1, "exactly the `a_b/` subtree is pruned");

        let survivors = db
            .event_log_replay_from(&exec, 0)
            .await
            .expect("replay survivors");
        let ids: Vec<&str> = survivors.iter().map(|e| e.node_id.as_str()).collect();
        assert!(
            ids.contains(&"axb/iter0"),
            "`_` must not act as a wildcard; got {:?}",
            ids
        );
        assert!(ids.contains(&"pct/iter0"), "unrelated node survives");
        assert!(!ids.contains(&"a_b/iter0"));

        drop_execution(&db, &exec).await;
    }

    #[tokio::test]
    #[ignore = "requires PG via DATABASE_URL"]
    async fn prune_keeps_rows_at_or_after_the_cursor() {
        let db = test_db().await;
        let exec = unique_execution_id("prune-cursor");
        seed_execution(&db, &exec).await;

        db.event_log_append(
            &exec,
            "loop-a/iter0/body",
            &EventType::Completed,
            None,
            Some(TEST_FP),
        )
        .await
        .expect("append iter0");
        let keep_from = db
            .event_log_append(
                &exec,
                "loop-a/iter1/body",
                &EventType::Completed,
                None,
                Some(TEST_FP),
            )
            .await
            .expect("append iter1");

        let deleted = db
            .event_log_prune_before(&exec, "loop-a", keep_from)
            .await
            .expect("prune below cursor");
        assert_eq!(deleted, 1, "only rows strictly below the cursor are pruned");
        assert!(
            db.event_log_node_completed(&exec, "loop-a/iter1/body", TEST_FP)
                .await
                .expect("query kept row")
                .hit()
                .is_some(),
            "the checkpoint row itself is retained"
        );

        drop_execution(&db, &exec).await;
    }

    #[tokio::test]
    #[ignore = "requires PG via DATABASE_URL"]
    async fn delete_execution_removes_every_node_and_latest_cursor_resets() {
        let db = test_db().await;
        let exec = unique_execution_id("delete");
        seed_execution(&db, &exec).await;

        db.event_log_append(&exec, "node-a", &EventType::Completed, None, Some(TEST_FP))
            .await
            .expect("append a");
        db.event_log_append(&exec, "node-b", &EventType::Failed, None, Some(TEST_FP))
            .await
            .expect("append b");

        let deleted = db
            .event_log_delete_execution(&exec)
            .await
            .expect("delete execution");
        assert_eq!(deleted, 2);
        assert_eq!(
            db.event_log_latest_cursor(&exec)
                .await
                .expect("latest cursor after delete"),
            0,
            "a drained execution reports cursor 0"
        );

        drop_execution(&db, &exec).await;
    }

    // ── step_fingerprint round-trip (Phase 3b) ──────────────────────────────

    /// The fingerprint survives the append/read round-trip, and an UNCHANGED
    /// step still replays. Without this the "changed" tests below could pass
    /// because nothing ever hits.
    #[tokio::test]
    #[ignore = "requires PG via DATABASE_URL"]
    async fn node_completed_hits_when_the_fingerprint_round_trips() {
        let db = test_db().await;
        let exec = unique_execution_id("fp-hit");
        seed_execution(&db, &exec).await;

        db.event_log_append(
            &exec,
            "node-a",
            &EventType::Completed,
            Some(&serde_json::json!({ "output": "cached" })),
            Some(TEST_FP),
        )
        .await
        .expect("append completion with fingerprint");

        assert_eq!(
            db.event_log_node_completed(&exec, "node-a", TEST_FP)
                .await
                .expect("query")
                .hit()
                .and_then(|v| v.get("output").cloned()),
            Some(serde_json::json!("cached")),
            "an unchanged step must still replay"
        );

        drop_execution(&db, &exec).await;
    }

    /// The defect this phase closes: the row is in the right slice, but the
    /// prompt/model/definition changed, so the cached output must NOT be
    /// served — and the miss must be reported as a fingerprint mismatch, not
    /// as "no row".
    #[tokio::test]
    #[ignore = "requires PG via DATABASE_URL"]
    async fn node_completed_misses_when_the_fingerprint_changed() {
        let db = test_db().await;
        let exec = unique_execution_id("fp-changed");
        seed_execution(&db, &exec).await;

        db.event_log_append(
            &exec,
            "node-a",
            &EventType::Completed,
            Some(&serde_json::json!({ "output": "stale" })),
            Some(TEST_FP),
        )
        .await
        .expect("append completion");

        assert_eq!(
            db.event_log_node_completed(&exec, "node-a", "sf1:edited-prompt")
                .await
                .expect("query with a different fingerprint"),
            NodeReplay::FingerprintMismatch {
                stored: Some(TEST_FP.to_string())
            },
            "an edited node must re-execute, distinguishably from 'never ran'"
        );

        drop_execution(&db, &exec).await;
    }

    /// **NULL is a MISS, never a wildcard.** Rows written before
    /// `wf_resume_fingerprint_01` — and rows written by a runner talking to a
    /// database that has not deployed the migration — carry no fingerprint.
    /// The SQL instinct `step_fingerprint IS NULL OR step_fingerprint = $3`
    /// would replay them, which is exactly the stale hit this column prevents.
    #[tokio::test]
    #[ignore = "requires PG via DATABASE_URL"]
    async fn node_completed_treats_a_null_fingerprint_as_a_miss() {
        let db = test_db().await;
        let exec = unique_execution_id("fp-null");
        seed_execution(&db, &exec).await;

        db.event_log_append(
            &exec,
            "node-a",
            &EventType::Completed,
            Some(&serde_json::json!({ "output": "legacy" })),
            None,
        )
        .await
        .expect("append legacy completion with no fingerprint");

        assert_eq!(
            db.event_log_node_completed(&exec, "node-a", TEST_FP)
                .await
                .expect("query legacy row"),
            NodeReplay::FingerprintMismatch { stored: None },
            "a NULL fingerprint must never match"
        );

        // ...and an empty expectation is the same wildcard by another
        // spelling: it must not match either.
        assert_eq!(
            db.event_log_node_completed(&exec, "node-a", "")
                .await
                .expect("query legacy row with an empty expectation"),
            NodeReplay::FingerprintMismatch { stored: None }
        );

        drop_execution(&db, &exec).await;
    }
}

// ============================================================================
// Raw SQL for the DAG event-log journal
// ============================================================================
//
// Spelled out rather than added as a Clorinde `--!` block: the checked-in
// bindings are generated in CI from `queries/*.sql` against a live Postgres
// (`.github/workflows/clorinde-bindings-fresh.yml`), so a new query block does
// not compile locally until that regeneration runs. Same precedent as
// `database/pg/token_usage.rs`.

/// Append WITHOUT `step_fingerprint` — the pre-`wf_resume_fingerprint_01`
/// shape, used when the live schema does not have the column yet.
const EVENT_LOG_APPEND_SQL: &str = r#"
WITH next_cursor AS (
    SELECT COALESCE(MAX(cursor), 0) + 1 AS cur
    FROM workflow_event_log
    WHERE execution_id = $1
)
INSERT INTO workflow_event_log (execution_id, node_id, event_type, event_data, cursor)
SELECT $1, $2, $3, $4, cur FROM next_cursor
RETURNING cursor
"#;

/// Append WITH `step_fingerprint`.
const EVENT_LOG_APPEND_SQL_WITH_FINGERPRINT: &str = r#"
WITH next_cursor AS (
    SELECT COALESCE(MAX(cursor), 0) + 1 AS cur
    FROM workflow_event_log
    WHERE execution_id = $1
)
INSERT INTO workflow_event_log
    (execution_id, node_id, event_type, event_data, cursor, step_fingerprint)
SELECT $1, $2, $3, $4, cur, $5 FROM next_cursor
RETURNING cursor
"#;

/// Latest `completed` event for one journal key, optionally selecting
/// `step_fingerprint` as column 1.
///
/// The row is located by the EXISTING key (`idx_event_log_node`); the
/// fingerprint is compared afterwards in Rust, never used as a search
/// predicate — which is why `wf_resume_fingerprint_01` deliberately adds no
/// index for it.
fn node_completed_sql(with_fingerprint: bool) -> &'static str {
    if with_fingerprint {
        r#"
        SELECT event_data, step_fingerprint
        FROM workflow_event_log
        WHERE execution_id = $1 AND node_id = $2 AND event_type = 'completed'
        ORDER BY cursor DESC
        LIMIT 1
        "#
    } else {
        r#"
        SELECT event_data
        FROM workflow_event_log
        WHERE execution_id = $1 AND node_id = $2 AND event_type = 'completed'
        ORDER BY cursor DESC
        LIMIT 1
        "#
    }
}

//! PostgreSQL deferred question operations.
//!
//! Uses raw SQL queries until Clorinde code generation is re-run.
//! The query definitions are in `queries/deferred_questions.sql` — once the
//! pre-existing `task_run_events.sql` error is fixed, run `clorinde schema`
//! to generate typed bindings and switch back to the generated API.

use super::PgDb;
use crate::memory::tenant_sync::{enqueue_memory_record, MemoryRecordKind, TenantMemoryRecord};

/// Cap for the derived memory title (chars). The tenant-memory emitter
/// re-clamps to its own server cap; this is just a readable snippet.
const FEEDBACK_TITLE_CHARS: usize = 120;

/// Importance for operator-feedback memories — deliberate human verdicts are
/// high-signal training data, above the 0.5 default.
const FEEDBACK_IMPORTANCE: f64 = 0.7;

/// Build the tenant-memory record that mirrors an operator's verdict on a
/// deferred question into the agentic-memory store. Pure (no I/O) so it is
/// unit-testable without a live database. The returned record still rides the
/// emitter's consent gate + redactor at [`enqueue_memory_record`].
pub(crate) fn build_review_feedback_record(
    id: &str,
    question: &str,
    context_json: &str,
    task_run_id: &str,
    status: &str,
    reviewer_comment: Option<&str>,
) -> TenantMemoryRecord {
    let title: String = question.chars().take(FEEDBACK_TITLE_CHARS).collect();

    let mut content = format!(
        "Deferred question: {question}\n\nContext: {context_json}\n\nOperator verdict: {status}"
    );
    if let Some(comment) = reviewer_comment.filter(|c| !c.trim().is_empty()) {
        content.push_str(&format!("\n\nReviewer comment: {comment}"));
    }

    let mut source = serde_json::json!({ "deferred_question_id": id });
    if !task_run_id.is_empty() {
        source["task_run_id"] = serde_json::Value::String(task_run_id.to_string());
    }

    TenantMemoryRecord::new(title, content, MemoryRecordKind::Feedback)
        .with_importance(FEEDBACK_IMPORTANCE)
        .with_source(source)
}

fn row_to_json(row: &tokio_postgres::Row) -> serde_json::Value {
    let reviewed_at: Option<chrono::DateTime<chrono::FixedOffset>> = row.get("reviewed_at");
    serde_json::json!({
        "id": row.get::<_, String>("id"),
        "task_run_id": row.get::<_, String>("task_run_id"),
        "iteration": row.get::<_, i32>("iteration"),
        "question": row.get::<_, String>("question"),
        "context_json": row.get::<_, String>("context_json"),
        "auto_decision_type": row.get::<_, String>("auto_decision_type"),
        "auto_decision_detail": row.get::<_, Option<String>>("auto_decision_detail"),
        "confidence": row.get::<_, f64>("confidence"),
        "risk_level": row.get::<_, String>("risk_level"),
        "status": row.get::<_, String>("status"),
        "git_checkpoint": row.get::<_, Option<String>>("git_checkpoint"),
        "contingent_iterations": row.get::<_, String>("contingent_iterations"),
        "reviewer_comment": row.get::<_, Option<String>>("reviewer_comment"),
        "created_at": row.get::<_, chrono::DateTime<chrono::FixedOffset>>("created_at").to_rfc3339(),
        "reviewed_at": reviewed_at.map(|t| t.to_rfc3339()),
    })
}

impl PgDb {
    /// Record a new deferred question.
    pub async fn insert_deferred_question(
        &self,
        id: &str,
        task_run_id: &str,
        iteration: i32,
        question: &str,
        context_json: &str,
        auto_decision_type: &str,
        auto_decision_detail: Option<&str>,
        confidence: f64,
        risk_level: &str,
        git_checkpoint: Option<&str>,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        conn.execute(
            "INSERT INTO deferred_questions \
             (id, task_run_id, iteration, question, context_json, auto_decision_type, \
              auto_decision_detail, confidence, risk_level, git_checkpoint) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
            &[
                &id,
                &task_run_id,
                &iteration,
                &question,
                &context_json,
                &auto_decision_type,
                &auto_decision_detail,
                &confidence,
                &risk_level,
                &git_checkpoint,
            ],
        )
        .await
        .map_err(|e| format!("PG insert_deferred_question: {}", e))?;
        Ok(())
    }

    /// Review a deferred question (approve/reject).
    pub async fn review_deferred_question(
        &self,
        id: &str,
        status: &str,
        reviewer_comment: Option<&str>,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        conn.execute(
            "UPDATE deferred_questions SET status = $1, reviewer_comment = $2, reviewed_at = NOW() WHERE id = $3",
            &[&status, &reviewer_comment, &id],
        )
        .await
        .map_err(|e| format!("PG review_deferred_question: {}", e))?;

        // Phase 3 (plan 2026-07-11-tenant-memory-v1-1): mirror the operator's
        // verdict into the tenant agentic-memory store as a `feedback` record
        // so future runs learn from human review. Best-effort — a lookup or
        // enqueue failure must NEVER break the verdict path, and the enqueue
        // is itself a no-op when cloud sync is disabled.
        if let Err(e) = self
            .emit_review_feedback(id, status, reviewer_comment)
            .await
        {
            tracing::warn!(
                question_id = %id,
                error = %e,
                "review_deferred_question: feedback memory emit skipped (non-fatal)"
            );
        }

        Ok(())
    }

    /// Look up the reviewed question and enqueue an operator-feedback memory
    /// record. Separated so the verdict UPDATE stays the single fallible step
    /// the caller depends on. Returns `Err` only for the diagnostic warn — the
    /// caller swallows it.
    async fn emit_review_feedback(
        &self,
        id: &str,
        status: &str,
        reviewer_comment: Option<&str>,
    ) -> Result<(), String> {
        let Some(q) = self.get_deferred_question_by_id(id).await? else {
            // Row vanished between UPDATE and SELECT — nothing to mirror.
            return Ok(());
        };
        let question = q
            .get("question")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let context_json = q
            .get("context_json")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let task_run_id = q
            .get("task_run_id")
            .and_then(|v| v.as_str())
            .unwrap_or_default();

        let record = build_review_feedback_record(
            id,
            question,
            context_json,
            task_run_id,
            status,
            reviewer_comment,
        );
        // Consent gate + redaction are applied inside enqueue_memory_record;
        // a no-op when `cloud_sync_enabled` is off.
        enqueue_memory_record(record);
        Ok(())
    }

    /// Append an iteration number to a deferred question's contingent iterations.
    ///
    /// Uses PostgreSQL jsonb concatenation to append without overwriting.
    pub async fn append_deferred_question_contingent_iteration(
        &self,
        id: &str,
        iteration: i32,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let iteration_json = format!("[{}]", iteration);
        conn.execute(
            "UPDATE deferred_questions \
             SET contingent_iterations = (contingent_iterations::jsonb || $1::jsonb)::text \
             WHERE id = $2",
            &[&iteration_json, &id],
        )
        .await
        .map_err(|e| format!("PG append_contingent_iteration: {}", e))?;
        Ok(())
    }

    /// Get all pending deferred questions across all task runs.
    pub async fn get_all_pending_questions(&self) -> Result<Vec<serde_json::Value>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let rows = conn
            .query(
                "SELECT id, task_run_id, iteration, question, context_json, \
                    auto_decision_type, auto_decision_detail, confidence, risk_level, \
                    status, git_checkpoint, contingent_iterations, reviewer_comment, \
                    created_at, reviewed_at \
                 FROM deferred_questions WHERE status = 'pending' ORDER BY created_at DESC",
                &[],
            )
            .await
            .map_err(|e| format!("PG get_all_pending_questions: {}", e))?;

        Ok(rows.iter().map(row_to_json).collect())
    }

    /// Get all deferred questions for a task run.
    pub async fn get_deferred_questions_for_task_run(
        &self,
        task_run_id: &str,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let rows = conn
            .query(
                "SELECT id, task_run_id, iteration, question, context_json, \
                    auto_decision_type, auto_decision_detail, confidence, risk_level, \
                    status, git_checkpoint, contingent_iterations, reviewer_comment, \
                    created_at, reviewed_at \
             FROM deferred_questions WHERE task_run_id = $1 ORDER BY created_at ASC",
                &[&task_run_id],
            )
            .await
            .map_err(|e| format!("PG get_deferred_questions_for_task_run: {}", e))?;

        Ok(rows.iter().map(row_to_json).collect())
    }

    /// Get deferred questions by status for a task run.
    pub async fn get_deferred_questions_by_status(
        &self,
        task_run_id: &str,
        status: &str,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let rows = conn.query(
            "SELECT id, task_run_id, iteration, question, context_json, \
                    auto_decision_type, auto_decision_detail, confidence, risk_level, \
                    status, git_checkpoint, contingent_iterations, reviewer_comment, \
                    created_at, reviewed_at \
             FROM deferred_questions WHERE task_run_id = $1 AND status = $2 ORDER BY created_at ASC",
            &[&task_run_id, &status],
        )
        .await
        .map_err(|e| format!("PG get_deferred_questions_by_status: {}", e))?;

        Ok(rows.iter().map(row_to_json).collect())
    }

    /// Get a single deferred question by ID.
    pub async fn get_deferred_question_by_id(
        &self,
        id: &str,
    ) -> Result<Option<serde_json::Value>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let rows = conn
            .query(
                "SELECT id, task_run_id, iteration, question, context_json, \
                    auto_decision_type, auto_decision_detail, confidence, risk_level, \
                    status, git_checkpoint, contingent_iterations, reviewer_comment, \
                    created_at, reviewed_at \
             FROM deferred_questions WHERE id = $1",
                &[&id],
            )
            .await
            .map_err(|e| format!("PG get_deferred_question_by_id: {}", e))?;

        Ok(rows.first().map(row_to_json))
    }

    /// Count pending deferred questions for a task run.
    pub async fn count_pending_deferred_questions(&self, task_run_id: &str) -> Result<i64, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let row = conn.query_one(
            "SELECT COUNT(*) as count FROM deferred_questions WHERE task_run_id = $1 AND status = 'pending'",
            &[&task_run_id],
        )
        .await
        .map_err(|e| format!("PG count_pending_deferred_questions: {}", e))?;
        Ok(row.get::<_, i64>("count"))
    }

    /// Get reviewed deferred questions for a workflow (for learning loop).
    ///
    /// Returns (confidence, approved) pairs for all questions reviewed in the last 30 days.
    pub async fn get_reviewed_deferred_questions_for_workflow(
        &self,
        workflow_id: &str,
    ) -> Result<Vec<(f64, bool)>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let rows = conn
            .query(
                "SELECT dq.confidence, dq.status \
             FROM deferred_questions dq \
             JOIN task_runs tr ON dq.task_run_id = tr.id \
             WHERE tr.workflow_id = $1 \
               AND dq.status IN ('approved', 'rejected') \
               AND dq.reviewed_at > NOW() - INTERVAL '30 days' \
             ORDER BY dq.created_at",
                &[&workflow_id],
            )
            .await
            .map_err(|e| format!("PG get_reviewed_deferred_questions: {}", e))?;

        Ok(rows
            .iter()
            .map(|r| {
                let confidence: f64 = r.get("confidence");
                let status: String = r.get("status");
                (confidence, status == "approved")
            })
            .collect())
    }

    /// Upsert a learned confidence threshold for a workflow.
    ///
    /// Stores the threshold in `agentic_metric_baselines` with
    /// metric_type = 'deferred_confidence_threshold'.
    pub async fn upsert_deferred_confidence_threshold(
        &self,
        workflow_id: &str,
        threshold: f64,
        evidence: &str,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let baseline_value = serde_json::json!({
            "threshold": threshold,
            "evidence": evidence,
        })
        .to_string();
        let id = format!("deferred-threshold-{}", workflow_id);
        conn.execute(
            "INSERT INTO agentic_metric_baselines (id, workflow_id, metric_type, baseline_value, sample_count, updated_at) \
             VALUES ($1, $2, 'deferred_confidence_threshold', $3, 1, NOW()) \
             ON CONFLICT (workflow_id, metric_type) DO UPDATE SET \
               baseline_value = EXCLUDED.baseline_value, \
               sample_count = agentic_metric_baselines.sample_count + 1, \
               updated_at = NOW()",
            &[&id, &workflow_id, &baseline_value],
        )
        .await
        .map_err(|e| format!("PG upsert_deferred_confidence_threshold: {}", e))?;
        Ok(())
    }

    /// Load the learned confidence threshold for a workflow.
    ///
    /// Returns None if no learned threshold exists (use default).
    pub async fn get_deferred_confidence_threshold(
        &self,
        workflow_id: &str,
    ) -> Result<Option<f64>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let rows = conn
            .query(
                "SELECT baseline_value FROM agentic_metric_baselines \
             WHERE workflow_id = $1 AND metric_type = 'deferred_confidence_threshold'",
                &[&workflow_id],
            )
            .await
            .map_err(|e| format!("PG get_deferred_confidence_threshold: {}", e))?;

        if let Some(row) = rows.first() {
            let value: String = row.get("baseline_value");
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&value) {
                return Ok(parsed["threshold"].as_f64());
            }
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::tenant_sync::TenantMemorySync;
    use crate::session::local_store::OutboxWriter;
    use std::sync::Arc;
    use tempfile::tempdir;
    use uuid::Uuid;

    #[test]
    fn build_review_feedback_record_captures_join_fields() {
        let record = build_review_feedback_record(
            "dq-123",
            "Should I delete the legacy migration file?",
            "{\"phase\":\"cleanup\",\"files\":[\"old.sql\"]}",
            "tr-77",
            "approved",
            Some("Yes, it's superseded."),
        );

        assert_eq!(record.kind, MemoryRecordKind::Feedback);
        assert_eq!(record.kind.as_str(), "feedback");
        assert!((record.importance - FEEDBACK_IMPORTANCE).abs() < f64::EPSILON);
        assert!(record.title.starts_with("Should I delete"));
        assert!(record.content.contains("Operator verdict: approved"));
        assert!(record
            .content
            .contains("Reviewer comment: Yes, it's superseded."));
        assert!(record.content.contains("phase"));
        assert_eq!(record.source["deferred_question_id"], "dq-123");
        assert_eq!(record.source["task_run_id"], "tr-77");
    }

    #[test]
    fn build_review_feedback_record_truncates_title_and_omits_empty_comment() {
        let long_question = "q".repeat(FEEDBACK_TITLE_CHARS + 50);
        let record =
            build_review_feedback_record("dq-1", &long_question, "{}", "", "rejected", Some("   "));
        assert_eq!(record.title.chars().count(), FEEDBACK_TITLE_CHARS);
        assert!(!record.content.contains("Reviewer comment"));
        // Empty task_run_id must not appear as a null/blank join key.
        assert!(record.source.get("task_run_id").is_none());
        assert_eq!(record.source["deferred_question_id"], "dq-1");
    }

    /// A verdict-derived record, enqueued through a consenting emitter, lands
    /// in the outbox as a `feedback` record carrying the join fields.
    #[test]
    fn feedback_record_enqueues_when_consent_on() {
        let dir = tempdir().unwrap();
        let outbox = Arc::new(OutboxWriter::open(dir.path().join("memory-outbox.jsonl")).unwrap());
        let sync = TenantMemorySync::with_probes(
            outbox.clone(),
            Uuid::new_v4(),
            Box::new(|| true), // consent ON
            Box::new(|| Some("test.jwt".to_string())),
        );

        let record = build_review_feedback_record(
            "dq-9",
            "Ship the schema change?",
            "{\"risk\":\"low\"}",
            "tr-9",
            "approved",
            None,
        );
        sync.enqueue(record);

        let pending = outbox.pending().unwrap();
        assert_eq!(pending.len(), 1);
        let p = &pending[0].payload;
        assert_eq!(p["kind"], "feedback");
        assert_eq!(p["source"]["deferred_question_id"], "dq-9");
        assert_eq!(p["source"]["task_run_id"], "tr-9");
        assert!(p["content"]
            .as_str()
            .unwrap()
            .contains("Operator verdict: approved"));
    }

    /// Consent OFF ⇒ the same record is a hard no-op (nothing hits the outbox).
    #[test]
    fn feedback_record_is_noop_when_consent_off() {
        let dir = tempdir().unwrap();
        let outbox = Arc::new(OutboxWriter::open(dir.path().join("memory-outbox.jsonl")).unwrap());
        let sync = TenantMemorySync::with_probes(
            outbox.clone(),
            Uuid::new_v4(),
            Box::new(|| false), // consent OFF
            Box::new(|| Some("test.jwt".to_string())),
        );

        sync.enqueue(build_review_feedback_record(
            "dq-9", "Ship it?", "{}", "tr-9", "approved", None,
        ));

        assert!(
            outbox.pending().unwrap().is_empty(),
            "consent off ⇒ operator feedback never leaves the machine"
        );
    }
}

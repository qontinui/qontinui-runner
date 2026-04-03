//! PostgreSQL deferred question operations.
//!
//! Uses raw SQL queries until Clorinde code generation is re-run.
//! The query definitions are in `queries/deferred_questions.sql` — once the
//! pre-existing `task_run_events.sql` error is fixed, run `clorinde schema`
//! to generate typed bindings and switch back to the generated API.

use super::PgDb;

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

//! PostgreSQL reflection fixes operations.
//!
//! Provides CRUD for `reflection_fixes` and `fix_applications` tables using raw SQL.
//! This is one of the most heavily used tables (126+ callers for reflection_fixes).

use super::PgDb;
use crate::reflection::types::{CreateReflectionFixInput, ReflectionFix};
use tracing::{debug, info, warn};

fn row_to_fix(row: &tokio_postgres::Row) -> ReflectionFix {
    ReflectionFix {
        id: row.get("id"),
        source_task_run_id: row.get("source_task_run_id"),
        reflection_task_run_id: row.get("reflection_task_run_id"),
        source_finding_id: row.get("source_finding_id"),
        source_knowledge_id: row.get("source_knowledge_id"),
        fix_type: row.get("fix_type"),
        fix_description: row.get("fix_description"),
        file_changed: row.get("file_changed"),
        old_value: row.get("old_value"),
        new_value: row.get("new_value"),
        confidence: row.get("confidence"),
        content_hash: row.get("content_hash"),
        status: row.get("status"),
        effectiveness: row.get("effectiveness"),
        effectiveness_evidence: row.get("effectiveness_evidence"),
        applied_at: row.get("applied_at"),
        evaluated_at: row.get("evaluated_at"),
        created_at: row.get("created_at"),
        source_agent: row.get("source_agent"),
        reasoning: row.get("reasoning"),
        alternatives_considered: row.get("alternatives_considered"),
        reflection_scope: row.get("reflection_scope"),
        project_path: row.get("project_path"),
        applicability_context: row.get("applicability_context"),
    }
}

const SELECT_ALL_COLUMNS: &str = r#"
    id, source_task_run_id, reflection_task_run_id,
    source_finding_id, source_knowledge_id,
    fix_type, fix_description, file_changed,
    old_value, new_value, confidence, content_hash,
    status, effectiveness, effectiveness_evidence,
    applied_at, evaluated_at, created_at, source_agent,
    reasoning, alternatives_considered,
    reflection_scope, project_path, applicability_context
"#;

/// Compute a content hash for deduplication of reflection fixes.
fn compute_content_hash(
    fix_type: &str,
    description: &str,
    old_value: Option<&str>,
    new_value: Option<&str>,
) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut hasher = DefaultHasher::new();
    fix_type.hash(&mut hasher);
    description.to_lowercase().hash(&mut hasher);
    old_value.map(|s| s.to_lowercase()).hash(&mut hasher);
    new_value.map(|s| s.to_lowercase()).hash(&mut hasher);
    format!("{:x}", hasher.finish())
}

impl PgDb {
    // ========================================================================
    // Reflection Fixes
    // ========================================================================

    /// Insert a new reflection fix with content-hash deduplication.
    pub async fn save_reflection_fix(
        &self,
        input: &CreateReflectionFixInput,
    ) -> Result<ReflectionFix, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let content_hash = compute_content_hash(
            &input.fix_type,
            &input.fix_description,
            input.old_value.as_deref(),
            input.new_value.as_deref(),
        );

        // Check for existing fix with same content hash
        let existing = conn
            .query_opt(
                "SELECT id FROM reflection_fixes WHERE content_hash = $1 AND status = 'applied' LIMIT 1",
                &[&content_hash],
            )
            .await
            .map_err(|e| format!("PG dedup check: {}", e))?;

        if let Some(row) = existing {
            let existing_id: String = row.get(0);
            warn!("Skipping duplicate PG reflection fix (hash: {}, existing: {})", content_hash, existing_id);
            return self.get_reflection_fix(&existing_id).await?
                .ok_or_else(|| "Existing fix not found after dedup check".to_string());
        }

        let id = uuid::Uuid::new_v4().to_string();
        let now = chrono::Utc::now().to_rfc3339();

        conn.execute(
            r#"INSERT INTO reflection_fixes (
                id, source_task_run_id, reflection_task_run_id,
                source_finding_id, source_knowledge_id,
                fix_type, fix_description, file_changed,
                old_value, new_value, confidence, content_hash,
                status, applied_at, created_at, source_agent,
                reasoning, alternatives_considered
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, 'applied', $13, $13, $14, $15, $16)"#,
            &[
                &id as &(dyn tokio_postgres::types::ToSql + Sync),
                &input.source_task_run_id,
                &input.reflection_task_run_id,
                &input.source_finding_id as &(dyn tokio_postgres::types::ToSql + Sync),
                &input.source_knowledge_id as &(dyn tokio_postgres::types::ToSql + Sync),
                &input.fix_type,
                &input.fix_description,
                &input.file_changed as &(dyn tokio_postgres::types::ToSql + Sync),
                &input.old_value as &(dyn tokio_postgres::types::ToSql + Sync),
                &input.new_value as &(dyn tokio_postgres::types::ToSql + Sync),
                &input.confidence,
                &content_hash,
                &now,
                &input.source_agent as &(dyn tokio_postgres::types::ToSql + Sync),
                &input.reasoning as &(dyn tokio_postgres::types::ToSql + Sync),
                &input.alternatives_considered as &(dyn tokio_postgres::types::ToSql + Sync),
            ],
        )
        .await
        .map_err(|e| format!("PG save_reflection_fix: {}", e))?;

        info!("Inserted PG reflection fix {} (type: {})", id, input.fix_type);

        self.get_reflection_fix(&id).await?
            .ok_or_else(|| "Fix not found after PG insert".to_string())
    }

    /// Get a single reflection fix by ID.
    pub async fn get_reflection_fix(&self, id: &str) -> Result<Option<ReflectionFix>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let sql = format!("SELECT {} FROM reflection_fixes WHERE id = $1", SELECT_ALL_COLUMNS);
        let rows = conn
            .query(&sql, &[&id])
            .await
            .map_err(|e| format!("PG get_reflection_fix: {}", e))?;

        Ok(rows.first().map(row_to_fix))
    }

    /// Get all fixes for a given source task run.
    pub async fn get_fixes_for_source_run(
        &self,
        source_task_run_id: &str,
    ) -> Result<Vec<ReflectionFix>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let sql = format!(
            "SELECT {} FROM reflection_fixes WHERE source_task_run_id = $1 ORDER BY created_at",
            SELECT_ALL_COLUMNS
        );
        let rows = conn
            .query(&sql, &[&source_task_run_id])
            .await
            .map_err(|e| format!("PG get_fixes_for_source_run: {}", e))?;

        Ok(rows.iter().map(row_to_fix).collect())
    }

    /// Get unresolved reflection fixes (status != 'resolved' and != 'superseded').
    pub async fn get_unresolved_fixes(&self) -> Result<Vec<ReflectionFix>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let sql = format!(
            "SELECT {} FROM reflection_fixes WHERE status NOT IN ('resolved', 'superseded') ORDER BY created_at DESC",
            SELECT_ALL_COLUMNS
        );
        let rows = conn
            .query(&sql, &[])
            .await
            .map_err(|e| format!("PG get_unresolved_fixes: {}", e))?;

        Ok(rows.iter().map(row_to_fix).collect())
    }

    /// Update the status of a reflection fix.
    pub async fn update_fix_status(&self, id: &str, status: &str) -> Result<(), String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let affected = conn
            .execute(
                "UPDATE reflection_fixes SET status = $1 WHERE id = $2",
                &[&status, &id],
            )
            .await
            .map_err(|e| format!("PG update_fix_status: {}", e))?;

        if affected == 0 {
            return Err(format!("PG reflection fix {} not found", id));
        }

        debug!("Updated PG reflection fix {} status to {}", id, status);
        Ok(())
    }

    /// Update the effectiveness evaluation of a reflection fix.
    pub async fn update_fix_effectiveness(
        &self,
        id: &str,
        effectiveness: &str,
        evidence: Option<&str>,
    ) -> Result<(), String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let now = chrono::Utc::now().to_rfc3339();

        let affected = conn
            .execute(
                r#"UPDATE reflection_fixes
                   SET effectiveness = $1, effectiveness_evidence = $2, evaluated_at = $3
                   WHERE id = $4"#,
                &[
                    &effectiveness as &(dyn tokio_postgres::types::ToSql + Sync),
                    &evidence as &(dyn tokio_postgres::types::ToSql + Sync),
                    &now,
                    &id,
                ],
            )
            .await
            .map_err(|e| format!("PG update_fix_effectiveness: {}", e))?;

        if affected == 0 {
            return Err(format!("PG reflection fix {} not found", id));
        }

        info!("Updated PG reflection fix {} effectiveness to {}", id, effectiveness);
        Ok(())
    }

    /// Get fixes by workflow name via JOIN with task_runs.
    pub async fn get_fixes_by_workflow_name(
        &self,
        workflow_name: &str,
        status_filter: Option<&str>,
    ) -> Result<Vec<ReflectionFix>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let prefixed = SELECT_ALL_COLUMNS
            .split(',')
            .map(|col| {
                let col = col.trim();
                if col.is_empty() { String::new() } else { format!("rf.{}", col) }
            })
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(", ");

        let mut sql = format!(
            r#"SELECT {} FROM reflection_fixes rf
               INNER JOIN task_runs tr ON rf.source_task_run_id = tr.id
               WHERE tr.workflow_name = $1"#,
            prefixed
        );

        if status_filter.is_some() {
            sql.push_str(" AND rf.status = $2");
        }
        sql.push_str(" ORDER BY rf.created_at DESC");

        let rows = if let Some(status) = status_filter {
            conn.query(&sql, &[&workflow_name, &status]).await
        } else {
            conn.query(&sql, &[&workflow_name]).await
        }
        .map_err(|e| format!("PG get_fixes_by_workflow_name: {}", e))?;

        Ok(rows.iter().map(row_to_fix).collect())
    }

    /// Get project-scoped fixes for a project path.
    pub async fn get_fixes_by_project_path(
        &self,
        project_path: &str,
        status_filter: Option<&str>,
    ) -> Result<Vec<ReflectionFix>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let mut sql = format!(
            "SELECT {} FROM reflection_fixes WHERE project_path = $1 AND reflection_scope = 'project'",
            SELECT_ALL_COLUMNS
        );

        if status_filter.is_some() {
            sql.push_str(" AND status = $2");
        }
        sql.push_str(" ORDER BY created_at DESC");

        let rows = if let Some(status) = status_filter {
            conn.query(&sql, &[&project_path, &status]).await
        } else {
            conn.query(&sql, &[&project_path]).await
        }
        .map_err(|e| format!("PG get_fixes_by_project_path: {}", e))?;

        Ok(rows.iter().map(row_to_fix).collect())
    }

    /// Get universal-scoped fixes ordered by reuse_count.
    pub async fn get_universal_fixes(&self, limit: i64) -> Result<Vec<ReflectionFix>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let sql = format!(
            "SELECT {} FROM reflection_fixes WHERE reflection_scope = 'universal' AND status = 'applied' ORDER BY reuse_count DESC, created_at DESC LIMIT $1",
            SELECT_ALL_COLUMNS
        );

        let rows = conn
            .query(&sql, &[&limit])
            .await
            .map_err(|e| format!("PG get_universal_fixes: {}", e))?;

        Ok(rows.iter().map(row_to_fix).collect())
    }

    // ========================================================================
    // Fix Applications
    // ========================================================================

    /// Save a fix application record.
    pub async fn save_fix_application(
        &self,
        fix_id: &str,
        task_run_id: &str,
        error_signature_hash: Option<&str>,
        outcome: &str,
    ) -> Result<String, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let id = uuid::Uuid::new_v4().to_string();
        let now = chrono::Utc::now().to_rfc3339();

        conn.execute(
            r#"INSERT INTO fix_applications
               (id, fix_id, task_run_id, error_signature_hash, outcome, applied_at)
               VALUES ($1, $2, $3, $4, $5, $6)"#,
            &[
                &id as &(dyn tokio_postgres::types::ToSql + Sync),
                &fix_id,
                &task_run_id,
                &error_signature_hash as &(dyn tokio_postgres::types::ToSql + Sync),
                &outcome,
                &now,
            ],
        )
        .await
        .map_err(|e| format!("PG save_fix_application: {}", e))?;

        // Increment reuse_count on the fix
        let _ = conn
            .execute(
                "UPDATE reflection_fixes SET reuse_count = COALESCE(reuse_count, 0) + 1 WHERE id = $1",
                &[&fix_id],
            )
            .await;

        debug!("Saved PG fix application {} for fix {}", id, fix_id);
        Ok(id)
    }

    /// Get fixes by workflow name with optional status and effectiveness filters.
    pub async fn get_fixes_by_workflow_name_filtered(
        &self,
        workflow_name: &str,
        status_filter: Option<&str>,
        effectiveness_filter: Option<&str>,
    ) -> Result<Vec<ReflectionFix>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let prefixed = SELECT_ALL_COLUMNS
            .split(',')
            .map(|col| {
                let col = col.trim();
                if col.is_empty() { String::new() } else { format!("rf.{}", col) }
            })
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(", ");

        let mut sql = format!(
            r#"SELECT {} FROM reflection_fixes rf
               INNER JOIN task_runs tr ON rf.source_task_run_id = tr.id
               WHERE tr.workflow_name = $1"#,
            prefixed
        );

        let mut params: Vec<Box<dyn tokio_postgres::types::ToSql + Sync + Send>> = Vec::new();
        params.push(Box::new(workflow_name.to_string()));

        if let Some(s) = status_filter {
            params.push(Box::new(s.to_string()));
            sql.push_str(&format!(" AND rf.status = ${}", params.len()));
        }
        if let Some(e) = effectiveness_filter {
            if e == "unevaluated" {
                sql.push_str(" AND rf.effectiveness IS NULL");
            } else {
                params.push(Box::new(e.to_string()));
                sql.push_str(&format!(" AND rf.effectiveness = ${}", params.len()));
            }
        }
        sql.push_str(" ORDER BY rf.created_at DESC");

        let param_refs: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> =
            params.iter().map(|p| p.as_ref() as &(dyn tokio_postgres::types::ToSql + Sync)).collect();

        let rows = conn
            .query(&sql, &param_refs)
            .await
            .map_err(|e| format!("PG get_fixes_by_workflow_name_filtered: {}", e))?;

        Ok(rows.iter().map(row_to_fix).collect())
    }

    /// Get reflection history for a workflow (reflection task runs with fix counts).
    pub async fn get_reflection_history(
        &self,
        workflow_name: &str,
    ) -> Result<Vec<crate::reflection::storage::ReflectionRunSummary>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                r#"
                SELECT
                    tr.id,
                    tr.reflection_source_task_run_id,
                    tr.status,
                    tr.created_at,
                    tr.completed_at,
                    (SELECT COUNT(*) FROM reflection_fixes rf WHERE rf.reflection_task_run_id = tr.id) as fix_count
                FROM task_runs tr
                WHERE tr.is_reflection = true
                  AND tr.workflow_name LIKE '%' || $1 || '%'
                ORDER BY tr.created_at DESC
                "#,
                &[&workflow_name],
            )
            .await
            .map_err(|e| format!("PG get_reflection_history: {}", e))?;

        Ok(rows
            .iter()
            .map(|r| crate::reflection::storage::ReflectionRunSummary {
                task_run_id: r.get(0),
                source_task_run_id: r.get(1),
                status: r.get(2),
                created_at: r.get(3),
                completed_at: r.get(4),
                fix_count: {
                    let c: i64 = r.get(5);
                    c as u32
                },
            })
            .collect())
    }

    /// Get effectiveness report for a workflow (computed from fixes).
    pub async fn get_effectiveness_report(
        &self,
        workflow_name: &str,
    ) -> Result<crate::reflection::types::EffectivenessReport, String> {
        let fixes = self
            .get_fixes_by_workflow_name_filtered(workflow_name, None, None)
            .await?;
        let total = fixes.len() as u32;

        let mut effective = 0u32;
        let mut ineffective = 0u32;
        let mut regression = 0u32;
        let mut inconclusive = 0u32;
        let mut unevaluated = 0u32;

        for fix in &fixes {
            match fix.effectiveness.as_deref() {
                Some("effective") => effective += 1,
                Some("ineffective") => ineffective += 1,
                Some("caused_regression") => regression += 1,
                Some("inconclusive") => inconclusive += 1,
                None => unevaluated += 1,
                _ => unevaluated += 1,
            }
        }

        let evaluated = effective + ineffective + regression;
        let effectiveness_rate = if evaluated > 0 {
            effective as f64 / evaluated as f64
        } else {
            0.0
        };

        Ok(crate::reflection::types::EffectivenessReport {
            workflow_name: workflow_name.to_string(),
            total_fixes: total,
            effective_count: effective,
            ineffective_count: ineffective,
            regression_count: regression,
            inconclusive_count: inconclusive,
            unevaluated_count: unevaluated,
            effectiveness_rate,
            fixes,
        })
    }

    /// Get all applications for a given fix.
    pub async fn get_applications_for_fix(
        &self,
        fix_id: &str,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                r#"SELECT id, fix_id, task_run_id, error_signature_hash, outcome, applied_at, evaluated_at
                   FROM fix_applications
                   WHERE fix_id = $1
                   ORDER BY applied_at DESC"#,
                &[&fix_id],
            )
            .await
            .map_err(|e| format!("PG get_applications_for_fix: {}", e))?;

        Ok(rows
            .iter()
            .map(|r| {
                serde_json::json!({
                    "id": r.get::<_, String>(0),
                    "fix_id": r.get::<_, String>(1),
                    "task_run_id": r.get::<_, String>(2),
                    "error_signature_hash": r.get::<_, Option<String>>(3),
                    "outcome": r.get::<_, Option<String>>(4),
                    "applied_at": r.get::<_, String>(5),
                    "evaluated_at": r.get::<_, Option<String>>(6),
                })
            })
            .collect())
    }
}

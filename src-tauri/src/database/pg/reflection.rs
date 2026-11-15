//! PostgreSQL reflection fixes operations.
//!
//! Provides CRUD for `reflection_fixes` and `fix_applications` tables using raw SQL.
//! This is one of the most heavily used tables (126+ callers for reflection_fixes).

use super::PgDb;
use crate::reflection::types::{CreateReflectionFixInput, FixEffectiveness, ReflectionFix};
use crate::str_utils::truncate_str;
use tracing::{debug, info, warn};

#[expect(
    clippy::disallowed_methods,
    reason = "legacy Row::get — migrate to try_get; dossier row-get-panic-kills-spawned-loop"
)]
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
    applied_at::TEXT, evaluated_at::TEXT, created_at::TEXT, source_agent,
    reasoning, alternatives_considered,
    reflection_scope, project_path, applicability_context
"#;

/// [`SELECT_ALL_COLUMNS`] with every column qualified by `alias`. Required in
/// any query that joins `task_runs`, which has its own `id`, `status` and
/// `created_at`: left bare, those names are ambiguous and Postgres rejects the
/// whole statement.
fn prefixed_select_columns(alias: &str) -> String {
    SELECT_ALL_COLUMNS
        .split(',')
        .map(str::trim)
        .filter(|col| !col.is_empty())
        .map(|col| format!("{alias}.{col}"))
        .collect::<Vec<_>>()
        .join(", ")
}

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
    #[expect(
        clippy::disallowed_methods,
        reason = "legacy Row::get — migrate to try_get; dossier row-get-panic-kills-spawned-loop"
    )]
    pub async fn save_reflection_fix(
        &self,
        input: &CreateReflectionFixInput,
    ) -> Result<ReflectionFix, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

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
            .map_err(|e| crate::database::pg::pg_err("PG dedup check", &e))?;

        if let Some(row) = existing {
            let existing_id: String = row.get(0);
            warn!(
                "Skipping duplicate PG reflection fix (hash: {}, existing: {})",
                content_hash, existing_id
            );
            return self
                .get_reflection_fix(&existing_id)
                .await?
                .ok_or_else(|| "Existing fix not found after dedup check".to_string());
        }

        let id = uuid::Uuid::new_v4().to_string();

        // `applied_at` / `created_at` are `timestamptz`, stamped by `NOW()` in
        // SQL. postgres-types binds a Rust `String` only to a text-family
        // parameter, so the RFC 3339 string bound here before was refused by
        // the driver and the INSERT never ran on a database built from
        // `schema.pg.sql.generated`.
        conn.execute(
            r#"INSERT INTO reflection_fixes (
                id, source_task_run_id, reflection_task_run_id,
                source_finding_id, source_knowledge_id,
                fix_type, fix_description, file_changed,
                old_value, new_value, confidence, content_hash,
                status, applied_at, created_at, source_agent,
                reasoning, alternatives_considered
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, 'applied', NOW(), NOW(), $13, $14, $15)"#,
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
                &input.source_agent as &(dyn tokio_postgres::types::ToSql + Sync),
                &input.reasoning as &(dyn tokio_postgres::types::ToSql + Sync),
                &input.alternatives_considered as &(dyn tokio_postgres::types::ToSql + Sync),
            ],
        )
        .await
        .map_err(|e| crate::database::pg::pg_err("PG save_reflection_fix", &e))?;

        info!(
            "Inserted PG reflection fix {} (type: {})",
            id, input.fix_type
        );

        // Tenant-memory mirror (plan 2026-07-10-tenant-agentic-memory,
        // Phase 2): a freshly learned reflection fix is a tenant-level fact.
        // Single choke point — covers every save_reflection_fix caller
        // (claude_session runner, MCP reflection API). The dedup early-return
        // above means an already-known fix never re-emits. Consent-gated +
        // redacted inside the emitter; best-effort.
        {
            let mut content = input.fix_description.clone();
            if let Some(reasoning) = input.reasoning.as_deref() {
                if !reasoning.trim().is_empty() {
                    content.push_str("\n\nReasoning: ");
                    content.push_str(reasoning);
                }
            }
            let importance = match input.confidence.as_str() {
                "high" => 0.8,
                "low" => 0.4,
                _ => 0.6,
            };
            crate::memory::tenant_sync::enqueue_memory_record(
                crate::memory::tenant_sync::TenantMemoryRecord::new(
                    format!("Reflection fix: {}", input.fix_type),
                    content,
                    crate::memory::tenant_sync::MemoryRecordKind::Fact,
                )
                .with_importance(importance)
                .with_source(serde_json::json!({
                    "task_run_id": input.source_task_run_id,
                    "fix_id": id,
                    "fix_type": input.fix_type,
                    "file_changed": input.file_changed,
                })),
            );
        }

        self.get_reflection_fix(&id)
            .await?
            .ok_or_else(|| "Fix not found after PG insert".to_string())
    }

    /// Get a single reflection fix by ID.
    pub async fn get_reflection_fix(&self, id: &str) -> Result<Option<ReflectionFix>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let sql = format!(
            "SELECT {} FROM reflection_fixes WHERE id = $1",
            SELECT_ALL_COLUMNS
        );
        let rows = conn
            .query(&sql, &[&id])
            .await
            .map_err(|e| crate::database::pg::pg_err("PG get_reflection_fix", &e))?;

        Ok(rows.first().map(row_to_fix))
    }

    /// Get all fixes for a given source task run.
    pub async fn get_fixes_for_source_run(
        &self,
        source_task_run_id: &str,
    ) -> Result<Vec<ReflectionFix>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let sql = format!(
            "SELECT {} FROM reflection_fixes WHERE source_task_run_id = $1 ORDER BY created_at",
            SELECT_ALL_COLUMNS
        );
        let rows = conn
            .query(&sql, &[&source_task_run_id])
            .await
            .map_err(|e| crate::database::pg::pg_err("PG get_fixes_for_source_run", &e))?;

        Ok(rows.iter().map(row_to_fix).collect())
    }

    /// Get unresolved reflection fixes (status != 'resolved' and != 'superseded').
    pub async fn get_unresolved_fixes(&self) -> Result<Vec<ReflectionFix>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let sql = format!(
            "SELECT {} FROM reflection_fixes WHERE status NOT IN ('resolved', 'superseded') ORDER BY created_at DESC",
            SELECT_ALL_COLUMNS
        );
        let rows = conn
            .query(&sql, &[])
            .await
            .map_err(|e| crate::database::pg::pg_err("PG get_unresolved_fixes", &e))?;

        Ok(rows.iter().map(row_to_fix).collect())
    }

    /// Update the status of a reflection fix.
    pub async fn update_fix_status(&self, id: &str, status: &str) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let affected = conn
            .execute(
                "UPDATE reflection_fixes SET status = $1 WHERE id = $2",
                &[&status, &id],
            )
            .await
            .map_err(|e| crate::database::pg::pg_err("PG update_fix_status", &e))?;

        if affected == 0 {
            return Err(format!("PG reflection fix {} not found", id));
        }

        debug!("Updated PG reflection fix {} status to {}", id, status);
        Ok(())
    }

    /// Update the effectiveness evaluation of a reflection fix — the hand-set
    /// door behind `PUT /reflection-fixes/:id/effectiveness`.
    ///
    /// `evaluated_at` is `timestamptz`, stamped by `NOW()` in SQL: the RFC 3339
    /// `String` bound here before is refused by postgres-types, so the UPDATE
    /// never ran on a database built from `schema.pg.sql.generated`.
    pub async fn update_fix_effectiveness(
        &self,
        id: &str,
        effectiveness: &str,
        evidence: Option<&str>,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let affected = conn
            .execute(
                r#"UPDATE reflection_fixes
                   SET effectiveness = $1, effectiveness_evidence = $2, evaluated_at = NOW()
                   WHERE id = $3"#,
                &[
                    &effectiveness as &(dyn tokio_postgres::types::ToSql + Sync),
                    &evidence as &(dyn tokio_postgres::types::ToSql + Sync),
                    &id,
                ],
            )
            .await
            .map_err(|e| crate::database::pg::pg_err("PG update_fix_effectiveness", &e))?;

        if affected == 0 {
            return Err(format!("PG reflection fix {} not found", id));
        }

        info!(
            "Updated PG reflection fix {} effectiveness to {}",
            id, effectiveness
        );
        Ok(())
    }

    /// Get fixes by workflow name via JOIN with task_runs.
    pub async fn get_fixes_by_workflow_name(
        &self,
        workflow_name: &str,
        status_filter: Option<&str>,
    ) -> Result<Vec<ReflectionFix>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let mut sql = format!(
            r#"SELECT {} FROM reflection_fixes rf
               INNER JOIN task_runs tr ON rf.source_task_run_id = tr.id
               WHERE tr.workflow_name = $1"#,
            prefixed_select_columns("rf")
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
        .map_err(|e| crate::database::pg::pg_err("PG get_fixes_by_workflow_name", &e))?;

        Ok(rows.iter().map(row_to_fix).collect())
    }

    /// Get project-scoped fixes for a project path.
    pub async fn get_fixes_by_project_path(
        &self,
        project_path: &str,
        status_filter: Option<&str>,
    ) -> Result<Vec<ReflectionFix>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

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
        .map_err(|e| crate::database::pg::pg_err("PG get_fixes_by_project_path", &e))?;

        Ok(rows.iter().map(row_to_fix).collect())
    }

    /// Get universal-scoped fixes ordered by reuse_count.
    pub async fn get_universal_fixes(&self, limit: i64) -> Result<Vec<ReflectionFix>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let sql = format!(
            "SELECT {} FROM reflection_fixes WHERE reflection_scope = 'universal' AND status = 'applied' ORDER BY reuse_count DESC, created_at DESC LIMIT $1",
            SELECT_ALL_COLUMNS
        );

        let rows = conn
            .query(&sql, &[&limit])
            .await
            .map_err(|e| crate::database::pg::pg_err("PG get_universal_fixes", &e))?;

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
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let id = uuid::Uuid::new_v4().to_string();

        // `applied_at` is `timestamptz`, stamped by `NOW()` in SQL — a bound
        // RFC 3339 `String` is refused by postgres-types before the INSERT runs.
        conn.execute(
            r#"INSERT INTO fix_applications
               (id, fix_id, task_run_id, error_signature_hash, outcome, applied_at)
               VALUES ($1, $2, $3, $4, $5, NOW())"#,
            &[
                &id as &(dyn tokio_postgres::types::ToSql + Sync),
                &fix_id,
                &task_run_id,
                &error_signature_hash as &(dyn tokio_postgres::types::ToSql + Sync),
                &outcome,
            ],
        )
        .await
        .map_err(|e| crate::database::pg::pg_err("PG save_fix_application", &e))?;

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
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let mut sql = format!(
            r#"SELECT {} FROM reflection_fixes rf
               INNER JOIN task_runs tr ON rf.source_task_run_id = tr.id
               WHERE tr.workflow_name = $1"#,
            prefixed_select_columns("rf")
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

        let param_refs: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = params
            .iter()
            .map(|p| p.as_ref() as &(dyn tokio_postgres::types::ToSql + Sync))
            .collect();

        let rows = conn.query(&sql, &param_refs).await.map_err(|e| {
            crate::database::pg::pg_err("PG get_fixes_by_workflow_name_filtered", &e)
        })?;

        Ok(rows.iter().map(row_to_fix).collect())
    }

    /// Get reflection history for a workflow (reflection task runs with fix counts).
    #[expect(
        clippy::disallowed_methods,
        reason = "legacy Row::get — migrate to try_get; dossier row-get-panic-kills-spawned-loop"
    )]
    pub async fn get_reflection_history(
        &self,
        workflow_name: &str,
    ) -> Result<Vec<crate::reflection::storage::ReflectionRunSummary>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                r#"
                SELECT
                    tr.id,
                    tr.reflection_source_task_run_id,
                    tr.status,
                    tr.created_at::TEXT,
                    tr.completed_at::TEXT,
                    (SELECT COUNT(*) FROM reflection_fixes rf WHERE rf.reflection_task_run_id = tr.id) as fix_count
                FROM task_runs tr
                WHERE tr.is_reflection = true
                  AND tr.workflow_name LIKE '%' || $1 || '%'
                ORDER BY tr.created_at DESC
                "#,
                &[&workflow_name],
            )
            .await
            .map_err(|e| crate::database::pg::pg_err("PG get_reflection_history", &e))?;

        Ok(rows
            .iter()
            .map(|r| crate::reflection::storage::ReflectionRunSummary {
                task_run_id: r.get(0),
                source_task_run_id: r.get(1),
                status: r.get(2),
                // TIMESTAMPTZ columns cast ::TEXT in SQL so they decode as String.
                // Use try_get to avoid panic if cast is missing or type changes.
                created_at: r.try_get(3).unwrap_or_else(|e| {
                    tracing::warn!(
                        "get_reflection_history: col 3 (created_at) decode error: {}",
                        e
                    );
                    String::new()
                }),
                completed_at: r.try_get(4).unwrap_or_else(|e| {
                    tracing::warn!(
                        "get_reflection_history: col 4 (completed_at) decode error: {}",
                        e
                    );
                    None
                }),
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
    #[expect(
        clippy::disallowed_methods,
        reason = "legacy Row::get — migrate to try_get; dossier row-get-panic-kills-spawned-loop"
    )]
    pub async fn get_applications_for_fix(
        &self,
        fix_id: &str,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                r#"SELECT id, fix_id, task_run_id, error_signature_hash, outcome, applied_at::TEXT, evaluated_at::TEXT
                   FROM fix_applications
                   WHERE fix_id = $1
                   ORDER BY applied_at DESC"#,
                &[&fix_id],
            )
            .await
            .map_err(|e| crate::database::pg::pg_err("PG get_applications_for_fix", &e))?;

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

    // =========================================================================
    // Prediction / Convergence / Knowledge Scoring (PG equivalents)
    // =========================================================================

    /// PG equivalent of prediction::predict_fix_for_error
    #[expect(
        clippy::disallowed_methods,
        reason = "legacy Row::get — migrate to try_get; dossier row-get-panic-kills-spawned-loop"
    )]
    pub async fn predict_fix_for_error(
        &self,
        error_signature_hash: &str,
    ) -> Result<Option<crate::reflection::prediction::PredictedFix>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool: {}", e))?;

        let rows = conn
            .query(
                r#"SELECT fa.fix_id,
                          rf.fix_description,
                          COALESCE(rf.reuse_count, 0) as reuse_count,
                          rf.applied_at::TEXT,
                          COUNT(*) as resolved_count,
                          (SELECT COUNT(*) FROM fix_applications fa2
                           WHERE fa2.fix_id = fa.fix_id) as total_applications
                   FROM fix_applications fa
                   INNER JOIN reflection_fixes rf ON rf.id = fa.fix_id
                   WHERE fa.error_signature_hash = $1
                     AND fa.outcome = 'resolved'
                     AND rf.status = 'applied'
                   GROUP BY fa.fix_id, rf.fix_description, rf.reuse_count, rf.applied_at
                   ORDER BY COUNT(*) DESC
                   LIMIT 5"#,
                &[&error_signature_hash],
            )
            .await
            .map_err(|e| crate::database::pg::pg_err("PG predict_fix", &e))?;

        if rows.is_empty() {
            return Ok(None);
        }

        let now = chrono::Utc::now();
        let mut best: Option<crate::reflection::prediction::PredictedFix> = None;
        let mut best_score = 0.0f64;

        for row in &rows {
            let fix_id: String = row.get(0);
            let description: String = row.get(1);
            let reuse_count: i32 = row.get(2);
            let applied_at: String = row.get(3);
            let resolved_count: i64 = row.get(4);
            let total_applications: i64 = row.get(5);

            let effective_rate = if total_applications > 0 {
                resolved_count as f64 / total_applications as f64
            } else {
                0.0
            };

            let days_old = chrono::DateTime::parse_from_rfc3339(&applied_at)
                .map(|dt| (now - dt.with_timezone(&chrono::Utc)).num_days() as f64)
                .unwrap_or(30.0);
            let recency_weight = 1.0 / (1.0 + days_old * 0.1);

            let score = effective_rate * (reuse_count as f64 + 1.0) * recency_weight;
            if score > best_score {
                best_score = score;
                best = Some(crate::reflection::prediction::PredictedFix {
                    fix_id,
                    fix_description: description,
                    confidence: (effective_rate * recency_weight).min(1.0),
                    reuse_count: reuse_count as u32,
                    effective_rate,
                });
            }
        }

        Ok(best)
    }

    /// PG equivalent of prediction::compute_convergence_score
    #[expect(
        clippy::disallowed_methods,
        reason = "legacy Row::get — migrate to try_get; dossier row-get-panic-kills-spawned-loop"
    )]
    pub async fn compute_convergence_score(
        &self,
        workflow_name: &str,
        scope: &str,
    ) -> Result<crate::reflection::prediction::ConvergenceMetrics, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool: {}", e))?;

        // 1. Count consecutive clean runs
        let rows = conn
            .query(
                r#"SELECT (SELECT COUNT(*) FROM task_run_findings WHERE task_run_id = tr.id)
                   FROM task_runs tr
                   WHERE tr.workflow_name = $1
                     AND (tr.is_reflection = false OR tr.is_reflection IS NULL)
                     AND tr.status IN ('complete', 'completed')
                   ORDER BY tr.completed_at DESC
                   LIMIT 20"#,
                &[&workflow_name],
            )
            .await
            .map_err(|e| crate::database::pg::pg_err("PG convergence clean_runs", &e))?;

        let consecutive_clean = rows
            .iter()
            .map(|r| r.get::<_, i64>(0))
            .take_while(|&c| c == 0)
            .count() as u32;
        let clean_ratio = (consecutive_clean as f64 / 5.0).min(1.0);

        // 2. Novelty score
        let recent_ids: Vec<String> = conn
            .query(
                r#"SELECT id FROM task_runs
                   WHERE workflow_name = $1
                     AND (is_reflection = false OR is_reflection IS NULL)
                     AND status IN ('complete', 'completed')
                   ORDER BY completed_at DESC LIMIT 3"#,
                &[&workflow_name],
            )
            .await
            .map_err(|e| crate::database::pg::pg_err("PG convergence recent_ids", &e))?
            .iter()
            .map(|r| r.get::<_, String>(0))
            .collect();

        let novelty_score = if recent_ids.is_empty() {
            0.0
        } else {
            let recent_unique: i64 = conn
                .query_one(
                    r#"SELECT COUNT(DISTINCT content_hash)
                       FROM reflection_fixes
                       WHERE source_task_run_id = ANY($1)
                         AND content_hash IS NOT NULL"#,
                    &[&recent_ids],
                )
                .await
                .map(|r| r.get(0))
                .unwrap_or(0);

            let total_unique: i64 = conn
                .query_one(
                    r#"SELECT COUNT(DISTINCT rf.content_hash)
                       FROM reflection_fixes rf
                       INNER JOIN task_runs tr ON tr.id = rf.source_task_run_id
                       WHERE tr.workflow_name = $1
                         AND rf.content_hash IS NOT NULL"#,
                    &[&workflow_name],
                )
                .await
                .map(|r| r.get(0))
                .unwrap_or(0);

            if total_unique == 0 {
                0.0
            } else {
                recent_unique as f64 / total_unique as f64
            }
        };

        // 3. Effective fix rate
        let rate_row = conn
            .query_one(
                // COALESCE wraps every SUM because Postgres returns NULL (not
                // 0) for SUM when the WHERE clause filters out every row — and
                // `r.get::<_, i64>(col)` panics on NULL with no nullable type.
                // COUNT(*) is safe (always 0+) so it's left bare.
                r#"SELECT
                     COUNT(*),
                     COALESCE(SUM(CASE WHEN rf.effectiveness = 'effective' THEN 1 ELSE 0 END), 0)::BIGINT,
                     COALESCE(SUM(CASE WHEN rf.effectiveness = 'ineffective' THEN 1 ELSE 0 END), 0)::BIGINT,
                     COALESCE(SUM(CASE WHEN rf.effectiveness = 'caused_regression' THEN 1 ELSE 0 END), 0)::BIGINT
                   FROM reflection_fixes rf
                   INNER JOIN task_runs tr ON tr.id = rf.source_task_run_id
                   WHERE tr.workflow_name = $1
                     AND rf.reflection_scope = $2
                     AND rf.effectiveness IS NOT NULL"#,
                &[&workflow_name, &scope],
            )
            .await
            .ok();

        let (total_fixes, effective_fixes, effective_rate) = match rate_row {
            Some(r) => {
                let total: i64 = r.get(0);
                let effective: i64 = r.get(1);
                let ineffective: i64 = r.get(2);
                let regression: i64 = r.get(3);
                let denom = effective + ineffective + regression;
                let rate = if denom > 0 {
                    effective as f64 / denom as f64
                } else {
                    1.0
                };
                (total as u32, effective as u32, rate)
            }
            None => (0, 0, 1.0),
        };

        // 4. Change velocity
        let fix_count: i64 = conn
            .query_one(
                r#"SELECT COUNT(*)
                   FROM reflection_fixes rf
                   INNER JOIN task_runs tr ON tr.id = rf.source_task_run_id
                   WHERE tr.workflow_name = $1
                     AND (tr.is_reflection = false OR tr.is_reflection IS NULL)
                     AND tr.completed_at >= (
                         SELECT COALESCE(MIN(completed_at), '1970-01-01')
                         FROM (
                             SELECT completed_at FROM task_runs
                             WHERE workflow_name = $1
                               AND (is_reflection = false OR is_reflection IS NULL)
                               AND status IN ('complete', 'completed')
                             ORDER BY completed_at DESC
                             LIMIT 5
                         ) sub
                     )"#,
                &[&workflow_name],
            )
            .await
            .map(|r| r.get(0))
            .unwrap_or(0);
        let change_velocity = fix_count as f64 / 5.0;

        let score = clean_ratio * (1.0 - novelty_score) * effective_rate.max(0.1);

        Ok(crate::reflection::prediction::ConvergenceMetrics {
            score: score.min(1.0),
            consecutive_clean_runs: consecutive_clean,
            novelty_score,
            effective_fix_rate: effective_rate,
            change_velocity,
            total_fixes,
            effective_fixes,
        })
    }

    /// PG equivalent of prediction::compute_change_velocity
    #[expect(
        clippy::disallowed_methods,
        reason = "legacy Row::get — migrate to try_get; dossier row-get-panic-kills-spawned-loop"
    )]
    pub async fn compute_change_velocity(
        &self,
        component_path: &str,
        window_size: u32,
    ) -> Result<f64, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool: {}", e))?;
        let window_i64 = window_size as i64;

        let fix_count: i64 = conn
            .query_one(
                r#"SELECT COUNT(*)
                   FROM reflection_fixes
                   WHERE target_component = $1
                     AND applied_at >= (
                         SELECT COALESCE(MIN(applied_at), '1970-01-01')
                         FROM (
                             SELECT DISTINCT applied_at FROM reflection_fixes
                             WHERE target_component = $1
                             ORDER BY applied_at DESC
                             LIMIT $2
                         ) sub
                     )"#,
                &[&component_path, &window_i64],
            )
            .await
            .map(|r| r.get(0))
            .unwrap_or(0);

        Ok(fix_count as f64 / window_size as f64)
    }

    /// PG equivalent of prediction::score_knowledge_relevance
    #[expect(
        clippy::disallowed_methods,
        reason = "legacy Row::get — migrate to try_get; dossier row-get-panic-kills-spawned-loop"
    )]
    pub async fn score_knowledge_relevance(
        &self,
        workflow_name: &str,
        limit: u32,
    ) -> Result<Vec<crate::reflection::prediction::ScoredKnowledge>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool: {}", e))?;

        let rows = conn
            .query(
                r#"SELECT tk.id, tk.content, tk.category, tk.last_validated_at,
                          tk.reflection_fix_id, tk.created_at
                   FROM task_knowledge tk
                   INNER JOIN task_runs tr ON tr.id = tk.task_run_id
                   WHERE tr.workflow_name = $1
                     AND (tk.is_resolved = false OR tk.is_resolved IS NULL)
                     AND tk.category IN ('recurring_pattern', 'context', 'finding', 'root_cause',
                                         'project_environment', 'project_architecture',
                                         'project_test_pattern', 'project_recurring_issue')
                   ORDER BY tk.created_at DESC
                   LIMIT 50"#,
                &[&workflow_name],
            )
            .await
            .map_err(|e| crate::database::pg::pg_err("PG score_knowledge", &e))?;

        let now = chrono::Utc::now();
        let mut scored: Vec<crate::reflection::prediction::ScoredKnowledge> = Vec::new();

        for row in &rows {
            let id: String = row.get(0);
            let content: String = row.get(1);
            let category: String = row.get(2);
            let last_validated_at: Option<String> = row.get(3);
            let reflection_fix_id: Option<String> = row.get(4);
            let created_at: String = row.get(5);

            // Effectiveness weight from linked fix
            let effectiveness_weight = if let Some(ref fix_id) = reflection_fix_id {
                let eff: Option<String> = conn
                    .query_one(
                        "SELECT effectiveness FROM reflection_fixes WHERE id = $1",
                        &[&fix_id],
                    )
                    .await
                    .ok()
                    .and_then(|r| r.get(0));

                match eff.as_deref() {
                    Some("effective") => 1.0,
                    Some("ineffective") | Some("caused_regression") => 0.0,
                    Some("inconclusive") => 0.5,
                    _ => 0.5,
                }
            } else {
                0.7
            };

            // Recency weight
            let reference_date = last_validated_at.as_deref().unwrap_or(&created_at);
            let days_old = chrono::DateTime::parse_from_rfc3339(reference_date)
                .map(|dt| (now - dt.with_timezone(&chrono::Utc)).num_days() as f64)
                .unwrap_or(30.0);
            let recency_weight = 1.0 / (1.0 + days_old * 0.1);

            // Velocity factor from linked fix's target_component
            let velocity_factor = if let Some(ref fix_id) = reflection_fix_id {
                let comp: Option<String> = conn
                    .query_one(
                        "SELECT target_component FROM reflection_fixes WHERE id = $1",
                        &[&fix_id],
                    )
                    .await
                    .ok()
                    .and_then(|r| r.get(0));

                if let Some(comp) = comp {
                    let velocity = self.compute_change_velocity(&comp, 5).await.unwrap_or(0.0);
                    1.0 / (1.0 + velocity)
                } else {
                    1.0
                }
            } else {
                1.0
            };

            let relevance_score = effectiveness_weight * recency_weight * velocity_factor;

            scored.push(crate::reflection::prediction::ScoredKnowledge {
                knowledge_id: id,
                content,
                category,
                relevance_score,
                effectiveness_weight,
                recency_weight,
                velocity_factor,
            });
        }

        scored.sort_by(|a, b| {
            b.relevance_score
                .partial_cmp(&a.relevance_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        scored.truncate(limit as usize);
        Ok(scored)
    }
}

// ============================================================================
// Fix effectiveness — Reflection judges a fix only after the fix was exposed
// (plan 2026-11-06-a-product-workflow-verification-with-nothing-to-check-never-reports-passed,
// Phase 4, design decision A2-D3).
//
// The evaluator this replaced stamped a fix `effective` whenever its count of
// later findings read zero, and read a FAILED count as zero (`.unwrap_or(0)`).
// Measured 2026-11-07 on a database loaded from `schema.pg.sql.generated`, it
// never reached that count: its pending-fix read selected the bare
// `SELECT_ALL_COLUMNS` through a join to `task_runs` and failed with
// `column reference "id" is ambiguous`; and the count itself bound the fix's
// `created_at::TEXT` copy (a `String`) to `tr.completed_at > $2`, a
// `timestamptz` parameter postgres-types refuses before the query runs.
// ============================================================================

/// Completed runs of a fix's workflow after `applied_at` that a fix must have
/// been exposed to before it is judged at all. With fewer, `effectiveness`
/// stays NULL (unevaluated) — never `effective`.
const MIN_LATER_RUNS_FOR_EFFECTIVENESS: i64 = 3;

/// A signature first seen after `applied_at` is a regression the fix caused
/// once it appears in at least this many later runs.
const MIN_LATER_RUNS_FOR_REGRESSION: i64 = 2;

/// What the evaluator observed about one fix: every count taken in SQL
/// ([`FIX_EXPOSURE_SQL`]) over `task_runs.completed_at > reflection_fixes.applied_at`,
/// joined to the fix row by its text id. That compares two `timestamptz`
/// columns; this module reads `applied_at` / `created_at` only as `TEXT`
/// copies (`SELECT_ALL_COLUMNS`), which must never be bound back as a
/// timestamp.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FixExposure {
    /// Completed runs of the fix's workflow after the fix was applied.
    later_runs: i64,
    /// The fix names the finding it addressed (`source_finding_id` is set).
    has_source_finding: bool,
    /// That finding resolves to a `signature_hash`, so a recurrence is observable.
    source_signature_known: bool,
    /// Later runs whose findings carry the source finding's signature again.
    recurrences: i64,
    /// The most later runs that any ONE signature unseen up to `applied_at`
    /// (the source signature aside) appeared in.
    new_signature_peak_runs: i64,
}

/// A2-D3. `None` is "not judged": the fix stays unevaluated.
///
/// The rows are tested in the decision's order, so exposure comes first: a fix
/// with fewer than [`MIN_LATER_RUNS_FOR_EFFECTIVENESS`] later runs is not
/// judged even when it has no finding or its finding already recurred. A fix
/// whose recurrence cannot be observed — no source finding, or one with no
/// signature — is `inconclusive`: zero recurrences of something nobody can see
/// recur is not evidence the fix worked.
fn decide_fix_effectiveness(exposure: &FixExposure) -> Option<FixEffectiveness> {
    if exposure.later_runs < MIN_LATER_RUNS_FOR_EFFECTIVENESS {
        return None;
    }
    if !exposure.has_source_finding || !exposure.source_signature_known {
        return Some(FixEffectiveness::Inconclusive);
    }
    if exposure.recurrences > 0 {
        return Some(FixEffectiveness::Ineffective);
    }
    if exposure.new_signature_peak_runs >= MIN_LATER_RUNS_FOR_REGRESSION {
        return Some(FixEffectiveness::CausedRegression);
    }
    Some(FixEffectiveness::Effective)
}

/// One pending fix's verdict from its exposure READ: A2-D3 including its
/// failed-read row. An unreadable exposure is NO verdict — warn, write
/// nothing, the fix stays unevaluated — and never a zero count read as
/// `effective`. That covers a connection lost mid-loop too. An outage is
/// recognized only at the evaluator's one pool checkout, which already
/// propagates it; every exposure read then runs on that held connection, and
/// a failed query on it reads the same whether or not the connection is gone
/// (qontinui-runner#2385's `or_else_unless_unavailable` decides by the same
/// token, so it would not change this path). Telling the two apart would
/// mean classifying the `tokio_postgres::Error` (for example `is_closed()`)
/// before `pg_err` turns it into a string. The fix stays pending and the next
/// evaluation judges it. `Some` carries the verdict and the evidence JSON it
/// rests on.
fn judge_fix_exposure_read(
    fix_id: &str,
    read: Result<FixExposure, String>,
) -> Option<(FixEffectiveness, String)> {
    let exposure = match read {
        Ok(exposure) => exposure,
        Err(e) => {
            warn!(
                fix_id,
                error = %e,
                "reflection fix exposure unreadable — no verdict written; the fix stays unevaluated"
            );
            return None;
        }
    };
    decide_fix_effectiveness(&exposure)
        .map(|verdict| (verdict, verdict_evidence_json(&exposure, verdict)))
}

/// The counts a verdict rests on, written to `effectiveness_evidence`. Every
/// verdict this evaluator writes carries it, so a NULL evidence beside a
/// verdict marks one nothing judged on exposure (the evaluator this replaced
/// wrote none).
fn verdict_evidence_json(exposure: &FixExposure, verdict: FixEffectiveness) -> String {
    serde_json::json!({
        "verdict": verdict.as_str(),
        "window": "task_runs.completed_at > reflection_fixes.applied_at",
        "later_runs": exposure.later_runs,
        "min_later_runs": MIN_LATER_RUNS_FOR_EFFECTIVENESS,
        "source_finding": exposure.has_source_finding,
        "source_signature_known": exposure.source_signature_known,
        "recurrences": exposure.recurrences,
        "new_signature_peak_runs": exposure.new_signature_peak_runs,
        "min_later_runs_for_regression": MIN_LATER_RUNS_FOR_REGRESSION,
    })
    .to_string()
}

/// The evaluator's first statement: the scoped, idempotent re-queue. Within
/// the evaluated workflow (`$1`) it withdraws every `effective` (`$2`) stamp
/// that nothing judged on exposure:
///
/// * `effectiveness_evidence IS NULL` — this evaluator writes evidence with
///   every verdict, and Phase 4's re-measure (2026-11-07) found the evaluator
///   it replaced judged nothing: its count never ran (see the section note).
///   This is the scope the re-measure selected.
/// * no observable source finding, or fewer than `$3`
///   ([`MIN_LATER_RUNS_FOR_EFFECTIVENESS`]) completed runs after `applied_at` —
///   A2-D3's own scope, which also withdraws an `effective` set by hand
///   (`PUT /reflection-fixes/:id/effectiveness`) without that exposure.
///
/// It does not withdraw this evaluator's own verdicts while their exposure
/// stands: those carry evidence and are written `effective` only with it.
/// One can still go back to unevaluated if its source finding is deleted
/// (`source_finding_id` is `ON DELETE SET NULL`), which leaves no observable
/// finding. A re-queued row's evidence goes with its verdict.
const REQUEUE_UNEXPOSED_EFFECTIVE_SQL: &str = r#"
    UPDATE reflection_fixes rf
       SET effectiveness = NULL, effectiveness_evidence = NULL, evaluated_at = NULL
      FROM task_runs src
     WHERE src.id = rf.source_task_run_id
       AND src.workflow_name = $1
       AND rf.effectiveness = $2
       AND (rf.effectiveness_evidence IS NULL
            OR NOT EXISTS (SELECT 1
                             FROM task_run_findings f
                            WHERE f.id = rf.source_finding_id
                              AND f.signature_hash IS NOT NULL)
            OR (SELECT COUNT(*)
                  FROM task_runs later
                 WHERE later.workflow_name = src.workflow_name
                   AND later.id <> rf.source_task_run_id
                   AND later.completed_at > rf.applied_at) < $3)
"#;

/// One fix's [`FixExposure`], joined to the fix row by its text id (`$1`).
/// "Later" is `completed_at > rf.applied_at` and "before" is
/// `completed_at <= rf.applied_at`, over runs of the fix's own workflow; a run
/// that never completed is neither, and the fix's own source run is never a
/// later run (a manual trigger can apply a fix before that run completes). A
/// new signature is one no finding of a run completed by `applied_at`, nor of
/// the source run itself, carried.
const FIX_EXPOSURE_SQL: &str = r#"
    WITH fix AS (
        SELECT rf.applied_at,
               rf.source_task_run_id AS source_run_id,
               src.workflow_name,
               rf.source_finding_id IS NOT NULL AS has_source_finding,
               (SELECT f.signature_hash
                  FROM task_run_findings f
                 WHERE f.id = rf.source_finding_id) AS source_signature
          FROM reflection_fixes rf
          JOIN task_runs src ON src.id = rf.source_task_run_id
         WHERE rf.id = $1
    ),
    later_runs AS (
        SELECT tr.id
          FROM task_runs tr, fix
         WHERE tr.workflow_name = fix.workflow_name
           AND tr.id <> fix.source_run_id
           AND tr.completed_at > fix.applied_at
    ),
    new_signature_runs AS (
        SELECT trf.signature_hash, COUNT(DISTINCT trf.task_run_id) AS runs
          FROM task_run_findings trf
          JOIN later_runs lr ON lr.id = trf.task_run_id
         CROSS JOIN fix
         WHERE trf.signature_hash IS NOT NULL
           AND trf.signature_hash IS DISTINCT FROM fix.source_signature
           AND NOT EXISTS (
               SELECT 1
                 FROM task_run_findings prior
                 JOIN task_runs pr ON pr.id = prior.task_run_id
                WHERE pr.workflow_name = fix.workflow_name
                  AND (pr.completed_at <= fix.applied_at OR pr.id = fix.source_run_id)
                  AND prior.signature_hash = trf.signature_hash)
         GROUP BY trf.signature_hash
    )
    SELECT (SELECT COUNT(*) FROM later_runs) AS later_runs,
           fix.has_source_finding,
           fix.source_signature IS NOT NULL AS source_signature_known,
           (SELECT COUNT(DISTINCT trf.task_run_id)
              FROM task_run_findings trf
              JOIN later_runs lr ON lr.id = trf.task_run_id
             WHERE fix.source_signature IS NOT NULL
               AND trf.signature_hash = fix.source_signature) AS recurrences,
           COALESCE((SELECT MAX(runs) FROM new_signature_runs), 0) AS new_signature_peak_runs
      FROM fix
"#;

impl PgDb {
    /// Judge the applied, unevaluated fixes of `workflow_name`, and return ONLY
    /// the rows this call wrote, each carrying the effectiveness it wrote —
    /// `POST /reflection/evaluate` counts exactly these.
    ///
    /// It first runs the scoped re-queue ([`REQUEUE_UNEXPOSED_EFFECTIVE_SQL`]).
    /// It then counts each pending fix's exposure in SQL ([`FIX_EXPOSURE_SQL`])
    /// and judges it by [`decide_fix_effectiveness`]: a fix with fewer than
    /// [`MIN_LATER_RUNS_FOR_EFFECTIVENESS`] later runs stays unevaluated, and
    /// an unreadable exposure writes nothing ([`judge_fix_exposure_read`]). A
    /// verdict is written with the counts it rests on as
    /// `effectiveness_evidence`, and `evaluated_at = NOW()` in SQL.
    pub async fn evaluate_pending_fixes(
        &self,
        workflow_name: &str,
    ) -> Result<Vec<ReflectionFix>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool: {}", e))?;

        let requeued = conn
            .execute(
                REQUEUE_UNEXPOSED_EFFECTIVE_SQL,
                &[
                    &workflow_name,
                    &FixEffectiveness::Effective.as_str(),
                    &MIN_LATER_RUNS_FOR_EFFECTIVENESS,
                ],
            )
            .await
            .map_err(|e| crate::database::pg::pg_err("PG requeue unexposed effective fixes", &e))?;

        // Every column `rf.`-qualified: a bare SELECT_ALL_COLUMNS through this
        // join is ambiguous (`id`, `status`, `created_at`). Oldest exposure
        // first: an unexposed fix now STAYS pending, so newest-first would let
        // a hundred recent unexposed fixes keep an older, exposed one out of
        // the LIMIT for good.
        let pending_sql = format!(
            "SELECT {} FROM reflection_fixes rf \
             INNER JOIN task_runs tr ON tr.id = rf.source_task_run_id \
             WHERE tr.workflow_name = $1 \
               AND rf.status = 'applied' \
               AND rf.effectiveness IS NULL \
             ORDER BY rf.applied_at ASC, rf.id ASC \
             LIMIT 100",
            prefixed_select_columns("rf")
        );
        let pending: Vec<ReflectionFix> = conn
            .query(&pending_sql, &[&workflow_name])
            .await
            .map_err(|e| crate::database::pg::pg_err("PG evaluate_pending", &e))?
            .iter()
            .map(row_to_fix)
            .collect();

        let write_sql = format!(
            "UPDATE reflection_fixes \
             SET effectiveness = $1, effectiveness_evidence = $2, evaluated_at = NOW() \
             WHERE id = $3 AND effectiveness IS NULL \
             RETURNING {}",
            SELECT_ALL_COLUMNS
        );
        let mut written = Vec::new();
        for fix in &pending {
            let read = Self::read_fix_exposure(&conn, &fix.id).await;
            let Some((verdict, evidence)) = judge_fix_exposure_read(&fix.id, read) else {
                continue;
            };
            match conn
                .query_opt(&write_sql, &[&verdict.as_str(), &evidence, &fix.id])
                .await
            {
                Ok(Some(row)) => written.push(row_to_fix(&row)),
                // Another evaluation wrote this fix first: not this call's row.
                Ok(None) => debug!(
                    fix_id = %fix.id,
                    "reflection fix judged elsewhere first; this verdict was not written"
                ),
                Err(e) => warn!(
                    fix_id = %fix.id,
                    error = %crate::database::pg::pg_err("PG write fix effectiveness", &e),
                    "reflection fix verdict not written; the fix stays unevaluated"
                ),
            }
        }

        info!(
            workflow_name,
            requeued,
            pending = pending.len(),
            written = written.len(),
            "reflection fix effectiveness evaluated"
        );
        Ok(written)
    }

    /// [`FIX_EXPOSURE_SQL`] for one fix. `Err` when the read fails, or when the
    /// fix or its source run is gone.
    async fn read_fix_exposure(
        conn: &tokio_postgres::Client,
        fix_id: &str,
    ) -> Result<FixExposure, String> {
        let row = conn
            .query_opt(FIX_EXPOSURE_SQL, &[&fix_id])
            .await
            .map_err(|e| crate::database::pg::pg_err("PG fix exposure", &e))?
            .ok_or_else(|| format!("PG fix exposure: fix {fix_id} or its source run is gone"))?;
        let column =
            |name: &str, e: tokio_postgres::Error| format!("PG fix exposure: column {name}: {e}");
        Ok(FixExposure {
            later_runs: row
                .try_get("later_runs")
                .map_err(|e| column("later_runs", e))?,
            has_source_finding: row
                .try_get("has_source_finding")
                .map_err(|e| column("has_source_finding", e))?,
            source_signature_known: row
                .try_get("source_signature_known")
                .map_err(|e| column("source_signature_known", e))?,
            recurrences: row
                .try_get("recurrences")
                .map_err(|e| column("recurrences", e))?,
            new_signature_peak_runs: row
                .try_get("new_signature_peak_runs")
                .map_err(|e| column("new_signature_peak_runs", e))?,
        })
    }

    /// PG equivalent of cross_run_learning::post_run_analysis (simplified)
    /// Returns (patterns_detected, rules_disabled, fixes_auto_applied)
    #[expect(
        clippy::disallowed_methods,
        reason = "legacy Row::get — migrate to try_get; dossier row-get-panic-kills-spawned-loop"
    )]
    pub async fn post_run_analysis(
        &self,
        workflow_name: &str,
        _task_run_id: &str,
    ) -> Result<(u32, u32, u32), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool: {}", e))?;

        // Detect recurring findings (simplified: count finding signatures with 3+ occurrences)
        let patterns: i64 = conn
            .query_one(
                r#"SELECT COUNT(DISTINCT trf.signature_hash)
                   FROM task_run_findings trf
                   INNER JOIN task_runs tr ON tr.id = trf.task_run_id
                   WHERE tr.workflow_name = $1
                     AND trf.signature_hash IS NOT NULL
                   GROUP BY trf.signature_hash
                   HAVING COUNT(*) >= 3"#,
                &[&workflow_name],
            )
            .await
            .map(|r| r.get(0))
            .unwrap_or(0);

        // Count patterns from cross_run_patterns table
        let pattern_count: i64 = conn
            .query_one(
                "SELECT COUNT(*) FROM cross_run_patterns WHERE workflow_name = $1 AND is_active = true",
                &[&workflow_name],
            )
            .await
            .map(|r| r.get(0))
            .unwrap_or(patterns);

        Ok((pattern_count as u32, 0, 0))
    }

    /// PG equivalent of fuzzy_matching::find_similar_errors (text-only, ILIKE based)
    #[expect(
        clippy::disallowed_methods,
        reason = "legacy Row::get — migrate to try_get; dossier row-get-panic-kills-spawned-loop"
    )]
    pub async fn find_similar_errors(
        &self,
        error_description: &str,
        min_similarity: f64,
        limit: usize,
    ) -> Result<Vec<crate::reflection::fuzzy_matching::SimilarError>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool: {}", e))?;

        // Use ILIKE for basic text matching since PG trigram extension may not be installed
        let search_term = format!("%{}%", truncate_str(error_description, 50));
        let limit_i64 = (limit * 2) as i64; // fetch more, then filter by similarity

        let rows = conn
            .query(
                r#"SELECT id, signature_hash, message
                   FROM error_events
                   WHERE message ILIKE $1
                     AND status != 'resolved'
                   ORDER BY last_seen_at DESC
                   LIMIT $2"#,
                &[&search_term, &limit_i64],
            )
            .await
            .map_err(|e| crate::database::pg::pg_err("PG find_similar_errors", &e))?;

        let mut results = Vec::new();
        for row in &rows {
            let id: String = row.get(0);
            let sig: String = row.get(1);
            let msg: String = row.get(2);
            let sim =
                crate::reflection::fuzzy_matching::trigram_similarity(error_description, &msg);
            if sim >= min_similarity {
                results.push(crate::reflection::fuzzy_matching::SimilarError {
                    error_id: id,
                    signature_hash: sig,
                    message: msg,
                    similarity: sim,
                });
            }
        }

        results.sort_by(|a, b| {
            b.similarity
                .partial_cmp(&a.similarity)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        results.truncate(limit);
        Ok(results)
    }

    // ========================================================================
    // Reflection Trigger Guards (PG equivalents of trigger.rs functions)
    // ========================================================================

    /// PG equivalent of trigger::should_launch_reflection.
    /// Returns false if reflection should be skipped.
    #[expect(
        clippy::disallowed_methods,
        reason = "legacy Row::get — migrate to try_get; dossier row-get-panic-kills-spawned-loop"
    )]
    pub async fn should_launch_reflection(&self, source_task_run_id: &str) -> Result<bool, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool: {}", e))?;

        // Guard 0: Check if a reflection for the SAME workflow is already running
        let has_running: bool = conn
            .query_one(
                r#"SELECT COUNT(*) > 0 FROM task_runs r
                   WHERE r.status = 'running'
                     AND r.is_reflection = true
                     AND r.workflow_type = 'unified'
                     AND r.reflection_source_task_run_id IN (
                         SELECT s.id FROM task_runs s
                         WHERE s.workflow_name = (
                             SELECT workflow_name FROM task_runs WHERE id = $1
                         )
                     )"#,
                &[&source_task_run_id],
            )
            .await
            .map(|r| r.get(0))
            .unwrap_or(false);

        if has_running {
            return Ok(false);
        }

        // Guard 1: Skip if source is already a reflection run
        let is_reflection: bool = conn
            .query_one(
                "SELECT COALESCE(is_reflection, false) FROM task_runs WHERE id = $1",
                &[&source_task_run_id],
            )
            .await
            .map(|r| r.get(0))
            .unwrap_or(false);

        if is_reflection {
            return Ok(false);
        }

        // Guard 2: Check if source run has any findings worth reflecting on
        let finding_count: i64 = conn
            .query_one(
                "SELECT COUNT(*) FROM task_run_findings WHERE task_run_id = $1",
                &[&source_task_run_id],
            )
            .await
            .map(|r| r.get(0))
            .unwrap_or(0);

        Ok(finding_count > 0)
    }

    /// PG equivalent of trigger::should_launch_project_reflection.
    #[expect(
        clippy::disallowed_methods,
        reason = "legacy Row::get — migrate to try_get; dossier row-get-panic-kills-spawned-loop"
    )]
    pub async fn should_launch_project_reflection(
        &self,
        source_task_run_id: &str,
    ) -> Result<bool, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool: {}", e))?;

        // Guard 0: Block if a project reflection for the SAME workflow is already running
        let has_running: bool = conn
            .query_one(
                r#"SELECT COUNT(*) > 0 FROM task_runs r
                   WHERE r.status = 'running'
                     AND r.is_reflection = true
                     AND r.workflow_type = 'unified'
                     AND r.workflow_name LIKE 'Project Reflection:%'
                     AND r.reflection_source_task_run_id IN (
                         SELECT s.id FROM task_runs s
                         WHERE s.workflow_name = (
                             SELECT workflow_name FROM task_runs WHERE id = $1
                         )
                     )"#,
                &[&source_task_run_id],
            )
            .await
            .map(|r| r.get(0))
            .unwrap_or(false);

        if has_running {
            return Ok(false);
        }

        // Guard 1: Skip if source is already a reflection run
        let is_reflection: bool = conn
            .query_one(
                "SELECT COALESCE(is_reflection, false) FROM task_runs WHERE id = $1",
                &[&source_task_run_id],
            )
            .await
            .map(|r| r.get(0))
            .unwrap_or(false);

        if is_reflection {
            return Ok(false);
        }

        // Guard 2: Check project-scoped convergence
        let workflow_name: Option<String> = conn
            .query_opt(
                "SELECT COALESCE(workflow_name, task_name) FROM task_runs WHERE id = $1",
                &[&source_task_run_id],
            )
            .await
            .map_err(|e| crate::database::pg::pg_err("PG should_launch_project_reflection", &e))?
            .map(|r| r.get(0));

        if let Some(ref wf) = workflow_name {
            let metrics = self.compute_convergence_score(wf, "project").await;
            if let Ok(m) = metrics {
                if m.score >= 0.8 {
                    return Ok(false);
                }
            }
        }

        // Guard 3: Must have project-scoped findings
        let project_finding_count: i64 = conn
            .query_one(
                r#"SELECT COUNT(*) FROM task_run_findings trf
                   INNER JOIN task_runs tr ON tr.id = trf.task_run_id
                   WHERE trf.task_run_id = $1
                     AND trf.category IN ('project_environment', 'project_architecture',
                                          'project_test_pattern', 'project_recurring_issue')"#,
                &[&source_task_run_id],
            )
            .await
            .map(|r| r.get(0))
            .unwrap_or(0);

        Ok(project_finding_count > 0)
    }

    /// PG equivalent of trigger::should_launch_ui_bridge_reflection.
    #[expect(
        clippy::disallowed_methods,
        reason = "legacy Row::get — migrate to try_get; dossier row-get-panic-kills-spawned-loop"
    )]
    pub async fn should_launch_ui_bridge_reflection(
        &self,
        source_task_run_id: &str,
    ) -> Result<bool, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool: {}", e))?;

        // Guard 0: Block if a UI Bridge reflection for the SAME workflow is already running
        let has_running: bool = conn
            .query_one(
                r#"SELECT COUNT(*) > 0 FROM task_runs r
                   WHERE r.status = 'running'
                     AND r.is_reflection = true
                     AND r.workflow_type = 'unified'
                     AND r.workflow_name LIKE 'UI Bridge Reflection:%'
                     AND r.reflection_source_task_run_id IN (
                         SELECT s.id FROM task_runs s
                         WHERE s.workflow_name = (
                             SELECT workflow_name FROM task_runs WHERE id = $1
                         )
                     )"#,
                &[&source_task_run_id],
            )
            .await
            .map(|r| r.get(0))
            .unwrap_or(false);

        if has_running {
            return Ok(false);
        }

        // Guard 1: Skip if source is already a reflection run
        let is_reflection: bool = conn
            .query_one(
                "SELECT COALESCE(is_reflection, false) FROM task_runs WHERE id = $1",
                &[&source_task_run_id],
            )
            .await
            .map(|r| r.get(0))
            .unwrap_or(false);

        if is_reflection {
            return Ok(false);
        }

        // Guard 2: Check if source run used UI Bridge
        let has_ui_bridge: bool = conn
            .query_one(
                r#"SELECT COUNT(*) > 0 FROM task_run_events
                   WHERE task_run_id = $1
                     AND event_type = 'ui_bridge'"#,
                &[&source_task_run_id],
            )
            .await
            .map(|r| r.get(0))
            .unwrap_or(false);

        Ok(has_ui_bridge)
    }

    /// Get workflow name for a task run (COALESCE of workflow_name and task_name).
    #[expect(
        clippy::disallowed_methods,
        reason = "legacy Row::get — migrate to try_get; dossier row-get-panic-kills-spawned-loop"
    )]
    pub async fn get_workflow_name_for_task_run(
        &self,
        task_run_id: &str,
    ) -> Result<String, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool: {}", e))?;

        conn.query_one(
            "SELECT COALESCE(workflow_name, task_name) FROM task_runs WHERE id = $1",
            &[&task_run_id],
        )
        .await
        .map(|r| r.get(0))
        .map_err(|e| {
            format!(
                "Failed to get workflow name for task run {}: {}",
                task_run_id, e
            )
        })
    }

    /// Get workflow name and project path for a task run (used by project reflection).
    #[expect(
        clippy::disallowed_methods,
        reason = "legacy Row::get — migrate to try_get; dossier row-get-panic-kills-spawned-loop"
    )]
    pub async fn get_workflow_name_and_project_path(
        &self,
        task_run_id: &str,
    ) -> Result<(String, Option<String>), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool: {}", e))?;

        let wf_name: String = conn
            .query_one(
                "SELECT COALESCE(workflow_name, task_name) FROM task_runs WHERE id = $1",
                &[&task_run_id],
            )
            .await
            .map(|r| r.get(0))
            .map_err(|e| crate::database::pg::pg_err("Failed to get task run", &e))?;

        // Get project path from unified_workflows setup_steps
        let proj_path: Option<String> = conn
            .query_opt(
                r#"SELECT COALESCE(
                    s->>'shell_command_working_directory',
                    s->>'working_directory'
                )
                FROM unified_workflows uw,
                     jsonb_array_elements(uw.setup_steps::jsonb) AS s
                INNER JOIN task_runs tr ON tr.workflow_name = uw.name
                WHERE tr.id = $1
                  AND COALESCE(
                    s->>'shell_command_working_directory',
                    s->>'working_directory'
                  ) IS NOT NULL
                LIMIT 1"#,
                &[&task_run_id],
            )
            .await
            .ok()
            .flatten()
            .map(|r| r.get(0));

        Ok((wf_name, proj_path))
    }

    /// Get the source task run ID for a reflection task run (for inferred context).
    #[expect(
        clippy::disallowed_methods,
        reason = "legacy Row::get — migrate to try_get; dossier row-get-panic-kills-spawned-loop"
    )]
    pub async fn get_source_task_run_id_for_reflection(
        &self,
        reflection_task_run_id: &str,
    ) -> Result<Option<String>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool: {}", e))?;

        let row = conn
            .query_opt(
                "SELECT reflection_source_task_run_id FROM task_runs WHERE id = $1",
                &[&reflection_task_run_id],
            )
            .await
            .map_err(|e| crate::database::pg::pg_err("PG get_source_task_run_id", &e))?;

        Ok(row.and_then(|r| r.get(0)))
    }

    /// Update reflection fix with scope (universal or project).
    pub async fn update_fix_scope(
        &self,
        fix_id: &str,
        scope: &str,
        project_path: Option<&str>,
        applicability_context: Option<&str>,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool: {}", e))?;

        conn.execute(
            r#"UPDATE reflection_fixes
               SET reflection_scope = $1,
                   project_path = COALESCE($2, project_path),
                   applicability_context = COALESCE($3, applicability_context)
               WHERE id = $4"#,
            &[
                &scope as &(dyn tokio_postgres::types::ToSql + Sync),
                &project_path as &(dyn tokio_postgres::types::ToSql + Sync),
                &applicability_context as &(dyn tokio_postgres::types::ToSql + Sync),
                &fix_id,
            ],
        )
        .await
        .map_err(|e| crate::database::pg::pg_err("PG update_fix_scope", &e))?;

        Ok(())
    }

    /// Link error_events to a fix (set resolved_by_fix_id).
    pub async fn link_error_events_to_fix(
        &self,
        fix_id: &str,
        finding_id: &str,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool: {}", e))?;

        conn.execute(
            r#"UPDATE error_events SET resolved_by_fix_id = $1
               WHERE signature_hash = (
                   SELECT signature_hash FROM task_run_findings WHERE id = $2
               ) AND resolved_by_fix_id IS NULL"#,
            &[&fix_id, &finding_id],
        )
        .await
        .map_err(|e| crate::database::pg::pg_err("PG link_error_events_to_fix", &e))?;

        Ok(())
    }

    /// Store convergence snapshot.
    pub async fn store_convergence_snapshot(
        &self,
        workflow_name: &str,
        project_path: Option<&str>,
        scope: &str,
        metrics: &crate::reflection::prediction::ConvergenceMetrics,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool: {}", e))?;

        let id = uuid::Uuid::new_v4().to_string();

        // The table's own column names, every NOT NULL column supplied, `i32`
        // for its `integer` columns, and `snapshot_at` stamped by `NOW()` in
        // SQL: postgres-types refuses a Rust `String` bound to a `timestamptz`
        // and an `i64` bound to an `integer`, so the INSERT this replaced
        // (`score`, `created_at`, no `effective_fixes`) never ran.
        conn.execute(
            r#"INSERT INTO convergence_snapshots
               (id, workflow_name, project_path, scope, convergence_score,
                consecutive_clean_runs, novelty_score, effective_fix_rate,
                change_velocity, total_fixes, effective_fixes, snapshot_at)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, NOW())"#,
            &[
                &id as &(dyn tokio_postgres::types::ToSql + Sync),
                &workflow_name as &(dyn tokio_postgres::types::ToSql + Sync),
                &project_path as &(dyn tokio_postgres::types::ToSql + Sync),
                &scope as &(dyn tokio_postgres::types::ToSql + Sync),
                &metrics.score as &(dyn tokio_postgres::types::ToSql + Sync),
                &(metrics.consecutive_clean_runs as i32)
                    as &(dyn tokio_postgres::types::ToSql + Sync),
                &metrics.novelty_score as &(dyn tokio_postgres::types::ToSql + Sync),
                &metrics.effective_fix_rate as &(dyn tokio_postgres::types::ToSql + Sync),
                &metrics.change_velocity as &(dyn tokio_postgres::types::ToSql + Sync),
                &(metrics.total_fixes as i32) as &(dyn tokio_postgres::types::ToSql + Sync),
                &(metrics.effective_fixes as i32) as &(dyn tokio_postgres::types::ToSql + Sync),
            ],
        )
        .await
        .map_err(|e| crate::database::pg::pg_err("PG store_convergence_snapshot", &e))?;

        Ok(())
    }

    /// PG equivalent of trigger::analyze_convergence_status
    pub async fn analyze_convergence_status(
        &self,
        workflow_name: &str,
    ) -> Result<crate::reflection::trigger::ConvergenceStatus, String> {
        let metrics = self
            .compute_convergence_score(workflow_name, "workflow")
            .await?;

        let (status, reason, recommendation) = if metrics.score >= 0.8 {
            (
                "converging".to_string(),
                format!(
                    "Workflow '{}' has converged: {} consecutive clean runs, {:.0}% fix effectiveness.",
                    workflow_name, metrics.consecutive_clean_runs, metrics.effective_fix_rate * 100.0
                ),
                "No action needed. The system is stable.".to_string(),
            )
        } else if metrics.change_velocity > 2.0 {
            (
                "regressing".to_string(),
                format!(
                    "High change velocity ({:.1} fixes/run) detected. The system is not stabilizing.",
                    metrics.change_velocity
                ),
                "Review recent fixes for regressions. Consider pausing reflection until stable.".to_string(),
            )
        } else if metrics.novelty_score > 0.8 {
            (
                "improving".to_string(),
                format!(
                    "Most recent fixes are novel ({:.0}% new). The system is still exploring solutions.",
                    metrics.novelty_score * 100.0
                ),
                "Continue allowing reflection runs. The system is finding new solutions.".to_string(),
            )
        } else if metrics.consecutive_clean_runs == 0 && metrics.total_fixes == 0 {
            (
                "insufficient_data".to_string(),
                "Not enough data to determine convergence status.".to_string(),
                "Run more task workflows to accumulate convergence data.".to_string(),
            )
        } else {
            (
                "stalled".to_string(),
                format!(
                    "Convergence score {:.2}. Progress has stalled with {:.0}% fix effectiveness.",
                    metrics.score,
                    metrics.effective_fix_rate * 100.0
                ),
                "Consider reviewing ineffective fixes or changing the approach.".to_string(),
            )
        };

        Ok(crate::reflection::trigger::ConvergenceStatus {
            status,
            reason,
            consecutive_clean_runs: metrics.consecutive_clean_runs,
            convergence_score: metrics.score,
            stall_findings: vec![],
            recommendation,
        })
    }

    // ========================================================================
    // Causal Chain Operations (PG equivalents)
    // ========================================================================

    /// Trace a causal chain forward from a cause event using BFS.
    #[expect(
        clippy::disallowed_methods,
        reason = "legacy Row::get — migrate to try_get; dossier row-get-panic-kills-spawned-loop"
    )]
    pub async fn trace_causal_chain_forward(
        &self,
        event_type: &str,
        event_id: &str,
        max_depth: u32,
    ) -> Result<crate::reflection::causal::CausalChain, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool: {}", e))?;
        let mut events = Vec::new();
        let mut visited = std::collections::HashSet::new();
        let mut queue = std::collections::VecDeque::new();

        queue.push_back((event_type.to_string(), event_id.to_string(), 0u32));
        visited.insert(format!("{}:{}", event_type, event_id));

        while let Some((etype, eid, depth)) = queue.pop_front() {
            if depth >= max_depth {
                continue;
            }
            let rows = conn
                .query(
                    r#"SELECT id, cause_event_type, cause_event_id,
                              effect_event_type, effect_event_id,
                              relationship, confidence, source,
                              task_run_id, workflow_name, description, created_at
                       FROM causal_events
                       WHERE cause_event_type = $1 AND cause_event_id = $2
                       ORDER BY created_at ASC"#,
                    &[&etype, &eid],
                )
                .await
                .map_err(|e| crate::database::pg::pg_err("PG trace_causal_chain_forward", &e))?;
            for row in &rows {
                let event = crate::reflection::causal::CausalEvent {
                    id: row.get(0),
                    cause_event_type: row.get(1),
                    cause_event_id: row.get(2),
                    effect_event_type: row.get(3),
                    effect_event_id: row.get(4),
                    relationship: row.get(5),
                    confidence: row.get(6),
                    source: row.get(7),
                    task_run_id: row.get(8),
                    workflow_name: row.get(9),
                    description: row.get(10),
                    created_at: row.get(11),
                };
                let key = format!("{}:{}", event.effect_event_type, event.effect_event_id);
                if !visited.contains(&key) {
                    visited.insert(key);
                    queue.push_back((
                        event.effect_event_type.clone(),
                        event.effect_event_id.clone(),
                        depth + 1,
                    ));
                }
                events.push(event);
            }
        }
        let (terminal_type, terminal_id) = events
            .last()
            .map(|e| (e.effect_event_type.clone(), e.effect_event_id.clone()))
            .unwrap_or_else(|| (event_type.to_string(), event_id.to_string()));
        Ok(crate::reflection::causal::CausalChain {
            chain_length: events.len(),
            events,
            root_cause_type: event_type.to_string(),
            root_cause_id: event_id.to_string(),
            terminal_type,
            terminal_id,
        })
    }

    /// Trace a causal chain backward from an effect event using BFS.
    #[expect(
        clippy::disallowed_methods,
        reason = "legacy Row::get — migrate to try_get; dossier row-get-panic-kills-spawned-loop"
    )]
    pub async fn trace_causal_chain_backward(
        &self,
        event_type: &str,
        event_id: &str,
        max_depth: u32,
    ) -> Result<crate::reflection::causal::CausalChain, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool: {}", e))?;
        let mut events = Vec::new();
        let mut visited = std::collections::HashSet::new();
        let mut queue = std::collections::VecDeque::new();

        queue.push_back((event_type.to_string(), event_id.to_string(), 0u32));
        visited.insert(format!("{}:{}", event_type, event_id));

        while let Some((etype, eid, depth)) = queue.pop_front() {
            if depth >= max_depth {
                continue;
            }
            let rows = conn
                .query(
                    r#"SELECT id, cause_event_type, cause_event_id,
                              effect_event_type, effect_event_id,
                              relationship, confidence, source,
                              task_run_id, workflow_name, description, created_at
                       FROM causal_events
                       WHERE effect_event_type = $1 AND effect_event_id = $2
                       ORDER BY created_at ASC"#,
                    &[&etype, &eid],
                )
                .await
                .map_err(|e| crate::database::pg::pg_err("PG trace_causal_chain_backward", &e))?;
            for row in &rows {
                let event = crate::reflection::causal::CausalEvent {
                    id: row.get(0),
                    cause_event_type: row.get(1),
                    cause_event_id: row.get(2),
                    effect_event_type: row.get(3),
                    effect_event_id: row.get(4),
                    relationship: row.get(5),
                    confidence: row.get(6),
                    source: row.get(7),
                    task_run_id: row.get(8),
                    workflow_name: row.get(9),
                    description: row.get(10),
                    created_at: row.get(11),
                };
                let key = format!("{}:{}", event.cause_event_type, event.cause_event_id);
                if !visited.contains(&key) {
                    visited.insert(key);
                    queue.push_back((
                        event.cause_event_type.clone(),
                        event.cause_event_id.clone(),
                        depth + 1,
                    ));
                }
                events.push(event);
            }
        }
        let (root_type, root_id) = events
            .last()
            .map(|e| (e.cause_event_type.clone(), e.cause_event_id.clone()))
            .unwrap_or_else(|| (event_type.to_string(), event_id.to_string()));
        Ok(crate::reflection::causal::CausalChain {
            chain_length: events.len(),
            events,
            root_cause_type: root_type,
            root_cause_id: root_id,
            terminal_type: event_type.to_string(),
            terminal_id: event_id.to_string(),
        })
    }

    /// Get causal events for a workflow (reflection module version with typed return).
    #[expect(
        clippy::disallowed_methods,
        reason = "legacy Row::get — migrate to try_get; dossier row-get-panic-kills-spawned-loop"
    )]
    pub async fn get_causal_events_typed(
        &self,
        workflow_name: &str,
        limit: u32,
    ) -> Result<Vec<crate::reflection::causal::CausalEvent>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool: {}", e))?;
        let limit_i64 = limit as i64;
        let rows = conn.query(
            r#"SELECT id, cause_event_type, cause_event_id, effect_event_type, effect_event_id,
                      relationship, confidence, source, task_run_id, workflow_name, description, created_at
               FROM causal_events WHERE workflow_name = $1 ORDER BY created_at DESC LIMIT $2"#,
            &[&workflow_name, &limit_i64],
        ).await.map_err(|e| crate::database::pg::pg_err("PG get_causal_events", &e))?;
        Ok(rows
            .iter()
            .map(|row| crate::reflection::causal::CausalEvent {
                id: row.get(0),
                cause_event_type: row.get(1),
                cause_event_id: row.get(2),
                effect_event_type: row.get(3),
                effect_event_id: row.get(4),
                relationship: row.get(5),
                confidence: row.get(6),
                source: row.get(7),
                task_run_id: row.get(8),
                workflow_name: row.get(9),
                description: row.get(10),
                created_at: row.get(11),
            })
            .collect())
    }

    /// Build automated causal links for a task run.
    #[expect(
        clippy::disallowed_methods,
        reason = "legacy Row::get — migrate to try_get; dossier row-get-panic-kills-spawned-loop"
    )]
    pub async fn build_automated_causal_links(
        &self,
        task_run_id: &str,
        workflow_name: &str,
    ) -> Result<u32, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool: {}", e))?;
        let mut count = 0u32;
        let fix_rows = conn.query(
            "SELECT id, source_finding_id FROM reflection_fixes WHERE source_task_run_id = $1 AND source_finding_id IS NOT NULL",
            &[&task_run_id],
        ).await.map_err(|e| crate::database::pg::pg_err("PG build_causal fix query", &e))?;
        for row in &fix_rows {
            let fix_id: String = row.get(0);
            let finding_id: String = row.get(1);
            let id = uuid::Uuid::new_v4().to_string();
            let affected = conn.execute(
                r#"INSERT INTO causal_events (id, cause_event_type, cause_event_id, effect_event_type, effect_event_id,
                    relationship, confidence, source, task_run_id, workflow_name)
                   VALUES ($1, 'fix_applied', $2, 'finding_detected', $3, 'triggered_by', 'high', 'automated', $4, $5)
                   ON CONFLICT (cause_event_type, cause_event_id, effect_event_type, effect_event_id) DO NOTHING"#,
                &[&id, &fix_id, &finding_id, &task_run_id, &workflow_name],
            ).await.unwrap_or(0);
            count += affected as u32;
        }
        info!(
            "PG: Built {} automated causal links for task_run={}, workflow={}",
            count, task_run_id, workflow_name
        );
        Ok(count)
    }

    /// Get causal summary for a workflow.
    #[expect(
        clippy::disallowed_methods,
        reason = "legacy Row::get — migrate to try_get; dossier row-get-panic-kills-spawned-loop"
    )]
    pub async fn get_causal_summary(
        &self,
        workflow_name: &str,
    ) -> Result<crate::reflection::causal::CausalSummary, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool: {}", e))?;
        let total_links: i64 = conn
            .query_one(
                "SELECT COUNT(*) FROM causal_events WHERE workflow_name = $1",
                &[&workflow_name],
            )
            .await
            .map(|r| r.get(0))
            .map_err(|e| crate::database::pg::pg_err("PG causal count", &e))?;

        let mut by_relationship = std::collections::HashMap::new();
        for row in conn.query(
            "SELECT relationship, COUNT(*)::INT FROM causal_events WHERE workflow_name = $1 GROUP BY relationship",
            &[&workflow_name],
        ).await.unwrap_or_default() {
            by_relationship.insert(row.get::<_, String>(0), row.get::<_, i32>(1) as u32);
        }

        let mut by_cause_type = std::collections::HashMap::new();
        for row in conn.query(
            "SELECT cause_event_type, COUNT(*)::INT FROM causal_events WHERE workflow_name = $1 GROUP BY cause_event_type",
            &[&workflow_name],
        ).await.unwrap_or_default() {
            by_cause_type.insert(row.get::<_, String>(0), row.get::<_, i32>(1) as u32);
        }

        let avg_chain_length = if total_links > 0 {
            let root_count: i64 = conn.query_one(
                r#"SELECT COUNT(DISTINCT cause_event_type || ':' || cause_event_id) FROM causal_events
                   WHERE workflow_name = $1 AND (cause_event_type || ':' || cause_event_id) NOT IN (
                       SELECT effect_event_type || ':' || effect_event_id FROM causal_events WHERE workflow_name = $1)"#,
                &[&workflow_name],
            ).await.map(|r| r.get(0)).unwrap_or(1);
            if root_count > 0 {
                total_links as f64 / root_count as f64
            } else {
                total_links as f64
            }
        } else {
            0.0
        };

        Ok(crate::reflection::causal::CausalSummary {
            total_links: total_links as u32,
            by_relationship,
            by_cause_type,
            avg_chain_length,
        })
    }

    // ========================================================================
    // Architecture Model Operations (PG equivalents)
    // ========================================================================

    /// Get the component graph for a workflow.
    #[expect(
        clippy::disallowed_methods,
        reason = "legacy Row::get — migrate to try_get; dossier row-get-panic-kills-spawned-loop"
    )]
    pub async fn get_component_graph(
        &self,
        workflow_name: &str,
    ) -> Result<crate::reflection::architecture::ComponentGraph, String> {
        use crate::reflection::architecture::*;
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool: {}", e))?;
        let node_rows = conn
            .query(
                r#"SELECT id, component_path, component_type, fix_count, error_count,
                      causal_involvement_count, effective_fix_count, ineffective_fix_count,
                      health_score, change_velocity, last_activity_at
               FROM architecture_components WHERE workflow_name = $1 ORDER BY component_path"#,
                &[&workflow_name],
            )
            .await
            .map_err(|e| crate::database::pg::pg_err("PG get_component_graph", &e))?;

        let nodes: Vec<ComponentNode> = node_rows
            .iter()
            .map(|r| ComponentNode {
                id: r.get(0),
                component_path: r.get(1),
                component_type: r.get(2),
                fix_count: r.get::<_, i32>(3) as u32,
                error_count: r.get::<_, i32>(4) as u32,
                causal_involvement_count: r.get::<_, i32>(5) as u32,
                effective_fix_count: r.get::<_, i32>(6) as u32,
                ineffective_fix_count: r.get::<_, i32>(7) as u32,
                health_score: r.get(8),
                change_velocity: r.get(9),
                last_activity_at: r.get(10),
            })
            .collect();

        let edge_rows = conn.query(
            "SELECT source_component, target_component, relationship_type, strength FROM architecture_relationships WHERE workflow_name = $1",
            &[&workflow_name],
        ).await.map_err(|e| crate::database::pg::pg_err("PG get_component_graph edges", &e))?;

        let edges: Vec<ComponentEdge> = edge_rows
            .iter()
            .map(|r| ComponentEdge {
                source: r.get(0),
                target: r.get(1),
                relationship_type: r.get(2),
                strength: r.get::<_, i32>(3) as u32,
            })
            .collect();

        let avg_health = if !nodes.is_empty() {
            nodes.iter().map(|n| n.health_score).sum::<f64>() / nodes.len() as f64
        } else {
            0.0
        };
        let mut volatile: Vec<_> = nodes
            .iter()
            .map(|n| (n.component_path.clone(), n.change_velocity))
            .collect();
        volatile.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        let most_volatile: Vec<String> = volatile.into_iter().take(5).map(|(p, _)| p).collect();

        Ok(ComponentGraph {
            nodes,
            edges,
            stats: GraphStats {
                total_components: node_rows.len() as u32,
                total_relationships: edge_rows.len() as u32,
                avg_health_score: avg_health,
                most_volatile,
            },
        })
    }

    /// Rebuild the architecture model for a workflow.
    #[expect(
        clippy::disallowed_methods,
        reason = "legacy Row::get — migrate to try_get; dossier row-get-panic-kills-spawned-loop"
    )]
    pub async fn rebuild_architecture_model(
        &self,
        workflow_name: &str,
    ) -> Result<crate::reflection::architecture::RebuildResult, String> {
        use crate::reflection::architecture::*;
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool: {}", e))?;
        conn.execute(
            "DELETE FROM architecture_components WHERE workflow_name = $1",
            &[&workflow_name],
        )
        .await
        .map_err(|e| crate::database::pg::pg_err("PG rebuild_arch delete", &e))?;
        conn.execute(
            "DELETE FROM architecture_relationships WHERE workflow_name = $1",
            &[&workflow_name],
        )
        .await
        .map_err(|e| crate::database::pg::pg_err("PG rebuild_arch delete rels", &e))?;

        let fix_rows = conn
            .query(
                // `MAX(created_at)::TEXT`: `MAX` preserves its input type, and
                // `reflection_fixes.created_at` is `timestamp with time zone`,
                // which `String`/`Option<String>` cannot deserialize. The two
                // `SUM(CASE ...)` columns are `bigint` and are read as `i64`,
                // so those are already correct and are left as they are.
                r#"SELECT file_changed, COUNT(*) as fix_count,
                      SUM(CASE WHEN effectiveness = 'effective' THEN 1 ELSE 0 END) as effective,
                      SUM(CASE WHEN effectiveness = 'ineffective' THEN 1 ELSE 0 END) as ineffective,
                      MAX(created_at)::TEXT as last_activity
               FROM reflection_fixes
               WHERE file_changed IS NOT NULL
                 AND source_task_run_id IN (SELECT id FROM task_runs WHERE workflow_name = $1)
               GROUP BY file_changed"#,
                &[&workflow_name],
            )
            .await
            .map_err(|e| crate::database::pg::pg_err("PG rebuild_arch scan", &e))?;

        let mut components_count = 0u32;
        for row in &fix_rows {
            let raw_path: String = row.get(0);
            let path = normalize_component_path(&raw_path);
            let comp_type = infer_component_type(&path);
            let fix_count: i64 = row.get(1);
            let effective: i64 = row.get(2);
            let ineffective: i64 = row.get(3);
            let last_activity: Option<String> = row.get(4);
            let health = if fix_count > 0 {
                effective as f64 / fix_count as f64
            } else {
                1.0
            };
            let id = uuid::Uuid::new_v4().to_string();
            conn.execute(
                r#"INSERT INTO architecture_components
                   (id, workflow_name, component_path, component_type, fix_count, error_count,
                    causal_involvement_count, effective_fix_count, ineffective_fix_count, health_score, change_velocity, last_activity_at)
                   VALUES ($1, $2, $3, $4, $5, 0, 0, $6, $7, $8, 0.0, $9::TEXT::TIMESTAMPTZ)
                   ON CONFLICT (workflow_name, component_path) DO UPDATE
                   SET fix_count = $5, effective_fix_count = $6, ineffective_fix_count = $7, health_score = $8, last_activity_at = $9::TEXT::TIMESTAMPTZ"#,
                &[&id, &workflow_name, &path, &comp_type, &(fix_count as i32), &(effective as i32), &(ineffective as i32), &health, &last_activity],
            ).await.map_err(|e| crate::database::pg::pg_err("PG rebuild_arch insert", &e))?;
            components_count += 1;
        }

        let rel_count: i64 = conn
            .query_one(
                "SELECT COUNT(*) FROM architecture_relationships WHERE workflow_name = $1",
                &[&workflow_name],
            )
            .await
            .map(|r| r.get(0))
            .unwrap_or(0);

        Ok(RebuildResult {
            components_count,
            relationships_count: rel_count as u32,
            workflow_name: workflow_name.to_string(),
        })
    }

    /// Get component details for a specific path.
    #[expect(
        clippy::disallowed_methods,
        reason = "legacy Row::get — migrate to try_get; dossier row-get-panic-kills-spawned-loop"
    )]
    pub async fn get_component_details(
        &self,
        workflow_name: &str,
        path: &str,
    ) -> Result<crate::reflection::architecture::ComponentDetails, String> {
        use crate::reflection::architecture::*;
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool: {}", e))?;
        let norm_path = normalize_component_path(path);
        let node_row = conn
            .query_opt(
                r#"SELECT id, component_path, component_type, fix_count, error_count,
                      causal_involvement_count, effective_fix_count, ineffective_fix_count,
                      health_score, change_velocity, last_activity_at
               FROM architecture_components WHERE workflow_name = $1 AND component_path = $2"#,
                &[&workflow_name, &norm_path],
            )
            .await
            .map_err(|e| crate::database::pg::pg_err("PG get_component_details", &e))?;
        let node = match node_row {
            Some(r) => ComponentNode {
                id: r.get(0),
                component_path: r.get(1),
                component_type: r.get(2),
                fix_count: r.get::<_, i32>(3) as u32,
                error_count: r.get::<_, i32>(4) as u32,
                causal_involvement_count: r.get::<_, i32>(5) as u32,
                effective_fix_count: r.get::<_, i32>(6) as u32,
                ineffective_fix_count: r.get::<_, i32>(7) as u32,
                health_score: r.get(8),
                change_velocity: r.get(9),
                last_activity_at: r.get(10),
            },
            None => return Err(format!("Component not found: {}", path)),
        };
        let recent_fixes: Vec<FixSummary> = conn.query(
            "SELECT id, fix_type, fix_description, effectiveness, applied_at::TEXT FROM reflection_fixes WHERE file_changed = $1 ORDER BY created_at DESC LIMIT 10",
            &[&norm_path],
        ).await.unwrap_or_default().iter().map(|r| FixSummary {
            id: r.get(0), fix_type: r.get(1), fix_description: r.get(2), effectiveness: r.get(3), applied_at: r.get(4),
        }).collect();
        let impacts: Vec<ImpactEntry> = conn.query(
            "SELECT target_component, relationship_type, strength FROM architecture_relationships WHERE workflow_name = $1 AND source_component = $2",
            &[&workflow_name, &norm_path],
        ).await.unwrap_or_default().iter().map(|r| ImpactEntry {
            component_path: r.get(0), relationship_type: r.get(1), strength: r.get::<_, i32>(2) as u32,
        }).collect();
        let impacted_by: Vec<ImpactEntry> = conn.query(
            "SELECT source_component, relationship_type, strength FROM architecture_relationships WHERE workflow_name = $1 AND target_component = $2",
            &[&workflow_name, &norm_path],
        ).await.unwrap_or_default().iter().map(|r| ImpactEntry {
            component_path: r.get(0), relationship_type: r.get(1), strength: r.get::<_, i32>(2) as u32,
        }).collect();
        Ok(ComponentDetails {
            node,
            recent_fixes,
            impacted_by,
            impacts,
        })
    }

    /// Get architecture impact analysis for a component (BFS, max 3 hops).
    #[expect(
        clippy::disallowed_methods,
        reason = "legacy Row::get — migrate to try_get; dossier row-get-panic-kills-spawned-loop"
    )]
    pub async fn get_architecture_impact_analysis(
        &self,
        workflow_name: &str,
        component: &str,
    ) -> Result<crate::reflection::architecture::ImpactAnalysis, String> {
        use crate::reflection::architecture::*;
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool: {}", e))?;
        let norm = normalize_component_path(component);
        let direct_rows = conn.query(
            "SELECT target_component, relationship_type, strength FROM architecture_relationships WHERE workflow_name = $1 AND source_component = $2",
            &[&workflow_name, &norm],
        ).await.unwrap_or_default();
        let direct_impacts: Vec<ImpactEntry> = direct_rows
            .iter()
            .map(|r| ImpactEntry {
                component_path: r.get(0),
                relationship_type: r.get(1),
                strength: r.get::<_, i32>(2) as u32,
            })
            .collect();
        let mut transitive = Vec::new();
        let mut visited = std::collections::HashSet::new();
        visited.insert(norm.clone());
        let mut frontier: Vec<String> = direct_impacts
            .iter()
            .map(|d| d.component_path.clone())
            .collect();
        for _ in 0..2 {
            let mut next = Vec::new();
            for f in &frontier {
                if visited.contains(f) {
                    continue;
                }
                visited.insert(f.clone());
                for r in conn.query(
                    "SELECT target_component, relationship_type, strength FROM architecture_relationships WHERE workflow_name = $1 AND source_component = $2",
                    &[&workflow_name, f],
                ).await.unwrap_or_default() {
                    let target: String = r.get(0);
                    if !visited.contains(&target) {
                        transitive.push(ImpactEntry { component_path: target.clone(), relationship_type: r.get(1), strength: r.get::<_, i32>(2) as u32 });
                        next.push(target);
                    }
                }
            }
            frontier = next;
            if frontier.is_empty() {
                break;
            }
        }
        let total = direct_impacts.len() + transitive.len();
        Ok(ImpactAnalysis {
            component: component.to_string(),
            direct_impacts,
            transitive_impacts: transitive,
            total_impact_radius: total as u32,
            highest_risk_path: vec![component.to_string()],
        })
    }

    /// Get impact analysis with graph enrichment.
    pub async fn get_impact_analysis_with_graph(
        &self,
        workflow_name: &str,
        component: &str,
    ) -> Result<crate::reflection::architecture::ImpactAnalysis, String> {
        self.get_architecture_impact_analysis(workflow_name, component)
            .await
    }

    // ========================================================================
    // Workflow Trends Operations (PG equivalents)
    // ========================================================================

    /// Get workflow-level convergence trends.
    #[expect(
        clippy::disallowed_methods,
        reason = "legacy Row::get — migrate to try_get; dossier row-get-panic-kills-spawned-loop"
    )]
    pub async fn get_workflow_trends(
        &self,
        workflow_name: &str,
        time_range: Option<&str>,
    ) -> Result<crate::reflection::trends::WorkflowTrends, String> {
        use crate::reflection::trends::{TrendPoint, WorkflowTrends};
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool: {}", e))?;
        let cutoff = parse_time_cutoff_static(time_range);
        let rows = if let Some(ref c) = cutoff {
            conn.query(
                "SELECT snapshot_at::TEXT AS snapshot_at_text, convergence_score, effective_fix_rate, change_velocity, total_fixes FROM convergence_snapshots WHERE workflow_name = $1 AND snapshot_at > $2::TEXT::timestamptz ORDER BY snapshot_at ASC",
                &[&workflow_name, c],
            ).await
        } else {
            conn.query(
                "SELECT snapshot_at::TEXT AS snapshot_at_text, convergence_score, effective_fix_rate, change_velocity, total_fixes FROM convergence_snapshots WHERE workflow_name = $1 ORDER BY snapshot_at ASC",
                &[&workflow_name],
            ).await
        }.map_err(|e| crate::database::pg::pg_err("PG get_workflow_trends", &e))?;
        let mut convergence = Vec::new();
        let mut fix_rate = Vec::new();
        let mut velocity = Vec::new();
        let mut total_fixes = Vec::new();
        for row in &rows {
            let ts: String = row.get(0);
            convergence.push(TrendPoint {
                timestamp: ts.clone(),
                value: row.get(1),
                count: None,
            });
            fix_rate.push(TrendPoint {
                timestamp: ts.clone(),
                value: row.get(2),
                count: None,
            });
            velocity.push(TrendPoint {
                timestamp: ts.clone(),
                value: row.get(3),
                count: None,
            });
            total_fixes.push(TrendPoint {
                timestamp: ts.clone(),
                value: row.get::<_, i32>(4) as f64,
                count: Some(row.get::<_, i32>(4) as u32),
            });
        }
        Ok(WorkflowTrends {
            workflow_name: workflow_name.to_string(),
            convergence,
            fix_rate,
            velocity,
            total_fixes,
            snapshot_count: rows.len(),
        })
    }

    /// Get component-level health trends.
    #[expect(
        clippy::disallowed_methods,
        reason = "legacy Row::get — migrate to try_get; dossier row-get-panic-kills-spawned-loop"
    )]
    pub async fn get_component_trend(
        &self,
        workflow_name: &str,
        path: &str,
        time_range: Option<&str>,
    ) -> Result<crate::reflection::trends::ComponentTrend, String> {
        use crate::reflection::trends::{ComponentTrend, TrendPoint};
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool: {}", e))?;
        let norm_path = crate::reflection::architecture::normalize_component_path(path);
        let cutoff = parse_time_cutoff_static(time_range);
        let rows = if let Some(ref c) = cutoff {
            conn.query(
                "SELECT snapshot_at::TEXT AS snapshot_at_text, health_score, fix_count FROM component_health_snapshots WHERE workflow_name = $1 AND component_path = $2 AND snapshot_at > $3::TEXT::timestamptz ORDER BY snapshot_at ASC",
                &[&workflow_name, &norm_path, c],
            ).await
        } else {
            conn.query(
                "SELECT snapshot_at::TEXT AS snapshot_at_text, health_score, fix_count FROM component_health_snapshots WHERE workflow_name = $1 AND component_path = $2 ORDER BY snapshot_at ASC",
                &[&workflow_name, &norm_path],
            ).await
        }.map_err(|e| crate::database::pg::pg_err("PG get_component_trend", &e))?;
        let mut health_scores = Vec::new();
        let mut fix_counts = Vec::new();
        for row in &rows {
            let ts: String = row.get(0);
            health_scores.push(TrendPoint {
                timestamp: ts.clone(),
                value: row.get(1),
                count: None,
            });
            fix_counts.push(TrendPoint {
                timestamp: ts.clone(),
                value: row.get::<_, i32>(2) as f64,
                count: Some(row.get::<_, i32>(2) as u32),
            });
        }
        Ok(ComponentTrend {
            component_path: norm_path,
            health_scores,
            fix_counts,
        })
    }

    /// Get time-bucketed effectiveness rate from reflection_fixes.
    #[expect(
        clippy::disallowed_methods,
        reason = "legacy Row::get — migrate to try_get; dossier row-get-panic-kills-spawned-loop"
    )]
    pub async fn get_effectiveness_over_time(
        &self,
        workflow_name: &str,
        bucket_type: &str,
        time_range: Option<&str>,
    ) -> Result<crate::reflection::trends::EffectivenessOverTime, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool: {}", e))?;
        let cutoff = parse_time_cutoff_static(time_range);
        let date_trunc = match bucket_type {
            "day" => "day",
            "month" => "month",
            _ => "week",
        };
        let base_query = format!(
            r#"SELECT DATE_TRUNC('{}', created_at::timestamp)::TEXT as bucket,
                      COUNT(*) as total,
                      SUM(CASE WHEN effectiveness = 'effective' THEN 1 ELSE 0 END) as effective
               FROM reflection_fixes
               WHERE source_task_run_id IN (SELECT id FROM task_runs WHERE workflow_name = $1)
               {} GROUP BY bucket ORDER BY bucket ASC"#,
            date_trunc,
            if cutoff.is_some() {
                "AND created_at > $2::TEXT::timestamptz"
            } else {
                ""
            }
        );
        let rows = if let Some(ref c) = cutoff {
            conn.query(&base_query, &[&workflow_name, c]).await
        } else {
            conn.query(&base_query, &[&workflow_name]).await
        }
        .map_err(|e| crate::database::pg::pg_err("PG get_effectiveness_over_time", &e))?;
        let mut eb = Vec::new();
        for row in &rows {
            let bucket_label: String = row.get(0);
            let total: i64 = row.get(1);
            let effective: i64 = row.get(2);
            let ineffective = total - effective;
            let rate = if total > 0 {
                effective as f64 / total as f64
            } else {
                0.0
            };
            eb.push(crate::reflection::trends::EffectivenessBucket {
                bucket: bucket_label,
                total: total as u32,
                effective: effective as u32,
                ineffective: ineffective as u32,
                regression: 0,
                effectiveness_rate: rate,
            });
        }
        Ok(crate::reflection::trends::EffectivenessOverTime {
            workflow_name: workflow_name.to_string(),
            bucket_type: bucket_type.to_string(),
            buckets: eb,
        })
    }
}

/// Parse time range to cutoff (standalone function for use by PgDb).
fn parse_time_cutoff_static(time_range: Option<&str>) -> Option<String> {
    let range = time_range?;
    if range == "all" {
        return None;
    }
    let now = chrono::Utc::now();
    let duration = if range.ends_with('d') {
        chrono::Duration::days(range.trim_end_matches('d').parse().ok()?)
    } else if range.ends_with('h') {
        chrono::Duration::hours(range.trim_end_matches('h').parse().ok()?)
    } else {
        return None;
    };
    Some((now - duration).format("%Y-%m-%dT%H:%M:%SZ").to_string())
}

#[cfg(test)]
mod decision_tests;

#[cfg(test)]
mod tests;

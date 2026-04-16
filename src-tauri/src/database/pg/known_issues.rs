//! PostgreSQL known_issues CRUD operations (raw SQL).

use super::PgDb;
use crate::known_issues::types::*;
use chrono::Utc;

impl PgDb {
    // ========================================================================
    // Known Issues
    // ========================================================================

    /// List known issues with optional filters.
    pub async fn list_known_issues(
        &self,
        query: &ListKnownIssuesQuery,
    ) -> Result<Vec<KnownIssue>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let mut where_clauses: Vec<String> = Vec::new();
        let mut param_values: Vec<String> = Vec::new();

        // Handle spec_id convenience shortcut
        if let Some(ref spec_id) = query.spec_id {
            where_clauses.push("scope_type = 'spec'".to_string());
            param_values.push(spec_id.clone());
            where_clauses.push(format!("scope_value = ${}", param_values.len()));
        } else {
            if let Some(ref scope_type) = query.scope_type {
                param_values.push(scope_type.clone());
                where_clauses.push(format!("scope_type = ${}", param_values.len()));
            }
            if let Some(ref scope_value) = query.scope_value {
                param_values.push(scope_value.clone());
                where_clauses.push(format!("scope_value = ${}", param_values.len()));
            }
        }

        if let Some(ref category) = query.category {
            param_values.push(category.clone());
            where_clauses.push(format!("category = ${}", param_values.len()));
        }
        if let Some(ref severity) = query.severity {
            param_values.push(severity.clone());
            where_clauses.push(format!("severity = ${}", param_values.len()));
        }
        if let Some(ref status) = query.status {
            param_values.push(status.clone());
            where_clauses.push(format!("status = ${}", param_values.len()));
        }

        let where_sql = if where_clauses.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", where_clauses.join(" AND "))
        };

        let sql = format!(
            r#"
            SELECT id, title, description, category, scope_type, scope_value,
                   scope_tags, detection_method, detection_config, pattern_template_id,
                   reproduction_context, trigger_conditions, severity, status,
                   confidence, provenance, source_finding_ids, source_task_run_id,
                   verification_hint, verification_step_template,
                   times_detected, times_checked,
                   last_detected_at::TEXT, last_checked_at::TEXT, resolved_at::TEXT,
                   created_at::TEXT, updated_at::TEXT
            FROM known_issues
            {}
            ORDER BY
                CASE severity WHEN 'critical' THEN 0 WHEN 'high' THEN 1 WHEN 'medium' THEN 2 WHEN 'low' THEN 3 END,
                created_at DESC
            "#,
            where_sql
        );

        let params: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = param_values
            .iter()
            .map(|v| v as &(dyn tokio_postgres::types::ToSql + Sync))
            .collect();

        let rows = conn
            .query(&sql, &params)
            .await
            .map_err(|e| format!("PG list_known_issues: {}", e))?;

        Ok(rows
            .iter()
            .map(|r| Self::ki_row_to_known_issue(r))
            .collect())
    }

    /// Get a single known issue by ID.
    pub async fn get_known_issue(&self, id: &str) -> Result<Option<KnownIssue>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let row = conn
            .query_opt(
                r#"
                SELECT id, title, description, category, scope_type, scope_value,
                       scope_tags, detection_method, detection_config, pattern_template_id,
                       reproduction_context, trigger_conditions, severity, status,
                       confidence, provenance, source_finding_ids, source_task_run_id,
                       verification_hint, verification_step_template,
                       times_detected, times_checked,
                       last_detected_at::TEXT, last_checked_at::TEXT, resolved_at::TEXT,
                       created_at::TEXT, updated_at::TEXT
                FROM known_issues
                WHERE id = $1
                "#,
                &[&id],
            )
            .await
            .map_err(|e| format!("PG get_known_issue: {}", e))?;

        Ok(row.map(|r| Self::ki_row_to_known_issue(&r)))
    }

    /// Create a new known issue.
    pub async fn create_known_issue(
        &self,
        req: &CreateKnownIssueRequest,
    ) -> Result<KnownIssue, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let id = uuid::Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();

        let scope_tags_json = serde_json::to_string(&req.scope_tags.clone().unwrap_or_default())
            .unwrap_or_else(|_| "[]".to_string());
        let detection_config_json = serde_json::to_string(
            &req.detection_config
                .clone()
                .unwrap_or(serde_json::json!({})),
        )
        .unwrap_or_else(|_| "{}".to_string());
        let trigger_conditions_json =
            serde_json::to_string(&req.trigger_conditions.clone().unwrap_or_default())
                .unwrap_or_else(|_| "[]".to_string());
        let source_finding_ids_json =
            serde_json::to_string(&req.source_finding_ids.clone().unwrap_or_default())
                .unwrap_or_else(|_| "[]".to_string());
        let verification_step_template_json = req
            .verification_step_template
            .as_ref()
            .map(|v| serde_json::to_string(v).unwrap_or_else(|_| "null".to_string()));

        let provenance = req
            .provenance
            .as_ref()
            .map(|p| p.as_str())
            .unwrap_or("manual")
            .to_string();
        let category_str = req.category.as_str().to_string();
        let scope_type_str = req.scope_type.as_str().to_string();
        let detection_method_str = req.detection_method.as_str().to_string();
        let severity_str = req.severity.as_str().to_string();
        let confidence: f64 = 0.5;
        let times_detected: i32 = 1;
        let times_checked: i32 = 0;
        let status = "active".to_string();

        conn.execute(
            r#"
            INSERT INTO known_issues (
                id, title, description, category, scope_type, scope_value,
                scope_tags, detection_method, detection_config, pattern_template_id,
                reproduction_context, trigger_conditions, severity, status,
                confidence, provenance, source_finding_ids, source_task_run_id,
                verification_hint, verification_step_template,
                times_detected, times_checked, last_detected_at, last_checked_at,
                resolved_at, created_at, updated_at
            ) VALUES (
                $1, $2, $3, $4, $5, $6,
                $7, $8, $9, $10,
                $11, $12, $13, $14,
                $15, $16, $17, $18,
                $19, $20,
                $21, $22, $23, NULL,
                NULL, $24::TIMESTAMPTZ, $24::TIMESTAMPTZ
            )
            "#,
            &[
                &id as &(dyn tokio_postgres::types::ToSql + Sync),
                &req.title as &(dyn tokio_postgres::types::ToSql + Sync),
                &req.description as &(dyn tokio_postgres::types::ToSql + Sync),
                &category_str as &(dyn tokio_postgres::types::ToSql + Sync),
                &scope_type_str as &(dyn tokio_postgres::types::ToSql + Sync),
                &req.scope_value as &(dyn tokio_postgres::types::ToSql + Sync),
                &scope_tags_json as &(dyn tokio_postgres::types::ToSql + Sync),
                &detection_method_str as &(dyn tokio_postgres::types::ToSql + Sync),
                &detection_config_json as &(dyn tokio_postgres::types::ToSql + Sync),
                &req.pattern_template_id as &(dyn tokio_postgres::types::ToSql + Sync),
                &req.reproduction_context as &(dyn tokio_postgres::types::ToSql + Sync),
                &trigger_conditions_json as &(dyn tokio_postgres::types::ToSql + Sync),
                &severity_str as &(dyn tokio_postgres::types::ToSql + Sync),
                &status as &(dyn tokio_postgres::types::ToSql + Sync),
                &confidence as &(dyn tokio_postgres::types::ToSql + Sync),
                &provenance as &(dyn tokio_postgres::types::ToSql + Sync),
                &source_finding_ids_json as &(dyn tokio_postgres::types::ToSql + Sync),
                &req.source_task_run_id as &(dyn tokio_postgres::types::ToSql + Sync),
                &req.verification_hint as &(dyn tokio_postgres::types::ToSql + Sync),
                &verification_step_template_json as &(dyn tokio_postgres::types::ToSql + Sync),
                &times_detected as &(dyn tokio_postgres::types::ToSql + Sync),
                &times_checked as &(dyn tokio_postgres::types::ToSql + Sync),
                &now as &(dyn tokio_postgres::types::ToSql + Sync),
                &now as &(dyn tokio_postgres::types::ToSql + Sync),
            ],
        )
        .await
        .map_err(|e| format!("PG create_known_issue: {}", e))?;

        self.get_known_issue(&id)
            .await?
            .ok_or_else(|| "Known issue not found after insert".to_string())
    }

    /// Update an existing known issue.
    pub async fn update_known_issue(
        &self,
        id: &str,
        req: &UpdateKnownIssueRequest,
    ) -> Result<KnownIssue, String> {
        let existing = self
            .get_known_issue(id)
            .await?
            .ok_or_else(|| format!("Known issue not found: {}", id))?;

        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let now = Utc::now().to_rfc3339();

        let title = req.title.as_deref().unwrap_or(&existing.title).to_string();
        let description = req
            .description
            .as_deref()
            .unwrap_or(&existing.description)
            .to_string();
        let category = req
            .category
            .as_ref()
            .unwrap_or(&existing.category)
            .as_str()
            .to_string();
        let scope_type = req
            .scope_type
            .as_ref()
            .unwrap_or(&existing.scope_type)
            .as_str()
            .to_string();
        let scope_value = req
            .scope_value
            .as_ref()
            .or(existing.scope_value.as_ref())
            .cloned();
        let scope_tags_json = req
            .scope_tags
            .as_ref()
            .map(|tags| serde_json::to_string(tags).unwrap_or_else(|_| "[]".to_string()))
            .unwrap_or_else(|| {
                serde_json::to_string(&existing.scope_tags).unwrap_or_else(|_| "[]".to_string())
            });
        let detection_method = req
            .detection_method
            .as_ref()
            .unwrap_or(&existing.detection_method)
            .as_str()
            .to_string();
        let detection_config_json = req
            .detection_config
            .as_ref()
            .map(|v| serde_json::to_string(v).unwrap_or_else(|_| "{}".to_string()))
            .unwrap_or_else(|| {
                serde_json::to_string(&existing.detection_config)
                    .unwrap_or_else(|_| "{}".to_string())
            });
        let pattern_template_id = req
            .pattern_template_id
            .as_ref()
            .or(existing.pattern_template_id.as_ref())
            .cloned();
        let reproduction_context = req
            .reproduction_context
            .as_ref()
            .or(existing.reproduction_context.as_ref())
            .cloned();
        let trigger_conditions_json = req
            .trigger_conditions
            .as_ref()
            .map(|tc| serde_json::to_string(tc).unwrap_or_else(|_| "[]".to_string()))
            .unwrap_or_else(|| {
                serde_json::to_string(&existing.trigger_conditions)
                    .unwrap_or_else(|_| "[]".to_string())
            });
        let severity = req
            .severity
            .as_ref()
            .unwrap_or(&existing.severity)
            .as_str()
            .to_string();
        let status = req
            .status
            .as_ref()
            .unwrap_or(&existing.status)
            .as_str()
            .to_string();
        let confidence = req.confidence.unwrap_or(existing.confidence);
        let verification_hint = req
            .verification_hint
            .as_ref()
            .or(existing.verification_hint.as_ref())
            .cloned();
        let verification_step_template_json = req
            .verification_step_template
            .as_ref()
            .or(existing.verification_step_template.as_ref())
            .map(|v| serde_json::to_string(v).unwrap_or_else(|_| "null".to_string()));

        let resolved_at = if status == "resolved" && existing.status != IssueStatus::Resolved {
            Some(now.clone())
        } else {
            existing.resolved_at.clone()
        };

        conn.execute(
            r#"
            UPDATE known_issues SET
                title = $2,
                description = $3,
                category = $4,
                scope_type = $5,
                scope_value = $6,
                scope_tags = $7,
                detection_method = $8,
                detection_config = $9,
                pattern_template_id = $10,
                reproduction_context = $11,
                trigger_conditions = $12,
                severity = $13,
                status = $14,
                confidence = $15,
                verification_hint = $16,
                verification_step_template = $17,
                resolved_at = $18,
                updated_at = $19::TIMESTAMPTZ
            WHERE id = $1
            "#,
            &[
                &id as &(dyn tokio_postgres::types::ToSql + Sync),
                &title as &(dyn tokio_postgres::types::ToSql + Sync),
                &description as &(dyn tokio_postgres::types::ToSql + Sync),
                &category as &(dyn tokio_postgres::types::ToSql + Sync),
                &scope_type as &(dyn tokio_postgres::types::ToSql + Sync),
                &scope_value as &(dyn tokio_postgres::types::ToSql + Sync),
                &scope_tags_json as &(dyn tokio_postgres::types::ToSql + Sync),
                &detection_method as &(dyn tokio_postgres::types::ToSql + Sync),
                &detection_config_json as &(dyn tokio_postgres::types::ToSql + Sync),
                &pattern_template_id as &(dyn tokio_postgres::types::ToSql + Sync),
                &reproduction_context as &(dyn tokio_postgres::types::ToSql + Sync),
                &trigger_conditions_json as &(dyn tokio_postgres::types::ToSql + Sync),
                &severity as &(dyn tokio_postgres::types::ToSql + Sync),
                &status as &(dyn tokio_postgres::types::ToSql + Sync),
                &confidence as &(dyn tokio_postgres::types::ToSql + Sync),
                &verification_hint as &(dyn tokio_postgres::types::ToSql + Sync),
                &verification_step_template_json as &(dyn tokio_postgres::types::ToSql + Sync),
                &resolved_at as &(dyn tokio_postgres::types::ToSql + Sync),
                &now as &(dyn tokio_postgres::types::ToSql + Sync),
            ],
        )
        .await
        .map_err(|e| format!("PG update_known_issue: {}", e))?;

        self.get_known_issue(id)
            .await?
            .ok_or_else(|| "Known issue not found after update".to_string())
    }

    /// Increment times_checked and update last_checked_at.
    pub async fn increment_known_issue_checked(&self, id: &str) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let now = Utc::now().to_rfc3339();
        conn.execute(
            r#"UPDATE known_issues
               SET times_checked = COALESCE(times_checked, 0) + 1,
                   last_checked_at = $2, updated_at = $2
               WHERE id = $1"#,
            &[&id, &now],
        )
        .await
        .map_err(|e| format!("PG increment_known_issue_checked: {}", e))?;
        Ok(())
    }

    /// Increment times_detected and update last_detected_at.
    pub async fn increment_known_issue_detected(&self, id: &str) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let now = Utc::now().to_rfc3339();
        conn.execute(
            r#"UPDATE known_issues
               SET times_detected = COALESCE(times_detected, 0) + 1,
                   last_detected_at = $2, updated_at = $2
               WHERE id = $1"#,
            &[&id, &now],
        )
        .await
        .map_err(|e| format!("PG increment_known_issue_detected: {}", e))?;
        Ok(())
    }

    /// Decay confidence when a regression check passes (issue was NOT detected).
    /// Confidence decays toward 0.0 by multiplying by a decay factor.
    /// If confidence drops below the threshold (0.1), auto-resolve the issue.
    pub async fn decay_known_issue_confidence(&self, id: &str) -> Result<(), String> {
        const DECAY_FACTOR: f64 = 0.85;
        const AUTO_RESOLVE_THRESHOLD: f64 = 0.1;

        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let now = Utc::now().to_rfc3339();

        let row = conn
            .query_opt(
                "SELECT confidence FROM known_issues WHERE id = $1 AND status = 'active'",
                &[&id],
            )
            .await
            .map_err(|e| format!("PG decay confidence query: {}", e))?;

        let current_confidence: f64 = match row {
            Some(r) => r.get(0),
            None => return Ok(()), // Not active or not found
        };

        let new_confidence = current_confidence * DECAY_FACTOR;

        if new_confidence < AUTO_RESOLVE_THRESHOLD {
            conn.execute(
                r#"UPDATE known_issues
                   SET confidence = $2, status = 'resolved', resolved_at = $3, updated_at = $3
                   WHERE id = $1"#,
                &[&id, &new_confidence, &now],
            )
            .await
            .map_err(|e| format!("PG auto-resolve known issue: {}", e))?;
        } else {
            conn.execute(
                r#"UPDATE known_issues SET confidence = $2, updated_at = $3 WHERE id = $1"#,
                &[&id, &new_confidence, &now],
            )
            .await
            .map_err(|e| format!("PG decay confidence: {}", e))?;
        }

        Ok(())
    }

    /// Delete a known issue by ID.
    pub async fn delete_known_issue(&self, id: &str) -> Result<bool, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let affected = conn
            .execute("DELETE FROM known_issues WHERE id = $1", &[&id])
            .await
            .map_err(|e| format!("PG delete_known_issue: {}", e))?;

        Ok(affected > 0)
    }

    /// Resolve a known issue (set status to resolved + resolved_at).
    pub async fn resolve_known_issue(
        &self,
        id: &str,
        resolution: Option<&str>,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let now = Utc::now().to_rfc3339();

        let affected = if let Some(note) = resolution {
            conn.execute(
                r#"
                UPDATE known_issues
                SET status = 'resolved',
                    resolved_at = $2,
                    description = description || E'\n\n' || 'Resolution: ' || $3,
                    updated_at = $2
                WHERE id = $1
                "#,
                &[&id, &now, &note],
            )
            .await
            .map_err(|e| format!("PG resolve_known_issue: {}", e))?
        } else {
            conn.execute(
                r#"
                UPDATE known_issues
                SET status = 'resolved',
                    resolved_at = $2,
                    updated_at = $2
                WHERE id = $1
                "#,
                &[&id, &now],
            )
            .await
            .map_err(|e| format!("PG resolve_known_issue: {}", e))?
        };

        if affected == 0 {
            tracing::warn!("resolve_known_issue: no issue found with id '{}'", id);
        } else {
            tracing::info!("Resolved known issue '{}'", id);
        }

        Ok(())
    }

    /// Find issues relevant to a spec (by spec_id, URL, or global).
    pub async fn find_issues_for_spec(
        &self,
        spec_id: &str,
        page_url: Option<&str>,
    ) -> Result<Vec<KnownIssue>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let url = page_url.unwrap_or("");

        let rows = conn
            .query(
                r#"
                SELECT id, title, description, category, scope_type, scope_value,
                       scope_tags, detection_method, detection_config, pattern_template_id,
                       reproduction_context, trigger_conditions, severity, status,
                       confidence, provenance, source_finding_ids, source_task_run_id,
                       verification_hint, verification_step_template,
                       times_detected, times_checked,
                       last_detected_at::TEXT, last_checked_at::TEXT, resolved_at::TEXT,
                       created_at::TEXT, updated_at::TEXT
                FROM known_issues
                WHERE status = 'active'
                  AND (
                    (scope_type = 'spec' AND scope_value = $1)
                    OR scope_type = 'global'
                    OR (scope_type = 'url' AND scope_value = $2)
                  )
                ORDER BY
                    CASE severity WHEN 'critical' THEN 0 WHEN 'high' THEN 1 WHEN 'medium' THEN 2 WHEN 'low' THEN 3 END
                "#,
                &[&spec_id, &url],
            )
            .await
            .map_err(|e| format!("PG find_issues_for_spec: {}", e))?;

        Ok(rows
            .iter()
            .map(|r| Self::ki_row_to_known_issue(r))
            .collect())
    }

    // ========================================================================
    // Pattern Templates
    // ========================================================================

    /// List all pattern templates.
    pub async fn list_pattern_templates(&self) -> Result<Vec<IssuePatternTemplate>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                r#"
                SELECT id, name, description, category, detection_type,
                       step_template, ai_prompt_template, parameters,
                       built_in, status, created_at::TEXT, updated_at::TEXT
                FROM issue_pattern_templates
                ORDER BY name
                "#,
                &[],
            )
            .await
            .map_err(|e| format!("PG list_pattern_templates: {}", e))?;

        Ok(rows
            .iter()
            .map(|r| Self::ki_row_to_pattern_template(r))
            .collect())
    }

    /// Insert a new pattern template.
    pub async fn insert_pattern_template(
        &self,
        req: &CreatePatternTemplateRequest,
    ) -> Result<IssuePatternTemplate, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let id = uuid::Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();

        let parameters_json = req
            .parameters
            .as_deref()
            .and_then(|s| {
                let trimmed = s.trim();
                if trimmed.is_empty() {
                    None
                } else {
                    serde_json::from_str::<serde_json::Value>(trimmed)
                        .ok()
                        .map(|_| trimmed.to_string())
                }
            })
            .unwrap_or_else(|| "[]".to_string());
        let built_in = false;
        let status = "active".to_string();
        let step_template: Option<String> = None;

        conn.execute(
            r#"
            INSERT INTO issue_pattern_templates (
                id, name, description, category, detection_type,
                step_template, ai_prompt_template, parameters,
                built_in, status, created_at, updated_at
            ) VALUES (
                $1, $2, $3, $4, $5,
                $6, $7, $8,
                $9, $10, $11::TIMESTAMPTZ, $11::TIMESTAMPTZ
            )
            "#,
            &[
                &id as &(dyn tokio_postgres::types::ToSql + Sync),
                &req.name as &(dyn tokio_postgres::types::ToSql + Sync),
                &req.description as &(dyn tokio_postgres::types::ToSql + Sync),
                &req.category as &(dyn tokio_postgres::types::ToSql + Sync),
                &req.detection_type as &(dyn tokio_postgres::types::ToSql + Sync),
                &step_template as &(dyn tokio_postgres::types::ToSql + Sync),
                &req.ai_prompt_template as &(dyn tokio_postgres::types::ToSql + Sync),
                &parameters_json as &(dyn tokio_postgres::types::ToSql + Sync),
                &built_in as &(dyn tokio_postgres::types::ToSql + Sync),
                &status as &(dyn tokio_postgres::types::ToSql + Sync),
                &now as &(dyn tokio_postgres::types::ToSql + Sync),
            ],
        )
        .await
        .map_err(|e| format!("PG insert_pattern_template: {}", e))?;

        tracing::info!("Inserted pattern template '{}' (id={})", req.name, id);

        // Re-query to return the inserted template
        let row = conn
            .query_one(
                r#"
                SELECT id, name, description, category, detection_type,
                       step_template, ai_prompt_template, parameters,
                       built_in, status, created_at::TEXT, updated_at::TEXT
                FROM issue_pattern_templates
                WHERE id = $1
                "#,
                &[&id],
            )
            .await
            .map_err(|e| format!("PG get pattern template after insert: {}", e))?;

        Ok(Self::ki_row_to_pattern_template(&row))
    }

    /// Get a pattern template by ID.
    pub async fn get_pattern_template(
        &self,
        id: &str,
    ) -> Result<Option<IssuePatternTemplate>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                r#"
                SELECT id, name, description, category, detection_type,
                       step_template, ai_prompt_template, parameters,
                       built_in, status, created_at::TEXT, updated_at::TEXT
                FROM issue_pattern_templates
                WHERE id = $1
                "#,
                &[&id],
            )
            .await
            .map_err(|e| format!("PG get_pattern_template: {}", e))?;

        Ok(rows.first().map(Self::ki_row_to_pattern_template))
    }

    /// Find active known issues relevant for workflow generation based on depth level.
    /// - "thorough": only critical + high severity
    /// - "regression": all active issues
    pub async fn find_relevant_issues_for_generation(
        &self,
        depth: &str,
    ) -> Result<Vec<KnownIssue>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let sql = match depth {
            "thorough" => {
                r#"SELECT id, title, description, category, scope_type, scope_value,
                          scope_tags, detection_method, detection_config, pattern_template_id,
                          reproduction_context, trigger_conditions, severity, status,
                          confidence, provenance, source_finding_ids, source_task_run_id,
                          verification_hint, verification_step_template,
                          times_detected, times_checked,
                          last_detected_at::TEXT, last_checked_at::TEXT, resolved_at::TEXT,
                          created_at::TEXT, updated_at::TEXT
                   FROM known_issues
                   WHERE status = 'active' AND severity IN ('critical', 'high')
                   ORDER BY CASE severity WHEN 'critical' THEN 0 WHEN 'high' THEN 1 END,
                            times_detected DESC"#
            }
            _ => {
                r#"SELECT id, title, description, category, scope_type, scope_value,
                          scope_tags, detection_method, detection_config, pattern_template_id,
                          reproduction_context, trigger_conditions, severity, status,
                          confidence, provenance, source_finding_ids, source_task_run_id,
                          verification_hint, verification_step_template,
                          times_detected, times_checked,
                          last_detected_at::TEXT, last_checked_at::TEXT, resolved_at::TEXT,
                          created_at::TEXT, updated_at::TEXT
                   FROM known_issues
                   WHERE status = 'active'
                   ORDER BY CASE severity WHEN 'critical' THEN 0 WHEN 'high' THEN 1 WHEN 'medium' THEN 2 WHEN 'low' THEN 3 END,
                            times_detected DESC"#
            }
        };

        let rows = conn
            .query(sql, &[])
            .await
            .map_err(|e| format!("PG find_relevant_issues_for_generation: {}", e))?;

        Ok(rows
            .iter()
            .map(|r| Self::ki_row_to_known_issue(r))
            .collect())
    }

    // -- helpers --

    fn ki_row_to_pattern_template(row: &tokio_postgres::Row) -> IssuePatternTemplate {
        let step_template_json: Option<String> = row.get(5);
        let parameters_json: String = row
            .get::<_, Option<String>>(7)
            .unwrap_or_else(|| "[]".to_string());
        let built_in_val: bool = row.get(8);

        IssuePatternTemplate {
            id: row.get(0),
            name: row.get(1),
            description: row.get(2),
            category: row.get(3),
            detection_type: row.get(4),
            step_template: step_template_json.and_then(|s| serde_json::from_str(&s).ok()),
            ai_prompt_template: row.get(6),
            parameters: serde_json::from_str(&parameters_json).unwrap_or_default(),
            built_in: built_in_val,
            status: row.get(9),
            created_at: row.get(10),
            updated_at: row.get(11),
        }
    }

    fn ki_row_to_known_issue(row: &tokio_postgres::Row) -> KnownIssue {
        let scope_tags_json: String = row
            .get::<_, Option<String>>(6)
            .unwrap_or_else(|| "[]".to_string());
        let detection_config_json: String = row
            .get::<_, Option<String>>(8)
            .unwrap_or_else(|| "{}".to_string());
        let trigger_conditions_json: String = row
            .get::<_, Option<String>>(11)
            .unwrap_or_else(|| "[]".to_string());
        let source_finding_ids_json: String = row
            .get::<_, Option<String>>(16)
            .unwrap_or_else(|| "[]".to_string());
        let verification_step_json: Option<String> = row.get(19);
        let times_detected: i32 = row.get::<_, Option<i32>>(20).unwrap_or(1);
        let times_checked: i32 = row.get::<_, Option<i32>>(21).unwrap_or(0);

        // Timestamp columns are TIMESTAMPTZ in PG but typed as String in Rust — SELECTs
        // cast ::TEXT so these decode as Option<String>. Use try_get to avoid panicking
        // the tokio worker if a legacy row or schema drift slips a non-text value through.
        let last_detected_at: Option<String> = row.try_get(22).unwrap_or_else(|e| {
            tracing::warn!("ki_row_to_known_issue: col 22 (last_detected_at) decode error: {}", e);
            None
        });
        let last_checked_at: Option<String> = row.try_get(23).unwrap_or_else(|e| {
            tracing::warn!("ki_row_to_known_issue: col 23 (last_checked_at) decode error: {}", e);
            None
        });
        let resolved_at: Option<String> = row.try_get(24).unwrap_or_else(|e| {
            tracing::warn!("ki_row_to_known_issue: col 24 (resolved_at) decode error: {}", e);
            None
        });
        let created_at: String = row.try_get(25).unwrap_or_else(|e| {
            tracing::warn!("ki_row_to_known_issue: col 25 (created_at) decode error: {}", e);
            String::new()
        });
        let updated_at: String = row.try_get(26).unwrap_or_else(|e| {
            tracing::warn!("ki_row_to_known_issue: col 26 (updated_at) decode error: {}", e);
            String::new()
        });

        KnownIssue {
            id: row.get(0),
            title: row.get(1),
            description: row.get(2),
            category: IssueCategory::from_str(&row.get::<_, String>(3))
                .unwrap_or(IssueCategory::Other),
            scope_type: ScopeType::from_str(&row.get::<_, String>(4)).unwrap_or(ScopeType::Global),
            scope_value: row.get(5),
            scope_tags: serde_json::from_str(&scope_tags_json).unwrap_or_default(),
            detection_method: DetectionMethod::from_str(&row.get::<_, String>(7))
                .unwrap_or(DetectionMethod::AiJudgment),
            detection_config: serde_json::from_str(&detection_config_json)
                .unwrap_or(serde_json::json!({})),
            pattern_template_id: row.get(9),
            reproduction_context: row.get(10),
            trigger_conditions: serde_json::from_str(&trigger_conditions_json).unwrap_or_default(),
            severity: IssueSeverity::from_str(&row.get::<_, String>(12))
                .unwrap_or(IssueSeverity::Medium),
            status: IssueStatus::from_str(&row.get::<_, String>(13)).unwrap_or(IssueStatus::Active),
            confidence: row.get(14),
            provenance: IssueProvenance::from_str(&row.get::<_, String>(15))
                .unwrap_or(IssueProvenance::Manual),
            source_finding_ids: serde_json::from_str(&source_finding_ids_json).unwrap_or_default(),
            source_task_run_id: row.get(17),
            verification_hint: row.get(18),
            verification_step_template: verification_step_json
                .and_then(|s| serde_json::from_str(&s).ok()),
            times_detected: times_detected as u32,
            times_checked: times_checked as u32,
            last_detected_at,
            last_checked_at,
            resolved_at,
            created_at,
            updated_at,
        }
    }
}

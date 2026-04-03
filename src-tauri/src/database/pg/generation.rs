//! PostgreSQL generation rules operations.
//!
//! Provides CRUD for the `generation_rules` table using raw SQL.

use super::PgDb;
use crate::workflow_generation::rules::{
    GenerationRule, InsertRuleInput, ListRulesQuery, UpdateRuleInput,
};
use tracing::{info, warn};

fn row_to_rule(row: &tokio_postgres::Row) -> GenerationRule {
    GenerationRule {
        id: row.get("id"),
        agent: row.get("agent"),
        section: row.get("section"),
        rule_number: row.get("rule_number"),
        title: row.get("title"),
        content: row.get("content"),
        condition: row.get("condition"),
        status: row.get("status"),
        provenance: row.get("provenance"),
        source_fix_id: row.get("source_fix_id"),
        severity: row.get("severity"),
        failure_count: row.get("failure_count"),
        examples_json: row.get("examples_json"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
    }
}

impl PgDb {
    // ========================================================================
    // Generation Rules
    // ========================================================================

    /// Get active rules for a specific agent, optionally filtered by section.
    pub async fn get_active_rules(
        &self,
        agent: &str,
        section: Option<&str>,
    ) -> Result<Vec<GenerationRule>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = if let Some(section) = section {
            conn.query(
                r#"SELECT id, agent, section, rule_number, title, content, condition, status,
                          provenance, source_fix_id, severity, failure_count, examples_json,
                          created_at::TEXT, updated_at::TEXT
                   FROM generation_rules
                   WHERE agent = $1 AND section = $2 AND status = 'active'
                   ORDER BY rule_number"#,
                &[&agent, &section],
            )
            .await
        } else {
            conn.query(
                r#"SELECT id, agent, section, rule_number, title, content, condition, status,
                          provenance, source_fix_id, severity, failure_count, examples_json,
                          created_at::TEXT, updated_at::TEXT
                   FROM generation_rules
                   WHERE agent = $1 AND status = 'active'
                   ORDER BY section, rule_number"#,
                &[&agent],
            )
            .await
        }
        .map_err(|e| format!("PG get_active_rules: {}", e))?;

        Ok(rows.iter().map(row_to_rule).collect())
    }

    /// Get a single rule by ID.
    pub async fn get_rule_by_id(&self, id: &str) -> Result<Option<GenerationRule>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                r#"SELECT id, agent, section, rule_number, title, content, condition, status,
                          provenance, source_fix_id, severity, failure_count, examples_json,
                          created_at::TEXT, updated_at::TEXT
                   FROM generation_rules WHERE id = $1"#,
                &[&id],
            )
            .await
            .map_err(|e| format!("PG get_rule_by_id: {}", e))?;

        Ok(rows.first().map(row_to_rule))
    }

    /// Upsert a generation rule (INSERT ON CONFLICT DO UPDATE).
    pub async fn upsert_rule(&self, input: &InsertRuleInput) -> Result<GenerationRule, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let id = format!("rule-{}", uuid::Uuid::new_v4());
        let now = chrono::Utc::now().to_rfc3339();
        let severity = input.severity.as_deref().unwrap_or("normal");

        conn.execute(
            r#"INSERT INTO generation_rules
                (id, agent, section, rule_number, title, content, condition, status,
                 provenance, source_fix_id, severity, examples_json, created_at, updated_at)
               VALUES ($1, $2, $3, $4, $5, $6, $7, 'active', $8, $9, $10, $11, $12, $13)
               ON CONFLICT (id) DO UPDATE SET
                 title = EXCLUDED.title,
                 content = EXCLUDED.content,
                 condition = EXCLUDED.condition,
                 severity = EXCLUDED.severity,
                 examples_json = EXCLUDED.examples_json,
                 updated_at = EXCLUDED.updated_at"#,
            &[
                &id as &(dyn tokio_postgres::types::ToSql + Sync),
                &input.agent,
                &input.section,
                &input.rule_number,
                &input.title,
                &input.content,
                &input.condition as &(dyn tokio_postgres::types::ToSql + Sync),
                &input.provenance,
                &input.source_fix_id as &(dyn tokio_postgres::types::ToSql + Sync),
                &severity,
                &input.examples_json as &(dyn tokio_postgres::types::ToSql + Sync),
                &now,
                &now,
            ],
        )
        .await
        .map_err(|e| format!("PG upsert_rule: {}", e))?;

        Ok(GenerationRule {
            id,
            agent: input.agent.clone(),
            section: input.section.clone(),
            rule_number: input.rule_number,
            title: input.title.clone(),
            content: input.content.clone(),
            condition: input.condition.clone(),
            status: "active".to_string(),
            provenance: input.provenance.clone(),
            source_fix_id: input.source_fix_id.clone(),
            severity: severity.to_string(),
            failure_count: 0,
            examples_json: input.examples_json.clone(),
            created_at: now.clone(),
            updated_at: now,
        })
    }

    /// List all generation rules with optional filters.
    pub async fn list_all_rules(
        &self,
        query: &ListRulesQuery,
    ) -> Result<Vec<GenerationRule>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let mut sql = String::from(
            r#"SELECT id, agent, section, rule_number, title, content, condition, status,
                      provenance, source_fix_id, severity, failure_count, examples_json,
                      created_at::TEXT, updated_at::TEXT
               FROM generation_rules WHERE 1=1"#,
        );
        let mut params: Vec<Box<dyn tokio_postgres::types::ToSql + Sync + Send>> = Vec::new();

        if let Some(ref agent) = query.agent {
            params.push(Box::new(agent.clone()));
            sql.push_str(&format!(" AND agent = ${}", params.len()));
        }
        if let Some(ref section) = query.section {
            params.push(Box::new(section.clone()));
            sql.push_str(&format!(" AND section = ${}", params.len()));
        }
        if let Some(ref status) = query.status {
            params.push(Box::new(status.clone()));
            sql.push_str(&format!(" AND status = ${}", params.len()));
        }
        if let Some(ref provenance) = query.provenance {
            params.push(Box::new(provenance.clone()));
            sql.push_str(&format!(" AND provenance = ${}", params.len()));
        }
        if let Some(ref severity) = query.severity {
            params.push(Box::new(severity.clone()));
            sql.push_str(&format!(" AND severity = ${}", params.len()));
        }
        sql.push_str(" ORDER BY agent, section, rule_number");

        let param_refs: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = params
            .iter()
            .map(|p| p.as_ref() as &(dyn tokio_postgres::types::ToSql + Sync))
            .collect();

        let rows = conn
            .query(&sql, &param_refs)
            .await
            .map_err(|e| format!("PG list_all_rules: {}", e))?;

        Ok(rows.iter().map(row_to_rule).collect())
    }

    /// Update an existing generation rule.
    pub async fn update_rule(
        &self,
        id: &str,
        input: &UpdateRuleInput,
    ) -> Result<GenerationRule, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let now = chrono::Utc::now().to_rfc3339();

        let mut sets = vec!["updated_at = $1".to_string()];
        let mut param_values: Vec<Box<dyn tokio_postgres::types::ToSql + Sync + Send>> = Vec::new();
        param_values.push(Box::new(now));
        let mut param_idx = 2u32;

        if let Some(ref title) = input.title {
            sets.push(format!("title = ${}", param_idx));
            param_values.push(Box::new(title.clone()));
            param_idx += 1;
        }
        if let Some(ref content) = input.content {
            sets.push(format!("content = ${}", param_idx));
            param_values.push(Box::new(content.clone()));
            param_idx += 1;
        }
        if let Some(ref condition) = input.condition {
            sets.push(format!("condition = ${}", param_idx));
            param_values.push(Box::new(condition.clone()));
            param_idx += 1;
        }
        if let Some(ref status) = input.status {
            sets.push(format!("status = ${}", param_idx));
            param_values.push(Box::new(status.clone()));
            param_idx += 1;
        }
        if let Some(rule_number) = input.rule_number {
            sets.push(format!("rule_number = ${}", param_idx));
            param_values.push(Box::new(rule_number));
            param_idx += 1;
        }
        if let Some(ref severity) = input.severity {
            sets.push(format!("severity = ${}", param_idx));
            param_values.push(Box::new(severity.clone()));
            param_idx += 1;
        }
        if let Some(ref examples_json) = input.examples_json {
            sets.push(format!("examples_json = ${}", param_idx));
            param_values.push(Box::new(examples_json.clone()));
            param_idx += 1;
        }

        let sql = format!(
            "UPDATE generation_rules SET {} WHERE id = ${}",
            sets.join(", "),
            param_idx
        );
        param_values.push(Box::new(id.to_string()));

        let params: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = param_values
            .iter()
            .map(|v| v.as_ref() as &(dyn tokio_postgres::types::ToSql + Sync))
            .collect();

        conn.execute(&sql, &params)
            .await
            .map_err(|e| format!("PG update_rule: {}", e))?;

        self.get_rule_by_id(id)
            .await?
            .ok_or_else(|| format!("Rule {} not found after update", id))
    }

    /// Delete a generation rule by ID.
    pub async fn delete_rule(&self, id: &str) -> Result<bool, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let affected = conn
            .execute("DELETE FROM generation_rules WHERE id = $1", &[&id])
            .await
            .map_err(|e| format!("PG delete_rule: {}", e))?;

        Ok(affected > 0)
    }

    /// PG equivalent of exploration_stats query for a workflow.
    pub async fn get_exploration_stats(
        &self,
        workflow_id: &str,
    ) -> Result<Option<serde_json::Value>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool: {}", e))?;

        let row = conn
            .query_opt(
                r#"SELECT id, workflow_id, total_candidates, search_depth, search_duration_ms,
                          best_score, strategy_used, score_progression, created_at::TEXT
                   FROM exploration_stats
                   WHERE workflow_id = $1
                   ORDER BY created_at DESC LIMIT 1"#,
                &[&workflow_id],
            )
            .await
            .map_err(|e| format!("PG get_exploration_stats: {}", e))?;

        match row {
            Some(r) => Ok(Some(serde_json::json!({
                "id": r.get::<_, String>(0),
                "workflow_id": r.get::<_, String>(1),
                "total_candidates": r.get::<_, i32>(2),
                "search_depth": r.get::<_, i32>(3),
                "search_duration_ms": r.get::<_, i64>(4),
                "best_score": r.get::<_, Option<f64>>(5),
                "strategy_used": r.get::<_, Option<String>>(6),
                "score_progression": r.get::<_, Option<String>>(7),
                "created_at": r.get::<_, String>(8),
            }))),
            None => Ok(None),
        }
    }

    /// PG equivalent of template library list (returns empty if table missing).
    pub async fn list_templates(
        &self,
        domain_filter: Option<&str>,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool: {}", e))?;

        // Check if the step_templates table exists in PG
        let exists = conn
            .query_one(
                "SELECT EXISTS(SELECT 1 FROM information_schema.tables WHERE table_name = 'step_templates')",
                &[],
            )
            .await
            .map(|r| r.get::<_, bool>(0))
            .unwrap_or(false);

        if !exists {
            return Ok(vec![]);
        }

        let rows = if let Some(domain) = domain_filter {
            conn.query(
                "SELECT id, name, domain, template_json, created_at::TEXT FROM step_templates WHERE domain = $1 ORDER BY name",
                &[&domain],
            ).await
        } else {
            conn.query(
                "SELECT id, name, domain, template_json, created_at::TEXT FROM step_templates ORDER BY name",
                &[],
            ).await
        }.map_err(|e| format!("PG list_templates: {}", e))?;

        Ok(rows
            .iter()
            .map(|r| {
                serde_json::json!({
                    "id": r.get::<_, String>(0),
                    "name": r.get::<_, String>(1),
                    "domain": r.get::<_, String>(2),
                    "template_json": r.get::<_, String>(3),
                    "created_at": r.get::<_, String>(4),
                })
            })
            .collect())
    }

    // ========================================================================
    // Generation Rules — helpers for meta-optimizer recommendations
    // ========================================================================

    /// Get the next rule number for a given agent/section.
    pub async fn next_rule_number(&self, agent: &str, section: &str) -> Result<i32, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let row = conn.query_one(
            "SELECT COALESCE(MAX(rule_number), 0) + 1 FROM generation_rules WHERE agent = $1 AND section = $2",
            &[&agent, &section],
        ).await.map_err(|e| format!("PG next_rule_number: {}", e))?;
        Ok(row.get(0))
    }

    /// Insert a new generation rule (PG version matching the SQLite insert_rule).
    pub async fn insert_rule(
        &self,
        input: &crate::workflow_generation::rules::InsertRuleInput,
    ) -> Result<crate::workflow_generation::rules::GenerationRule, String> {
        self.upsert_rule(input).await
    }

    /// Check if a generation rule exists by ID.
    pub async fn rule_exists(&self, id: &str) -> Result<bool, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let row = conn
            .query_one(
                "SELECT COUNT(*) > 0 FROM generation_rules WHERE id = $1",
                &[&id],
            )
            .await
            .map_err(|e| format!("PG rule_exists: {}", e))?;
        Ok(row.get(0))
    }

    /// Get examples_json for a rule.
    pub async fn get_rule_examples_json(&self, id: &str) -> Result<Option<String>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let row = conn
            .query_opt(
                "SELECT examples_json FROM generation_rules WHERE id = $1",
                &[&id],
            )
            .await
            .map_err(|e| format!("PG get_rule_examples: {}", e))?;
        Ok(row.and_then(|r| r.get(0)))
    }

    /// Update examples_json for a rule.
    pub async fn update_rule_examples(&self, id: &str, examples_json: &str) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let now = chrono::Utc::now().to_rfc3339();
        conn.execute(
            "UPDATE generation_rules SET examples_json = $1, updated_at = $2 WHERE id = $3",
            &[&examples_json, &now, &id],
        )
        .await
        .map_err(|e| format!("PG update_rule_examples: {}", e))?;
        Ok(())
    }

    /// Find a rule by source_fix_id (used for rollback).
    pub async fn find_rule_by_source_fix_id(
        &self,
        source_fix_id: &str,
    ) -> Result<Option<String>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let row = conn
            .query_opt(
                "SELECT id FROM generation_rules WHERE source_fix_id = $1 LIMIT 1",
                &[&source_fix_id],
            )
            .await
            .map_err(|e| format!("PG find_rule_by_source_fix_id: {}", e))?;
        Ok(row.map(|r| r.get(0)))
    }

    /// Increment the failure_count for a rule and auto-promote to 'critical'
    /// once failure_count reaches 5.
    pub async fn increment_rule_failure_count(&self, rule_id: &str) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        conn.execute(
            r#"UPDATE generation_rules
               SET failure_count = failure_count + 1,
                   severity = CASE WHEN failure_count + 1 >= 5 THEN 'critical' ELSE severity END,
                   updated_at = NOW()
               WHERE id = $1"#,
            &[&rule_id],
        )
        .await
        .map_err(|e| format!("PG increment_rule_failure_count: {}", e))?;
        Ok(())
    }
}

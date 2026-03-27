//! PostgreSQL generation rules operations.
//!
//! Provides CRUD for the `generation_rules` table using raw SQL.

use super::PgDb;
use crate::workflow_generation::rules::{GenerationRule, InsertRuleInput, ListRulesQuery, UpdateRuleInput};
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
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let rows = if let Some(section) = section {
            conn.query(
                r#"SELECT id, agent, section, rule_number, title, content, condition, status,
                          provenance, source_fix_id, severity, failure_count, examples_json,
                          created_at, updated_at
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
                          created_at, updated_at
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
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                r#"SELECT id, agent, section, rule_number, title, content, condition, status,
                          provenance, source_fix_id, severity, failure_count, examples_json,
                          created_at, updated_at
                   FROM generation_rules WHERE id = $1"#,
                &[&id],
            )
            .await
            .map_err(|e| format!("PG get_rule_by_id: {}", e))?;

        Ok(rows.first().map(row_to_rule))
    }

    /// Upsert a generation rule (INSERT ON CONFLICT DO UPDATE).
    pub async fn upsert_rule(&self, input: &InsertRuleInput) -> Result<GenerationRule, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

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
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let mut sql = String::from(
            r#"SELECT id, agent, section, rule_number, title, content, condition, status,
                      provenance, source_fix_id, severity, failure_count, examples_json,
                      created_at, updated_at
               FROM generation_rules WHERE 1=1"#,
        );
        let mut params: Vec<Box<dyn tokio_postgres::types::ToSql + Sync>> = Vec::new();

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

        let param_refs: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> =
            params.iter().map(|p| p.as_ref()).collect();

        let rows = conn
            .query(&sql, &param_refs)
            .await
            .map_err(|e| format!("PG list_all_rules: {}", e))?;

        Ok(rows.iter().map(row_to_rule).collect())
    }

    /// Delete a generation rule by ID.
    pub async fn delete_rule(&self, id: &str) -> Result<bool, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let affected = conn
            .execute("DELETE FROM generation_rules WHERE id = $1", &[&id])
            .await
            .map_err(|e| format!("PG delete_rule: {}", e))?;

        Ok(affected > 0)
    }
}

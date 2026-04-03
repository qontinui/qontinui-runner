//! PostgreSQL step_type_knowledge CRUD operations (raw SQL).
//!
//! Mirrors the SQLite workflow_generation/step_type_knowledge.rs operations.

use super::PgDb;
use crate::workflow_generation::step_type_knowledge::{
    InsertKnowledgeInput, ListKnowledgeQuery, StepTypeKnowledge, UpdateKnowledgeInput,
};
use chrono::Utc;

impl PgDb {
    // ========================================================================
    // Step Type Knowledge
    // ========================================================================

    /// Load active knowledge entries for the given step types.
    pub async fn load_knowledge_for_step_types(
        &self,
        step_types: &[&str],
    ) -> Result<Vec<StepTypeKnowledge>, String> {
        if step_types.is_empty() {
            return Ok(vec![]);
        }

        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        // Build IN clause with positional parameters
        let placeholders: Vec<String> = (1..=step_types.len()).map(|i| format!("${}", i)).collect();
        let sql = format!(
            r#"
            SELECT id, step_type, layer, title, content, priority, status, provenance, source_fix_id, created_at, updated_at
            FROM step_type_knowledge
            WHERE step_type IN ({}) AND status = 'active'
            ORDER BY step_type, priority DESC
            "#,
            placeholders.join(", ")
        );

        let params: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = step_types
            .iter()
            .map(|s| s as &(dyn tokio_postgres::types::ToSql + Sync))
            .collect();

        let rows = conn
            .query(&sql, &params)
            .await
            .map_err(|e| format!("PG load_knowledge_for_step_types: {}", e))?;

        Ok(rows.iter().map(|r| Self::stk_row_to_struct(r)).collect())
    }

    /// List knowledge entries with optional filters.
    pub async fn list_step_type_knowledge(
        &self,
        query: &ListKnowledgeQuery,
    ) -> Result<Vec<StepTypeKnowledge>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let mut sql = String::from(
            "SELECT id, step_type, layer, title, content, priority, status, provenance, source_fix_id, created_at, updated_at
             FROM step_type_knowledge WHERE 1=1",
        );
        let mut param_values: Vec<String> = Vec::new();

        if let Some(ref step_type) = query.step_type {
            param_values.push(step_type.clone());
            sql.push_str(&format!(" AND step_type = ${}", param_values.len()));
        }
        if let Some(ref layer) = query.layer {
            param_values.push(layer.clone());
            sql.push_str(&format!(" AND layer = ${}", param_values.len()));
        }
        if let Some(ref status) = query.status {
            param_values.push(status.clone());
            sql.push_str(&format!(" AND status = ${}", param_values.len()));
        }
        sql.push_str(" ORDER BY step_type, priority DESC");

        let params: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = param_values
            .iter()
            .map(|v| v as &(dyn tokio_postgres::types::ToSql + Sync))
            .collect();

        let rows = conn
            .query(&sql, &params)
            .await
            .map_err(|e| format!("PG list_step_type_knowledge: {}", e))?;

        Ok(rows.iter().map(|r| Self::stk_row_to_struct(r)).collect())
    }

    /// Get a single knowledge entry by ID.
    pub async fn get_step_type_knowledge(
        &self,
        id: &str,
    ) -> Result<Option<StepTypeKnowledge>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let row = conn
            .query_opt(
                "SELECT id, step_type, layer, title, content, priority, status, provenance, source_fix_id, created_at, updated_at
                 FROM step_type_knowledge WHERE id = $1",
                &[&id],
            )
            .await
            .map_err(|e| format!("PG get_step_type_knowledge: {}", e))?;

        Ok(row.map(|r| Self::stk_row_to_struct(&r)))
    }

    /// Insert a new knowledge entry (with dedup guard on source_fix_id).
    pub async fn insert_step_type_knowledge(
        &self,
        input: &InsertKnowledgeInput,
    ) -> Result<StepTypeKnowledge, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        // Dedup guard
        if let Some(ref fix_id) = input.source_fix_id {
            let existing = conn
                .query_opt(
                    "SELECT id, step_type, layer, title, content, priority, status, provenance, source_fix_id, created_at, updated_at
                     FROM step_type_knowledge WHERE source_fix_id = $1 LIMIT 1",
                    &[fix_id],
                )
                .await
                .map_err(|e| format!("PG dedup check: {}", e))?;

            if let Some(r) = existing {
                return Ok(Self::stk_row_to_struct(&r));
            }
        }

        let id = format!("stk-{}", uuid::Uuid::new_v4());
        let now = Utc::now().to_rfc3339();

        conn.execute(
            r#"
            INSERT INTO step_type_knowledge (id, step_type, layer, title, content, priority, status, provenance, source_fix_id, created_at, updated_at)
            VALUES ($1, $2, $3, $4, $5, $6, 'active', $7, $8, $9, $10)
            "#,
            &[
                &id as &(dyn tokio_postgres::types::ToSql + Sync),
                &input.step_type as &(dyn tokio_postgres::types::ToSql + Sync),
                &input.layer as &(dyn tokio_postgres::types::ToSql + Sync),
                &input.title as &(dyn tokio_postgres::types::ToSql + Sync),
                &input.content as &(dyn tokio_postgres::types::ToSql + Sync),
                &input.priority as &(dyn tokio_postgres::types::ToSql + Sync),
                &input.provenance as &(dyn tokio_postgres::types::ToSql + Sync),
                &input.source_fix_id as &(dyn tokio_postgres::types::ToSql + Sync),
                &now as &(dyn tokio_postgres::types::ToSql + Sync),
                &now as &(dyn tokio_postgres::types::ToSql + Sync),
            ],
        )
        .await
        .map_err(|e| format!("PG insert_step_type_knowledge: {}", e))?;

        Ok(StepTypeKnowledge {
            id,
            step_type: input.step_type.clone(),
            layer: input.layer.clone(),
            title: input.title.clone(),
            content: input.content.clone(),
            priority: input.priority,
            status: "active".to_string(),
            provenance: input.provenance.clone(),
            source_fix_id: input.source_fix_id.clone(),
            created_at: now.clone(),
            updated_at: now,
        })
    }

    /// Update an existing knowledge entry.
    pub async fn update_step_type_knowledge(
        &self,
        id: &str,
        input: &UpdateKnowledgeInput,
    ) -> Result<StepTypeKnowledge, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let now = Utc::now().to_rfc3339();

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
        if let Some(priority) = input.priority {
            sets.push(format!("priority = ${}", param_idx));
            param_values.push(Box::new(priority));
            param_idx += 1;
        }
        if let Some(ref status) = input.status {
            sets.push(format!("status = ${}", param_idx));
            param_values.push(Box::new(status.clone()));
            param_idx += 1;
        }
        if let Some(ref layer) = input.layer {
            sets.push(format!("layer = ${}", param_idx));
            param_values.push(Box::new(layer.clone()));
            param_idx += 1;
        }

        let sql = format!(
            "UPDATE step_type_knowledge SET {} WHERE id = ${}",
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
            .map_err(|e| format!("PG update_step_type_knowledge: {}", e))?;

        self.get_step_type_knowledge(id)
            .await?
            .ok_or_else(|| format!("Knowledge entry {} not found after update", id))
    }

    /// Delete a knowledge entry by ID.
    pub async fn delete_step_type_knowledge(&self, id: &str) -> Result<bool, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let affected = conn
            .execute("DELETE FROM step_type_knowledge WHERE id = $1", &[&id])
            .await
            .map_err(|e| format!("PG delete_step_type_knowledge: {}", e))?;

        Ok(affected > 0)
    }

    // -- helpers --

    fn stk_row_to_struct(row: &tokio_postgres::Row) -> StepTypeKnowledge {
        StepTypeKnowledge {
            id: row.get(0),
            step_type: row.get(1),
            layer: row.get(2),
            title: row.get(3),
            content: row.get(4),
            priority: row.get(5),
            status: row.get(6),
            provenance: row.get(7),
            source_fix_id: row.get(8),
            created_at: row.get(9),
            updated_at: row.get(10),
        }
    }
}

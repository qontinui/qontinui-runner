//! PostgreSQL wrapper for prompt registry operations.
//!
//! TODO: Switch to Clorinde-generated queries once `clorinde` is run against the
//! prompt_registry table. For now uses raw tokio-postgres queries.

use tracing::info;

use super::PgDb;
use crate::meta_optimizer::types::PromptVariant;

impl PgDb {
    /// Get the currently active prompt for a given agent type.
    pub async fn get_active_prompt(
        &self,
        agent_type: &str,
    ) -> Result<Option<PromptVariant>, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let row = conn
            .query_opt(
                r#"SELECT id, agent_type, variant_name, prompt_content, version,
                          is_active, source_recommendation_id, performance_metrics,
                          created_at::text, updated_at::text
                   FROM prompt_registry
                   WHERE agent_type = $1 AND is_active = true
                   LIMIT 1"#,
                &[&agent_type],
            )
            .await
            .map_err(|e| format!("Failed to get active prompt: {}", e))?;

        Ok(row.map(|r| PromptVariant {
            id: r.get(0),
            agent_type: r.get(1),
            variant_name: r.get(2),
            prompt_content: r.get(3),
            version: r.get(4),
            is_active: r.get(5),
            source_recommendation_id: r.get(6),
            performance_metrics: r.get(7),
            created_at: r.get(8),
            updated_at: r.get(9),
        }))
    }

    /// Get a prompt variant by agent_type and version number.
    pub async fn get_prompt_by_version(
        &self,
        agent_type: &str,
        version: i32,
    ) -> Result<Option<PromptVariant>, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let row = conn
            .query_opt(
                r#"SELECT id, agent_type, variant_name, prompt_content, version,
                          is_active, source_recommendation_id, performance_metrics,
                          created_at::text, updated_at::text
                   FROM prompt_registry
                   WHERE agent_type = $1 AND version = $2
                   LIMIT 1"#,
                &[&agent_type, &version],
            )
            .await
            .map_err(|e| format!("Failed to get prompt by version: {}", e))?;

        Ok(row.map(|r| PromptVariant {
            id: r.get(0),
            agent_type: r.get(1),
            variant_name: r.get(2),
            prompt_content: r.get(3),
            version: r.get(4),
            is_active: r.get(5),
            source_recommendation_id: r.get(6),
            performance_metrics: r.get(7),
            created_at: r.get(8),
            updated_at: r.get(9),
        }))
    }

    /// Create a new prompt variant (initially inactive).
    pub async fn create_prompt_variant(
        &self,
        agent_type: &str,
        variant_name: &str,
        prompt_content: &str,
        source_recommendation_id: Option<&str>,
    ) -> Result<PromptVariant, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let id = format!("pv-{}", uuid::Uuid::new_v4());
        let now = chrono::Utc::now().to_rfc3339();

        // Determine next version for this agent_type + variant_name
        let next_version: i32 = conn
            .query_one(
                "SELECT COALESCE(MAX(version), 0) + 1 FROM prompt_registry WHERE agent_type = $1 AND variant_name = $2",
                &[&agent_type, &variant_name],
            )
            .await
            .map(|row| row.get(0))
            .unwrap_or(1);

        conn.execute(
            r#"INSERT INTO prompt_registry
               (id, agent_type, variant_name, prompt_content, version, is_active,
                source_recommendation_id, performance_metrics, created_at, updated_at)
               VALUES ($1, $2, $3, $4, $5, false, $6, '{}', $7::timestamptz, $7::timestamptz)"#,
            &[
                &id,
                &agent_type,
                &variant_name,
                &prompt_content,
                &next_version,
                &source_recommendation_id,
                &now,
            ],
        )
        .await
        .map_err(|e| format!("Failed to create prompt variant: {}", e))?;

        info!(
            "Created prompt variant {} for agent {} (v{})",
            id, agent_type, next_version
        );

        Ok(PromptVariant {
            id,
            agent_type: agent_type.to_string(),
            variant_name: variant_name.to_string(),
            prompt_content: prompt_content.to_string(),
            version: next_version,
            is_active: false,
            source_recommendation_id: source_recommendation_id.map(|s| s.to_string()),
            performance_metrics: Some("{}".to_string()),
            created_at: now.clone(),
            updated_at: now,
        })
    }

    /// Activate a prompt variant (deactivating any previously active variant for that agent_type).
    pub async fn activate_variant(&self, id: &str) -> Result<(), String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let now = chrono::Utc::now().to_rfc3339();

        // Get the agent_type for this variant
        let agent_type_row = conn
            .query_one(
                "SELECT agent_type FROM prompt_registry WHERE id = $1",
                &[&id],
            )
            .await
            .map_err(|e| format!("Variant not found: {}", e))?;

        let agent_type: String = agent_type_row.get(0);

        // Deactivate all variants for this agent_type
        conn.execute(
            "UPDATE prompt_registry SET is_active = false, updated_at = $1::timestamptz WHERE agent_type = $2",
            &[&now, &agent_type],
        )
        .await
        .map_err(|e| format!("Failed to deactivate variants: {}", e))?;

        // Activate the requested variant
        conn.execute(
            "UPDATE prompt_registry SET is_active = true, updated_at = $1::timestamptz WHERE id = $2",
            &[&now, &id],
        )
        .await
        .map_err(|e| format!("Failed to activate variant: {}", e))?;

        info!("Activated prompt variant {} for agent {}", id, agent_type);
        Ok(())
    }

    /// List all prompt variants, optionally filtered by agent type.
    pub async fn list_variants(
        &self,
        agent_type: Option<&str>,
    ) -> Result<Vec<PromptVariant>, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = if let Some(at) = agent_type {
            conn.query(
                r#"SELECT id, agent_type, variant_name, prompt_content, version,
                          is_active, source_recommendation_id, performance_metrics,
                          created_at::text, updated_at::text
                   FROM prompt_registry WHERE agent_type = $1
                   ORDER BY agent_type, variant_name, version DESC"#,
                &[&at],
            )
            .await
        } else {
            conn.query(
                r#"SELECT id, agent_type, variant_name, prompt_content, version,
                          is_active, source_recommendation_id, performance_metrics,
                          created_at::text, updated_at::text
                   FROM prompt_registry
                   ORDER BY agent_type, variant_name, version DESC"#,
                &[],
            )
            .await
        }
        .map_err(|e| format!("Failed to query prompt variants: {}", e))?;

        let results = rows
            .iter()
            .map(|r| PromptVariant {
                id: r.get(0),
                agent_type: r.get(1),
                variant_name: r.get(2),
                prompt_content: r.get(3),
                version: r.get(4),
                is_active: r.get(5),
                source_recommendation_id: r.get(6),
                performance_metrics: r.get(7),
                created_at: r.get(8),
                updated_at: r.get(9),
            })
            .collect();

        Ok(results)
    }

    /// Update performance metrics for a prompt variant.
    pub async fn update_performance_metrics(
        &self,
        id: &str,
        metrics: &str,
    ) -> Result<(), String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let now = chrono::Utc::now().to_rfc3339();

        conn.execute(
            "UPDATE prompt_registry SET performance_metrics = $1, updated_at = $2::timestamptz WHERE id = $3",
            &[&metrics, &now, &id],
        )
        .await
        .map_err(|e| format!("Failed to update performance metrics: {}", e))?;

        Ok(())
    }
}

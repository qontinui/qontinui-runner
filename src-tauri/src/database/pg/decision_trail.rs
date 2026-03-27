//! PostgreSQL decision trail operations via Clorinde-generated queries.
//!
//! Persistent architectural decision history: typed, searchable decisions
//! and concept summaries with full-text search.

use super::PgDb;
use crate::database::types::*;

impl PgDb {
    // ========================================================================
    // Decisions
    // ========================================================================

    /// Save a new decision. Returns the decision ID.
    pub async fn save_decision(&self, input: &CreateDecisionInput) -> Result<String, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let id = uuid::Uuid::new_v4().to_string();
        let status = "active";
        let alts = input.alternatives_json.clone().unwrap_or_else(|| "[]".to_string());
        let tradeoffs = input.tradeoffs_json.clone().unwrap_or_else(|| "[]".to_string());
        let related = input.related_decisions_json.clone().unwrap_or_else(|| "[]".to_string());
        let files = input.affected_files_json.clone().unwrap_or_else(|| "[]".to_string());
        let endpoints = input.affected_endpoints_json.clone().unwrap_or_else(|| "[]".to_string());
        let tables = input.affected_tables_json.clone().unwrap_or_else(|| "[]".to_string());
        let tags = input.tags_json.clone().unwrap_or_else(|| "[]".to_string());
        let superseded_by: Option<String> = None;

        qontinui_db::queries::decisions::save_decision()
            .bind(
                &conn,
                &id.as_str(),
                &input.scale.as_str(),
                &input.category.as_str(),
                &status,
                &input.title.as_str(),
                &input.summary.as_str(),
                &input.rationale.as_str(),
                &alts.as_str(),
                &tradeoffs.as_str(),
                &input.triggered_by,
                &input.inspiration_json,
                &related.as_str(),
                &files.as_str(),
                &endpoints.as_str(),
                &tables.as_str(),
                &input.created_by,
                &superseded_by,
                &tags.as_str(),
            )
            .one()
            .await
            .map_err(|e| format!("PG save_decision: {}", e))?;

        Ok(id)
    }

    /// Get a single decision by ID (full content).
    pub async fn get_decision(&self, id: &str) -> Result<Option<Decision>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let row = qontinui_db::queries::decisions::get_decision()
            .bind(&conn, &id)
            .opt()
            .await
            .map_err(|e| format!("PG get_decision: {}", e))?;

        Ok(row.map(|r| Decision {
            id: r.id,
            timestamp: r.timestamp.to_rfc3339(),
            scale: r.scale,
            category: r.category,
            status: r.status,
            title: r.title,
            summary: r.summary,
            rationale: r.rationale,
            alternatives_json: r.alternatives_json,
            tradeoffs_json: r.tradeoffs_json,
            triggered_by: r.triggered_by,
            inspiration_json: r.inspiration_json,
            related_decisions_json: r.related_decisions_json,
            affected_files_json: r.affected_files_json,
            affected_endpoints_json: r.affected_endpoints_json,
            affected_tables_json: r.affected_tables_json,
            created_by: r.created_by,
            superseded_by: r.superseded_by,
            tags_json: r.tags_json,
            is_deleted: r.is_deleted,
            created_at: r.created_at.to_rfc3339(),
            updated_at: r.updated_at.to_rfc3339(),
        }))
    }

    /// List recent decisions (truncated previews).
    pub async fn list_decisions(&self, max_results: i64) -> Result<Vec<DecisionPreview>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let rows = qontinui_db::queries::decisions::list_decisions()
            .bind(&conn, &max_results)
            .all()
            .await
            .map_err(|e| format!("PG list_decisions: {}", e))?;

        Ok(rows.into_iter().map(|r| DecisionPreview {
            id: r.id,
            timestamp: r.timestamp.to_rfc3339(),
            scale: r.scale,
            category: r.category,
            status: r.status,
            title: r.title,
            summary_preview: r.summary_preview,
            triggered_by: r.triggered_by,
            tags_json: r.tags_json,
            created_by: r.created_by,
            created_at: r.created_at.to_rfc3339(),
            updated_at: r.updated_at.to_rfc3339(),
        }).collect())
    }

    /// List decisions filtered by category.
    pub async fn list_decisions_by_category(
        &self,
        category: &str,
        max_results: i64,
    ) -> Result<Vec<DecisionPreview>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let rows = qontinui_db::queries::decisions::list_decisions_by_category()
            .bind(&conn, &category, &max_results)
            .all()
            .await
            .map_err(|e| format!("PG list_decisions_by_category: {}", e))?;

        Ok(rows.into_iter().map(|r| DecisionPreview {
            id: r.id,
            timestamp: r.timestamp.to_rfc3339(),
            scale: r.scale,
            category: r.category,
            status: r.status,
            title: r.title,
            summary_preview: r.summary_preview,
            triggered_by: r.triggered_by,
            tags_json: r.tags_json,
            created_by: r.created_by,
            created_at: r.created_at.to_rfc3339(),
            updated_at: r.updated_at.to_rfc3339(),
        }).collect())
    }

    /// List decisions filtered by scale.
    pub async fn list_decisions_by_scale(
        &self,
        scale: &str,
        max_results: i64,
    ) -> Result<Vec<DecisionPreview>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let rows = qontinui_db::queries::decisions::list_decisions_by_scale()
            .bind(&conn, &scale, &max_results)
            .all()
            .await
            .map_err(|e| format!("PG list_decisions_by_scale: {}", e))?;

        Ok(rows.into_iter().map(|r| DecisionPreview {
            id: r.id,
            timestamp: r.timestamp.to_rfc3339(),
            scale: r.scale,
            category: r.category,
            status: r.status,
            title: r.title,
            summary_preview: r.summary_preview,
            triggered_by: r.triggered_by,
            tags_json: r.tags_json,
            created_by: r.created_by,
            created_at: r.created_at.to_rfc3339(),
            updated_at: r.updated_at.to_rfc3339(),
        }).collect())
    }

    /// Full-text search decisions.
    pub async fn search_decisions(
        &self,
        query: &str,
        max_results: i64,
    ) -> Result<Vec<DecisionSearchResult>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let rows = qontinui_db::queries::decisions::search_decisions()
            .bind(&conn, &query, &max_results)
            .all()
            .await
            .map_err(|e| format!("PG search_decisions: {}", e))?;

        Ok(rows.into_iter().map(|r| DecisionSearchResult {
            id: r.id,
            timestamp: r.timestamp.to_rfc3339(),
            scale: r.scale,
            category: r.category,
            status: r.status,
            title: r.title,
            summary_preview: r.summary_preview,
            triggered_by: r.triggered_by,
            tags_json: r.tags_json,
            created_by: r.created_by,
            created_at: r.created_at.to_rfc3339(),
            updated_at: r.updated_at.to_rfc3339(),
            rank: r.rank,
        }).collect())
    }

    /// Update decision status (active/superseded/reversed).
    pub async fn update_decision_status(
        &self,
        id: &str,
        status: &str,
    ) -> Result<bool, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let result = qontinui_db::queries::decisions::update_decision_status()
            .bind(&conn, &status, &id)
            .opt()
            .await
            .map_err(|e| format!("PG update_decision_status: {}", e))?;

        Ok(result.is_some())
    }

    /// Supersede a decision with a new one.
    pub async fn supersede_decision(
        &self,
        id: &str,
        superseded_by: &str,
    ) -> Result<bool, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let result = qontinui_db::queries::decisions::supersede_decision()
            .bind(&conn, &superseded_by, &id)
            .opt()
            .await
            .map_err(|e| format!("PG supersede_decision: {}", e))?;

        Ok(result.is_some())
    }

    /// Update decision fields.
    pub async fn update_decision(&self, input: &UpdateDecisionInput) -> Result<bool, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let result = qontinui_db::queries::decisions::update_decision()
            .bind(
                &conn,
                &input.title,
                &input.summary,
                &input.rationale,
                &input.alternatives_json,
                &input.tradeoffs_json,
                &input.tags_json,
                &input.triggered_by,
                &input.inspiration_json,
                &input.related_decisions_json,
                &input.affected_files_json,
                &input.affected_endpoints_json,
                &input.affected_tables_json,
                &input.id.as_str(),
            )
            .opt()
            .await
            .map_err(|e| format!("PG update_decision: {}", e))?;

        Ok(result.is_some())
    }

    /// Soft-delete a decision.
    pub async fn delete_decision(&self, id: &str) -> Result<bool, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let result = qontinui_db::queries::decisions::soft_delete_decision()
            .bind(&conn, &id)
            .opt()
            .await
            .map_err(|e| format!("PG soft_delete_decision: {}", e))?;

        Ok(result.is_some())
    }

    // ========================================================================
    // Concept Summaries
    // ========================================================================

    /// Save a new concept summary. Returns the ID.
    pub async fn save_concept_summary(
        &self,
        input: &CreateConceptSummaryInput,
    ) -> Result<String, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let id = uuid::Uuid::new_v4().to_string();
        let benefits = input.benefits_json.clone().unwrap_or_else(|| "[]".to_string());
        let components = input.components_json.clone().unwrap_or_else(|| "[]".to_string());
        let related = input.related_decisions_json.clone().unwrap_or_else(|| "[]".to_string());

        qontinui_db::queries::decisions::save_concept_summary()
            .bind(
                &conn,
                &id.as_str(),
                &input.name.as_str(),
                &input.tagline.as_str(),
                &input.description.as_str(),
                &input.inspiration_json,
                &benefits.as_str(),
                &components.as_str(),
                &related.as_str(),
                &input.metrics_json,
            )
            .one()
            .await
            .map_err(|e| format!("PG save_concept_summary: {}", e))?;

        Ok(id)
    }

    /// Get a single concept summary by ID.
    pub async fn get_concept_summary(
        &self,
        id: &str,
    ) -> Result<Option<ConceptSummaryRecord>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let row = qontinui_db::queries::decisions::get_concept_summary()
            .bind(&conn, &id)
            .opt()
            .await
            .map_err(|e| format!("PG get_concept_summary: {}", e))?;

        Ok(row.map(|r| ConceptSummaryRecord {
            id: r.id,
            name: r.name,
            tagline: r.tagline,
            description: r.description,
            inspiration_json: r.inspiration_json,
            benefits_json: r.benefits_json,
            components_json: r.components_json,
            related_decisions_json: r.related_decisions_json,
            metrics_json: r.metrics_json,
            is_deleted: r.is_deleted,
            created_at: r.created_at.to_rfc3339(),
            updated_at: r.updated_at.to_rfc3339(),
        }))
    }

    /// List concept summaries.
    pub async fn list_concept_summaries(
        &self,
        max_results: i64,
    ) -> Result<Vec<ConceptSummaryPreview>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let rows = qontinui_db::queries::decisions::list_concept_summaries()
            .bind(&conn, &max_results)
            .all()
            .await
            .map_err(|e| format!("PG list_concept_summaries: {}", e))?;

        Ok(rows.into_iter().map(|r| ConceptSummaryPreview {
            id: r.id,
            name: r.name,
            tagline: r.tagline,
            description_preview: r.description_preview,
            inspiration_json: r.inspiration_json,
            benefits_json: r.benefits_json,
            components_json: r.components_json,
            related_decisions_json: r.related_decisions_json,
            metrics_json: r.metrics_json,
            created_at: r.created_at.to_rfc3339(),
            updated_at: r.updated_at.to_rfc3339(),
        }).collect())
    }

    /// Full-text search concept summaries.
    pub async fn search_concept_summaries(
        &self,
        query: &str,
        max_results: i64,
    ) -> Result<Vec<ConceptSummaryPreview>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let rows = qontinui_db::queries::decisions::search_concept_summaries()
            .bind(&conn, &query, &max_results)
            .all()
            .await
            .map_err(|e| format!("PG search_concept_summaries: {}", e))?;

        Ok(rows.into_iter().map(|r| ConceptSummaryPreview {
            id: r.id,
            name: r.name,
            tagline: r.tagline,
            description_preview: r.description_preview,
            inspiration_json: r.inspiration_json,
            benefits_json: r.benefits_json,
            components_json: r.components_json,
            related_decisions_json: r.related_decisions_json,
            metrics_json: r.metrics_json,
            created_at: r.created_at.to_rfc3339(),
            updated_at: r.updated_at.to_rfc3339(),
        }).collect())
    }

    /// Update concept summary fields.
    pub async fn update_concept_summary(
        &self,
        input: &UpdateConceptSummaryInput,
    ) -> Result<bool, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let result = qontinui_db::queries::decisions::update_concept_summary()
            .bind(
                &conn,
                &input.name,
                &input.tagline,
                &input.description,
                &input.inspiration_json,
                &input.benefits_json,
                &input.components_json,
                &input.related_decisions_json,
                &input.metrics_json,
                &input.id.as_str(),
            )
            .opt()
            .await
            .map_err(|e| format!("PG update_concept_summary: {}", e))?;

        Ok(result.is_some())
    }

    /// Soft-delete a concept summary.
    pub async fn delete_concept_summary(&self, id: &str) -> Result<bool, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let result = qontinui_db::queries::decisions::soft_delete_concept_summary()
            .bind(&conn, &id)
            .opt()
            .await
            .map_err(|e| format!("PG soft_delete_concept_summary: {}", e))?;

        Ok(result.is_some())
    }
}

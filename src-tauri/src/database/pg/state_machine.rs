//! PostgreSQL state_machine_configs CRUD operations (raw SQL).

use super::PgDb;
use crate::state_machine_configs::types::*;

impl PgDb {
    // ========================================================================
    // State Machine Configs
    // ========================================================================

    /// List all state machine configs, ordered by updated_at DESC.
    pub async fn list_sm_configs(&self) -> Result<Vec<SmConfig>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                r#"
                SELECT id, name, description, render_count, element_count, include_html_ids,
                       created_at::TEXT, updated_at::TEXT
                FROM state_machine_configs
                ORDER BY updated_at DESC
                "#,
                &[],
            )
            .await
            .map_err(|e| format!("PG list_sm_configs: {}", e))?;

        Ok(rows.iter().map(|r| Self::sm_row_to_config(r)).collect())
    }

    /// Get a single state machine config by ID.
    pub async fn get_sm_config(&self, id: &str) -> Result<Option<SmConfig>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let row = conn
            .query_opt(
                r#"
                SELECT id, name, description, render_count, element_count, include_html_ids,
                       created_at::TEXT, updated_at::TEXT
                FROM state_machine_configs
                WHERE id = $1
                "#,
                &[&id],
            )
            .await
            .map_err(|e| format!("PG get_sm_config: {}", e))?;

        Ok(row.map(|r| Self::sm_row_to_config(&r)))
    }

    /// Insert a new state machine config. Returns the created config.
    pub async fn insert_sm_config(&self, req: &CreateSmConfigRequest) -> Result<SmConfig, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let id = uuid::Uuid::new_v4().to_string();
        let description = req.description.as_deref().unwrap_or("");

        conn.execute(
            r#"
            INSERT INTO state_machine_configs (id, name, description, render_count, element_count, include_html_ids, created_at, updated_at)
            VALUES ($1, $2, $3, 0, 0, FALSE, NOW(), NOW())
            "#,
            &[&id, &req.name, &description],
        )
        .await
        .map_err(|e| format!("PG insert_sm_config: {}", e))?;

        self.get_sm_config(&id)
            .await?
            .ok_or_else(|| "Config not found after insert".to_string())
    }

    /// Update an existing state machine config.
    pub async fn update_sm_config(
        &self,
        id: &str,
        req: &UpdateSmConfigRequest,
    ) -> Result<SmConfig, String> {
        let existing = self
            .get_sm_config(id)
            .await?
            .ok_or_else(|| format!("Config not found: {}", id))?;

        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let name = req.name.as_deref().unwrap_or(&existing.name).to_string();
        let description = req
            .description
            .as_deref()
            .or(existing.description.as_deref())
            .map(|s| s.to_string());
        let render_count = req.render_count.unwrap_or(existing.render_count) as i32;
        let element_count = req.element_count.unwrap_or(existing.element_count) as i32;
        let include_html_ids = req.include_html_ids.unwrap_or(existing.include_html_ids);

        conn.execute(
            r#"
            UPDATE state_machine_configs
            SET name = $1, description = $2, render_count = $3, element_count = $4,
                include_html_ids = $5, updated_at = NOW()
            WHERE id = $6
            "#,
            &[
                &name as &(dyn tokio_postgres::types::ToSql + Sync),
                &description as &(dyn tokio_postgres::types::ToSql + Sync),
                &render_count as &(dyn tokio_postgres::types::ToSql + Sync),
                &element_count as &(dyn tokio_postgres::types::ToSql + Sync),
                &include_html_ids as &(dyn tokio_postgres::types::ToSql + Sync),
                &id as &(dyn tokio_postgres::types::ToSql + Sync),
            ],
        )
        .await
        .map_err(|e| format!("PG update_sm_config: {}", e))?;

        self.get_sm_config(id)
            .await?
            .ok_or_else(|| "Config not found after update".to_string())
    }

    /// Delete a state machine config by ID (cascades to states and transitions).
    pub async fn delete_sm_config(&self, id: &str) -> Result<bool, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let affected = conn
            .execute("DELETE FROM state_machine_configs WHERE id = $1", &[&id])
            .await
            .map_err(|e| format!("PG delete_sm_config: {}", e))?;

        Ok(affected > 0)
    }

    // ========================================================================
    // Get Config Full (with states + transitions)
    // ========================================================================

    /// Get a full state machine config with its states and transitions.
    pub async fn get_sm_config_full(&self, id: &str) -> Result<Option<SmConfigFull>, String> {
        let config = match self.get_sm_config(id).await? {
            Some(c) => c,
            None => return Ok(None),
        };

        let states = self.list_sm_states(id).await?;
        let transitions = self.list_sm_transitions(id).await?;

        Ok(Some(SmConfigFull {
            config,
            states,
            transitions,
        }))
    }

    // ========================================================================
    // State CRUD
    // ========================================================================

    /// List all states for a config.
    pub async fn list_sm_states(&self, config_id: &str) -> Result<Vec<SmState>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                r#"
                SELECT id, config_id, state_id, name, description, element_ids, render_ids,
                       confidence, acceptance_criteria, extra_metadata, domain_knowledge,
                       created_at::TEXT, updated_at::TEXT
                FROM state_machine_states
                WHERE config_id = $1
                ORDER BY name
                "#,
                &[&config_id],
            )
            .await
            .map_err(|e| format!("PG list_sm_states: {}", e))?;

        Ok(rows.iter().map(|r| Self::sm_row_to_state(r)).collect())
    }

    /// Get a single state by ID.
    pub async fn get_sm_state(&self, id: &str) -> Result<Option<SmState>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let row = conn
            .query_opt(
                r#"
                SELECT id, config_id, state_id, name, description, element_ids, render_ids,
                       confidence, acceptance_criteria, extra_metadata, domain_knowledge,
                       created_at::TEXT, updated_at::TEXT
                FROM state_machine_states WHERE id = $1
                "#,
                &[&id],
            )
            .await
            .map_err(|e| format!("PG get_sm_state: {}", e))?;

        Ok(row.map(|r| Self::sm_row_to_state(&r)))
    }

    /// Insert a new state.
    pub async fn insert_sm_state(
        &self,
        config_id: &str,
        req: &CreateSmStateRequest,
    ) -> Result<SmState, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let id = uuid::Uuid::new_v4().to_string();
        let state_id = req
            .state_id
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let element_ids = serde_json::to_string(req.element_ids.as_deref().unwrap_or(&[]))
            .map_err(|e| format!("Failed to serialize element_ids: {}", e))?;
        let render_ids = serde_json::to_string(req.render_ids.as_deref().unwrap_or(&[]))
            .map_err(|e| format!("Failed to serialize render_ids: {}", e))?;
        let acceptance_criteria =
            serde_json::to_string(req.acceptance_criteria.as_deref().unwrap_or(&[]))
                .map_err(|e| format!("Failed to serialize acceptance_criteria: {}", e))?;
        let extra_metadata = serde_json::to_string(
            req.extra_metadata
                .as_ref()
                .unwrap_or(&serde_json::json!({})),
        )
        .map_err(|e| format!("Failed to serialize extra_metadata: {}", e))?;
        let domain_knowledge =
            serde_json::to_string(req.domain_knowledge.as_deref().unwrap_or(&[]))
                .map_err(|e| format!("Failed to serialize domain_knowledge: {}", e))?;
        let confidence = req.confidence.unwrap_or(0.9);
        let description = req.description.as_deref().unwrap_or("");

        conn.execute(
            r#"
            INSERT INTO state_machine_states
            (id, config_id, state_id, name, description, element_ids, render_ids,
             confidence, acceptance_criteria, extra_metadata, domain_knowledge,
             created_at, updated_at)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, NOW(), NOW())
            "#,
            &[
                &id as &(dyn tokio_postgres::types::ToSql + Sync),
                &config_id as &(dyn tokio_postgres::types::ToSql + Sync),
                &state_id as &(dyn tokio_postgres::types::ToSql + Sync),
                &req.name as &(dyn tokio_postgres::types::ToSql + Sync),
                &description as &(dyn tokio_postgres::types::ToSql + Sync),
                &element_ids as &(dyn tokio_postgres::types::ToSql + Sync),
                &render_ids as &(dyn tokio_postgres::types::ToSql + Sync),
                &confidence as &(dyn tokio_postgres::types::ToSql + Sync),
                &acceptance_criteria as &(dyn tokio_postgres::types::ToSql + Sync),
                &extra_metadata as &(dyn tokio_postgres::types::ToSql + Sync),
                &domain_knowledge as &(dyn tokio_postgres::types::ToSql + Sync),
            ],
        )
        .await
        .map_err(|e| format!("PG insert_sm_state: {}", e))?;

        self.get_sm_state(&id)
            .await?
            .ok_or_else(|| "State not found after insert".to_string())
    }

    /// Update an existing state.
    pub async fn update_sm_state(
        &self,
        id: &str,
        req: &UpdateSmStateRequest,
    ) -> Result<SmState, String> {
        let existing = self
            .get_sm_state(id)
            .await?
            .ok_or_else(|| format!("State not found: {}", id))?;

        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let name = req.name.as_deref().unwrap_or(&existing.name).to_string();
        let description = req
            .description
            .as_deref()
            .or(existing.description.as_deref())
            .map(|s| s.to_string());
        let element_ids =
            serde_json::to_string(req.element_ids.as_ref().unwrap_or(&existing.element_ids))
                .map_err(|e| format!("Failed to serialize element_ids: {}", e))?;
        let render_ids =
            serde_json::to_string(req.render_ids.as_ref().unwrap_or(&existing.render_ids))
                .map_err(|e| format!("Failed to serialize render_ids: {}", e))?;
        let confidence = req.confidence.unwrap_or(existing.confidence);
        let acceptance_criteria = serde_json::to_string(
            req.acceptance_criteria
                .as_ref()
                .unwrap_or(&existing.acceptance_criteria),
        )
        .map_err(|e| format!("Failed to serialize acceptance_criteria: {}", e))?;
        let extra_metadata = serde_json::to_string(
            req.extra_metadata
                .as_ref()
                .unwrap_or(&existing.extra_metadata),
        )
        .map_err(|e| format!("Failed to serialize extra_metadata: {}", e))?;
        let domain_knowledge = serde_json::to_string(
            req.domain_knowledge
                .as_ref()
                .unwrap_or(&existing.domain_knowledge),
        )
        .map_err(|e| format!("Failed to serialize domain_knowledge: {}", e))?;

        conn.execute(
            r#"
            UPDATE state_machine_states
            SET name = $1, description = $2, element_ids = $3, render_ids = $4,
                confidence = $5, acceptance_criteria = $6, extra_metadata = $7,
                domain_knowledge = $8, updated_at = NOW()
            WHERE id = $9
            "#,
            &[
                &name as &(dyn tokio_postgres::types::ToSql + Sync),
                &description as &(dyn tokio_postgres::types::ToSql + Sync),
                &element_ids as &(dyn tokio_postgres::types::ToSql + Sync),
                &render_ids as &(dyn tokio_postgres::types::ToSql + Sync),
                &confidence as &(dyn tokio_postgres::types::ToSql + Sync),
                &acceptance_criteria as &(dyn tokio_postgres::types::ToSql + Sync),
                &extra_metadata as &(dyn tokio_postgres::types::ToSql + Sync),
                &domain_knowledge as &(dyn tokio_postgres::types::ToSql + Sync),
                &id as &(dyn tokio_postgres::types::ToSql + Sync),
            ],
        )
        .await
        .map_err(|e| format!("PG update_sm_state: {}", e))?;

        self.get_sm_state(id)
            .await?
            .ok_or_else(|| "State not found after update".to_string())
    }

    /// Delete a state by ID.
    pub async fn delete_sm_state(&self, id: &str) -> Result<bool, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let affected = conn
            .execute("DELETE FROM state_machine_states WHERE id = $1", &[&id])
            .await
            .map_err(|e| format!("PG delete_sm_state: {}", e))?;

        Ok(affected > 0)
    }

    // ========================================================================
    // Transition CRUD
    // ========================================================================

    /// List all transitions for a config.
    pub async fn list_sm_transitions(&self, config_id: &str) -> Result<Vec<SmTransition>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                r#"
                SELECT id, config_id, transition_id, name, from_states, activate_states,
                       exit_states, actions, path_cost, stays_visible, extra_metadata,
                       created_at::TEXT, updated_at::TEXT
                FROM state_machine_transitions
                WHERE config_id = $1
                ORDER BY name
                "#,
                &[&config_id],
            )
            .await
            .map_err(|e| format!("PG list_sm_transitions: {}", e))?;

        Ok(rows.iter().map(|r| Self::sm_row_to_transition(r)).collect())
    }

    /// Get a single transition by ID.
    pub async fn get_sm_transition(&self, id: &str) -> Result<Option<SmTransition>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let row = conn
            .query_opt(
                r#"
                SELECT id, config_id, transition_id, name, from_states, activate_states,
                       exit_states, actions, path_cost, stays_visible, extra_metadata,
                       created_at::TEXT, updated_at::TEXT
                FROM state_machine_transitions WHERE id = $1
                "#,
                &[&id],
            )
            .await
            .map_err(|e| format!("PG get_sm_transition: {}", e))?;

        Ok(row.map(|r| Self::sm_row_to_transition(&r)))
    }

    /// Insert a new transition.
    pub async fn insert_sm_transition(
        &self,
        config_id: &str,
        req: &CreateSmTransitionRequest,
    ) -> Result<SmTransition, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let id = uuid::Uuid::new_v4().to_string();
        let transition_id = req
            .transition_id
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let from_states = serde_json::to_string(&req.from_states)
            .map_err(|e| format!("Failed to serialize from_states: {}", e))?;
        let activate_states = serde_json::to_string(&req.activate_states)
            .map_err(|e| format!("Failed to serialize activate_states: {}", e))?;
        let exit_states = serde_json::to_string(&req.exit_states)
            .map_err(|e| format!("Failed to serialize exit_states: {}", e))?;
        let actions = serde_json::to_string(&req.actions)
            .map_err(|e| format!("Failed to serialize actions: {}", e))?;
        let extra_metadata = serde_json::to_string(
            req.extra_metadata
                .as_ref()
                .unwrap_or(&serde_json::json!({})),
        )
        .map_err(|e| format!("Failed to serialize extra_metadata: {}", e))?;
        let path_cost = req.path_cost.unwrap_or(1.0);
        let stays_visible = req.stays_visible.unwrap_or(false);

        conn.execute(
            r#"
            INSERT INTO state_machine_transitions
            (id, config_id, transition_id, name, from_states, activate_states, exit_states,
             actions, path_cost, stays_visible, extra_metadata, created_at, updated_at)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, NOW(), NOW())
            "#,
            &[
                &id as &(dyn tokio_postgres::types::ToSql + Sync),
                &config_id as &(dyn tokio_postgres::types::ToSql + Sync),
                &transition_id as &(dyn tokio_postgres::types::ToSql + Sync),
                &req.name as &(dyn tokio_postgres::types::ToSql + Sync),
                &from_states as &(dyn tokio_postgres::types::ToSql + Sync),
                &activate_states as &(dyn tokio_postgres::types::ToSql + Sync),
                &exit_states as &(dyn tokio_postgres::types::ToSql + Sync),
                &actions as &(dyn tokio_postgres::types::ToSql + Sync),
                &path_cost as &(dyn tokio_postgres::types::ToSql + Sync),
                &stays_visible as &(dyn tokio_postgres::types::ToSql + Sync),
                &extra_metadata as &(dyn tokio_postgres::types::ToSql + Sync),
            ],
        )
        .await
        .map_err(|e| format!("PG insert_sm_transition: {}", e))?;

        self.get_sm_transition(&id)
            .await?
            .ok_or_else(|| "Transition not found after insert".to_string())
    }

    /// Update an existing transition.
    pub async fn update_sm_transition(
        &self,
        id: &str,
        req: &UpdateSmTransitionRequest,
    ) -> Result<SmTransition, String> {
        let existing = self
            .get_sm_transition(id)
            .await?
            .ok_or_else(|| format!("Transition not found: {}", id))?;

        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let name = req.name.as_deref().unwrap_or(&existing.name).to_string();
        let from_states =
            serde_json::to_string(req.from_states.as_ref().unwrap_or(&existing.from_states))
                .map_err(|e| format!("Failed to serialize from_states: {}", e))?;
        let activate_states = serde_json::to_string(
            req.activate_states
                .as_ref()
                .unwrap_or(&existing.activate_states),
        )
        .map_err(|e| format!("Failed to serialize activate_states: {}", e))?;
        let exit_states =
            serde_json::to_string(req.exit_states.as_ref().unwrap_or(&existing.exit_states))
                .map_err(|e| format!("Failed to serialize exit_states: {}", e))?;
        let actions = serde_json::to_string(req.actions.as_ref().unwrap_or(&existing.actions))
            .map_err(|e| format!("Failed to serialize actions: {}", e))?;
        let path_cost = req.path_cost.unwrap_or(existing.path_cost);
        let stays_visible = req.stays_visible.unwrap_or(existing.stays_visible);
        let extra_metadata = serde_json::to_string(
            req.extra_metadata
                .as_ref()
                .unwrap_or(&existing.extra_metadata),
        )
        .map_err(|e| format!("Failed to serialize extra_metadata: {}", e))?;

        conn.execute(
            r#"
            UPDATE state_machine_transitions
            SET name = $1, from_states = $2, activate_states = $3, exit_states = $4,
                actions = $5, path_cost = $6, stays_visible = $7, extra_metadata = $8,
                updated_at = NOW()
            WHERE id = $9
            "#,
            &[
                &name as &(dyn tokio_postgres::types::ToSql + Sync),
                &from_states as &(dyn tokio_postgres::types::ToSql + Sync),
                &activate_states as &(dyn tokio_postgres::types::ToSql + Sync),
                &exit_states as &(dyn tokio_postgres::types::ToSql + Sync),
                &actions as &(dyn tokio_postgres::types::ToSql + Sync),
                &path_cost as &(dyn tokio_postgres::types::ToSql + Sync),
                &stays_visible as &(dyn tokio_postgres::types::ToSql + Sync),
                &extra_metadata as &(dyn tokio_postgres::types::ToSql + Sync),
                &id as &(dyn tokio_postgres::types::ToSql + Sync),
            ],
        )
        .await
        .map_err(|e| format!("PG update_sm_transition: {}", e))?;

        self.get_sm_transition(id)
            .await?
            .ok_or_else(|| "Transition not found after update".to_string())
    }

    /// Delete a transition by ID.
    pub async fn delete_sm_transition(&self, id: &str) -> Result<bool, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let affected = conn
            .execute(
                "DELETE FROM state_machine_transitions WHERE id = $1",
                &[&id],
            )
            .await
            .map_err(|e| format!("PG delete_sm_transition: {}", e))?;

        Ok(affected > 0)
    }

    // ========================================================================
    // Import
    // ========================================================================

    /// Import a state machine config from the export format (transactional).
    pub async fn import_sm_config(&self, req: &SmImportRequest) -> Result<SmConfigFull, String> {
        let data = &req.config;

        let config_data = data
            .get("config")
            .ok_or("Missing 'config' field in import data")?;
        let config_name = req
            .name
            .as_deref()
            .or_else(|| config_data.get("name").and_then(|v| v.as_str()))
            .unwrap_or("Imported Config");

        let create_req = CreateSmConfigRequest {
            name: config_name.to_string(),
            description: config_data
                .get("description")
                .and_then(|v| v.as_str())
                .map(String::from),
        };

        let mut config = self.insert_sm_config(&create_req).await?;

        // Update render/element counts from import
        let render_count = config_data
            .get("render_count")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        let element_count = config_data
            .get("element_count")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        let include_html_ids = config_data
            .get("include_html_ids")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        config = self
            .update_sm_config(
                &config.id,
                &UpdateSmConfigRequest {
                    name: None,
                    description: None,
                    render_count: Some(render_count),
                    element_count: Some(element_count),
                    include_html_ids: Some(include_html_ids),
                },
            )
            .await?;

        // Import states
        let mut states = Vec::new();
        if let Some(states_obj) = data.get("states").and_then(|v| v.as_object()) {
            for (state_id, state_data) in states_obj {
                let element_ids: Vec<String> = state_data
                    .get("element_ids")
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
                    .unwrap_or_default();
                let render_ids: Vec<String> = state_data
                    .get("render_ids")
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
                    .unwrap_or_default();
                let acceptance_criteria: Vec<String> = state_data
                    .get("acceptance_criteria")
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
                    .unwrap_or_default();

                let req = CreateSmStateRequest {
                    state_id: Some(state_id.clone()),
                    name: state_data
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or(state_id)
                        .to_string(),
                    description: state_data
                        .get("description")
                        .and_then(|v| v.as_str())
                        .map(String::from),
                    element_ids: Some(element_ids),
                    render_ids: Some(render_ids),
                    confidence: state_data.get("confidence").and_then(|v| v.as_f64()),
                    acceptance_criteria: Some(acceptance_criteria),
                    extra_metadata: state_data.get("extra_metadata").cloned(),
                    domain_knowledge: None,
                };

                states.push(self.insert_sm_state(&config.id, &req).await?);
            }
        }

        // Import transitions
        let mut transitions = Vec::new();
        if let Some(transitions_obj) = data.get("transitions").and_then(|v| v.as_object()) {
            for (_transition_id, t_data) in transitions_obj {
                let from_states: Vec<String> = t_data
                    .get("from_states")
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
                    .unwrap_or_default();
                let activate_states: Vec<String> = t_data
                    .get("activate_states")
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
                    .unwrap_or_default();
                let exit_states: Vec<String> = t_data
                    .get("exit_states")
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
                    .unwrap_or_default();
                let actions: Vec<serde_json::Value> = t_data
                    .get("actions")
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
                    .unwrap_or_default();

                let req = CreateSmTransitionRequest {
                    transition_id: t_data
                        .get("transition_id")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()),
                    name: t_data
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("Unnamed")
                        .to_string(),
                    from_states,
                    activate_states,
                    exit_states,
                    actions,
                    path_cost: t_data.get("path_cost").and_then(|v| v.as_f64()),
                    stays_visible: t_data.get("stays_visible").and_then(|v| v.as_bool()),
                    extra_metadata: t_data.get("extra_metadata").cloned(),
                };

                transitions.push(self.insert_sm_transition(&config.id, &req).await?);
            }
        }

        tracing::info!(
            "Imported state machine config '{}': {} states, {} transitions",
            config.name,
            states.len(),
            transitions.len()
        );

        Ok(SmConfigFull {
            config,
            states,
            transitions,
        })
    }

    // ========================================================================
    // Thumbnails
    // ========================================================================

    /// Save thumbnails for a config.
    pub async fn save_sm_thumbnails(
        &self,
        config_id: &str,
        thumbnails: &std::collections::HashMap<String, String>,
    ) -> Result<usize, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let mut count = 0;
        for (hash, data) in thumbnails {
            conn.execute(
                r#"INSERT INTO sm_element_thumbnails (config_id, fingerprint_hash, thumbnail_base64)
                   VALUES ($1, $2, $3)
                   ON CONFLICT (config_id, fingerprint_hash)
                   DO UPDATE SET thumbnail_base64 = EXCLUDED.thumbnail_base64"#,
                &[&config_id, hash, data],
            )
            .await
            .map_err(|e| format!("PG save thumbnail: {}", e))?;
            count += 1;
        }
        tracing::info!("Saved {} thumbnails for config {}", count, config_id);
        Ok(count)
    }

    /// Get thumbnails for a config.
    pub async fn get_sm_thumbnails(
        &self,
        config_id: &str,
    ) -> Result<std::collections::HashMap<String, String>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let rows = conn
            .query(
                "SELECT fingerprint_hash, thumbnail_base64 FROM sm_element_thumbnails WHERE config_id = $1",
                &[&config_id],
            )
            .await
            .map_err(|e| format!("PG get thumbnails: {}", e))?;

        let mut result = std::collections::HashMap::new();
        for row in &rows {
            let hash: String = row.get(0);
            let data: String = row.get(1);
            result.insert(hash, data);
        }
        tracing::info!(
            "Loaded {} thumbnails for config {}",
            result.len(),
            config_id
        );
        Ok(result)
    }

    // ========================================================================
    // Capture Screenshots
    // ========================================================================

    /// Save capture screenshots (with PNG to WebP conversion done by caller).
    pub async fn save_capture_screenshots(
        &self,
        config_id: &str,
        screenshots: &[SmCaptureScreenshotSave],
    ) -> Result<usize, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let mut count = 0;
        for ss in screenshots {
            let id = uuid::Uuid::new_v4().to_string();

            // Decode base64 and convert PNG to WebP for size reduction
            let png_bytes = base64::Engine::decode(
                &base64::engine::general_purpose::STANDARD,
                &ss.screenshot_base64,
            )
            .map_err(|e| format!("Failed to decode screenshot base64: {}", e))?;

            // Decode the PNG so we can authoritatively know the captured
            // dimensions. Frontend callers historically multiplied
            // ssJson.data.width by the device-pixel-ratio twice, producing
            // metadata that was 1.5x the actual stored bytes on HiDPI
            // displays. We always trust the decoded image and log a warning
            // if the caller's claim disagrees.
            let (decoded_w, decoded_h, webp_bytes) = match image::load_from_memory(&png_bytes) {
                Ok(img) => {
                    let dw = img.width() as i32;
                    let dh = img.height() as i32;
                    let mut buf = std::io::Cursor::new(Vec::new());
                    let bytes = match img.write_to(&mut buf, image::ImageFormat::WebP) {
                        Ok(()) => buf.into_inner(),
                        Err(_) => png_bytes.clone(),
                    };
                    (dw, dh, bytes)
                }
                Err(e) => {
                    tracing::warn!(
                        "save_capture_screenshots: failed to decode PNG ({}); falling back to client dims {}x{} for config {}",
                        e,
                        ss.width,
                        ss.height,
                        config_id
                    );
                    (ss.width as i32, ss.height as i32, png_bytes)
                }
            };

            if ss.width as i32 != decoded_w || ss.height as i32 != decoded_h {
                tracing::warn!(
                    "save_capture_screenshots: client-supplied dims {}x{} disagree with decoded {}x{} for config {} (storing decoded dims)",
                    ss.width,
                    ss.height,
                    decoded_w,
                    decoded_h,
                    config_id
                );
            }

            let capture_index = ss.capture_index as i32;
            let width = decoded_w;
            let height = decoded_h;

            conn.execute(
                r#"INSERT INTO sm_capture_screenshots
                   (id, config_id, capture_index, screenshot_webp, width, height,
                    element_bounds_json, fingerprint_hashes_json, captured_at)
                   VALUES ($1, $2, $3, $4, $5, $6, $7, $8, NOW())"#,
                &[
                    &id as &(dyn tokio_postgres::types::ToSql + Sync),
                    &config_id as &(dyn tokio_postgres::types::ToSql + Sync),
                    &capture_index as &(dyn tokio_postgres::types::ToSql + Sync),
                    &webp_bytes as &(dyn tokio_postgres::types::ToSql + Sync),
                    &width as &(dyn tokio_postgres::types::ToSql + Sync),
                    &height as &(dyn tokio_postgres::types::ToSql + Sync),
                    &ss.element_bounds_json as &(dyn tokio_postgres::types::ToSql + Sync),
                    &ss.fingerprint_hashes_json as &(dyn tokio_postgres::types::ToSql + Sync),
                ],
            )
            .await
            .map_err(|e| format!("PG save capture screenshot: {}", e))?;
            count += 1;
        }
        tracing::info!(
            "Saved {} capture screenshots for config {}",
            count,
            config_id
        );
        Ok(count)
    }

    /// One-shot backfill: walk every row in `sm_capture_screenshots`, decode
    /// the stored WebP, and rewrite `width`/`height` from the actual image
    /// dimensions. Pre-fix rows on HiDPI displays were stored at 1.5x the
    /// actual bytes due to a frontend double-multiply by device-pixel-ratio.
    ///
    /// Returns `(scanned, rewritten)`. Safe to run multiple times — rows that
    /// already match are left untouched.
    pub async fn backfill_capture_screenshot_dimensions(&self) -> Result<(usize, usize), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                "SELECT id, screenshot_webp, width, height FROM sm_capture_screenshots",
                &[],
            )
            .await
            .map_err(|e| format!("PG backfill select: {}", e))?;

        let mut scanned = 0usize;
        let mut rewritten = 0usize;

        for row in &rows {
            scanned += 1;
            let id: String = row.get(0);
            let bytes: Vec<u8> = row.get(1);
            let old_w: i32 = row.get(2);
            let old_h: i32 = row.get(3);

            let img = match image::load_from_memory(&bytes) {
                Ok(img) => img,
                Err(e) => {
                    tracing::warn!("backfill: row {} failed to decode: {}", id, e);
                    continue;
                }
            };
            let new_w = img.width() as i32;
            let new_h = img.height() as i32;

            if new_w == old_w && new_h == old_h {
                continue;
            }

            conn.execute(
                "UPDATE sm_capture_screenshots SET width = $1, height = $2 WHERE id = $3",
                &[&new_w, &new_h, &id],
            )
            .await
            .map_err(|e| format!("PG backfill update {}: {}", id, e))?;

            rewritten += 1;
            tracing::info!(
                "backfill_capture_screenshot_dimensions: row {} {}x{} -> {}x{}",
                id,
                old_w,
                old_h,
                new_w,
                new_h
            );
        }

        tracing::info!(
            "backfill_capture_screenshot_dimensions: scanned={} rewritten={}",
            scanned,
            rewritten
        );
        Ok((scanned, rewritten))
    }

    /// Get capture screenshot metadata (without image data).
    pub async fn get_capture_screenshots(
        &self,
        config_id: &str,
    ) -> Result<Vec<SmCaptureScreenshotMeta>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let rows = conn
            .query(
                r#"SELECT id, config_id, capture_index, width, height,
                          element_bounds_json, fingerprint_hashes_json, captured_at::TEXT
                   FROM sm_capture_screenshots
                   WHERE config_id = $1
                   ORDER BY capture_index"#,
                &[&config_id],
            )
            .await
            .map_err(|e| format!("PG get capture screenshots: {}", e))?;

        let result: Vec<SmCaptureScreenshotMeta> = rows
            .iter()
            .map(|row| SmCaptureScreenshotMeta {
                id: row.get(0),
                config_id: row.get(1),
                capture_index: row.get::<_, i32>(2) as i64,
                width: row.get::<_, i32>(3) as i64,
                height: row.get::<_, i32>(4) as i64,
                element_bounds_json: row.get(5),
                fingerprint_hashes_json: row.get(6),
                captured_at: row.get(7),
            })
            .collect();

        tracing::info!(
            "Loaded {} capture screenshot metadata for config {}",
            result.len(),
            config_id
        );
        Ok(result)
    }

    /// Get a single capture screenshot image as base64 WebP data URI.
    pub async fn get_capture_screenshot_image(
        &self,
        screenshot_id: &str,
    ) -> Result<String, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let row = conn
            .query_one(
                "SELECT screenshot_webp FROM sm_capture_screenshots WHERE id = $1",
                &[&screenshot_id],
            )
            .await
            .map_err(|e| format!("PG get screenshot image: {}", e))?;

        let webp_bytes: Vec<u8> = row.get(0);
        let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &webp_bytes);
        Ok(format!("data:image/webp;base64,{}", b64))
    }

    /// Move pending capture screenshots from one config_id to another.
    pub async fn move_pending_screenshots(
        &self,
        from_config_id: &str,
        to_config_id: &str,
    ) -> Result<usize, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let affected = conn
            .execute(
                "UPDATE sm_capture_screenshots SET config_id = $1 WHERE config_id = $2",
                &[&to_config_id, &from_config_id],
            )
            .await
            .map_err(|e| format!("PG move screenshots: {}", e))?;

        if affected > 0 {
            tracing::info!(
                "Moved {} capture screenshots from '{}' to config '{}'",
                affected,
                from_config_id,
                to_config_id
            );
        }
        Ok(affected as usize)
    }

    /// Delete capture screenshots for a config.
    pub async fn delete_capture_screenshots(&self, config_id: &str) -> Result<usize, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let affected = conn
            .execute(
                "DELETE FROM sm_capture_screenshots WHERE config_id = $1",
                &[&config_id],
            )
            .await
            .map_err(|e| format!("PG delete screenshots: {}", e))?;

        Ok(affected as usize)
    }

    /// Read-only audit of `sm_capture_screenshots` element_bounds_json.
    ///
    /// For each row with non-empty bounds, decodes the WebP to get physical
    /// dimensions and classifies whether the bounds are stored in physical
    /// pixels or CSS pixels (off by device-pixel-ratio).
    ///
    /// Returns a JSON value with per-row details and a summary.
    pub async fn audit_capture_screenshot_bounds(&self) -> Result<serde_json::Value, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                r#"SELECT id, config_id, capture_index, screenshot_webp, width, height,
                          element_bounds_json
                   FROM sm_capture_screenshots
                   WHERE element_bounds_json IS NOT NULL
                     AND element_bounds_json <> ''
                     AND element_bounds_json <> '[]'
                     AND element_bounds_json <> '{}'
                "#,
                &[],
            )
            .await
            .map_err(|e| format!("PG audit bounds select: {}", e))?;

        let common_dpr: f64 = 1.5;
        let tolerance: f64 = 2.0; // allow 2px tolerance for rounding

        let mut details: Vec<serde_json::Value> = Vec::new();
        let mut count_physical = 0usize;
        let mut count_css = 0usize;
        let mut count_unknown = 0usize;
        let mut count_decode_error = 0usize;

        for row in &rows {
            let id: String = row.get(0);
            let config_id: String = row.get(1);
            let capture_index: i32 = row.get(2);
            let bytes: Vec<u8> = row.get(3);
            let stored_w: i32 = row.get(4);
            let stored_h: i32 = row.get(5);
            let bounds_json_str: String = row.get(6);

            // Decode image to get actual physical dimensions
            let img = match image::load_from_memory(&bytes) {
                Ok(img) => img,
                Err(e) => {
                    count_decode_error += 1;
                    details.push(serde_json::json!({
                        "id": id,
                        "config_id": config_id,
                        "capture_index": capture_index,
                        "classification": "decode-error",
                        "error": format!("{}", e),
                    }));
                    continue;
                }
            };
            let phys_w = img.width() as f64;
            let phys_h = img.height() as f64;

            // Parse bounds to find max extents
            let bounds_val: serde_json::Value = match serde_json::from_str(&bounds_json_str) {
                Ok(v) => v,
                Err(_) => {
                    count_unknown += 1;
                    details.push(serde_json::json!({
                        "id": id,
                        "config_id": config_id,
                        "capture_index": capture_index,
                        "classification": "unknown",
                        "reason": "invalid JSON",
                    }));
                    continue;
                }
            };

            // Collect all bounds objects — support both array-of-objects and
            // object-with-nested-bounds patterns
            let bound_items: Vec<&serde_json::Value> = if let Some(arr) = bounds_val.as_array() {
                arr.iter().collect()
            } else if bounds_val.is_object() {
                vec![&bounds_val]
            } else {
                count_unknown += 1;
                details.push(serde_json::json!({
                    "id": id,
                    "config_id": config_id,
                    "capture_index": capture_index,
                    "classification": "unknown",
                    "reason": "bounds is not array or object",
                }));
                continue;
            };

            if bound_items.is_empty() {
                count_unknown += 1;
                details.push(serde_json::json!({
                    "id": id,
                    "config_id": config_id,
                    "capture_index": capture_index,
                    "classification": "unknown",
                    "reason": "empty bounds",
                }));
                continue;
            }

            // Compute max right-edge and max bottom-edge from all bounds
            let mut max_right: f64 = 0.0;
            let mut max_bottom: f64 = 0.0;
            let mut valid_bounds = 0usize;

            for item in &bound_items {
                let x = item.get("x").and_then(|v| v.as_f64()).unwrap_or(0.0);
                let y = item.get("y").and_then(|v| v.as_f64()).unwrap_or(0.0);
                let w = item.get("width").and_then(|v| v.as_f64()).unwrap_or(0.0);
                let h = item.get("height").and_then(|v| v.as_f64()).unwrap_or(0.0);
                if w > 0.0 || h > 0.0 {
                    valid_bounds += 1;
                    let right = x + w;
                    let bottom = y + h;
                    if right > max_right {
                        max_right = right;
                    }
                    if bottom > max_bottom {
                        max_bottom = bottom;
                    }
                }
            }

            if valid_bounds == 0 {
                count_unknown += 1;
                details.push(serde_json::json!({
                    "id": id,
                    "config_id": config_id,
                    "capture_index": capture_index,
                    "classification": "unknown",
                    "reason": "no bounds with positive dimensions",
                }));
                continue;
            }

            // Classify: bounds extents vs physical image dimensions
            // "physical-in-physical": max extents are close to decoded physical dims
            let phys_match_w =
                (max_right - phys_w).abs() < tolerance || (max_right > 0.0 && max_right <= phys_w);
            let phys_match_h = (max_bottom - phys_h).abs() < tolerance
                || (max_bottom > 0.0 && max_bottom <= phys_h);

            // "css-in-physical": max extents * dpr are close to decoded physical dims
            let css_right_scaled = max_right * common_dpr;
            let css_bottom_scaled = max_bottom * common_dpr;
            let css_match_w = (css_right_scaled - phys_w).abs() < tolerance
                || (css_right_scaled > 0.0 && css_right_scaled <= phys_w);
            let css_match_h = (css_bottom_scaled - phys_h).abs() < tolerance
                || (css_bottom_scaled > 0.0 && css_bottom_scaled <= phys_h);

            // Heuristic: if bounds extents are significantly smaller than physical
            // and scaling them up matches better, classify as CSS
            let ratio_w = if phys_w > 0.0 {
                max_right / phys_w
            } else {
                0.0
            };

            let classification = if max_right <= 0.0 && max_bottom <= 0.0 {
                "unknown"
            } else if phys_match_w && phys_match_h {
                "physical-in-physical"
            } else if (ratio_w - 1.0 / common_dpr).abs() < 0.15 && (css_match_w || css_match_h) {
                "css-in-physical"
            } else {
                "unknown"
            };

            match classification {
                "physical-in-physical" => count_physical += 1,
                "css-in-physical" => count_css += 1,
                _ => count_unknown += 1,
            }

            details.push(serde_json::json!({
                "id": id,
                "config_id": config_id,
                "capture_index": capture_index,
                "classification": classification,
                "physical_dims": { "width": phys_w, "height": phys_h },
                "stored_dims": { "width": stored_w, "height": stored_h },
                "bounds_extent": { "max_right": max_right, "max_bottom": max_bottom },
                "bounds_count": valid_bounds,
                "ratio_w": ratio_w,
            }));
        }

        let total = details.len();
        tracing::info!(
            "audit_capture_screenshot_bounds: total={} physical={} css={} unknown={} decode_error={}",
            total, count_physical, count_css, count_unknown, count_decode_error
        );

        Ok(serde_json::json!({
            "summary": {
                "total": total,
                "physical_in_physical": count_physical,
                "css_in_physical": count_css,
                "unknown": count_unknown,
                "decode_error": count_decode_error,
                "common_dpr_used": common_dpr,
            },
            "rows": details,
        }))
    }

    // -- helpers --

    fn sm_row_to_config(row: &tokio_postgres::Row) -> SmConfig {
        SmConfig {
            id: row.get(0),
            name: row.get(1),
            description: row.get(2),
            render_count: row.get::<_, i32>(3) as i64,
            element_count: row.get::<_, i32>(4) as i64,
            include_html_ids: row.get(5),
            created_at: row.get(6),
            updated_at: row.get(7),
        }
    }

    fn sm_row_to_state(row: &tokio_postgres::Row) -> SmState {
        let element_ids_json: String = row
            .get::<_, Option<String>>(5)
            .unwrap_or_else(|| "[]".to_string());
        let render_ids_json: String = row
            .get::<_, Option<String>>(6)
            .unwrap_or_else(|| "[]".to_string());
        let acceptance_criteria_json: String = row
            .get::<_, Option<String>>(8)
            .unwrap_or_else(|| "[]".to_string());
        let extra_metadata_json: String = row
            .get::<_, Option<String>>(9)
            .unwrap_or_else(|| "{}".to_string());
        let domain_knowledge_json: String = row
            .get::<_, Option<String>>(10)
            .unwrap_or_else(|| "[]".to_string());

        SmState {
            id: row.get(0),
            config_id: row.get(1),
            state_id: row.get(2),
            name: row.get(3),
            description: row.get(4),
            element_ids: serde_json::from_str(&element_ids_json).unwrap_or_default(),
            render_ids: serde_json::from_str(&render_ids_json).unwrap_or_default(),
            confidence: row.get(7),
            acceptance_criteria: serde_json::from_str(&acceptance_criteria_json)
                .unwrap_or_default(),
            extra_metadata: serde_json::from_str(&extra_metadata_json)
                .unwrap_or(serde_json::json!({})),
            domain_knowledge: serde_json::from_str(&domain_knowledge_json).unwrap_or_default(),
            created_at: row.get(11),
            updated_at: row.get(12),
        }
    }

    fn sm_row_to_transition(row: &tokio_postgres::Row) -> SmTransition {
        let from_states_json: String = row
            .get::<_, Option<String>>(4)
            .unwrap_or_else(|| "[]".to_string());
        let activate_states_json: String = row
            .get::<_, Option<String>>(5)
            .unwrap_or_else(|| "[]".to_string());
        let exit_states_json: String = row
            .get::<_, Option<String>>(6)
            .unwrap_or_else(|| "[]".to_string());
        let actions_json: String = row
            .get::<_, Option<String>>(7)
            .unwrap_or_else(|| "[]".to_string());
        let extra_metadata_json: String = row
            .get::<_, Option<String>>(10)
            .unwrap_or_else(|| "{}".to_string());

        SmTransition {
            id: row.get(0),
            config_id: row.get(1),
            transition_id: row.get(2),
            name: row.get(3),
            from_states: serde_json::from_str(&from_states_json).unwrap_or_default(),
            activate_states: serde_json::from_str(&activate_states_json).unwrap_or_default(),
            exit_states: serde_json::from_str(&exit_states_json).unwrap_or_default(),
            actions: serde_json::from_str(&actions_json).unwrap_or_default(),
            path_cost: row.get(8),
            stays_visible: row.get(9),
            extra_metadata: serde_json::from_str(&extra_metadata_json)
                .unwrap_or(serde_json::json!({})),
            created_at: row.get(11),
            updated_at: row.get(12),
        }
    }
}

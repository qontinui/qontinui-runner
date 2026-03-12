//! SQLite CRUD operations for state machine configs, states, and transitions.

use rusqlite::{params, Connection, OptionalExtension};
use tracing::{info, warn};

use super::types::*;

// =============================================================================
// Config CRUD
// =============================================================================

pub fn list_configs(conn: &Connection) -> Result<Vec<SmConfig>, String> {
    let mut stmt = conn
        .prepare(
            r#"SELECT id, name, description, render_count, element_count, include_html_ids,
                      created_at, updated_at
               FROM state_machine_configs
               ORDER BY updated_at DESC"#,
        )
        .map_err(|e| format!("Failed to prepare list configs: {}", e))?;

    let rows = stmt
        .query_map([], |row| {
            Ok(SmConfig {
                id: row.get(0)?,
                name: row.get(1)?,
                description: row.get(2)?,
                render_count: row.get(3)?,
                element_count: row.get(4)?,
                include_html_ids: row.get(5)?,
                created_at: row.get(6)?,
                updated_at: row.get(7)?,
            })
        })
        .map_err(|e| format!("Failed to query configs: {}", e))?;

    let mut configs = Vec::new();
    for row in rows {
        configs.push(row.map_err(|e| format!("Failed to read config row: {}", e))?);
    }
    Ok(configs)
}

pub fn get_config(conn: &Connection, id: &str) -> Result<Option<SmConfig>, String> {
    conn.query_row(
        r#"SELECT id, name, description, render_count, element_count, include_html_ids,
                  created_at, updated_at
           FROM state_machine_configs WHERE id = ?1"#,
        params![id],
        |row| {
            Ok(SmConfig {
                id: row.get(0)?,
                name: row.get(1)?,
                description: row.get(2)?,
                render_count: row.get(3)?,
                element_count: row.get(4)?,
                include_html_ids: row.get(5)?,
                created_at: row.get(6)?,
                updated_at: row.get(7)?,
            })
        },
    )
    .optional()
    .map_err(|e| format!("Failed to get config: {}", e))
}

pub fn insert_config(conn: &Connection, req: &CreateSmConfigRequest) -> Result<SmConfig, String> {
    let id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();

    conn.execute(
        r#"INSERT INTO state_machine_configs (id, name, description, render_count, element_count, include_html_ids, created_at, updated_at)
           VALUES (?1, ?2, ?3, 0, 0, FALSE, ?4, ?4)"#,
        params![id, req.name, req.description, now],
    )
    .map_err(|e| format!("Failed to insert config: {}", e))?;

    get_config(conn, &id)?.ok_or_else(|| "Config not found after insert".to_string())
}

pub fn update_config(
    conn: &Connection,
    id: &str,
    req: &UpdateSmConfigRequest,
) -> Result<SmConfig, String> {
    let existing = get_config(conn, id)?.ok_or_else(|| format!("Config not found: {}", id))?;
    let now = chrono::Utc::now().to_rfc3339();

    let name = req.name.as_deref().unwrap_or(&existing.name);
    let description = req
        .description
        .as_deref()
        .or(existing.description.as_deref());
    let render_count = req.render_count.unwrap_or(existing.render_count);
    let element_count = req.element_count.unwrap_or(existing.element_count);
    let include_html_ids = req.include_html_ids.unwrap_or(existing.include_html_ids);

    conn.execute(
        r#"UPDATE state_machine_configs
           SET name = ?1, description = ?2, render_count = ?3, element_count = ?4,
               include_html_ids = ?5, updated_at = ?6
           WHERE id = ?7"#,
        params![
            name,
            description,
            render_count,
            element_count,
            include_html_ids,
            now,
            id
        ],
    )
    .map_err(|e| format!("Failed to update config: {}", e))?;

    get_config(conn, id)?.ok_or_else(|| "Config not found after update".to_string())
}

pub fn delete_config(conn: &Connection, id: &str) -> Result<bool, String> {
    // CASCADE will handle states and transitions via FK constraints
    let rows = conn
        .execute(
            "DELETE FROM state_machine_configs WHERE id = ?1",
            params![id],
        )
        .map_err(|e| format!("Failed to delete config: {}", e))?;

    if rows > 0 {
        info!("Deleted state machine config: {}", id);
    }
    Ok(rows > 0)
}

// =============================================================================
// Config Full (with states + transitions)
// =============================================================================

pub fn get_config_full(conn: &Connection, id: &str) -> Result<Option<SmConfigFull>, String> {
    let config = match get_config(conn, id)? {
        Some(c) => c,
        None => return Ok(None),
    };

    let states = list_states(conn, id)?;
    let transitions = list_transitions(conn, id)?;

    Ok(Some(SmConfigFull {
        config,
        states,
        transitions,
    }))
}

// =============================================================================
// State CRUD
// =============================================================================

fn row_to_state(row: &rusqlite::Row) -> rusqlite::Result<SmState> {
    let id: String = row.get(0)?;
    let element_ids_json: String = row.get(5)?;
    let render_ids_json: String = row.get(6)?;
    let acceptance_criteria_json: String = row.get(8)?;
    let extra_metadata_json: String = row.get(9)?;
    let domain_knowledge_json: String = row.get(10)?;

    let element_ids = match serde_json::from_str(&element_ids_json) {
        Ok(v) => v,
        Err(e) => {
            warn!("Failed to deserialize element_ids for state {}: {}", id, e);
            Vec::new()
        }
    };
    let render_ids = match serde_json::from_str(&render_ids_json) {
        Ok(v) => v,
        Err(e) => {
            warn!("Failed to deserialize render_ids for state {}: {}", id, e);
            Vec::new()
        }
    };
    let acceptance_criteria = match serde_json::from_str(&acceptance_criteria_json) {
        Ok(v) => v,
        Err(e) => {
            warn!(
                "Failed to deserialize acceptance_criteria for state {}: {}",
                id, e
            );
            Vec::new()
        }
    };
    let extra_metadata = match serde_json::from_str(&extra_metadata_json) {
        Ok(v) => v,
        Err(e) => {
            warn!(
                "Failed to deserialize extra_metadata for state {}: {}",
                id, e
            );
            serde_json::json!({})
        }
    };
    let domain_knowledge = match serde_json::from_str(&domain_knowledge_json) {
        Ok(v) => v,
        Err(e) => {
            warn!(
                "Failed to deserialize domain_knowledge for state {}: {}",
                id, e
            );
            Vec::new()
        }
    };

    Ok(SmState {
        id,
        config_id: row.get(1)?,
        state_id: row.get(2)?,
        name: row.get(3)?,
        description: row.get(4)?,
        element_ids,
        render_ids,
        confidence: row.get(7)?,
        acceptance_criteria,
        extra_metadata,
        domain_knowledge,
        created_at: row.get(11)?,
        updated_at: row.get(12)?,
    })
}

pub fn list_states(conn: &Connection, config_id: &str) -> Result<Vec<SmState>, String> {
    let mut stmt = conn
        .prepare(
            r#"SELECT id, config_id, state_id, name, description, element_ids, render_ids,
                      confidence, acceptance_criteria, extra_metadata, domain_knowledge,
                      created_at, updated_at
               FROM state_machine_states
               WHERE config_id = ?1
               ORDER BY name"#,
        )
        .map_err(|e| format!("Failed to prepare list states: {}", e))?;

    let rows = stmt
        .query_map(params![config_id], row_to_state)
        .map_err(|e| format!("Failed to query states: {}", e))?;

    let mut states = Vec::new();
    for row in rows {
        states.push(row.map_err(|e| format!("Failed to read state row: {}", e))?);
    }
    Ok(states)
}

pub fn get_state(conn: &Connection, id: &str) -> Result<Option<SmState>, String> {
    conn.query_row(
        r#"SELECT id, config_id, state_id, name, description, element_ids, render_ids,
                  confidence, acceptance_criteria, extra_metadata, domain_knowledge,
                  created_at, updated_at
           FROM state_machine_states WHERE id = ?1"#,
        params![id],
        row_to_state,
    )
    .optional()
    .map_err(|e| format!("Failed to get state: {}", e))
}

pub fn insert_state(
    conn: &Connection,
    config_id: &str,
    req: &CreateSmStateRequest,
) -> Result<SmState, String> {
    let id = uuid::Uuid::new_v4().to_string();
    let state_id = req
        .state_id
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let now = chrono::Utc::now().to_rfc3339();

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
    let domain_knowledge = serde_json::to_string(req.domain_knowledge.as_deref().unwrap_or(&[]))
        .map_err(|e| format!("Failed to serialize domain_knowledge: {}", e))?;
    let confidence = req.confidence.unwrap_or(0.9);

    conn.execute(
        r#"INSERT INTO state_machine_states
           (id, config_id, state_id, name, description, element_ids, render_ids,
            confidence, acceptance_criteria, extra_metadata, domain_knowledge,
            created_at, updated_at)
           VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?12)"#,
        params![
            id,
            config_id,
            state_id,
            req.name,
            req.description,
            element_ids,
            render_ids,
            confidence,
            acceptance_criteria,
            extra_metadata,
            domain_knowledge,
            now,
        ],
    )
    .map_err(|e| format!("Failed to insert state: {}", e))?;

    get_state(conn, &id)?.ok_or_else(|| "State not found after insert".to_string())
}

pub fn update_state(
    conn: &Connection,
    id: &str,
    req: &UpdateSmStateRequest,
) -> Result<SmState, String> {
    let existing = get_state(conn, id)?.ok_or_else(|| format!("State not found: {}", id))?;
    let now = chrono::Utc::now().to_rfc3339();

    let name = req.name.as_deref().unwrap_or(&existing.name);
    let description = req
        .description
        .as_deref()
        .or(existing.description.as_deref());
    let element_ids =
        serde_json::to_string(req.element_ids.as_ref().unwrap_or(&existing.element_ids))
            .map_err(|e| format!("Failed to serialize element_ids: {}", e))?;
    let render_ids = serde_json::to_string(req.render_ids.as_ref().unwrap_or(&existing.render_ids))
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
        r#"UPDATE state_machine_states
           SET name = ?1, description = ?2, element_ids = ?3, render_ids = ?4,
               confidence = ?5, acceptance_criteria = ?6, extra_metadata = ?7,
               domain_knowledge = ?8, updated_at = ?9
           WHERE id = ?10"#,
        params![
            name,
            description,
            element_ids,
            render_ids,
            confidence,
            acceptance_criteria,
            extra_metadata,
            domain_knowledge,
            now,
            id,
        ],
    )
    .map_err(|e| format!("Failed to update state: {}", e))?;

    get_state(conn, id)?.ok_or_else(|| "State not found after update".to_string())
}

pub fn delete_state(conn: &Connection, id: &str) -> Result<bool, String> {
    let rows = conn
        .execute(
            "DELETE FROM state_machine_states WHERE id = ?1",
            params![id],
        )
        .map_err(|e| format!("Failed to delete state: {}", e))?;
    Ok(rows > 0)
}

// =============================================================================
// Transition CRUD
// =============================================================================

fn row_to_transition(row: &rusqlite::Row) -> rusqlite::Result<SmTransition> {
    let id: String = row.get(0)?;
    let from_states_json: String = row.get(4)?;
    let activate_states_json: String = row.get(5)?;
    let exit_states_json: String = row.get(6)?;
    let actions_json: String = row.get(7)?;
    let extra_metadata_json: String = row.get(10)?;

    let from_states = match serde_json::from_str(&from_states_json) {
        Ok(v) => v,
        Err(e) => {
            warn!(
                "Failed to deserialize from_states for transition {}: {}",
                id, e
            );
            Vec::new()
        }
    };
    let activate_states = match serde_json::from_str(&activate_states_json) {
        Ok(v) => v,
        Err(e) => {
            warn!(
                "Failed to deserialize activate_states for transition {}: {}",
                id, e
            );
            Vec::new()
        }
    };
    let exit_states = match serde_json::from_str(&exit_states_json) {
        Ok(v) => v,
        Err(e) => {
            warn!(
                "Failed to deserialize exit_states for transition {}: {}",
                id, e
            );
            Vec::new()
        }
    };
    let actions = match serde_json::from_str(&actions_json) {
        Ok(v) => v,
        Err(e) => {
            warn!("Failed to deserialize actions for transition {}: {}", id, e);
            Vec::new()
        }
    };
    let extra_metadata = match serde_json::from_str(&extra_metadata_json) {
        Ok(v) => v,
        Err(e) => {
            warn!(
                "Failed to deserialize extra_metadata for transition {}: {}",
                id, e
            );
            serde_json::json!({})
        }
    };

    Ok(SmTransition {
        id,
        config_id: row.get(1)?,
        transition_id: row.get(2)?,
        name: row.get(3)?,
        from_states,
        activate_states,
        exit_states,
        actions,
        path_cost: row.get(8)?,
        stays_visible: row.get(9)?,
        extra_metadata,
        created_at: row.get(11)?,
        updated_at: row.get(12)?,
    })
}

pub fn list_transitions(conn: &Connection, config_id: &str) -> Result<Vec<SmTransition>, String> {
    let mut stmt = conn
        .prepare(
            r#"SELECT id, config_id, transition_id, name, from_states, activate_states,
                      exit_states, actions, path_cost, stays_visible, extra_metadata,
                      created_at, updated_at
               FROM state_machine_transitions
               WHERE config_id = ?1
               ORDER BY name"#,
        )
        .map_err(|e| format!("Failed to prepare list transitions: {}", e))?;

    let rows = stmt
        .query_map(params![config_id], row_to_transition)
        .map_err(|e| format!("Failed to query transitions: {}", e))?;

    let mut transitions = Vec::new();
    for row in rows {
        transitions.push(row.map_err(|e| format!("Failed to read transition row: {}", e))?);
    }
    Ok(transitions)
}

pub fn get_transition(conn: &Connection, id: &str) -> Result<Option<SmTransition>, String> {
    conn.query_row(
        r#"SELECT id, config_id, transition_id, name, from_states, activate_states,
                  exit_states, actions, path_cost, stays_visible, extra_metadata,
                  created_at, updated_at
           FROM state_machine_transitions WHERE id = ?1"#,
        params![id],
        row_to_transition,
    )
    .optional()
    .map_err(|e| format!("Failed to get transition: {}", e))
}

pub fn insert_transition(
    conn: &Connection,
    config_id: &str,
    req: &CreateSmTransitionRequest,
) -> Result<SmTransition, String> {
    let id = uuid::Uuid::new_v4().to_string();
    let transition_id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();

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
        r#"INSERT INTO state_machine_transitions
           (id, config_id, transition_id, name, from_states, activate_states, exit_states,
            actions, path_cost, stays_visible, extra_metadata, created_at, updated_at)
           VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?12)"#,
        params![
            id,
            config_id,
            transition_id,
            req.name,
            from_states,
            activate_states,
            exit_states,
            actions,
            path_cost,
            stays_visible,
            extra_metadata,
            now,
        ],
    )
    .map_err(|e| format!("Failed to insert transition: {}", e))?;

    get_transition(conn, &id)?.ok_or_else(|| "Transition not found after insert".to_string())
}

pub fn update_transition(
    conn: &Connection,
    id: &str,
    req: &UpdateSmTransitionRequest,
) -> Result<SmTransition, String> {
    let existing =
        get_transition(conn, id)?.ok_or_else(|| format!("Transition not found: {}", id))?;
    let now = chrono::Utc::now().to_rfc3339();

    let name = req.name.as_deref().unwrap_or(&existing.name);
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
        r#"UPDATE state_machine_transitions
           SET name = ?1, from_states = ?2, activate_states = ?3, exit_states = ?4,
               actions = ?5, path_cost = ?6, stays_visible = ?7, extra_metadata = ?8,
               updated_at = ?9
           WHERE id = ?10"#,
        params![
            name,
            from_states,
            activate_states,
            exit_states,
            actions,
            path_cost,
            stays_visible,
            extra_metadata,
            now,
            id,
        ],
    )
    .map_err(|e| format!("Failed to update transition: {}", e))?;

    get_transition(conn, id)?.ok_or_else(|| "Transition not found after update".to_string())
}

pub fn delete_transition(conn: &Connection, id: &str) -> Result<bool, String> {
    let rows = conn
        .execute(
            "DELETE FROM state_machine_transitions WHERE id = ?1",
            params![id],
        )
        .map_err(|e| format!("Failed to delete transition: {}", e))?;
    Ok(rows > 0)
}

// =============================================================================
// Import
// =============================================================================

/// Import a state machine config from the export format.
///
/// The export format has `config`, `states`, and `transitions` as nested objects.
/// This creates a new config with all its states and transitions.
pub fn import_config(conn: &Connection, req: &SmImportRequest) -> Result<SmConfigFull, String> {
    let data = &req.config;

    // Extract config metadata before starting transaction
    let config_data = data
        .get("config")
        .ok_or("Missing 'config' field in import data")?;
    let config_name = req
        .name
        .as_deref()
        .or_else(|| config_data.get("name").and_then(|v| v.as_str()))
        .unwrap_or("Imported Config");

    // Wrap all inserts in a transaction for atomicity
    conn.execute_batch("BEGIN;")
        .map_err(|e| format!("Failed to begin import transaction: {}", e))?;

    let result = (|| -> Result<SmConfigFull, String> {
        let create_req = CreateSmConfigRequest {
            name: config_name.to_string(),
            description: config_data
                .get("description")
                .and_then(|v| v.as_str())
                .map(String::from),
        };

        let mut config = insert_config(conn, &create_req)?;

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

        config = update_config(
            conn,
            &config.id,
            &UpdateSmConfigRequest {
                name: None,
                description: None,
                render_count: Some(render_count),
                element_count: Some(element_count),
                include_html_ids: Some(include_html_ids),
            },
        )?;

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

                states.push(insert_state(conn, &config.id, &req)?);
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

                transitions.push(insert_transition(conn, &config.id, &req)?);
            }
        }

        Ok(SmConfigFull {
            config,
            states,
            transitions,
        })
    })();

    match result {
        Ok(config_full) => {
            conn.execute_batch("COMMIT;")
                .map_err(|e| format!("Failed to commit import transaction: {}", e))?;
            info!(
                "Imported state machine config '{}': {} states, {} transitions",
                config_full.config.name,
                config_full.states.len(),
                config_full.transitions.len()
            );
            Ok(config_full)
        }
        Err(e) => {
            if let Err(rollback_err) = conn.execute_batch("ROLLBACK;") {
                warn!("Failed to rollback import transaction: {}", rollback_err);
            }
            Err(e)
        }
    }
}

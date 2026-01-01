//! Workflow execution commands
//!
//! Commands for starting, stopping, and configuring workflow execution,
//! including initial states resolution and screen requirements analysis.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tauri::State;
use tracing::{debug, info};

use super::super::{AppState, CommandResponse};

/// Start workflow execution.
///
/// Begins executing a workflow with the specified parameters.
///
/// # Arguments
/// * `process_id` - The workflow ID to execute (required)
/// * `monitor_indices` - Array of monitor indices to use (defaults to [0])
/// * `monitor_index` - Legacy single monitor index (deprecated, use monitor_indices)
/// * `initial_state_ids` - Optional override for initial active states (session-only)
/// * `state` - Application state containing the Python bridge
///
/// Note: Monitor offset calculation is handled by the qontinui Python library
/// using MSS, ensuring coordinate consistency with screenshot capture.
///
/// Initial states priority (highest to lowest):
/// 1. `initial_state_ids` parameter (runner override, session-only)
/// 2. Workflow's `initialStateIds` field (workflow-level override)
/// 3. States with `is_initial: true` (state machine defaults)
///
/// # Returns
/// * `Ok(CommandResponse)` - Success message
/// * `Err(String)` - Error if executor not running or workflow ID missing
#[tauri::command]
pub fn start_execution(
    process_id: Option<String>,
    monitor_indices: Option<Vec<i32>>,
    monitor_index: Option<i32>, // Legacy single monitor support
    initial_state_ids: Option<Vec<String>>, // Override for initial active states
    state: State<Arc<AppState>>,
) -> Result<CommandResponse, String> {
    let mut bridge_lock = state.python_bridge.lock().unwrap();

    if let Some(ref mut bridge) = *bridge_lock {
        if !bridge.is_running() {
            return Err("Python executor not running".to_string());
        }

        // Build params
        let mut params = serde_json::Map::new();

        // Resolve monitor indices (prefer array, fall back to legacy single index)
        let resolved_monitors = monitor_indices.unwrap_or_else(|| vec![monitor_index.unwrap_or(0)]);

        // Pass both formats for compatibility
        params.insert(
            "monitor_indices".to_string(),
            serde_json::json!(resolved_monitors),
        );
        // Also pass single monitor_index for backward compatibility with Python
        params.insert(
            "monitor_index".to_string(),
            serde_json::json!(resolved_monitors.first().copied().unwrap_or(0)),
        );
        debug!("Using monitor indices: {:?}", resolved_monitors);

        // Add workflow_id (required)
        let workflow_id = if let Some(pid) = process_id {
            params.insert("workflow_id".to_string(), serde_json::json!(&pid));
            pid
        } else {
            return Err("Workflow ID is required".to_string());
        };

        // Resolve and add initial_state_ids
        // Priority: override param > workflow.initialStateIds > states with is_initial=true
        let config_lock = state.current_config.lock().unwrap();
        let resolved_initial_states =
            resolve_initial_states(config_lock.as_ref(), &workflow_id, initial_state_ids);
        drop(config_lock);

        if !resolved_initial_states.is_empty() {
            params.insert(
                "initial_state_ids".to_string(),
                serde_json::json!(resolved_initial_states),
            );
            debug!("Using initial state IDs: {:?}", resolved_initial_states);
        }

        bridge
            .start_execution_with_params(Some(serde_json::Value::Object(params)))
            .map_err(|e| format!("Failed to start execution: {}", e))?;

        Ok(CommandResponse {
            success: true,
            message: Some("Execution started".to_string()),
            data: None,
        })
    } else {
        Err("Python executor not initialized".to_string())
    }
}

/// Stop the current workflow execution.
///
/// Stops any running workflow but keeps the executor active.
///
/// # Arguments
/// * `state` - Application state containing the Python bridge
///
/// # Returns
/// * `Ok(CommandResponse)` - Success message
/// * `Err(String)` - Error if executor not initialized or stop fails
#[tauri::command]
pub fn stop_execution(state: State<Arc<AppState>>) -> Result<CommandResponse, String> {
    let mut bridge_lock = state.python_bridge.lock().unwrap();

    if let Some(ref mut bridge) = *bridge_lock {
        bridge
            .stop_execution()
            .map_err(|e| format!("Failed to stop execution: {}", e))?;

        Ok(CommandResponse {
            success: true,
            message: Some("Execution stopped".to_string()),
            data: None,
        })
    } else {
        Err("Python executor not initialized".to_string())
    }
}

/// Resolve initial state IDs with priority system.
///
/// Priority (highest to lowest):
/// 1. Override IDs passed directly (runner session override)
/// 2. Workflow's `initialStateIds` field (workflow-level configuration)
/// 3. States with `is_initial: true` (state machine defaults)
///
/// # Arguments
/// * `config` - Optional reference to loaded configuration
/// * `workflow_id` - The workflow ID to resolve initial states for
/// * `override_ids` - Optional override from runner UI (highest priority)
///
/// # Returns
/// Vector of state IDs to use as initial states (may be empty)
pub fn resolve_initial_states(
    config: Option<&crate::config::QontinuiConfig>,
    workflow_id: &str,
    override_ids: Option<Vec<String>>,
) -> Vec<String> {
    // Priority 1: Use override if provided
    if let Some(ids) = override_ids {
        if !ids.is_empty() {
            debug!("Using override initial states: {:?}", ids);
            return ids;
        }
    }

    // Need config for remaining priorities
    let config = match config {
        Some(c) => c,
        None => {
            debug!("No config loaded, returning empty initial states");
            return Vec::new();
        }
    };

    // Priority 2: Check workflow.initialStateIds
    if let Some(workflow) = config
        .workflows
        .iter()
        .find(|w| w.get("id").and_then(|id| id.as_str()) == Some(workflow_id))
    {
        if let Some(initial_ids) = workflow
            .get("initialStateIds")
            .and_then(|ids| ids.as_array())
        {
            let ids: Vec<String> = initial_ids
                .iter()
                .filter_map(|id| id.as_str().map(String::from))
                .collect();
            if !ids.is_empty() {
                debug!("Using workflow initial states: {:?}", ids);
                return ids;
            }
        }
    }

    // Priority 3: Fall back to states with is_initial=true (or isInitial for JSON compat)
    let default_ids: Vec<String> = config
        .states
        .iter()
        .filter(|s| {
            s.get("isInitial")
                .or_else(|| s.get("is_initial"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
        })
        .filter_map(|s| s.get("id").and_then(|id| id.as_str()).map(String::from))
        .collect();

    if !default_ids.is_empty() {
        debug!(
            "Using default initial states (is_initial=true): {:?}",
            default_ids
        );
    }

    default_ids
}

/// Determine the source of resolved initial states.
///
/// Returns the source type: "override", "workflow", or "defaults"
fn get_initial_states_source(
    config: Option<&crate::config::QontinuiConfig>,
    workflow_id: &str,
    override_ids: &Option<Vec<String>>,
) -> &'static str {
    // Check override first
    if let Some(ids) = override_ids {
        if !ids.is_empty() {
            return "override";
        }
    }

    // Need config for remaining checks
    let config = match config {
        Some(c) => c,
        None => return "defaults",
    };

    // Check workflow.initialStateIds
    if let Some(workflow) = config
        .workflows
        .iter()
        .find(|w| w.get("id").and_then(|id| id.as_str()) == Some(workflow_id))
    {
        if let Some(initial_ids) = workflow
            .get("initialStateIds")
            .and_then(|ids| ids.as_array())
        {
            if !initial_ids.is_empty() {
                return "workflow";
            }
        }
    }

    "defaults"
}

/// Get the resolved initial states for a workflow.
///
/// Returns the initial states that would be used for execution, along with
/// the source of those states (defaults, workflow, or override).
///
/// This is useful for the UI to display current initial states before execution
/// without actually starting a workflow.
///
/// # Arguments
/// * `workflow_id` - The workflow ID to get initial states for
/// * `state` - Application state containing the config
///
/// # Returns
/// * `Ok(CommandResponse)` - Success with stateIds array and source
/// * `Err(String)` - Error if query fails
#[tauri::command]
pub fn get_resolved_initial_states(
    workflow_id: String,
    state: State<Arc<AppState>>,
) -> Result<CommandResponse, String> {
    debug!(
        "Getting resolved initial states for workflow: {}",
        workflow_id
    );

    let config_lock = state.current_config.lock().unwrap();
    let config = config_lock.as_ref();

    // Get resolved states (without override, since this is for display)
    let state_ids = resolve_initial_states(config, &workflow_id, None);
    let source = get_initial_states_source(config, &workflow_id, &None);

    // Also get state names for better UI display
    let state_names: Vec<serde_json::Value> = if let Some(cfg) = config {
        state_ids
            .iter()
            .map(|id| {
                let name = cfg
                    .states
                    .iter()
                    .find(|s| s.get("id").and_then(|i| i.as_str()) == Some(id))
                    .and_then(|s| s.get("name").and_then(|n| n.as_str()))
                    .unwrap_or(id);
                serde_json::json!({
                    "id": id,
                    "name": name
                })
            })
            .collect()
    } else {
        state_ids
            .iter()
            .map(|id| serde_json::json!({ "id": id, "name": id }))
            .collect()
    };

    Ok(CommandResponse {
        success: true,
        message: None,
        data: Some(serde_json::json!({
            "stateIds": state_ids,
            "source": source,
            "states": state_names,
            "workflowId": workflow_id
        })),
    })
}

/// Get the required screens/monitors for a workflow based on automatic calculation.
///
/// Analyzes the workflow's actions and their associated states to determine
/// which monitors will be used during execution.
///
/// Note: This is a lightweight analysis that happens synchronously. The Python
/// executor performs the analysis and returns the result immediately.
///
/// # Arguments
/// * `workflow_id` - The workflow ID to analyze
/// * `state` - Application state containing the Python bridge and config
///
/// # Returns
/// * `Ok(CommandResponse)` - Success with array of monitor indices in data.screens
/// * `Err(String)` - Error if executor not running or analysis fails
#[tauri::command]
pub fn get_workflow_required_screens(
    workflow_id: String,
    state: State<Arc<AppState>>,
) -> Result<CommandResponse, String> {
    info!("Getting required screens for workflow: {}", workflow_id);

    // Get the current config from state
    let config_lock = state
        .current_config
        .lock()
        .expect("current_config mutex poisoned");
    let config = match config_lock.as_ref() {
        Some(c) => c,
        None => {
            return Ok(CommandResponse {
                success: false,
                message: Some("No configuration loaded".to_string()),
                data: None,
            });
        }
    };

    // Find the workflow
    let workflow = match config
        .workflows
        .iter()
        .find(|w| w.get("id").and_then(|id| id.as_str()) == Some(&workflow_id))
    {
        Some(w) => w,
        None => {
            return Ok(CommandResponse {
                success: false,
                message: Some(format!("Workflow '{}' not found", workflow_id)),
                data: None,
            });
        }
    };

    // Collect monitor associations from states
    let mut screens = HashSet::new();

    // Build stateimage_id -> monitors mapping (more precise than per-state)
    let mut stateimage_monitors_map: HashMap<String, Vec<i32>> = HashMap::new();
    // Also build region/location/string -> monitors mapping
    let mut element_monitors_map: HashMap<String, Vec<i32>> = HashMap::new();

    for state in &config.states {
        // Check stateImages for monitors field
        if let Some(state_images) = state.get("stateImages").and_then(|si| si.as_array()) {
            for state_image in state_images {
                if let Some(state_image_id) = state_image.get("id").and_then(|id| id.as_str()) {
                    if let Some(monitors) = state_image.get("monitors").and_then(|m| m.as_array()) {
                        let mut image_monitors = Vec::new();
                        for monitor in monitors {
                            if let Some(monitor_idx) = monitor.as_i64() {
                                image_monitors.push(monitor_idx as i32);
                            }
                        }
                        if !image_monitors.is_empty() {
                            image_monitors.sort();
                            image_monitors.dedup();
                            stateimage_monitors_map
                                .insert(state_image_id.to_string(), image_monitors);
                        }
                    }
                }
            }
        }

        // Also check regions, locations, and strings for monitors
        for field_name in &["regions", "locations", "strings"] {
            if let Some(items) = state.get(*field_name).and_then(|f| f.as_array()) {
                for item in items {
                    if let Some(item_id) = item.get("id").and_then(|id| id.as_str()) {
                        if let Some(monitors) = item.get("monitors").and_then(|m| m.as_array()) {
                            let mut item_monitors = Vec::new();
                            for monitor in monitors {
                                if let Some(monitor_idx) = monitor.as_i64() {
                                    item_monitors.push(monitor_idx as i32);
                                }
                            }
                            if !item_monitors.is_empty() {
                                item_monitors.sort();
                                item_monitors.dedup();
                                element_monitors_map.insert(item_id.to_string(), item_monitors);
                            }
                        }
                    }
                }
            }
        }
    }

    // Helper closure to add monitors from a stateimage ID
    let add_stateimage_monitors = |screens: &mut HashSet<i32>, id: &str| {
        if let Some(monitors) = stateimage_monitors_map.get(id) {
            for &monitor in monitors {
                screens.insert(monitor);
            }
        }
    };

    // Helper closure to add monitors from an element ID (region/location/string)
    let add_element_monitors = |screens: &mut HashSet<i32>, id: &str| {
        if let Some(monitors) = element_monitors_map.get(id) {
            for &monitor in monitors {
                screens.insert(monitor);
            }
        }
    };

    // NOTE: We intentionally skip initialStateIds and GO_TO_STATE for monitor calculation.
    // These represent navigation targets, not the actual elements being interacted with.
    // The monitors should be determined by the specific images/elements used in actions.

    // Analyze actions - collect monitors from specific image/element references
    if let Some(actions) = workflow.get("actions").and_then(|a| a.as_array()) {
        for action in actions {
            if let Some(action_config) = action.get("config") {
                // Check target for imageIds array (handles CLICK, FIND with type "image" or "stateImage")
                if let Some(target) = action_config.get("target") {
                    // Check imageIds array (most common pattern)
                    if let Some(image_ids) = target.get("imageIds").and_then(|ids| ids.as_array()) {
                        for image_id in image_ids {
                            if let Some(id_str) = image_id.as_str() {
                                add_stateimage_monitors(&mut screens, id_str);
                            }
                        }
                    }
                    // Check single imageId field (alternative format)
                    else if let Some(image_id) = target.get("imageId").and_then(|id| id.as_str())
                    {
                        add_stateimage_monitors(&mut screens, image_id);
                    }
                    // Check stateImageId field (used in some FIND actions)
                    else if let Some(state_image_id) =
                        target.get("stateImageId").and_then(|id| id.as_str())
                    {
                        add_stateimage_monitors(&mut screens, state_image_id);
                    }

                    // Check regionId for region-based actions
                    if let Some(region_id) = target.get("regionId").and_then(|id| id.as_str()) {
                        add_element_monitors(&mut screens, region_id);
                    }

                    // Check locationId for location-based actions
                    if let Some(location_id) = target.get("locationId").and_then(|id| id.as_str()) {
                        add_element_monitors(&mut screens, location_id);
                    }
                }
            }
        }
    }

    // NOTE: We intentionally skip transitions for monitor calculation.
    // Transitions are about state navigation, not element interaction.

    // Convert to sorted vec
    let mut screen_list: Vec<i32> = screens.into_iter().collect();
    screen_list.sort();

    // Default to screen 0 if no screens found
    if screen_list.is_empty() {
        screen_list.push(0);
    }

    info!(
        "Workflow '{}' requires screens: {:?} (found {} stateImages with monitor info)",
        workflow_id,
        screen_list,
        stateimage_monitors_map.len()
    );
    debug!(
        "StateImage -> monitors mapping: {:?}",
        stateimage_monitors_map
    );
    debug!("Element -> monitors mapping: {:?}", element_monitors_map);

    Ok(CommandResponse {
        success: true,
        message: None,
        data: Some(serde_json::json!({
            "screens": screen_list
        })),
    })
}

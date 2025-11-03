use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use crate::display::{DisplayProfile, RawEvent, ViewType};

/// Configuration for Action Log profile
#[derive(Debug, Clone)]
pub struct ActionLogConfig {
    /// Action types to include (if empty, include all)
    pub include_actions: HashSet<String>,

    /// Action types to explicitly exclude
    pub exclude_actions: HashSet<String>,

    /// Whether to exclude inline workflows (helper workflows)
    pub exclude_inline_workflows: bool,

    /// Whether to flatten the hierarchy
    pub flatten_hierarchy: bool,
}

impl Default for ActionLogConfig {
    fn default() -> Self {
        // Default for Actions tab: Exclude IF and WAIT actions, include all workflows
        let include_actions = HashSet::new(); // Empty = include all by default

        // Exclude IF (internal control flow) and WAIT (redundant with other actions)
        let mut exclude_actions = HashSet::new();
        exclude_actions.insert("IF".to_string());
        exclude_actions.insert("WAIT".to_string());

        Self {
            include_actions,
            exclude_actions,
            exclude_inline_workflows: false,  // Include helper workflows
            flatten_hierarchy: true,
        }
    }
}

/// Display profile for Action Log view
pub struct ActionLogProfile {
    config: ActionLogConfig,
}

impl ActionLogProfile {
    pub fn new(config: ActionLogConfig) -> Self {
        Self { config }
    }

    pub fn with_default_config() -> Self {
        Self::new(ActionLogConfig::default())
    }

    /// Check if an action should be visible based on configuration
    fn is_visible_action(&self, action_type: &str, is_inline_workflow: bool) -> bool {
        // Exclude inline workflows if configured
        if is_inline_workflow && self.config.exclude_inline_workflows {
            return false;
        }

        // Check explicit exclude list
        if self.config.exclude_actions.contains(action_type) {
            return false;
        }

        // If include list is specified, check it
        if !self.config.include_actions.is_empty() {
            return self.config.include_actions.contains(action_type);
        }

        // Default: include
        true
    }

    /// Extract action info from event data
    fn extract_action_info(&self, event: &RawEvent, node_type_filter: &str) -> Option<ActionInfo> {
        let node = event.data.get("node")?;
        let id = node.get("id").and_then(|v| v.as_str())?.to_string();
        let node_type = node.get("node_type").and_then(|v| v.as_str())?;

        // Process action and workflow nodes
        if node_type != node_type_filter {
            return None;
        }

        let action_type = node.get("name").and_then(|v| v.as_str())?.to_string();
        let timestamp = node.get("timestamp").and_then(|v| v.as_f64())?;
        let status = node.get("status").and_then(|v| v.as_str())?.to_string();

        // Get both metadata and execution_record (execution_record has runtime data)
        let mut metadata = node.get("metadata").and_then(|m| m.as_object()).cloned().unwrap_or_default();

        // Merge execution_record into metadata if it exists
        if let Some(exec_record) = node.get("metadata")
            .and_then(|m| m.as_object())
            .and_then(|m| m.get("execution_record"))
            .and_then(|er| er.as_object()) {

            // Copy runtime data from execution_record.metadata.runtime (nested!)
            if let Some(exec_meta) = exec_record.get("metadata").and_then(|m| m.as_object()) {
                if let Some(runtime) = exec_meta.get("runtime") {
                    metadata.insert("runtime".to_string(), runtime.clone());
                }
            }

            // Copy nesting_level (directly in execution_record)
            if let Some(level) = exec_record.get("nesting_level") {
                metadata.insert("level".to_string(), level.clone());
            }
        }

        // Also check if nesting_level is directly on the node
        if let Some(level) = node.get("nesting_level") {
            metadata.insert("level".to_string(), level.clone());
        }

        let parent_id = node.get("parent_id").and_then(|p| p.as_str()).map(|s| s.to_string());
        let end_timestamp = node.get("end_timestamp").and_then(|t| t.as_f64());
        let duration = node.get("duration").and_then(|d| d.as_f64());
        let error = node.get("error").and_then(|e| e.as_str()).map(|s| s.to_string());

        // Check if this is from an inline workflow
        let is_inline_workflow = node.get("metadata")
            .and_then(|m| m.as_object())
            .and_then(|m| m.get("is_inline"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        Some(ActionInfo {
            id,
            action_type,
            timestamp,
            end_timestamp,
            duration,
            status,
            error,
            parent_id,
            metadata: if metadata.is_empty() { None } else { Some(metadata) },
            is_inline_workflow,
        })
    }
}

#[derive(Debug, Clone)]
struct ActionInfo {
    id: String,
    action_type: String,
    timestamp: f64,
    end_timestamp: Option<f64>,
    duration: Option<f64>,
    status: String,
    error: Option<String>,
    parent_id: Option<String>,
    metadata: Option<serde_json::Map<String, serde_json::Value>>,
    is_inline_workflow: bool,
}

/// View data for Action Log
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionLogViewData {
    /// List of actions to display
    pub actions: Vec<ActionLogEntry>,

    /// Workflow start time (for relative timestamps)
    pub workflow_start_time: Option<f64>,

    /// Total action count
    pub total_count: usize,

    /// Filtered action count
    pub visible_count: usize,
}

/// Single action entry in the log
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionLogEntry {
    /// Action ID
    pub id: String,

    /// Action type (FIND, CLICK, TYPE, etc.)
    pub action_type: String,

    /// Timestamp (Unix epoch)
    pub timestamp: f64,

    /// End timestamp (Unix epoch)
    pub end_timestamp: Option<f64>,

    /// Duration in seconds
    pub duration: Option<f64>,

    /// Status (pending, running, success, failed)
    pub status: String,

    /// Error message if failed
    pub error: Option<String>,

    /// Parent action ID (for hierarchy)
    pub parent_action_id: Option<String>,

    /// Whether this action can be expanded (has children)
    pub is_expandable: bool,

    /// Action metadata
    pub metadata: HashMap<String, serde_json::Value>,
}

impl DisplayProfile for ActionLogProfile {
    type Output = ActionLogViewData;

    fn process(&self, events: &[RawEvent]) -> ActionLogViewData {
        let mut actions_map: HashMap<String, ActionInfo> = HashMap::new();
        let mut workflow_parents: HashMap<String, String> = HashMap::new();  // workflow_id -> parent_id
        let mut workflow_start_time: Option<f64> = None;

        // First pass: Collect all action events
        for event in events {
            match event.event_type.as_str() {
                "transition_started" => {
                    // Add all transitions (they're always expandable containers)
                    if let Some(action_info) = self.extract_action_info(event, "transition") {
                        actions_map
                            .entry(action_info.id.clone())
                            .or_insert(action_info);
                    }
                }
                "transition_completed" | "transition_failed" => {
                    // Update transition with completion data
                    if let Some(action_info) = self.extract_action_info(event, "transition") {
                        actions_map
                            .entry(action_info.id.clone())
                            .and_modify(|existing| {
                                if action_info.end_timestamp.is_some() {
                                    existing.end_timestamp = action_info.end_timestamp;
                                }
                                if action_info.duration.is_some() {
                                    existing.duration = action_info.duration;
                                }
                                existing.status = action_info.status.clone();
                                if action_info.error.is_some() {
                                    existing.error = action_info.error.clone();
                                }
                            })
                            .or_insert(action_info);
                    }
                }
                "workflow_started" => {
                    // Workflows are containers, not actions - don't add them to the action log
                    // But track their parent relationships so we can reparent their children
                    if workflow_start_time.is_none() {
                        workflow_start_time = Some(event.timestamp);
                    }

                    // Extract workflow info to get its ID and parent_id
                    if let Some(workflow_info) = self.extract_action_info(event, "workflow") {
                        if let Some(parent_id) = workflow_info.parent_id {
                            // Track: workflow_id -> parent_id mapping
                            workflow_parents.insert(workflow_info.id.clone(), parent_id);
                        }
                    }
                }
                "workflow_completed" | "workflow_failed" => {
                    // Workflows are containers, not actions - ignore workflow completion events
                    // Only actions and transitions are tracked in this table
                }
                "action_started" | "action_completed" | "action_failed" => {
                    if let Some(action_info) = self.extract_action_info(event, "action") {
                        // Update existing entry or create new one
                        actions_map
                            .entry(action_info.id.clone())
                            .and_modify(|existing| {
                                // Update with completion/failure data
                                if action_info.end_timestamp.is_some() {
                                    existing.end_timestamp = action_info.end_timestamp;
                                }
                                if action_info.duration.is_some() {
                                    existing.duration = action_info.duration;
                                }
                                existing.status = action_info.status.clone();
                                if action_info.error.is_some() {
                                    existing.error = action_info.error.clone();
                                }
                                // Merge metadata
                                if let Some(new_meta) = &action_info.metadata {
                                    if let Some(existing_meta) = &mut existing.metadata {
                                        for (k, v) in new_meta {
                                            existing_meta.insert(k.clone(), v.clone());
                                        }
                                    } else {
                                        existing.metadata = Some(new_meta.clone());
                                    }
                                }
                            })
                            .or_insert(action_info);
                    }
                }
                _ => {}
            }
        }

        // Second pass: Fix parent references that point to workflow nodes
        // Since workflows are not in actions_map, we need to reparent actions to the
        // action/transition that contains the workflow
        let mut fixed_actions = actions_map.clone();
        for (action_id, action_info) in actions_map.iter() {
            if let Some(parent_id) = &action_info.parent_id {
                // Check if parent exists in actions_map
                if !actions_map.contains_key(parent_id) {
                    // Parent is a workflow node - use workflow_parents map to find real parent
                    if let Some(workflow_parent_id) = workflow_parents.get(parent_id) {
                        // Reparent to the workflow's parent (the action/transition that owns it)
                        if let Some(action_to_update) = fixed_actions.get_mut(action_id) {
                            action_to_update.parent_id = Some(workflow_parent_id.clone());
                        }
                    } else {
                        // Workflow has no parent (top-level) - make this action root level
                        if let Some(action_to_update) = fixed_actions.get_mut(action_id) {
                            action_to_update.parent_id = None;
                        }
                    }
                }
            }
        }

        // Recalculate nesting levels based on actual parent chain (after reparenting)
        // This is necessary because actions inherited their level from when they were under workflows
        fn calculate_level(action_id: &str, actions: &HashMap<String, ActionInfo>) -> i32 {
            let action = match actions.get(action_id) {
                Some(a) => a,
                None => return 0,
            };

            match &action.parent_id {
                Some(parent_id) => 1 + calculate_level(parent_id, actions),
                None => 0,
            }
        }

        // First calculate all levels (immutable borrow)
        let mut level_map: HashMap<String, i32> = HashMap::new();
        for (action_id, _) in fixed_actions.iter() {
            let correct_level = calculate_level(action_id, &fixed_actions);
            level_map.insert(action_id.clone(), correct_level);
        }

        // Then update metadata (mutable borrow)
        for (action_id, action_info) in fixed_actions.iter_mut() {
            if let Some(&correct_level) = level_map.get(action_id) {
                if let Some(metadata) = &mut action_info.metadata {
                    metadata.insert("level".to_string(), serde_json::Value::Number(correct_level.into()));
                }
            }
        }

        // Third pass: Convert to ActionLogEntry
        let mut actions: Vec<ActionLogEntry> = fixed_actions
            .values()
            .filter(|info| {
                // All actions and transitions are visible
                self.is_visible_action(&info.action_type, info.is_inline_workflow)
            })
            .map(|info| {
                let is_expandable = info.metadata
                    .as_ref()
                    .and_then(|m| m.get("is_expandable"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);

                let mut metadata: HashMap<String, serde_json::Value> = info.metadata
                    .as_ref()
                    .map(|m| {
                        m.iter()
                            .map(|(k, v)| (k.clone(), v.clone()))
                            .collect()
                    })
                    .unwrap_or_default();

                // Only transform helper workflows - all other action types stay as-is
                let is_helper_find_workflow = info.action_type.starts_with("wf-helper-find-any");

                let action_type = if info.action_type.starts_with("wf-helper-") && !is_helper_find_workflow {
                    // Only transform NON-find-any helper workflows to FIND actions
                    // Extract the state name from "wf-helper-StateName"
                    let state_name = info.action_type.strip_prefix("wf-helper-").unwrap_or(&info.action_type);

                    // Add the state name to metadata as the target
                    let mut config_map = serde_json::Map::new();
                    config_map.insert("target".to_string(), serde_json::Value::String(state_name.to_string()));
                    metadata.insert("config".to_string(), serde_json::Value::Object(config_map));

                    "FIND".to_string()
                } else if info.action_type == "FIND" {
                    // For real FIND actions, extract image name from config if available
                    // The config might have imageId or target fields
                    if let Some(config) = metadata.get("config").and_then(|c| c.as_object()) {
                        // Try to get a human-readable target name from config
                        // Check for 'target' field first (might already be set), then 'imageId'
                        if !config.contains_key("target") {
                            if let Some(image_id) = config.get("imageId").and_then(|v| v.as_str()) {
                                // Extract just the readable part from image IDs like "stateimage-123456-abc"
                                // For now, just use the imageId as-is; we could look up the actual name from state_map
                                let mut config_map = config.clone();
                                config_map.insert("target".to_string(), serde_json::Value::String(image_id.to_string()));
                                metadata.insert("config".to_string(), serde_json::Value::Object(config_map));
                            }
                        }
                    }
                    info.action_type.clone()
                } else {
                    info.action_type.clone()
                };

                ActionLogEntry {
                    id: info.id.clone(),
                    action_type,
                    timestamp: info.timestamp,
                    end_timestamp: info.end_timestamp,
                    duration: info.duration,
                    status: info.status.clone(),
                    error: info.error.clone(),
                    parent_action_id: info.parent_id.clone(),
                    is_expandable,
                    metadata,
                }
            })
            .collect();

        // Sort by timestamp
        actions.sort_by(|a, b| a.timestamp.partial_cmp(&b.timestamp).unwrap_or(std::cmp::Ordering::Equal));

        let visible_count = actions.len();
        let total_count = actions_map.len(); // Count unique actions, not events

        ActionLogViewData {
            actions,
            workflow_start_time,
            total_count,
            visible_count,
        }
    }

    fn name(&self) -> &str {
        "action_log"
    }

    fn view_type(&self) -> ViewType {
        ViewType::ActionLog
    }
}

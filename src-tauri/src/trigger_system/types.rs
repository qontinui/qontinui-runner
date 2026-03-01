//! Core types for the trigger system.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ============================================================================
// Trigger Config (type-discriminated)
// ============================================================================

/// Type-specific configuration for each trigger type.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TriggerConfig {
    /// External webhook ingestion (GitHub, CI/CD, etc.)
    Webhook {
        /// HMAC-SHA256 secret for signature verification
        #[serde(default)]
        secret: Option<String>,
        /// JSONPath filter on payload (must match for trigger to fire)
        #[serde(default)]
        payload_filter: Option<String>,
        /// Map trigger variables from webhook payload using JSONPath
        /// e.g., {"branch": "$.ref", "message": "$.head_commit.message"}
        #[serde(default)]
        variable_mapping: HashMap<String, String>,
    },

    /// Watch files/directories for changes
    FileWatch {
        /// Paths to watch
        paths: Vec<String>,
        /// Glob patterns to include (e.g., "*.rs", "*.ts")
        #[serde(default)]
        patterns: Vec<String>,
        /// Glob patterns to exclude
        #[serde(default)]
        ignore_patterns: Vec<String>,
        /// Whether to watch recursively
        #[serde(default = "default_true")]
        recursive: bool,
    },

    /// Trigger when another workflow completes
    WorkflowChain {
        /// Source workflow ID to watch
        source_workflow_id: String,
        /// What status to trigger on: "completed", "failed", "any"
        #[serde(default = "default_completed")]
        on_status: String,
        /// Pass source workflow output as context to triggered workflow
        #[serde(default)]
        pass_context: bool,
    },

    /// Trigger on git events (commits, branch switches, tags)
    GitEvent {
        /// Path to the git repository
        repo_path: String,
        /// Events to watch: "commit", "branch_switch", "tag"
        events: Vec<String>,
        /// Regex filter for branch names
        #[serde(default)]
        branch_filter: Option<String>,
    },

    /// Continuous health check monitoring
    HealthCheck {
        /// URLs to monitor
        urls: Vec<HealthCheckTarget>,
        /// How often to check (seconds)
        #[serde(default = "default_check_interval")]
        check_interval_seconds: u64,
        /// How many consecutive failures before triggering
        #[serde(default = "default_consecutive_failures")]
        consecutive_failures: u32,
    },
}

/// A URL target for health checking.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthCheckTarget {
    /// URL to check
    pub url: String,
    /// Expected HTTP status code (default: 200)
    #[serde(default = "default_status_code")]
    pub expected_status: u16,
    /// Request timeout in seconds
    #[serde(default = "default_health_timeout")]
    pub timeout_seconds: u64,
}

fn default_true() -> bool {
    true
}

fn default_completed() -> String {
    "completed".to_string()
}

fn default_check_interval() -> u64 {
    30
}

fn default_consecutive_failures() -> u32 {
    3
}

fn default_status_code() -> u16 {
    200
}

fn default_health_timeout() -> u64 {
    10
}

// ============================================================================
// Trigger Definition
// ============================================================================

/// A workflow trigger definition stored in the database.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowTrigger {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub trigger_type: String,
    pub trigger_config: TriggerConfig,
    pub workflow_id: String,
    /// Optional JSON overrides for the workflow (max_iterations, model, etc.)
    #[serde(default)]
    pub workflow_overrides: Option<serde_json::Value>,
    /// Conditions that must be met before firing
    #[serde(default)]
    pub conditions: Vec<TriggerCondition>,
    /// Debounce window in milliseconds (coalesce rapid events)
    #[serde(default = "default_debounce_ms")]
    pub debounce_ms: u64,
    /// Minimum seconds between trigger executions
    #[serde(default = "default_cooldown")]
    pub cooldown_seconds: u64,
    /// Maximum concurrent workflow executions from this trigger
    #[serde(default = "default_max_concurrent")]
    pub max_concurrent: u32,
    pub enabled: bool,
    pub last_triggered_at: Option<String>,
    pub last_execution_id: Option<String>,
    #[serde(default)]
    pub trigger_count: u64,
    pub created_at: String,
    pub updated_at: String,
}

fn default_debounce_ms() -> u64 {
    1000
}

fn default_cooldown() -> u64 {
    60
}

fn default_max_concurrent() -> u32 {
    1
}

/// A condition that must be met before a trigger fires.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TriggerCondition {
    /// Condition type: "require_idle", "branch_filter", "custom"
    #[serde(rename = "type")]
    pub condition_type: String,
    /// Condition-specific configuration
    pub config: serde_json::Value,
}

// ============================================================================
// Trigger Events
// ============================================================================

/// An event sent to the TriggerService for evaluation.
#[derive(Debug, Clone)]
pub struct TriggerEvent {
    /// ID of the trigger that should be evaluated
    pub trigger_id: String,
    /// Type of event
    pub event_type: String,
    /// Event payload data
    pub event_data: serde_json::Value,
    /// Variables extracted from the event (for template substitution)
    pub variables: HashMap<String, String>,
    /// Chain depth (for circular chain prevention)
    pub chain_depth: u32,
}

// ============================================================================
// Trigger History
// ============================================================================

/// A record of a trigger evaluation/execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TriggerHistoryEntry {
    pub id: String,
    pub trigger_id: String,
    pub event_type: String,
    pub event_data: serde_json::Value,
    /// What happened: "executed", "debounced", "throttled", "condition_failed", "error"
    pub action: String,
    /// Task run ID if a workflow was spawned
    pub task_run_id: Option<String>,
    /// Error message if something went wrong
    pub error_message: Option<String>,
    pub triggered_at: String,
}

// ============================================================================
// API Request/Response Types
// ============================================================================

/// Request to create a new trigger.
#[derive(Debug, Deserialize)]
pub struct CreateTriggerRequest {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub trigger_config: TriggerConfig,
    pub workflow_id: String,
    #[serde(default)]
    pub workflow_overrides: Option<serde_json::Value>,
    #[serde(default)]
    pub conditions: Vec<TriggerCondition>,
    #[serde(default = "default_debounce_ms")]
    pub debounce_ms: u64,
    #[serde(default = "default_cooldown")]
    pub cooldown_seconds: u64,
    #[serde(default = "default_max_concurrent")]
    pub max_concurrent: u32,
}

/// Request to update an existing trigger.
#[derive(Debug, Deserialize)]
pub struct UpdateTriggerRequest {
    pub name: Option<String>,
    pub description: Option<Option<String>>,
    pub trigger_config: Option<TriggerConfig>,
    pub workflow_id: Option<String>,
    pub workflow_overrides: Option<Option<serde_json::Value>>,
    pub conditions: Option<Vec<TriggerCondition>>,
    pub debounce_ms: Option<u64>,
    pub cooldown_seconds: Option<u64>,
    pub max_concurrent: Option<u32>,
}

/// Request to enable/disable a trigger.
#[derive(Debug, Deserialize)]
pub struct SetEnabledRequest {
    pub enabled: bool,
}

/// Overall trigger system status.
#[derive(Debug, Serialize)]
pub struct TriggerSystemStatus {
    pub running: bool,
    pub total_triggers: u64,
    pub enabled_triggers: u64,
    pub active_watchers: u64,
    pub active_executions: u64,
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_trigger_config_serialization_roundtrip() {
        let config = TriggerConfig::Webhook {
            secret: Some("my-secret".to_string()),
            payload_filter: None,
            variable_mapping: {
                let mut m = HashMap::new();
                m.insert("branch".to_string(), "$.ref".to_string());
                m
            },
        };

        let json = serde_json::to_string(&config).unwrap();
        let deserialized: TriggerConfig = serde_json::from_str(&json).unwrap();

        match deserialized {
            TriggerConfig::Webhook {
                secret,
                variable_mapping,
                ..
            } => {
                assert_eq!(secret, Some("my-secret".to_string()));
                assert_eq!(variable_mapping.get("branch").unwrap(), "$.ref");
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_file_watch_config() {
        let config = TriggerConfig::FileWatch {
            paths: vec!["/src".to_string()],
            patterns: vec!["*.rs".to_string()],
            ignore_patterns: vec!["target/*".to_string()],
            recursive: true,
        };

        let json = serde_json::to_string(&config).unwrap();
        assert!(json.contains("file_watch"));
        let _: TriggerConfig = serde_json::from_str(&json).unwrap();
    }

    #[test]
    fn test_workflow_chain_config() {
        let config = TriggerConfig::WorkflowChain {
            source_workflow_id: "wf-123".to_string(),
            on_status: "completed".to_string(),
            pass_context: true,
        };

        let json = serde_json::to_string(&config).unwrap();
        let deserialized: TriggerConfig = serde_json::from_str(&json).unwrap();

        match deserialized {
            TriggerConfig::WorkflowChain {
                source_workflow_id,
                on_status,
                pass_context,
            } => {
                assert_eq!(source_workflow_id, "wf-123");
                assert_eq!(on_status, "completed");
                assert!(pass_context);
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_health_check_config_defaults() {
        let json = r#"{
            "type": "health_check",
            "urls": [{"url": "http://localhost:3000"}]
        }"#;

        let config: TriggerConfig = serde_json::from_str(json).unwrap();
        match config {
            TriggerConfig::HealthCheck {
                check_interval_seconds,
                consecutive_failures,
                ..
            } => {
                assert_eq!(check_interval_seconds, 30);
                assert_eq!(consecutive_failures, 3);
            }
            _ => panic!("Wrong variant"),
        }
    }
}

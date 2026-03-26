//! Type definitions for the database module.
//!
//! Contains all structs, enums, constants, and helper functions
//! used across the database module.

use serde::{Deserialize, Serialize};

/// Result of database optimization operations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatabaseOptimizeResult {
    /// Time taken for optimization in milliseconds
    pub duration_ms: u64,
    /// Database size before optimization in bytes
    pub size_before_bytes: i64,
    /// Database size after optimization in bytes
    pub size_after_bytes: i64,
    /// Space reclaimed by VACUUM in bytes
    pub space_reclaimed_bytes: i64,
    /// Whether integrity check passed (if run)
    pub integrity_check_passed: bool,
}

/// Database statistics for monitoring.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatabaseStats {
    /// Total database size in bytes
    pub total_size_bytes: i64,
    /// Number of pages in database
    pub page_count: i64,
    /// Size of each page in bytes
    pub page_size: i64,
    /// Number of free/unused pages
    pub freelist_count: i64,
    /// Number of pages in WAL file
    pub wal_pages: i64,
    /// Number of frames in WAL file
    pub wal_frames: i64,
    /// Row counts per table
    pub table_counts: Vec<TableRowCount>,
}

/// Row count for a single table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableRowCount {
    pub table_name: String,
    pub row_count: i64,
}

/// Checkpoint data structure for API compatibility.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointData {
    /// Session ID (for session-based checkpoints)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Workflow name (for workflow-based checkpoints like "improve-all")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workflow_name: Option<String>,
    /// Current phase number
    pub current_phase: u32,
    /// Total phases (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_phases: Option<u32>,
    /// Whether the workflow/session is complete
    #[serde(default)]
    pub completed: bool,
    /// Whether restart is permitted
    #[serde(default)]
    pub restart_permitted: bool,
    /// Session status
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// Run ID for grouping sessions
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    /// Repos to process (for improve-all)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repos_to_process: Option<Vec<String>>,
    /// Work completed per phase
    #[serde(skip_serializing_if = "Option::is_none")]
    pub work_completed: Option<serde_json::Value>,
    /// Items needing user input
    #[serde(skip_serializing_if = "Option::is_none")]
    pub items_needing_user_input: Option<Vec<String>>,
    /// Created timestamp
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    /// Updated timestamp
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    /// Error message if any
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    /// Extra custom data
    #[serde(flatten)]
    pub extra: Option<serde_json::Value>,
}

/// Session event for history tracking.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionEvent {
    pub id: i64,
    pub session_id: String,
    pub event_type: String,
    pub message: String,
    pub timestamp: String,
    pub data: Option<serde_json::Value>,
}

/// Cached spec for an external app discovered via UI Bridge SDK.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedAppSpec {
    pub id: String,
    pub app_url: String,
    pub app_name: String,
    pub spec_id: String,
    pub spec_json: String,
    pub discovered_at: String,
    pub page_url: Option<String>,
}

/// Lightweight summary of an AI session for sidebar listing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiSessionSummary {
    pub id: String,
    pub task_name: String,
    pub status: String,
    pub updated_at: String,
    pub created_at: String,
}

/// Task run data structure for the unified task model.
/// TaskRun is THE single concept for all runs (AI, automation, or mixed).
/// Every task runs until [TASK_COMPLETE] is found in output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRun {
    pub id: String,
    pub task_name: String,
    /// Task prompt (NULL for pure automation tasks)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,

    /// Task type: 'task' (default), 'automation', 'scheduled'
    #[serde(default = "default_task_type")]
    pub task_type: String,

    /// Status: 'running', 'complete', 'failed', 'stopped'
    pub status: String,

    /// Number of AI sessions spawned
    pub sessions_count: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_sessions: Option<u32>,

    /// Per-run auto-continue setting (defaults to true)
    #[serde(default = "default_auto_continue")]
    pub auto_continue: bool,

    /// Accumulated output with [SESSION_START:N] markers
    pub output_log: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,

    /// Execution steps JSON (for re-execution on resume)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution_steps_json: Option<String>,
    /// Log sources JSON (for capturing logs during execution)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub log_sources_json: Option<String>,

    /// Config linkage (for automation-enabled tasks)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config_id: Option<String>,
    /// Workflow name being executed
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workflow_name: Option<String>,

    /// Unified workflow ID that this task run executes (FK to unified_workflows).
    /// Set when executing a generated or saved workflow so the Recap page can
    /// link runs to their workflow definition.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workflow_id: Option<String>,

    /// AI-generated paragraph summary of the task run
    /// Note: This is the canonical field. `ai_summary` is kept for backward compatibility.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// Backward compatibility alias for summary
    #[serde(skip_serializing_if = "Option::is_none", rename = "ai_summary")]
    pub ai_summary: Option<String>,
    /// Whether the stated goal was achieved (determined by AI after completion)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub goal_achieved: Option<bool>,
    /// What remains to be done if goal was not achieved
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remaining_work: Option<String>,
    /// Timestamp when the summary was generated
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary_generated_at: Option<String>,

    /// JSON array of StateTransition objects from orchestrator (for stage-based recap)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transition_history_json: Option<String>,

    /// Workflow type: 'unified', 'legacy_session', 'automation_only', or NULL (legacy)
    /// Unified workflows should only have status modified by the LoopController.
    /// This prevents TaskMonitor and legacy session code from interfering with
    /// the verification-agentic loop.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workflow_type: Option<String>,

    // ========================================================================
    // Web Backend Context (for tasks created via API)
    // ========================================================================
    /// Workspace/organization ID (from web backend when task is created via API).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,

    /// User ID who triggered the task (from web backend when task is created via API).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub triggered_by: Option<String>,

    // ========================================================================
    // Hierarchy Fields (for nested task runs / subtasks)
    // ========================================================================
    /// Parent task run ID (for subtasks spawned by a parent task).
    /// NULL for root-level tasks.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_task_run_id: Option<String>,

    /// Root task run ID (top of the task hierarchy).
    /// Same as `id` for root-level tasks.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root_task_run_id: Option<String>,

    /// Nesting depth (0 = root/top-level task).
    #[serde(default)]
    pub depth: u32,

    // ========================================================================
    // Multi-Bridge Support (for concurrent execution)
    // ========================================================================
    /// Bridge ID handling this task (for multi-bridge scenarios).
    /// NULL for legacy single-bridge tasks.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bridge_id: Option<String>,

    /// Structured result data (JSON) — used by meta-workflows to store outputs
    /// like generated_workflow_id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result_data: Option<String>,

    // ========================================================================
    // Reflection Fields
    // ========================================================================
    /// Whether this task run is a reflection analysis run.
    #[serde(default)]
    pub is_reflection: bool,

    /// The source task run ID that this reflection analyzes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reflection_source_task_run_id: Option<String>,

    // ========================================================================
    // Follow-Up Fields
    // ========================================================================
    /// Whether this task run is a follow-up run for unfixed issues.
    #[serde(default)]
    pub is_follow_up: bool,

    /// The source task run ID whose unfixed issues this run addresses.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub follow_up_source_task_run_id: Option<String>,

    // ========================================================================
    // Fixer Fields
    // ========================================================================
    /// Whether this task run is a fixer run (aggregates reflection/follow-up fixes).
    #[serde(default)]
    pub is_fixer: bool,

    /// The source task run ID that this fixer addresses.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fixer_source_task_run_id: Option<String>,

    // ========================================================================
    // Meta-Optimizer Fields
    // ========================================================================
    /// Whether this task run is a meta-optimizer run.
    #[serde(default)]
    pub is_meta_optimizer: bool,

    pub created_at: String,
    pub updated_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
}

fn default_task_type() -> String {
    "task".to_string()
}

fn default_auto_continue() -> bool {
    true
}

/// Input for creating a task run.
/// Use the builder pattern to construct this:
///
/// ```rust
/// use crate::database::CreateTaskRunInput;
///
/// let input = CreateTaskRunInput::new("task-123", "My Task")
///     .with_prompt("Do something useful")
///     .with_config_id("config-456")
///     .with_workflow_type("unified");
/// ```
#[derive(Debug, Clone, Default)]
pub struct CreateTaskRunInput {
    pub id: String,
    pub task_name: String,
    pub prompt: Option<String>,
    pub task_type: Option<String>,
    pub config_id: Option<String>,
    pub workflow_name: Option<String>,
    pub workflow_id: Option<String>,
    pub max_sessions: Option<u32>,
    pub auto_continue: Option<bool>,
    pub execution_steps_json: Option<String>,
    pub log_sources_json: Option<String>,
    pub workflow_type: Option<String>,
    pub parent_task_run_id: Option<String>,
    pub root_task_run_id: Option<String>,
    pub depth: u32,
    pub workspace_id: Option<String>,
    pub triggered_by: Option<String>,
    pub bridge_id: Option<String>,
    pub is_reflection: bool,
    pub reflection_source_task_run_id: Option<String>,
    pub is_follow_up: bool,
    pub follow_up_source_task_run_id: Option<String>,
    pub is_fixer: bool,
    pub fixer_source_task_run_id: Option<String>,
    pub is_meta_optimizer: bool,
    pub runner_port: Option<u16>,
}

impl CreateTaskRunInput {
    /// Create a new task run input with required fields.
    pub fn new(id: impl Into<String>, task_name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            task_name: task_name.into(),
            prompt: None,
            task_type: None,
            config_id: None,
            workflow_name: None,
            workflow_id: None,
            max_sessions: None,
            auto_continue: None,
            execution_steps_json: None,
            log_sources_json: None,
            workflow_type: None,
            parent_task_run_id: None,
            root_task_run_id: None,
            depth: 0,
            workspace_id: None,
            triggered_by: None,
            bridge_id: None,
            is_reflection: false,
            reflection_source_task_run_id: None,
            is_follow_up: false,
            follow_up_source_task_run_id: None,
            is_fixer: false,
            fixer_source_task_run_id: None,
            is_meta_optimizer: false,
            runner_port: None,
        }
    }

    /// Set the task prompt.
    pub fn with_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.prompt = Some(prompt.into());
        self
    }

    /// Set the task type (e.g., "task", "automation", "scheduled").
    pub fn with_task_type(mut self, task_type: impl Into<String>) -> Self {
        self.task_type = Some(task_type.into());
        self
    }

    /// Set the config ID for automation-enabled tasks.
    pub fn with_config_id(mut self, config_id: impl Into<String>) -> Self {
        self.config_id = Some(config_id.into());
        self
    }

    /// Set the workflow name being executed.
    pub fn with_workflow_name(mut self, workflow_name: impl Into<String>) -> Self {
        self.workflow_name = Some(workflow_name.into());
        self
    }

    /// Set the unified workflow ID being executed.
    pub fn with_workflow_id(mut self, workflow_id: impl Into<String>) -> Self {
        self.workflow_id = Some(workflow_id.into());
        self
    }

    /// Set the maximum number of AI sessions.
    pub fn with_max_sessions(mut self, max_sessions: u32) -> Self {
        self.max_sessions = Some(max_sessions);
        self
    }

    /// Set the auto-continue behavior.
    pub fn with_auto_continue(mut self, auto_continue: bool) -> Self {
        self.auto_continue = Some(auto_continue);
        self
    }

    /// Set the execution steps JSON for re-execution on resume.
    pub fn with_execution_steps_json(mut self, json: impl Into<String>) -> Self {
        self.execution_steps_json = Some(json.into());
        self
    }

    /// Set the log sources JSON for capturing logs during execution.
    pub fn with_log_sources_json(mut self, json: impl Into<String>) -> Self {
        self.log_sources_json = Some(json.into());
        self
    }

    /// Set the workflow type ("unified", "legacy_session", "automation_only").
    pub fn with_workflow_type(mut self, workflow_type: impl Into<String>) -> Self {
        self.workflow_type = Some(workflow_type.into());
        self
    }

    /// Set the parent task run ID for subtasks.
    pub fn with_parent_task_run_id(mut self, parent_id: impl Into<String>) -> Self {
        self.parent_task_run_id = Some(parent_id.into());
        self
    }

    /// Set the root task run ID for the task hierarchy.
    pub fn with_root_task_run_id(mut self, root_id: impl Into<String>) -> Self {
        self.root_task_run_id = Some(root_id.into());
        self
    }

    /// Set the nesting depth (0 = root/top-level task).
    pub fn with_depth(mut self, depth: u32) -> Self {
        self.depth = depth;
        self
    }

    /// Set the workspace/organization ID from the web backend.
    pub fn with_workspace_id(mut self, workspace_id: impl Into<String>) -> Self {
        self.workspace_id = Some(workspace_id.into());
        self
    }

    /// Set the user ID who triggered the task.
    pub fn with_triggered_by(mut self, triggered_by: impl Into<String>) -> Self {
        self.triggered_by = Some(triggered_by.into());
        self
    }

    /// Set the bridge ID for multi-bridge scenarios.
    pub fn with_bridge_id(mut self, bridge_id: impl Into<String>) -> Self {
        self.bridge_id = Some(bridge_id.into());
        self
    }

    /// Mark this task run as a reflection run.
    pub fn with_is_reflection(mut self, is_reflection: bool) -> Self {
        self.is_reflection = is_reflection;
        self
    }

    /// Set the source task run ID that this reflection analyzes.
    pub fn with_reflection_source_task_run_id(mut self, source_id: impl Into<String>) -> Self {
        self.reflection_source_task_run_id = Some(source_id.into());
        self
    }

    /// Mark this task run as a follow-up run.
    pub fn with_is_follow_up(mut self, is_follow_up: bool) -> Self {
        self.is_follow_up = is_follow_up;
        self
    }

    /// Set the source task run ID whose unfixed issues this run addresses.
    pub fn with_follow_up_source_task_run_id(mut self, source_id: impl Into<String>) -> Self {
        self.follow_up_source_task_run_id = Some(source_id.into());
        self
    }

    /// Mark this task run as a fixer run.
    pub fn with_is_fixer(mut self, is_fixer: bool) -> Self {
        self.is_fixer = is_fixer;
        self
    }

    /// Set the source task run ID that this fixer addresses.
    pub fn with_fixer_source_task_run_id(mut self, source_id: impl Into<String>) -> Self {
        self.fixer_source_task_run_id = Some(source_id.into());
        self
    }

    /// Mark this task run as a meta-optimizer run.
    pub fn with_is_meta_optimizer(mut self, is_meta_optimizer: bool) -> Self {
        self.is_meta_optimizer = is_meta_optimizer;
        self
    }

    /// Set the runner API port that created this task run.
    pub fn with_runner_port(mut self, port: u16) -> Self {
        self.runner_port = Some(port);
        self
    }
}

/// Stored config entry metadata (without the full JSON).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigStorageEntry {
    pub id: String,
    pub name: String,
    pub source_type: String, // 'web' or 'file'
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_path: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// TaskRunAutomation - Child record for automation metrics within a TaskRun.
/// Stores automation execution data within a task run.
/// Some runs have ONLY automation, some have ONLY AI, some have BOTH.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRunAutomation {
    pub id: String,
    pub task_run_id: String,

    /// Workflow details
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workflow_name: Option<String>,
    pub started_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<i64>,

    /// Status: 'running', 'success', 'failed', 'timeout', 'cancelled'
    pub automation_status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub success: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,

    /// Metrics (JSON strings for flexibility)
    /// JSON {\"total\": N, \"success\": N, \"failed\": N, \"skipped\": N}
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actions_summary: Option<String>,
    /// JSON array of state names
    #[serde(skip_serializing_if = "Option::is_none")]
    pub states_visited: Option<String>,
    /// JSON array of {from, to, action, success, duration_ms}
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transitions_executed: Option<String>,
    /// JSON array of {template, count, avg_confidence, failures}
    #[serde(skip_serializing_if = "Option::is_none")]
    pub template_matches: Option<String>,
    /// JSON array for anomaly detection
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anomalies: Option<String>,

    /// Iteration tracking (1-indexed)
    pub iteration_number: u32,
}

// =============================================================================
// Hybrid Logging Structs (Phase 10)
// =============================================================================

/// Task run event for unified event logging.
/// Replaces JSONL files for historical queries while JSONL remains for real-time streaming.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRunEvent {
    pub id: i64,
    pub task_run_id: String,

    /// Event type: 'general', 'action', 'image_recognition', 'state_change', 'ai_output'
    pub event_type: String,
    /// Event subtype: 'start', 'complete', 'error', 'match', 'transition', etc.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_subtype: Option<String>,

    /// Human-readable message
    pub message: String,
    /// JSON payload with event-specific data
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,

    /// Context
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workflow_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action_id: Option<String>,

    /// Timing
    pub timestamp: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<i64>,
}

/// Input for creating a task run event.
#[derive(Debug, Clone)]
pub struct CreateTaskRunEventInput {
    pub task_run_id: String,
    pub event_type: String,
    pub event_subtype: Option<String>,
    pub message: String,
    pub data: Option<String>,
    pub workflow_name: Option<String>,
    pub state_name: Option<String>,
    pub action_id: Option<String>,
    pub timestamp: String,
    pub duration_ms: Option<i64>,
}

/// Task run screenshot for image recognition results.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRunScreenshot {
    pub id: String,
    pub task_run_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_id: Option<i64>,

    /// Path to PNG file in .dev-logs/screenshots/
    pub file_path: String,
    /// Screenshot type: 'annotated', 'raw', 'diff', 'failure'
    pub screenshot_type: String,

    /// Context from image recognition
    #[serde(skip_serializing_if = "Option::is_none")]
    pub template_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
    /// JSON {x, y, width, height}
    #[serde(skip_serializing_if = "Option::is_none")]
    pub match_location: Option<String>,

    /// Metadata
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_size_bytes: Option<i64>,

    pub created_at: String,
}

/// Input for creating a task run screenshot record.
#[derive(Debug, Clone)]
pub struct CreateTaskRunScreenshotInput {
    pub task_run_id: String,
    pub event_id: Option<i64>,
    pub file_path: String,
    pub screenshot_type: String,
    pub template_name: Option<String>,
    pub confidence: Option<f64>,
    pub match_location: Option<String>,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub file_size_bytes: Option<i64>,
}

/// Task run Playwright result for test execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRunPlaywrightResult {
    pub id: String,
    pub task_run_id: String,

    /// Test identification
    pub test_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spec_file: Option<String>,

    /// Status: 'passed', 'failed', 'skipped', 'timeout'
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<i64>,

    /// Output
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stdout: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stderr: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub console_output: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_snapshot: Option<String>,

    /// Failure details
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_screenshot_path: Option<String>,

    /// Assertion summary
    pub assertions_passed: i32,
    pub assertions_failed: i32,

    pub created_at: String,
}

/// Input for creating a Playwright result.
#[derive(Debug, Clone)]
pub struct CreateTaskRunPlaywrightResultInput {
    pub task_run_id: String,
    pub test_name: String,
    pub spec_file: Option<String>,
    pub status: String,
    pub duration_ms: Option<i64>,
    pub stdout: Option<String>,
    pub stderr: Option<String>,
    pub console_output: Option<String>,
    pub page_snapshot: Option<String>,
    pub error_message: Option<String>,
    pub failure_screenshot_path: Option<String>,
    pub assertions_passed: i32,
    pub assertions_failed: i32,
}

/// Task run API request from SQLite database.
/// Stores API request execution results migrated from JSONL logs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRunApiRequest {
    pub id: String,
    pub task_run_id: String,

    /// Step identification
    pub step_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step_name: Option<String>,

    /// Request details
    pub method: String,
    pub url: String,
    pub resolved_url: String,
    /// JSON object {header: value}
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_headers: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_body: Option<String>,

    /// Response details
    pub status_code: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_text: Option<String>,
    /// JSON object {header: value}
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_headers: Option<String>,
    pub response_time_ms: i64,

    /// Response body handling
    pub response_body_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_body: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_size_bytes: Option<i64>,

    /// JSON array of extraction results
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extractions: Option<String>,
    /// JSON array of assertion results
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assertions: Option<String>,

    /// Overall result
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,

    pub created_at: String,
}

/// Input for creating a task run API request record.
#[derive(Debug, Clone)]
pub struct CreateTaskRunApiRequestInput {
    pub task_run_id: String,
    pub step_id: String,
    pub step_name: Option<String>,
    pub method: String,
    pub url: String,
    pub resolved_url: String,
    pub request_headers: Option<String>,
    pub request_body: Option<String>,
    pub status_code: i32,
    pub status_text: Option<String>,
    pub response_headers: Option<String>,
    pub response_time_ms: i64,
    pub response_body_type: String,
    pub response_body: Option<String>,
    pub response_size_bytes: Option<i64>,
    pub extractions: Option<String>,
    pub assertions: Option<String>,
    pub success: bool,
    pub error_message: Option<String>,
    pub timestamp: String,
}

/// Task run AWAS step from SQLite database.
/// Stores AWAS (Automated Web Agent System) step execution results.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRunAwasStep {
    pub id: String,
    pub task_run_id: String,

    /// Step identification
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step_name: Option<String>,
    /// Step type: 'awas_discover', 'awas_execute', 'awas_check_support', 'awas_list_actions', 'awas_extract_elements'
    pub step_type: String,

    /// Context
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,

    /// Execution parameters
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action_id: Option<String>,
    /// JSON: step-specific parameters
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parameters: Option<String>,

    /// Response data (JSON: contains manifest, actions, elements, etc. depending on step_type)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_data: Option<String>,

    /// Results
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<i64>,

    pub created_at: String,
}

/// Input for creating a task run AWAS step record.
#[derive(Debug, Clone)]
pub struct CreateTaskRunAwasStepInput {
    pub task_run_id: String,
    pub step_id: Option<String>,
    pub step_name: Option<String>,
    pub step_type: String,
    pub url: Option<String>,
    pub action_id: Option<String>,
    pub parameters: Option<String>,
    pub response_data: Option<String>,
    pub success: bool,
    pub error_message: Option<String>,
    pub duration_ms: Option<i64>,
    pub timestamp: String,
}

/// Execution span from the tracing system.
/// Used for querying performance data from the execution_spans table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionSpan {
    pub id: i64,
    /// Task/execution ID (may be empty for global spans)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution_id: Option<String>,
    /// Shared trace ID across related spans
    pub trace_id: String,
    /// Unique span ID
    pub span_id: String,
    /// Parent span ID (None for root spans)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_span_id: Option<String>,
    /// Span name (e.g., "workflow.execute", "ai.session")
    pub name: String,
    /// Start timestamp (ISO 8601)
    pub start_ts: String,
    /// End timestamp (ISO 8601)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_ts: Option<String>,
    /// Duration in milliseconds
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<i64>,
    /// JSON object of span attributes
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attributes: Option<String>,
    /// Whether the span completed successfully
    pub success: bool,
    /// Error message if failed
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub created_at: String,
}

/// Test type enum for verification tests.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum TestType {
    PlaywrightCdp,
    QontinuiVision,
    PythonScript,
    RepositoryTest,
}

impl std::fmt::Display for TestType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TestType::PlaywrightCdp => write!(f, "playwright_cdp"),
            TestType::QontinuiVision => write!(f, "qontinui_vision"),
            TestType::PythonScript => write!(f, "python_script"),
            TestType::RepositoryTest => write!(f, "repository_test"),
        }
    }
}

impl std::str::FromStr for TestType {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "playwright_cdp" => Ok(TestType::PlaywrightCdp),
            "qontinui_vision" => Ok(TestType::QontinuiVision),
            "python_script" => Ok(TestType::PythonScript),
            "repository_test" => Ok(TestType::RepositoryTest),
            _ => Err(format!("Unknown test type: {}", s)),
        }
    }
}

/// Test result status enum.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum TestResultStatus {
    Pending,
    Running,
    Passed,
    Failed,
    Skipped,
    Error,
    Timeout,
}

impl std::fmt::Display for TestResultStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TestResultStatus::Pending => write!(f, "pending"),
            TestResultStatus::Running => write!(f, "running"),
            TestResultStatus::Passed => write!(f, "passed"),
            TestResultStatus::Failed => write!(f, "failed"),
            TestResultStatus::Skipped => write!(f, "skipped"),
            TestResultStatus::Error => write!(f, "error"),
            TestResultStatus::Timeout => write!(f, "timeout"),
        }
    }
}

impl std::str::FromStr for TestResultStatus {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "pending" => Ok(TestResultStatus::Pending),
            "running" => Ok(TestResultStatus::Running),
            "passed" => Ok(TestResultStatus::Passed),
            "failed" => Ok(TestResultStatus::Failed),
            "skipped" => Ok(TestResultStatus::Skipped),
            "error" => Ok(TestResultStatus::Error),
            "timeout" => Ok(TestResultStatus::Timeout),
            _ => Err(format!("Unknown test result status: {}", s)),
        }
    }
}

/// Trigger point for test associations.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum TriggerPoint {
    BeforeWorkflow,
    AfterWorkflow,
    OnAction,
    Manual,
}

impl std::fmt::Display for TriggerPoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TriggerPoint::BeforeWorkflow => write!(f, "before_workflow"),
            TriggerPoint::AfterWorkflow => write!(f, "after_workflow"),
            TriggerPoint::OnAction => write!(f, "on_action"),
            TriggerPoint::Manual => write!(f, "manual"),
        }
    }
}

impl std::str::FromStr for TriggerPoint {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "before_workflow" => Ok(TriggerPoint::BeforeWorkflow),
            "after_workflow" => Ok(TriggerPoint::AfterWorkflow),
            "on_action" => Ok(TriggerPoint::OnAction),
            "manual" => Ok(TriggerPoint::Manual),
            _ => Err(format!("Unknown trigger point: {}", s)),
        }
    }
}

/// Verification test definition stored in the runner's database.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationTest {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// Test type: playwright_cdp, qontinui_vision, python_script, repository_test
    pub test_type: TestType,

    /// Category for organization: visual, dom, network, data, log, layout, unit, integration, custom
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,

    /// TypeScript code for playwright_cdp tests
    #[serde(skip_serializing_if = "Option::is_none")]
    pub playwright_code: Option<String>,
    /// JSON config for qontinui_vision tests
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vision_config: Option<serde_json::Value>,
    /// Python code for python_script tests
    #[serde(skip_serializing_if = "Option::is_none")]
    pub python_code: Option<String>,
    /// JSON config for repository_test (command, working_dir, etc.)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo_test_config: Option<serde_json::Value>,

    /// Natural language description for AI generation
    #[serde(skip_serializing_if = "Option::is_none")]
    pub success_criteria: Option<String>,

    /// Test configuration (timeout_seconds, cdp_port, env_vars, etc.)
    #[serde(default)]
    pub config: serde_json::Value,

    /// Timeout in seconds (None = disabled by default, no timeout)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<u32>,
    pub is_critical: bool,
    pub enabled: bool,

    /// AI generation tracking
    #[serde(default)]
    pub ai_generated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ai_generation_prompt: Option<String>,

    /// Page analysis captured during test creation (for AI debugging context)
    /// Contains screenshot, annotated_screenshot, detected elements, etc.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub creation_analysis: Option<serde_json::Value>,

    /// Tags for organization (JSON array stored as Vec)
    #[serde(default)]
    pub tags: Vec<String>,

    /// Source file path if imported
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_exported_at: Option<String>,

    pub created_at: String,
    pub updated_at: String,
}

/// Test result from executing a verification test.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestResult {
    pub id: String,
    pub test_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_run_id: Option<String>,

    pub status: TestResultStatus,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<i64>,

    /// Combined stdout/stderr output
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    /// Parsed assertions, metrics, coverage as JSON
    #[serde(skip_serializing_if = "Option::is_none")]
    pub structured_output: Option<serde_json::Value>,

    pub assertions_passed: u32,
    pub assertions_failed: u32,

    /// Screenshot paths as JSON array
    #[serde(default)]
    pub screenshots: Vec<String>,

    /// Visual evidence with annotated screenshots and assertion overlays
    #[serde(skip_serializing_if = "Option::is_none")]
    pub visual_evidence: Option<serde_json::Value>,

    /// AI analysis of the result
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ai_analysis: Option<String>,

    pub created_at: String,
}

/// Association between tests and configs/workflows.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestAssociation {
    pub id: String,
    pub test_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workflow_name: Option<String>,

    pub trigger_point: TriggerPoint,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action_id: Option<String>,

    pub execution_order: i32,
    pub enabled: bool,

    pub created_at: String,
    pub updated_at: String,
}

// ============================================================================
// Orchestrator Types (Verification Plans, Task Knowledge, Results)
// ============================================================================

/// Stored verification plan from the database.
/// Contains the full plan JSON plus metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredVerificationPlan {
    pub id: String,
    pub task_run_id: String,
    pub version: u32,
    /// Full VerificationPlan serialized as JSON
    pub plan_json: String,
    pub goal_summary: String,
    pub criteria_count: u32,
    pub has_ai_criteria: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replan_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_version_id: Option<String>,
    pub created_at: String,
}

impl StoredVerificationPlan {
    /// Parse the plan_json into a VerificationPlan struct.
    pub fn parse_plan(&self) -> Result<crate::orchestrator::VerificationPlan, String> {
        serde_json::from_str(&self.plan_json)
            .map_err(|e| format!("Failed to parse verification plan: {}", e))
    }
}

/// Stored task knowledge entry from the database.
/// Represents findings, observations, hypotheses, etc. from agents.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredTaskKnowledge {
    pub id: String,
    pub task_run_id: String,
    /// Category: 'finding', 'root_cause', 'observation', 'hypothesis', 'solution', 'context'
    pub category: String,
    /// Agent type: 'planning', 'worker', 'verification', 'system'
    pub agent_type: String,
    pub iteration: u32,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence: Option<String>,
    /// Confidence: 'high', 'medium', 'low'
    pub confidence: String,
    #[serde(default)]
    pub related_files: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub related_criterion_id: Option<String>,
    pub is_resolved: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolution_notes: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_at: Option<String>,
    pub created_at: String,
}

/// Stored verification result from the orchestrator.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredVerificationResult {
    pub id: String,
    pub task_run_id: String,
    pub plan_id: String,
    pub iteration: u32,
    pub criterion_id: String,
    /// 'deterministic' or 'ai_evaluated'
    pub criterion_type: String,
    pub passed: bool,
    pub is_critical: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<String>,
    #[serde(default)]
    pub observations: Vec<String>,
    #[serde(default)]
    pub issues: Vec<String>,
    #[serde(default)]
    pub suggestions: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_output: Option<String>,
    pub created_at: String,
}

/// Input for creating a new verification test.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateVerificationTestInput {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub test_type: TestType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub playwright_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vision_config: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub python_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo_test_config: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub success_criteria: Option<String>,
    #[serde(default)]
    pub config: serde_json::Value,
    /// Timeout in seconds (None = disabled by default, no timeout)
    #[serde(default)]
    pub timeout_seconds: Option<u32>,
    #[serde(default)]
    pub is_critical: bool,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub ai_generated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ai_generation_prompt: Option<String>,
    /// Page analysis captured during test creation (for AI debugging context)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub creation_analysis: Option<serde_json::Value>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_file: Option<String>,
}

fn default_timeout() -> u32 {
    60
}

fn default_true() -> bool {
    true
}

/// Input for creating a test result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateTestResultInput {
    pub test_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_run_id: Option<String>,
}

// ============================================================================
// Check Types for Database
// ============================================================================

/// Check definition stored in the database.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Check {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Check type: lint, format, typecheck, custom_command
    pub check_type: String,
    /// Tool: black, isort, ruff, mypy, eslint, prettier, tsc, clippy, rustfmt, custom, etc.
    pub tool: String,
    /// Custom command override
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// Working directory
    #[serde(skip_serializing_if = "Option::is_none")]
    pub working_directory: Option<String>,
    /// Path to config file (e.g., pyproject.toml)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config_path: Option<String>,
    /// Whether to run in auto-fix mode
    pub auto_fix: bool,
    /// Whether to fail on warnings
    pub fail_on_warning: bool,
    /// Timeout in seconds (None = disabled by default, no timeout)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<u32>,
    /// Whether check failure should fail the entire workflow
    pub is_critical: bool,
    /// Whether check is enabled
    pub enabled: bool,
    /// Whether AI generated this check
    pub ai_generated: bool,
    /// AI generation prompt (if AI generated)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ai_generation_prompt: Option<String>,
    /// Tags for organization
    #[serde(default)]
    pub tags: Vec<String>,
    /// Creation timestamp
    pub created_at: String,
    /// Last update timestamp
    pub updated_at: String,
}

/// Input for creating a new check.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateCheckInput {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub check_type: String,
    pub tool: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub working_directory: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config_path: Option<String>,
    #[serde(default)]
    pub auto_fix: bool,
    #[serde(default)]
    pub fail_on_warning: bool,
    /// Timeout in seconds (None = disabled by default, no timeout)
    #[serde(default)]
    pub timeout_seconds: Option<u32>,
    #[serde(default)]
    pub is_critical: bool,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub ai_generated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ai_generation_prompt: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
}

/// Input for updating an existing check.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UpdateCheckInput {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub check_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub working_directory: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_fix: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fail_on_warning: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_critical: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
}

/// Check group definition stored in the database.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckGroup {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Color for UI display (e.g., 'purple', 'blue')
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// Whether the group is enabled
    pub enabled: bool,
    /// Run checks in parallel (vs sequential)
    pub run_in_parallel: bool,
    /// Stop running checks if one fails
    pub stop_on_failure: bool,
    /// Tags for organization
    #[serde(default)]
    pub tags: Vec<String>,
    /// Checks in this group (populated when fetching)
    #[serde(default)]
    pub checks: Vec<Check>,
    /// Creation timestamp
    pub created_at: String,
    /// Last update timestamp
    pub updated_at: String,
}

/// Input for creating a new check group.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateCheckGroupInput {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub run_in_parallel: bool,
    #[serde(default = "default_true")]
    pub stop_on_failure: bool,
    #[serde(default)]
    pub tags: Vec<String>,
    /// Check IDs to add to the group
    #[serde(default)]
    pub check_ids: Vec<String>,
}

/// Input for updating an existing check group.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UpdateCheckGroupInput {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_in_parallel: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_on_failure: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
}

/// Check execution result stored in the database.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckResult {
    pub id: String,
    pub check_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_run_id: Option<String>,
    /// Status: pending, running, passed, failed, fixed, error, timeout
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    pub issues_found: i32,
    pub issues_fixed: i32,
    pub files_checked: i32,
    /// Structured output as JSON string
    #[serde(skip_serializing_if = "Option::is_none")]
    pub structured_output: Option<String>,
    pub created_at: String,
}

/// Shell command definition stored in the database.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShellCommand {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The shell command to execute
    pub command: String,
    /// Working directory for command execution
    #[serde(skip_serializing_if = "Option::is_none")]
    pub working_directory: Option<String>,
    /// Timeout in seconds
    pub timeout_seconds: i32,
    /// Whether to fail the workflow if the command fails
    pub fail_on_error: bool,
    /// Category: git, npm, poetry, docker, general
    pub category: String,
    /// Tags for organization
    pub tags: Vec<String>,
    /// Whether the command is enabled
    pub enabled: bool,
    pub created_at: String,
    pub updated_at: String,
}

/// Input for creating a new shell command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateShellCommandInput {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub command: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub working_directory: Option<String>,
    #[serde(default = "default_timeout_i32")]
    pub timeout_seconds: i32,
    #[serde(default = "default_true")]
    pub fail_on_error: bool,
    #[serde(default = "default_general_category")]
    pub category: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

/// Input for updating an existing shell command.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UpdateShellCommandInput {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub working_directory: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fail_on_error: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
}

/// Shell command execution result stored in the database.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShellCommandResult {
    pub id: String,
    pub shell_command_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_run_id: Option<String>,
    /// Status: pending, running, success, failed, error, timeout
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stdout: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stderr: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
    pub created_at: String,
}

// =============================================================================
// Mobile Development Feedback Types (Version 30)
// =============================================================================

/// Mobile device/app state capture stored in the database.
/// Used for AI feedback loop during qontinui-mobile development.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MobileState {
    pub id: i64,
    pub task_run_id: String,
    pub timestamp: String,

    /// Device identification
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_model: Option<String>,

    /// App state
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app_package: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app_activity: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app_state: Option<String>,

    /// Metro/Expo state
    pub metro_connected: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bundle_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_reload_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_reload_time: Option<String>,

    /// Capture paths
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screenshot_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logcat_path: Option<String>,

    /// Error summary
    pub has_errors: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_summary: Option<String>,

    pub created_at: String,
}

/// Input for creating a mobile state capture.
#[derive(Debug, Clone)]
pub struct CreateMobileStateInput {
    pub task_run_id: String,
    pub device_id: Option<String>,
    pub device_type: Option<String>,
    pub device_model: Option<String>,
    pub app_package: Option<String>,
    pub app_activity: Option<String>,
    pub app_state: Option<String>,
    pub metro_connected: bool,
    pub bundle_status: Option<String>,
    pub last_reload_type: Option<String>,
    pub last_reload_time: Option<String>,
    pub screenshot_path: Option<String>,
    pub logcat_path: Option<String>,
    pub has_errors: bool,
    pub error_summary: Option<String>,
}

/// Mobile log entry stored in the database.
/// Stores parsed log entries from Metro, Logcat, etc.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MobileLog {
    pub id: i64,
    pub task_run_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mobile_state_id: Option<i64>,

    /// Log classification
    pub log_source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub log_level: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub log_tag: Option<String>,

    /// Content
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_line: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,

    /// Error details
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stack_trace: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line_number: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column_number: Option<i32>,

    /// Timing
    pub timestamp: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_timestamp: Option<String>,

    pub created_at: String,
}

/// Input for creating a mobile log entry.
#[derive(Debug, Clone)]
pub struct CreateMobileLogInput {
    pub task_run_id: String,
    pub mobile_state_id: Option<i64>,
    pub log_source: String,
    pub log_level: Option<String>,
    pub log_tag: Option<String>,
    pub message: String,
    pub raw_line: Option<String>,
    pub data: Option<String>,
    pub error_type: Option<String>,
    pub error_code: Option<String>,
    pub stack_trace: Option<String>,
    pub file_path: Option<String>,
    pub line_number: Option<i32>,
    pub column_number: Option<i32>,
    pub device_timestamp: Option<String>,
}

fn default_general_category() -> String {
    "general".to_string()
}

fn default_timeout_i32() -> i32 {
    60
}

/// Completion marker that AI uses to signal task is done
/// NOTE: This is deprecated in favor of WORK_COMPLETE_MARKER for the new orchestrator architecture.
/// Workers should emit [WORK_COMPLETE] to signal they believe work is done.
/// Only the orchestrator (system) marks tasks as truly complete after verification.
pub const TASK_COMPLETE_MARKER: &str = "[TASK_COMPLETE]";

/// Work complete marker that workers emit when they believe work is done.
/// The orchestrator then runs verification (deterministic + AI) before
/// deciding if the task is truly complete.
pub const WORK_COMPLETE_MARKER: &str = "[WORK_COMPLETE]";

/// Replan request marker that workers emit when they discover the plan needs revision.
pub const NEED_REPLAN_MARKER: &str = "[NEED_REPLAN]";

/// Finding marker prefix for workers to report discoveries.
/// Format: [FINDING:type] description
pub const FINDING_MARKER_PREFIX: &str = "[FINDING:";

/// Session start marker format
pub fn session_start_marker(session_num: u32) -> String {
    format!("[SESSION_START:{}]", session_num)
}

// ========================================================================
// Types defined after impl blocks
// ========================================================================

/// A row from the phase_token_usage table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhaseTokenUsageRow {
    pub phase: String,
    pub stage_index: Option<u32>,
    pub iteration: Option<u32>,
    pub model_used: Option<String>,
    pub provider_used: Option<String>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cost_cents: u64,
    pub duration_ms: Option<u64>,
    pub created_at: String,
}

// ========================================================================
// Generator Evaluation Types
// ========================================================================

/// Dashboard metrics for generator evaluation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneratorDashboardMetrics {
    pub total_generations: i64,
    pub successful_generations: i64,
    pub success_rate: f64,
    pub avg_total_duration_ms: Option<f64>,
    pub avg_verification_iterations: Option<f64>,
    pub first_pass_rate: Option<f64>,
    pub hardener_total_processed: i64,
    pub hardener_total_converted: i64,
    pub total_edits: i64,
    pub total_deletes: i64,
    pub total_ratings: i64,
    pub avg_rating: Option<f64>,
}

/// A single data point in a time series.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneratorTimeSeriesPoint {
    pub date: String,
    pub total_generations: i64,
    pub successful_generations: i64,
    pub avg_duration_ms: Option<f64>,
    pub avg_verification_iterations: Option<f64>,
}

/// Benchmark definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneratorBenchmark {
    pub id: String,
    pub name: String,
    pub description: String,
    pub category: Option<String>,
    pub tags: Option<Vec<String>>,
    pub expected_structure: crate::workflow_generation::benchmark::ExpectedStructure,
    pub created_at: String,
    pub updated_at: String,
    pub enabled: bool,
}

/// Update request for a benchmark.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BenchmarkUpdate {
    pub name: Option<String>,
    pub description: Option<String>,
    pub expected_structure: Option<crate::workflow_generation::benchmark::ExpectedStructure>,
    pub enabled: Option<bool>,
}

/// Benchmark result record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneratorBenchmarkResult {
    pub id: String,
    pub benchmark_id: String,
    pub artifact_id: Option<String>,
    pub run_at: String,
    pub model_used: Option<String>,
    pub structure_score: Option<f64>,
    pub content_score: Option<f64>,
    pub step_type_score: Option<f64>,
    pub overall_score: Option<f64>,
    pub score_breakdown: Option<serde_json::Value>,
    pub generated_json: Option<serde_json::Value>,
    pub duration_ms: Option<u64>,
    pub passed: bool,
    pub notes: Option<String>,
}

/// Aggregated edit analysis from feedback data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EditAnalysis {
    pub edited_fields: Vec<(String, i64)>,
    pub type_distribution: Vec<(String, i64)>,
    pub rating_distribution: Vec<(i32, i64)>,
    pub recent_feedback: Vec<RecentFeedback>,
}

/// A single feedback entry for display.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecentFeedback {
    pub id: String,
    pub workflow_id: String,
    pub feedback_type: String,
    pub edited_field: Option<String>,
    pub old_value: Option<String>,
    pub new_value: Option<String>,
    pub created_at: String,
    pub workflow_name: Option<String>,
}

/// Summary of a workflow in the example library.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExampleWorkflowSummary {
    pub id: String,
    pub name: String,
    pub description: String,
    pub category: String,
    pub example_status: Option<String>,
    pub created_at: String,
}

/// Persisted process session record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessSession {
    pub id: String,
    pub process_config_id: String,
    pub process_name: String,
    pub started_at: String,
    pub stopped_at: Option<String>,
    pub exit_code: Option<i32>,
    pub state: String,
    pub error_count: u32,
}

/// A single line of persisted process output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessSessionOutputLine {
    pub id: i64,
    pub session_id: String,
    pub timestamp: String,
    pub stream: String,
    pub line: String,
}

/// Result of an import operation.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ImportResult {
    pub imported: usize,
    pub skipped: usize,
    pub errors: Vec<String>,
}

/// Result of JSON file migration.
#[derive(Debug, Default)]
pub struct MigrationResult {
    pub settings_migrated: usize,
    pub prompts_migrated: usize,
    pub scheduler_tasks_migrated: usize,
    pub workflows_migrated: usize,
    pub checkpoints_migrated: usize,
    pub errors: Vec<String>,
}

impl MigrationResult {
    #[allow(dead_code)]
    pub fn is_success(&self) -> bool {
        self.errors.is_empty()
    }

    pub fn total_migrated(&self) -> usize {
        self.settings_migrated
            + self.prompts_migrated
            + self.scheduler_tasks_migrated
            + self.workflows_migrated
            + self.checkpoints_migrated
    }
}

/// Summary of a single iteration's verification phase results (for replay point listing).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationPhaseSummary {
    pub iteration: i32,
    pub all_passed: bool,
    pub passed_steps: i32,
    pub failed_steps: i32,
    pub created_at: String,
}

/// A persisted artifact from the UI Bridge IPC artifact store.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Artifact {
    pub artifact_id: String,
    pub source_json: String,
    pub result_json: String,
    pub environment_json: String,
    pub created_at: String,
    pub passed: Option<bool>,
}

/// Query parameters for listing artifacts.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactQuery {
    /// Filter by spec ID (JSON-extracted from source_json)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spec_id: Option<String>,
    /// Filter by date range start (ISO 8601)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date_from: Option<String>,
    /// Filter by date range end (ISO 8601)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date_to: Option<String>,
    /// If true, only return passed artifacts
    #[serde(skip_serializing_if = "Option::is_none")]
    pub passed_only: Option<bool>,
    /// If true, only return failed artifacts
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failed_only: Option<bool>,
    /// Maximum number of results to return
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    /// Offset for pagination
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offset: Option<u32>,
}

/// Count parameters for counting artifacts (mirrors query filters).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactCountQuery {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spec_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date_from: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date_to: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub passed_only: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failed_only: Option<bool>,
}

// ============================================================================
// Observations (Engram-inspired persistent memory)
// ============================================================================

/// A persistent observation — cross-session knowledge with type classification.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Observation {
    pub id: i64,
    pub title: String,
    pub content: String,
    pub observation_type: String,
    pub scope: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub topic_key: Option<String>,
    pub content_hash: String,
    pub revision_count: i32,
    pub duplicate_count: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workflow_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub is_deleted: bool,
    pub created_at: String,
    pub updated_at: String,
}

/// Truncated search result with relevance rank.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ObservationSearchResult {
    pub id: i64,
    pub title: String,
    pub content_preview: String,
    pub observation_type: String,
    pub scope: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub topic_key: Option<String>,
    pub revision_count: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rank: Option<f32>,
}

/// Input for creating a new observation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateObservationInput {
    pub title: String,
    pub content: String,
    pub observation_type: String,
    #[serde(default = "default_scope")]
    pub scope: String,
    pub topic_key: Option<String>,
    pub project_id: Option<String>,
    pub workflow_id: Option<String>,
    pub task_run_id: Option<String>,
    pub session_id: Option<String>,
}

fn default_scope() -> String {
    "project".to_string()
}

/// Input for updating an existing observation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateObservationInput {
    #[serde(default)]
    pub id: i64,
    pub title: Option<String>,
    pub content: Option<String>,
    pub observation_type: Option<String>,
}

/// Observation type statistics.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ObservationTypeStat {
    pub observation_type: String,
    pub count: i64,
    pub latest_updated: String,
}

// ============================================================================
// Activity Timeline (screenpipe-inspired capture history)
// ============================================================================

/// Input for creating an activity timeline entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityTimelineInput {
    pub text_content: String,
    /// Source of text extraction: "accessibility", "ocr", "ui_bridge"
    pub source_type: String,
    /// Automation mode: "white_box" (UI Bridge) or "black_box" (HAL)
    pub capture_mode: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screenshot_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub element_count: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata_json: Option<String>,
}

/// Full activity timeline entry (single record).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityTimelineEntry {
    pub id: i64,
    pub text_content: String,
    pub content_hash: String,
    pub source_type: String,
    pub capture_mode: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screenshot_path: Option<String>,
    pub element_count: Option<i32>,
    pub confidence: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata_json: Option<String>,
    pub duplicate_count: i32,
    pub created_at: String,
}

/// Search result from activity timeline FTS (500-char preview + rank).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityTimelineSearchResult {
    pub id: i64,
    pub text_preview: String,
    pub source_type: String,
    pub capture_mode: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screenshot_path: Option<String>,
    pub element_count: Option<i32>,
    pub confidence: Option<f64>,
    pub created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rank: Option<f32>,
}

/// Activity timeline capture statistics by source and mode.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityTimelineStat {
    pub source_type: String,
    pub capture_mode: String,
    pub count: i64,
    pub latest_capture: String,
}

// ============================================================================
// Watchers (screenpipe-inspired scheduled reactive AI agents)
// ============================================================================

/// Input for creating a new watcher.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateWatcherInput {
    pub name: String,
    /// ScheduleExpression as JSON (cron, interval, once, condition).
    pub schedule_json: String,
    /// Activity timeline FTS query string.
    pub timeline_query: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app_name_filter: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_type_filter: Option<String>,
    /// PostgreSQL interval for lookback (e.g. "15 minutes", "1 hour").
    #[serde(default = "default_lookback")]
    pub lookback_window: String,
    /// AI prompt template with {{results}}, {{result_count}}, {{query}} placeholders.
    pub reasoning_prompt: String,
    /// WatcherAction as JSON (RunWorkflow, Notify, CreateObservation, LogOnly).
    pub action_json: String,
}

fn default_lookback() -> String {
    "15 minutes".to_string()
}

/// Input for updating an existing watcher.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateWatcherInput {
    pub id: String,
    pub name: Option<String>,
    pub schedule_json: Option<String>,
    pub timeline_query: Option<String>,
    pub app_name_filter: Option<String>,
    pub source_type_filter: Option<String>,
    pub lookback_window: Option<String>,
    pub reasoning_prompt: Option<String>,
    pub action_json: Option<String>,
    pub enabled: Option<bool>,
}

/// Stored watcher definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Watcher {
    pub id: String,
    pub name: String,
    pub schedule_json: String,
    pub timeline_query: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app_name_filter: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_type_filter: Option<String>,
    pub lookback_window: String,
    pub reasoning_prompt: String,
    pub action_json: String,
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_run_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_result_json: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

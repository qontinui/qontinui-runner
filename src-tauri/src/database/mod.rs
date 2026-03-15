//! SQLite database for qontinui-runner persistence.
//!
//! Provides transaction-safe storage for sessions, checkpoints, settings,
//! prompts, workflows, and scheduler state.

#![allow(dead_code)]

pub mod embedding_client;
pub mod embedding_jobs;
pub mod embeddings;
pub mod hybrid_search;
pub mod query_builder;

use chrono::Utc;
use r2d2::{Pool, PooledConnection};
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::{params, Connection, OptionalExtension, Result as SqliteResult};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tracing::{info, warn};

/// Database handle for checkpoint and session persistence.
pub struct CheckpointDb {
    pool: Pool<SqliteConnectionManager>,
    db_path: PathBuf,
}

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

impl CheckpointDb {
    /// Create a new database connection at the default location.
    ///
    /// Database location: `~/.config/com.qontinui.runner/runner.db`
    pub fn new() -> Result<Self, String> {
        let config_dir = dirs::config_dir()
            .ok_or("Failed to get config directory")?
            .join("com.qontinui.runner");

        // Create directory if needed
        std::fs::create_dir_all(&config_dir)
            .map_err(|e| format!("Failed to create config directory: {}", e))?;

        let db_path = config_dir.join("runner.db");
        Self::new_at_path(db_path)
    }

    /// Create a new database connection at a specific path.
    pub fn new_at_path(db_path: PathBuf) -> Result<Self, String> {
        // Create connection manager with PRAGMA initialization
        // Each connection from the pool will have these settings applied
        let manager = SqliteConnectionManager::file(&db_path).with_init(|conn| {
            // Enable WAL mode and configure for better concurrency
            // - journal_mode=WAL: Write-Ahead Logging for concurrent readers
            // - foreign_keys=ON: Enforce referential integrity
            // - busy_timeout=5000: Wait up to 5 seconds on lock contention (instead of immediate failure)
            // - synchronous=NORMAL: Safe for WAL mode, better performance than FULL
            // - temp_store=MEMORY: Keep temp tables in memory
            // - cache_size=-32000: 32MB page cache for better performance
            conn.execute_batch(
                r#"
                PRAGMA journal_mode=WAL;
                PRAGMA foreign_keys=ON;
                PRAGMA busy_timeout=5000;
                PRAGMA synchronous=NORMAL;
                PRAGMA temp_store=MEMORY;
                PRAGMA cache_size=-32000;
                "#,
            )?;
            Ok(())
        });

        // Build the connection pool with max 10 connections
        let pool = Pool::builder()
            .max_size(10)
            .build(manager)
            .map_err(|e| format!("Failed to create connection pool: {}", e))?;

        // Run migrations using a connection from the pool
        {
            let conn = pool
                .get()
                .map_err(|e| format!("Failed to get connection from pool: {}", e))?;
            Self::run_migrations_on_conn(&conn)?;
        }

        info!(
            "Checkpoint database initialized with connection pool at {:?}",
            db_path
        );

        Ok(Self {
            pool,
            db_path: db_path.clone(),
        })
    }

    /// Create an in-memory database for testing or no-op logging.
    ///
    /// This creates a database that exists only in memory and is not persisted.
    /// Useful for testing or when you need a valid CheckpointDb instance but
    /// don't want to actually store anything.
    pub fn new_in_memory() -> Result<Self, String> {
        let manager = SqliteConnectionManager::memory().with_init(|conn| {
            conn.execute_batch(
                r#"
                PRAGMA foreign_keys=ON;
                PRAGMA synchronous=OFF;
                "#,
            )?;
            Ok(())
        });

        let pool = Pool::builder()
            .max_size(1)
            .build(manager)
            .map_err(|e| format!("Failed to create in-memory connection pool: {}", e))?;

        // Run migrations
        {
            let conn = pool
                .get()
                .map_err(|e| format!("Failed to get connection from pool: {}", e))?;
            Self::run_migrations_on_conn(&conn)?;
        }

        Ok(Self {
            pool,
            db_path: PathBuf::from(":memory:"),
        })
    }

    /// Get the database path.
    pub fn path(&self) -> &PathBuf {
        &self.db_path
    }

    /// Get a clone of the underlying connection pool.
    ///
    /// This is primarily used for the tracing span layer which needs
    /// its own reference to the pool for writing span data.
    pub fn get_pool(&self) -> Pool<SqliteConnectionManager> {
        self.pool.clone()
    }

    /// Get a connection from the pool.
    ///
    /// # Errors
    /// Returns `AppError::PoolError` if unable to get a connection from the pool.
    pub fn get_conn(
        &self,
    ) -> Result<PooledConnection<SqliteConnectionManager>, crate::error::AppError> {
        Ok(self.pool.get()?)
    }

    /// Get a connection from the pool (legacy API returning String error).
    ///
    /// Deprecated: Use `get_conn()` instead which returns `AppError`.
    pub fn get_conn_string(&self) -> Result<PooledConnection<SqliteConnectionManager>, String> {
        self.pool
            .get()
            .map_err(|e| format!("Failed to get connection from pool: {}", e))
    }

    /// Get a reference to the underlying connection for direct rusqlite operations.
    /// This is useful for modules that need raw rusqlite::Connection access (e.g., findings storage).
    ///
    /// Note: The returned reference borrows the PooledConnection, so the caller must
    /// ensure the PooledConnection stays alive while using the Connection.
    ///
    /// # Errors
    /// Returns `AppError::PoolError` if unable to get a connection from the pool.
    pub fn connection(
        &self,
    ) -> Result<PooledConnection<SqliteConnectionManager>, crate::error::AppError> {
        self.get_conn()
    }

    /// Execute a closure with a database connection, returning the closure's result.
    ///
    /// This is a convenience method for modules that need to run operations on
    /// a raw `rusqlite::Connection` reference without managing the pooled connection lifetime.
    pub fn with_conn<F, T>(&self, f: F) -> Result<T, String>
    where
        F: FnOnce(&rusqlite::Connection) -> Result<T, String>,
    {
        let conn = self.get_conn_string()?;
        f(&conn)
    }

    /// Get a connection (legacy API returning String error).
    ///
    /// Deprecated: Use `connection()` instead which returns `AppError`.
    pub fn connection_string(&self) -> Result<PooledConnection<SqliteConnectionManager>, String> {
        self.get_conn_string()
    }

    /// Execute a function within a database transaction.
    ///
    /// The transaction is automatically committed if the function returns Ok,
    /// or rolled back if it returns Err or panics.
    ///
    /// This ensures atomic updates across multiple tables (e.g., checkpoint + state).
    ///
    /// # Example
    /// ```ignore
    /// db.transaction(|conn| {
    ///     conn.execute("UPDATE table1 SET ...", params![...])?;
    ///     conn.execute("UPDATE table2 SET ...", params![...])?;
    ///     Ok(())
    /// })?;
    /// ```
    pub fn transaction<F, T>(&self, f: F) -> Result<T, String>
    where
        F: FnOnce(&Connection) -> Result<T, rusqlite::Error>,
    {
        let conn = self.get_conn_string()?;

        // Start transaction with IMMEDIATE mode for write isolation
        conn.execute("BEGIN IMMEDIATE", [])
            .map_err(|e| format!("Failed to begin transaction: {}", e))?;

        // Execute the user function
        match f(&conn) {
            Ok(result) => {
                // Commit on success
                conn.execute("COMMIT", [])
                    .map_err(|e| format!("Failed to commit transaction: {}", e))?;
                Ok(result)
            }
            Err(e) => {
                // Rollback on error
                let _ = conn.execute("ROLLBACK", []);
                Err(format!("Transaction failed: {}", e))
            }
        }
    }

    /// Optimize the database for better performance.
    ///
    /// This function performs:
    /// - VACUUM: Rebuilds the database file, reclaiming unused space
    /// - ANALYZE: Updates statistics for the query planner
    /// - Integrity check (optional)
    ///
    /// Returns optimization statistics including space reclaimed and time taken.
    pub fn optimize_database(
        &self,
        run_integrity_check: bool,
    ) -> Result<DatabaseOptimizeResult, String> {
        let conn = self.get_conn()?;
        let start_time = std::time::Instant::now();

        // Get database size before optimization
        let size_before: i64 = conn
            .query_row(
                "SELECT page_count * page_size FROM pragma_page_count(), pragma_page_size()",
                [],
                |row| row.get(0),
            )
            .unwrap_or(0);

        // Run integrity check if requested
        let integrity_ok = if run_integrity_check {
            let result: String = conn
                .query_row("PRAGMA integrity_check", [], |row| row.get(0))
                .unwrap_or_else(|_| "error".to_string());
            result == "ok"
        } else {
            true
        };

        if !integrity_ok {
            warn!("Database integrity check failed - skipping VACUUM");
            return Err("Database integrity check failed".to_string());
        }

        // Run ANALYZE to update statistics for query planner
        conn.execute_batch("ANALYZE")
            .map_err(|e| format!("Failed to run ANALYZE: {}", e))?;
        info!("ANALYZE completed successfully");

        // Run VACUUM to rebuild the database and reclaim space
        // Note: VACUUM requires exclusive access and cannot run in a transaction
        conn.execute_batch("VACUUM")
            .map_err(|e| format!("Failed to run VACUUM: {}", e))?;
        info!("VACUUM completed successfully");

        // Get database size after optimization
        let size_after: i64 = conn
            .query_row(
                "SELECT page_count * page_size FROM pragma_page_count(), pragma_page_size()",
                [],
                |row| row.get(0),
            )
            .unwrap_or(0);

        let duration_ms = start_time.elapsed().as_millis() as u64;
        let space_reclaimed = if size_before > size_after {
            size_before - size_after
        } else {
            0
        };

        info!(
            "Database optimization completed in {}ms. Space reclaimed: {} bytes",
            duration_ms, space_reclaimed
        );

        Ok(DatabaseOptimizeResult {
            duration_ms,
            size_before_bytes: size_before,
            size_after_bytes: size_after,
            space_reclaimed_bytes: space_reclaimed,
            integrity_check_passed: integrity_ok,
        })
    }

    /// Get database statistics for monitoring and debugging.
    pub fn get_database_stats(&self) -> Result<DatabaseStats, String> {
        let conn = self.get_conn()?;

        // Get page count and size
        let (page_count, page_size): (i64, i64) = conn
            .query_row(
                "SELECT page_count, page_size FROM pragma_page_count(), pragma_page_size()",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap_or((0, 0));

        // Get freelist count (unused pages)
        let freelist_count: i64 = conn
            .query_row("PRAGMA freelist_count", [], |row| row.get(0))
            .unwrap_or(0);

        // Get WAL checkpoint info
        let (wal_pages, wal_frames): (i64, i64) = conn
            .query_row("PRAGMA wal_checkpoint(PASSIVE)", [], |row| {
                Ok((row.get(1).unwrap_or(0), row.get(2).unwrap_or(0)))
            })
            .unwrap_or((0, 0));

        // Get table counts
        let table_counts = self.get_table_row_counts()?;

        Ok(DatabaseStats {
            total_size_bytes: page_count * page_size,
            page_count,
            page_size,
            freelist_count,
            wal_pages,
            wal_frames,
            table_counts,
        })
    }

    /// Get row counts for all tables.
    fn get_table_row_counts(&self) -> Result<Vec<TableRowCount>, String> {
        let conn = self.get_conn()?;

        // Get list of tables
        let mut stmt = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name")
            .map_err(|e| format!("Failed to list tables: {}", e))?;

        let tables: Vec<String> = stmt
            .query_map([], |row| row.get(0))
            .map_err(|e| format!("Failed to query tables: {}", e))?
            .filter_map(|r| r.ok())
            .collect();
        drop(stmt);

        let mut counts = Vec::new();
        for table in tables {
            let count: i64 = conn
                .query_row(&format!("SELECT COUNT(*) FROM \"{}\"", table), [], |row| {
                    row.get(0)
                })
                .unwrap_or(0);
            counts.push(TableRowCount {
                table_name: table,
                row_count: count,
            });
        }

        Ok(counts)
    }

    /// Run EXPLAIN QUERY PLAN on a query for debugging.
    /// Returns the query plan as a formatted string.
    pub fn explain_query_plan(&self, query: &str) -> Result<String, String> {
        let conn = self.get_conn()?;

        let explain_query = format!("EXPLAIN QUERY PLAN {}", query);
        let mut stmt = conn
            .prepare(&explain_query)
            .map_err(|e| format!("Failed to prepare EXPLAIN QUERY PLAN: {}", e))?;

        let rows: Vec<String> = stmt
            .query_map([], |row| {
                let id: i32 = row.get(0)?;
                let parent: i32 = row.get(1)?;
                let notused: i32 = row.get(2)?;
                let detail: String = row.get(3)?;
                Ok(format!("{}|{}|{}|{}", id, parent, notused, detail))
            })
            .map_err(|e| format!("Failed to run EXPLAIN QUERY PLAN: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(rows.join("\n"))
    }

    /// Run database migrations on a specific connection.
    /// This is a static method to allow calling during pool initialization.
    fn run_migrations_on_conn(conn: &Connection) -> Result<(), String> {
        // Check current schema version
        let current_version: i32 = conn
            .query_row(
                "SELECT version FROM schema_version ORDER BY version DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .unwrap_or(0);

        if current_version == 0 {
            // Fresh database - create schema
            // schema.sql contains the complete, up-to-date schema, so no migrations needed
            info!("Creating database schema from schema.sql (fresh database)");
            conn.execute_batch(include_str!("schema.sql"))
                .map_err(|e| format!("Failed to create schema: {}", e))?;
            // Return early - schema.sql is the complete schema at the latest version
            // No need to run migrations which would try to add columns/tables that already exist
            return Ok(());
        }

        // Migration to version 2: Add task_runs table
        if current_version == 1 {
            info!("Migrating database to version 2 (adding task_runs table)");
            conn.execute_batch(
                r#"
                -- Task Runs (simplified task execution model)
                CREATE TABLE IF NOT EXISTS task_runs (
                    id TEXT PRIMARY KEY,
                    task_name TEXT NOT NULL,
                    prompt TEXT NOT NULL,
                    status TEXT NOT NULL DEFAULT 'running',
                    sessions_count INTEGER NOT NULL DEFAULT 0,
                    max_sessions INTEGER,
                    output_log TEXT DEFAULT '',
                    error_message TEXT,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL,
                    completed_at TEXT
                );

                CREATE INDEX IF NOT EXISTS idx_task_runs_status ON task_runs(status);
                CREATE INDEX IF NOT EXISTS idx_task_runs_created_at ON task_runs(created_at);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (2, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 2: {}", e))?;
        }

        // Migration to version 3: Add auto_continue column to task_runs
        if current_version == 1 || current_version == 2 {
            info!("Migrating database to version 3 (adding auto_continue to task_runs)");
            conn.execute_batch(
                r#"
                -- Add auto_continue column to task_runs (default true)
                ALTER TABLE task_runs ADD COLUMN auto_continue BOOLEAN NOT NULL DEFAULT 1;

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (3, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 3: {}", e))?;
        }

        // Migration to version 4: Add task_run_output_chunks table for O(1) appending
        if (1..4).contains(&current_version) {
            info!("Migrating database to version 4 (adding task_run_output_chunks table)");

            // Create the chunks table
            conn.execute_batch(
                r#"
                -- Task Run Output Chunks (for efficient O(1) appending)
                CREATE TABLE IF NOT EXISTS task_run_output_chunks (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    task_run_id TEXT NOT NULL,
                    chunk_sequence INTEGER NOT NULL,
                    content TEXT NOT NULL,
                    created_at TEXT NOT NULL,
                    FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE
                );
                CREATE INDEX IF NOT EXISTS idx_chunks_task_run ON task_run_output_chunks(task_run_id, chunk_sequence);
                "#,
            )
            .map_err(|e| format!("Failed to create task_run_output_chunks table: {}", e))?;

            // Migrate existing output_log data to chunks (one-time migration)
            let now = Utc::now().to_rfc3339();

            // Get all task_runs with non-empty output_log
            let mut stmt = conn
                .prepare("SELECT id, output_log FROM task_runs WHERE output_log != '' AND output_log IS NOT NULL")
                .map_err(|e| format!("Failed to prepare migration query: {}", e))?;

            let tasks: Vec<(String, String)> = stmt
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
                .map_err(|e| format!("Failed to query task_runs for migration: {}", e))?
                .filter_map(|r| r.ok())
                .collect();

            drop(stmt); // Release the statement before executing more queries

            for (task_id, output) in &tasks {
                conn.execute(
                    "INSERT INTO task_run_output_chunks (task_run_id, chunk_sequence, content, created_at) VALUES (?1, 1, ?2, ?3)",
                    params![task_id, output, now],
                )
                .map_err(|e| format!("Failed to migrate output for task {}: {}", task_id, e))?;
            }

            if !tasks.is_empty() {
                info!("Migrated {} task runs' output_log to chunks", tasks.len());
            }

            conn.execute_batch(
                r#"
                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (4, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to update schema version to 4: {}", e))?;
        }

        // Migration to version 5: Add configs table for ConfigStorage
        if (1..5).contains(&current_version) {
            info!("Migrating database to version 5 (adding configs table)");
            conn.execute_batch(
                r#"
                -- Configs storage (for auto-storing imported/loaded configs)
                CREATE TABLE IF NOT EXISTS configs (
                    id TEXT PRIMARY KEY,
                    name TEXT NOT NULL,
                    config_json TEXT NOT NULL,
                    source_type TEXT NOT NULL,
                    source_path TEXT,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL
                );

                CREATE INDEX IF NOT EXISTS idx_configs_name ON configs(name);
                CREATE INDEX IF NOT EXISTS idx_configs_updated_at ON configs(updated_at);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (5, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 5: {}", e))?;
        }

        // Migration to version 6: Add task_run_findings table
        // (This migration was added between 5 and 7, keeping the version gap for backward compat)
        if (1..6).contains(&current_version) {
            info!("Migrating database to version 6 (adding task_run_findings table)");
            conn.execute_batch(
                r#"
                -- Task Run Findings (AI-detected issues tied to task runs)
                CREATE TABLE IF NOT EXISTS task_run_findings (
                    id TEXT PRIMARY KEY,
                    task_run_id TEXT NOT NULL,
                    category TEXT NOT NULL,
                    severity TEXT NOT NULL,
                    signature_hash TEXT,
                    title TEXT NOT NULL,
                    description TEXT NOT NULL,
                    file_path TEXT,
                    line_number INTEGER,
                    column_number INTEGER,
                    code_snippet TEXT,
                    status TEXT NOT NULL DEFAULT 'detected',
                    action_type TEXT NOT NULL DEFAULT 'auto_fix',
                    resolution TEXT,
                    detected_in_session INTEGER NOT NULL,
                    resolved_in_session INTEGER,
                    needs_input BOOLEAN DEFAULT 0,
                    question TEXT,
                    input_options TEXT,
                    user_response TEXT,
                    detected_at TEXT NOT NULL,
                    resolved_at TEXT,
                    updated_at TEXT NOT NULL,
                    FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE
                );

                CREATE INDEX IF NOT EXISTS idx_findings_task_run ON task_run_findings(task_run_id);
                CREATE INDEX IF NOT EXISTS idx_findings_status ON task_run_findings(status);
                CREATE INDEX IF NOT EXISTS idx_findings_signature ON task_run_findings(signature_hash);
                CREATE INDEX IF NOT EXISTS idx_findings_category ON task_run_findings(category);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (6, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 6: {}", e))?;
        }

        // Migration to version 7: Add Tiered Information Model tables
        if (1..7).contains(&current_version) {
            info!("Migrating database to version 7 (adding tiered information tables)");
            conn.execute_batch(
                r#"
                -- Run Details (Tier 1 - Detailed run data)
                CREATE TABLE IF NOT EXISTS run_details (
                    id TEXT PRIMARY KEY,
                    config_id TEXT NOT NULL,
                    workflow_name TEXT,
                    started_at TEXT NOT NULL,
                    ended_at TEXT,
                    duration_ms INTEGER,
                    status TEXT NOT NULL,
                    success BOOLEAN,
                    error_type TEXT,
                    error_message TEXT,
                    actions_summary TEXT,
                    states_visited TEXT,
                    transitions_executed TEXT,
                    template_matches TEXT,
                    anomalies TEXT,
                    FOREIGN KEY (config_id) REFERENCES configs(id) ON DELETE CASCADE
                );

                CREATE INDEX IF NOT EXISTS idx_run_details_config_id ON run_details(config_id);
                CREATE INDEX IF NOT EXISTS idx_run_details_started_at ON run_details(started_at);
                CREATE INDEX IF NOT EXISTS idx_run_details_status ON run_details(status);

                -- Config Statistics (Tier 4 - Aggregated statistics)
                CREATE TABLE IF NOT EXISTS config_statistics (
                    id TEXT PRIMARY KEY,
                    config_id TEXT NOT NULL UNIQUE,
                    config_hash TEXT,
                    total_runs INTEGER DEFAULT 0,
                    successful_runs INTEGER DEFAULT 0,
                    failed_runs INTEGER DEFAULT 0,
                    timeout_runs INTEGER DEFAULT 0,
                    avg_duration_ms INTEGER,
                    recent_success_rate REAL,
                    recent_avg_duration_ms INTEGER,
                    transition_stats TEXT,
                    template_stats TEXT,
                    state_stats TEXT,
                    error_patterns TEXT,
                    flaky_transitions TEXT,
                    flaky_templates TEXT,
                    first_run_at TEXT,
                    last_run_at TEXT,
                    last_updated_at TEXT,
                    FOREIGN KEY (config_id) REFERENCES configs(id) ON DELETE CASCADE
                );

                CREATE INDEX IF NOT EXISTS idx_config_statistics_config_id ON config_statistics(config_id);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (7, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 7: {}", e))?;
        }

        // Migration to version 8: Add pending_discoveries table for Discovery Push
        if (1..8).contains(&current_version) {
            info!("Migrating database to version 8 (adding pending_discoveries table)");
            conn.execute_batch(
                r#"
                -- Pending Discoveries (Discovery Push queue)
                -- Stores discoveries awaiting sync to qontinui-web
                CREATE TABLE IF NOT EXISTS pending_discoveries (
                    id TEXT PRIMARY KEY,
                    payload TEXT NOT NULL,
                    created_at TEXT NOT NULL,
                    last_attempt TEXT,
                    attempt_count INTEGER DEFAULT 0,
                    error TEXT
                );

                CREATE INDEX IF NOT EXISTS idx_pending_discoveries_created_at ON pending_discoveries(created_at);
                CREATE INDEX IF NOT EXISTS idx_pending_discoveries_attempt_count ON pending_discoveries(attempt_count);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (8, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 8: {}", e))?;
        }

        // Migration to version 9: Add execution_steps_json and log_sources_json to task_runs
        if (1..9).contains(&current_version) {
            info!("Migrating database to version 9 (adding execution_steps_json and log_sources_json to task_runs)");
            conn.execute_batch(
                r#"
                -- Add columns for storing execution steps and log sources for re-execution on resume
                ALTER TABLE task_runs ADD COLUMN execution_steps_json TEXT;
                ALTER TABLE task_runs ADD COLUMN log_sources_json TEXT;

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (9, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 9: {}", e))?;
        }

        // Migration to version 10: Add AI summary fields to task_runs
        if (1..10).contains(&current_version) {
            info!("Migrating database to version 10 (adding AI summary fields to task_runs)");
            conn.execute_batch(
                r#"
                -- Add columns for AI-generated summary and goal achievement tracking
                ALTER TABLE task_runs ADD COLUMN ai_summary TEXT;
                ALTER TABLE task_runs ADD COLUMN goal_achieved BOOLEAN;
                ALTER TABLE task_runs ADD COLUMN remaining_work TEXT;
                ALTER TABLE task_runs ADD COLUMN summary_generated_at TEXT;

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (10, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 10: {}", e))?;
        }

        // Migration to version 11: Unified TaskRun architecture
        // - Add task_type, config_id, workflow_name to task_runs
        // - Rename ai_summary -> summary (keeping ai_summary as alias via code)
        // - Create task_run_automation table
        if (1..11).contains(&current_version) {
            info!("Migrating database to version 11 (unified TaskRun architecture)");

            // Step 1: Add new columns to task_runs
            // Note: SQLite doesn't support renaming columns in older versions, so we
            // add a new 'summary' column and will handle ai_summary -> summary mapping in code
            conn.execute_batch(
                r#"
                -- Add task_type column (default 'task' for existing runs)
                ALTER TABLE task_runs ADD COLUMN task_type TEXT NOT NULL DEFAULT 'task';

                -- Add config linkage columns
                ALTER TABLE task_runs ADD COLUMN config_id TEXT;
                ALTER TABLE task_runs ADD COLUMN workflow_name TEXT;

                -- Add summary column (new name for ai_summary - existing ai_summary still works)
                ALTER TABLE task_runs ADD COLUMN summary TEXT;

                -- Create indexes for new columns
                CREATE INDEX IF NOT EXISTS idx_task_runs_task_type ON task_runs(task_type);
                CREATE INDEX IF NOT EXISTS idx_task_runs_config_id ON task_runs(config_id);
                "#,
            )
            .map_err(|e| format!("Failed to add unified columns to task_runs: {}", e))?;

            // Step 2: Copy ai_summary to summary for existing rows
            conn.execute(
                "UPDATE task_runs SET summary = ai_summary WHERE ai_summary IS NOT NULL AND summary IS NULL",
                [],
            )
            .map_err(|e| format!("Failed to copy ai_summary to summary: {}", e))?;

            // Step 3: Create task_run_automation table
            conn.execute_batch(
                r#"
                -- Task Run Automation (child table for automation metrics)
                CREATE TABLE IF NOT EXISTS task_run_automation (
                    id TEXT PRIMARY KEY,
                    task_run_id TEXT NOT NULL,
                    workflow_name TEXT,
                    started_at TEXT NOT NULL,
                    ended_at TEXT,
                    duration_ms INTEGER,
                    automation_status TEXT NOT NULL DEFAULT 'running',
                    success BOOLEAN,
                    error_type TEXT,
                    error_message TEXT,
                    actions_summary TEXT,
                    states_visited TEXT,
                    transitions_executed TEXT,
                    template_matches TEXT,
                    anomalies TEXT,
                    iteration_number INTEGER NOT NULL DEFAULT 1,
                    FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE
                );

                CREATE INDEX IF NOT EXISTS idx_task_run_automation_task_run_id ON task_run_automation(task_run_id);
                CREATE INDEX IF NOT EXISTS idx_task_run_automation_started_at ON task_run_automation(started_at);
                CREATE INDEX IF NOT EXISTS idx_task_run_automation_status ON task_run_automation(automation_status);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (11, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to create task_run_automation table: {}", e))?;

            info!("Successfully migrated to version 11 (unified TaskRun architecture)");
        }

        // Migration to version 12: Migrate existing run_details to task_run_automation
        // Creates parent task_runs rows for each run_details, then task_run_automation children
        if (1..12).contains(&current_version) {
            info!(
                "Migrating database to version 12 (migrating run_details to task_run_automation)"
            );

            // Step 1: Create task_runs rows for each run_details that doesn't have one
            // We use the run_details.id prefixed with 'tr-' as the task_run_id
            conn.execute_batch(
                r#"
                -- Create task_runs for each run_details (pure automation tasks)
                INSERT OR IGNORE INTO task_runs (
                    id,
                    task_name,
                    prompt,
                    task_type,
                    status,
                    sessions_count,
                    auto_continue,
                    output_log,
                    config_id,
                    workflow_name,
                    created_at,
                    updated_at,
                    completed_at
                )
                SELECT
                    'tr-' || rd.id,
                    COALESCE(rd.workflow_name, 'Automation Run'),
                    NULL,  -- No prompt for pure automation
                    'automation',
                    CASE
                        WHEN rd.status = 'completed' AND rd.success = 1 THEN 'complete'
                        WHEN rd.status = 'completed' AND rd.success = 0 THEN 'failed'
                        WHEN rd.status = 'failed' THEN 'failed'
                        WHEN rd.status = 'timeout' THEN 'failed'
                        WHEN rd.status = 'cancelled' THEN 'stopped'
                        ELSE 'complete'
                    END,
                    0,
                    0,
                    '',
                    rd.config_id,
                    rd.workflow_name,
                    rd.started_at,
                    COALESCE(rd.ended_at, rd.started_at),
                    rd.ended_at
                FROM run_details rd
                WHERE NOT EXISTS (
                    SELECT 1 FROM task_runs tr WHERE tr.id = 'tr-' || rd.id
                );
                "#,
            )
            .map_err(|e| format!("Failed to create task_runs from run_details: {}", e))?;

            // Step 2: Create task_run_automation rows from run_details
            conn.execute_batch(
                r#"
                -- Create task_run_automation for each run_details
                INSERT OR IGNORE INTO task_run_automation (
                    id,
                    task_run_id,
                    workflow_name,
                    started_at,
                    ended_at,
                    duration_ms,
                    automation_status,
                    success,
                    error_type,
                    error_message,
                    actions_summary,
                    states_visited,
                    transitions_executed,
                    template_matches,
                    anomalies,
                    iteration_number
                )
                SELECT
                    'tra-' || rd.id,
                    'tr-' || rd.id,
                    rd.workflow_name,
                    rd.started_at,
                    rd.ended_at,
                    rd.duration_ms,
                    CASE
                        WHEN rd.status = 'completed' AND rd.success = 1 THEN 'success'
                        WHEN rd.status = 'completed' AND rd.success = 0 THEN 'failed'
                        WHEN rd.status = 'failed' THEN 'failed'
                        WHEN rd.status = 'timeout' THEN 'timeout'
                        WHEN rd.status = 'cancelled' THEN 'cancelled'
                        ELSE 'success'
                    END,
                    rd.success,
                    rd.error_type,
                    rd.error_message,
                    rd.actions_summary,
                    rd.states_visited,
                    rd.transitions_executed,
                    rd.template_matches,
                    rd.anomalies,
                    1
                FROM run_details rd
                WHERE NOT EXISTS (
                    SELECT 1 FROM task_run_automation tra WHERE tra.id = 'tra-' || rd.id
                );

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (12, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to create task_run_automation from run_details: {}", e))?;

            info!(
                "Successfully migrated to version 12 (run_details migrated to task_run_automation)"
            );
        }

        // Migration to version 13: Drop deprecated run_details table
        // All data was migrated to task_run_automation in version 12
        if (1..13).contains(&current_version) {
            info!("Migrating database to version 13 (removing deprecated run_details table)");

            conn.execute_batch(
                r#"
                -- Drop the deprecated run_details table
                DROP TABLE IF EXISTS run_details;

                -- Update schema version
                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (13, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to drop run_details table: {}", e))?;

            info!("Successfully migrated to version 13 (run_details table removed)");
        }

        // Migration to version 14: Add verification test infrastructure
        // Creates tables for test definitions, results, and associations
        if (1..14).contains(&current_version) {
            info!("Migrating database to version 14 (adding verification test infrastructure)");

            conn.execute_batch(
                r#"
                -- Verification Tests (test definitions stored in runner)
                CREATE TABLE IF NOT EXISTS verification_tests (
                    id TEXT PRIMARY KEY,
                    name TEXT NOT NULL,
                    description TEXT,
                    test_type TEXT NOT NULL,
                    category TEXT,
                    playwright_code TEXT,
                    vision_config TEXT,
                    python_code TEXT,
                    repo_test_config TEXT,
                    success_criteria TEXT,
                    config TEXT DEFAULT '{}',
                    timeout_seconds INTEGER NOT NULL DEFAULT 60,
                    is_critical BOOLEAN NOT NULL DEFAULT 1,
                    enabled BOOLEAN NOT NULL DEFAULT 1,
                    ai_generated BOOLEAN NOT NULL DEFAULT 0,
                    ai_generation_prompt TEXT,
                    tags TEXT DEFAULT '[]',
                    source_file TEXT,
                    last_exported_at TEXT,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL
                );

                CREATE INDEX IF NOT EXISTS idx_verification_tests_test_type ON verification_tests(test_type);
                CREATE INDEX IF NOT EXISTS idx_verification_tests_category ON verification_tests(category);
                CREATE INDEX IF NOT EXISTS idx_verification_tests_enabled ON verification_tests(enabled);

                -- Test Results (execution results linked to task runs)
                CREATE TABLE IF NOT EXISTS test_results (
                    id TEXT PRIMARY KEY,
                    test_id TEXT NOT NULL,
                    task_run_id TEXT,
                    status TEXT NOT NULL DEFAULT 'pending',
                    started_at TEXT,
                    completed_at TEXT,
                    duration_ms INTEGER,
                    output TEXT,
                    error_message TEXT,
                    structured_output TEXT,
                    assertions_passed INTEGER DEFAULT 0,
                    assertions_failed INTEGER DEFAULT 0,
                    screenshots TEXT DEFAULT '[]',
                    ai_analysis TEXT,
                    created_at TEXT NOT NULL,
                    FOREIGN KEY (test_id) REFERENCES verification_tests(id) ON DELETE CASCADE,
                    FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE
                );

                CREATE INDEX IF NOT EXISTS idx_test_results_test_id ON test_results(test_id);
                CREATE INDEX IF NOT EXISTS idx_test_results_task_run_id ON test_results(task_run_id);
                CREATE INDEX IF NOT EXISTS idx_test_results_status ON test_results(status);

                -- Test Associations (link tests to configs/workflows)
                CREATE TABLE IF NOT EXISTS test_associations (
                    id TEXT PRIMARY KEY,
                    test_id TEXT NOT NULL,
                    config_id TEXT,
                    workflow_name TEXT,
                    trigger_point TEXT NOT NULL,
                    action_id TEXT,
                    execution_order INTEGER DEFAULT 0,
                    enabled BOOLEAN NOT NULL DEFAULT 1,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL,
                    FOREIGN KEY (test_id) REFERENCES verification_tests(id) ON DELETE CASCADE,
                    FOREIGN KEY (config_id) REFERENCES configs(id) ON DELETE CASCADE
                );

                CREATE INDEX IF NOT EXISTS idx_test_associations_test_id ON test_associations(test_id);
                CREATE INDEX IF NOT EXISTS idx_test_associations_config_id ON test_associations(config_id);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (14, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 14: {}", e))?;

            info!("Successfully migrated to version 14 (verification test infrastructure added)");
        }

        // Migration to version 15: Add creation_analysis to verification_tests and visual_evidence to test_results
        if (1..15).contains(&current_version) {
            info!("Migrating database to version 15 (adding creation_analysis and visual_evidence columns)");

            conn.execute_batch(
                r#"
                -- Add creation_analysis to verification_tests for AI debugging context
                ALTER TABLE verification_tests ADD COLUMN creation_analysis TEXT;

                -- Add visual_evidence to test_results for annotated screenshots
                ALTER TABLE test_results ADD COLUMN visual_evidence TEXT;

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (15, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 15: {}", e))?;

            info!("Successfully migrated to version 15 (creation_analysis and visual_evidence columns added)");
        }

        // Migration to version 16: Add API request infrastructure tables
        if (1..16).contains(&current_version) {
            info!("Migrating database to version 16 (adding API request infrastructure tables)");

            conn.execute_batch(
                r#"
                -- API Credentials (metadata only, secrets in secure storage)
                CREATE TABLE IF NOT EXISTS api_credentials (
                    id TEXT PRIMARY KEY,
                    name TEXT NOT NULL,
                    credential_type TEXT NOT NULL,
                    storage_type TEXT NOT NULL DEFAULT 'secure',
                    token_endpoint TEXT,
                    client_id TEXT,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL,
                    expires_at TEXT
                );

                CREATE INDEX IF NOT EXISTS idx_api_credentials_name ON api_credentials(name);
                CREATE INDEX IF NOT EXISTS idx_api_credentials_type ON api_credentials(credential_type);

                -- API Request Logs
                CREATE TABLE IF NOT EXISTS api_request_logs (
                    id TEXT PRIMARY KEY,
                    task_run_id TEXT,
                    step_id TEXT NOT NULL,
                    step_name TEXT,
                    method TEXT NOT NULL,
                    url TEXT NOT NULL,
                    resolved_url TEXT NOT NULL,
                    status_code INTEGER NOT NULL,
                    response_time_ms INTEGER NOT NULL,
                    response_body_type TEXT NOT NULL,
                    response_file_path TEXT,
                    response_size_bytes INTEGER,
                    success BOOLEAN NOT NULL,
                    assertion_failures INTEGER DEFAULT 0,
                    extractions_json TEXT,
                    assertions_json TEXT,
                    error TEXT,
                    created_at TEXT NOT NULL,
                    FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE
                );

                CREATE INDEX IF NOT EXISTS idx_api_request_logs_task_run_id ON api_request_logs(task_run_id);
                CREATE INDEX IF NOT EXISTS idx_api_request_logs_created_at ON api_request_logs(created_at);
                CREATE INDEX IF NOT EXISTS idx_api_request_logs_step_id ON api_request_logs(step_id);

                -- Workflow Variables
                CREATE TABLE IF NOT EXISTS workflow_variables (
                    id TEXT PRIMARY KEY,
                    task_run_id TEXT NOT NULL,
                    variable_name TEXT NOT NULL,
                    variable_value TEXT NOT NULL,
                    source TEXT NOT NULL,
                    source_step_id TEXT,
                    created_at TEXT NOT NULL,
                    FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE,
                    UNIQUE(task_run_id, variable_name)
                );

                CREATE INDEX IF NOT EXISTS idx_workflow_variables_task_run_id ON workflow_variables(task_run_id);
                CREATE INDEX IF NOT EXISTS idx_workflow_variables_name ON workflow_variables(variable_name);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (16, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 16: {}", e))?;

            info!("Successfully migrated to version 16 (API request infrastructure tables added)");
        }

        // Migration to version 18: Add saved_api_requests table for API Request Library
        if (1..18).contains(&current_version) {
            info!("Migrating database to version 18 (adding saved_api_requests table)");

            conn.execute_batch(
                r#"
                -- Saved API Request Templates (Library)
                CREATE TABLE IF NOT EXISTS saved_api_requests (
                    id TEXT PRIMARY KEY,
                    name TEXT NOT NULL,
                    description TEXT,
                    category TEXT DEFAULT 'general',
                    tags TEXT DEFAULT '[]',
                    method TEXT NOT NULL DEFAULT 'GET',
                    url TEXT NOT NULL,
                    headers TEXT DEFAULT '{}',
                    body TEXT,
                    body_content_type TEXT DEFAULT 'application/json',
                    timeout_ms INTEGER DEFAULT 30000,
                    follow_redirects BOOLEAN DEFAULT 1,
                    variable_extractions TEXT DEFAULT '[]',
                    assertions TEXT DEFAULT '[]',
                    credential_id TEXT,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL,
                    FOREIGN KEY (credential_id) REFERENCES api_credentials(id) ON DELETE SET NULL
                );

                CREATE INDEX IF NOT EXISTS idx_saved_api_requests_category ON saved_api_requests(category);
                CREATE INDEX IF NOT EXISTS idx_saved_api_requests_updated_at ON saved_api_requests(updated_at);
                CREATE INDEX IF NOT EXISTS idx_saved_api_requests_name ON saved_api_requests(name);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (18, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 18: {}", e))?;

            info!("Successfully migrated to version 18 (saved_api_requests table added)");
        }

        // Migration to version 19: Add unified_workflows table for Workflow Builder
        if (1..19).contains(&current_version) {
            info!("Migrating database to version 19 (adding unified_workflows table)");

            conn.execute_batch(
                r#"
                -- Unified Workflows (Phase-based workflow builder)
                CREATE TABLE IF NOT EXISTS unified_workflows (
                    id TEXT PRIMARY KEY,
                    name TEXT NOT NULL,
                    description TEXT DEFAULT '',
                    category TEXT DEFAULT 'general',
                    tags TEXT DEFAULT '[]',

                    -- Phase steps (JSON arrays)
                    setup_steps TEXT DEFAULT '[]',
                    verification_steps TEXT DEFAULT '[]',
                    agentic_steps TEXT DEFAULT '[]',

                    -- Agentic configuration
                    max_iterations INTEGER DEFAULT 10,
                    provider TEXT,
                    model TEXT,

                    -- Timestamps
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL
                );

                CREATE INDEX IF NOT EXISTS idx_unified_workflows_category ON unified_workflows(category);
                CREATE INDEX IF NOT EXISTS idx_unified_workflows_updated_at ON unified_workflows(updated_at);
                CREATE INDEX IF NOT EXISTS idx_unified_workflows_name ON unified_workflows(name);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (19, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 19: {}", e))?;

            info!("Successfully migrated to version 19 (unified_workflows table added)");
        }

        // Migration to version 20: Ensure API infrastructure tables exist (fix for missing v16 migration)
        if (1..20).contains(&current_version) {
            info!("Migrating database to version 20 (ensuring API infrastructure tables exist)");

            conn.execute_batch(
                r#"
                -- Create api_credentials if it doesn't exist (was missing in some v18 migrations)
                CREATE TABLE IF NOT EXISTS api_credentials (
                    id TEXT PRIMARY KEY,
                    name TEXT NOT NULL,
                    credential_type TEXT NOT NULL,
                    storage_type TEXT NOT NULL DEFAULT 'secure',
                    token_endpoint TEXT,
                    client_id TEXT,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL,
                    expires_at TEXT
                );

                CREATE INDEX IF NOT EXISTS idx_api_credentials_name ON api_credentials(name);
                CREATE INDEX IF NOT EXISTS idx_api_credentials_type ON api_credentials(credential_type);

                -- Create other API tables if they don't exist
                CREATE TABLE IF NOT EXISTS api_request_logs (
                    id TEXT PRIMARY KEY,
                    task_run_id TEXT,
                    step_id TEXT NOT NULL,
                    step_name TEXT,
                    method TEXT NOT NULL,
                    url TEXT NOT NULL,
                    resolved_url TEXT NOT NULL,
                    status_code INTEGER NOT NULL,
                    response_time_ms INTEGER NOT NULL,
                    response_body_type TEXT NOT NULL,
                    response_file_path TEXT,
                    response_size_bytes INTEGER,
                    success BOOLEAN NOT NULL,
                    assertion_failures INTEGER DEFAULT 0,
                    extractions_json TEXT,
                    assertions_json TEXT,
                    error TEXT,
                    created_at TEXT NOT NULL,
                    FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE
                );

                CREATE INDEX IF NOT EXISTS idx_api_request_logs_task_run_id ON api_request_logs(task_run_id);
                CREATE INDEX IF NOT EXISTS idx_api_request_logs_created_at ON api_request_logs(created_at);
                CREATE INDEX IF NOT EXISTS idx_api_request_logs_step_id ON api_request_logs(step_id);

                CREATE TABLE IF NOT EXISTS workflow_variables (
                    id TEXT PRIMARY KEY,
                    task_run_id TEXT NOT NULL,
                    variable_name TEXT NOT NULL,
                    variable_value TEXT NOT NULL,
                    source TEXT NOT NULL,
                    source_step_id TEXT,
                    created_at TEXT NOT NULL,
                    FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE,
                    UNIQUE(task_run_id, variable_name)
                );

                CREATE INDEX IF NOT EXISTS idx_workflow_variables_task_run_id ON workflow_variables(task_run_id);
                CREATE INDEX IF NOT EXISTS idx_workflow_variables_name ON workflow_variables(variable_name);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (20, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 20: {}", e))?;

            info!("Successfully migrated to version 20 (API infrastructure tables ensured)");
        }

        // Migration 21: Add completion_steps and skip_ai_summary to unified_workflows
        if current_version < 21 {
            conn.execute_batch(
                r#"
                -- Add completion_steps column for completion phase steps
                ALTER TABLE unified_workflows ADD COLUMN completion_steps TEXT DEFAULT '[]';

                -- Add skip_ai_summary column for controlling AI summary generation
                ALTER TABLE unified_workflows ADD COLUMN skip_ai_summary BOOLEAN NOT NULL DEFAULT 0;

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (21, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 21: {}", e))?;

            info!("Successfully migrated to version 21 (completion_steps and skip_ai_summary columns added to unified_workflows)");
        }

        // Migration 22: Add hybrid logging tables (task_run_events, task_run_screenshots, task_run_playwright_results)
        if current_version < 22 {
            conn.execute_batch(
                r#"
                -- Task Run Events (unifies JSONL logs for historical queries)
                CREATE TABLE IF NOT EXISTS task_run_events (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    task_run_id TEXT NOT NULL,
                    event_type TEXT NOT NULL,
                    event_subtype TEXT,
                    message TEXT NOT NULL,
                    data TEXT,
                    workflow_name TEXT,
                    state_name TEXT,
                    action_id TEXT,
                    timestamp TEXT NOT NULL,
                    duration_ms INTEGER,
                    FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE
                );

                CREATE INDEX IF NOT EXISTS idx_task_run_events_task_run_id ON task_run_events(task_run_id);
                CREATE INDEX IF NOT EXISTS idx_task_run_events_event_type ON task_run_events(event_type);
                CREATE INDEX IF NOT EXISTS idx_task_run_events_timestamp ON task_run_events(timestamp);

                -- Task Run Screenshots
                CREATE TABLE IF NOT EXISTS task_run_screenshots (
                    id TEXT PRIMARY KEY,
                    task_run_id TEXT NOT NULL,
                    event_id INTEGER,
                    file_path TEXT NOT NULL,
                    screenshot_type TEXT NOT NULL,
                    template_name TEXT,
                    confidence REAL,
                    match_location TEXT,
                    width INTEGER,
                    height INTEGER,
                    file_size_bytes INTEGER,
                    created_at TEXT NOT NULL,
                    FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE,
                    FOREIGN KEY (event_id) REFERENCES task_run_events(id) ON DELETE SET NULL
                );

                CREATE INDEX IF NOT EXISTS idx_task_run_screenshots_task_run_id ON task_run_screenshots(task_run_id);
                CREATE INDEX IF NOT EXISTS idx_task_run_screenshots_type ON task_run_screenshots(screenshot_type);

                -- Task Run Playwright Results
                CREATE TABLE IF NOT EXISTS task_run_playwright_results (
                    id TEXT PRIMARY KEY,
                    task_run_id TEXT NOT NULL,
                    test_name TEXT NOT NULL,
                    spec_file TEXT,
                    status TEXT NOT NULL,
                    duration_ms INTEGER,
                    stdout TEXT,
                    stderr TEXT,
                    console_output TEXT,
                    page_snapshot TEXT,
                    error_message TEXT,
                    failure_screenshot_path TEXT,
                    assertions_passed INTEGER DEFAULT 0,
                    assertions_failed INTEGER DEFAULT 0,
                    created_at TEXT NOT NULL,
                    FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE
                );

                CREATE INDEX IF NOT EXISTS idx_task_run_playwright_task_run_id ON task_run_playwright_results(task_run_id);
                CREATE INDEX IF NOT EXISTS idx_task_run_playwright_status ON task_run_playwright_results(status);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (22, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 22: {}", e))?;

            info!("Successfully migrated to version 22 (hybrid logging tables: task_run_events, task_run_screenshots, task_run_playwright_results)");
        }

        // Migration 23: Add task_knowledge_summaries
        if current_version < 23 {
            conn.execute_batch(
                r#"
                -- Task Knowledge Summaries (Memory Compression)
                CREATE TABLE IF NOT EXISTS task_knowledge_summaries (
                    id TEXT PRIMARY KEY,
                    task_run_id TEXT NOT NULL,
                    category TEXT NOT NULL,
                    summary TEXT NOT NULL,
                    covered_iterations TEXT NOT NULL,
                    item_count INTEGER NOT NULL,
                    original_tokens INTEGER,
                    compressed_tokens INTEGER,
                    created_at TEXT NOT NULL,
                    FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE
                );

                CREATE INDEX IF NOT EXISTS idx_task_knowledge_summaries_task_run_id ON task_knowledge_summaries(task_run_id);
                CREATE INDEX IF NOT EXISTS idx_task_knowledge_summaries_category ON task_knowledge_summaries(category);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (23, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 23: {}", e))?;

            info!("Successfully migrated to version 23 (task_knowledge_summaries)");
        }

        // Migration 24: Add task_run_api_requests table
        if current_version < 24 {
            conn.execute_batch(
                r#"
                -- Task Run API Requests (from runner-api-requests.jsonl)
                CREATE TABLE IF NOT EXISTS task_run_api_requests (
                    id TEXT PRIMARY KEY,
                    task_run_id TEXT NOT NULL,
                    step_id TEXT NOT NULL,
                    step_name TEXT,
                    method TEXT NOT NULL,
                    url TEXT NOT NULL,
                    resolved_url TEXT NOT NULL,
                    request_headers TEXT,
                    request_body TEXT,
                    status_code INTEGER NOT NULL,
                    status_text TEXT,
                    response_headers TEXT,
                    response_time_ms INTEGER NOT NULL,
                    response_body_type TEXT NOT NULL,
                    response_body TEXT,
                    response_size_bytes INTEGER,
                    extractions TEXT,
                    assertions TEXT,
                    success BOOLEAN NOT NULL,
                    error_message TEXT,
                    created_at TEXT NOT NULL,
                    FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE
                );

                CREATE INDEX IF NOT EXISTS idx_task_run_api_requests_task_run_id ON task_run_api_requests(task_run_id);
                CREATE INDEX IF NOT EXISTS idx_task_run_api_requests_step_id ON task_run_api_requests(step_id);
                CREATE INDEX IF NOT EXISTS idx_task_run_api_requests_created_at ON task_run_api_requests(created_at);
                CREATE INDEX IF NOT EXISTS idx_task_run_api_requests_success ON task_run_api_requests(success);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (24, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 24: {}", e))?;

            info!("Successfully migrated to version 24 (task_run_api_requests table)");
        }

        // Migration 25: Add runtime_context_json to task_runs and task_hooks table
        if current_version < 25 {
            conn.execute_batch(
                r#"
                -- Task Hooks (Lifecycle hooks for execution events)
                CREATE TABLE IF NOT EXISTS task_hooks (
                    id TEXT PRIMARY KEY,
                    name TEXT NOT NULL,
                    description TEXT,

                    -- Hook trigger: 'pre_execution', 'post_execution', 'on_error', 'on_verification_fail', 'on_complete', 'pre_iteration', 'post_iteration'
                    trigger TEXT NOT NULL,

                    -- Hook action configuration
                    action_type TEXT NOT NULL,          -- 'command', 'webhook', 'log', 'notification'
                    action_config TEXT NOT NULL,        -- JSON: {command, url, headers, body, message, etc.}

                    -- Execution settings
                    enabled BOOLEAN DEFAULT 1,
                    execution_order INTEGER DEFAULT 0,  -- Lower = executes earlier
                    continue_on_failure BOOLEAN DEFAULT 1,

                    -- Optional conditions (JSON array of {variable, operator, value})
                    conditions TEXT DEFAULT '[]',

                    -- Scope: NULL = global, or specific task_run_id for task-specific hooks
                    task_run_id TEXT,

                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL,

                    FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE
                );

                CREATE INDEX IF NOT EXISTS idx_task_hooks_trigger ON task_hooks(trigger);
                CREATE INDEX IF NOT EXISTS idx_task_hooks_enabled ON task_hooks(enabled);
                CREATE INDEX IF NOT EXISTS idx_task_hooks_task_run_id ON task_hooks(task_run_id);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (25, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 25: {}", e))?;

            // Add runtime_context_json column (ALTER TABLE must be separate)
            let _ = conn.execute(
                "ALTER TABLE task_runs ADD COLUMN runtime_context_json TEXT",
                [],
            );

            info!("Successfully migrated to version 25 (task_hooks table, runtime_context_json)");
        }

        // Migration to version 26: Add orchestrator tables (learning, checkpoints, flows)
        if (1..26).contains(&current_version) {
            info!("Migrating database to version 26 (orchestrator tables: learning, checkpoints, flows)");
            conn.execute_batch(
                r#"
                -- Learning Outcomes: Records task outcomes for learning system
                CREATE TABLE IF NOT EXISTS learning_outcomes (
                    id TEXT PRIMARY KEY,
                    task_id TEXT NOT NULL,
                    status TEXT NOT NULL,  -- 'success', 'failure', 'partial'
                    duration_secs REAL,
                    iterations INTEGER,
                    strategy TEXT,
                    tools_used TEXT,  -- JSON array
                    files_modified TEXT,  -- JSON array
                    error_type TEXT,
                    error_message TEXT,
                    feedback TEXT,  -- JSON array
                    created_at TEXT NOT NULL
                );
                CREATE INDEX IF NOT EXISTS idx_learning_outcomes_task_id ON learning_outcomes(task_id);
                CREATE INDEX IF NOT EXISTS idx_learning_outcomes_status ON learning_outcomes(status);
                CREATE INDEX IF NOT EXISTS idx_learning_outcomes_created_at ON learning_outcomes(created_at);

                -- Learning Patterns: Identified patterns from task analysis
                CREATE TABLE IF NOT EXISTS learning_patterns (
                    id TEXT PRIMARY KEY,
                    pattern_type TEXT NOT NULL,  -- 'success', 'failure', 'tool_usage', etc.
                    description TEXT NOT NULL,
                    confidence REAL NOT NULL,
                    occurrences INTEGER NOT NULL DEFAULT 1,
                    context TEXT,  -- JSON with additional context
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL
                );
                CREATE INDEX IF NOT EXISTS idx_learning_patterns_type ON learning_patterns(pattern_type);
                CREATE INDEX IF NOT EXISTS idx_learning_patterns_confidence ON learning_patterns(confidence);

                -- Orchestrator Checkpoints: State snapshots for time-travel debugging
                CREATE TABLE IF NOT EXISTS orchestrator_checkpoints (
                    id TEXT PRIMARY KEY,
                    task_id TEXT NOT NULL,
                    iteration INTEGER NOT NULL,
                    trigger TEXT NOT NULL,  -- 'automatic', 'manual', 'iteration_boundary', etc.
                    state TEXT NOT NULL,  -- JSON serialized StateSnapshot
                    name TEXT,  -- Optional user-provided name
                    created_at TEXT NOT NULL
                );
                CREATE INDEX IF NOT EXISTS idx_orchestrator_checkpoints_task_id ON orchestrator_checkpoints(task_id);
                CREATE INDEX IF NOT EXISTS idx_orchestrator_checkpoints_task_iteration ON orchestrator_checkpoints(task_id, iteration);

                -- Orchestrator Flows: Flow definitions for deterministic workflows
                CREATE TABLE IF NOT EXISTS orchestrator_flows (
                    id TEXT PRIMARY KEY,
                    name TEXT NOT NULL,
                    description TEXT,
                    steps TEXT NOT NULL,  -- JSON object of step definitions
                    start_step TEXT,
                    timeout_secs INTEGER,
                    inputs TEXT,  -- JSON array of input definitions
                    outputs TEXT,  -- JSON array of output definitions
                    tags TEXT,  -- JSON array
                    version TEXT NOT NULL DEFAULT '1.0.0',
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL
                );
                CREATE INDEX IF NOT EXISTS idx_orchestrator_flows_name ON orchestrator_flows(name);

                -- Flow Executions: Runtime state for flow execution
                CREATE TABLE IF NOT EXISTS flow_executions (
                    instance_id TEXT PRIMARY KEY,
                    flow_id TEXT NOT NULL,
                    current_step TEXT,
                    status TEXT NOT NULL DEFAULT 'pending',  -- 'pending', 'running', 'waiting_for_input', 'completed', 'failed', 'cancelled'
                    context TEXT,  -- JSON object of flow variables
                    history TEXT,  -- JSON array of step executions
                    error TEXT,
                    started_at TEXT NOT NULL,
                    completed_at TEXT,
                    FOREIGN KEY (flow_id) REFERENCES orchestrator_flows(id) ON DELETE CASCADE
                );
                CREATE INDEX IF NOT EXISTS idx_flow_executions_flow_id ON flow_executions(flow_id);
                CREATE INDEX IF NOT EXISTS idx_flow_executions_status ON flow_executions(status);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (26, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 26: {}", e))?;

            info!("Successfully migrated to version 26 (orchestrator tables)");
        }

        // Migration to version 27: Add code quality checks infrastructure
        if (1..27).contains(&current_version) {
            info!("Migrating database to version 27 (adding code quality checks infrastructure)");

            conn.execute_batch(
                r#"
                -- Code Quality Checks (check definitions stored in runner)
                CREATE TABLE IF NOT EXISTS checks (
                    id TEXT PRIMARY KEY,
                    name TEXT NOT NULL,
                    description TEXT,
                    check_type TEXT NOT NULL,
                    tool TEXT NOT NULL,
                    command TEXT,
                    working_directory TEXT,
                    config_path TEXT,
                    auto_fix BOOLEAN NOT NULL DEFAULT 0,
                    fail_on_warning BOOLEAN NOT NULL DEFAULT 0,
                    timeout_seconds INTEGER NOT NULL DEFAULT 60,
                    is_critical BOOLEAN NOT NULL DEFAULT 0,
                    enabled BOOLEAN NOT NULL DEFAULT 1,
                    ai_generated BOOLEAN NOT NULL DEFAULT 0,
                    ai_generation_prompt TEXT,
                    tags TEXT DEFAULT '[]',
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL
                );

                CREATE INDEX IF NOT EXISTS idx_checks_check_type ON checks(check_type);
                CREATE INDEX IF NOT EXISTS idx_checks_tool ON checks(tool);
                CREATE INDEX IF NOT EXISTS idx_checks_enabled ON checks(enabled);

                -- Check Results (execution results linked to task runs)
                CREATE TABLE IF NOT EXISTS check_results (
                    id TEXT PRIMARY KEY,
                    check_id TEXT NOT NULL,
                    task_run_id TEXT,
                    status TEXT NOT NULL DEFAULT 'pending',
                    started_at TEXT,
                    completed_at TEXT,
                    duration_ms INTEGER,
                    output TEXT,
                    error_message TEXT,
                    issues_found INTEGER DEFAULT 0,
                    issues_fixed INTEGER DEFAULT 0,
                    files_checked INTEGER DEFAULT 0,
                    structured_output TEXT,
                    created_at TEXT NOT NULL,
                    FOREIGN KEY (check_id) REFERENCES checks(id) ON DELETE CASCADE,
                    FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE
                );

                CREATE INDEX IF NOT EXISTS idx_check_results_check_id ON check_results(check_id);
                CREATE INDEX IF NOT EXISTS idx_check_results_task_run_id ON check_results(task_run_id);
                CREATE INDEX IF NOT EXISTS idx_check_results_status ON check_results(status);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (27, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 27: {}", e))?;

            info!("Successfully migrated to version 27 (code quality checks infrastructure)");
        }

        // Migration to version 28: Performance optimization - additional indexes
        // Also creates task_knowledge table if missing (was in schema.sql but not in migrations)
        if (1..28).contains(&current_version) {
            info!("Migrating database to version 28 (performance optimization indexes)");

            // First, ensure task_knowledge table exists (was missing from migrations)
            conn.execute_batch(
                r#"
                -- Task Knowledge table (was in schema.sql but missing from migrations)
                CREATE TABLE IF NOT EXISTS task_knowledge (
                    id TEXT PRIMARY KEY,
                    task_run_id TEXT NOT NULL,
                    category TEXT NOT NULL,
                    agent_type TEXT NOT NULL,
                    iteration INTEGER NOT NULL DEFAULT 1,
                    content TEXT NOT NULL,
                    evidence TEXT,
                    confidence TEXT DEFAULT 'medium',
                    related_files TEXT DEFAULT '[]',
                    related_criterion_id TEXT,
                    is_resolved BOOLEAN NOT NULL DEFAULT 0,
                    resolution_notes TEXT,
                    resolved_at TEXT,
                    created_at TEXT NOT NULL,
                    FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE
                );
                CREATE INDEX IF NOT EXISTS idx_task_knowledge_task_run_id ON task_knowledge(task_run_id);
                CREATE INDEX IF NOT EXISTS idx_task_knowledge_category ON task_knowledge(category);
                CREATE INDEX IF NOT EXISTS idx_task_knowledge_is_resolved ON task_knowledge(is_resolved);
                "#,
            )
            .map_err(|e| format!("Failed to create task_knowledge table: {}", e))?;

            // Create verification_plans table (was in schema.sql but missing from migrations)
            conn.execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS verification_plans (
                    id TEXT PRIMARY KEY,
                    task_run_id TEXT NOT NULL,
                    version INTEGER NOT NULL DEFAULT 1,
                    plan_json TEXT NOT NULL,
                    goal_summary TEXT NOT NULL,
                    criteria_count INTEGER NOT NULL DEFAULT 0,
                    has_ai_criteria BOOLEAN NOT NULL DEFAULT 0,
                    replan_reason TEXT,
                    previous_version_id TEXT,
                    created_at TEXT NOT NULL,
                    FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE,
                    FOREIGN KEY (previous_version_id) REFERENCES verification_plans(id) ON DELETE SET NULL
                );
                CREATE INDEX IF NOT EXISTS idx_verification_plans_task_run_id ON verification_plans(task_run_id);
                CREATE INDEX IF NOT EXISTS idx_verification_plans_version ON verification_plans(version);
                "#,
            )
            .map_err(|e| format!("Failed to create verification_plans table: {}", e))?;

            // Create orchestrator_verification_results table (was in schema.sql but missing from migrations)
            conn.execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS orchestrator_verification_results (
                    id TEXT PRIMARY KEY,
                    task_run_id TEXT NOT NULL,
                    plan_id TEXT NOT NULL,
                    iteration INTEGER NOT NULL,
                    criterion_id TEXT NOT NULL,
                    criterion_type TEXT NOT NULL,
                    passed BOOLEAN NOT NULL,
                    is_critical BOOLEAN NOT NULL DEFAULT 1,
                    confidence TEXT,
                    observations TEXT DEFAULT '[]',
                    issues TEXT DEFAULT '[]',
                    suggestions TEXT DEFAULT '[]',
                    raw_output TEXT,
                    created_at TEXT NOT NULL,
                    FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE,
                    FOREIGN KEY (plan_id) REFERENCES verification_plans(id) ON DELETE CASCADE
                );
                CREATE INDEX IF NOT EXISTS idx_orch_ver_results_task_run_id ON orchestrator_verification_results(task_run_id);
                CREATE INDEX IF NOT EXISTS idx_orch_ver_results_plan_id ON orchestrator_verification_results(plan_id);
                CREATE INDEX IF NOT EXISTS idx_orch_ver_results_iteration ON orchestrator_verification_results(iteration);
                CREATE INDEX IF NOT EXISTS idx_orch_ver_results_passed ON orchestrator_verification_results(passed);
                "#,
            )
            .map_err(|e| format!("Failed to create orchestrator_verification_results table: {}", e))?;

            conn.execute_batch(
                r#"
                -- Additional indexes for learning_outcomes (frequently filtered columns)
                CREATE INDEX IF NOT EXISTS idx_learning_outcomes_strategy ON learning_outcomes(strategy);

                -- Additional indexes for learning_patterns (ORDER BY support)
                CREATE INDEX IF NOT EXISTS idx_learning_patterns_updated_at ON learning_patterns(updated_at);

                -- Additional indexes for orchestrator_checkpoints (filtering)
                CREATE INDEX IF NOT EXISTS idx_orchestrator_checkpoints_trigger ON orchestrator_checkpoints(trigger);
                CREATE INDEX IF NOT EXISTS idx_orchestrator_checkpoints_created_at ON orchestrator_checkpoints(created_at);

                -- Additional indexes for orchestrator_flows (ORDER BY support)
                CREATE INDEX IF NOT EXISTS idx_orchestrator_flows_created_at ON orchestrator_flows(created_at);
                CREATE INDEX IF NOT EXISTS idx_orchestrator_flows_updated_at ON orchestrator_flows(updated_at);

                -- Additional indexes for flow_executions (ORDER BY support)
                CREATE INDEX IF NOT EXISTS idx_flow_executions_started_at ON flow_executions(started_at);

                -- Additional composite indexes for task_knowledge (iteration queries)
                CREATE INDEX IF NOT EXISTS idx_task_knowledge_task_run_iteration ON task_knowledge(task_run_id, iteration);
                CREATE INDEX IF NOT EXISTS idx_task_knowledge_iteration ON task_knowledge(iteration);

                -- Additional indexes for orchestrator_verification_results (criterion queries)
                CREATE INDEX IF NOT EXISTS idx_orch_ver_results_criterion_id ON orchestrator_verification_results(criterion_id);

                -- Additional indexes for task_run_events (event filtering)
                CREATE INDEX IF NOT EXISTS idx_task_run_events_subtype ON task_run_events(event_subtype);
                CREATE INDEX IF NOT EXISTS idx_task_run_events_workflow ON task_run_events(workflow_name);

                -- Additional indexes for checks (ordering)
                CREATE INDEX IF NOT EXISTS idx_checks_created_at ON checks(created_at);
                CREATE INDEX IF NOT EXISTS idx_checks_updated_at ON checks(updated_at);

                -- Additional indexes for check_results (created_at for ordering)
                CREATE INDEX IF NOT EXISTS idx_check_results_created_at ON check_results(created_at);

                -- Additional indexes for sessions (completed status filtering)
                CREATE INDEX IF NOT EXISTS idx_sessions_completed ON sessions(completed);
                CREATE INDEX IF NOT EXISTS idx_sessions_session_type ON sessions(session_type);

                -- Additional indexes for scheduler_history (status filtering)
                CREATE INDEX IF NOT EXISTS idx_scheduler_history_status ON scheduler_history(status);

                -- Additional indexes for task_runs (workflow_name filtering)
                CREATE INDEX IF NOT EXISTS idx_task_runs_workflow_name ON task_runs(workflow_name);
                CREATE INDEX IF NOT EXISTS idx_task_runs_updated_at ON task_runs(updated_at);

                -- Additional indexes for verification_plans (created_at ordering)
                CREATE INDEX IF NOT EXISTS idx_verification_plans_created_at ON verification_plans(created_at);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (28, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 28: {}", e))?;

            info!("Successfully migrated to version 28 (performance optimization indexes)");
        }

        // Migration to version 29: Add shell_commands and shell_command_results tables
        if (1..29).contains(&current_version) {
            info!("Migrating database to version 29 (shell command library)");

            conn.execute_batch(
                r#"
                -- Shell commands table
                CREATE TABLE IF NOT EXISTS shell_commands (
                    id TEXT PRIMARY KEY,
                    name TEXT NOT NULL,
                    description TEXT,
                    command TEXT NOT NULL,
                    working_directory TEXT,
                    timeout_seconds INTEGER DEFAULT 30,
                    fail_on_error INTEGER DEFAULT 1,
                    category TEXT,
                    tags TEXT DEFAULT '[]',
                    enabled INTEGER DEFAULT 1,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL
                );

                CREATE INDEX IF NOT EXISTS idx_shell_commands_category ON shell_commands(category);
                CREATE INDEX IF NOT EXISTS idx_shell_commands_enabled ON shell_commands(enabled);
                CREATE INDEX IF NOT EXISTS idx_shell_commands_name ON shell_commands(name);
                CREATE INDEX IF NOT EXISTS idx_shell_commands_created_at ON shell_commands(created_at);
                CREATE INDEX IF NOT EXISTS idx_shell_commands_updated_at ON shell_commands(updated_at);

                -- Shell command execution results
                CREATE TABLE IF NOT EXISTS shell_command_results (
                    id TEXT PRIMARY KEY,
                    shell_command_id TEXT NOT NULL,
                    task_run_id TEXT,
                    status TEXT NOT NULL DEFAULT 'pending',
                    exit_code INTEGER,
                    stdout TEXT,
                    stderr TEXT,
                    duration_ms INTEGER,
                    started_at TEXT,
                    completed_at TEXT,
                    created_at TEXT NOT NULL,
                    FOREIGN KEY (shell_command_id) REFERENCES shell_commands(id) ON DELETE CASCADE
                );

                CREATE INDEX IF NOT EXISTS idx_shell_command_results_shell_command_id ON shell_command_results(shell_command_id);
                CREATE INDEX IF NOT EXISTS idx_shell_command_results_task_run_id ON shell_command_results(task_run_id);
                CREATE INDEX IF NOT EXISTS idx_shell_command_results_status ON shell_command_results(status);
                CREATE INDEX IF NOT EXISTS idx_shell_command_results_created_at ON shell_command_results(created_at);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (29, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 29: {}", e))?;

            info!("Successfully migrated to version 29 (shell command library)");
        }

        // Migration to version 30: Add mobile development feedback tables
        if (1..30).contains(&current_version) {
            info!("Migrating database to version 30 (mobile development feedback)");

            conn.execute_batch(
                r#"
                -- Mobile state captures
                CREATE TABLE IF NOT EXISTS task_run_mobile_state (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    task_run_id TEXT NOT NULL,
                    timestamp TEXT NOT NULL,
                    device_id TEXT,
                    device_type TEXT,
                    device_model TEXT,
                    app_package TEXT,
                    app_activity TEXT,
                    app_state TEXT,
                    metro_connected INTEGER DEFAULT 0,
                    bundle_status TEXT,
                    last_reload_type TEXT,
                    last_reload_time TEXT,
                    screenshot_path TEXT,
                    logcat_path TEXT,
                    has_errors INTEGER DEFAULT 0,
                    error_summary TEXT,
                    created_at TEXT NOT NULL,
                    FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE
                );

                CREATE INDEX IF NOT EXISTS idx_mobile_state_task_run_id ON task_run_mobile_state(task_run_id);
                CREATE INDEX IF NOT EXISTS idx_mobile_state_timestamp ON task_run_mobile_state(timestamp);
                CREATE INDEX IF NOT EXISTS idx_mobile_state_device_id ON task_run_mobile_state(device_id);
                CREATE INDEX IF NOT EXISTS idx_mobile_state_has_errors ON task_run_mobile_state(has_errors);

                -- Mobile log entries
                CREATE TABLE IF NOT EXISTS task_run_mobile_logs (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    task_run_id TEXT NOT NULL,
                    mobile_state_id INTEGER,
                    log_source TEXT NOT NULL,
                    log_level TEXT,
                    log_tag TEXT,
                    message TEXT NOT NULL,
                    raw_line TEXT,
                    data TEXT,
                    error_type TEXT,
                    error_code TEXT,
                    stack_trace TEXT,
                    file_path TEXT,
                    line_number INTEGER,
                    column_number INTEGER,
                    timestamp TEXT NOT NULL,
                    device_timestamp TEXT,
                    created_at TEXT NOT NULL,
                    FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE,
                    FOREIGN KEY (mobile_state_id) REFERENCES task_run_mobile_state(id) ON DELETE SET NULL
                );

                CREATE INDEX IF NOT EXISTS idx_mobile_logs_task_run_id ON task_run_mobile_logs(task_run_id);
                CREATE INDEX IF NOT EXISTS idx_mobile_logs_source ON task_run_mobile_logs(log_source);
                CREATE INDEX IF NOT EXISTS idx_mobile_logs_level ON task_run_mobile_logs(log_level);
                CREATE INDEX IF NOT EXISTS idx_mobile_logs_error_type ON task_run_mobile_logs(error_type);
                CREATE INDEX IF NOT EXISTS idx_mobile_logs_timestamp ON task_run_mobile_logs(timestamp);
                CREATE INDEX IF NOT EXISTS idx_mobile_logs_state_id ON task_run_mobile_logs(mobile_state_id);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (30, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 30: {}", e))?;

            info!("Successfully migrated to version 30 (mobile development feedback)");
        }

        // Version 31: Add log_source_selection to unified_workflows
        if current_version < 31 {
            info!("Migrating database to version 31 (adding log_source_selection to unified_workflows)");

            conn.execute_batch(
                r#"
                -- Add log_source_selection column for per-workflow log source selection
                -- Values: "default", "ai", "all", or JSON object like {"profile_id": "..."}
                ALTER TABLE unified_workflows ADD COLUMN log_source_selection TEXT DEFAULT '"default"';

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (31, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 31: {}", e))?;

            info!("Successfully migrated to version 31 (log_source_selection)");
        }

        // Version 32: Add context management fields to unified_workflows
        if current_version < 32 {
            info!(
                "Migrating database to version 32 (adding context management to unified_workflows)"
            );

            conn.execute_batch(
                r#"
                -- Add context_ids column (JSON array of manually added context IDs)
                ALTER TABLE unified_workflows ADD COLUMN context_ids TEXT DEFAULT '[]';

                -- Add disabled_context_ids column (JSON array of disabled context IDs)
                ALTER TABLE unified_workflows ADD COLUMN disabled_context_ids TEXT DEFAULT '[]';

                -- Add auto_include_contexts column (whether to auto-include contexts, default true)
                ALTER TABLE unified_workflows ADD COLUMN auto_include_contexts INTEGER DEFAULT 1;

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (32, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 32: {}", e))?;

            info!("Successfully migrated to version 32 (context management for unified_workflows)");
        }

        // Version 33: Add prompt_template to unified_workflows
        if current_version < 33 {
            info!("Migrating database to version 33 (adding prompt_template to unified_workflows)");

            conn.execute_batch(
                r#"
                -- Add prompt_template column (custom developer prompt template per workflow)
                ALTER TABLE unified_workflows ADD COLUMN prompt_template TEXT DEFAULT NULL;

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (33, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 33: {}", e))?;

            info!("Successfully migrated to version 33 (prompt_template for unified_workflows)");
        }

        // Version 34: Add transition_history_json to task_runs for stage-based recap
        if current_version < 34 {
            info!("Migrating database to version 34 (adding transition_history_json to task_runs)");

            conn.execute_batch(
                r#"
                -- Add transition_history_json column for orchestrator state transition history
                ALTER TABLE task_runs ADD COLUMN transition_history_json TEXT;

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (34, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 34: {}", e))?;

            info!("Successfully migrated to version 34 (transition_history_json for task_runs)");
        }

        // Version 35: Add check_groups tables for organizing checks
        if current_version < 35 {
            info!("Migrating database to version 35 (adding check_groups infrastructure)");

            conn.execute_batch(
                r#"
                -- Check Groups (organize checks into reusable groups)
                CREATE TABLE IF NOT EXISTS check_groups (
                    id TEXT PRIMARY KEY,
                    name TEXT NOT NULL,
                    description TEXT,
                    color TEXT,
                    enabled BOOLEAN NOT NULL DEFAULT 1,
                    run_in_parallel BOOLEAN NOT NULL DEFAULT 0,
                    stop_on_failure BOOLEAN NOT NULL DEFAULT 1,
                    tags TEXT DEFAULT '[]',
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL
                );

                CREATE INDEX IF NOT EXISTS idx_check_groups_enabled ON check_groups(enabled);

                -- Check Group Members (many-to-many relationship)
                CREATE TABLE IF NOT EXISTS check_group_members (
                    id TEXT PRIMARY KEY,
                    group_id TEXT NOT NULL,
                    check_id TEXT NOT NULL,
                    sort_order INTEGER NOT NULL DEFAULT 0,
                    created_at TEXT NOT NULL,
                    FOREIGN KEY (group_id) REFERENCES check_groups(id) ON DELETE CASCADE,
                    FOREIGN KEY (check_id) REFERENCES checks(id) ON DELETE CASCADE,
                    UNIQUE(group_id, check_id)
                );

                CREATE INDEX IF NOT EXISTS idx_check_group_members_group_id ON check_group_members(group_id);
                CREATE INDEX IF NOT EXISTS idx_check_group_members_check_id ON check_group_members(check_id);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (35, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 35: {}", e))?;

            info!("Successfully migrated to version 35 (check_groups infrastructure)");
        }

        // Migration to version 36: Add workflow_verification_phase_results table
        // Stores step-executor-based verification results from unified workflow execution
        if (1..36).contains(&current_version) {
            info!(
                "Migrating database to version 36 (adding workflow_verification_phase_results table)"
            );
            conn.execute_batch(
                r#"
                -- Workflow Verification Phase Results
                -- Stores results from execute_verification_steps in unified workflow execution
                CREATE TABLE IF NOT EXISTS workflow_verification_phase_results (
                    id TEXT PRIMARY KEY,
                    task_run_id TEXT NOT NULL,
                    iteration INTEGER NOT NULL,

                    -- Summary fields
                    all_passed BOOLEAN NOT NULL,
                    total_steps INTEGER NOT NULL,
                    passed_steps INTEGER NOT NULL,
                    failed_steps INTEGER NOT NULL,
                    skipped_steps INTEGER NOT NULL,
                    total_duration_ms INTEGER NOT NULL,
                    critical_failure BOOLEAN NOT NULL DEFAULT 0,

                    -- Full result as JSON (for detailed access)
                    result_json TEXT NOT NULL,

                    created_at TEXT NOT NULL,

                    FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE
                );

                CREATE INDEX IF NOT EXISTS idx_wf_ver_phase_task_run_id ON workflow_verification_phase_results(task_run_id);
                CREATE INDEX IF NOT EXISTS idx_wf_ver_phase_iteration ON workflow_verification_phase_results(iteration);
                CREATE INDEX IF NOT EXISTS idx_wf_ver_phase_all_passed ON workflow_verification_phase_results(all_passed);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (36, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 36: {}", e))?;

            info!(
                "Successfully migrated to version 36 (workflow_verification_phase_results table)"
            );
        }

        // Migration to version 37: Add unique constraint on (task_run_id, iteration) for verification results
        if current_version < 37 {
            info!(
                "Migrating database to version 37 (unique constraint for verification phase results)"
            );

            // First, remove any duplicate rows keeping only the latest per (task_run_id, iteration)
            conn.execute_batch(
                r#"
                -- Delete duplicates, keeping only the row with the latest created_at for each (task_run_id, iteration)
                DELETE FROM workflow_verification_phase_results
                WHERE id NOT IN (
                    SELECT id FROM (
                        SELECT id, ROW_NUMBER() OVER (
                            PARTITION BY task_run_id, iteration
                            ORDER BY created_at DESC
                        ) as rn
                        FROM workflow_verification_phase_results
                    ) WHERE rn = 1
                );

                -- Now add the unique constraint
                CREATE UNIQUE INDEX IF NOT EXISTS idx_wf_ver_phase_unique
                ON workflow_verification_phase_results(task_run_id, iteration);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (37, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 37: {}", e))?;

            info!("Successfully migrated to version 37 (unique constraint for verification phase results)");
        }

        // Migration to version 38: Add workflow_type column to task_runs
        // This enables external code to check if a task is a "unified" workflow
        // before modifying its status. Unified workflows should only have status
        // modified by the LoopController, not by TaskMonitor or legacy session code.
        if current_version < 38 {
            info!("Migrating database to version 38 (adding workflow_type to task_runs)");

            conn.execute_batch(
                r#"
                -- Add workflow_type column: 'unified', 'legacy_session', 'automation_only', or NULL (legacy)
                -- Unified workflows should only have status modified by LoopController
                ALTER TABLE task_runs ADD COLUMN workflow_type TEXT;

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (38, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 38: {}", e))?;

            info!("Successfully migrated to version 38 (workflow_type column added)");
        }

        // Migration to version 39: Add task hierarchy fields to task_runs
        // Enables nested subtasks with parent/root/depth tracking for complex workflows
        if current_version < 39 {
            info!("Migrating database to version 39 (adding task hierarchy fields to task_runs)");

            conn.execute_batch(
                r#"
                -- Add parent_task_run_id column: links subtasks to their parent task
                ALTER TABLE task_runs ADD COLUMN parent_task_run_id TEXT REFERENCES task_runs(id) ON DELETE SET NULL;

                -- Add root_task_run_id column: links to the root of the task hierarchy (same as id for root tasks)
                ALTER TABLE task_runs ADD COLUMN root_task_run_id TEXT;

                -- Add depth column: nesting depth (0 = root/top-level)
                ALTER TABLE task_runs ADD COLUMN depth INTEGER DEFAULT 0;

                -- Create indexes for querying child/subtasks
                CREATE INDEX IF NOT EXISTS idx_task_runs_parent_task_run_id ON task_runs(parent_task_run_id);
                CREATE INDEX IF NOT EXISTS idx_task_runs_root_task_run_id ON task_runs(root_task_run_id);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (39, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 39: {}", e))?;

            info!("Successfully migrated to version 39 (task hierarchy fields added)");
        }

        // Migration to version 40: Add workspace_id and triggered_by to task_runs
        // Enables integration with qontinui-web backend for multi-tenant workspaces
        if current_version < 40 {
            info!(
                "Migrating database to version 40 (adding workspace_id and triggered_by to task_runs)"
            );

            conn.execute_batch(
                r#"
                -- Add workspace_id column: links task to a workspace/organization from qontinui-web
                ALTER TABLE task_runs ADD COLUMN workspace_id TEXT;

                -- Add triggered_by column: identifies who/what triggered the task run
                ALTER TABLE task_runs ADD COLUMN triggered_by TEXT;

                -- Create index for workspace queries
                CREATE INDEX IF NOT EXISTS idx_task_runs_workspace_id ON task_runs(workspace_id);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (40, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 40: {}", e))?;

            info!("Successfully migrated to version 40 (workspace_id and triggered_by added)");
        }

        // Migration to version 41: Add log_watch_enabled to unified_workflows
        // Enables automatic log error detection during verification phase
        if current_version < 41 {
            info!(
                "Migrating database to version 41 (adding log_watch_enabled to unified_workflows)"
            );

            conn.execute_batch(
                r#"
                -- Add log_watch_enabled column: enables automatic log error detection
                -- Default to 1 (enabled) for all existing and new workflows
                ALTER TABLE unified_workflows ADD COLUMN log_watch_enabled INTEGER DEFAULT 1;

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (41, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 41: {}", e))?;

            info!("Successfully migrated to version 41 (log_watch_enabled added)");
        }

        // Migration to version 42: Add health_check_enabled to unified_workflows
        // Enables automatic server health checks (backend/frontend) before verification
        if current_version < 42 {
            info!(
                "Migrating database to version 42 (adding health_check_enabled to unified_workflows)"
            );

            conn.execute_batch(
                r#"
                -- Add health_check_enabled column: enables automatic server health checks
                -- Default to 1 (enabled) for all existing and new workflows
                ALTER TABLE unified_workflows ADD COLUMN health_check_enabled INTEGER DEFAULT 1;

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (42, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 42: {}", e))?;

            info!("Successfully migrated to version 42 (health_check_enabled added)");
        }

        // Migration to version 43: Add health_check_urls to unified_workflows
        // User-configurable health check URLs (replaces hardcoded backend/frontend checks)
        if current_version < 43 {
            info!(
                "Migrating database to version 43 (adding health_check_urls to unified_workflows)"
            );

            conn.execute_batch(
                r#"
                -- Add health_check_urls column: JSON array of health check configurations
                -- Each entry: { name, url, expected_status, timeout_seconds, is_critical }
                -- Default to empty array (no health checks configured)
                ALTER TABLE unified_workflows ADD COLUMN health_check_urls TEXT DEFAULT '[]';

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (43, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 43: {}", e))?;

            info!("Successfully migrated to version 43 (health_check_urls added)");
        }

        // Migration to version 44: Add error monitoring system
        // Captures errors from user-configured application logs for debug agent integration
        if current_version < 44 {
            info!("Migrating database to version 44 (adding error monitoring system)");

            conn.execute_batch(
                r#"
                -- Log Sources: User-configured application log files to monitor
                CREATE TABLE IF NOT EXISTS log_sources (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    name TEXT NOT NULL UNIQUE,
                    description TEXT,
                    path TEXT NOT NULL,
                    path_type TEXT DEFAULT 'file',
                    format TEXT DEFAULT 'plaintext',
                    parser TEXT DEFAULT 'generic',
                    timestamp_pattern TEXT,
                    timezone TEXT DEFAULT 'local',
                    error_patterns TEXT,
                    warning_patterns TEXT,
                    ignore_patterns TEXT,
                    enabled INTEGER DEFAULT 1,
                    poll_interval_ms INTEGER DEFAULT 5000,
                    created_at TEXT NOT NULL DEFAULT (datetime('now')),
                    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
                );

                CREATE INDEX IF NOT EXISTS idx_log_sources_name ON log_sources(name);
                CREATE INDEX IF NOT EXISTS idx_log_sources_enabled ON log_sources(enabled);

                -- Error Events: Captured errors from application logs
                CREATE TABLE IF NOT EXISTS error_events (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    log_source_id INTEGER REFERENCES log_sources(id) ON DELETE SET NULL,
                    log_source_name TEXT NOT NULL,
                    task_run_id TEXT REFERENCES task_runs(id) ON DELETE SET NULL,
                    workflow_step_id TEXT,
                    log_timestamp TEXT,
                    captured_at TEXT NOT NULL DEFAULT (datetime('now')),
                    severity TEXT NOT NULL DEFAULT 'error',
                    error_type TEXT,
                    error_code TEXT,
                    message TEXT NOT NULL,
                    stack_trace TEXT,
                    context_lines TEXT,
                    raw_entry TEXT,
                    file_path TEXT,
                    line_number INTEGER,
                    column_number INTEGER,
                    function_name TEXT,
                    signature_hash TEXT NOT NULL,
                    occurrence_count INTEGER DEFAULT 1,
                    first_seen_at TEXT NOT NULL DEFAULT (datetime('now')),
                    last_seen_at TEXT NOT NULL DEFAULT (datetime('now')),
                    status TEXT DEFAULT 'new',
                    finding_id INTEGER REFERENCES task_run_findings(id) ON DELETE SET NULL,
                    resolved_by_task_run_id TEXT,
                    resolution_notes TEXT,
                    acknowledged_at TEXT,
                    resolved_at TEXT
                );

                CREATE INDEX IF NOT EXISTS idx_error_events_log_source ON error_events(log_source_id);
                CREATE INDEX IF NOT EXISTS idx_error_events_task_run ON error_events(task_run_id);
                CREATE INDEX IF NOT EXISTS idx_error_events_signature ON error_events(signature_hash);
                CREATE INDEX IF NOT EXISTS idx_error_events_status ON error_events(status);
                CREATE INDEX IF NOT EXISTS idx_error_events_severity ON error_events(severity);
                CREATE INDEX IF NOT EXISTS idx_error_events_captured ON error_events(captured_at DESC);
                CREATE INDEX IF NOT EXISTS idx_error_events_last_seen ON error_events(last_seen_at DESC);
                CREATE INDEX IF NOT EXISTS idx_error_events_source_name ON error_events(log_source_name);

                -- Full-text search for error messages
                CREATE VIRTUAL TABLE IF NOT EXISTS error_events_fts USING fts5(
                    message,
                    stack_trace,
                    error_type,
                    content='error_events',
                    content_rowid='id'
                );

                -- FTS sync triggers
                CREATE TRIGGER IF NOT EXISTS error_events_ai AFTER INSERT ON error_events BEGIN
                    INSERT INTO error_events_fts(rowid, message, stack_trace, error_type)
                    VALUES (new.id, new.message, new.stack_trace, new.error_type);
                END;

                CREATE TRIGGER IF NOT EXISTS error_events_ad AFTER DELETE ON error_events BEGIN
                    INSERT INTO error_events_fts(error_events_fts, rowid, message, stack_trace, error_type)
                    VALUES ('delete', old.id, old.message, old.stack_trace, old.error_type);
                END;

                CREATE TRIGGER IF NOT EXISTS error_events_au AFTER UPDATE ON error_events BEGIN
                    INSERT INTO error_events_fts(error_events_fts, rowid, message, stack_trace, error_type)
                    VALUES ('delete', old.id, old.message, old.stack_trace, old.error_type);
                    INSERT INTO error_events_fts(rowid, message, stack_trace, error_type)
                    VALUES (new.id, new.message, new.stack_trace, new.error_type);
                END;

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (44, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 44: {}", e))?;

            info!("Successfully migrated to version 44 (error monitoring system added)");
        }

        // Version 45: Add bridge_id to task_runs for multi-bridge support
        if current_version < 45 {
            let _ = conn.execute("ALTER TABLE task_runs ADD COLUMN bridge_id TEXT", []);

            conn.execute(
                "CREATE INDEX IF NOT EXISTS idx_task_runs_bridge_id ON task_runs(bridge_id)",
                [],
            )
            .map_err(|e| format!("Failed to create bridge_id index: {}", e))?;

            conn.execute(
                "INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (45, datetime('now'))",
                [],
            )
            .map_err(|e| format!("Failed to migrate to version 45: {}", e))?;

            info!("Successfully migrated to version 45 (bridge_id added to task_runs)");
        }

        // Migration to version 46: Add flow_versions table for version history
        if current_version < 46 {
            info!("Migrating database to version 46 (adding flow_versions table)");

            conn.execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS flow_versions (
                    id TEXT PRIMARY KEY,
                    flow_id TEXT NOT NULL,
                    version INTEGER NOT NULL,
                    definition TEXT NOT NULL,
                    message TEXT,
                    created_by TEXT,
                    created_at TEXT NOT NULL,
                    FOREIGN KEY (flow_id) REFERENCES orchestrator_flows(id) ON DELETE CASCADE,
                    UNIQUE(flow_id, version)
                );
                CREATE INDEX IF NOT EXISTS idx_flow_versions_flow_id ON flow_versions(flow_id);
                CREATE INDEX IF NOT EXISTS idx_flow_versions_flow_version ON flow_versions(flow_id, version);
                "#,
            )
            .map_err(|e| format!("Failed to create flow_versions table: {}", e))?;

            conn.execute(
                "INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (46, datetime('now'))",
                [],
            )
            .map_err(|e| format!("Failed to migrate to version 46: {}", e))?;

            info!("Successfully migrated to version 46 (flow_versions table added)");
        }

        // Migration to version 47: Add timeout_seconds to unified_workflows
        if (1..47).contains(&current_version) {
            info!("Migrating database to version 47 (add timeout_seconds to unified_workflows)");

            conn.execute_batch(
                r#"
                ALTER TABLE unified_workflows ADD COLUMN timeout_seconds INTEGER DEFAULT NULL;

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (47, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 47: {}", e))?;

            info!(
                "Successfully migrated to version 47 (timeout_seconds added to unified_workflows)"
            );
        }

        // Migration to version 48: Add workflow state management tables
        if (1..48).contains(&current_version) {
            info!("Migrating database to version 48 (workflow state management tables)");

            conn.execute_batch(
                r#"
                -- Workflow Execution State: Explicit state tracking for workflows
                CREATE TABLE IF NOT EXISTS workflow_execution_state (
                    execution_id TEXT PRIMARY KEY,
                    workflow_type TEXT NOT NULL,
                    state_name TEXT NOT NULL,
                    state_data TEXT,
                    phase TEXT,
                    iteration INTEGER,
                    updated_at TEXT NOT NULL,
                    FOREIGN KEY (execution_id) REFERENCES task_runs(id) ON DELETE CASCADE
                );

                CREATE INDEX IF NOT EXISTS idx_workflow_exec_state_type ON workflow_execution_state(workflow_type);
                CREATE INDEX IF NOT EXISTS idx_workflow_exec_state_name ON workflow_execution_state(state_name);

                -- Workflow Step Checkpoints: Step-level checkpointing for resume
                CREATE TABLE IF NOT EXISTS workflow_step_checkpoints (
                    id TEXT PRIMARY KEY,
                    execution_id TEXT NOT NULL,
                    workflow_type TEXT NOT NULL,
                    phase TEXT NOT NULL,
                    iteration INTEGER,
                    step_index INTEGER NOT NULL,
                    step_type TEXT NOT NULL,
                    step_name TEXT,
                    status TEXT NOT NULL,
                    result_json TEXT,
                    started_at TEXT,
                    completed_at TEXT,
                    duration_ms INTEGER,
                    error TEXT,
                    FOREIGN KEY (execution_id) REFERENCES task_runs(id) ON DELETE CASCADE,
                    UNIQUE(execution_id, phase, iteration, step_index)
                );

                CREATE INDEX IF NOT EXISTS idx_step_checkpoints_execution ON workflow_step_checkpoints(execution_id);
                CREATE INDEX IF NOT EXISTS idx_step_checkpoints_lookup ON workflow_step_checkpoints(execution_id, phase, iteration);
                CREATE INDEX IF NOT EXISTS idx_step_checkpoints_status ON workflow_step_checkpoints(status);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (48, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 48: {}", e))?;

            info!("Successfully migrated to version 48 (workflow state management tables)");
        }

        // Migration to version 49: Add step_config_json column to workflow_step_checkpoints
        // This provides a single source of truth for step configuration instead of
        // duplicating data between checkpoints and task_runs.execution_steps_json
        if (1..49).contains(&current_version) {
            info!("Migrating database to version 49 (add step_config_json and cursor index)");

            conn.execute_batch(
                r#"
                ALTER TABLE workflow_step_checkpoints ADD COLUMN step_config_json TEXT;

                -- Add index for cursor-based pagination
                CREATE INDEX IF NOT EXISTS idx_step_checkpoints_cursor ON workflow_step_checkpoints(execution_id, step_index);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (49, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 49: {}", e))?;

            info!("Successfully migrated to version 49 (step_config_json column added)");
        }

        // Migration to version 50: Add step_progress_markers table for intra-step progress tracking
        // This enables tracking progress within long AI operations (e.g., "analyzed 50/100 files")
        if (1..50).contains(&current_version) {
            info!("Migrating database to version 50 (add step_progress_markers table)");

            conn.execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS step_progress_markers (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    checkpoint_id TEXT NOT NULL,
                    marker_type TEXT NOT NULL,
                    current_value INTEGER NOT NULL,
                    total_value INTEGER,
                    description TEXT,
                    data_json TEXT,
                    created_at TEXT NOT NULL,
                    FOREIGN KEY (checkpoint_id) REFERENCES workflow_step_checkpoints(id) ON DELETE CASCADE
                );

                CREATE INDEX IF NOT EXISTS idx_progress_markers_checkpoint ON step_progress_markers(checkpoint_id);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (50, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 50: {}", e))?;

            info!("Successfully migrated to version 50 (step_progress_markers table added)");
        }

        // Migration to version 51: Change timeout_seconds from NOT NULL to nullable
        // This disables default timeouts - None means no timeout (user must explicitly set one)
        // Fixes: "The AI step reports that multiple checks timed out after 60 seconds"
        //
        // SQLite doesn't support ALTER COLUMN to remove NOT NULL, so we use the
        // rename-recreate pattern: create new table, copy data, drop old, rename new.
        if (1..51).contains(&current_version) {
            info!("Migrating database to version 51 (disable default timeouts)");

            // Step 1: Recreate 'checks' table without NOT NULL on timeout_seconds
            conn.execute_batch(
                r#"
                -- Create new checks table with nullable timeout_seconds
                CREATE TABLE IF NOT EXISTS checks_new (
                    id TEXT PRIMARY KEY,
                    name TEXT NOT NULL,
                    description TEXT,
                    check_type TEXT NOT NULL,
                    tool TEXT NOT NULL,
                    command TEXT,
                    working_directory TEXT,
                    config_path TEXT,
                    auto_fix BOOLEAN NOT NULL DEFAULT 0,
                    fail_on_warning BOOLEAN NOT NULL DEFAULT 0,
                    timeout_seconds INTEGER,  -- Now nullable (NULL = no timeout)
                    is_critical BOOLEAN NOT NULL DEFAULT 0,
                    enabled BOOLEAN NOT NULL DEFAULT 1,
                    ai_generated BOOLEAN NOT NULL DEFAULT 0,
                    ai_generation_prompt TEXT,
                    tags TEXT DEFAULT '[]',
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL
                );

                -- Copy data, converting 60 to NULL
                INSERT INTO checks_new SELECT
                    id, name, description, check_type, tool, command,
                    working_directory, config_path, auto_fix, fail_on_warning,
                    CASE WHEN timeout_seconds = 60 THEN NULL ELSE timeout_seconds END,
                    is_critical, enabled, ai_generated, ai_generation_prompt,
                    tags, created_at, updated_at
                FROM checks;

                -- Drop old table and rename new
                DROP TABLE checks;
                ALTER TABLE checks_new RENAME TO checks;

                -- Recreate indexes
                CREATE INDEX IF NOT EXISTS idx_checks_check_type ON checks(check_type);
                CREATE INDEX IF NOT EXISTS idx_checks_tool ON checks(tool);
                CREATE INDEX IF NOT EXISTS idx_checks_enabled ON checks(enabled);
                CREATE INDEX IF NOT EXISTS idx_checks_created_at ON checks(created_at);
                CREATE INDEX IF NOT EXISTS idx_checks_updated_at ON checks(updated_at);
                "#,
            )
            .map_err(|e| format!("Failed to migrate checks table to version 51: {}", e))?;

            // Step 2: Recreate 'verification_tests' table without NOT NULL on timeout_seconds
            conn.execute_batch(
                r#"
                -- Create new verification_tests table with nullable timeout_seconds
                CREATE TABLE IF NOT EXISTS verification_tests_new (
                    id TEXT PRIMARY KEY,
                    name TEXT NOT NULL,
                    description TEXT,
                    test_type TEXT NOT NULL,
                    category TEXT,
                    playwright_code TEXT,
                    vision_config TEXT,
                    python_code TEXT,
                    repo_test_config TEXT,
                    success_criteria TEXT,
                    config TEXT DEFAULT '{}',
                    timeout_seconds INTEGER,  -- Now nullable (NULL = no timeout)
                    is_critical BOOLEAN NOT NULL DEFAULT 1,
                    enabled BOOLEAN NOT NULL DEFAULT 1,
                    ai_generated BOOLEAN NOT NULL DEFAULT 0,
                    ai_generation_prompt TEXT,
                    tags TEXT DEFAULT '[]',
                    source_file TEXT,
                    last_exported_at TEXT,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL
                );

                -- Copy data, converting 60 to NULL
                INSERT INTO verification_tests_new SELECT
                    id, name, description, test_type, category,
                    playwright_code, vision_config, python_code, repo_test_config,
                    success_criteria, config,
                    CASE WHEN timeout_seconds = 60 THEN NULL ELSE timeout_seconds END,
                    is_critical, enabled, ai_generated, ai_generation_prompt,
                    tags, source_file, last_exported_at, created_at, updated_at
                FROM verification_tests;

                -- Drop old table and rename new
                DROP TABLE verification_tests;
                ALTER TABLE verification_tests_new RENAME TO verification_tests;

                -- Recreate indexes
                CREATE INDEX IF NOT EXISTS idx_verification_tests_test_type ON verification_tests(test_type);
                CREATE INDEX IF NOT EXISTS idx_verification_tests_category ON verification_tests(category);
                CREATE INDEX IF NOT EXISTS idx_verification_tests_enabled ON verification_tests(enabled);
                CREATE INDEX IF NOT EXISTS idx_verification_tests_created_at ON verification_tests(created_at);
                CREATE INDEX IF NOT EXISTS idx_verification_tests_updated_at ON verification_tests(updated_at);
                "#,
            )
            .map_err(|e| format!("Failed to migrate verification_tests table to version 51: {}", e))?;

            // Step 3: Update shell_commands (already allows NULL, just update values)
            conn.execute_batch(
                r#"
                UPDATE shell_commands SET timeout_seconds = NULL WHERE timeout_seconds = 30;

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (51, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate shell_commands to version 51: {}", e))?;

            info!("Successfully migrated to version 51 (default timeouts disabled)");
        }

        // Migration to version 52: Add execution_spans table for tracing
        if current_version < 52 {
            info!("Migrating to version 52 (execution spans table)...");

            conn.execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS execution_spans (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    execution_id TEXT,
                    trace_id TEXT NOT NULL,
                    span_id TEXT NOT NULL,
                    parent_span_id TEXT,
                    name TEXT NOT NULL,
                    start_ts TEXT NOT NULL,
                    end_ts TEXT,
                    duration_ms INTEGER,
                    attributes TEXT,
                    success INTEGER DEFAULT 1,
                    error TEXT,
                    created_at TEXT NOT NULL,
                    FOREIGN KEY (execution_id) REFERENCES task_runs(id) ON DELETE CASCADE
                );

                CREATE INDEX IF NOT EXISTS idx_spans_execution ON execution_spans(execution_id);
                CREATE INDEX IF NOT EXISTS idx_spans_trace ON execution_spans(trace_id);
                CREATE INDEX IF NOT EXISTS idx_spans_name ON execution_spans(name);
                CREATE INDEX IF NOT EXISTS idx_spans_start ON execution_spans(start_ts);
                CREATE INDEX IF NOT EXISTS idx_spans_duration ON execution_spans(duration_ms);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (52, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to create execution_spans table (version 52): {}", e))?;

            info!("Successfully migrated to version 52 (execution spans table)");
        }

        // Migration to version 53: Add preflight_check_enabled to unified_workflows
        if current_version < 53 {
            info!("Migrating database to version 53 (adding preflight_check_enabled to unified_workflows)");

            // Check if column already exists (idempotent migration)
            let column_exists: bool = conn
                .query_row(
                    "SELECT COUNT(*) FROM pragma_table_info('unified_workflows') WHERE name = 'preflight_check_enabled'",
                    [],
                    |row| row.get::<_, i32>(0),
                )
                .map(|count| count > 0)
                .unwrap_or(false);

            if !column_exists {
                conn.execute(
                    "ALTER TABLE unified_workflows ADD COLUMN preflight_check_enabled INTEGER DEFAULT 1",
                    [],
                )
                .map_err(|e| format!("Failed to add preflight_check_enabled column: {}", e))?;
                info!("Added preflight_check_enabled column to unified_workflows");
            } else {
                info!("Column preflight_check_enabled already exists, skipping ALTER TABLE");
            }

            conn.execute(
                "INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (53, datetime('now'))",
                [],
            )
            .map_err(|e| format!("Failed to update schema version to 53: {}", e))?;

            info!("Successfully migrated to version 53 (preflight_check_enabled)");
        }

        // Migration to version 54: Add result_data to task_runs, generated_by_task_run_id to unified_workflows
        if current_version < 54 {
            info!("Migrating database to version 54 (result_data, generated_by_task_run_id)");
            conn.execute_batch(
                r#"
                -- Structured result data for task runs (JSON blob for meta-workflow outputs)
                ALTER TABLE task_runs ADD COLUMN result_data TEXT;

                -- Links generated workflows back to the meta-workflow task run that created them
                ALTER TABLE unified_workflows ADD COLUMN generated_by_task_run_id TEXT REFERENCES task_runs(id) ON DELETE SET NULL;

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (54, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 54: {}", e))?;

            info!(
                "Successfully migrated to version 54 (result_data, generated_by_task_run_id added)"
            );
        }

        // Migration to version 55: Add embedding BLOB columns for hybrid RAG search + feedback table
        if current_version < 55 {
            info!("Migrating database to version 55 (embedding columns + workflow_generation_feedback)");
            conn.execute_batch(
                r#"
                -- Embedding BLOB columns for hybrid RAG search (384-dim MiniLM, 1536 bytes each)
                ALTER TABLE task_runs ADD COLUMN prompt_embedding BLOB;
                ALTER TABLE task_runs ADD COLUMN summary_embedding BLOB;

                ALTER TABLE task_run_findings ADD COLUMN title_embedding BLOB;
                ALTER TABLE task_run_findings ADD COLUMN description_embedding BLOB;

                ALTER TABLE task_knowledge ADD COLUMN content_embedding BLOB;

                ALTER TABLE unified_workflows ADD COLUMN description_embedding BLOB;

                ALTER TABLE learning_outcomes ADD COLUMN context_embedding BLOB;

                ALTER TABLE learning_patterns ADD COLUMN description_embedding BLOB;

                ALTER TABLE error_events ADD COLUMN message_embedding BLOB;

                -- Workflow generation feedback table
                CREATE TABLE IF NOT EXISTS workflow_generation_feedback (
                    id TEXT PRIMARY KEY,
                    workflow_id TEXT NOT NULL,
                    task_run_id TEXT,
                    feedback_type TEXT NOT NULL,
                    edited_field TEXT,
                    old_value TEXT,
                    new_value TEXT,
                    delete_reason TEXT,
                    rating INTEGER,
                    rating_comment TEXT,
                    workflow_category TEXT,
                    workflow_description TEXT,
                    created_at TEXT NOT NULL DEFAULT (datetime('now')),
                    FOREIGN KEY (workflow_id) REFERENCES unified_workflows(id) ON DELETE CASCADE,
                    FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE SET NULL
                );
                CREATE INDEX IF NOT EXISTS idx_wgf_workflow_id ON workflow_generation_feedback(workflow_id);
                CREATE INDEX IF NOT EXISTS idx_wgf_task_run_id ON workflow_generation_feedback(task_run_id);
                CREATE INDEX IF NOT EXISTS idx_wgf_feedback_type ON workflow_generation_feedback(feedback_type);
                CREATE INDEX IF NOT EXISTS idx_wgf_created_at ON workflow_generation_feedback(created_at);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (55, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 55: {}", e))?;

            info!("Successfully migrated to version 55 (embedding columns + workflow_generation_feedback)");
        }

        // Version 56: Add sync_pending column to unified_workflows for offline cache sync
        if current_version < 56 {
            info!("Migrating to version 56 (unified_workflows sync_pending)...");
            conn.execute_batch(
                r#"
                ALTER TABLE unified_workflows ADD COLUMN sync_pending INTEGER DEFAULT 0;
                CREATE INDEX IF NOT EXISTS idx_unified_workflows_sync_pending ON unified_workflows(sync_pending);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (56, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 56: {}", e))?;

            info!("Successfully migrated to version 56 (unified_workflows sync_pending)");
        }

        // Migration to version 57: Add example_status column to unified_workflows
        // Tracks whether a workflow is in the example library for RAG-based generation
        if current_version < 57 {
            info!("Migrating database to version 57 (add example_status to unified_workflows)");
            conn.execute_batch(
                r#"
                ALTER TABLE unified_workflows ADD COLUMN example_status TEXT DEFAULT 'pending';
                CREATE INDEX IF NOT EXISTS idx_unified_workflows_example_status ON unified_workflows(example_status);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (57, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 57: {}", e))?;

            info!("Successfully migrated to version 57 (example_status column added)");
        }

        // Migration to version 58: Reflection workflow system
        // Adds reflection_fixes table, is_reflection/reflection_source_task_run_id on task_runs,
        // and reflection_fix_id on task_run_findings and task_knowledge.
        if current_version < 58 {
            info!("Migrating database to version 58 (reflection workflow system)");
            conn.execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS reflection_fixes (
                    id TEXT PRIMARY KEY,
                    source_task_run_id TEXT NOT NULL,
                    reflection_task_run_id TEXT NOT NULL,
                    source_finding_id TEXT,
                    source_knowledge_id TEXT,
                    fix_type TEXT NOT NULL,
                    fix_description TEXT NOT NULL,
                    file_changed TEXT,
                    old_value TEXT,
                    new_value TEXT,
                    confidence TEXT NOT NULL DEFAULT 'medium',
                    status TEXT NOT NULL DEFAULT 'applied',
                    effectiveness TEXT,
                    effectiveness_evidence TEXT,
                    applied_at TEXT NOT NULL,
                    evaluated_at TEXT,
                    created_at TEXT NOT NULL,
                    FOREIGN KEY (source_task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE,
                    FOREIGN KEY (reflection_task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE,
                    FOREIGN KEY (source_finding_id) REFERENCES task_run_findings(id) ON DELETE SET NULL,
                    FOREIGN KEY (source_knowledge_id) REFERENCES task_knowledge(id) ON DELETE SET NULL
                );

                CREATE INDEX IF NOT EXISTS idx_reflection_fixes_source ON reflection_fixes(source_task_run_id);
                CREATE INDEX IF NOT EXISTS idx_reflection_fixes_reflection ON reflection_fixes(reflection_task_run_id);
                CREATE INDEX IF NOT EXISTS idx_reflection_fixes_status ON reflection_fixes(status);
                CREATE INDEX IF NOT EXISTS idx_reflection_fixes_effectiveness ON reflection_fixes(effectiveness);
                CREATE INDEX IF NOT EXISTS idx_reflection_fixes_applied_at ON reflection_fixes(applied_at);

                ALTER TABLE task_runs ADD COLUMN is_reflection INTEGER DEFAULT 0;
                ALTER TABLE task_runs ADD COLUMN reflection_source_task_run_id TEXT;

                ALTER TABLE task_run_findings ADD COLUMN reflection_fix_id TEXT;
                ALTER TABLE task_knowledge ADD COLUMN reflection_fix_id TEXT;

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (58, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 58: {}", e))?;

            info!("Successfully migrated to version 58 (reflection workflow system)");
        }

        // Version 59: Reflection deduplication, auto-apply support, and stale fix archival
        if current_version < 59 {
            info!("Migrating database to version 59 (reflection dedup & auto-apply)");

            // Check if content_hash column already exists (idempotent migration)
            let column_exists: bool = conn
                .query_row(
                    "SELECT COUNT(*) FROM pragma_table_info('reflection_fixes') WHERE name = 'content_hash'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .map(|count| count > 0)
                .unwrap_or(false);

            if !column_exists {
                conn.execute(
                    "ALTER TABLE reflection_fixes ADD COLUMN content_hash TEXT",
                    [],
                )
                .map_err(|e| format!("Failed to add content_hash column: {}", e))?;
            }

            conn.execute_batch(
                r#"
                CREATE INDEX IF NOT EXISTS idx_reflection_fixes_content_hash ON reflection_fixes(content_hash);

                -- Archive stale template variable resolution fixes (resolved by code fix)
                UPDATE reflection_fixes SET status = 'superseded'
                WHERE fix_description LIKE '%template variable%' AND status = 'applied';

                -- Archive stale UI Bridge API migration fixes (already absorbed)
                UPDATE reflection_fixes SET status = 'superseded'
                WHERE (fix_description LIKE '%@qontinui/ui-bridge%' OR fix_description LIKE '%CapturedError%' OR fix_description LIKE '%onBrowserEvent%')
                AND status = 'applied';

                -- Archive duplicate finding dedup instruction fixes (issue resolved)
                UPDATE reflection_fixes SET status = 'superseded'
                WHERE fix_description LIKE '%duplicate%FINDING%' AND status = 'applied';

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (59, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 59: {}", e))?;

            info!("Successfully migrated to version 59 (reflection dedup & auto-apply)");
        }

        // Version 60: Generation rules table — externalized workflow generation rules
        if current_version < 60 {
            info!("Migrating database to version 60 (generation_rules table)");

            // Create the table
            conn.execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS generation_rules (
                    id TEXT PRIMARY KEY,
                    agent TEXT NOT NULL,
                    section TEXT NOT NULL,
                    rule_number INTEGER NOT NULL,
                    title TEXT NOT NULL,
                    content TEXT NOT NULL,
                    condition TEXT,
                    status TEXT NOT NULL DEFAULT 'active',
                    provenance TEXT NOT NULL DEFAULT 'seed',
                    source_fix_id TEXT,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL,
                    FOREIGN KEY (source_fix_id) REFERENCES reflection_fixes(id) ON DELETE SET NULL
                );

                CREATE INDEX IF NOT EXISTS idx_generation_rules_agent ON generation_rules(agent);
                CREATE INDEX IF NOT EXISTS idx_generation_rules_status ON generation_rules(status);
                CREATE INDEX IF NOT EXISTS idx_generation_rules_agent_section ON generation_rules(agent, section, rule_number);
                "#,
            )
            .map_err(|e| format!("Failed to create generation_rules table (version 60): {}", e))?;

            // Seed schema_context important_rules (rules 1-5)
            let now = Utc::now().to_rfc3339();
            let seed_rules: Vec<(&str, &str, &str, i32, &str, &str, Option<&str>)> = vec![
                // schema_context / important_rules
                ("seed-schema_context-important_rules-1", "schema_context", "important_rules", 1,
                 "Generate valid UUIDs",
                 "Generate valid UUIDs for all `id` fields (format: xxxxxxxx-xxxx-4xxx-yxxx-xxxxxxxxxxxx)",
                 None),
                ("seed-schema_context-important_rules-2", "schema_context", "important_rules", 2,
                 "Phase field must match array",
                 "`phase` field MUST match the array the step is in (setup_steps -> \"setup\", etc.)",
                 None),
                ("seed-schema_context-important_rules-3", "schema_context", "important_rules", 3,
                 "Agentic steps are prompt only",
                 "`agentic_steps` can ONLY contain `prompt` type steps",
                 None),
                ("seed-schema_context-important_rules-4", "schema_context", "important_rules", 4,
                 "ISO 8601 timestamps",
                 "Use ISO 8601 format for timestamps (e.g., \"2024-01-15T10:30:00Z\")",
                 None),
                ("seed-schema_context-important_rules-5", "schema_context", "important_rules", 5,
                 "JSON only output",
                 "Return ONLY the JSON object, no markdown formatting",
                 None),

                // schema_context / verification_quality (rules 6-16)
                ("seed-schema_context-verification_quality-6", "schema_context", "verification_quality", 6,
                 "Deterministic verification step required",
                 "verification_steps MUST include at least one deterministic, automated step — a `command` step (with check_type, test_type, or a shell command) or a `ui_bridge` step (with assert action). Do NOT use only `prompt` type steps for verification. Prompts provide AI judgment, not deterministic pass/fail results. A verification phase with ONLY prompt steps is INVALID.",
                 None),
                ("seed-schema_context-verification_quality-7", "schema_context", "verification_quality", 7,
                 "Code modification requires typecheck",
                 "When the workflow creates or modifies source code files (TypeScript, Python, Rust, etc.), verification MUST include a `command` step with `check_type` set to the appropriate type checker:\n   - TypeScript/TSX/JSX: `{\"type\": \"command\", \"check_type\": \"typecheck\", \"command\": \"npx tsc --noEmit\", \"working_directory\": \"...\"}`\n   - Python: `{\"type\": \"command\", \"check_type\": \"typecheck\", \"command\": \"mypy .\", \"working_directory\": \"...\"}`\n   - Rust: `{\"type\": \"command\", \"check_type\": \"typecheck\", \"command\": \"cargo check\", \"working_directory\": \"...\"}`",
                 None),
                ("seed-schema_context-verification_quality-8", "schema_context", "verification_quality", 8,
                 "Web app verification requires SDK or Playwright",
                 "When the workflow targets a web application (localhost:3001, localhost:1420), verification MUST include at least one of:\n   - A `command` step using curl to query UI Bridge SDK endpoints (preferred) to verify UI state\n   - A `command` step with `test_type: \"playwright\"` for browser-based verification\n   - A `ui_bridge` step with an `assert` action for direct element assertions",
                 None),
                ("seed-schema_context-verification_quality-9", "schema_context", "verification_quality", 9,
                 "Verification must be deterministic",
                 "Every workflow with 2+ verification steps should ensure ALL non-prompt verification steps are meaningful and required. If a step is worth including in verification, its failure should be visible to the verification loop. Do NOT include verification steps whose failures would be silently ignored.",
                 None),
                ("seed-schema_context-verification_quality-10", "schema_context", "verification_quality", 10,
                 "Prompts are supplementary",
                 "`prompt` type steps in verification are acceptable as supplementary checks (e.g., semantic code review, cross-referencing documentation) but must NEVER be the sole verification mechanism",
                 None),
                ("seed-schema_context-verification_quality-11", "schema_context", "verification_quality", 11,
                 "Test steps with inline commands use repository",
                 "When a `command` step with `test_type` runs a shell command (e.g., `npx playwright test ...`, `cargo test ...`), set `test_type: \"repository\"`. The `test_type: \"playwright\"` value is ONLY for steps that provide `code` with Playwright assertions to be executed via CDP. Using `\"playwright\"` for shell commands causes a \"No test_id specified\" error.",
                 None),
                ("seed-schema_context-verification_quality-12", "schema_context", "verification_quality", 12,
                 "Next.js App Router path conventions",
                 "For Next.js projects using the App Router (`src/app/`), components are organized under route groups like `src/app/(app)/`. When creating `command` steps with `check_type`, use the correct working directory paths — e.g., the frontend directory, not `src/components/`. Always verify path patterns match the actual project structure.",
                 None),
                ("seed-schema_context-verification_quality-13", "schema_context", "verification_quality", 13,
                 "Verification step caching",
                 "Running tasks cache their verification steps at startup. Updating a workflow definition via the API (e.g., `PUT /unified-workflows/{id}`) does NOT affect the currently executing task's verification steps. The AI agent should NOT attempt to fix verification step configuration by updating the workflow API mid-run — instead, it should focus on fixing the underlying code issues or report the config issue for the next run.",
                 None),
                ("seed-schema_context-verification_quality-14", "schema_context", "verification_quality", 14,
                 "Feature-specific verification required",
                 "Verification plans for feature-implementation workflows MUST include feature-specific checks (element presence, tab visibility, component rendering) in addition to compilation/lint gates. Compilation-only verification causes premature completion when the AI fixes pre-existing errors without implementing the actual feature.",
                 None),
                ("seed-schema_context-verification_quality-15", "schema_context", "verification_quality", 15,
                 "SDK verification must verify content",
                 "When a `command` step calls a UI Bridge SDK endpoint via curl (`/ui-bridge/sdk/...`), checking only the exit code is INSUFFICIENT. SDK endpoints return 200 even for empty results (e.g., `ai/search` returns `{\"results\": [], \"total\": 0}`). Every SDK verification command MUST pipe to `grep` with expected text/element content to verify meaningful results.",
                 None),
                ("seed-schema_context-verification_quality-16", "schema_context", "verification_quality", 16,
                 "Agentic-verification correspondence",
                 "Each `prompt` step in `agentic_steps` describes a specific piece of work (e.g., \"implement drag-and-drop\", \"add thumbnails\"). For EACH agentic step, `verification_steps` MUST contain at least one deterministic `command` or `ui_bridge` step that verifies the output of that work.",
                 None),

                // hardener / conversion_rules
                ("seed-hardener-conversion_rules-1", "hardener", "conversion_rules", 1,
                 "Convert prompt steps to deterministic",
                 "Convert `prompt` steps to deterministic equivalents. Only 3 step types are valid: `command`, `ui_bridge`, `prompt`.\n| Prompt check type | Convert to | Method |\n|---|---|---|\n| UI element presence/structure | `command` | curl to UI Bridge SDK endpoint, pipe to grep for content check |\n| Content/text on page | `command` | curl to UI Bridge SDK `/ai/search`, pipe to grep for expected text |\n| File existence | `command` | `check_type: \"custom_command\"` with `test -f <path>` |\n| File content | `command` | `check_type: \"custom_command\"` with `grep -q <pattern> <file>` |\n| Code quality (lint) | `command` | `check_type: \"lint\"` with appropriate command |\n| Code quality (typecheck) | `command` | `check_type: \"typecheck\"` with appropriate command |\n| API health/response | `command` | curl to endpoint, check exit code |\n| UI assertion | `ui_bridge` | Use assert action with target and expected value |\n| Subjective/qualitative | Keep as `prompt` | Cannot be made deterministic |",
                 None),
                ("seed-hardener-conversion_rules-2", "hardener", "conversion_rules", 2,
                 "Replace Playwright with SDK checks",
                 "When the UI Bridge SDK is connected, Playwright-based UI verification tests should be converted to `command` steps (using curl to SDK endpoints piped to grep) or `ui_bridge` steps. The SDK provides direct programmatic access to registered UI elements without requiring a Playwright browser instance. If a single Playwright test checks multiple things, split it into multiple `command` or `ui_bridge` steps — one per distinct verification concern. Tests that require keyboard shortcuts, file uploads, or screenshot comparisons MUST remain as `command` steps with `test_type: \"playwright\"`.",
                 Some("has_sdk_connect")),
                ("seed-hardener-conversion_rules-3", "hardener", "conversion_rules", 3,
                 "Strengthen weak SDK verification commands",
                 "If an existing `command` step calls a UI Bridge SDK endpoint via curl but only checks exit code (no grep), add a pipe to `grep` to verify meaningful content. A successful curl to the SDK just means the endpoint is reachable — it doesn't verify the UI state. SDK endpoints return 200 even for EMPTY results.",
                 Some("has_sdk_connect")),
                ("seed-hardener-conversion_rules-4", "hardener", "conversion_rules", 4,
                 "Inject page navigation before SDK checks",
                 "If the workflow's setup_steps include a page navigation step (curl POST to `/ui-bridge/sdk/page/navigate` or a `ui_bridge` step with `action: \"navigate\"`), the verification phase MUST also navigate to that same URL before any SDK element checks. Use a `command` step with curl or a `ui_bridge` navigate step.",
                 Some("has_sdk_connect")),
                ("seed-hardener-conversion_rules-5", "hardener", "conversion_rules", 5,
                 "Agentic-verification correspondence",
                 "Examine EACH prompt step in `agentic_steps` and identify the distinct goals/features it describes. Then check whether `verification_steps` has at least one deterministic `command` or `ui_bridge` step that would FAIL if that specific goal was NOT implemented. For each uncovered agentic goal, ADD a new `command` verification step (e.g., curl to SDK endpoint piped to grep for expected content).",
                 None),

                // hardener / critical_rules
                ("seed-hardener-critical_rules-1", "hardener", "critical_rules", 1,
                 "Only modify verification_steps",
                 "Do NOT change setup_steps, agentic_steps, or completion_steps",
                 None),
                ("seed-hardener-critical_rules-2", "hardener", "critical_rules", 2,
                 "Preserve step IDs",
                 "Every step must keep its original `id` field (unless splitting a step, in which case keep the original ID on one and generate new UUIDs for additions)",
                 None),
                ("seed-hardener-critical_rules-3", "hardener", "critical_rules", 3,
                 "Preserve step order",
                 "Steps must remain in the same relative position",
                 None),
                ("seed-hardener-critical_rules-4", "hardener", "critical_rules", 4,
                 "Adding steps is allowed",
                 "If a Playwright test step checks multiple things, you MAY replace it with multiple `command` or `ui_bridge` steps. You MAY also add NEW verification steps to cover uncovered agentic goals. Keep original `id`s on existing steps and generate new UUIDs for additions.",
                 None),
                ("seed-hardener-critical_rules-5", "hardener", "critical_rules", 5,
                 "Keep subjective prompts",
                 "If a prompt step is genuinely subjective (e.g., \"Is the UX intuitive?\"), keep it as `prompt`",
                 None),
                ("seed-hardener-critical_rules-6", "hardener", "critical_rules", 6,
                 "Complete required fields",
                 "Every converted step must have all required fields for its new type",
                 None),
                ("seed-hardener-critical_rules-7", "hardener", "critical_rules", 7,
                 "Only 3 step types",
                 "All steps must use `command`, `ui_bridge`, or `prompt`. Do NOT output `api_request`, `check`, `test`, `gate`, or `spec` types.",
                 None),
                ("seed-hardener-critical_rules-8", "hardener", "critical_rules", 8,
                 "Command with check_type fields",
                 "For check conversions, use `command` type with `check_type`, `command`, and `working_directory` fields.",
                 None),
                ("seed-hardener-critical_rules-9", "hardener", "critical_rules", 9,
                 "Do not convert existing command+check_type steps",
                 "Do NOT convert `command` steps that already have `check_type` set (lint, typecheck, etc.) — they are already deterministic.",
                 None),
                ("seed-hardener-critical_rules-10", "hardener", "critical_rules", 10,
                 "SDK verification uses command+curl",
                 "Use `command` steps with curl piped to grep for SDK-based verification, not `api_request`.",
                 None),

                // verification / check_rules
                ("seed-verification-check_rules-1", "verification", "check_rules", 1,
                 "command step validation (plain shell mode)",
                 "`command` is a real, syntactically valid shell command (not a placeholder like \"echo TODO\" or \"/path/to/script\"). `working_directory`, if present, looks like a real path. `timeout_seconds` is reasonable. `fail_on_error` is appropriate. Step type MUST be `command` (not `shell_command`).",
                 None),
                ("seed-verification-check_rules-2", "verification", "check_rules", 2,
                 "command step validation (check mode — check_type set)",
                 "`check_type` and `command` are consistent: \"lint\" → linter, \"typecheck\" → type checker, \"format\" → formatter check, \"analyze\" → static analysis, \"security\" → security scanner, \"custom_command\" → any command. `command` is non-empty and syntactically valid. Step type MUST be `command` (not `check`).",
                 None),
                ("seed-verification-check_rules-3", "verification", "check_rules", 3,
                 "command step validation (test mode — test_type set)",
                 "Has either `command` (for repository/custom_command) or `code` (for playwright/python). `test_type` is one of: playwright, qontinui_vision, python, repository, custom_command. The command/code looks substantive (not a placeholder). Step type MUST be `command` (not `test`).",
                 None),
                ("seed-verification-check_rules-4", "verification", "check_rules", 4,
                 "ui_bridge step validation",
                 "`action` is one of: navigate, execute, assert, snapshot. Required fields vary by action: navigate needs `url`, execute needs `instruction`, assert needs `target` and `assert_type`. `timeout_ms` is reasonable if set.",
                 None),
                ("seed-verification-check_rules-5", "verification", "check_rules", 5,
                 "prompt step quality",
                 "Content is substantive — at least 2 sentences with specific instructions. Agentic prompts reference verification results and describe what to fix. Not a generic placeholder like \"Fix the errors\" or \"Do the task\".",
                 None),
                ("seed-verification-check_rules-6", "verification", "check_rules", 6,
                 "Invalid step type detection",
                 "If any step uses a type other than `command`, `ui_bridge`, or `prompt`, flag it immediately. Common mistakes: using `check` (should be `command` with `check_type`), `test` (should be `command` with `test_type`), `api_request` (should be `command` with curl), `shell_command` (should be `command`), `gate` or `spec` (removed).",
                 None),
                ("seed-verification-check_rules-7", "verification", "check_rules", 7,
                 "Step type consistency",
                 "All step types must be one of: `command`, `ui_bridge`, `prompt`. No other types are valid. Verify that the `type` field of every step matches this constraint.",
                 None),
                ("seed-verification-check_rules-8", "verification", "check_rules", 8,
                 "UI Bridge SDK usage",
                 "If the workflow targets a web app but does NOT include a setup step to connect via UI Bridge SDK (POST to /ui-bridge/sdk/connect), flag it. If the workflow uses Playwright for simple element inspection when SDK endpoints could be used instead, flag it. If agentic prompt steps mention web UI interaction but don't reference SDK tools, flag it.",
                 None),
                ("seed-verification-check_rules-9", "verification", "check_rules", 9,
                 "Agentic-verification correspondence",
                 "For EACH prompt step in agentic_steps, there MUST be at least one corresponding deterministic verification step that can detect whether that agentic step's work succeeded. Tab/section existence checks do NOT count as adequate verification for the tab's CONTENT or FUNCTIONALITY.",
                 None),
                ("seed-verification-check_rules-10", "verification", "check_rules", 10,
                 "Cross-step and structural checks",
                 "If there are verification steps, there should be at least one agentic prompt step. Setup steps should logically prepare for what verification checks. Step names are descriptive (not \"Step 1\", \"Test\", \"Check\"). No duplicate step IDs. Steps match the user's original request.",
                 None),
            ];

            for (id, agent, section, rule_number, title, content, condition) in &seed_rules {
                conn.execute(
                    "INSERT OR IGNORE INTO generation_rules (id, agent, section, rule_number, title, content, condition, status, provenance, created_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'active', 'seed', ?8, ?9)",
                    params![id, agent, section, rule_number, title, content, condition, now, now],
                )
                .map_err(|e| format!("Failed to seed generation rule {}: {}", id, e))?;
            }

            conn.execute_batch(
                "INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (60, datetime('now'));",
            )
            .map_err(|e| format!("Failed to update schema version to 60: {}", e))?;

            info!(
                "Successfully migrated to version 60 (generation_rules table with {} seed rules)",
                seed_rules.len()
            );
        }

        // Migration to version 61: Add sweep fields to unified_workflows
        if current_version < 61 {
            info!("Migrating database to version 61 (adding sweep fields to unified_workflows)");
            conn.execute_batch(
                r#"
                ALTER TABLE unified_workflows ADD COLUMN enable_sweep INTEGER DEFAULT 0;
                ALTER TABLE unified_workflows ADD COLUMN max_sweep_iterations INTEGER DEFAULT 5;

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (61, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 61: {}", e))?;
        }

        // Migration to version 62: Add assertion types generation rule
        if current_version < 62 {
            info!(
                "Migrating database to version 62 (adding valid assertion types generation rule)"
            );
            let now = chrono::Utc::now().to_rfc3339();
            conn.execute(
                "INSERT OR IGNORE INTO generation_rules (id, agent, section, rule_number, title, content, condition, status, provenance, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'active', 'seed', ?8, ?9)",
                params![
                    "seed-schema_context-verification_quality-17",
                    "schema_context",
                    "verification_quality",
                    17,
                    "Valid assertion types only",
                    "API request assertions MUST use one of these 5 types: `status_code`, `body_contains`, `json_path`, `header`, `response_time`. Do NOT use `body_not_contains` or any other unlisted type — they will cause runtime errors. Supported operators (optional `operator` field, default `equals`): `equals`, `contains`, `matches` (regex), `greater_than`, `less_than`.",
                    Option::<&str>::None,
                    now,
                    now,
                ],
            )
            .map_err(|e| format!("Failed to seed assertion types rule: {}", e))?;

            conn.execute_batch(
                "INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (62, datetime('now'));",
            )
            .map_err(|e| format!("Failed to update schema version to 62: {}", e))?;

            info!("Successfully migrated to version 62 (valid assertion types rule)");
        }

        // Migration to version 63: Add step_type_knowledge table with seed data
        if current_version < 63 {
            info!("Migrating database to version 63 (adding step_type_knowledge table)");

            conn.execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS step_type_knowledge (
                    id TEXT PRIMARY KEY,
                    step_type TEXT NOT NULL,
                    layer TEXT NOT NULL DEFAULT 'universal',
                    title TEXT NOT NULL,
                    content TEXT NOT NULL,
                    priority INTEGER NOT NULL DEFAULT 0,
                    status TEXT NOT NULL DEFAULT 'active',
                    provenance TEXT NOT NULL DEFAULT 'seed',
                    source_fix_id TEXT,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL,
                    FOREIGN KEY (source_fix_id) REFERENCES reflection_fixes(id) ON DELETE SET NULL
                );

                CREATE INDEX IF NOT EXISTS idx_stk_step_type ON step_type_knowledge(step_type);
                CREATE INDEX IF NOT EXISTS idx_stk_layer ON step_type_knowledge(layer);
                CREATE INDEX IF NOT EXISTS idx_stk_composite ON step_type_knowledge(step_type, layer, status);
                "#,
            )
            .map_err(|e| format!("Failed to create step_type_knowledge table: {}", e))?;

            // Seed universal knowledge entries
            let now = chrono::Utc::now().to_rfc3339();
            let seed_entries: &[(&str, &str, &str, i32)] = &[
                // command (6 entries - covers shell commands, API requests, checks)
                (
                    "command",
                    "Always set working_directory for shell commands",
                    "Shell commands must specify working_directory to ensure they run in the correct location. Without it, the command runs in whatever directory the runner process happens to be in, which is unpredictable.",
                    10,
                ),
                (
                    "command",
                    "Use fail_on_error appropriately",
                    "Set fail_on_error: true for critical setup steps (installing deps, building). Set fail_on_error: false only for commands where non-zero exit is expected (e.g., grep that may match nothing).",
                    8,
                ),
                (
                    "command",
                    "Keep commands simple and single-purpose",
                    "Each command step should do one thing. Avoid long chained commands with && or ||. Split complex operations into multiple steps so failures are attributable to a specific operation.",
                    6,
                ),
                (
                    "command",
                    "Always include content-specific assertions for API requests",
                    "A status_code: 200 assertion alone is INSUFFICIENT. Always add body_contains or json_path assertions to verify the response has the expected content. Many endpoints return 200 with empty or error bodies.",
                    10,
                ),
                (
                    "command",
                    "Set content_type for request bodies",
                    "When sending a request body, always set content_type (usually 'application/json'). Missing content_type causes the server to reject or misinterpret the body.",
                    8,
                ),
                (
                    "command",
                    "Use json_path for structured response validation",
                    "Prefer json_path assertions over body_contains for JSON APIs. Use json_path with operators like 'greater_than' or 'contains' for flexible yet precise validation. Example: json_path '$.total' greater_than '0'.",
                    6,
                ),
                // prompt (2 entries)
                (
                    "prompt",
                    "Include specific agent instructions",
                    "The prompt field must contain clear, actionable instructions for the AI agent. Vague prompts like 'fix the errors' lead to unfocused actions. Tell the agent exactly what to look for and what corrective actions to take.",
                    10,
                ),
                (
                    "prompt",
                    "Reference verification failures explicitly",
                    "In agentic prompts, tell the agent to check previous verification results and address specific failures. Use phrases like 'Check which verification steps failed and fix the underlying issues'.",
                    8,
                ),
                // check-related knowledge (stored under "command" since check is now command)
                (
                    "command",
                    "Match check_type to the technology",
                    "Use the correct check_type for the project: 'typescript' for TS/JS projects, 'python' for Python, 'rust' for Rust. Using the wrong check_type produces misleading results.",
                    10,
                ),
                (
                    "command",
                    "Set path to the project directory for checks",
                    "Always set the path field to the project root or source directory being checked. Without it, the checker may run in the wrong location or fail to find source files.",
                    8,
                ),
                // test (2 entries)
                (
                    "test",
                    "Use test_type repository for inline commands",
                    "When a test step runs a shell command (e.g., 'npm test', 'pytest'), set test_type: 'repository'. The 'repository' type executes the command in the project directory.",
                    10,
                ),
                (
                    "test",
                    "Target specific test files or patterns",
                    "Avoid running the entire test suite when only specific functionality needs verification. Use the command field to target specific test files or patterns (e.g., 'pytest tests/test_auth.py').",
                    6,
                ),
                // gate (2 entries)
                (
                    "gate",
                    "List ALL non-gate non-prompt verification step IDs",
                    "The required_steps array must contain the IDs of every non-gate, non-prompt step in verification_steps. Missing a step ID means that step's failure won't block the workflow from completing.",
                    10,
                ),
                (
                    "gate",
                    "Use exactly one gate step per workflow",
                    "Each workflow should have exactly one gate step in verification_steps. Multiple gates create confusing pass/fail semantics. The single gate aggregates all deterministic check results.",
                    8,
                ),
                // spec (2 entries)
                (
                    "spec",
                    "Describe behavioral requirements not implementation",
                    "Spec content should describe what the system should DO, not how it's implemented. Focus on observable behavior: 'The login form should display an error message when credentials are invalid'.",
                    10,
                ),
                (
                    "spec",
                    "Pair with deterministic verification",
                    "Spec steps use AI to evaluate behavior, which is non-deterministic. Always pair spec steps with deterministic steps (check, test, api_request) for reliable verification.",
                    8,
                ),
            ];

            for (step_type, title, content, priority) in seed_entries {
                let id = format!(
                    "seed-stk-{}-{}",
                    step_type,
                    title
                        .to_lowercase()
                        .replace(' ', "-")
                        .chars()
                        .take(30)
                        .collect::<String>()
                );
                conn.execute(
                    "INSERT OR IGNORE INTO step_type_knowledge (id, step_type, layer, title, content, priority, status, provenance, created_at, updated_at)
                     VALUES (?1, ?2, 'universal', ?3, ?4, ?5, 'active', 'seed', ?6, ?7)",
                    params![id, step_type, title, content, priority, now, now],
                )
                .map_err(|e| format!("Failed to seed step type knowledge: {}", e))?;
            }

            conn.execute_batch(
                "INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (63, datetime('now'));",
            )
            .map_err(|e| format!("Failed to update schema version to 63: {}", e))?;

            info!("Successfully migrated to version 63 (step_type_knowledge table with {} seed entries)", seed_entries.len());
        }

        // Version 64: Add react-doctor knowledge entries for command step type
        if current_version < 64 {
            info!("Migrating to version 64 (react-doctor step_type_knowledge entries)...");

            let now = chrono::Utc::now().to_rfc3339();

            let react_doctor_entries: Vec<(&str, &str, &str, i32)> = vec![
                (
                    "command",
                    "Use react-doctor for React project health analysis",
                    "For React/Next.js projects, use `npx -y react-doctor@latest <path> --verbose --yes` as a command step to get a health score (0-100) and diagnostics across state/effects, performance, architecture, bundle size, security, correctness, and accessibility. Always include --yes to skip interactive prompts.",
                    6,
                ),
                (
                    "command",
                    "Detect React projects before running react-doctor",
                    "Only run react-doctor on directories that have a package.json containing a 'react' dependency. Non-React TypeScript projects will produce meaningless output.",
                    4,
                ),
            ];

            for (step_type, title, content, priority) in &react_doctor_entries {
                let id = format!(
                    "seed-stk-{}-{}",
                    step_type,
                    title
                        .to_lowercase()
                        .replace(' ', "-")
                        .chars()
                        .take(30)
                        .collect::<String>()
                );
                conn.execute(
                    "INSERT OR IGNORE INTO step_type_knowledge (id, step_type, layer, title, content, priority, status, provenance, created_at, updated_at)
                     VALUES (?1, ?2, 'universal', ?3, ?4, ?5, 'active', 'seed', ?6, ?7)",
                    params![id, step_type, title, content, priority, now, now],
                )
                .map_err(|e| format!("Failed to seed react-doctor knowledge: {}", e))?;
            }

            conn.execute_batch(
                "INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (64, datetime('now'));",
            )
            .map_err(|e| format!("Failed to update schema version to 64: {}", e))?;

            info!("Successfully migrated to version 64 (react-doctor knowledge entries)");
        }

        // Version 65: Update seed generation rules for 3-type step system
        //
        // The original v60 seeds referenced old step types (api_request, check, test,
        // gate, spec). This migration updates existing seed rules to use only the 3
        // valid types: command, ui_bridge, prompt. Only updates provenance='seed' rules
        // to avoid touching reflection-learned rules.
        if current_version < 65 {
            info!("Migrating to version 65 (update seed rules for 3-type system)...");

            let rule_updates: Vec<(&str, &str, &str)> = vec![
                // schema_context / verification_quality
                ("seed-schema_context-verification_quality-6",
                 "Deterministic verification step required",
                 "verification_steps MUST include at least one deterministic, automated step — a `command` step (with check_type, test_type, or a shell command) or a `ui_bridge` step (with assert action). Do NOT use only `prompt` type steps for verification. Prompts provide AI judgment, not deterministic pass/fail results. A verification phase with ONLY prompt steps is INVALID."),
                ("seed-schema_context-verification_quality-7",
                 "Code modification requires typecheck",
                 "When the workflow creates or modifies source code files (TypeScript, Python, Rust, etc.), verification MUST include a `command` step with `check_type` set to the appropriate type checker:\n   - TypeScript/TSX/JSX: `{\"type\": \"command\", \"check_type\": \"typecheck\", \"command\": \"npx tsc --noEmit\", \"working_directory\": \"...\"}`\n   - Python: `{\"type\": \"command\", \"check_type\": \"typecheck\", \"command\": \"mypy .\", \"working_directory\": \"...\"}`\n   - Rust: `{\"type\": \"command\", \"check_type\": \"typecheck\", \"command\": \"cargo check\", \"working_directory\": \"...\"}`"),
                ("seed-schema_context-verification_quality-8",
                 "Web app verification requires SDK or Playwright",
                 "When the workflow targets a web application (localhost:3001, localhost:1420), verification MUST include at least one of:\n   - A `command` step using curl to query UI Bridge SDK endpoints (preferred) to verify UI state\n   - A `command` step with `test_type: \"playwright\"` for browser-based verification\n   - A `ui_bridge` step with an `assert` action for direct element assertions"),
                ("seed-schema_context-verification_quality-9",
                 "Verification must be deterministic",
                 "Every workflow with 2+ verification steps should ensure ALL non-prompt verification steps are meaningful and required. If a step is worth including in verification, its failure should be visible to the verification loop. Do NOT include verification steps whose failures would be silently ignored."),
                ("seed-schema_context-verification_quality-11",
                 "Test steps with inline commands use repository",
                 "When a `command` step with `test_type` runs a shell command (e.g., `npx playwright test ...`, `cargo test ...`), set `test_type: \"repository\"`. The `test_type: \"playwright\"` value is ONLY for steps that provide `code` with Playwright assertions to be executed via CDP. Using `\"playwright\"` for shell commands causes a \"No test_id specified\" error."),
                ("seed-schema_context-verification_quality-12",
                 "Next.js App Router path conventions",
                 "For Next.js projects using the App Router (`src/app/`), components are organized under route groups like `src/app/(app)/`. When creating `command` steps with `check_type`, use the correct working directory paths — e.g., the frontend directory, not `src/components/`. Always verify path patterns match the actual project structure."),
                ("seed-schema_context-verification_quality-15",
                 "SDK verification must verify content",
                 "When a `command` step calls a UI Bridge SDK endpoint via curl (`/ui-bridge/sdk/...`), checking only the exit code is INSUFFICIENT. SDK endpoints return 200 even for empty results (e.g., `ai/search` returns `{\"results\": [], \"total\": 0}`). Every SDK verification command MUST pipe to `grep` with expected text/element content to verify meaningful results."),
                ("seed-schema_context-verification_quality-16",
                 "Agentic-verification correspondence",
                 "Each `prompt` step in `agentic_steps` describes a specific piece of work (e.g., \"implement drag-and-drop\", \"add thumbnails\"). For EACH agentic step, `verification_steps` MUST contain at least one deterministic `command` or `ui_bridge` step that verifies the output of that work."),

                // hardener / conversion_rules
                ("seed-hardener-conversion_rules-1",
                 "Convert prompt steps to deterministic",
                 "Convert `prompt` steps to deterministic equivalents. Only 3 step types are valid: `command`, `ui_bridge`, `prompt`.\n| Prompt check type | Convert to | Method |\n|---|---|---|\n| UI element presence/structure | `command` | curl to UI Bridge SDK endpoint, pipe to grep for content check |\n| Content/text on page | `command` | curl to UI Bridge SDK `/ai/search`, pipe to grep for expected text |\n| File existence | `command` | `check_type: \"custom_command\"` with `test -f <path>` |\n| File content | `command` | `check_type: \"custom_command\"` with `grep -q <pattern> <file>` |\n| Code quality (lint) | `command` | `check_type: \"lint\"` with appropriate command |\n| Code quality (typecheck) | `command` | `check_type: \"typecheck\"` with appropriate command |\n| API health/response | `command` | curl to endpoint, check exit code |\n| UI assertion | `ui_bridge` | Use assert action with target and expected value |\n| Subjective/qualitative | Keep as `prompt` | Cannot be made deterministic |"),
                ("seed-hardener-conversion_rules-2",
                 "Replace Playwright with SDK checks",
                 "When the UI Bridge SDK is connected, Playwright-based UI verification tests should be converted to `command` steps (using curl to SDK endpoints piped to grep) or `ui_bridge` steps. The SDK provides direct programmatic access to registered UI elements without requiring a Playwright browser instance. If a single Playwright test checks multiple things, split it into multiple `command` or `ui_bridge` steps — one per distinct verification concern. Tests that require keyboard shortcuts, file uploads, or screenshot comparisons MUST remain as `command` steps with `test_type: \"playwright\"`."),
                ("seed-hardener-conversion_rules-3",
                 "Strengthen weak SDK verification commands",
                 "If an existing `command` step calls a UI Bridge SDK endpoint via curl but only checks exit code (no grep), add a pipe to `grep` to verify meaningful content. A successful curl to the SDK just means the endpoint is reachable — it doesn't verify the UI state. SDK endpoints return 200 even for EMPTY results."),
                ("seed-hardener-conversion_rules-4",
                 "Inject page navigation before SDK checks",
                 "If the workflow's setup_steps include a page navigation step (curl POST to `/ui-bridge/sdk/page/navigate` or a `ui_bridge` step with `action: \"navigate\"`), the verification phase MUST also navigate to that same URL before any SDK element checks. Use a `command` step with curl or a `ui_bridge` navigate step."),
                ("seed-hardener-conversion_rules-5",
                 "Agentic-verification correspondence",
                 "Examine EACH prompt step in `agentic_steps` and identify the distinct goals/features it describes. Then check whether `verification_steps` has at least one deterministic `command` or `ui_bridge` step that would FAIL if that specific goal was NOT implemented. For each uncovered agentic goal, ADD a new `command` verification step (e.g., curl to SDK endpoint piped to grep for expected content)."),

                // hardener / critical_rules
                ("seed-hardener-critical_rules-4",
                 "Adding steps is allowed",
                 "If a Playwright test step checks multiple things, you MAY replace it with multiple `command` or `ui_bridge` steps. You MAY also add NEW verification steps to cover uncovered agentic goals. Keep original `id`s on existing steps and generate new UUIDs for additions."),
                ("seed-hardener-critical_rules-7",
                 "Only 3 step types",
                 "All steps must use `command`, `ui_bridge`, or `prompt`. Do NOT output `api_request`, `check`, `test`, `gate`, or `spec` types."),
                ("seed-hardener-critical_rules-8",
                 "Command with check_type fields",
                 "For check conversions, use `command` type with `check_type`, `command`, and `working_directory` fields."),
                ("seed-hardener-critical_rules-9",
                 "Do not convert existing command+check_type steps",
                 "Do NOT convert `command` steps that already have `check_type` set (lint, typecheck, etc.) — they are already deterministic."),
                ("seed-hardener-critical_rules-10",
                 "SDK verification uses command+curl",
                 "Use `command` steps with curl piped to grep for SDK-based verification, not `api_request`."),

                // verification / check_rules
                ("seed-verification-check_rules-1",
                 "command step validation (plain shell mode)",
                 "`command` is a real, syntactically valid shell command (not a placeholder like \"echo TODO\" or \"/path/to/script\"). `working_directory`, if present, looks like a real path. `timeout_seconds` is reasonable. `fail_on_error` is appropriate. Step type MUST be `command` (not `shell_command`)."),
                ("seed-verification-check_rules-2",
                 "command step validation (check mode — check_type set)",
                 "`check_type` and `command` are consistent: \"lint\" → linter, \"typecheck\" → type checker, \"format\" → formatter check, \"analyze\" → static analysis, \"security\" → security scanner, \"custom_command\" → any command. `command` is non-empty and syntactically valid. Step type MUST be `command` (not `check`)."),
                ("seed-verification-check_rules-3",
                 "command step validation (test mode — test_type set)",
                 "Has either `command` (for repository/custom_command) or `code` (for playwright/python). `test_type` is one of: playwright, qontinui_vision, python, repository, custom_command. The command/code looks substantive (not a placeholder). Step type MUST be `command` (not `test`)."),
                ("seed-verification-check_rules-4",
                 "ui_bridge step validation",
                 "`action` is one of: navigate, execute, assert, snapshot. Required fields vary by action: navigate needs `url`, execute needs `instruction`, assert needs `target` and `assert_type`. `timeout_ms` is reasonable if set."),
                ("seed-verification-check_rules-6",
                 "Invalid step type detection",
                 "If any step uses a type other than `command`, `ui_bridge`, or `prompt`, flag it immediately. Common mistakes: using `check` (should be `command` with `check_type`), `test` (should be `command` with `test_type`), `api_request` (should be `command` with curl), `shell_command` (should be `command`), `gate` or `spec` (removed)."),
                ("seed-verification-check_rules-7",
                 "Step type consistency",
                 "All step types must be one of: `command`, `ui_bridge`, `prompt`. No other types are valid. Verify that the `type` field of every step matches this constraint."),
            ];

            let mut updated = 0;
            for (rule_id, new_title, new_content) in &rule_updates {
                let rows = conn.execute(
                    "UPDATE generation_rules SET title = ?1, content = ?2, updated_at = datetime('now') WHERE id = ?3 AND provenance = 'seed'",
                    params![new_title, new_content, rule_id],
                )
                .map_err(|e| format!("Failed to update seed rule {}: {}", rule_id, e))?;
                if rows > 0 {
                    updated += 1;
                }
            }

            // Insert new rules that didn't exist in the original v60 seeds
            let now = chrono::Utc::now().to_rfc3339();

            // Rule 17: Only 3 step types
            conn.execute(
                "INSERT OR IGNORE INTO generation_rules (id, agent, section, rule_number, title, content, condition, status, provenance, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'active', 'seed', ?8, ?9)",
                params![
                    "seed-schema_context-verification_quality-17",
                    "schema_context", "verification_quality", 17,
                    "Only 3 step types exist",
                    "The only valid step types are `command`, `ui_bridge`, and `prompt`. Do NOT use `shell_command`, `api_request`, `mcp_call`, `check`, `check_group`, `test`, `gate`, or `spec` — these are not valid types. Tests are run via `command` with `test_type` set. Checks are run via `command` with `check_type` set.",
                    Option::<&str>::None, now, now
                ],
            )
            .map_err(|e| format!("Failed to insert rule 17: {}", e))?;

            // Rule for explicit mode field on command steps
            conn.execute(
                "INSERT OR IGNORE INTO generation_rules (id, agent, section, rule_number, title, content, condition, status, provenance, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'active', 'seed', ?8, ?9)",
                params![
                    "seed-schema_context-important_rules-6",
                    "schema_context", "important_rules", 6,
                    "Every command step MUST include a mode field",
                    "Every `command` step MUST include a `mode` field set to one of: `shell`, `check`, `check_group`, `test`. The mode must match the fields present: `check` requires `check_type`, `check_group` requires `check_group_id`, `test` requires `test_type` or `test_id`, `shell` is the default for plain commands.",
                    Option::<&str>::None, now, now
                ],
            )
            .map_err(|e| format!("Failed to insert mode rule: {}", e))?;

            conn.execute_batch(
                "INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (65, datetime('now'));",
            )
            .map_err(|e| format!("Failed to update schema version to 65: {}", e))?;

            info!(
                "Successfully migrated to version 65 ({} seed rules updated for 3-type system)",
                updated
            );
        }

        // Version 66: Add stage_index to workflow_step_checkpoints and stages/stop_on_failure to unified_workflows
        if current_version < 66 {
            info!(
                "Migrating to version 66 (add stage_index to checkpoints, stages to workflows)..."
            );

            // Add stage_index column to workflow_step_checkpoints
            // For existing rows, default to 0 (single-stage backward compat)
            conn.execute_batch(
                r#"
                ALTER TABLE workflow_step_checkpoints ADD COLUMN stage_index INTEGER DEFAULT 0;

                -- Add stages and stop_on_failure to unified_workflows
                ALTER TABLE unified_workflows ADD COLUMN stages TEXT DEFAULT '[]';
                ALTER TABLE unified_workflows ADD COLUMN stop_on_failure INTEGER DEFAULT 0;

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (66, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 66: {}", e))?;

            info!("Successfully migrated to version 66 (stage_index, stages, stop_on_failure)");
        }

        // Migration to version 67: Fix UNIQUE constraint to include stage_index
        // The original UNIQUE(execution_id, phase, iteration, step_index) from v48 causes
        // multi-stage workflows to silently overwrite checkpoints when two stages have
        // steps with the same (phase, iteration, step_index). Recreate table with
        // UNIQUE(execution_id, phase, iteration, step_index, stage_index).
        if current_version < 67 {
            info!("Migrating to version 67 (fix UNIQUE constraint to include stage_index)...");

            conn.execute_batch(
                r#"
                -- Recreate workflow_step_checkpoints with corrected UNIQUE constraint
                CREATE TABLE IF NOT EXISTS workflow_step_checkpoints_new (
                    id TEXT PRIMARY KEY,
                    execution_id TEXT NOT NULL,
                    workflow_type TEXT NOT NULL,
                    phase TEXT NOT NULL,
                    iteration INTEGER,
                    step_index INTEGER NOT NULL,
                    step_type TEXT NOT NULL,
                    step_name TEXT,
                    status TEXT NOT NULL,
                    result_json TEXT,
                    step_config_json TEXT,
                    started_at TEXT,
                    completed_at TEXT,
                    duration_ms INTEGER,
                    error TEXT,
                    stage_index INTEGER DEFAULT 0,
                    FOREIGN KEY (execution_id) REFERENCES task_runs(id) ON DELETE CASCADE,
                    UNIQUE(execution_id, phase, iteration, step_index, stage_index)
                );

                -- Copy all existing data
                INSERT OR IGNORE INTO workflow_step_checkpoints_new
                    (id, execution_id, workflow_type, phase, iteration, step_index,
                     step_type, step_name, status, result_json, step_config_json,
                     started_at, completed_at, duration_ms, error, stage_index)
                SELECT
                    id, execution_id, workflow_type, phase, iteration, step_index,
                    step_type, step_name, status, result_json, step_config_json,
                    started_at, completed_at, duration_ms, error,
                    COALESCE(stage_index, 0)
                FROM workflow_step_checkpoints;

                -- Drop old table and rename new one
                DROP TABLE workflow_step_checkpoints;
                ALTER TABLE workflow_step_checkpoints_new RENAME TO workflow_step_checkpoints;

                -- Recreate indexes
                CREATE INDEX IF NOT EXISTS idx_step_checkpoints_execution ON workflow_step_checkpoints(execution_id);
                CREATE INDEX IF NOT EXISTS idx_step_checkpoints_lookup ON workflow_step_checkpoints(execution_id, phase, iteration);
                CREATE INDEX IF NOT EXISTS idx_step_checkpoints_status ON workflow_step_checkpoints(status);
                CREATE INDEX IF NOT EXISTS idx_step_checkpoints_cursor ON workflow_step_checkpoints(execution_id, step_index);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (67, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 67: {}", e))?;

            info!(
                "Successfully migrated to version 67 (UNIQUE constraint now includes stage_index)"
            );
        }

        // Migration to version 68: Add process_sessions and process_session_output tables
        if current_version < 68 {
            info!("Migrating to version 68 (process session persistence)...");

            conn.execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS process_sessions (
                    id TEXT PRIMARY KEY,
                    process_config_id TEXT NOT NULL,
                    process_name TEXT NOT NULL,
                    started_at TEXT NOT NULL,
                    stopped_at TEXT,
                    exit_code INTEGER,
                    state TEXT NOT NULL DEFAULT 'running',
                    error_count INTEGER NOT NULL DEFAULT 0
                );

                CREATE INDEX IF NOT EXISTS idx_process_sessions_config_id ON process_sessions(process_config_id);
                CREATE INDEX IF NOT EXISTS idx_process_sessions_started_at ON process_sessions(started_at);

                CREATE TABLE IF NOT EXISTS process_session_output (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    session_id TEXT NOT NULL,
                    timestamp TEXT NOT NULL,
                    stream TEXT NOT NULL,
                    line TEXT NOT NULL,
                    FOREIGN KEY (session_id) REFERENCES process_sessions(id) ON DELETE CASCADE
                );

                CREATE INDEX IF NOT EXISTS idx_process_session_output_session ON process_session_output(session_id);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (68, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 68: {}", e))?;

            info!("Successfully migrated to version 68 (process session persistence)");
        }

        if current_version < 69 {
            info!("Migrating to version 69 (cached app specs)...");

            conn.execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS cached_app_specs (
                    id TEXT PRIMARY KEY,
                    app_url TEXT NOT NULL,
                    app_name TEXT NOT NULL,
                    spec_id TEXT NOT NULL,
                    spec_json TEXT NOT NULL,
                    discovered_at TEXT NOT NULL,
                    page_url TEXT
                );

                CREATE INDEX IF NOT EXISTS idx_cached_specs_app ON cached_app_specs(app_url);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (69, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 69: {}", e))?;

            info!("Successfully migrated to version 69 (cached app specs)");
        }

        // Version 70: Add reflection_mode to unified_workflows
        if current_version < 70 {
            info!("Migrating to version 70 (add reflection_mode to unified_workflows)...");

            conn.execute_batch(
                r#"
                ALTER TABLE unified_workflows ADD COLUMN reflection_mode INTEGER DEFAULT 1;

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (70, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 70: {}", e))?;

            info!("Successfully migrated to version 70 (reflection_mode)");
        }

        // Version 71: Add follow-up columns to task_runs
        if current_version < 71 {
            info!("Migrating to version 71 (add follow-up columns to task_runs)...");

            conn.execute_batch(
                r#"
                ALTER TABLE task_runs ADD COLUMN is_follow_up INTEGER DEFAULT 0;
                ALTER TABLE task_runs ADD COLUMN follow_up_source_task_run_id TEXT;

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (71, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 71: {}", e))?;

            info!("Successfully migrated to version 71 (follow-up columns)");
        }

        // Version 72: Generator evaluation - pipeline artifacts
        if current_version < 72 {
            info!("Migrating to version 72 (generation_pipeline_artifacts)...");

            conn.execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS generation_pipeline_artifacts (
                    id TEXT PRIMARY KEY,
                    workflow_id TEXT,
                    task_run_id TEXT,
                    description TEXT NOT NULL,
                    category TEXT,
                    created_at TEXT NOT NULL,
                    discovery_duration_ms INTEGER,
                    builder_duration_ms INTEGER,
                    autofix_duration_ms INTEGER,
                    verification_duration_ms INTEGER,
                    hardener_duration_ms INTEGER,
                    total_duration_ms INTEGER,
                    discovery_calls TEXT,
                    builder_raw_output TEXT,
                    builder_parsed_json TEXT,
                    autofix_diff TEXT,
                    verification_iterations TEXT,
                    fixer_snapshots TEXT,
                    hardening_summary TEXT,
                    hardened_json TEXT,
                    final_json TEXT,
                    validation_errors TEXT,
                    success INTEGER NOT NULL DEFAULT 1,
                    error_message TEXT,
                    model_used TEXT
                );

                CREATE INDEX IF NOT EXISTS idx_pipeline_artifacts_workflow ON generation_pipeline_artifacts(workflow_id);
                CREATE INDEX IF NOT EXISTS idx_pipeline_artifacts_created ON generation_pipeline_artifacts(created_at);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (72, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 72: {}", e))?;

            info!("Successfully migrated to version 72 (generation_pipeline_artifacts)");
        }

        // Version 73: Generator evaluation - benchmarks
        if current_version < 73 {
            info!("Migrating to version 73 (generator_benchmarks)...");

            conn.execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS generator_benchmarks (
                    id TEXT PRIMARY KEY,
                    name TEXT NOT NULL,
                    description TEXT NOT NULL,
                    category TEXT,
                    tags TEXT,
                    expected_structure TEXT NOT NULL,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL,
                    enabled INTEGER NOT NULL DEFAULT 1
                );

                CREATE TABLE IF NOT EXISTS generator_benchmark_results (
                    id TEXT PRIMARY KEY,
                    benchmark_id TEXT NOT NULL REFERENCES generator_benchmarks(id),
                    artifact_id TEXT REFERENCES generation_pipeline_artifacts(id),
                    run_at TEXT NOT NULL,
                    model_used TEXT,
                    structure_score REAL,
                    content_score REAL,
                    step_type_score REAL,
                    overall_score REAL,
                    score_breakdown TEXT,
                    generated_json TEXT,
                    duration_ms INTEGER,
                    passed INTEGER NOT NULL DEFAULT 0,
                    notes TEXT
                );

                CREATE INDEX IF NOT EXISTS idx_benchmark_results_benchmark ON generator_benchmark_results(benchmark_id);
                CREATE INDEX IF NOT EXISTS idx_benchmark_results_run_at ON generator_benchmark_results(run_at);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (73, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 73: {}", e))?;

            info!("Successfully migrated to version 73 (generator_benchmarks)");
        }

        // Version 74: Add investigation columns to generation_pipeline_artifacts
        if current_version < 74 {
            info!("Migrating to version 74 (add investigation columns to pipeline_artifacts)...");

            conn.execute_batch(
                r#"
                ALTER TABLE generation_pipeline_artifacts ADD COLUMN investigation_duration_ms INTEGER;
                ALTER TABLE generation_pipeline_artifacts ADD COLUMN investigation_enriched_description TEXT;

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (74, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 74: {}", e))?;

            info!("Successfully migrated to version 74 (investigation columns)");
        }

        if current_version < 75 {
            info!("Migrating to version 75 (workflow triggers system)...");

            conn.execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS workflow_triggers (
                    id TEXT PRIMARY KEY,
                    name TEXT NOT NULL,
                    description TEXT,
                    trigger_type TEXT NOT NULL,
                    trigger_config TEXT NOT NULL,
                    workflow_id TEXT NOT NULL,
                    workflow_overrides TEXT,
                    conditions TEXT DEFAULT '[]',
                    debounce_ms INTEGER DEFAULT 1000,
                    cooldown_seconds INTEGER DEFAULT 60,
                    max_concurrent INTEGER DEFAULT 1,
                    enabled BOOLEAN DEFAULT 1,
                    last_triggered_at TEXT,
                    last_execution_id TEXT,
                    trigger_count INTEGER DEFAULT 0,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL,
                    FOREIGN KEY (workflow_id) REFERENCES unified_workflows(id) ON DELETE CASCADE
                );

                CREATE INDEX IF NOT EXISTS idx_workflow_triggers_type ON workflow_triggers(trigger_type);
                CREATE INDEX IF NOT EXISTS idx_workflow_triggers_enabled ON workflow_triggers(enabled);

                CREATE TABLE IF NOT EXISTS trigger_history (
                    id TEXT PRIMARY KEY,
                    trigger_id TEXT NOT NULL,
                    event_type TEXT NOT NULL,
                    event_data TEXT DEFAULT '{}',
                    action TEXT NOT NULL,
                    task_run_id TEXT,
                    error_message TEXT,
                    triggered_at TEXT NOT NULL,
                    FOREIGN KEY (trigger_id) REFERENCES workflow_triggers(id) ON DELETE CASCADE
                );

                CREATE INDEX IF NOT EXISTS idx_trigger_history_trigger_id ON trigger_history(trigger_id);
                CREATE INDEX IF NOT EXISTS idx_trigger_history_triggered_at ON trigger_history(triggered_at);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (75, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 75: {}", e))?;

            info!("Successfully migrated to version 75 (workflow triggers system)");
        }

        if current_version < 76 {
            info!("Migrating to version 76 (canvas panels)...");

            conn.execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS canvas_panels (
                    id TEXT PRIMARY KEY,
                    task_run_id TEXT NOT NULL,
                    component TEXT NOT NULL,
                    title TEXT NOT NULL,
                    data_json TEXT NOT NULL,
                    priority INTEGER DEFAULT 50,
                    size TEXT DEFAULT 'normal',
                    created_at TEXT NOT NULL DEFAULT (datetime('now')),
                    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
                    FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE
                );

                CREATE INDEX IF NOT EXISTS idx_canvas_panels_task_run_id ON canvas_panels(task_run_id);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (76, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 76: {}", e))?;

            info!("Successfully migrated to version 76 (canvas panels)");
        }

        // Version 77: Add model_overrides to unified_workflows
        if current_version < 77 {
            info!("Migrating to version 77 (add model_overrides to unified_workflows)...");

            conn.execute_batch(
                r#"
                ALTER TABLE unified_workflows ADD COLUMN model_overrides TEXT DEFAULT '{}';

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (77, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 77: {}", e))?;

            info!("Successfully migrated to version 77 (model_overrides)");
        }

        // Version 78: Add approval_gates table for human-in-the-loop audit trail
        if current_version < 78 {
            info!("Migrating to version 78 (approval_gates table)...");

            conn.execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS approval_gates (
                    id TEXT PRIMARY KEY,
                    task_run_id TEXT NOT NULL,
                    iteration INTEGER NOT NULL,
                    prompt TEXT NOT NULL,
                    context_json TEXT DEFAULT '{}',
                    action TEXT,
                    comment TEXT,
                    status TEXT NOT NULL DEFAULT 'pending',
                    created_at TEXT NOT NULL DEFAULT (datetime('now')),
                    resolved_at TEXT,
                    FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE
                );

                CREATE INDEX IF NOT EXISTS idx_approval_gates_task_run_id ON approval_gates(task_run_id);
                CREATE INDEX IF NOT EXISTS idx_approval_gates_status ON approval_gates(status);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (78, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 78: {}", e))?;

            info!("Successfully migrated to version 78 (approval_gates)");
        }

        // Version 79: Add user_skills table for user-created skill definitions
        if current_version < 79 {
            info!("Migrating to version 79 (user_skills table)...");

            conn.execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS user_skills (
                    id TEXT PRIMARY KEY,
                    name TEXT NOT NULL,
                    slug TEXT NOT NULL UNIQUE,
                    description TEXT DEFAULT '',
                    category TEXT DEFAULT 'custom',
                    tags TEXT DEFAULT '[]',
                    icon TEXT DEFAULT 'puzzle',
                    color TEXT DEFAULT 'gray',
                    allowed_phases TEXT NOT NULL DEFAULT '["setup"]',
                    parameters TEXT DEFAULT '[]',
                    template TEXT NOT NULL,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL
                );

                CREATE INDEX IF NOT EXISTS idx_user_skills_slug ON user_skills(slug);
                CREATE INDEX IF NOT EXISTS idx_user_skills_category ON user_skills(category);
                CREATE INDEX IF NOT EXISTS idx_user_skills_updated_at ON user_skills(updated_at);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (79, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 79: {}", e))?;

            info!("Successfully migrated to version 79 (user_skills)");
        }

        // Version 80: Add approval_gate column to unified_workflows
        if current_version < 80 {
            info!("Migrating to version 80 (add approval_gate to unified_workflows)...");

            conn.execute_batch(
                r#"
                ALTER TABLE unified_workflows ADD COLUMN approval_gate BOOLEAN DEFAULT 0;

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (80, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 80: {}", e))?;

            info!("Successfully migrated to version 80 (approval_gate)");
        }

        // Version 81: Add source column to user_skills for community skills
        if current_version < 81 {
            info!("Migrating to version 81 (add source column to user_skills)...");

            conn.execute_batch(
                r#"
                ALTER TABLE user_skills ADD COLUMN source TEXT NOT NULL DEFAULT 'user';

                CREATE INDEX IF NOT EXISTS idx_user_skills_source ON user_skills(source);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (81, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 81: {}", e))?;

            info!("Successfully migrated to version 81 (user_skills source column)");
        }

        if current_version < 82 {
            info!("Migrating to version 82 (canvas panels group_name column)...");

            conn.execute_batch(
                r#"
                ALTER TABLE canvas_panels ADD COLUMN group_name TEXT;

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (82, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 82: {}", e))?;

            info!("Successfully migrated to version 82 (canvas panels group_name)");
        }

        if current_version < 83 {
            info!("Migrating to version 83 (trigger retry columns)...");

            conn.execute_batch(
                r#"
                ALTER TABLE workflow_triggers ADD COLUMN retry_count INTEGER DEFAULT 0;
                ALTER TABLE workflow_triggers ADD COLUMN retry_delay_seconds INTEGER DEFAULT 30;

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (83, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 83: {}", e))?;

            info!("Successfully migrated to version 83 (trigger retry columns)");
        }

        // Version 84: Extend user_skills with versioning, author, checksums, dependencies, approval
        if current_version < 84 {
            info!(
                "Migrating to version 84 (extend user_skills for skill registry improvements)..."
            );

            conn.execute_batch(
                r#"
                ALTER TABLE user_skills ADD COLUMN version TEXT DEFAULT '1.0.0';
                ALTER TABLE user_skills ADD COLUMN author TEXT DEFAULT NULL;
                ALTER TABLE user_skills ADD COLUMN checksum TEXT DEFAULT NULL;
                ALTER TABLE user_skills ADD COLUMN depends_on TEXT DEFAULT '[]';
                ALTER TABLE user_skills ADD COLUMN usage_count INTEGER DEFAULT 0;
                ALTER TABLE user_skills ADD COLUMN approval_status TEXT DEFAULT NULL;
                ALTER TABLE user_skills ADD COLUMN forked_from TEXT DEFAULT NULL;

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (84, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 84: {}", e))?;

            info!("Successfully migrated to version 84 (skill registry improvements)");
        }

        // Version 85: Phase token usage tracking for cost analysis
        if current_version < 85 {
            info!("Migrating to version 85 (phase token usage tracking)...");

            conn.execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS phase_token_usage (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    task_run_id TEXT NOT NULL,
                    phase TEXT NOT NULL,
                    stage_index INTEGER,
                    iteration INTEGER,
                    model_used TEXT,
                    provider_used TEXT,
                    input_tokens INTEGER NOT NULL DEFAULT 0,
                    output_tokens INTEGER NOT NULL DEFAULT 0,
                    cost_cents INTEGER NOT NULL DEFAULT 0,
                    duration_ms INTEGER,
                    created_at TEXT NOT NULL DEFAULT (datetime('now')),
                    FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE
                );

                CREATE INDEX IF NOT EXISTS idx_phase_token_usage_task_run ON phase_token_usage(task_run_id);

                ALTER TABLE task_runs ADD COLUMN total_input_tokens INTEGER DEFAULT 0;
                ALTER TABLE task_runs ADD COLUMN total_output_tokens INTEGER DEFAULT 0;
                ALTER TABLE task_runs ADD COLUMN total_cost_cents INTEGER DEFAULT 0;

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (85, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 85: {}", e))?;

            info!("Successfully migrated to version 85 (phase token usage tracking)");
        }

        // Version 86: Add trace_id to error_events for cross-service trace correlation
        if current_version < 86 {
            info!("Migrating to version 86 (cross-service trace propagation)...");

            conn.execute_batch(
                r#"
                ALTER TABLE error_events ADD COLUMN trace_id TEXT;
                CREATE INDEX IF NOT EXISTS idx_error_events_trace_id ON error_events(trace_id);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (86, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 86: {}", e))?;

            info!("Successfully migrated to version 86 (cross-service trace propagation)");
        }

        // Version 87: Add UI Bridge integrations tracking table
        if current_version < 87 {
            info!("Migrating to version 87 (UI Bridge integrations tracking)...");

            conn.execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS ui_bridge_integrations (
                    id TEXT PRIMARY KEY,
                    project_path TEXT NOT NULL,
                    label TEXT,
                    framework TEXT,
                    integration_type TEXT NOT NULL,
                    sdk_version TEXT,
                    status TEXT NOT NULL DEFAULT 'active',
                    proxy_port INTEGER,
                    target_url TEXT,
                    last_health_check INTEGER,
                    element_count INTEGER,
                    created_at INTEGER NOT NULL,
                    updated_at INTEGER NOT NULL
                );

                CREATE INDEX IF NOT EXISTS idx_ui_bridge_integrations_status ON ui_bridge_integrations(status);
                CREATE INDEX IF NOT EXISTS idx_ui_bridge_integrations_type ON ui_bridge_integrations(integration_type);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (87, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 87: {}", e))?;

            info!("Successfully migrated to version 87 (UI Bridge integrations tracking)");
        }

        if current_version < 88 {
            info!(
                "Migrating to version 88 (specification + prompt columns on pipeline_artifacts)..."
            );

            conn.execute_batch(
                r#"
                ALTER TABLE generation_pipeline_artifacts ADD COLUMN specification_duration_ms INTEGER;
                ALTER TABLE generation_pipeline_artifacts ADD COLUMN specification_criteria TEXT;
                ALTER TABLE generation_pipeline_artifacts ADD COLUMN specification_prompt TEXT;
                ALTER TABLE generation_pipeline_artifacts ADD COLUMN builder_prompt TEXT;
                ALTER TABLE generation_pipeline_artifacts ADD COLUMN verification_prompts TEXT;
                ALTER TABLE generation_pipeline_artifacts ADD COLUMN hardener_prompt TEXT;

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (88, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 88: {}", e))?;

            info!("Successfully migrated to version 88 (specification + prompt columns on pipeline_artifacts)");
        }

        if current_version < 89 {
            info!("Migrating to version 89 (source_agent on reflection_fixes, auto-rule fields on generation_rules)...");

            conn.execute_batch(
                r#"
                ALTER TABLE reflection_fixes ADD COLUMN source_agent TEXT;
                CREATE INDEX IF NOT EXISTS idx_reflection_fixes_source_agent ON reflection_fixes(source_agent);

                ALTER TABLE generation_rules ADD COLUMN confidence REAL DEFAULT 1.0;
                ALTER TABLE generation_rules ADD COLUMN auto_generated_at TEXT;
                ALTER TABLE generation_rules ADD COLUMN evidence_count INTEGER DEFAULT 0;

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (89, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 89: {}", e))?;

            info!("Successfully migrated to version 89 (source_agent + auto-rule fields)");
        }

        // Version 90: Add completion_prompts_first to unified_workflows
        if current_version < 90 {
            info!("Migrating to version 90 (add completion_prompts_first to unified_workflows)...");

            conn.execute_batch(
                r#"
                ALTER TABLE unified_workflows ADD COLUMN completion_prompts_first INTEGER NOT NULL DEFAULT 0;

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (90, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 90: {}", e))?;

            info!("Successfully migrated to version 90 (completion_prompts_first)");
        }

        // Version 91: Known Issues Registry + Issue Pattern Templates
        if current_version < 91 {
            info!("Migrating to version 91 (known issues registry + pattern templates)...");

            conn.execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS issue_pattern_templates (
                    id TEXT PRIMARY KEY,
                    name TEXT NOT NULL,
                    description TEXT NOT NULL,
                    category TEXT NOT NULL,
                    detection_type TEXT NOT NULL,
                    step_template TEXT,
                    ai_prompt_template TEXT,
                    parameters TEXT NOT NULL DEFAULT '[]',
                    built_in BOOLEAN NOT NULL DEFAULT 0,
                    status TEXT NOT NULL DEFAULT 'active',
                    created_at TEXT NOT NULL DEFAULT (datetime('now')),
                    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
                );

                CREATE INDEX IF NOT EXISTS idx_ipt_category ON issue_pattern_templates(category);
                CREATE INDEX IF NOT EXISTS idx_ipt_status ON issue_pattern_templates(status);

                CREATE TABLE IF NOT EXISTS known_issues (
                    id TEXT PRIMARY KEY,
                    title TEXT NOT NULL,
                    description TEXT NOT NULL,
                    category TEXT NOT NULL DEFAULT 'other',
                    scope_type TEXT NOT NULL DEFAULT 'global',
                    scope_value TEXT,
                    scope_tags TEXT DEFAULT '[]',
                    detection_method TEXT NOT NULL DEFAULT 'ai_judgment',
                    detection_config TEXT DEFAULT '{}',
                    pattern_template_id TEXT,
                    reproduction_context TEXT,
                    trigger_conditions TEXT DEFAULT '[]',
                    severity TEXT NOT NULL DEFAULT 'medium',
                    status TEXT NOT NULL DEFAULT 'active',
                    confidence REAL NOT NULL DEFAULT 1.0,
                    provenance TEXT NOT NULL DEFAULT 'manual',
                    source_finding_ids TEXT DEFAULT '[]',
                    source_task_run_id TEXT,
                    verification_hint TEXT,
                    verification_step_template TEXT,
                    times_detected INTEGER DEFAULT 1,
                    times_checked INTEGER DEFAULT 0,
                    last_detected_at TEXT,
                    last_checked_at TEXT,
                    resolved_at TEXT,
                    description_embedding BLOB,
                    created_at TEXT NOT NULL DEFAULT (datetime('now')),
                    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
                    FOREIGN KEY (pattern_template_id) REFERENCES issue_pattern_templates(id) ON DELETE SET NULL,
                    FOREIGN KEY (source_task_run_id) REFERENCES task_runs(id) ON DELETE SET NULL
                );

                CREATE INDEX IF NOT EXISTS idx_known_issues_category ON known_issues(category);
                CREATE INDEX IF NOT EXISTS idx_known_issues_scope_type ON known_issues(scope_type);
                CREATE INDEX IF NOT EXISTS idx_known_issues_status ON known_issues(status);
                CREATE INDEX IF NOT EXISTS idx_known_issues_severity ON known_issues(severity);
                CREATE INDEX IF NOT EXISTS idx_known_issues_scope_value ON known_issues(scope_value);
                CREATE INDEX IF NOT EXISTS idx_known_issues_scope_compound ON known_issues(scope_type, scope_value, status);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (91, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 91: {}", e))?;

            info!(
                "Successfully migrated to version 91 (known issues registry + pattern templates)"
            );
        }

        if current_version < 92 {
            info!("Migrating database to version 92 (workflow favorites)");

            conn.execute_batch(
                r#"
                ALTER TABLE unified_workflows ADD COLUMN is_favorite INTEGER DEFAULT 0;

                CREATE INDEX IF NOT EXISTS idx_unified_workflows_is_favorite ON unified_workflows(is_favorite);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (92, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 92: {}", e))?;

            info!("Successfully migrated to version 92 (workflow favorites)");
        }

        // Migration to version 93: State Machine Config Builder tables
        if current_version < 93 {
            info!("Migrating database to version 93 (state machine config builder tables)");
            conn.execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS state_machine_configs (
                    id TEXT PRIMARY KEY,
                    name TEXT NOT NULL DEFAULT 'default',
                    description TEXT,
                    render_count INTEGER NOT NULL DEFAULT 0,
                    element_count INTEGER NOT NULL DEFAULT 0,
                    include_html_ids BOOLEAN NOT NULL DEFAULT FALSE,
                    created_at TEXT NOT NULL DEFAULT (datetime('now')),
                    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
                );

                CREATE TABLE IF NOT EXISTS state_machine_states (
                    id TEXT PRIMARY KEY,
                    config_id TEXT NOT NULL REFERENCES state_machine_configs(id) ON DELETE CASCADE,
                    state_id TEXT NOT NULL,
                    name TEXT NOT NULL,
                    description TEXT,
                    element_ids TEXT NOT NULL DEFAULT '[]',
                    render_ids TEXT NOT NULL DEFAULT '[]',
                    confidence REAL NOT NULL DEFAULT 0.9,
                    acceptance_criteria TEXT NOT NULL DEFAULT '[]',
                    extra_metadata TEXT NOT NULL DEFAULT '{}',
                    domain_knowledge TEXT NOT NULL DEFAULT '[]',
                    created_at TEXT NOT NULL DEFAULT (datetime('now')),
                    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
                );

                CREATE INDEX IF NOT EXISTS idx_sm_states_config_id ON state_machine_states(config_id);

                CREATE TABLE IF NOT EXISTS state_machine_transitions (
                    id TEXT PRIMARY KEY,
                    config_id TEXT NOT NULL REFERENCES state_machine_configs(id) ON DELETE CASCADE,
                    transition_id TEXT NOT NULL,
                    name TEXT NOT NULL,
                    from_states TEXT NOT NULL DEFAULT '[]',
                    activate_states TEXT NOT NULL DEFAULT '[]',
                    exit_states TEXT NOT NULL DEFAULT '[]',
                    actions TEXT NOT NULL DEFAULT '[]',
                    path_cost REAL NOT NULL DEFAULT 1.0,
                    stays_visible BOOLEAN NOT NULL DEFAULT FALSE,
                    extra_metadata TEXT NOT NULL DEFAULT '{}',
                    created_at TEXT NOT NULL DEFAULT (datetime('now')),
                    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
                );

                CREATE INDEX IF NOT EXISTS idx_sm_transitions_config_id ON state_machine_transitions(config_id);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (93, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 93: {}", e))?;

            info!("Successfully migrated to version 93 (state machine config builder tables)");
        }

        // Migration 94: Workflow AI Sessions table for restart survival
        if current_version < 94 {
            conn.execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS workflow_ai_sessions (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    task_run_id TEXT NOT NULL,
                    iteration INTEGER NOT NULL,
                    phase TEXT NOT NULL,
                    stage_index INTEGER,
                    claude_cli_session_id TEXT,
                    session_started_at TEXT NOT NULL,
                    session_completed_at TEXT,
                    output_length INTEGER NOT NULL DEFAULT 0,
                    status TEXT NOT NULL DEFAULT 'running',
                    FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE
                );

                CREATE INDEX IF NOT EXISTS idx_wf_ai_sessions_task_run ON workflow_ai_sessions(task_run_id);
                CREATE UNIQUE INDEX IF NOT EXISTS idx_wf_ai_sessions_unique
                    ON workflow_ai_sessions(task_run_id, iteration, phase, COALESCE(stage_index, -1));

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (94, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 94: {}", e))?;

            info!(
                "Successfully migrated to version 94 (workflow AI sessions for restart survival)"
            );
        }

        // Version 95: Quality improvements — dependency graph, cost annotations, quality report
        if current_version < 95 {
            conn.execute_batch(
                r#"
                ALTER TABLE unified_workflows ADD COLUMN dependency_graph TEXT DEFAULT NULL;
                ALTER TABLE unified_workflows ADD COLUMN cost_annotations TEXT DEFAULT NULL;
                ALTER TABLE unified_workflows ADD COLUMN quality_report TEXT DEFAULT NULL;

                ALTER TABLE generation_pipeline_artifacts ADD COLUMN revision_duration_ms INTEGER DEFAULT NULL;
                ALTER TABLE generation_pipeline_artifacts ADD COLUMN quality_report TEXT DEFAULT NULL;
                ALTER TABLE generation_pipeline_artifacts ADD COLUMN revision_cycles INTEGER DEFAULT NULL;

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (95, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 95: {}", e))?;

            info!("Successfully migrated to version 95 (quality improvements)");
        }

        // Version 96: Add workflow_id to task_runs for linking to unified_workflows
        if current_version < 96 {
            conn.execute_batch(
                r#"
                ALTER TABLE task_runs ADD COLUMN workflow_id TEXT DEFAULT NULL;
                CREATE INDEX IF NOT EXISTS idx_task_runs_workflow_id ON task_runs(workflow_id);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (96, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 96: {}", e))?;

            info!("Successfully migrated to version 96 (workflow_id on task_runs)");
        }

        if current_version < 97 {
            info!("Migrating to version 97 (project reflection: scope + project_path columns)...");

            conn.execute_batch(
                r#"
                ALTER TABLE reflection_fixes ADD COLUMN reflection_scope TEXT DEFAULT 'workflow';
                ALTER TABLE reflection_fixes ADD COLUMN project_path TEXT;
                ALTER TABLE task_knowledge ADD COLUMN project_path TEXT;

                CREATE INDEX IF NOT EXISTS idx_reflection_fixes_project ON reflection_fixes(project_path);
                CREATE INDEX IF NOT EXISTS idx_reflection_fixes_scope ON reflection_fixes(reflection_scope);
                CREATE INDEX IF NOT EXISTS idx_task_knowledge_project ON task_knowledge(project_path);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (97, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 97: {}", e))?;

            info!("Successfully migrated to version 97 (project reflection columns)");
        }

        // Migration to version 98: Add workflow_constraint_results table
        // Stores constraint engine evaluation results per-iteration for post-run review
        if current_version < 98 {
            info!("Migrating to version 98 (workflow_constraint_results table)...");

            conn.execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS workflow_constraint_results (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    task_run_id TEXT NOT NULL,
                    iteration INTEGER NOT NULL,
                    constraint_id TEXT NOT NULL,
                    constraint_name TEXT NOT NULL,
                    passed INTEGER NOT NULL,
                    severity TEXT NOT NULL,
                    violations_json TEXT,
                    created_at TEXT NOT NULL DEFAULT (datetime('now')),

                    FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE
                );

                CREATE INDEX IF NOT EXISTS idx_wf_constraint_task_run ON workflow_constraint_results(task_run_id);
                CREATE INDEX IF NOT EXISTS idx_wf_constraint_iteration ON workflow_constraint_results(iteration);
                CREATE INDEX IF NOT EXISTS idx_wf_constraint_passed ON workflow_constraint_results(passed);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (98, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 98: {}", e))?;

            info!("Successfully migrated to version 98 (workflow_constraint_results table)");
        }

        // Migration to version 99: Cognitive System Model
        // Adds knowledge properties (accumulation monotonicity, convergence gradient,
        // relevance decay) and prediction capabilities to the reflection system.
        if current_version < 99 {
            info!("Migrating to version 99 (cognitive system model)...");

            conn.execute_batch(
                r#"
                -- New columns on reflection_fixes
                ALTER TABLE reflection_fixes ADD COLUMN target_component TEXT;
                ALTER TABLE reflection_fixes ADD COLUMN reuse_count INTEGER DEFAULT 0;

                -- New columns on task_knowledge
                ALTER TABLE task_knowledge ADD COLUMN last_validated_at TEXT;
                ALTER TABLE task_knowledge ADD COLUMN validation_count INTEGER DEFAULT 0;

                -- New column on error_events
                ALTER TABLE error_events ADD COLUMN resolved_by_fix_id TEXT;

                -- Fix applications: tracks each time a fix is reused
                CREATE TABLE IF NOT EXISTS fix_applications (
                    id TEXT PRIMARY KEY,
                    fix_id TEXT NOT NULL,
                    task_run_id TEXT NOT NULL,
                    error_signature_hash TEXT,
                    outcome TEXT DEFAULT 'pending',
                    applied_at TEXT NOT NULL,
                    evaluated_at TEXT,
                    FOREIGN KEY (fix_id) REFERENCES reflection_fixes(id) ON DELETE CASCADE,
                    FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE
                );
                CREATE INDEX IF NOT EXISTS idx_fix_applications_fix ON fix_applications(fix_id);
                CREATE INDEX IF NOT EXISTS idx_fix_applications_task ON fix_applications(task_run_id);
                CREATE INDEX IF NOT EXISTS idx_fix_applications_sig ON fix_applications(error_signature_hash);

                -- Convergence snapshots: time-series convergence metrics
                CREATE TABLE IF NOT EXISTS convergence_snapshots (
                    id TEXT PRIMARY KEY,
                    workflow_name TEXT NOT NULL,
                    project_path TEXT,
                    scope TEXT NOT NULL DEFAULT 'workflow',
                    convergence_score REAL NOT NULL,
                    consecutive_clean_runs INTEGER NOT NULL,
                    novelty_score REAL NOT NULL,
                    effective_fix_rate REAL NOT NULL,
                    change_velocity REAL NOT NULL,
                    total_fixes INTEGER NOT NULL,
                    effective_fixes INTEGER NOT NULL,
                    snapshot_at TEXT NOT NULL
                );
                CREATE INDEX IF NOT EXISTS idx_convergence_workflow ON convergence_snapshots(workflow_name);
                CREATE INDEX IF NOT EXISTS idx_convergence_project ON convergence_snapshots(project_path);
                CREATE INDEX IF NOT EXISTS idx_convergence_scope ON convergence_snapshots(scope);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (99, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 99: {}", e))?;

            info!("Successfully migrated to version 99 (cognitive system model)");
        }

        // Migration to version 100: Causal Chain Tracking
        // Adds directed cause→effect graph for tracking causal relationships between
        // events (findings, errors, fixes, verifications).
        if current_version < 100 {
            info!("Migrating to version 100 (causal chain tracking)...");

            conn.execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS causal_events (
                    id TEXT PRIMARY KEY,
                    cause_event_type TEXT NOT NULL,
                    cause_event_id TEXT NOT NULL,
                    effect_event_type TEXT NOT NULL,
                    effect_event_id TEXT NOT NULL,
                    relationship TEXT NOT NULL,
                    confidence TEXT NOT NULL DEFAULT 'high',
                    source TEXT NOT NULL DEFAULT 'automated',
                    task_run_id TEXT,
                    workflow_name TEXT,
                    description TEXT,
                    created_at TEXT NOT NULL DEFAULT (datetime('now'))
                );
                CREATE INDEX IF NOT EXISTS idx_causal_cause ON causal_events(cause_event_type, cause_event_id);
                CREATE INDEX IF NOT EXISTS idx_causal_effect ON causal_events(effect_event_type, effect_event_id);
                CREATE INDEX IF NOT EXISTS idx_causal_workflow ON causal_events(workflow_name);
                CREATE INDEX IF NOT EXISTS idx_causal_task_run ON causal_events(task_run_id);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (100, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 100: {}", e))?;

            info!("Successfully migrated to version 100 (causal chain tracking)");
        }

        // Migration to version 101: Architecture Model
        // Aggregated component-level data from reflection fixes, causal events,
        // and knowledge into a queryable graph of components and relationships.
        if current_version < 101 {
            info!("Migrating to version 101 (architecture model)...");

            conn.execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS architecture_components (
                    id TEXT PRIMARY KEY,
                    workflow_name TEXT NOT NULL,
                    component_path TEXT NOT NULL,
                    component_type TEXT NOT NULL DEFAULT 'file',
                    fix_count INTEGER NOT NULL DEFAULT 0,
                    error_count INTEGER NOT NULL DEFAULT 0,
                    causal_involvement_count INTEGER NOT NULL DEFAULT 0,
                    effective_fix_count INTEGER NOT NULL DEFAULT 0,
                    ineffective_fix_count INTEGER NOT NULL DEFAULT 0,
                    health_score REAL NOT NULL DEFAULT 1.0,
                    change_velocity REAL NOT NULL DEFAULT 0.0,
                    last_activity_at TEXT,
                    created_at TEXT NOT NULL DEFAULT (datetime('now')),
                    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
                    UNIQUE(workflow_name, component_path)
                );
                CREATE INDEX IF NOT EXISTS idx_arch_comp_workflow ON architecture_components(workflow_name);
                CREATE INDEX IF NOT EXISTS idx_arch_comp_health ON architecture_components(health_score);

                CREATE TABLE IF NOT EXISTS component_relationships (
                    id TEXT PRIMARY KEY,
                    workflow_name TEXT NOT NULL,
                    source_component TEXT NOT NULL,
                    target_component TEXT NOT NULL,
                    relationship_type TEXT NOT NULL,
                    strength INTEGER NOT NULL DEFAULT 1,
                    last_seen_at TEXT,
                    created_at TEXT NOT NULL DEFAULT (datetime('now')),
                    UNIQUE(workflow_name, source_component, target_component, relationship_type)
                );
                CREATE INDEX IF NOT EXISTS idx_comp_rel_workflow ON component_relationships(workflow_name);
                CREATE INDEX IF NOT EXISTS idx_comp_rel_source ON component_relationships(source_component);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (101, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 101: {}", e))?;

            info!("Successfully migrated to version 101 (architecture model)");
        }

        // Migration to version 102: Add constraint_overrides to unified_workflows
        if current_version < 102 {
            info!("Migrating to version 102 (add constraint_overrides to unified_workflows)...");

            conn.execute_batch(
                r#"
                ALTER TABLE unified_workflows ADD COLUMN constraint_overrides TEXT DEFAULT '{}';
                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (102, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 102: {}", e))?;

            info!("Successfully migrated to version 102 (constraint_overrides)");
        }

        // Migration to version 103: Component health snapshots for temporal trends
        if current_version < 103 {
            info!("Migrating to version 103 (component health snapshots)...");

            conn.execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS component_health_snapshots (
                    id TEXT PRIMARY KEY,
                    workflow_name TEXT NOT NULL,
                    component_path TEXT NOT NULL,
                    health_score REAL NOT NULL,
                    fix_count INTEGER NOT NULL DEFAULT 0,
                    effective_fix_count INTEGER NOT NULL DEFAULT 0,
                    change_velocity REAL NOT NULL DEFAULT 0.0,
                    snapshot_at TEXT NOT NULL DEFAULT (datetime('now'))
                );
                CREATE INDEX IF NOT EXISTS idx_comp_health_snap_wf ON component_health_snapshots(workflow_name);
                CREATE INDEX IF NOT EXISTS idx_comp_health_snap_comp ON component_health_snapshots(workflow_name, component_path);
                CREATE INDEX IF NOT EXISTS idx_comp_health_snap_at ON component_health_snapshots(snapshot_at);
                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (103, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 103: {}", e))?;

            info!("Successfully migrated to version 103 (component health snapshots)");
        }

        // Migration to version 104: Decision context capture
        if current_version < 104 {
            info!("Migrating to version 104 (decision context capture)...");

            conn.execute_batch(
                r#"
                ALTER TABLE reflection_fixes ADD COLUMN reasoning TEXT;
                ALTER TABLE reflection_fixes ADD COLUMN alternatives_considered TEXT;
                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (104, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 104: {}", e))?;

            info!("Successfully migrated to version 104 (decision context capture)");
        }

        // Migration to version 105: Cross-project patterns (hybrid RAG)
        if current_version < 105 {
            info!("Migrating to version 105 (cross-project patterns)...");

            conn.execute_batch(
                r#"
                ALTER TABLE reflection_fixes ADD COLUMN applicability_context TEXT;
                ALTER TABLE reflection_fixes ADD COLUMN fix_description_embedding BLOB;
                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (105, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 105: {}", e))?;

            info!("Successfully migrated to version 105 (cross-project patterns)");
        }

        if current_version < 106 {
            info!("Migrating to version 106 (generation rule application tracking)...");

            conn.execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS rule_applications (
                    id TEXT PRIMARY KEY,
                    rule_id TEXT NOT NULL,
                    workflow_id TEXT,
                    task_run_id TEXT,
                    agent TEXT NOT NULL,
                    section TEXT NOT NULL,
                    applied_at TEXT NOT NULL,
                    FOREIGN KEY (rule_id) REFERENCES generation_rules(id) ON DELETE CASCADE
                );
                CREATE INDEX IF NOT EXISTS idx_rule_apps_rule ON rule_applications(rule_id);
                CREATE INDEX IF NOT EXISTS idx_rule_apps_workflow ON rule_applications(workflow_id);
                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (106, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 106: {}", e))?;

            info!("Successfully migrated to version 106 (generation rule application tracking)");
        }

        if current_version < 107 {
            info!("Migrating to version 107 (causal events dedup index)...");

            conn.execute_batch(
                r#"
                CREATE UNIQUE INDEX IF NOT EXISTS idx_causal_dedup ON causal_events(cause_event_type, cause_event_id, effect_event_type, effect_event_id);
                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (107, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 107: {}", e))?;

            info!("Successfully migrated to version 107 (causal events dedup index)");
        }

        if current_version < 108 {
            info!("Migrating to version 108 (remove proxy_port from ui_bridge_integrations)...");

            conn.execute_batch(
                r#"
                ALTER TABLE ui_bridge_integrations DROP COLUMN proxy_port;
                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (108, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 108: {}", e))?;

            info!("Successfully migrated to version 108 (remove proxy_port)");
        }

        if current_version < 109 {
            info!("Migrating to version 109 (add confidence_score to pipeline artifacts)...");

            conn.execute_batch(
                r#"
                ALTER TABLE generation_pipeline_artifacts ADD COLUMN confidence_score REAL DEFAULT NULL;

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (109, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 109: {}", e))?;

            info!("Successfully migrated to version 109 (confidence_score)");
        }

        if current_version < 110 {
            info!("Migrating to version 110 (state machine element thumbnails)...");

            conn.execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS sm_element_thumbnails (
                    config_id TEXT NOT NULL REFERENCES state_machine_configs(id) ON DELETE CASCADE,
                    fingerprint_hash TEXT NOT NULL,
                    thumbnail_base64 TEXT NOT NULL,
                    PRIMARY KEY (config_id, fingerprint_hash)
                );

                CREATE INDEX IF NOT EXISTS idx_sm_thumbnails_config ON sm_element_thumbnails(config_id);

                INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (110, datetime('now'));
                "#,
            )
            .map_err(|e| format!("Failed to migrate to version 110: {}", e))?;

            info!("Successfully migrated to version 110 (sm_element_thumbnails)");
        }

        // Repair migration: is_favorite column may be missing on databases created from
        // schema.sql (which set version >= 94, skipping migration 92 that adds the column).
        // This is idempotent — ALTER TABLE ADD COLUMN fails if the column already exists,
        // so we check for it first via PRAGMA table_info.
        {
            let has_is_favorite: bool = conn
                .prepare("PRAGMA table_info(unified_workflows)")
                .and_then(|mut stmt| {
                    stmt.query_map([], |row| row.get::<_, String>(1))
                        .map(|rows| {
                            rows.filter_map(|r| r.ok())
                                .any(|name| name == "is_favorite")
                        })
                })
                .unwrap_or(false);

            if !has_is_favorite {
                info!("Repair: adding missing is_favorite column to unified_workflows");
                conn.execute_batch(
                    r#"
                    ALTER TABLE unified_workflows ADD COLUMN is_favorite INTEGER DEFAULT 0;
                    CREATE INDEX IF NOT EXISTS idx_unified_workflows_is_favorite ON unified_workflows(is_favorite);
                    "#,
                )
                .map_err(|e| format!("Failed to repair unified_workflows.is_favorite: {}", e))?;
                info!("Repair: successfully added is_favorite column");
            }
        }

        // Repair migration: cached_app_specs table may be missing on databases created from
        // schema.sql before the table was added (schema.sql skipped migration 69).
        {
            let has_table: bool = conn
                .prepare(
                    "SELECT name FROM sqlite_master WHERE type='table' AND name='cached_app_specs'",
                )
                .and_then(|mut stmt| {
                    stmt.query_map([], |row| row.get::<_, String>(0))
                        .map(|rows| rows.filter_map(|r| r.ok()).count() > 0)
                })
                .unwrap_or(false);

            if !has_table {
                info!("Repair: creating missing cached_app_specs table");
                conn.execute_batch(
                    r#"
                    CREATE TABLE IF NOT EXISTS cached_app_specs (
                        id TEXT PRIMARY KEY,
                        app_url TEXT NOT NULL,
                        app_name TEXT NOT NULL,
                        spec_id TEXT NOT NULL,
                        spec_json TEXT NOT NULL,
                        discovered_at TEXT NOT NULL,
                        page_url TEXT
                    );
                    CREATE INDEX IF NOT EXISTS idx_cached_specs_app ON cached_app_specs(app_url);
                    "#,
                )
                .map_err(|e| format!("Failed to repair cached_app_specs: {}", e))?;
                info!("Repair: successfully created cached_app_specs table");
            }
        }

        Ok(())
    }

    // ========================================================================
    // Checkpoint/Workflow Operations
    // ========================================================================

    /// Get a checkpoint by workflow name.
    pub fn get_checkpoint(&self, workflow_name: &str) -> Result<Option<CheckpointData>, String> {
        let conn = self.get_conn()?;

        let result: SqliteResult<CheckpointData> = conn.query_row(
            r#"
            SELECT
                NULL as session_id,
                workflow_name,
                json_extract(checkpoint_data, '$.current_phase') as current_phase,
                json_extract(checkpoint_data, '$.total_phases') as total_phases,
                completed,
                json_extract(checkpoint_data, '$.restart_permitted') as restart_permitted,
                json_extract(checkpoint_data, '$.status') as status,
                run_id,
                json_extract(checkpoint_data, '$.repos_to_process') as repos_to_process,
                json_extract(checkpoint_data, '$.work_completed') as work_completed,
                json_extract(checkpoint_data, '$.items_needing_user_input') as items_needing_user_input,
                created_at,
                updated_at,
                json_extract(checkpoint_data, '$.error_message') as error_message,
                checkpoint_data as extra
            FROM active_workflows
            WHERE workflow_name = ?1
            "#,
            params![workflow_name],
            |row| {
                Ok(CheckpointData {
                    session_id: row.get(0).ok(),
                    workflow_name: row.get(1).ok(),
                    current_phase: row.get::<_, Option<i64>>(2)?.unwrap_or(0) as u32,
                    total_phases: row.get::<_, Option<i64>>(3)?.map(|v| v as u32),
                    completed: row.get::<_, i32>(4)? != 0,
                    restart_permitted: row.get::<_, Option<i32>>(5)?.unwrap_or(0) != 0,
                    status: row.get(6).ok(),
                    run_id: row.get(7).ok(),
                    repos_to_process: row
                        .get::<_, Option<String>>(8)?
                        .and_then(|s| serde_json::from_str(&s).ok()),
                    work_completed: row
                        .get::<_, Option<String>>(9)?
                        .and_then(|s| serde_json::from_str(&s).ok()),
                    items_needing_user_input: row
                        .get::<_, Option<String>>(10)?
                        .and_then(|s| serde_json::from_str(&s).ok()),
                    created_at: row.get(11).ok(),
                    updated_at: row.get(12).ok(),
                    error_message: row.get(13).ok(),
                    extra: row
                        .get::<_, Option<String>>(14)?
                        .and_then(|s| serde_json::from_str(&s).ok()),
                })
            },
        );

        match result {
            Ok(checkpoint) => Ok(Some(checkpoint)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("Failed to get checkpoint: {}", e)),
        }
    }

    /// Save or update a checkpoint.
    pub fn save_checkpoint(&self, data: &CheckpointData) -> Result<(), String> {
        let conn = self.get_conn()?;

        let workflow_name = data
            .workflow_name
            .as_ref()
            .ok_or("workflow_name is required")?;

        let now = Utc::now().to_rfc3339();
        let run_id = data
            .run_id
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

        // Build checkpoint_data JSON
        let checkpoint_data = serde_json::json!({
            "current_phase": data.current_phase,
            "total_phases": data.total_phases,
            "completed": data.completed,
            "restart_permitted": data.restart_permitted,
            "status": data.status,
            "repos_to_process": data.repos_to_process,
            "work_completed": data.work_completed,
            "items_needing_user_input": data.items_needing_user_input,
            "error_message": data.error_message,
        });

        // Upsert into active_workflows
        conn.execute(
            r#"
            INSERT INTO active_workflows (workflow_name, checkpoint_data, run_id, phase_field, completion_value, created_at, updated_at, completed)
            VALUES (?1, ?2, ?3, 'current_phase', 12, ?4, ?4, ?5)
            ON CONFLICT(workflow_name) DO UPDATE SET
                checkpoint_data = ?2,
                run_id = CASE WHEN ?3 != '' THEN ?3 ELSE run_id END,
                updated_at = ?4,
                completed = ?5
            "#,
            params![
                workflow_name,
                checkpoint_data.to_string(),
                run_id,
                now,
                data.completed as i32,
            ],
        )
        .map_err(|e| format!("Failed to save checkpoint: {}", e))?;

        // Also record in session_events for history
        if let Some(session_id) = &data.session_id {
            let _ = conn.execute(
                r#"
                INSERT INTO session_events (session_id, event_type, message, timestamp, data)
                VALUES (?1, 'checkpoint_updated', ?2, ?3, ?4)
                "#,
                params![
                    session_id,
                    format!("Phase {} updated", data.current_phase),
                    now,
                    checkpoint_data.to_string(),
                ],
            );
        }

        Ok(())
    }

    /// Delete a checkpoint by workflow name.
    pub fn delete_checkpoint(&self, workflow_name: &str) -> Result<bool, String> {
        let conn = self.get_conn()?;

        let rows = conn
            .execute(
                "DELETE FROM active_workflows WHERE workflow_name = ?1",
                params![workflow_name],
            )
            .map_err(|e| format!("Failed to delete checkpoint: {}", e))?;

        Ok(rows > 0)
    }

    /// List all active (non-completed) checkpoints.
    pub fn list_active_checkpoints(&self) -> Result<Vec<CheckpointData>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
            SELECT
                workflow_name,
                json_extract(checkpoint_data, '$.current_phase') as current_phase,
                completed,
                run_id,
                created_at,
                updated_at
            FROM active_workflows
            WHERE completed = 0
            ORDER BY updated_at DESC
            "#,
            )
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        let checkpoints = stmt
            .query_map([], |row| {
                Ok(CheckpointData {
                    session_id: None,
                    workflow_name: row.get(0).ok(),
                    current_phase: row.get::<_, Option<i64>>(1)?.unwrap_or(0) as u32,
                    total_phases: None,
                    completed: row.get::<_, i32>(2)? != 0,
                    restart_permitted: false,
                    status: None,
                    run_id: row.get(3).ok(),
                    repos_to_process: None,
                    work_completed: None,
                    items_needing_user_input: None,
                    created_at: row.get(4).ok(),
                    updated_at: row.get(5).ok(),
                    error_message: None,
                    extra: None,
                })
            })
            .map_err(|e| format!("Failed to execute query: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(checkpoints)
    }

    /// Check external checkpoint status (for cross-session continuation).
    /// Returns (is_complete, current_phase) or None if not found.
    pub fn check_checkpoint_status(
        &self,
        workflow_name: &str,
        completion_value: u32,
    ) -> Result<Option<(bool, u32)>, String> {
        let conn = self.get_conn()?;

        let result: SqliteResult<(i32, i64)> = conn.query_row(
            r#"
            SELECT
                completed,
                COALESCE(json_extract(checkpoint_data, '$.current_phase'), 0) as current_phase
            FROM active_workflows
            WHERE workflow_name = ?1
            "#,
            params![workflow_name],
            |row| Ok((row.get(0)?, row.get(1)?)),
        );

        match result {
            Ok((completed_flag, current_phase)) => {
                let current = current_phase as u32;
                let is_complete = completed_flag != 0 || current >= completion_value;
                Ok(Some((is_complete, current)))
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("Failed to check checkpoint status: {}", e)),
        }
    }

    // ========================================================================
    // Session Operations
    // ========================================================================

    /// Create a new session.
    pub fn create_session(
        &self,
        id: &str,
        session_type: &str,
        name: &str,
        workflow_name: Option<&str>,
        run_id: Option<&str>,
    ) -> Result<(), String> {
        let conn = self.get_conn()?;

        let now = Utc::now().to_rfc3339();

        conn.execute(
            r#"
            INSERT INTO sessions (id, session_type, name, status, created_at, updated_at, workflow_name, run_id)
            VALUES (?1, ?2, ?3, 'starting', ?4, ?4, ?5, ?6)
            "#,
            params![id, session_type, name, now, workflow_name, run_id],
        )
        .map_err(|e| format!("Failed to create session: {}", e))?;

        // Record start event
        conn.execute(
            r#"
            INSERT INTO session_events (session_id, event_type, message, timestamp)
            VALUES (?1, 'started', 'Session started', ?2)
            "#,
            params![id, now],
        )
        .map_err(|e| format!("Failed to record session event: {}", e))?;

        Ok(())
    }

    /// Update session status.
    pub fn update_session_status(
        &self,
        session_id: &str,
        status: &str,
        current_phase: Option<u32>,
        error_message: Option<&str>,
    ) -> Result<(), String> {
        let conn = self.get_conn()?;

        let now = Utc::now().to_rfc3339();
        let completed = status == "completed" || status == "failed";
        let completed_at = if completed { Some(now.clone()) } else { None };

        conn.execute(
            r#"
            UPDATE sessions SET
                status = ?1,
                current_phase = COALESCE(?2, current_phase),
                completed = ?3,
                completed_at = COALESCE(?4, completed_at),
                error_message = COALESCE(?5, error_message),
                updated_at = ?6
            WHERE id = ?7
            "#,
            params![
                status,
                current_phase.map(|p| p as i64),
                completed as i32,
                completed_at,
                error_message,
                now,
                session_id,
            ],
        )
        .map_err(|e| format!("Failed to update session: {}", e))?;

        // Record event
        conn.execute(
            r#"
            INSERT INTO session_events (session_id, event_type, message, timestamp)
            VALUES (?1, 'status_changed', ?2, ?3)
            "#,
            params![session_id, format!("Status changed to {}", status), now],
        )
        .map_err(|e| format!("Failed to record session event: {}", e))?;

        Ok(())
    }

    /// Get session history/events.
    pub fn get_session_history(
        &self,
        workflow_name: Option<&str>,
        limit: u32,
    ) -> Result<Vec<SessionEvent>, String> {
        let conn = self.get_conn()?;

        // Helper function to map a row to SessionEvent
        fn map_row(row: &rusqlite::Row) -> rusqlite::Result<SessionEvent> {
            Ok(SessionEvent {
                id: row.get(0)?,
                session_id: row.get(1)?,
                event_type: row.get(2)?,
                message: row.get(3)?,
                timestamp: row.get(4)?,
                data: row
                    .get::<_, Option<String>>(5)?
                    .and_then(|s| serde_json::from_str(&s).ok()),
            })
        }

        let events: Vec<SessionEvent> = if let Some(wf) = workflow_name {
            let mut stmt = conn
                .prepare(
                    r#"
                    SELECT e.id, e.session_id, e.event_type, e.message, e.timestamp, e.data
                    FROM session_events e
                    JOIN sessions s ON e.session_id = s.id
                    WHERE s.workflow_name = ?1
                    ORDER BY e.timestamp DESC
                    LIMIT ?2
                    "#,
                )
                .map_err(|e| format!("Failed to prepare query: {}", e))?;

            let result: Vec<SessionEvent> = stmt
                .query_map(params![wf, limit], map_row)
                .map_err(|e| format!("Failed to execute query: {}", e))?
                .filter_map(|r| r.ok())
                .collect();
            result
        } else {
            let mut stmt = conn
                .prepare(
                    r#"
                    SELECT id, session_id, event_type, message, timestamp, data
                    FROM session_events
                    ORDER BY timestamp DESC
                    LIMIT ?1
                    "#,
                )
                .map_err(|e| format!("Failed to prepare query: {}", e))?;

            let result: Vec<SessionEvent> = stmt
                .query_map(params![limit], map_row)
                .map_err(|e| format!("Failed to execute query: {}", e))?
                .filter_map(|r| r.ok())
                .collect();
            result
        };

        Ok(events)
    }

    // ========================================================================
    // Task Run Operations (Simplified Task Model)
    // ========================================================================

    /// Create a new task run using the builder pattern.
    ///
    /// This is the canonical method for creating task runs. Use `CreateTaskRunInput::new()`
    /// with builder methods to construct the input.
    ///
    /// # Example
    /// ```rust
    /// use crate::database::{CheckpointDb, CreateTaskRunInput};
    ///
    /// let input = CreateTaskRunInput::new("task-123", "My Task")
    ///     .with_prompt("Do something useful")
    ///     .with_config_id("config-456")
    ///     .with_workflow_type("unified");
    ///
    /// let task_run = db.create_task_run(&input)?;
    /// ```
    ///
    /// # Workflow Types
    /// - `"unified"` - Uses LoopController for verification-agentic loop. External code
    ///   (TaskMonitor, legacy session code) should NOT modify status.
    /// - `"legacy_session"` - Legacy session-based execution
    /// - `"automation_only"` - Pure automation without AI
    /// - `None` - Legacy/unspecified (for backward compatibility)
    pub fn create_task_run(&self, input: &CreateTaskRunInput) -> Result<TaskRun, String> {
        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();
        let auto_continue_val = input.auto_continue.unwrap_or(true);
        let task_type = input.task_type.as_deref().unwrap_or("task");

        // Default root_task_run_id to self if not provided (root-level task)
        let effective_root = input.root_task_run_id.as_deref().unwrap_or(&input.id);

        conn.execute(
            r#"
            INSERT INTO task_runs (id, task_name, prompt, task_type, status, sessions_count, max_sessions,
                                   output_log, auto_continue, execution_steps_json, log_sources_json,
                                   config_id, workflow_name, workflow_id, workflow_type,
                                   parent_task_run_id, root_task_run_id, depth,
                                   workspace_id, triggered_by, bridge_id,
                                   is_reflection, reflection_source_task_run_id,
                                   is_follow_up, follow_up_source_task_run_id,
                                   created_at, updated_at)
            VALUES (?1, ?2, ?3, ?4, 'running', 0, ?5, '', ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?23)
            "#,
            params![
                input.id,
                input.task_name,
                input.prompt,
                task_type,
                input.max_sessions.map(|v| v as i64),
                auto_continue_val as i32,
                input.execution_steps_json,
                input.log_sources_json,
                input.config_id,
                input.workflow_name,
                input.workflow_id,
                input.workflow_type,
                input.parent_task_run_id,
                effective_root,
                input.depth as i64,
                input.workspace_id,
                input.triggered_by,
                input.bridge_id,
                input.is_reflection as i32,
                input.reflection_source_task_run_id,
                input.is_follow_up as i32,
                input.follow_up_source_task_run_id,
                now
            ],
        )
        .map_err(|e| format!("Failed to create task run: {}", e))?;

        Ok(TaskRun {
            id: input.id.clone(),
            task_name: input.task_name.clone(),
            prompt: input.prompt.clone(),
            task_type: task_type.to_string(),
            status: "running".to_string(),
            sessions_count: 0,
            max_sessions: input.max_sessions,
            output_log: String::new(),
            error_message: None,
            auto_continue: auto_continue_val,
            execution_steps_json: input.execution_steps_json.clone(),
            log_sources_json: input.log_sources_json.clone(),
            config_id: input.config_id.clone(),
            workflow_name: input.workflow_name.clone(),
            workflow_id: input.workflow_id.clone(),
            summary: None,
            ai_summary: None,
            goal_achieved: None,
            remaining_work: None,
            summary_generated_at: None,
            transition_history_json: None,
            workflow_type: input.workflow_type.clone(),
            workspace_id: input.workspace_id.clone(),
            triggered_by: input.triggered_by.clone(),
            parent_task_run_id: input.parent_task_run_id.clone(),
            root_task_run_id: Some(effective_root.to_string()),
            depth: input.depth,
            bridge_id: input.bridge_id.clone(),
            result_data: None,
            is_reflection: input.is_reflection,
            reflection_source_task_run_id: input.reflection_source_task_run_id.clone(),
            is_follow_up: input.is_follow_up,
            follow_up_source_task_run_id: input.follow_up_source_task_run_id.clone(),
            created_at: now.clone(),
            updated_at: now,
            completed_at: None,
        })
    }

    /// Create a new task run with basic parameters.
    ///
    /// **Deprecated:** Use `create_task_run` with `CreateTaskRunInput::new()` builder instead.
    #[deprecated(
        since = "0.1.0",
        note = "Use create_task_run with CreateTaskRunInput::new().with_prompt() builder instead"
    )]
    #[allow(clippy::too_many_arguments)]
    pub fn create_task_run_legacy(
        &self,
        id: &str,
        task_name: &str,
        prompt: Option<&str>,
        max_sessions: Option<u32>,
        auto_continue: Option<bool>,
        execution_steps_json: Option<String>,
        log_sources_json: Option<String>,
    ) -> Result<TaskRun, String> {
        let mut input = CreateTaskRunInput::new(id, task_name);
        if let Some(p) = prompt {
            input = input.with_prompt(p);
        }
        if let Some(ms) = max_sessions {
            input = input.with_max_sessions(ms);
        }
        if let Some(ac) = auto_continue {
            input = input.with_auto_continue(ac);
        }
        if let Some(esj) = execution_steps_json {
            input = input.with_execution_steps_json(esj);
        }
        if let Some(lsj) = log_sources_json {
            input = input.with_log_sources_json(lsj);
        }
        self.create_task_run(&input)
    }

    /// Create a new task run with full configuration options.
    ///
    /// **Deprecated:** Use `create_task_run` with `CreateTaskRunInput::new()` builder instead.
    #[deprecated(
        since = "0.1.0",
        note = "Use create_task_run with CreateTaskRunInput::new().with_*() builder instead"
    )]
    #[allow(clippy::too_many_arguments)]
    pub fn create_task_run_with_config(
        &self,
        id: &str,
        task_name: &str,
        prompt: Option<&str>,
        task_type: &str,
        config_id: Option<&str>,
        workflow_name: Option<&str>,
        max_sessions: Option<u32>,
        auto_continue: Option<bool>,
        execution_steps_json: Option<String>,
        log_sources_json: Option<String>,
    ) -> Result<TaskRun, String> {
        let mut input = CreateTaskRunInput::new(id, task_name).with_task_type(task_type);
        if let Some(p) = prompt {
            input = input.with_prompt(p);
        }
        if let Some(cid) = config_id {
            input = input.with_config_id(cid);
        }
        if let Some(wn) = workflow_name {
            input = input.with_workflow_name(wn);
        }
        if let Some(ms) = max_sessions {
            input = input.with_max_sessions(ms);
        }
        if let Some(ac) = auto_continue {
            input = input.with_auto_continue(ac);
        }
        if let Some(esj) = execution_steps_json {
            input = input.with_execution_steps_json(esj);
        }
        if let Some(lsj) = log_sources_json {
            input = input.with_log_sources_json(lsj);
        }
        self.create_task_run(&input)
    }

    /// Create a new task run with full configuration options including workflow_type.
    ///
    /// **Deprecated:** Use `create_task_run` with `CreateTaskRunInput::new()` builder instead.
    #[deprecated(
        since = "0.1.0",
        note = "Use create_task_run with CreateTaskRunInput::new().with_*() builder instead"
    )]
    #[allow(clippy::too_many_arguments)]
    pub fn create_task_run_with_workflow_type(
        &self,
        id: &str,
        task_name: &str,
        prompt: Option<&str>,
        task_type: &str,
        config_id: Option<&str>,
        workflow_name: Option<&str>,
        max_sessions: Option<u32>,
        auto_continue: Option<bool>,
        execution_steps_json: Option<String>,
        log_sources_json: Option<String>,
        workflow_type: Option<&str>,
    ) -> Result<TaskRun, String> {
        let mut input = CreateTaskRunInput::new(id, task_name).with_task_type(task_type);
        if let Some(p) = prompt {
            input = input.with_prompt(p);
        }
        if let Some(cid) = config_id {
            input = input.with_config_id(cid);
        }
        if let Some(wn) = workflow_name {
            input = input.with_workflow_name(wn);
        }
        if let Some(ms) = max_sessions {
            input = input.with_max_sessions(ms);
        }
        if let Some(ac) = auto_continue {
            input = input.with_auto_continue(ac);
        }
        if let Some(esj) = execution_steps_json {
            input = input.with_execution_steps_json(esj);
        }
        if let Some(lsj) = log_sources_json {
            input = input.with_log_sources_json(lsj);
        }
        if let Some(wt) = workflow_type {
            input = input.with_workflow_type(wt);
        }
        self.create_task_run(&input)
    }

    /// Create a new task run with full configuration options including hierarchy fields.
    ///
    /// **Deprecated:** Use `create_task_run` with `CreateTaskRunInput::new()` builder instead.
    #[deprecated(
        since = "0.1.0",
        note = "Use create_task_run with CreateTaskRunInput::new().with_*() builder instead"
    )]
    #[allow(clippy::too_many_arguments)]
    pub fn create_task_run_with_hierarchy(
        &self,
        id: &str,
        task_name: &str,
        prompt: Option<&str>,
        task_type: &str,
        config_id: Option<&str>,
        workflow_name: Option<&str>,
        max_sessions: Option<u32>,
        auto_continue: Option<bool>,
        execution_steps_json: Option<String>,
        log_sources_json: Option<String>,
        workflow_type: Option<&str>,
        parent_task_run_id: Option<&str>,
        root_task_run_id: Option<&str>,
        depth: u32,
        workspace_id: Option<&str>,
        triggered_by: Option<&str>,
        bridge_id: Option<&str>,
    ) -> Result<TaskRun, String> {
        let mut input = CreateTaskRunInput::new(id, task_name)
            .with_task_type(task_type)
            .with_depth(depth);
        if let Some(p) = prompt {
            input = input.with_prompt(p);
        }
        if let Some(cid) = config_id {
            input = input.with_config_id(cid);
        }
        if let Some(wn) = workflow_name {
            input = input.with_workflow_name(wn);
        }
        if let Some(ms) = max_sessions {
            input = input.with_max_sessions(ms);
        }
        if let Some(ac) = auto_continue {
            input = input.with_auto_continue(ac);
        }
        if let Some(esj) = execution_steps_json {
            input = input.with_execution_steps_json(esj);
        }
        if let Some(lsj) = log_sources_json {
            input = input.with_log_sources_json(lsj);
        }
        if let Some(wt) = workflow_type {
            input = input.with_workflow_type(wt);
        }
        if let Some(ptri) = parent_task_run_id {
            input = input.with_parent_task_run_id(ptri);
        }
        if let Some(rtri) = root_task_run_id {
            input = input.with_root_task_run_id(rtri);
        }
        if let Some(wid) = workspace_id {
            input = input.with_workspace_id(wid);
        }
        if let Some(tb) = triggered_by {
            input = input.with_triggered_by(tb);
        }
        if let Some(bid) = bridge_id {
            input = input.with_bridge_id(bid);
        }
        self.create_task_run(&input)
    }

    /// Get a task run by ID.
    /// Note: output_log is reconstructed from chunks table for backward compatibility.
    pub fn get_task_run(&self, id: &str) -> Result<Option<TaskRun>, String> {
        let conn = self.get_conn()?;

        // Get the task_run metadata including all fields
        let result: SqliteResult<TaskRun> = conn.query_row(
            r#"
            SELECT id, task_name, prompt, task_type, status, sessions_count, max_sessions, error_message, auto_continue,
                   execution_steps_json, log_sources_json, config_id, workflow_name, workflow_id,
                   COALESCE(summary, ai_summary) as summary, ai_summary, goal_achieved, remaining_work,
                   summary_generated_at, transition_history_json, workflow_type,
                   workspace_id, triggered_by,
                   parent_task_run_id, root_task_run_id, depth, bridge_id, result_data,
                   COALESCE(is_reflection, 0) as is_reflection, reflection_source_task_run_id,
                   COALESCE(is_follow_up, 0) as is_follow_up, follow_up_source_task_run_id,
                   created_at, updated_at, completed_at
            FROM task_runs
            WHERE id = ?1
            "#,
            params![id],
            |row| {
                Ok(TaskRun {
                    id: row.get(0)?,
                    task_name: row.get(1)?,
                    prompt: row.get(2)?,
                    task_type: row.get::<_, Option<String>>(3)?.unwrap_or_else(|| "task".to_string()),
                    status: row.get(4)?,
                    sessions_count: row.get::<_, i64>(5)? as u32,
                    max_sessions: row.get::<_, Option<i64>>(6)?.map(|v| v as u32),
                    output_log: String::new(), // Will be filled from chunks
                    error_message: row.get(7)?,
                    auto_continue: row.get::<_, i32>(8)? != 0,
                    execution_steps_json: row.get(9)?,
                    log_sources_json: row.get(10)?,
                    config_id: row.get(11)?,
                    workflow_name: row.get(12)?,
                    workflow_id: row.get(13)?,
                    summary: row.get(14)?,
                    ai_summary: row.get(15)?,
                    goal_achieved: row.get::<_, Option<i32>>(16)?.map(|v| v != 0),
                    remaining_work: row.get(17)?,
                    summary_generated_at: row.get(18)?,
                    transition_history_json: row.get(19)?,
                    workflow_type: row.get(20)?,
                    workspace_id: row.get(21)?,
                    triggered_by: row.get(22)?,
                    parent_task_run_id: row.get(23)?,
                    root_task_run_id: row.get(24)?,
                    depth: row.get::<_, Option<i64>>(25)?.unwrap_or(0) as u32,
                    bridge_id: row.get(26)?,
                    result_data: row.get(27)?,
                    is_reflection: row.get::<_, i32>(28)? != 0,
                    reflection_source_task_run_id: row.get(29)?,
                    is_follow_up: row.get::<_, i32>(30)? != 0,
                    follow_up_source_task_run_id: row.get(31)?,
                    created_at: row.get(32)?,
                    updated_at: row.get(33)?,
                    completed_at: row.get(34)?,
                })
            },
        );

        match result {
            Ok(mut task_run) => {
                // Get output from chunks
                drop(conn); // Release connection before calling another method
                task_run.output_log = self.get_full_task_output(id).unwrap_or_default();
                Ok(Some(task_run))
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("Failed to get task run: {}", e)),
        }
    }

    /// Get all child task runs (direct children) for a parent task.
    ///
    /// Returns task runs where `parent_task_run_id` matches the given parent ID.
    /// This only returns direct children (depth = parent.depth + 1), not all descendants.
    ///
    /// # Example
    /// ```ignore
    /// let children = db.get_child_task_runs("parent-task-123")?;
    /// for child in children {
    ///     println!("Child task: {} (depth: {})", child.task_name, child.depth);
    /// }
    /// ```
    pub fn get_child_task_runs(&self, parent_task_run_id: &str) -> Result<Vec<TaskRun>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, task_name, prompt, task_type, status, sessions_count, max_sessions, error_message, auto_continue,
                       execution_steps_json, log_sources_json, config_id, workflow_name, workflow_id,
                       COALESCE(summary, ai_summary) as summary, ai_summary, goal_achieved, remaining_work,
                       summary_generated_at, transition_history_json, workflow_type,
                       workspace_id, triggered_by,
                       parent_task_run_id, root_task_run_id, depth, bridge_id, result_data,
                       COALESCE(is_reflection, 0) as is_reflection, reflection_source_task_run_id,
                       COALESCE(is_follow_up, 0) as is_follow_up, follow_up_source_task_run_id,
                       created_at, updated_at, completed_at
                FROM task_runs
                WHERE parent_task_run_id = ?1
                ORDER BY created_at ASC
                "#,
            )
            .map_err(|e| format!("Failed to prepare child task query: {}", e))?;

        let task_runs = stmt
            .query_map(params![parent_task_run_id], |row| {
                Ok(TaskRun {
                    id: row.get(0)?,
                    task_name: row.get(1)?,
                    prompt: row.get(2)?,
                    task_type: row
                        .get::<_, Option<String>>(3)?
                        .unwrap_or_else(|| "task".to_string()),
                    status: row.get(4)?,
                    sessions_count: row.get::<_, i64>(5)? as u32,
                    max_sessions: row.get::<_, Option<i64>>(6)?.map(|v| v as u32),
                    output_log: String::new(), // Not fetching chunks for performance
                    error_message: row.get(7)?,
                    auto_continue: row.get::<_, i32>(8)? != 0,
                    execution_steps_json: row.get(9)?,
                    log_sources_json: row.get(10)?,
                    config_id: row.get(11)?,
                    workflow_name: row.get(12)?,
                    workflow_id: row.get(13)?,
                    summary: row.get(14)?,
                    ai_summary: row.get(15)?,
                    goal_achieved: row.get::<_, Option<i32>>(16)?.map(|v| v != 0),
                    remaining_work: row.get(17)?,
                    summary_generated_at: row.get(18)?,
                    transition_history_json: row.get(19)?,
                    workflow_type: row.get(20)?,
                    workspace_id: row.get(21)?,
                    triggered_by: row.get(22)?,
                    parent_task_run_id: row.get(23)?,
                    root_task_run_id: row.get(24)?,
                    depth: row.get::<_, Option<i64>>(25)?.unwrap_or(0) as u32,
                    bridge_id: row.get(26)?,
                    result_data: row.get(27)?,
                    is_reflection: row.get::<_, i32>(28)? != 0,
                    reflection_source_task_run_id: row.get(29)?,
                    is_follow_up: row.get::<_, i32>(30)? != 0,
                    follow_up_source_task_run_id: row.get(31)?,
                    created_at: row.get(32)?,
                    updated_at: row.get(33)?,
                    completed_at: row.get(34)?,
                })
            })
            .map_err(|e| format!("Failed to query child tasks: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(task_runs)
    }

    /// Get all task runs in a hierarchy (all descendants of a root task).
    ///
    /// Returns task runs where `root_task_run_id` matches the given root ID.
    /// This includes all descendants at any depth level.
    ///
    /// # Example
    /// ```ignore
    /// let all_tasks = db.get_task_run_hierarchy("root-task-123")?;
    /// for task in all_tasks {
    ///     let indent = "  ".repeat(task.depth as usize);
    ///     println!("{}Task: {} (depth: {})", indent, task.task_name, task.depth);
    /// }
    /// ```
    pub fn get_task_run_hierarchy(&self, root_task_run_id: &str) -> Result<Vec<TaskRun>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, task_name, prompt, task_type, status, sessions_count, max_sessions, error_message, auto_continue,
                       execution_steps_json, log_sources_json, config_id, workflow_name, workflow_id,
                       COALESCE(summary, ai_summary) as summary, ai_summary, goal_achieved, remaining_work,
                       summary_generated_at, transition_history_json, workflow_type,
                       workspace_id, triggered_by,
                       parent_task_run_id, root_task_run_id, depth, bridge_id, result_data,
                       COALESCE(is_reflection, 0) as is_reflection, reflection_source_task_run_id,
                       COALESCE(is_follow_up, 0) as is_follow_up, follow_up_source_task_run_id,
                       created_at, updated_at, completed_at
                FROM task_runs
                WHERE root_task_run_id = ?1 AND id != ?1
                ORDER BY depth ASC, created_at ASC
                "#,
            )
            .map_err(|e| format!("Failed to prepare hierarchy query: {}", e))?;

        let task_runs = stmt
            .query_map(params![root_task_run_id], |row| {
                Ok(TaskRun {
                    id: row.get(0)?,
                    task_name: row.get(1)?,
                    prompt: row.get(2)?,
                    task_type: row
                        .get::<_, Option<String>>(3)?
                        .unwrap_or_else(|| "task".to_string()),
                    status: row.get(4)?,
                    sessions_count: row.get::<_, i64>(5)? as u32,
                    max_sessions: row.get::<_, Option<i64>>(6)?.map(|v| v as u32),
                    output_log: String::new(), // Not fetching chunks for performance
                    error_message: row.get(7)?,
                    auto_continue: row.get::<_, i32>(8)? != 0,
                    execution_steps_json: row.get(9)?,
                    log_sources_json: row.get(10)?,
                    config_id: row.get(11)?,
                    workflow_name: row.get(12)?,
                    workflow_id: row.get(13)?,
                    summary: row.get(14)?,
                    ai_summary: row.get(15)?,
                    goal_achieved: row.get::<_, Option<i32>>(16)?.map(|v| v != 0),
                    remaining_work: row.get(17)?,
                    summary_generated_at: row.get(18)?,
                    transition_history_json: row.get(19)?,
                    workflow_type: row.get(20)?,
                    workspace_id: row.get(21)?,
                    triggered_by: row.get(22)?,
                    parent_task_run_id: row.get(23)?,
                    root_task_run_id: row.get(24)?,
                    depth: row.get::<_, Option<i64>>(25)?.unwrap_or(0) as u32,
                    bridge_id: row.get(26)?,
                    result_data: row.get(27)?,
                    is_reflection: row.get::<_, i32>(28)? != 0,
                    reflection_source_task_run_id: row.get(29)?,
                    is_follow_up: row.get::<_, i32>(30)? != 0,
                    follow_up_source_task_run_id: row.get(31)?,
                    created_at: row.get(32)?,
                    updated_at: row.get(33)?,
                    completed_at: row.get(34)?,
                })
            })
            .map_err(|e| format!("Failed to query task hierarchy: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(task_runs)
    }

    /// Append output to a task run and increment session count.
    /// Returns true if [TASK_COMPLETE] marker was found in the appended text.
    ///
    /// Uses O(1) chunk insertion instead of O(n) string concatenation.
    /// Output is stored in the task_run_output_chunks table for efficient appending.
    ///
    /// # Arguments
    /// * `id` - Task run ID
    /// * `output` - Output text to append
    /// * `increment_session` - Whether to increment the session count
    /// * `check_completion_marker` - Whether to check for [TASK_COMPLETE] marker and mark task complete.
    ///   Set to `false` for unified workflows where verification is the authority on completion.
    ///
    /// NOTE: This method handles task completion inline to avoid multiple connection acquisitions.
    pub fn append_task_output_ex(
        &self,
        id: &str,
        output: &str,
        increment_session: bool,
        check_completion_marker: bool,
    ) -> Result<bool, String> {
        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();

        // Get next chunk sequence number
        let next_seq: i64 = conn
            .query_row(
                "SELECT COALESCE(MAX(chunk_sequence), 0) + 1 FROM task_run_output_chunks WHERE task_run_id = ?",
                params![id],
                |row| row.get(0),
            )
            .unwrap_or(1);

        // Insert new chunk (O(1) operation)
        conn.execute(
            "INSERT INTO task_run_output_chunks (task_run_id, chunk_sequence, content, created_at) VALUES (?, ?, ?, ?)",
            params![id, next_seq, output, now],
        )
        .map_err(|e| format!("Failed to insert output chunk: {}", e))?;

        // Update task_run metadata only (no string concatenation)
        let session_increment = if increment_session { 1 } else { 0 };
        conn.execute(
            r#"
            UPDATE task_runs SET
                sessions_count = sessions_count + ?1,
                updated_at = ?2
            WHERE id = ?3
            "#,
            params![session_increment, now, id],
        )
        .map_err(|e| format!("Failed to update task run metadata: {}", e))?;

        // Only check for completion marker if requested
        // Unified workflows set check_completion_marker=false because verification is the authority
        let is_complete = check_completion_marker
            && output
                .lines()
                .any(|line| line.trim() == TASK_COMPLETE_MARKER);
        if is_complete {
            // IMPORTANT: Inline the completion logic here instead of calling complete_task_run()
            // to avoid nested lock acquisition (we already hold the conn lock above).
            conn.execute(
                r#"
                UPDATE task_runs SET
                    status = 'completed',
                    updated_at = ?1,
                    completed_at = ?1
                WHERE id = ?2 AND status = 'running'
                "#,
                params![now, id],
            )
            .map_err(|e| format!("Failed to complete task run: {}", e))?;

            info!("Task run {} marked completed via append_task_output", id);
        }

        Ok(is_complete)
    }

    /// Append output to a task run (legacy wrapper - checks for completion marker).
    ///
    /// This is the backward-compatible version that always checks for [TASK_COMPLETE].
    /// For unified workflows, use `append_task_output_ex` with `check_completion_marker=false`.
    pub fn append_task_output(
        &self,
        id: &str,
        output: &str,
        increment_session: bool,
    ) -> Result<bool, String> {
        self.append_task_output_ex(id, output, increment_session, true)
    }

    // ========================================================================
    // Workflow AI Sessions (restart survival)
    // ========================================================================

    /// Create a new workflow AI session record.
    /// Called when a Claude CLI subprocess is spawned for a workflow phase.
    pub fn create_workflow_ai_session(
        &self,
        task_run_id: &str,
        iteration: i32,
        phase: &str,
        stage_index: Option<i32>,
        claude_cli_session_id: &str,
    ) -> Result<i64, String> {
        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();

        conn.execute(
            r#"
            INSERT INTO workflow_ai_sessions
                (task_run_id, iteration, phase, stage_index, claude_cli_session_id, session_started_at, status)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'running')
            ON CONFLICT (task_run_id, iteration, phase, COALESCE(stage_index, -1))
            DO UPDATE SET
                claude_cli_session_id = ?5,
                session_started_at = ?6,
                session_completed_at = NULL,
                output_length = 0,
                status = 'running'
            "#,
            params![task_run_id, iteration, phase, stage_index, claude_cli_session_id, now],
        )
        .map_err(|e| format!("Failed to create workflow AI session: {}", e))?;

        let row_id = conn.last_insert_rowid();
        info!(
            "Created workflow AI session: task={}, iter={}, phase={}, cli_session={}",
            task_run_id, iteration, phase, claude_cli_session_id
        );
        Ok(row_id)
    }

    /// Mark a workflow AI session as completed, failed, or interrupted.
    pub fn complete_workflow_ai_session(
        &self,
        task_run_id: &str,
        iteration: i32,
        phase: &str,
        stage_index: Option<i32>,
        status: &str,
        output_length: i64,
    ) -> Result<(), String> {
        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();

        conn.execute(
            r#"
            UPDATE workflow_ai_sessions
            SET status = ?1, session_completed_at = ?2, output_length = ?3
            WHERE task_run_id = ?4 AND iteration = ?5 AND phase = ?6
              AND COALESCE(stage_index, -1) = COALESCE(?7, -1)
            "#,
            params![
                status,
                now,
                output_length,
                task_run_id,
                iteration,
                phase,
                stage_index
            ],
        )
        .map_err(|e| format!("Failed to complete workflow AI session: {}", e))?;

        Ok(())
    }

    /// Get the most recent AI session for a task run, filtered by phase and iteration.
    /// Returns (claude_cli_session_id, status) if found.
    pub fn get_workflow_ai_session(
        &self,
        task_run_id: &str,
        iteration: i32,
        phase: &str,
    ) -> Result<Option<(String, String)>, String> {
        let conn = self.get_conn()?;

        let result = conn.query_row(
            r#"
            SELECT claude_cli_session_id, status
            FROM workflow_ai_sessions
            WHERE task_run_id = ?1 AND iteration = ?2 AND phase = ?3
            ORDER BY id DESC
            LIMIT 1
            "#,
            params![task_run_id, iteration, phase],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        );

        match result {
            Ok(session) => Ok(Some(session)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("Failed to get workflow AI session: {}", e)),
        }
    }

    /// Mark all running workflow AI sessions as interrupted.
    /// Called on startup to clean up sessions from a previous runner instance.
    pub fn mark_running_ai_sessions_interrupted(&self) -> Result<usize, String> {
        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();

        let count = conn
            .execute(
                r#"
                UPDATE workflow_ai_sessions
                SET status = 'interrupted', session_completed_at = ?1
                WHERE status = 'running'
                "#,
                params![now],
            )
            .map_err(|e| format!("Failed to mark AI sessions interrupted: {}", e))?;

        if count > 0 {
            info!(
                "Marked {} running AI sessions as interrupted on startup",
                count
            );
        }
        Ok(count)
    }

    /// Flush partial AI output to task_run_output_chunks during a running session.
    /// Uses a dedicated chunk_type marker so the final output can replace it.
    pub fn flush_partial_ai_output(
        &self,
        task_run_id: &str,
        output: &str,
        iteration: i32,
    ) -> Result<(), String> {
        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();
        let like_pattern = format!(
            "\n--- AI Output (Iteration {} — in progress) ---%",
            iteration
        );
        let formatted = format!(
            "\n--- AI Output (Iteration {} — in progress) ---\n{}\n",
            iteration, output
        );

        // Wrap DELETE + INSERT in a transaction so partial output is never lost
        // if the runner crashes between the two operations.
        let tx = conn
            .unchecked_transaction()
            .map_err(|e| format!("Failed to begin flush transaction: {}", e))?;

        // Delete any previous partial flush for this iteration
        // (we always write the full accumulated output, not deltas)
        tx.execute(
            r#"
            DELETE FROM task_run_output_chunks
            WHERE task_run_id = ?1
              AND content LIKE ?2
            "#,
            params![task_run_id, like_pattern],
        )
        .map_err(|e| format!("Failed to delete previous partial flush: {}", e))?;

        // Insert the current partial output
        let next_seq: i64 = tx
            .query_row(
                "SELECT COALESCE(MAX(chunk_sequence), 0) + 1 FROM task_run_output_chunks WHERE task_run_id = ?",
                params![task_run_id],
                |row| row.get(0),
            )
            .unwrap_or_else(|e| {
                tracing::warn!("Failed to get max chunk_sequence for {}: {} — defaulting to 1", task_run_id, e);
                1
            });

        tx.execute(
            "INSERT INTO task_run_output_chunks (task_run_id, chunk_sequence, content, created_at) VALUES (?, ?, ?, ?)",
            params![task_run_id, next_seq, formatted, now],
        )
        .map_err(|e| format!("Failed to flush partial AI output: {}", e))?;

        tx.commit()
            .map_err(|e| format!("Failed to commit flush transaction: {}", e))?;

        Ok(())
    }

    /// Delete partial (in-progress) output chunks for a given iteration.
    /// Called when the final output is written, so the partial flush is replaced.
    pub fn delete_partial_ai_output(
        &self,
        task_run_id: &str,
        iteration: i32,
    ) -> Result<(), String> {
        let conn = self.get_conn()?;

        conn.execute(
            r#"
            DELETE FROM task_run_output_chunks
            WHERE task_run_id = ?1
              AND content LIKE ?2
            "#,
            params![
                task_run_id,
                format!(
                    "\n--- AI Output (Iteration {} — in progress) ---%",
                    iteration
                )
            ],
        )
        .map_err(|e| format!("Failed to delete partial AI output: {}", e))?;

        Ok(())
    }

    /// Mark a task run as complete.
    ///
    /// For unified workflows, this should ONLY be called by the LoopController.
    /// Other code paths (TaskMonitor, legacy session code) should check workflow_type
    /// before calling this method. Consider using `complete_task_run_if_allowed` instead.
    pub fn complete_task_run(&self, id: &str) -> Result<(), String> {
        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();

        conn.execute(
            r#"
            UPDATE task_runs SET
                status = 'completed',
                updated_at = ?1,
                completed_at = ?1
            WHERE id = ?2
            "#,
            params![now, id],
        )
        .map_err(|e| format!("Failed to complete task run: {}", e))?;

        info!("Task run {} marked completed", id);
        Ok(())
    }

    /// Mark a task run as complete, but only if it's not a unified workflow.
    ///
    /// Unified workflows should only have status modified by the LoopController.
    /// This method checks workflow_type and skips the update if it's "unified".
    ///
    /// # Returns
    /// - `Ok(true)` if the task was marked complete
    /// - `Ok(false)` if the task is a unified workflow and was NOT modified
    /// - `Err(...)` if there was a database error
    pub fn complete_task_run_if_allowed(&self, id: &str, caller: &str) -> Result<bool, String> {
        let conn = self.get_conn()?;

        // Check workflow_type first
        let workflow_type: Option<String> = conn
            .query_row(
                "SELECT workflow_type FROM task_runs WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| format!("Failed to get workflow_type: {}", e))?
            .flatten();

        if workflow_type.as_deref() == Some("unified") {
            warn!(
                "BLOCKED: {} attempted to complete unified workflow task {} - only LoopController should modify status",
                caller, id
            );
            return Ok(false);
        }

        let now = Utc::now().to_rfc3339();
        conn.execute(
            r#"
            UPDATE task_runs SET
                status = 'completed',
                updated_at = ?1,
                completed_at = ?1
            WHERE id = ?2
            "#,
            params![now, id],
        )
        .map_err(|e| format!("Failed to complete task run: {}", e))?;

        info!(
            "Task run {} marked completed by {} (workflow_type={:?})",
            id, caller, workflow_type
        );
        Ok(true)
    }

    /// Update a task run's status without changing other fields.
    ///
    /// Used by the unified workflow loop controller to reset task status to "running"
    /// at the start of each iteration, preventing external modifications from
    /// prematurely marking the task as complete or failed.
    pub fn update_task_run_status(&self, id: &str, status: &str) -> Result<(), String> {
        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();

        conn.execute(
            r#"
            UPDATE task_runs SET
                status = ?1,
                updated_at = ?2
            WHERE id = ?3
            "#,
            params![status, now, id],
        )
        .map_err(|e| format!("Failed to update task run status: {}", e))?;

        info!("Task run {} status updated to '{}'", id, status);
        Ok(())
    }

    /// Get the output_log for a task run (from chunks table).
    ///
    /// Returns `Ok(Some(output))` if the task run exists and has output,
    /// `Ok(None)` if the task run has no output, or an error string.
    pub fn get_task_run_output(&self, id: &str) -> Result<Option<String>, String> {
        let output = self.get_full_task_output(id)?;
        if output.is_empty() {
            Ok(None)
        } else {
            Ok(Some(output))
        }
    }

    /// Update the task_name for a task run.
    pub fn update_task_run_name(&self, id: &str, name: &str) -> Result<(), String> {
        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();

        conn.execute(
            r#"
            UPDATE task_runs SET
                task_name = ?1,
                updated_at = ?2
            WHERE id = ?3
            "#,
            params![name, now, id],
        )
        .map_err(|e| format!("Failed to update task run name: {}", e))?;

        info!("Task run {} renamed to '{}'", id, name);
        Ok(())
    }

    /// Update the result_data JSON field on a task run.
    ///
    /// Used by meta-workflow steps (e.g. save_workflow_artifact) to store
    /// structured results like generated workflow IDs.
    pub fn update_task_run_result_data(&self, id: &str, result_data: &str) -> Result<(), String> {
        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();

        conn.execute(
            r#"
            UPDATE task_runs SET
                result_data = ?1,
                updated_at = ?2
            WHERE id = ?3
            "#,
            params![result_data, now, id],
        )
        .map_err(|e| format!("Failed to update task run result_data: {}", e))?;

        info!("Task run {} result_data updated", id);
        Ok(())
    }

    /// Get the result_data JSON from a task run.
    pub fn get_task_run_result_data(&self, id: &str) -> Result<Option<String>, String> {
        let conn = self.get_conn()?;
        conn.query_row(
            "SELECT result_data FROM task_runs WHERE id = ?1",
            params![id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| format!("Failed to get result_data: {}", e))?
        .ok_or_else(|| format!("Task run {} not found", id))
        .map(|v: Option<String>| v)
    }

    /// Mark a task run as failed.
    ///
    /// For unified workflows, this should ONLY be called by the LoopController.
    /// Other code paths (TaskMonitor, legacy session code) should check workflow_type
    /// before calling this method. Consider using `fail_task_run_if_allowed` instead.
    pub fn fail_task_run(&self, id: &str, error_message: &str) -> Result<(), String> {
        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();

        conn.execute(
            r#"
            UPDATE task_runs SET
                status = 'failed',
                error_message = ?1,
                updated_at = ?2,
                completed_at = ?2
            WHERE id = ?3
            "#,
            params![error_message, now, id],
        )
        .map_err(|e| format!("Failed to fail task run: {}", e))?;

        info!("Task run {} marked failed: {}", id, error_message);
        Ok(())
    }

    /// Mark a task run as failed, but only if it's not a unified workflow.
    ///
    /// Unified workflows should only have status modified by the LoopController.
    /// This method checks workflow_type and skips the update if it's "unified".
    ///
    /// # Returns
    /// - `Ok(true)` if the task was marked failed
    /// - `Ok(false)` if the task is a unified workflow and was NOT modified
    /// - `Err(...)` if there was a database error
    pub fn fail_task_run_if_allowed(
        &self,
        id: &str,
        error_message: &str,
        caller: &str,
    ) -> Result<bool, String> {
        let conn = self.get_conn()?;

        // Check workflow_type first
        let workflow_type: Option<String> = conn
            .query_row(
                "SELECT workflow_type FROM task_runs WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| format!("Failed to get workflow_type: {}", e))?
            .flatten();

        if workflow_type.as_deref() == Some("unified") {
            warn!(
                "BLOCKED: {} attempted to fail unified workflow task {} with error '{}' - only LoopController should modify status",
                caller, id, error_message
            );
            return Ok(false);
        }

        let now = Utc::now().to_rfc3339();
        conn.execute(
            r#"
            UPDATE task_runs SET
                status = 'failed',
                error_message = ?1,
                updated_at = ?2,
                completed_at = ?2
            WHERE id = ?3
            "#,
            params![error_message, now, id],
        )
        .map_err(|e| format!("Failed to fail task run: {}", e))?;

        info!(
            "Task run {} marked failed by {} (workflow_type={:?}): {}",
            id, caller, workflow_type, error_message
        );
        Ok(true)
    }

    /// Stop a task run.
    /// Also disables auto_continue to prevent multi-step tasks from restarting.
    pub fn stop_task_run(&self, id: &str) -> Result<(), String> {
        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();

        conn.execute(
            r#"
            UPDATE task_runs SET
                status = 'stopped',
                auto_continue = 0,
                updated_at = ?1,
                completed_at = ?1
            WHERE id = ?2
            "#,
            params![now, id],
        )
        .map_err(|e| format!("Failed to stop task run: {}", e))?;

        Ok(())
    }

    /// Update execution steps for a task run.
    /// Used to add/update deterministic execution steps that should be re-run on session resume.
    pub fn update_task_run_execution_steps(
        &self,
        id: &str,
        execution_steps_json: Option<String>,
        log_sources_json: Option<String>,
    ) -> Result<(), String> {
        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();

        conn.execute(
            r#"
            UPDATE task_runs SET
                execution_steps_json = ?1,
                log_sources_json = ?2,
                updated_at = ?3
            WHERE id = ?4
            "#,
            params![execution_steps_json, log_sources_json, now, id],
        )
        .map_err(|e| format!("Failed to update task run execution steps: {}", e))?;

        Ok(())
    }

    /// Update runtime context for a task run.
    /// Used for storing execution context, replay lineage, and other runtime metadata.
    pub fn update_task_run_runtime_context(
        &self,
        id: &str,
        runtime_context_json: &str,
    ) -> Result<(), String> {
        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();

        conn.execute(
            r#"
            UPDATE task_runs SET
                runtime_context_json = ?1,
                updated_at = ?2
            WHERE id = ?3
            "#,
            params![runtime_context_json, now, id],
        )
        .map_err(|e| format!("Failed to update task run runtime context: {}", e))?;

        Ok(())
    }

    /// Update the transition history for a task run.
    /// This stores the orchestrator's state transition history for stage-based recap.
    pub fn update_task_run_transition_history(
        &self,
        id: &str,
        transition_history_json: &str,
    ) -> Result<(), String> {
        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();

        conn.execute(
            r#"
            UPDATE task_runs SET
                transition_history_json = ?1,
                updated_at = ?2
            WHERE id = ?3
            "#,
            params![transition_history_json, now, id],
        )
        .map_err(|e| format!("Failed to update task run transition history: {}", e))?;

        Ok(())
    }

    /// Get runtime context for a task run.
    pub fn get_task_run_runtime_context(&self, id: &str) -> Result<Option<String>, String> {
        let conn = self.get_conn()?;

        let result: rusqlite::Result<Option<String>> = conn.query_row(
            "SELECT runtime_context_json FROM task_runs WHERE id = ?",
            params![id],
            |row| row.get(0),
        );

        match result {
            Ok(context) => Ok(context),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("Failed to get runtime context: {}", e)),
        }
    }

    /// Get all running (incomplete) task runs.
    /// Note: output_log is empty for performance. Use get_full_task_output() to get output.
    /// Includes execution_steps_json and log_sources_json for re-execution on resume.
    pub fn get_running_task_runs(&self) -> Result<Vec<TaskRun>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, task_name, prompt, status, sessions_count, max_sessions, error_message, auto_continue,
                       execution_steps_json, log_sources_json,
                       COALESCE(summary, ai_summary) as summary, ai_summary,
                       goal_achieved, remaining_work, summary_generated_at,
                       workspace_id, triggered_by,
                       created_at, updated_at, completed_at,
                       task_type, config_id, workflow_name
                FROM task_runs
                WHERE status = 'running' AND (workflow_type IS NULL OR workflow_type != 'chat')
                ORDER BY updated_at DESC
                "#,
            )
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        let task_runs = stmt
            .query_map([], |row| {
                Ok(TaskRun {
                    id: row.get(0)?,
                    task_name: row.get(1)?,
                    prompt: row.get(2)?,
                    status: row.get(3)?,
                    sessions_count: row.get::<_, i64>(4)? as u32,
                    max_sessions: row.get::<_, Option<i64>>(5)?.map(|v| v as u32),
                    output_log: String::new(), // Empty for performance - use get_full_task_output()
                    error_message: row.get(6)?,
                    auto_continue: row.get::<_, i32>(7)? != 0,
                    execution_steps_json: row.get(8)?,
                    log_sources_json: row.get(9)?,
                    summary: row.get(10)?,
                    ai_summary: row.get(11)?,
                    goal_achieved: row.get::<_, Option<i32>>(12)?.map(|v| v != 0),
                    remaining_work: row.get(13)?,
                    summary_generated_at: row.get(14)?,
                    transition_history_json: None,
                    workflow_type: None, // Not queried for performance
                    workspace_id: row.get(15)?,
                    triggered_by: row.get(16)?,
                    parent_task_run_id: None, // Not queried for performance
                    root_task_run_id: None,   // Not queried for performance
                    depth: 0,                 // Not queried for performance
                    bridge_id: None,          // Not queried for performance
                    result_data: None,        // Not queried for performance
                    is_reflection: false,     // Not queried for performance
                    reflection_source_task_run_id: None, // Not queried for performance
                    is_follow_up: false,      // Not queried for performance
                    follow_up_source_task_run_id: None, // Not queried for performance
                    created_at: row.get(17)?,
                    updated_at: row.get(18)?,
                    completed_at: row.get(19)?,
                    task_type: row.get(20)?,
                    config_id: row.get(21)?,
                    workflow_name: row.get(22)?,
                    workflow_id: None,
                })
            })
            .map_err(|e| format!("Failed to execute query: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(task_runs)
    }

    /// Get all running unified workflow task runs for resume on startup.
    /// Returns task runs where status = 'running' AND workflow_type = 'unified'.
    pub fn get_running_unified_workflows(&self) -> Result<Vec<TaskRun>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, task_name, prompt, status, sessions_count, max_sessions, error_message, auto_continue,
                       execution_steps_json, log_sources_json,
                       COALESCE(summary, ai_summary) as summary, ai_summary,
                       goal_achieved, remaining_work, summary_generated_at,
                       workspace_id, triggered_by,
                       created_at, updated_at, completed_at,
                       task_type, config_id, workflow_name
                FROM task_runs
                WHERE status = 'running' AND workflow_type = 'unified'
                ORDER BY updated_at DESC
                "#,
            )
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        let task_runs = stmt
            .query_map([], |row| {
                Ok(TaskRun {
                    id: row.get(0)?,
                    task_name: row.get(1)?,
                    prompt: row.get(2)?,
                    status: row.get(3)?,
                    sessions_count: row.get::<_, i64>(4)? as u32,
                    max_sessions: row.get::<_, Option<i64>>(5)?.map(|v| v as u32),
                    output_log: String::new(), // Empty for performance
                    error_message: row.get(6)?,
                    auto_continue: row.get::<_, i32>(7)? != 0,
                    execution_steps_json: row.get(8)?,
                    log_sources_json: row.get(9)?,
                    summary: row.get(10)?,
                    ai_summary: row.get(11)?,
                    goal_achieved: row.get::<_, Option<i32>>(12)?.map(|v| v != 0),
                    remaining_work: row.get(13)?,
                    summary_generated_at: row.get(14)?,
                    transition_history_json: None,
                    workflow_type: Some("unified".to_string()), // We know it's unified
                    workspace_id: row.get(15)?,
                    triggered_by: row.get(16)?,
                    parent_task_run_id: None, // Not queried for performance
                    root_task_run_id: None,   // Not queried for performance
                    depth: 0,                 // Not queried for performance
                    bridge_id: None,          // Not queried for performance
                    result_data: None,        // Not queried for performance
                    is_reflection: false,     // Not queried for performance
                    reflection_source_task_run_id: None, // Not queried for performance
                    is_follow_up: false,      // Not queried for performance
                    follow_up_source_task_run_id: None, // Not queried for performance
                    created_at: row.get(17)?,
                    updated_at: row.get(18)?,
                    completed_at: row.get(19)?,
                    task_type: row.get(20)?,
                    config_id: row.get(21)?,
                    workflow_name: row.get(22)?,
                    workflow_id: None,
                })
            })
            .map_err(|e| format!("Failed to execute query: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(task_runs)
    }

    /// Get all running AI session task runs for resume on startup.
    /// Returns task runs where status = 'running' AND workflow_type = 'chat'.
    pub fn get_running_ai_sessions(&self) -> Result<Vec<TaskRun>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, task_name, prompt, status, sessions_count, max_sessions, error_message, auto_continue,
                       execution_steps_json, log_sources_json,
                       COALESCE(summary, ai_summary) as summary, ai_summary,
                       goal_achieved, remaining_work, summary_generated_at,
                       workspace_id, triggered_by,
                       created_at, updated_at, completed_at,
                       task_type, config_id, workflow_name
                FROM task_runs
                WHERE status = 'running' AND workflow_type = 'chat'
                ORDER BY updated_at DESC
                "#,
            )
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        let task_runs = stmt
            .query_map([], |row| {
                Ok(TaskRun {
                    id: row.get(0)?,
                    task_name: row.get(1)?,
                    prompt: row.get(2)?,
                    status: row.get(3)?,
                    sessions_count: row.get::<_, i64>(4)? as u32,
                    max_sessions: row.get::<_, Option<i64>>(5)?.map(|v| v as u32),
                    output_log: String::new(), // Empty for performance
                    error_message: row.get(6)?,
                    auto_continue: row.get::<_, i32>(7)? != 0,
                    execution_steps_json: row.get(8)?,
                    log_sources_json: row.get(9)?,
                    summary: row.get(10)?,
                    ai_summary: row.get(11)?,
                    goal_achieved: row.get::<_, Option<i32>>(12)?.map(|v| v != 0),
                    remaining_work: row.get(13)?,
                    summary_generated_at: row.get(14)?,
                    transition_history_json: None,
                    workflow_type: Some("chat".to_string()),
                    workspace_id: row.get(15)?,
                    triggered_by: row.get(16)?,
                    parent_task_run_id: None,
                    root_task_run_id: None,
                    depth: 0,
                    bridge_id: None,
                    result_data: None,
                    is_reflection: false,
                    reflection_source_task_run_id: None,
                    is_follow_up: false,
                    follow_up_source_task_run_id: None,
                    created_at: row.get(17)?,
                    updated_at: row.get(18)?,
                    completed_at: row.get(19)?,
                    task_type: row.get(20)?,
                    config_id: row.get(21)?,
                    workflow_name: row.get(22)?,
                    workflow_id: None,
                })
            })
            .map_err(|e| format!("Failed to execute query: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(task_runs)
    }

    /// Get recent AI sessions (all statuses) for sidebar listing.
    /// Returns lightweight summaries ordered by most recently updated.
    pub fn get_ai_sessions(&self, limit: u32) -> Result<Vec<AiSessionSummary>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, task_name, status, updated_at, created_at
                FROM task_runs
                WHERE workflow_type = 'chat'
                ORDER BY updated_at DESC
                LIMIT ?1
                "#,
            )
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        let sessions = stmt
            .query_map(params![limit], |row| {
                Ok(AiSessionSummary {
                    id: row.get(0)?,
                    task_name: row.get(1)?,
                    status: row.get(2)?,
                    updated_at: row.get(3)?,
                    created_at: row.get(4)?,
                })
            })
            .map_err(|e| format!("Failed to execute query: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(sessions)
    }

    /// Find an incomplete (running) task_run for a specific workflow by config_id.
    /// Returns the most recent running task_run for the given workflow, if any.
    /// Used to enable automatic resume when a workflow is re-run after a crash/restart.
    pub fn get_incomplete_task_run_for_workflow(
        &self,
        workflow_id: &str,
    ) -> Result<Option<String>, String> {
        let conn = self.get_conn()?;

        let result: Result<String, rusqlite::Error> = conn.query_row(
            r#"
            SELECT id
            FROM task_runs
            WHERE config_id = ?1
              AND status = 'running'
              AND workflow_type = 'unified'
            ORDER BY updated_at DESC
            LIMIT 1
            "#,
            params![workflow_id],
            |row| row.get(0),
        );

        match result {
            Ok(id) => Ok(Some(id)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("Failed to get incomplete task run: {}", e)),
        }
    }

    /// Mark an interrupted workflow as failed.
    /// Used when resume is disabled on startup.
    pub fn mark_interrupted_workflow_failed(&self, id: &str) -> Result<(), String> {
        let conn = self.get_conn()?;
        let now = chrono::Utc::now().to_rfc3339();

        conn.execute(
            "UPDATE task_runs SET status = 'failed', error_message = 'Workflow interrupted by runner restart (resume disabled)', completed_at = ?1, updated_at = ?1 WHERE id = ?2",
            params![now, id],
        )
        .map_err(|e| format!("Failed to mark interrupted workflow as failed: {}", e))?;

        Ok(())
    }

    /// Check if there's a running reflection workflow.
    /// Returns the task_run ID if one exists, None otherwise.
    /// Used to prevent duplicate reflection workflows from being created.
    pub fn has_running_reflection_workflow(&self) -> Result<Option<String>, String> {
        let conn = self.get_conn()?;

        let result: Result<String, rusqlite::Error> = conn.query_row(
            r#"
            SELECT id
            FROM task_runs
            WHERE status = 'running'
              AND is_reflection = 1
              AND workflow_type = 'unified'
            ORDER BY created_at DESC
            LIMIT 1
            "#,
            [],
            |row| row.get(0),
        );

        match result {
            Ok(id) => Ok(Some(id)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!(
                "Failed to check for running reflection workflow: {}",
                e
            )),
        }
    }

    /// Check if there's a running error-fix workflow.
    /// Returns the task_run ID if one exists, None otherwise.
    /// Used to prevent duplicate error-fix workflows from being created.
    pub fn has_running_error_fix_workflow(&self) -> Result<Option<String>, String> {
        let conn = self.get_conn()?;

        let result: Result<String, rusqlite::Error> = conn.query_row(
            r#"
            SELECT id
            FROM task_runs
            WHERE status = 'running'
              AND (task_name LIKE 'Fix%Error%' OR task_name LIKE '%error-fix%')
              AND workflow_type = 'unified'
            ORDER BY created_at DESC
            LIMIT 1
            "#,
            [],
            |row| row.get(0),
        );

        match result {
            Ok(id) => Ok(Some(id)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!(
                "Failed to check for running error-fix workflow: {}",
                e
            )),
        }
    }

    /// Get recent task runs (for display in UI).
    /// Note: output_log is empty for performance. Use get_full_task_output() to get output.
    pub fn get_recent_task_runs(&self, limit: u32) -> Result<Vec<TaskRun>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, task_name, prompt, task_type, status, sessions_count, max_sessions, error_message, auto_continue,
                       config_id, workflow_name, COALESCE(summary, ai_summary) as summary, ai_summary,
                       goal_achieved, remaining_work, summary_generated_at,
                       workspace_id, triggered_by,
                       created_at, updated_at, completed_at
                FROM task_runs
                WHERE workflow_type IS NULL OR workflow_type != 'chat'
                ORDER BY updated_at DESC
                LIMIT ?1
                "#,
            )
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        let task_runs = stmt
            .query_map(params![limit], |row| {
                Ok(TaskRun {
                    id: row.get(0)?,
                    task_name: row.get(1)?,
                    prompt: row.get(2)?,
                    task_type: row
                        .get::<_, Option<String>>(3)?
                        .unwrap_or_else(|| "task".to_string()),
                    status: row.get(4)?,
                    sessions_count: row.get::<_, i64>(5)? as u32,
                    max_sessions: row.get::<_, Option<i64>>(6)?.map(|v| v as u32),
                    output_log: String::new(), // Empty for performance - use get_full_task_output()
                    error_message: row.get(7)?,
                    auto_continue: row.get::<_, i32>(8)? != 0,
                    execution_steps_json: None,
                    log_sources_json: None,
                    config_id: row.get(9)?,
                    workflow_name: row.get(10)?,
                    workflow_id: None,
                    summary: row.get(11)?,
                    ai_summary: row.get(12)?,
                    goal_achieved: row.get::<_, Option<i32>>(13)?.map(|v| v != 0),
                    remaining_work: row.get(14)?,
                    summary_generated_at: row.get(15)?,
                    transition_history_json: None,
                    workflow_type: None, // Not queried for performance
                    workspace_id: row.get(16)?,
                    triggered_by: row.get(17)?,
                    parent_task_run_id: None, // Not queried for performance
                    root_task_run_id: None,   // Not queried for performance
                    depth: 0,                 // Not queried for performance
                    bridge_id: None,          // Not queried for performance
                    result_data: None,        // Not queried for performance
                    is_reflection: false,     // Not queried for performance
                    reflection_source_task_run_id: None, // Not queried for performance
                    is_follow_up: false,      // Not queried for performance
                    follow_up_source_task_run_id: None, // Not queried for performance
                    created_at: row.get(18)?,
                    updated_at: row.get(19)?,
                    completed_at: row.get(20)?,
                })
            })
            .map_err(|e| format!("Failed to execute query: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(task_runs)
    }

    /// Get recent task runs with optional workflow_type filter.
    /// When workflow_type is provided, only returns task runs matching that type.
    pub fn get_recent_task_runs_filtered(
        &self,
        limit: u32,
        workflow_type: Option<&str>,
    ) -> Result<Vec<TaskRun>, String> {
        let conn = self.get_conn()?;

        let (sql, params_vec): (String, Vec<Box<dyn rusqlite::types::ToSql>>) = if let Some(wt) =
            workflow_type
        {
            (
                    r#"
                    SELECT id, task_name, prompt, task_type, status, sessions_count, max_sessions, error_message, auto_continue,
                           config_id, workflow_name, COALESCE(summary, ai_summary) as summary, ai_summary,
                           goal_achieved, remaining_work, summary_generated_at,
                           workspace_id, triggered_by, workflow_type,
                           created_at, updated_at, completed_at
                    FROM task_runs
                    WHERE workflow_type = ?1
                    ORDER BY updated_at DESC
                    LIMIT ?2
                    "#.to_string(),
                    vec![
                        Box::new(wt.to_string()) as Box<dyn rusqlite::types::ToSql>,
                        Box::new(limit),
                    ],
                )
        } else {
            (
                    r#"
                    SELECT id, task_name, prompt, task_type, status, sessions_count, max_sessions, error_message, auto_continue,
                           config_id, workflow_name, COALESCE(summary, ai_summary) as summary, ai_summary,
                           goal_achieved, remaining_work, summary_generated_at,
                           workspace_id, triggered_by, workflow_type,
                           created_at, updated_at, completed_at
                    FROM task_runs
                    ORDER BY updated_at DESC
                    LIMIT ?1
                    "#.to_string(),
                    vec![Box::new(limit) as Box<dyn rusqlite::types::ToSql>],
                )
        };

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        let params_refs: Vec<&dyn rusqlite::types::ToSql> =
            params_vec.iter().map(|p| p.as_ref()).collect();

        let task_runs = stmt
            .query_map(params_refs.as_slice(), |row| {
                Ok(TaskRun {
                    id: row.get(0)?,
                    task_name: row.get(1)?,
                    prompt: row.get(2)?,
                    task_type: row
                        .get::<_, Option<String>>(3)?
                        .unwrap_or_else(|| "task".to_string()),
                    status: row.get(4)?,
                    sessions_count: row.get::<_, i64>(5)? as u32,
                    max_sessions: row.get::<_, Option<i64>>(6)?.map(|v| v as u32),
                    output_log: String::new(),
                    error_message: row.get(7)?,
                    auto_continue: row.get::<_, i32>(8)? != 0,
                    execution_steps_json: None,
                    log_sources_json: None,
                    config_id: row.get(9)?,
                    workflow_name: row.get(10)?,
                    workflow_id: None,
                    summary: row.get(11)?,
                    ai_summary: row.get(12)?,
                    goal_achieved: row.get::<_, Option<i32>>(13)?.map(|v| v != 0),
                    remaining_work: row.get(14)?,
                    summary_generated_at: row.get(15)?,
                    transition_history_json: None,
                    workflow_type: row.get(18)?,
                    workspace_id: row.get(16)?,
                    triggered_by: row.get(17)?,
                    parent_task_run_id: None,
                    root_task_run_id: None,
                    depth: 0,
                    bridge_id: None,
                    result_data: None,
                    is_reflection: false, // Not queried for performance
                    reflection_source_task_run_id: None, // Not queried for performance
                    is_follow_up: false,  // Not queried for performance
                    follow_up_source_task_run_id: None, // Not queried for performance
                    created_at: row.get(19)?,
                    updated_at: row.get(20)?,
                    completed_at: row.get(21)?,
                })
            })
            .map_err(|e| format!("Failed to execute query: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(task_runs)
    }

    /// Get the last N characters of output for continuation context.
    pub fn get_task_output_tail(&self, id: &str, chars: usize) -> Result<String, String> {
        let task_run = self
            .get_task_run(id)?
            .ok_or_else(|| format!("Task run not found: {}", id))?;

        let output = &task_run.output_log;
        if output.len() <= chars {
            Ok(output.clone())
        } else {
            let mut start = output.len() - chars;
            // Find the nearest char boundary to avoid panic on multi-byte UTF-8
            while start < output.len() && !output.is_char_boundary(start) {
                start += 1;
            }
            Ok(output[start..].to_string())
        }
    }

    /// Check if a task run should continue (not complete, not stopped, not at max sessions).
    ///
    /// Note: This does NOT check `auto_continue` because that setting only controls
    /// whether to resume on startup, not whether to continue after a step finishes.
    /// Workflows should always continue after steps are finished.
    pub fn should_continue_task(&self, id: &str) -> Result<bool, String> {
        let task_run = self
            .get_task_run(id)?
            .ok_or_else(|| format!("Task run not found: {}", id))?;

        // Already complete or stopped
        if task_run.status != "running" {
            return Ok(false);
        }

        // Check max sessions limit
        if let Some(max) = task_run.max_sessions {
            if task_run.sessions_count >= max {
                return Ok(false);
            }
        }

        Ok(true)
    }

    /// Delete a task run by ID.
    /// Note: CASCADE DELETE will automatically remove associated chunks.
    pub fn delete_task_run(&self, id: &str) -> Result<bool, String> {
        let conn = self.get_conn()?;

        let rows = conn
            .execute("DELETE FROM task_runs WHERE id = ?1", params![id])
            .map_err(|e| format!("Failed to delete task run: {}", e))?;

        Ok(rows > 0)
    }

    /// Get the full output log by joining all chunks.
    /// Use this when you need the complete output (e.g., for display or export).
    pub fn get_full_task_output(&self, id: &str) -> Result<String, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                "SELECT content FROM task_run_output_chunks WHERE task_run_id = ? ORDER BY chunk_sequence",
            )
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        let chunks: Vec<String> = stmt
            .query_map(params![id], |row| row.get(0))
            .map_err(|e| format!("Failed to query chunks: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(chunks.join(""))
    }

    /// Get the auto-continue setting for a specific task run.
    pub fn get_task_auto_continue(&self, id: &str) -> Result<bool, String> {
        let conn = self.get_conn()?;

        let result: SqliteResult<i32> = conn.query_row(
            "SELECT auto_continue FROM task_runs WHERE id = ?1",
            params![id],
            |row| row.get(0),
        );

        match result {
            Ok(value) => Ok(value != 0),
            Err(rusqlite::Error::QueryReturnedNoRows) => Err(format!("Task run not found: {}", id)),
            Err(e) => Err(format!("Failed to get task auto_continue: {}", e)),
        }
    }

    /// Set the auto-continue setting for a specific task run.
    pub fn set_task_auto_continue(&self, id: &str, auto_continue: bool) -> Result<(), String> {
        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();

        let rows = conn
            .execute(
                r#"
                UPDATE task_runs SET
                    auto_continue = ?1,
                    updated_at = ?2
                WHERE id = ?3
                "#,
                params![auto_continue as i32, now, id],
            )
            .map_err(|e| format!("Failed to set task auto_continue: {}", e))?;

        if rows == 0 {
            return Err(format!("Task run not found: {}", id));
        }

        Ok(())
    }

    /// Update the summary for a task run.
    /// Called after task completion to store the summary, goal achievement status, and remaining work.
    /// Note: Updates both 'summary' (new) and 'ai_summary' (legacy) columns for backward compatibility.
    pub fn update_task_summary(
        &self,
        id: &str,
        summary_text: &str,
        goal_achieved: bool,
        remaining_work: Option<&str>,
    ) -> Result<(), String> {
        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();

        let rows = conn
            .execute(
                r#"
                UPDATE task_runs SET
                    summary = ?1,
                    ai_summary = ?1,
                    goal_achieved = ?2,
                    remaining_work = ?3,
                    summary_generated_at = ?4,
                    updated_at = ?4
                WHERE id = ?5
                "#,
                params![summary_text, goal_achieved as i32, remaining_work, now, id],
            )
            .map_err(|e| format!("Failed to update task summary: {}", e))?;

        if rows == 0 {
            return Err(format!("Task run not found: {}", id));
        }

        info!("Updated summary for task run {}", id);
        Ok(())
    }

    /// Clear summary fields for a task run (used when reopening/continuing a run).
    pub fn clear_task_summary(&self, id: &str) -> Result<(), String> {
        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();

        conn.execute(
            r#"
            UPDATE task_runs SET
                summary = NULL,
                ai_summary = NULL,
                goal_achieved = NULL,
                remaining_work = NULL,
                summary_generated_at = NULL,
                updated_at = ?1
            WHERE id = ?2
            "#,
            params![now, id],
        )
        .map_err(|e| format!("Failed to clear task summary: {}", e))?;

        Ok(())
    }

    /// Reopen a finished task run to add more iterations.
    /// Changes status back to "running", increments max_sessions, clears summary.
    pub fn reopen_task_run(&self, id: &str, additional_sessions: u32) -> Result<TaskRun, String> {
        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();

        // First, get the current task run to verify it exists and is finished
        drop(conn); // Release connection before calling get_task_run
        let task_run = self
            .get_task_run(id)?
            .ok_or_else(|| format!("Task run not found: {}", id))?;

        // Verify the task is in a finished state
        if task_run.status == "running" {
            return Err("Task run is already running".to_string());
        }

        // Calculate new max_sessions
        let current_max = task_run.max_sessions.unwrap_or(task_run.sessions_count);
        let new_max = current_max + additional_sessions;

        // Reopen the task run
        let conn = self.get_conn()?;
        conn.execute(
            r#"
            UPDATE task_runs SET
                status = 'running',
                max_sessions = ?1,
                auto_continue = 1,
                ai_summary = NULL,
                goal_achieved = NULL,
                remaining_work = NULL,
                summary_generated_at = NULL,
                completed_at = NULL,
                updated_at = ?2
            WHERE id = ?3
            "#,
            params![new_max as i64, now, id],
        )
        .map_err(|e| format!("Failed to reopen task run: {}", e))?;

        info!(
            "Reopened task run {} with {} additional sessions (new max: {})",
            id, additional_sessions, new_max
        );

        // Return the updated task run
        drop(conn);
        self.get_task_run(id)?
            .ok_or_else(|| "Failed to retrieve updated task run".to_string())
    }

    // ========================================================================
    // Task Run Automation Operations (child records for automation metrics)
    // ========================================================================

    /// Create a new task run automation record.
    ///
    /// This creates a child record linked to a parent task_run.
    /// Use this when starting automation as part of a task.
    pub fn create_task_run_automation(
        &self,
        task_run_id: &str,
        workflow_name: Option<&str>,
        iteration_number: u32,
    ) -> Result<String, String> {
        let conn = self.get_conn()?;
        let id = uuid::Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();

        conn.execute(
            r#"
            INSERT INTO task_run_automation (id, task_run_id, workflow_name, started_at, automation_status, iteration_number)
            VALUES (?1, ?2, ?3, ?4, 'running', ?5)
            "#,
            params![id, task_run_id, workflow_name, now, iteration_number as i64],
        )
        .map_err(|e| format!("Failed to create task run automation: {}", e))?;

        Ok(id)
    }

    /// Complete a task run automation record with success.
    pub fn complete_task_run_automation(
        &self,
        id: &str,
        actions_summary: Option<&str>,
        states_visited: Option<&str>,
        transitions_executed: Option<&str>,
        template_matches: Option<&str>,
        anomalies: Option<&str>,
    ) -> Result<(), String> {
        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();

        // Get start time to calculate duration
        let started_at: String = conn
            .query_row(
                "SELECT started_at FROM task_run_automation WHERE id = ?",
                params![id],
                |row| row.get(0),
            )
            .map_err(|e| format!("Failed to get automation start time: {}", e))?;

        // Calculate duration
        let duration_ms = if let Ok(start) = chrono::DateTime::parse_from_rfc3339(&started_at) {
            let end = Utc::now();
            (end.signed_duration_since(start.with_timezone(&Utc))).num_milliseconds()
        } else {
            0
        };

        conn.execute(
            r#"
            UPDATE task_run_automation SET
                automation_status = 'success',
                success = 1,
                ended_at = ?1,
                duration_ms = ?2,
                actions_summary = ?3,
                states_visited = ?4,
                transitions_executed = ?5,
                template_matches = ?6,
                anomalies = ?7
            WHERE id = ?8
            "#,
            params![
                now,
                duration_ms,
                actions_summary,
                states_visited,
                transitions_executed,
                template_matches,
                anomalies,
                id
            ],
        )
        .map_err(|e| format!("Failed to complete task run automation: {}", e))?;

        Ok(())
    }

    /// Fail a task run automation record.
    pub fn fail_task_run_automation(
        &self,
        id: &str,
        error_type: Option<&str>,
        error_message: &str,
        actions_summary: Option<&str>,
        states_visited: Option<&str>,
        transitions_executed: Option<&str>,
        template_matches: Option<&str>,
        anomalies: Option<&str>,
    ) -> Result<(), String> {
        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();

        // Get start time to calculate duration
        let started_at: String = conn
            .query_row(
                "SELECT started_at FROM task_run_automation WHERE id = ?",
                params![id],
                |row| row.get(0),
            )
            .map_err(|e| format!("Failed to get automation start time: {}", e))?;

        // Calculate duration
        let duration_ms = if let Ok(start) = chrono::DateTime::parse_from_rfc3339(&started_at) {
            let end = Utc::now();
            (end.signed_duration_since(start.with_timezone(&Utc))).num_milliseconds()
        } else {
            0
        };

        conn.execute(
            r#"
            UPDATE task_run_automation SET
                automation_status = 'failed',
                success = 0,
                ended_at = ?1,
                duration_ms = ?2,
                error_type = ?3,
                error_message = ?4,
                actions_summary = ?5,
                states_visited = ?6,
                transitions_executed = ?7,
                template_matches = ?8,
                anomalies = ?9
            WHERE id = ?10
            "#,
            params![
                now,
                duration_ms,
                error_type,
                error_message,
                actions_summary,
                states_visited,
                transitions_executed,
                template_matches,
                anomalies,
                id
            ],
        )
        .map_err(|e| format!("Failed to fail task run automation: {}", e))?;

        Ok(())
    }

    /// Get automation records for a task run.
    pub fn get_task_run_automations(
        &self,
        task_run_id: &str,
    ) -> Result<Vec<TaskRunAutomation>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, task_run_id, workflow_name, started_at, ended_at, duration_ms,
                       automation_status, success, error_type, error_message,
                       actions_summary, states_visited, transitions_executed,
                       template_matches, anomalies, iteration_number
                FROM task_run_automation
                WHERE task_run_id = ?
                ORDER BY iteration_number ASC
                "#,
            )
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        let automations = stmt
            .query_map(params![task_run_id], |row| {
                Ok(TaskRunAutomation {
                    id: row.get(0)?,
                    task_run_id: row.get(1)?,
                    workflow_name: row.get(2)?,
                    started_at: row.get(3)?,
                    ended_at: row.get(4)?,
                    duration_ms: row.get(5)?,
                    automation_status: row.get(6)?,
                    success: row.get::<_, Option<i32>>(7)?.map(|v| v != 0),
                    error_type: row.get(8)?,
                    error_message: row.get(9)?,
                    actions_summary: row.get(10)?,
                    states_visited: row.get(11)?,
                    transitions_executed: row.get(12)?,
                    template_matches: row.get(13)?,
                    anomalies: row.get(14)?,
                    iteration_number: row.get::<_, i64>(15)? as u32,
                })
            })
            .map_err(|e| format!("Failed to execute query: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(automations)
    }

    /// Get a single automation record by its own ID.
    pub fn get_task_run_automation_by_id(
        &self,
        automation_id: &str,
    ) -> Result<Option<TaskRunAutomation>, String> {
        let conn = self.get_conn()?;

        let result = conn.query_row(
            r#"
            SELECT id, task_run_id, workflow_name, started_at, ended_at, duration_ms,
                   automation_status, success, error_type, error_message,
                   actions_summary, states_visited, transitions_executed,
                   template_matches, anomalies, iteration_number
            FROM task_run_automation
            WHERE id = ?
            "#,
            params![automation_id],
            |row| {
                Ok(TaskRunAutomation {
                    id: row.get(0)?,
                    task_run_id: row.get(1)?,
                    workflow_name: row.get(2)?,
                    started_at: row.get(3)?,
                    ended_at: row.get(4)?,
                    duration_ms: row.get(5)?,
                    automation_status: row.get(6)?,
                    success: row.get::<_, Option<i32>>(7)?.map(|v| v != 0),
                    error_type: row.get(8)?,
                    error_message: row.get(9)?,
                    actions_summary: row.get(10)?,
                    states_visited: row.get(11)?,
                    transitions_executed: row.get(12)?,
                    template_matches: row.get(13)?,
                    anomalies: row.get(14)?,
                    iteration_number: row.get::<_, i64>(15)? as u32,
                })
            },
        );

        match result {
            Ok(automation) => Ok(Some(automation)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("Failed to get automation record: {}", e)),
        }
    }

    // ========================================================================
    // Settings Operations
    // ========================================================================

    /// Get a setting value.
    pub fn get_setting(&self, key: &str) -> Result<Option<serde_json::Value>, String> {
        let conn = self.get_conn()?;

        let result: SqliteResult<String> = conn.query_row(
            "SELECT value FROM settings WHERE key = ?1",
            params![key],
            |row| row.get(0),
        );

        match result {
            Ok(value) => {
                let parsed = serde_json::from_str(&value)
                    .map_err(|e| format!("Failed to parse setting value: {}", e))?;
                Ok(Some(parsed))
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("Failed to get setting: {}", e)),
        }
    }

    /// Set a setting value.
    pub fn set_setting(&self, key: &str, value: &serde_json::Value) -> Result<(), String> {
        let conn = self.get_conn()?;

        let now = Utc::now().to_rfc3339();
        let value_str = value.to_string();

        conn.execute(
            r#"
            INSERT INTO settings (key, value, updated_at)
            VALUES (?1, ?2, ?3)
            ON CONFLICT(key) DO UPDATE SET value = ?2, updated_at = ?3
            "#,
            params![key, value_str, now],
        )
        .map_err(|e| format!("Failed to set setting: {}", e))?;

        Ok(())
    }

    /// Get all settings as a JSON object.
    pub fn get_all_settings(&self) -> Result<serde_json::Value, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare("SELECT key, value FROM settings")
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        let mut settings = serde_json::Map::new();

        let rows = stmt
            .query_map([], |row| {
                let key: String = row.get(0)?;
                let value: String = row.get(1)?;
                Ok((key, value))
            })
            .map_err(|e| format!("Failed to execute query: {}", e))?;

        for row in rows.flatten() {
            if let Ok(value) = serde_json::from_str(&row.1) {
                settings.insert(row.0, value);
            }
        }

        Ok(serde_json::Value::Object(settings))
    }

    // ========================================================================
    // Config Storage Operations
    // ========================================================================

    /// Save a config with a specific ID.
    /// Used when importing from web where we have a project_id.
    pub fn save_config_with_id(
        &self,
        id: &str,
        config: serde_json::Value,
        name: &str,
        source_type: &str,
        source_path: Option<&str>,
    ) -> Result<(), String> {
        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();
        let config_json = config.to_string();

        conn.execute(
            r#"
            INSERT INTO configs (id, name, config_json, source_type, source_path, created_at, updated_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)
            ON CONFLICT(id) DO UPDATE SET
                name = ?2,
                config_json = ?3,
                source_type = ?4,
                source_path = ?5,
                updated_at = ?6
            "#,
            params![id, name, config_json, source_type, source_path, now],
        )
        .map_err(|e| format!("Failed to save config: {}", e))?;

        info!("Saved config '{}' with id '{}'", name, id);
        Ok(())
    }

    /// Upsert a config (update if exists, insert if new).
    /// Used when loading from file.
    #[allow(dead_code)]
    pub fn upsert_config(
        &self,
        id: &str,
        config: serde_json::Value,
        name: &str,
        source_path: Option<&str>,
    ) -> Result<(), String> {
        self.save_config_with_id(id, config, name, "file", source_path)
    }

    /// Get a config by ID.
    #[allow(dead_code)]
    pub fn get_config(&self, id: &str) -> Result<Option<serde_json::Value>, String> {
        let conn = self.get_conn()?;

        let result: SqliteResult<String> = conn.query_row(
            "SELECT config_json FROM configs WHERE id = ?1",
            params![id],
            |row| row.get(0),
        );

        match result {
            Ok(json_str) => {
                let config = serde_json::from_str(&json_str)
                    .map_err(|e| format!("Failed to parse config JSON: {}", e))?;
                Ok(Some(config))
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("Failed to get config: {}", e)),
        }
    }

    /// List all stored configs.
    #[allow(dead_code)]
    pub fn list_configs(&self) -> Result<Vec<ConfigStorageEntry>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, name, source_type, source_path, created_at, updated_at
                FROM configs
                ORDER BY updated_at DESC
                "#,
            )
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        let configs = stmt
            .query_map([], |row| {
                Ok(ConfigStorageEntry {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    source_type: row.get(2)?,
                    source_path: row.get(3)?,
                    created_at: row.get(4)?,
                    updated_at: row.get(5)?,
                })
            })
            .map_err(|e| format!("Failed to execute query: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(configs)
    }

    /// Delete a config by ID.
    #[allow(dead_code)]
    pub fn delete_config(&self, id: &str) -> Result<bool, String> {
        let conn = self.get_conn()?;

        let rows = conn
            .execute("DELETE FROM configs WHERE id = ?1", params![id])
            .map_err(|e| format!("Failed to delete config: {}", e))?;

        Ok(rows > 0)
    }

    // ========================================================================
    // Migration from JSON files
    // ========================================================================

    /// Migrate existing JSON files to the database.
    /// Called on first startup after database creation.
    pub fn migrate_from_json_files(&self) -> Result<MigrationResult, String> {
        let mut result = MigrationResult::default();

        // Get paths
        let config_dir = dirs::config_dir()
            .ok_or("Failed to get config directory")?
            .join("com.qontinui.runner");

        let data_dir = dirs::data_local_dir()
            .ok_or("Failed to get data directory")?
            .join("com.qontinui.runner");

        // Migrate settings.json
        let settings_path = config_dir.join("settings.json");
        if settings_path.exists() {
            match self.migrate_settings_file(&settings_path) {
                Ok(count) => {
                    result.settings_migrated = count;
                    info!("Migrated {} settings from {:?}", count, settings_path);
                }
                Err(e) => {
                    warn!("Failed to migrate settings: {}", e);
                    result.errors.push(format!("Settings: {}", e));
                }
            }
        }

        // Migrate prompts.json
        let prompts_path = data_dir.join("prompts.json");
        if prompts_path.exists() {
            match self.migrate_prompts_file(&prompts_path) {
                Ok(count) => {
                    result.prompts_migrated = count;
                    info!("Migrated {} prompts from {:?}", count, prompts_path);
                }
                Err(e) => {
                    warn!("Failed to migrate prompts: {}", e);
                    result.errors.push(format!("Prompts: {}", e));
                }
            }
        }

        // Migrate scheduler.json
        let scheduler_path = config_dir.join("scheduler.json");
        if scheduler_path.exists() {
            match self.migrate_scheduler_file(&scheduler_path) {
                Ok(count) => {
                    result.scheduler_tasks_migrated = count;
                    info!(
                        "Migrated {} scheduler tasks from {:?}",
                        count, scheduler_path
                    );
                }
                Err(e) => {
                    warn!("Failed to migrate scheduler: {}", e);
                    result.errors.push(format!("Scheduler: {}", e));
                }
            }
        }

        Ok(result)
    }

    fn migrate_settings_file(&self, path: &PathBuf) -> Result<usize, String> {
        let content =
            std::fs::read_to_string(path).map_err(|e| format!("Failed to read file: {}", e))?;

        let settings: serde_json::Value =
            serde_json::from_str(&content).map_err(|e| format!("Failed to parse JSON: {}", e))?;

        let obj = settings
            .as_object()
            .ok_or("Settings file is not a JSON object")?;

        let mut count = 0;
        for (key, value) in obj {
            self.set_setting(key, value)?;
            count += 1;
        }

        // NOTE: Do NOT rename settings.json to .bak here!
        // settings.rs still reads from settings.json directly.
        // The database is used for settings that aren't in settings.rs yet.
        // Once settings.rs is fully migrated to database, we can rename.

        Ok(count)
    }

    fn migrate_prompts_file(&self, path: &PathBuf) -> Result<usize, String> {
        let content =
            std::fs::read_to_string(path).map_err(|e| format!("Failed to read file: {}", e))?;

        let prompts: Vec<serde_json::Value> =
            serde_json::from_str(&content).map_err(|e| format!("Failed to parse JSON: {}", e))?;

        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();

        let mut count = 0;
        for prompt in prompts {
            let id = prompt
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or(&uuid::Uuid::new_v4().to_string())
                .to_string();
            let name = prompt
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("Untitled");
            let category = prompt.get("category").and_then(|v| v.as_str());
            let content_field = prompt.get("content").and_then(|v| v.as_str()).unwrap_or("");
            let variables = prompt
                .get("variables")
                .map(|v| v.to_string())
                .unwrap_or_else(|| "[]".to_string());

            conn.execute(
                r#"
                INSERT OR REPLACE INTO prompts (id, name, category, content, variables, created_at, updated_at)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)
                "#,
                params![id, name, category, content_field, variables, now],
            )
            .map_err(|e| format!("Failed to insert prompt: {}", e))?;

            count += 1;
        }

        // Rename original file to .bak
        let backup_path = path.with_extension("json.bak");
        if let Err(e) = std::fs::rename(path, &backup_path) {
            warn!("Failed to rename {:?} to backup: {}", path, e);
        }

        Ok(count)
    }

    fn migrate_scheduler_file(&self, path: &PathBuf) -> Result<usize, String> {
        let content =
            std::fs::read_to_string(path).map_err(|e| format!("Failed to read file: {}", e))?;

        let scheduler: serde_json::Value =
            serde_json::from_str(&content).map_err(|e| format!("Failed to parse JSON: {}", e))?;

        let tasks = scheduler
            .get("tasks")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();

        let mut count = 0;
        for task in tasks {
            let id = task
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or(&uuid::Uuid::new_v4().to_string())
                .to_string();
            let name = task
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("Untitled");
            let description = task.get("description").and_then(|v| v.as_str());
            let enabled = task
                .get("enabled")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);

            let schedule = task
                .get("schedule")
                .cloned()
                .unwrap_or(serde_json::json!({}));
            let schedule_type = schedule
                .get("type")
                .and_then(|v| v.as_str())
                .unwrap_or("once");
            let schedule_value = schedule
                .get("value")
                .map(|v| v.to_string())
                .unwrap_or_default();

            let task_config = task.get("task").map(|v| v.to_string()).unwrap_or_default();

            conn.execute(
                r#"
                INSERT OR REPLACE INTO scheduled_tasks
                (id, name, description, enabled, schedule_type, schedule_value, task_config, created_at, modified_at)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)
                "#,
                params![
                    id,
                    name,
                    description,
                    enabled as i32,
                    schedule_type,
                    schedule_value,
                    task_config,
                    now
                ],
            )
            .map_err(|e| format!("Failed to insert scheduler task: {}", e))?;

            count += 1;
        }

        // Migrate settings
        if let Some(settings) = scheduler.get("settings") {
            let enabled = settings
                .get("enabled")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            let max_concurrent = settings
                .get("max_concurrent")
                .and_then(|v| v.as_i64())
                .unwrap_or(1);

            conn.execute(
                r#"
                UPDATE scheduler_settings SET enabled = ?1, max_concurrent = ?2 WHERE id = 1
                "#,
                params![enabled as i32, max_concurrent],
            )
            .map_err(|e| format!("Failed to update scheduler settings: {}", e))?;
        }

        // Rename original file to .bak
        let backup_path = path.with_extension("json.bak");
        if let Err(e) = std::fs::rename(path, &backup_path) {
            warn!("Failed to rename {:?} to backup: {}", path, e);
        }

        Ok(count)
    }

    // ========================================================================
    // Findings Operations (wrapper methods for findings::storage)
    // ========================================================================

    /// Get a finding by ID.
    pub fn get_finding(&self, id: &str) -> Result<Option<crate::findings::Finding>, String> {
        let conn = self.get_conn()?;
        crate::findings::storage::get_finding(&conn, id)
    }

    /// Get all findings for a task run.
    pub fn get_findings_for_task(
        &self,
        task_run_id: &str,
    ) -> Result<Vec<crate::findings::Finding>, String> {
        let conn = self.get_conn()?;
        crate::findings::storage::get_findings_for_task(&conn, task_run_id)
    }

    /// Get findings by status for a task run.
    pub fn get_findings_by_status(
        &self,
        task_run_id: &str,
        status: &crate::findings::FindingStatus,
    ) -> Result<Vec<crate::findings::Finding>, String> {
        let conn = self.get_conn()?;
        crate::findings::storage::get_findings_by_status(&conn, task_run_id, status)
    }

    /// Update finding status.
    pub fn update_finding_status(
        &self,
        id: &str,
        status: &crate::findings::FindingStatus,
        resolution: Option<&str>,
        session_num: Option<u32>,
    ) -> Result<(), String> {
        let conn = self.get_conn()?;
        crate::findings::storage::update_finding_status(&conn, id, status, resolution, session_num)
    }

    /// Set user response for a finding.
    pub fn set_finding_user_response(&self, id: &str, response: &str) -> Result<(), String> {
        let conn = self.get_conn()?;
        crate::findings::storage::set_user_response(&conn, id, response)
    }

    /// Get summary statistics for a task run.
    pub fn get_finding_summary(
        &self,
        task_run_id: &str,
    ) -> Result<crate::findings::FindingSummary, String> {
        let conn = self.get_conn()?;
        crate::findings::storage::get_finding_summary(&conn, task_run_id)
    }

    /// Format findings for inclusion in a continuation prompt.
    ///
    /// This creates a structured section showing resolved, outstanding,
    /// and needs_input findings to provide context for continuation sessions.
    pub fn format_findings_for_continuation_prompt(
        &self,
        task_run_id: &str,
    ) -> Result<String, String> {
        let conn = self.get_conn()?;
        crate::findings::storage::format_findings_for_continuation_prompt(&conn, task_run_id)
    }

    // ========================================================================
    // Verification Test Operations
    // ========================================================================

    /// Create a new verification test.
    pub fn create_verification_test(
        &self,
        input: &CreateVerificationTestInput,
    ) -> Result<VerificationTest, String> {
        let conn = self.get_conn()?;
        let id = uuid::Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();

        let tags_json = serde_json::to_string(&input.tags)
            .map_err(|e| format!("Failed to serialize tags: {}", e))?;
        let vision_config_json = input
            .vision_config
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|e| format!("Failed to serialize vision_config: {}", e))?;
        let repo_test_config_json = input
            .repo_test_config
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|e| format!("Failed to serialize repo_test_config: {}", e))?;
        let config_json = serde_json::to_string(&input.config)
            .map_err(|e| format!("Failed to serialize config: {}", e))?;
        let creation_analysis_json = input
            .creation_analysis
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|e| format!("Failed to serialize creation_analysis: {}", e))?;

        conn.execute(
            r#"
            INSERT INTO verification_tests (
                id, name, description, test_type, category,
                playwright_code, vision_config, python_code, repo_test_config,
                success_criteria, config, timeout_seconds, is_critical, enabled,
                ai_generated, ai_generation_prompt, creation_analysis, tags, source_file,
                created_at, updated_at
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5,
                ?6, ?7, ?8, ?9,
                ?10, ?11, ?12, ?13, ?14,
                ?15, ?16, ?17, ?18, ?19,
                ?20, ?20
            )
            "#,
            params![
                id,
                input.name,
                input.description,
                input.test_type.to_string(),
                input.category,
                input.playwright_code,
                vision_config_json,
                input.python_code,
                repo_test_config_json,
                input.success_criteria,
                config_json,
                input.timeout_seconds,
                input.is_critical as i32,
                input.enabled as i32,
                input.ai_generated as i32,
                input.ai_generation_prompt,
                creation_analysis_json,
                tags_json,
                input.source_file,
                now,
            ],
        )
        .map_err(|e| format!("Failed to create verification test: {}", e))?;

        self.get_verification_test(&id)?
            .ok_or_else(|| "Failed to retrieve created test".to_string())
    }

    /// Get a verification test by ID.
    pub fn get_verification_test(&self, id: &str) -> Result<Option<VerificationTest>, String> {
        let conn = self.get_conn()?;

        let result: SqliteResult<VerificationTest> = conn.query_row(
            r#"
            SELECT
                id, name, description, test_type, category,
                playwright_code, vision_config, python_code, repo_test_config,
                success_criteria, config, timeout_seconds, is_critical, enabled,
                ai_generated, ai_generation_prompt, creation_analysis, tags, source_file, last_exported_at,
                created_at, updated_at
            FROM verification_tests
            WHERE id = ?1
            "#,
            params![id],
            |row| {
                Ok(VerificationTest {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    description: row.get(2).ok(),
                    test_type: row
                        .get::<_, String>(3)?
                        .parse()
                        .unwrap_or(TestType::PythonScript),
                    category: row.get(4).ok(),
                    playwright_code: row.get(5).ok(),
                    vision_config: row
                        .get::<_, Option<String>>(6)?
                        .and_then(|s| serde_json::from_str(&s).ok()),
                    python_code: row.get(7).ok(),
                    repo_test_config: row
                        .get::<_, Option<String>>(8)?
                        .and_then(|s| serde_json::from_str(&s).ok()),
                    success_criteria: row.get(9).ok(),
                    config: row
                        .get::<_, Option<String>>(10)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or(serde_json::json!({})),
                    timeout_seconds: row.get::<_, Option<i64>>(11)?.map(|v| v as u32),
                    is_critical: row.get::<_, i32>(12)? != 0,
                    enabled: row.get::<_, i32>(13)? != 0,
                    ai_generated: row.get::<_, i32>(14)? != 0,
                    ai_generation_prompt: row.get(15).ok(),
                    creation_analysis: row
                        .get::<_, Option<String>>(16)?
                        .and_then(|s| serde_json::from_str(&s).ok()),
                    tags: row
                        .get::<_, Option<String>>(17)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    source_file: row.get(18).ok(),
                    last_exported_at: row.get(19).ok(),
                    created_at: row.get(20)?,
                    updated_at: row.get(21)?,
                })
            },
        );

        match result {
            Ok(test) => Ok(Some(test)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("Failed to get verification test: {}", e)),
        }
    }

    /// List all verification tests.
    pub fn list_verification_tests(
        &self,
        enabled_only: bool,
        test_type: Option<&TestType>,
        category: Option<&str>,
    ) -> Result<Vec<VerificationTest>, String> {
        let conn = self.get_conn()?;

        let mut sql = String::from(
            r#"
            SELECT
                id, name, description, test_type, category,
                playwright_code, vision_config, python_code, repo_test_config,
                success_criteria, config, timeout_seconds, is_critical, enabled,
                ai_generated, ai_generation_prompt, creation_analysis, tags, source_file, last_exported_at,
                created_at, updated_at
            FROM verification_tests
            WHERE 1=1
            "#,
        );

        if enabled_only {
            sql.push_str(" AND enabled = 1");
        }
        if test_type.is_some() {
            sql.push_str(" AND test_type = ?1");
        }
        if category.is_some() {
            sql.push_str(" AND category = ?2");
        }
        sql.push_str(" ORDER BY created_at DESC");

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        // Handle parameter binding based on which filters are set
        let tests: Vec<VerificationTest> = if let Some(tt) = test_type {
            if let Some(cat) = category {
                stmt.query_map(params![tt.to_string(), cat], Self::row_to_verification_test)
            } else {
                stmt.query_map(params![tt.to_string()], Self::row_to_verification_test)
            }
        } else if let Some(cat) = category {
            stmt.query_map(params![cat], Self::row_to_verification_test)
        } else {
            stmt.query_map([], Self::row_to_verification_test)
        }
        .map_err(|e| format!("Failed to query verification tests: {}", e))?
        .filter_map(|r| r.ok())
        .collect();

        Ok(tests)
    }

    /// Helper to map a row to VerificationTest.
    fn row_to_verification_test(row: &rusqlite::Row) -> SqliteResult<VerificationTest> {
        Ok(VerificationTest {
            id: row.get(0)?,
            name: row.get(1)?,
            description: row.get(2).ok(),
            test_type: row
                .get::<_, String>(3)?
                .parse()
                .unwrap_or(TestType::PythonScript),
            category: row.get(4).ok(),
            playwright_code: row.get(5).ok(),
            vision_config: row
                .get::<_, Option<String>>(6)?
                .and_then(|s| serde_json::from_str(&s).ok()),
            python_code: row.get(7).ok(),
            repo_test_config: row
                .get::<_, Option<String>>(8)?
                .and_then(|s| serde_json::from_str(&s).ok()),
            success_criteria: row.get(9).ok(),
            config: row
                .get::<_, Option<String>>(10)?
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or(serde_json::json!({})),
            timeout_seconds: row.get::<_, Option<i64>>(11)?.map(|v| v as u32),
            is_critical: row.get::<_, i32>(12)? != 0,
            enabled: row.get::<_, i32>(13)? != 0,
            ai_generated: row.get::<_, i32>(14)? != 0,
            ai_generation_prompt: row.get(15).ok(),
            creation_analysis: row
                .get::<_, Option<String>>(16)?
                .and_then(|s| serde_json::from_str(&s).ok()),
            tags: row
                .get::<_, Option<String>>(17)?
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default(),
            source_file: row.get(18).ok(),
            last_exported_at: row.get(19).ok(),
            created_at: row.get(20)?,
            updated_at: row.get(21)?,
        })
    }

    /// Update a verification test.
    pub fn update_verification_test(
        &self,
        id: &str,
        input: &CreateVerificationTestInput,
    ) -> Result<VerificationTest, String> {
        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();

        let tags_json = serde_json::to_string(&input.tags)
            .map_err(|e| format!("Failed to serialize tags: {}", e))?;
        let vision_config_json = input
            .vision_config
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|e| format!("Failed to serialize vision_config: {}", e))?;
        let repo_test_config_json = input
            .repo_test_config
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|e| format!("Failed to serialize repo_test_config: {}", e))?;
        let config_json = serde_json::to_string(&input.config)
            .map_err(|e| format!("Failed to serialize config: {}", e))?;
        let creation_analysis_json = input
            .creation_analysis
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|e| format!("Failed to serialize creation_analysis: {}", e))?;

        let rows = conn
            .execute(
                r#"
            UPDATE verification_tests SET
                name = ?2,
                description = ?3,
                test_type = ?4,
                category = ?5,
                playwright_code = ?6,
                vision_config = ?7,
                python_code = ?8,
                repo_test_config = ?9,
                success_criteria = ?10,
                config = ?11,
                timeout_seconds = ?12,
                is_critical = ?13,
                enabled = ?14,
                ai_generated = ?15,
                ai_generation_prompt = ?16,
                creation_analysis = ?17,
                tags = ?18,
                source_file = ?19,
                updated_at = ?20
            WHERE id = ?1
            "#,
                params![
                    id,
                    input.name,
                    input.description,
                    input.test_type.to_string(),
                    input.category,
                    input.playwright_code,
                    vision_config_json,
                    input.python_code,
                    repo_test_config_json,
                    input.success_criteria,
                    config_json,
                    input.timeout_seconds,
                    input.is_critical as i32,
                    input.enabled as i32,
                    input.ai_generated as i32,
                    input.ai_generation_prompt,
                    creation_analysis_json,
                    tags_json,
                    input.source_file,
                    now,
                ],
            )
            .map_err(|e| format!("Failed to update verification test: {}", e))?;

        if rows == 0 {
            return Err(format!("Verification test not found: {}", id));
        }

        self.get_verification_test(id)?
            .ok_or_else(|| "Failed to retrieve updated test".to_string())
    }

    /// Delete a verification test.
    pub fn delete_verification_test(&self, id: &str) -> Result<bool, String> {
        let conn = self.get_conn()?;

        let rows = conn
            .execute("DELETE FROM verification_tests WHERE id = ?1", params![id])
            .map_err(|e| format!("Failed to delete verification test: {}", e))?;

        Ok(rows > 0)
    }

    // ========================================================================
    // Test Result Operations
    // ========================================================================

    /// Create a new test result.
    pub fn create_test_result(&self, input: &CreateTestResultInput) -> Result<TestResult, String> {
        let conn = self.get_conn()?;
        let id = uuid::Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();

        conn.execute(
            r#"
            INSERT INTO test_results (
                id, test_id, task_run_id, status, created_at
            ) VALUES (?1, ?2, ?3, 'pending', ?4)
            "#,
            params![id, input.test_id, input.task_run_id, now],
        )
        .map_err(|e| format!("Failed to create test result: {}", e))?;

        self.get_test_result(&id)?
            .ok_or_else(|| "Failed to retrieve created test result".to_string())
    }

    /// Get a test result by ID.
    pub fn get_test_result(&self, id: &str) -> Result<Option<TestResult>, String> {
        let conn = self.get_conn()?;

        let result: SqliteResult<TestResult> = conn.query_row(
            r#"
            SELECT
                id, test_id, task_run_id, status,
                started_at, completed_at, duration_ms,
                output, error_message, structured_output,
                assertions_passed, assertions_failed,
                screenshots, visual_evidence, ai_analysis, created_at
            FROM test_results
            WHERE id = ?1
            "#,
            params![id],
            Self::row_to_test_result,
        );

        match result {
            Ok(result) => Ok(Some(result)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("Failed to get test result: {}", e)),
        }
    }

    /// Helper to map a row to TestResult.
    fn row_to_test_result(row: &rusqlite::Row) -> SqliteResult<TestResult> {
        Ok(TestResult {
            id: row.get(0)?,
            test_id: row.get(1)?,
            task_run_id: row.get(2).ok(),
            status: row
                .get::<_, String>(3)?
                .parse()
                .unwrap_or(TestResultStatus::Pending),
            started_at: row.get(4).ok(),
            completed_at: row.get(5).ok(),
            duration_ms: row.get(6).ok(),
            output: row.get(7).ok(),
            error_message: row.get(8).ok(),
            structured_output: row
                .get::<_, Option<String>>(9)?
                .and_then(|s| serde_json::from_str(&s).ok()),
            assertions_passed: row.get::<_, i64>(10)? as u32,
            assertions_failed: row.get::<_, i64>(11)? as u32,
            screenshots: row
                .get::<_, Option<String>>(12)?
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default(),
            visual_evidence: row
                .get::<_, Option<String>>(13)?
                .and_then(|s| serde_json::from_str(&s).ok()),
            ai_analysis: row.get(14).ok(),
            created_at: row.get(15)?,
        })
    }

    /// Get test results for a test.
    pub fn get_results_for_test(
        &self,
        test_id: &str,
        limit: Option<u32>,
    ) -> Result<Vec<TestResult>, String> {
        let conn = self.get_conn()?;

        let sql = format!(
            r#"
            SELECT
                id, test_id, task_run_id, status,
                started_at, completed_at, duration_ms,
                output, error_message, structured_output,
                assertions_passed, assertions_failed,
                screenshots, visual_evidence, ai_analysis, created_at
            FROM test_results
            WHERE test_id = ?1
            ORDER BY created_at DESC
            {}
            "#,
            limit.map(|l| format!("LIMIT {}", l)).unwrap_or_default()
        );

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        let results: Vec<TestResult> = stmt
            .query_map(params![test_id], Self::row_to_test_result)
            .map_err(|e| format!("Failed to query test results: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }

    /// Get test results for a task run.
    pub fn get_results_for_task_run(&self, task_run_id: &str) -> Result<Vec<TestResult>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT
                    id, test_id, task_run_id, status,
                    started_at, completed_at, duration_ms,
                    output, error_message, structured_output,
                    assertions_passed, assertions_failed,
                    screenshots, visual_evidence, ai_analysis, created_at
                FROM test_results
                WHERE task_run_id = ?1
                ORDER BY created_at DESC
                "#,
            )
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        let results: Vec<TestResult> = stmt
            .query_map(params![task_run_id], Self::row_to_test_result)
            .map_err(|e| format!("Failed to query test results: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }

    /// List test results with optional status filter.
    pub fn list_test_results(
        &self,
        status: Option<&TestResultStatus>,
        limit: u32,
    ) -> Result<Vec<TestResult>, String> {
        let conn = self.get_conn()?;

        let (sql, params): (String, Vec<Box<dyn rusqlite::ToSql>>) = match status {
            Some(s) => {
                let status_str = s.to_string();
                (
                    format!(
                        r#"
                        SELECT
                            id, test_id, task_run_id, status,
                            started_at, completed_at, duration_ms,
                            output, error_message, structured_output,
                            assertions_passed, assertions_failed,
                            screenshots, visual_evidence, ai_analysis, created_at
                        FROM test_results
                        WHERE status = ?1
                        ORDER BY created_at DESC
                        LIMIT {}
                        "#,
                        limit
                    ),
                    vec![Box::new(status_str)],
                )
            }
            None => (
                format!(
                    r#"
                    SELECT
                        id, test_id, task_run_id, status,
                        started_at, completed_at, duration_ms,
                        output, error_message, structured_output,
                        assertions_passed, assertions_failed,
                        screenshots, visual_evidence, ai_analysis, created_at
                    FROM test_results
                    ORDER BY created_at DESC
                    LIMIT {}
                    "#,
                    limit
                ),
                vec![],
            ),
        };

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        let results: Vec<TestResult> = if params.is_empty() {
            stmt.query_map([], Self::row_to_test_result)
                .map_err(|e| format!("Failed to query test results: {}", e))?
                .filter_map(|r| r.ok())
                .collect()
        } else {
            let params_refs: Vec<&dyn rusqlite::ToSql> =
                params.iter().map(|p| p.as_ref()).collect();
            stmt.query_map(params_refs.as_slice(), Self::row_to_test_result)
                .map_err(|e| format!("Failed to query test results: {}", e))?
                .filter_map(|r| r.ok())
                .collect()
        };

        Ok(results)
    }

    /// Update test result status and output.
    pub fn update_test_result(
        &self,
        id: &str,
        status: &TestResultStatus,
        output: Option<&str>,
        error_message: Option<&str>,
        structured_output: Option<&serde_json::Value>,
        assertions_passed: u32,
        assertions_failed: u32,
        screenshots: &[String],
    ) -> Result<(), String> {
        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();

        let structured_output_json = structured_output
            .map(serde_json::to_string)
            .transpose()
            .map_err(|e| format!("Failed to serialize structured_output: {}", e))?;
        let screenshots_json = serde_json::to_string(screenshots)
            .map_err(|e| format!("Failed to serialize screenshots: {}", e))?;

        // Calculate duration if completing
        let duration_sql = if matches!(
            status,
            TestResultStatus::Passed
                | TestResultStatus::Failed
                | TestResultStatus::Error
                | TestResultStatus::Timeout
                | TestResultStatus::Skipped
        ) {
            ", completed_at = ?9, duration_ms = CAST((julianday(?9) - julianday(started_at)) * 86400000 AS INTEGER)"
        } else {
            ""
        };

        let sql = format!(
            r#"
            UPDATE test_results SET
                status = ?2,
                output = COALESCE(?3, output),
                error_message = COALESCE(?4, error_message),
                structured_output = COALESCE(?5, structured_output),
                assertions_passed = ?6,
                assertions_failed = ?7,
                screenshots = ?8
                {}
            WHERE id = ?1
            "#,
            duration_sql
        );

        conn.execute(
            &sql,
            params![
                id,
                status.to_string(),
                output,
                error_message,
                structured_output_json,
                assertions_passed,
                assertions_failed,
                screenshots_json,
                now,
            ],
        )
        .map_err(|e| format!("Failed to update test result: {}", e))?;

        Ok(())
    }

    /// Mark test result as started.
    pub fn start_test_result(&self, id: &str) -> Result<(), String> {
        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();

        conn.execute(
            r#"
            UPDATE test_results SET
                status = 'running',
                started_at = ?2
            WHERE id = ?1
            "#,
            params![id, now],
        )
        .map_err(|e| format!("Failed to start test result: {}", e))?;

        Ok(())
    }

    // ========================================================================
    // Check Operations (Code Quality Checks)
    // ========================================================================

    /// List all checks with optional filters.
    pub fn list_checks(
        &self,
        enabled_only: bool,
        check_type: Option<&str>,
        tool: Option<&str>,
    ) -> Result<Vec<Check>, String> {
        let conn = self.get_conn()?;

        let mut sql = String::from(
            r#"
            SELECT
                id, name, description, check_type, tool,
                command, working_directory, config_path,
                auto_fix, fail_on_warning, timeout_seconds,
                is_critical, enabled, ai_generated, ai_generation_prompt,
                tags, created_at, updated_at
            FROM checks
            WHERE 1=1
            "#,
        );

        if enabled_only {
            sql.push_str(" AND enabled = 1");
        }
        if check_type.is_some() {
            sql.push_str(" AND check_type = ?1");
        }
        if tool.is_some() {
            if check_type.is_some() {
                sql.push_str(" AND tool = ?2");
            } else {
                sql.push_str(" AND tool = ?1");
            }
        }
        sql.push_str(" ORDER BY created_at DESC");

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        // Handle parameter binding based on which filters are set
        let checks: Vec<Check> = if let Some(ct) = check_type {
            if let Some(t) = tool {
                stmt.query_map(params![ct, t], Self::row_to_check)
            } else {
                stmt.query_map(params![ct], Self::row_to_check)
            }
        } else if let Some(t) = tool {
            stmt.query_map(params![t], Self::row_to_check)
        } else {
            stmt.query_map([], Self::row_to_check)
        }
        .map_err(|e| format!("Failed to query checks: {}", e))?
        .filter_map(|r| r.ok())
        .collect();

        Ok(checks)
    }

    /// Helper to map a row to Check.
    fn row_to_check(row: &rusqlite::Row) -> SqliteResult<Check> {
        Ok(Check {
            id: row.get(0)?,
            name: row.get(1)?,
            description: row.get(2).ok(),
            check_type: row.get(3)?,
            tool: row.get(4)?,
            command: row.get(5).ok(),
            working_directory: row.get(6).ok(),
            config_path: row.get(7).ok(),
            auto_fix: row.get::<_, i32>(8)? != 0,
            fail_on_warning: row.get::<_, i32>(9)? != 0,
            // Timeout is optional - None means disabled (no timeout)
            timeout_seconds: row.get::<_, Option<i64>>(10)?.map(|v| v as u32),
            is_critical: row.get::<_, i32>(11)? != 0,
            enabled: row.get::<_, i32>(12)? != 0,
            ai_generated: row.get::<_, i32>(13)? != 0,
            ai_generation_prompt: row.get(14).ok(),
            tags: row
                .get::<_, Option<String>>(15)?
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default(),
            created_at: row.get(16)?,
            updated_at: row.get(17)?,
        })
    }

    /// Get a check by ID.
    pub fn get_check(&self, id: &str) -> Result<Option<Check>, String> {
        let conn = self.get_conn()?;

        let result: SqliteResult<Check> = conn.query_row(
            r#"
            SELECT
                id, name, description, check_type, tool,
                command, working_directory, config_path,
                auto_fix, fail_on_warning, timeout_seconds,
                is_critical, enabled, ai_generated, ai_generation_prompt,
                tags, created_at, updated_at
            FROM checks
            WHERE id = ?1
            "#,
            params![id],
            Self::row_to_check,
        );

        match result {
            Ok(check) => Ok(Some(check)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("Failed to get check: {}", e)),
        }
    }

    /// Create a new check.
    pub fn create_check(&self, input: &CreateCheckInput) -> Result<Check, String> {
        let conn = self.get_conn()?;
        let id = uuid::Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();

        let tags_json = serde_json::to_string(&input.tags)
            .map_err(|e| format!("Failed to serialize tags: {}", e))?;

        conn.execute(
            r#"
            INSERT INTO checks (
                id, name, description, check_type, tool,
                command, working_directory, config_path,
                auto_fix, fail_on_warning, timeout_seconds,
                is_critical, enabled, ai_generated, ai_generation_prompt,
                tags, created_at, updated_at
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5,
                ?6, ?7, ?8,
                ?9, ?10, ?11,
                ?12, ?13, ?14, ?15,
                ?16, ?17, ?17
            )
            "#,
            params![
                id,
                input.name,
                input.description,
                input.check_type,
                input.tool,
                input.command,
                input.working_directory,
                input.config_path,
                input.auto_fix as i32,
                input.fail_on_warning as i32,
                input.timeout_seconds,
                input.is_critical as i32,
                input.enabled as i32,
                input.ai_generated as i32,
                input.ai_generation_prompt,
                tags_json,
                now,
            ],
        )
        .map_err(|e| format!("Failed to create check: {}", e))?;

        self.get_check(&id)?
            .ok_or_else(|| "Failed to retrieve created check".to_string())
    }

    /// Update an existing check.
    pub fn update_check(&self, id: &str, input: &UpdateCheckInput) -> Result<Check, String> {
        // First verify the check exists
        let existing = self
            .get_check(id)?
            .ok_or_else(|| format!("Check not found: {}", id))?;

        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();

        // Build the SET clause dynamically based on which fields are present
        let name = input.name.as_ref().unwrap_or(&existing.name);
        let description = input.description.clone().or(existing.description);
        let check_type = input.check_type.as_ref().unwrap_or(&existing.check_type);
        let tool = input.tool.as_ref().unwrap_or(&existing.tool);
        let command = input.command.clone().or(existing.command);
        let working_directory = input
            .working_directory
            .clone()
            .or(existing.working_directory);
        let config_path = input.config_path.clone().or(existing.config_path);
        let auto_fix = input.auto_fix.unwrap_or(existing.auto_fix);
        let fail_on_warning = input.fail_on_warning.unwrap_or(existing.fail_on_warning);
        // If input specifies a timeout (including None for disabled), use it; otherwise keep existing
        let timeout_seconds = input.timeout_seconds.or(existing.timeout_seconds);
        let is_critical = input.is_critical.unwrap_or(existing.is_critical);
        let enabled = input.enabled.unwrap_or(existing.enabled);
        let tags = input.tags.clone().unwrap_or(existing.tags);

        let tags_json =
            serde_json::to_string(&tags).map_err(|e| format!("Failed to serialize tags: {}", e))?;

        let rows = conn
            .execute(
                r#"
                UPDATE checks SET
                    name = ?2,
                    description = ?3,
                    check_type = ?4,
                    tool = ?5,
                    command = ?6,
                    working_directory = ?7,
                    config_path = ?8,
                    auto_fix = ?9,
                    fail_on_warning = ?10,
                    timeout_seconds = ?11,
                    is_critical = ?12,
                    enabled = ?13,
                    tags = ?14,
                    updated_at = ?15
                WHERE id = ?1
                "#,
                params![
                    id,
                    name,
                    description,
                    check_type,
                    tool,
                    command,
                    working_directory,
                    config_path,
                    auto_fix as i32,
                    fail_on_warning as i32,
                    timeout_seconds,
                    is_critical as i32,
                    enabled as i32,
                    tags_json,
                    now,
                ],
            )
            .map_err(|e| format!("Failed to update check: {}", e))?;

        if rows == 0 {
            return Err(format!("Check not found: {}", id));
        }

        self.get_check(id)?
            .ok_or_else(|| "Failed to retrieve updated check".to_string())
    }

    /// Delete a check by ID.
    pub fn delete_check(&self, id: &str) -> Result<bool, String> {
        let conn = self.get_conn()?;

        let rows = conn
            .execute("DELETE FROM checks WHERE id = ?1", params![id])
            .map_err(|e| format!("Failed to delete check: {}", e))?;

        Ok(rows > 0)
    }

    // ========================================================================
    // Check Group Operations
    // ========================================================================

    /// List all check groups.
    pub fn list_check_groups(&self, enabled_only: bool) -> Result<Vec<CheckGroup>, String> {
        let conn = self.get_conn()?;

        let sql = if enabled_only {
            r#"
            SELECT id, name, description, color, enabled, run_in_parallel,
                   stop_on_failure, tags, created_at, updated_at
            FROM check_groups
            WHERE enabled = 1
            ORDER BY name ASC
            "#
        } else {
            r#"
            SELECT id, name, description, color, enabled, run_in_parallel,
                   stop_on_failure, tags, created_at, updated_at
            FROM check_groups
            ORDER BY name ASC
            "#
        };

        let mut stmt = conn
            .prepare(sql)
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        let groups: Vec<CheckGroup> = stmt
            .query_map([], |row| self.row_to_check_group_without_checks(row))
            .map_err(|e| format!("Failed to query check groups: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        // Populate checks for each group
        let mut result = Vec::new();
        for mut group in groups {
            group.checks = self.get_checks_in_group(&group.id)?;
            result.push(group);
        }

        Ok(result)
    }

    /// Get a check group by ID.
    pub fn get_check_group(&self, id: &str) -> Result<Option<CheckGroup>, String> {
        let conn = self.get_conn()?;

        let result: SqliteResult<CheckGroup> = conn.query_row(
            r#"
            SELECT id, name, description, color, enabled, run_in_parallel,
                   stop_on_failure, tags, created_at, updated_at
            FROM check_groups
            WHERE id = ?1
            "#,
            params![id],
            |row| self.row_to_check_group_without_checks(row),
        );

        match result {
            Ok(mut group) => {
                group.checks = self.get_checks_in_group(id)?;
                Ok(Some(group))
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("Failed to get check group: {}", e)),
        }
    }

    /// Create a new check group.
    pub fn create_check_group(&self, input: &CreateCheckGroupInput) -> Result<CheckGroup, String> {
        let conn = self.get_conn()?;
        let id = uuid::Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();

        let tags_json = serde_json::to_string(&input.tags)
            .map_err(|e| format!("Failed to serialize tags: {}", e))?;

        conn.execute(
            r#"
            INSERT INTO check_groups (
                id, name, description, color, enabled,
                run_in_parallel, stop_on_failure, tags,
                created_at, updated_at
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9
            )
            "#,
            params![
                id,
                input.name,
                input.description,
                input.color,
                input.enabled as i32,
                input.run_in_parallel as i32,
                input.stop_on_failure as i32,
                tags_json,
                now,
            ],
        )
        .map_err(|e| format!("Failed to create check group: {}", e))?;

        // Add checks to the group
        for (index, check_id) in input.check_ids.iter().enumerate() {
            self.add_check_to_group(&id, check_id, index as i32)?;
        }

        self.get_check_group(&id)?
            .ok_or_else(|| "Failed to retrieve created check group".to_string())
    }

    /// Update an existing check group.
    pub fn update_check_group(
        &self,
        id: &str,
        input: &UpdateCheckGroupInput,
    ) -> Result<CheckGroup, String> {
        let existing = self
            .get_check_group(id)?
            .ok_or_else(|| format!("Check group not found: {}", id))?;

        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();

        let name = input.name.as_ref().unwrap_or(&existing.name);
        let description = input.description.clone().or(existing.description);
        let color = input.color.clone().or(existing.color);
        let enabled = input.enabled.unwrap_or(existing.enabled);
        let run_in_parallel = input.run_in_parallel.unwrap_or(existing.run_in_parallel);
        let stop_on_failure = input.stop_on_failure.unwrap_or(existing.stop_on_failure);
        let tags = input.tags.clone().unwrap_or(existing.tags);

        let tags_json =
            serde_json::to_string(&tags).map_err(|e| format!("Failed to serialize tags: {}", e))?;

        let rows = conn
            .execute(
                r#"
                UPDATE check_groups SET
                    name = ?2,
                    description = ?3,
                    color = ?4,
                    enabled = ?5,
                    run_in_parallel = ?6,
                    stop_on_failure = ?7,
                    tags = ?8,
                    updated_at = ?9
                WHERE id = ?1
                "#,
                params![
                    id,
                    name,
                    description,
                    color,
                    enabled as i32,
                    run_in_parallel as i32,
                    stop_on_failure as i32,
                    tags_json,
                    now,
                ],
            )
            .map_err(|e| format!("Failed to update check group: {}", e))?;

        if rows == 0 {
            return Err(format!("Check group not found: {}", id));
        }

        self.get_check_group(id)?
            .ok_or_else(|| "Failed to retrieve updated check group".to_string())
    }

    /// Delete a check group by ID.
    pub fn delete_check_group(&self, id: &str) -> Result<bool, String> {
        let conn = self.get_conn()?;

        let rows = conn
            .execute("DELETE FROM check_groups WHERE id = ?1", params![id])
            .map_err(|e| format!("Failed to delete check group: {}", e))?;

        Ok(rows > 0)
    }

    /// Add a check to a group.
    pub fn add_check_to_group(
        &self,
        group_id: &str,
        check_id: &str,
        sort_order: i32,
    ) -> Result<(), String> {
        let conn = self.get_conn()?;
        let id = uuid::Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();

        conn.execute(
            r#"
            INSERT OR REPLACE INTO check_group_members (id, group_id, check_id, sort_order, created_at)
            VALUES (?1, ?2, ?3, ?4, ?5)
            "#,
            params![id, group_id, check_id, sort_order, now],
        )
        .map_err(|e| format!("Failed to add check to group: {}", e))?;

        Ok(())
    }

    /// Remove a check from a group.
    pub fn remove_check_from_group(&self, group_id: &str, check_id: &str) -> Result<bool, String> {
        let conn = self.get_conn()?;

        let rows = conn
            .execute(
                "DELETE FROM check_group_members WHERE group_id = ?1 AND check_id = ?2",
                params![group_id, check_id],
            )
            .map_err(|e| format!("Failed to remove check from group: {}", e))?;

        Ok(rows > 0)
    }

    /// Get all checks in a group (ordered by sort_order).
    pub fn get_checks_in_group(&self, group_id: &str) -> Result<Vec<Check>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT c.id, c.name, c.description, c.check_type, c.tool,
                       c.command, c.working_directory, c.config_path,
                       c.auto_fix, c.fail_on_warning, c.timeout_seconds,
                       c.is_critical, c.enabled, c.ai_generated, c.ai_generation_prompt,
                       c.tags, c.created_at, c.updated_at
                FROM checks c
                INNER JOIN check_group_members cgm ON c.id = cgm.check_id
                WHERE cgm.group_id = ?1
                ORDER BY cgm.sort_order ASC
                "#,
            )
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        let checks: Vec<Check> = stmt
            .query_map(params![group_id], Self::row_to_check)
            .map_err(|e| format!("Failed to query checks in group: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(checks)
    }

    /// Update the checks in a group (replace all).
    pub fn set_checks_in_group(&self, group_id: &str, check_ids: &[String]) -> Result<(), String> {
        let conn = self.get_conn()?;

        // Remove all existing members
        conn.execute(
            "DELETE FROM check_group_members WHERE group_id = ?1",
            params![group_id],
        )
        .map_err(|e| format!("Failed to clear group members: {}", e))?;

        // Add new members
        for (index, check_id) in check_ids.iter().enumerate() {
            self.add_check_to_group(group_id, check_id, index as i32)?;
        }

        Ok(())
    }

    /// Repair check-group associations based on naming convention.
    ///
    /// Checks are named with format "{group_name} - {tool_name}" (e.g., "multistate - Ruff Linting").
    /// This function finds checks that match groups by this pattern and ensures they are linked.
    ///
    /// Returns the number of associations created.
    pub fn repair_check_group_associations(&self) -> Result<usize, String> {
        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();

        // Insert missing associations by matching check names that start with "group_name - "
        // Only insert if the association doesn't already exist
        let sql = r#"
            INSERT OR IGNORE INTO check_group_members (id, group_id, check_id, sort_order, created_at)
            SELECT
                lower(hex(randomblob(16))),
                cg.id,
                c.id,
                (SELECT COALESCE(MAX(sort_order), 0) + 1 FROM check_group_members WHERE group_id = cg.id),
                ?1
            FROM checks c
            JOIN check_groups cg ON c.name LIKE cg.name || ' - %'
            WHERE NOT EXISTS (
                SELECT 1 FROM check_group_members cgm
                WHERE cgm.group_id = cg.id AND cgm.check_id = c.id
            )
        "#;

        let rows = conn
            .execute(sql, params![now])
            .map_err(|e| format!("Failed to repair check-group associations: {}", e))?;

        if rows > 0 {
            tracing::info!(
                "Repaired {} check-group associations based on naming convention",
                rows
            );
        }

        Ok(rows)
    }

    /// Helper to map a row to CheckGroup (without checks populated).
    fn row_to_check_group_without_checks(&self, row: &rusqlite::Row) -> SqliteResult<CheckGroup> {
        let tags_json: String = row.get(7)?;
        let tags: Vec<String> = serde_json::from_str(&tags_json).unwrap_or_default();

        Ok(CheckGroup {
            id: row.get(0)?,
            name: row.get(1)?,
            description: row.get(2).ok(),
            color: row.get(3).ok(),
            enabled: row.get::<_, i32>(4)? != 0,
            run_in_parallel: row.get::<_, i32>(5)? != 0,
            stop_on_failure: row.get::<_, i32>(6)? != 0,
            tags,
            checks: Vec::new(), // Will be populated separately
            created_at: row.get(8)?,
            updated_at: row.get(9)?,
        })
    }

    /// Get check results for a specific check.
    pub fn get_check_results(
        &self,
        check_id: &str,
        limit: u32,
    ) -> Result<Vec<CheckResult>, String> {
        let conn = self.get_conn()?;

        let sql = format!(
            r#"
            SELECT
                id, check_id, task_run_id, status,
                started_at, completed_at, duration_ms,
                output, error_message, issues_found,
                issues_fixed, files_checked, structured_output,
                created_at
            FROM check_results
            WHERE check_id = ?1
            ORDER BY created_at DESC
            LIMIT {}
            "#,
            limit
        );

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        let results: Vec<CheckResult> = stmt
            .query_map(params![check_id], Self::row_to_check_result)
            .map_err(|e| format!("Failed to query check results: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }

    /// Helper to map a row to CheckResult.
    fn row_to_check_result(row: &rusqlite::Row) -> SqliteResult<CheckResult> {
        Ok(CheckResult {
            id: row.get(0)?,
            check_id: row.get(1)?,
            task_run_id: row.get(2).ok(),
            status: row.get(3)?,
            started_at: row.get(4).ok(),
            completed_at: row.get(5).ok(),
            duration_ms: row.get(6).ok(),
            output: row.get(7).ok(),
            error_message: row.get(8).ok(),
            issues_found: row.get::<_, i64>(9)? as i32,
            issues_fixed: row.get::<_, i64>(10)? as i32,
            files_checked: row.get::<_, i64>(11)? as i32,
            structured_output: row.get(12).ok(),
            created_at: row.get(13)?,
        })
    }

    /// Save a check execution result.
    ///
    /// Takes a CheckExecutionResult from the check_executor module and stores it in the database.
    pub fn save_check_result(
        &self,
        check_id: &str,
        status: &str,
        started_at: Option<&str>,
        completed_at: Option<&str>,
        duration_ms: Option<i64>,
        output: Option<&str>,
        error_message: Option<&str>,
        issues_found: i32,
        issues_fixed: i32,
        files_checked: i32,
        structured_output: Option<&str>,
        task_run_id: Option<&str>,
    ) -> Result<String, String> {
        let conn = self.get_conn()?;
        let id = uuid::Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();

        conn.execute(
            r#"
            INSERT INTO check_results (
                id, check_id, task_run_id, status,
                started_at, completed_at, duration_ms,
                output, error_message, issues_found,
                issues_fixed, files_checked, structured_output,
                created_at
            ) VALUES (
                ?1, ?2, ?3, ?4,
                ?5, ?6, ?7,
                ?8, ?9, ?10,
                ?11, ?12, ?13,
                ?14
            )
            "#,
            params![
                id,
                check_id,
                task_run_id,
                status,
                started_at,
                completed_at,
                duration_ms,
                output,
                error_message,
                issues_found,
                issues_fixed,
                files_checked,
                structured_output,
                now,
            ],
        )
        .map_err(|e| format!("Failed to save check result: {}", e))?;

        Ok(id)
    }

    // ========================================================================
    // Shell Command Operations
    // ========================================================================

    /// List all shell commands with optional filters.
    pub fn list_shell_commands(
        &self,
        enabled_only: bool,
        category: Option<&str>,
    ) -> Result<Vec<ShellCommand>, String> {
        let conn = self.get_conn()?;

        let mut sql = String::from(
            r#"
            SELECT
                id, name, description, command, working_directory,
                timeout_seconds, fail_on_error, category, tags,
                enabled, created_at, updated_at
            FROM shell_commands
            WHERE 1=1
            "#,
        );

        if enabled_only {
            sql.push_str(" AND enabled = 1");
        }
        if category.is_some() {
            sql.push_str(" AND category = ?1");
        }
        sql.push_str(" ORDER BY created_at DESC");

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        let shell_commands: Vec<ShellCommand> = if let Some(cat) = category {
            stmt.query_map(params![cat], Self::row_to_shell_command)
        } else {
            stmt.query_map([], Self::row_to_shell_command)
        }
        .map_err(|e| format!("Failed to query shell commands: {}", e))?
        .filter_map(|r| r.ok())
        .collect();

        Ok(shell_commands)
    }

    /// Helper to map a row to ShellCommand.
    fn row_to_shell_command(row: &rusqlite::Row) -> SqliteResult<ShellCommand> {
        Ok(ShellCommand {
            id: row.get(0)?,
            name: row.get(1)?,
            description: row.get(2).ok(),
            command: row.get(3)?,
            working_directory: row.get(4).ok(),
            timeout_seconds: row.get::<_, i64>(5)? as i32,
            fail_on_error: row.get::<_, i32>(6)? != 0,
            category: row
                .get::<_, Option<String>>(7)?
                .unwrap_or_else(|| "general".to_string()),
            tags: row
                .get::<_, Option<String>>(8)?
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default(),
            enabled: row.get::<_, i32>(9)? != 0,
            created_at: row.get(10)?,
            updated_at: row.get(11)?,
        })
    }

    /// Get a shell command by ID.
    pub fn get_shell_command(&self, id: &str) -> Result<Option<ShellCommand>, String> {
        let conn = self.get_conn()?;

        let result: SqliteResult<ShellCommand> = conn.query_row(
            r#"
            SELECT
                id, name, description, command, working_directory,
                timeout_seconds, fail_on_error, category, tags,
                enabled, created_at, updated_at
            FROM shell_commands
            WHERE id = ?1
            "#,
            params![id],
            Self::row_to_shell_command,
        );

        match result {
            Ok(cmd) => Ok(Some(cmd)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("Failed to get shell command: {}", e)),
        }
    }

    /// Create a new shell command.
    pub fn create_shell_command(
        &self,
        input: &CreateShellCommandInput,
    ) -> Result<ShellCommand, String> {
        let conn = self.get_conn()?;
        let id = uuid::Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();

        let tags_json = serde_json::to_string(&input.tags)
            .map_err(|e| format!("Failed to serialize tags: {}", e))?;

        conn.execute(
            r#"
            INSERT INTO shell_commands (
                id, name, description, command, working_directory,
                timeout_seconds, fail_on_error, category, tags,
                enabled, created_at, updated_at
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5,
                ?6, ?7, ?8, ?9,
                ?10, ?11, ?11
            )
            "#,
            params![
                id,
                input.name,
                input.description,
                input.command,
                input.working_directory,
                input.timeout_seconds,
                input.fail_on_error as i32,
                input.category,
                tags_json,
                input.enabled as i32,
                now,
            ],
        )
        .map_err(|e| format!("Failed to create shell command: {}", e))?;

        self.get_shell_command(&id)?
            .ok_or_else(|| "Failed to retrieve created shell command".to_string())
    }

    /// Update an existing shell command.
    pub fn update_shell_command(
        &self,
        id: &str,
        input: &UpdateShellCommandInput,
    ) -> Result<ShellCommand, String> {
        // First verify the shell command exists
        let existing = self
            .get_shell_command(id)?
            .ok_or_else(|| format!("Shell command not found: {}", id))?;

        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();

        // Build the SET clause based on which fields are present
        let name = input.name.as_ref().unwrap_or(&existing.name);
        let description = input.description.clone().or(existing.description);
        let command = input.command.as_ref().unwrap_or(&existing.command);
        let working_directory = input
            .working_directory
            .clone()
            .or(existing.working_directory);
        let timeout_seconds = input.timeout_seconds.unwrap_or(existing.timeout_seconds);
        let fail_on_error = input.fail_on_error.unwrap_or(existing.fail_on_error);
        let category = input.category.as_ref().unwrap_or(&existing.category);
        let tags = input.tags.clone().unwrap_or(existing.tags);
        let enabled = input.enabled.unwrap_or(existing.enabled);

        let tags_json =
            serde_json::to_string(&tags).map_err(|e| format!("Failed to serialize tags: {}", e))?;

        let rows = conn
            .execute(
                r#"
                UPDATE shell_commands SET
                    name = ?2,
                    description = ?3,
                    command = ?4,
                    working_directory = ?5,
                    timeout_seconds = ?6,
                    fail_on_error = ?7,
                    category = ?8,
                    tags = ?9,
                    enabled = ?10,
                    updated_at = ?11
                WHERE id = ?1
                "#,
                params![
                    id,
                    name,
                    description,
                    command,
                    working_directory,
                    timeout_seconds,
                    fail_on_error as i32,
                    category,
                    tags_json,
                    enabled as i32,
                    now,
                ],
            )
            .map_err(|e| format!("Failed to update shell command: {}", e))?;

        if rows == 0 {
            return Err(format!("Shell command not found: {}", id));
        }

        self.get_shell_command(id)?
            .ok_or_else(|| "Failed to retrieve updated shell command".to_string())
    }

    /// Delete a shell command by ID.
    pub fn delete_shell_command(&self, id: &str) -> Result<bool, String> {
        let conn = self.get_conn()?;

        let rows = conn
            .execute("DELETE FROM shell_commands WHERE id = ?1", params![id])
            .map_err(|e| format!("Failed to delete shell command: {}", e))?;

        Ok(rows > 0)
    }

    /// Get shell command results for a specific shell command.
    pub fn get_shell_command_results(
        &self,
        shell_command_id: &str,
        limit: u32,
    ) -> Result<Vec<ShellCommandResult>, String> {
        let conn = self.get_conn()?;

        let sql = format!(
            r#"
            SELECT
                id, shell_command_id, task_run_id, status,
                exit_code, stdout, stderr, duration_ms,
                started_at, completed_at, created_at
            FROM shell_command_results
            WHERE shell_command_id = ?1
            ORDER BY created_at DESC
            LIMIT {}
            "#,
            limit
        );

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        let results: Vec<ShellCommandResult> = stmt
            .query_map(params![shell_command_id], Self::row_to_shell_command_result)
            .map_err(|e| format!("Failed to query shell command results: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }

    /// Helper to map a row to ShellCommandResult.
    fn row_to_shell_command_result(row: &rusqlite::Row) -> SqliteResult<ShellCommandResult> {
        Ok(ShellCommandResult {
            id: row.get(0)?,
            shell_command_id: row.get(1)?,
            task_run_id: row.get(2).ok(),
            status: row.get(3)?,
            exit_code: row.get(4).ok(),
            stdout: row.get(5).ok(),
            stderr: row.get(6).ok(),
            duration_ms: row.get(7).ok(),
            started_at: row.get(8).ok(),
            completed_at: row.get(9).ok(),
            created_at: row.get(10)?,
        })
    }

    /// Save a shell command execution result.
    pub fn save_shell_command_result(
        &self,
        shell_command_id: &str,
        status: &str,
        exit_code: Option<i32>,
        stdout: Option<&str>,
        stderr: Option<&str>,
        duration_ms: Option<i64>,
        started_at: Option<&str>,
        completed_at: Option<&str>,
        task_run_id: Option<&str>,
    ) -> Result<String, String> {
        let conn = self.get_conn()?;
        let id = uuid::Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();

        conn.execute(
            r#"
            INSERT INTO shell_command_results (
                id, shell_command_id, task_run_id, status,
                exit_code, stdout, stderr, duration_ms,
                started_at, completed_at, created_at
            ) VALUES (
                ?1, ?2, ?3, ?4,
                ?5, ?6, ?7, ?8,
                ?9, ?10, ?11
            )
            "#,
            params![
                id,
                shell_command_id,
                task_run_id,
                status,
                exit_code,
                stdout,
                stderr,
                duration_ms,
                started_at,
                completed_at,
                now,
            ],
        )
        .map_err(|e| format!("Failed to save shell command result: {}", e))?;

        Ok(id)
    }

    /// Execute a shell command and save the result.
    ///
    /// Runs the shell command in a subprocess, captures stdout/stderr,
    /// and stores the execution result in the database.
    pub fn execute_shell_command(
        &self,
        id: &str,
        task_run_id: Option<&str>,
    ) -> Result<ShellCommandResult, String> {
        use std::time::Instant;

        // Get the shell command
        let cmd = self
            .get_shell_command(id)?
            .ok_or_else(|| format!("Shell command not found: {}", id))?;

        if !cmd.enabled {
            return Err(format!("Shell command '{}' is disabled", cmd.name));
        }

        let start_time = Instant::now();
        let started_at = Utc::now().to_rfc3339();

        // Build the command - use shell to execute the command string
        #[cfg(target_os = "windows")]
        let mut process = crate::process_helpers::cmd_no_window();
        #[cfg(target_os = "windows")]
        process.args(["/C", &cmd.command]);

        #[cfg(not(target_os = "windows"))]
        let mut process = crate::process_helpers::no_window("sh");
        #[cfg(not(target_os = "windows"))]
        process.args(["-c", &cmd.command]);

        // Set working directory if specified
        if let Some(ref wd) = cmd.working_directory {
            process.current_dir(wd);
        }

        // Execute with timeout
        let output = process
            .output()
            .map_err(|e| format!("Failed to execute command: {}", e))?;

        let duration_ms = start_time.elapsed().as_millis() as i64;
        let completed_at = Utc::now().to_rfc3339();

        let exit_code = output.status.code();
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        // Determine status based on exit code
        let status = if output.status.success() {
            "success"
        } else {
            "failed"
        };

        // Save the result
        let result_id = self.save_shell_command_result(
            id,
            status,
            exit_code,
            Some(&stdout),
            Some(&stderr),
            Some(duration_ms),
            Some(&started_at),
            Some(&completed_at),
            task_run_id,
        )?;

        // Return the result
        Ok(ShellCommandResult {
            id: result_id,
            shell_command_id: id.to_string(),
            task_run_id: task_run_id.map(|s| s.to_string()),
            status: status.to_string(),
            exit_code,
            stdout: Some(stdout),
            stderr: Some(stderr),
            duration_ms: Some(duration_ms),
            started_at: Some(started_at),
            completed_at: Some(completed_at),
            created_at: Utc::now().to_rfc3339(),
        })
    }

    // ========================================================================
    // Test Association Operations
    // ========================================================================

    /// Create a test association.
    pub fn create_test_association(
        &self,
        test_id: &str,
        config_id: Option<&str>,
        workflow_name: Option<&str>,
        trigger_point: &TriggerPoint,
        action_id: Option<&str>,
        execution_order: i32,
    ) -> Result<TestAssociation, String> {
        let conn = self.get_conn()?;
        let id = uuid::Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();

        conn.execute(
            r#"
            INSERT INTO test_associations (
                id, test_id, config_id, workflow_name,
                trigger_point, action_id, execution_order, enabled,
                created_at, updated_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 1, ?8, ?8)
            "#,
            params![
                id,
                test_id,
                config_id,
                workflow_name,
                trigger_point.to_string(),
                action_id,
                execution_order,
                now,
            ],
        )
        .map_err(|e| format!("Failed to create test association: {}", e))?;

        self.get_test_association(&id)?
            .ok_or_else(|| "Failed to retrieve created association".to_string())
    }

    /// Get a test association by ID.
    pub fn get_test_association(&self, id: &str) -> Result<Option<TestAssociation>, String> {
        let conn = self.get_conn()?;

        let result: SqliteResult<TestAssociation> = conn.query_row(
            r#"
            SELECT
                id, test_id, config_id, workflow_name,
                trigger_point, action_id, execution_order, enabled,
                created_at, updated_at
            FROM test_associations
            WHERE id = ?1
            "#,
            params![id],
            Self::row_to_test_association,
        );

        match result {
            Ok(assoc) => Ok(Some(assoc)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("Failed to get test association: {}", e)),
        }
    }

    /// Helper to map a row to TestAssociation.
    fn row_to_test_association(row: &rusqlite::Row) -> SqliteResult<TestAssociation> {
        Ok(TestAssociation {
            id: row.get(0)?,
            test_id: row.get(1)?,
            config_id: row.get(2).ok(),
            workflow_name: row.get(3).ok(),
            trigger_point: row
                .get::<_, String>(4)?
                .parse()
                .unwrap_or(TriggerPoint::Manual),
            action_id: row.get(5).ok(),
            execution_order: row.get(6)?,
            enabled: row.get::<_, i32>(7)? != 0,
            created_at: row.get(8)?,
            updated_at: row.get(9)?,
        })
    }

    /// Get test associations for a config.
    pub fn get_associations_for_config(
        &self,
        config_id: &str,
        trigger_point: Option<&TriggerPoint>,
    ) -> Result<Vec<TestAssociation>, String> {
        let conn = self.get_conn()?;

        let sql = if trigger_point.is_some() {
            r#"
            SELECT
                id, test_id, config_id, workflow_name,
                trigger_point, action_id, execution_order, enabled,
                created_at, updated_at
            FROM test_associations
            WHERE config_id = ?1 AND trigger_point = ?2 AND enabled = 1
            ORDER BY execution_order ASC
            "#
        } else {
            r#"
            SELECT
                id, test_id, config_id, workflow_name,
                trigger_point, action_id, execution_order, enabled,
                created_at, updated_at
            FROM test_associations
            WHERE config_id = ?1 AND enabled = 1
            ORDER BY execution_order ASC
            "#
        };

        let mut stmt = conn
            .prepare(sql)
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        let associations: Vec<TestAssociation> = if let Some(tp) = trigger_point {
            stmt.query_map(
                params![config_id, tp.to_string()],
                Self::row_to_test_association,
            )
        } else {
            stmt.query_map(params![config_id], Self::row_to_test_association)
        }
        .map_err(|e| format!("Failed to query associations: {}", e))?
        .filter_map(|r| r.ok())
        .collect();

        Ok(associations)
    }

    /// Get test associations for a test.
    pub fn get_associations_for_test(&self, test_id: &str) -> Result<Vec<TestAssociation>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT
                    id, test_id, config_id, workflow_name,
                    trigger_point, action_id, execution_order, enabled,
                    created_at, updated_at
                FROM test_associations
                WHERE test_id = ?1
                ORDER BY created_at DESC
                "#,
            )
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        let associations: Vec<TestAssociation> = stmt
            .query_map(params![test_id], Self::row_to_test_association)
            .map_err(|e| format!("Failed to query associations: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(associations)
    }

    /// Delete a test association.
    pub fn delete_test_association(&self, id: &str) -> Result<bool, String> {
        let conn = self.get_conn()?;

        let rows = conn
            .execute("DELETE FROM test_associations WHERE id = ?1", params![id])
            .map_err(|e| format!("Failed to delete test association: {}", e))?;

        Ok(rows > 0)
    }

    /// Enable or disable a test association.
    pub fn set_test_association_enabled(&self, id: &str, enabled: bool) -> Result<(), String> {
        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();

        conn.execute(
            r#"
            UPDATE test_associations SET
                enabled = ?2,
                updated_at = ?3
            WHERE id = ?1
            "#,
            params![id, enabled as i32, now],
        )
        .map_err(|e| format!("Failed to update test association: {}", e))?;

        Ok(())
    }

    // ========================================================================
    // Orchestrator Operations (Verification Plans, Task Knowledge, Results)
    // ========================================================================

    /// Create a new verification plan.
    ///
    /// This is called by the planning agent at task start and on replan requests.
    pub fn create_verification_plan(
        &self,
        task_run_id: &str,
        plan: &crate::orchestrator::VerificationPlan,
        replan_reason: Option<&str>,
        previous_version_id: Option<&str>,
    ) -> Result<StoredVerificationPlan, String> {
        let conn = self.get_conn()?;
        let id = uuid::Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();

        // Serialize the plan
        let plan_json = serde_json::to_string(plan)
            .map_err(|e| format!("Failed to serialize verification plan: {}", e))?;

        let criteria_count = plan.success_criteria.len() as i32;
        let has_ai_criteria = plan.has_ai_criteria();

        conn.execute(
            r#"
            INSERT INTO verification_plans (
                id, task_run_id, version, plan_json, goal_summary,
                criteria_count, has_ai_criteria, replan_reason,
                previous_version_id, created_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
            "#,
            params![
                id,
                task_run_id,
                plan.version,
                plan_json,
                plan.goal_summary,
                criteria_count,
                has_ai_criteria as i32,
                replan_reason,
                previous_version_id,
                now,
            ],
        )
        .map_err(|e| format!("Failed to create verification plan: {}", e))?;

        self.get_verification_plan(&id)?
            .ok_or_else(|| "Failed to retrieve created verification plan".to_string())
    }

    /// Get a verification plan by ID.
    pub fn get_verification_plan(
        &self,
        id: &str,
    ) -> Result<Option<StoredVerificationPlan>, String> {
        let conn = self.get_conn()?;

        let result: SqliteResult<StoredVerificationPlan> = conn.query_row(
            r#"
            SELECT
                id, task_run_id, version, plan_json, goal_summary,
                criteria_count, has_ai_criteria, replan_reason,
                previous_version_id, created_at
            FROM verification_plans
            WHERE id = ?1
            "#,
            params![id],
            |row| {
                Ok(StoredVerificationPlan {
                    id: row.get(0)?,
                    task_run_id: row.get(1)?,
                    version: row.get::<_, i64>(2)? as u32,
                    plan_json: row.get(3)?,
                    goal_summary: row.get(4)?,
                    criteria_count: row.get::<_, i64>(5)? as u32,
                    has_ai_criteria: row.get::<_, i32>(6)? != 0,
                    replan_reason: row.get(7).ok(),
                    previous_version_id: row.get(8).ok(),
                    created_at: row.get(9)?,
                })
            },
        );

        match result {
            Ok(plan) => Ok(Some(plan)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("Failed to get verification plan: {}", e)),
        }
    }

    /// Get the latest verification plan for a task run.
    pub fn get_latest_verification_plan(
        &self,
        task_run_id: &str,
    ) -> Result<Option<StoredVerificationPlan>, String> {
        let conn = self.get_conn()?;

        let result: SqliteResult<StoredVerificationPlan> = conn.query_row(
            r#"
            SELECT
                id, task_run_id, version, plan_json, goal_summary,
                criteria_count, has_ai_criteria, replan_reason,
                previous_version_id, created_at
            FROM verification_plans
            WHERE task_run_id = ?1
            ORDER BY version DESC
            LIMIT 1
            "#,
            params![task_run_id],
            |row| {
                Ok(StoredVerificationPlan {
                    id: row.get(0)?,
                    task_run_id: row.get(1)?,
                    version: row.get::<_, i64>(2)? as u32,
                    plan_json: row.get(3)?,
                    goal_summary: row.get(4)?,
                    criteria_count: row.get::<_, i64>(5)? as u32,
                    has_ai_criteria: row.get::<_, i32>(6)? != 0,
                    replan_reason: row.get(7).ok(),
                    previous_version_id: row.get(8).ok(),
                    created_at: row.get(9)?,
                })
            },
        );

        match result {
            Ok(plan) => Ok(Some(plan)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("Failed to get latest verification plan: {}", e)),
        }
    }

    /// List all verification plans for a task run (all versions).
    pub fn list_verification_plans(
        &self,
        task_run_id: &str,
    ) -> Result<Vec<StoredVerificationPlan>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT
                    id, task_run_id, version, plan_json, goal_summary,
                    criteria_count, has_ai_criteria, replan_reason,
                    previous_version_id, created_at
                FROM verification_plans
                WHERE task_run_id = ?1
                ORDER BY version ASC
                "#,
            )
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        let plans = stmt
            .query_map(params![task_run_id], |row| {
                Ok(StoredVerificationPlan {
                    id: row.get(0)?,
                    task_run_id: row.get(1)?,
                    version: row.get::<_, i64>(2)? as u32,
                    plan_json: row.get(3)?,
                    goal_summary: row.get(4)?,
                    criteria_count: row.get::<_, i64>(5)? as u32,
                    has_ai_criteria: row.get::<_, i32>(6)? != 0,
                    replan_reason: row.get(7).ok(),
                    previous_version_id: row.get(8).ok(),
                    created_at: row.get(9)?,
                })
            })
            .map_err(|e| format!("Failed to query verification plans: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(plans)
    }

    /// Create a task knowledge entry (finding, observation, hypothesis, etc.).
    pub fn create_task_knowledge(
        &self,
        task_run_id: &str,
        category: &str,
        agent_type: &str,
        iteration: u32,
        content: &str,
        evidence: Option<&str>,
        confidence: &str,
        related_files: &[String],
    ) -> Result<StoredTaskKnowledge, String> {
        let conn = self.get_conn()?;
        let id = uuid::Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();

        let related_files_json = serde_json::to_string(related_files)
            .map_err(|e| format!("Failed to serialize related_files: {}", e))?;

        conn.execute(
            r#"
            INSERT INTO task_knowledge (
                id, task_run_id, category, agent_type, iteration,
                content, evidence, confidence, related_files,
                is_resolved, created_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 0, ?10)
            "#,
            params![
                id,
                task_run_id,
                category,
                agent_type,
                iteration,
                content,
                evidence,
                confidence,
                related_files_json,
                now,
            ],
        )
        .map_err(|e| format!("Failed to create task knowledge: {}", e))?;

        self.get_task_knowledge(&id)?
            .ok_or_else(|| "Failed to retrieve created task knowledge".to_string())
    }

    /// Get a task knowledge entry by ID.
    pub fn get_task_knowledge(&self, id: &str) -> Result<Option<StoredTaskKnowledge>, String> {
        let conn = self.get_conn()?;

        let result: SqliteResult<StoredTaskKnowledge> = conn.query_row(
            r#"
            SELECT
                id, task_run_id, category, agent_type, iteration,
                content, evidence, confidence, related_files,
                related_criterion_id, is_resolved, resolution_notes,
                resolved_at, created_at
            FROM task_knowledge
            WHERE id = ?1
            "#,
            params![id],
            Self::row_to_task_knowledge,
        );

        match result {
            Ok(knowledge) => Ok(Some(knowledge)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("Failed to get task knowledge: {}", e)),
        }
    }

    /// List all task knowledge for a task run.
    pub fn list_task_knowledge(
        &self,
        task_run_id: &str,
        category: Option<&str>,
        unresolved_only: bool,
    ) -> Result<Vec<StoredTaskKnowledge>, String> {
        let conn = self.get_conn()?;

        let mut sql = String::from(
            r#"
            SELECT
                id, task_run_id, category, agent_type, iteration,
                content, evidence, confidence, related_files,
                related_criterion_id, is_resolved, resolution_notes,
                resolved_at, created_at
            FROM task_knowledge
            WHERE task_run_id = ?1
            "#,
        );

        if unresolved_only {
            sql.push_str(" AND is_resolved = 0");
        }
        if category.is_some() {
            sql.push_str(" AND category = ?2");
        }
        sql.push_str(" ORDER BY created_at ASC");

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        let knowledge: Vec<StoredTaskKnowledge> = if let Some(cat) = category {
            stmt.query_map(params![task_run_id, cat], Self::row_to_task_knowledge)
        } else {
            stmt.query_map(params![task_run_id], Self::row_to_task_knowledge)
        }
        .map_err(|e| format!("Failed to query task knowledge: {}", e))?
        .filter_map(|r| r.ok())
        .collect();

        Ok(knowledge)
    }

    /// Mark a task knowledge entry as resolved.
    pub fn resolve_task_knowledge(
        &self,
        id: &str,
        resolution_notes: Option<&str>,
    ) -> Result<(), String> {
        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();

        conn.execute(
            r#"
            UPDATE task_knowledge SET
                is_resolved = 1,
                resolution_notes = ?2,
                resolved_at = ?3
            WHERE id = ?1
            "#,
            params![id, resolution_notes, now],
        )
        .map_err(|e| format!("Failed to resolve task knowledge: {}", e))?;

        Ok(())
    }

    /// List reflection knowledge from previous runs of the same workflow.
    ///
    /// Joins `task_knowledge` with `task_runs` via `workflow_name` to find
    /// reflection-created entries (recurring_pattern, context) from other runs.
    /// This enables cross-run knowledge persistence.
    pub fn list_workflow_knowledge(
        &self,
        workflow_name: &str,
        exclude_task_run_id: &str,
        categories: &[&str],
        limit: u32,
    ) -> Result<Vec<StoredTaskKnowledge>, String> {
        let conn = self.get_conn()?;

        if categories.is_empty() {
            return Ok(Vec::new());
        }

        // Build placeholders for the IN clause: ?3, ?4, ...
        let placeholders: Vec<String> = (0..categories.len())
            .map(|i| format!("?{}", i + 3))
            .collect();
        let in_clause = placeholders.join(", ");

        let sql = format!(
            r#"
            SELECT
                tk.id, tk.task_run_id, tk.category, tk.agent_type, tk.iteration,
                tk.content, tk.evidence, tk.confidence, tk.related_files,
                tk.related_criterion_id, tk.is_resolved, tk.resolution_notes,
                tk.resolved_at, tk.created_at
            FROM task_knowledge tk
            INNER JOIN task_runs tr ON tk.task_run_id = tr.id
            WHERE tr.workflow_name = ?1
              AND tk.task_run_id != ?2
              AND tk.agent_type = 'reflection'
              AND tk.category IN ({})
            ORDER BY tk.created_at DESC
            LIMIT ?{}
            "#,
            in_clause,
            categories.len() + 3,
        );

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| format!("Failed to prepare workflow knowledge query: {}", e))?;

        // Build params: workflow_name, exclude_task_run_id, categories..., limit
        let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        param_values.push(Box::new(workflow_name.to_string()));
        param_values.push(Box::new(exclude_task_run_id.to_string()));
        for cat in categories {
            param_values.push(Box::new(cat.to_string()));
        }
        param_values.push(Box::new(limit));

        let refs: Vec<&dyn rusqlite::types::ToSql> =
            param_values.iter().map(|v| v.as_ref()).collect();

        let knowledge: Vec<StoredTaskKnowledge> = stmt
            .query_map(refs.as_slice(), Self::row_to_task_knowledge)
            .map_err(|e| format!("Failed to query workflow knowledge: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(knowledge)
    }

    /// Query knowledge from OTHER workflows with similar names (cross-workflow learning).
    ///
    /// Splits the given workflow name into keywords and searches for knowledge entries
    /// from task runs whose workflow_name contains any of those keywords, excluding the
    /// current task run. Returns tuples of (workflow_name, knowledge_content).
    ///
    /// This enables learning from similar workflows — e.g., if you're running "fix-login-page",
    /// you might benefit from knowledge discovered during "fix-signup-page".
    pub fn get_cross_workflow_knowledge(
        &self,
        workflow_name: &str,
        exclude_task_run_id: &str,
        limit: usize,
    ) -> Result<Vec<(String, String)>, String> {
        let conn = self.get_conn()?;

        // Extract meaningful keywords from workflow name (split on spaces, hyphens, underscores)
        let keywords: Vec<&str> = workflow_name
            .split([' ', '-', '_', '>'])
            .map(|s| s.trim())
            .filter(|s| s.len() >= 3) // Skip short words like "a", "to", etc.
            .collect();

        if keywords.is_empty() {
            return Ok(Vec::new());
        }

        // Build LIKE conditions: workflow_name LIKE '%keyword1%' OR '%keyword2%' ...
        let like_conditions: Vec<String> = keywords
            .iter()
            .enumerate()
            .map(|(i, _)| format!("tr.workflow_name LIKE ?{}", i + 3))
            .collect();
        let where_clause = like_conditions.join(" OR ");

        let sql = format!(
            r#"
            SELECT DISTINCT tr.workflow_name, tk.content
            FROM task_knowledge tk
            INNER JOIN task_runs tr ON tk.task_run_id = tr.id
            WHERE ({})
              AND tk.task_run_id != ?1
              AND tr.workflow_name != ?2
              AND tk.category IN ('recurring_pattern', 'context', 'solution', 'root_cause')
              AND tk.confidence IN ('high', 'medium')
            ORDER BY tk.created_at DESC
            LIMIT ?{}
            "#,
            where_clause,
            keywords.len() + 3,
        );

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| format!("Failed to prepare cross-workflow knowledge query: {}", e))?;

        // Build params: exclude_task_run_id, workflow_name (for exact exclusion), keyword LIKE patterns..., limit
        let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        param_values.push(Box::new(exclude_task_run_id.to_string()));
        param_values.push(Box::new(workflow_name.to_string()));
        for keyword in &keywords {
            param_values.push(Box::new(format!("%{}%", keyword)));
        }
        param_values.push(Box::new(limit as u32));

        let refs: Vec<&dyn rusqlite::types::ToSql> =
            param_values.iter().map(|v| v.as_ref()).collect();

        let results: Vec<(String, String)> = stmt
            .query_map(refs.as_slice(), |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|e| format!("Failed to query cross-workflow knowledge: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }

    /// Query project-scoped knowledge entries for a given project path.
    ///
    /// Returns knowledge entries created by project reflection workflows
    /// that analyzed runs targeting the same project directory.
    pub fn list_project_knowledge(
        &self,
        project_path: &str,
        exclude_task_run_id: &str,
        limit: usize,
    ) -> Result<Vec<(String, String)>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT tk.category, tk.content
                FROM task_knowledge tk
                WHERE tk.project_path = ?1
                  AND tk.task_run_id != ?2
                  AND tk.agent_type = 'reflection'
                  AND tk.category IN (
                      'project_environment', 'project_architecture',
                      'project_test_pattern', 'project_recurring_issue'
                  )
                  AND tk.confidence IN ('high', 'medium')
                ORDER BY tk.created_at DESC
                LIMIT ?3
                "#,
            )
            .map_err(|e| format!("Failed to prepare project knowledge query: {}", e))?;

        let results: Vec<(String, String)> = stmt
            .query_map(
                rusqlite::params![project_path, exclude_task_run_id, limit as u32],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .map_err(|e| format!("Failed to query project knowledge: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }

    /// Set the project_path on a task_knowledge entry.
    pub fn set_knowledge_project_path(
        &self,
        knowledge_id: &str,
        project_path: &str,
    ) -> Result<(), String> {
        let conn = self.get_conn()?;
        conn.execute(
            "UPDATE task_knowledge SET project_path = ?1 WHERE id = ?2",
            rusqlite::params![project_path, knowledge_id],
        )
        .map_err(|e| format!("Failed to set knowledge project_path: {}", e))?;
        Ok(())
    }

    /// Helper function to convert a row to StoredTaskKnowledge.
    fn row_to_task_knowledge(row: &rusqlite::Row) -> rusqlite::Result<StoredTaskKnowledge> {
        Ok(StoredTaskKnowledge {
            id: row.get(0)?,
            task_run_id: row.get(1)?,
            category: row.get(2)?,
            agent_type: row.get(3)?,
            iteration: row.get::<_, i64>(4)? as u32,
            content: row.get(5)?,
            evidence: row.get(6).ok(),
            confidence: row.get(7)?,
            related_files: row
                .get::<_, Option<String>>(8)?
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default(),
            related_criterion_id: row.get(9).ok(),
            is_resolved: row.get::<_, i32>(10)? != 0,
            resolution_notes: row.get(11).ok(),
            resolved_at: row.get(12).ok(),
            created_at: row.get(13)?,
        })
    }

    /// Create an orchestrator verification result.
    pub fn create_orchestrator_verification_result(
        &self,
        task_run_id: &str,
        plan_id: &str,
        iteration: u32,
        result: &crate::orchestrator::VerificationResult,
        is_critical: bool,
    ) -> Result<String, String> {
        let conn = self.get_conn()?;
        let id = uuid::Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();

        let criterion_type = match result.criterion_type {
            crate::orchestrator::CriterionType::Deterministic => "deterministic",
            crate::orchestrator::CriterionType::AiEvaluated => "ai_evaluated",
        };

        let confidence = result.confidence.map(|c| match c {
            crate::orchestrator::Confidence::High => "high",
            crate::orchestrator::Confidence::Medium => "medium",
            crate::orchestrator::Confidence::Low => "low",
        });

        let observations_json = serde_json::to_string(&result.observations)
            .map_err(|e| format!("Failed to serialize observations: {}", e))?;
        let issues_json = serde_json::to_string(&result.issues)
            .map_err(|e| format!("Failed to serialize issues: {}", e))?;
        let suggestions_json = serde_json::to_string(&result.suggestions)
            .map_err(|e| format!("Failed to serialize suggestions: {}", e))?;

        conn.execute(
            r#"
            INSERT INTO orchestrator_verification_results (
                id, task_run_id, plan_id, iteration, criterion_id,
                criterion_type, passed, is_critical, confidence,
                observations, issues, suggestions, raw_output, created_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
            "#,
            params![
                id,
                task_run_id,
                plan_id,
                iteration,
                result.criterion_id,
                criterion_type,
                result.passed as i32,
                is_critical as i32,
                confidence,
                observations_json,
                issues_json,
                suggestions_json,
                result.raw_output,
                now,
            ],
        )
        .map_err(|e| format!("Failed to create verification result: {}", e))?;

        Ok(id)
    }

    /// Get all verification results for a specific iteration.
    pub fn get_iteration_verification_results(
        &self,
        task_run_id: &str,
        iteration: u32,
    ) -> Result<Vec<StoredVerificationResult>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT
                    id, task_run_id, plan_id, iteration, criterion_id,
                    criterion_type, passed, is_critical, confidence,
                    observations, issues, suggestions, raw_output, created_at
                FROM orchestrator_verification_results
                WHERE task_run_id = ?1 AND iteration = ?2
                ORDER BY created_at ASC
                "#,
            )
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        let results = stmt
            .query_map(params![task_run_id, iteration], |row| {
                Ok(StoredVerificationResult {
                    id: row.get(0)?,
                    task_run_id: row.get(1)?,
                    plan_id: row.get(2)?,
                    iteration: row.get::<_, i64>(3)? as u32,
                    criterion_id: row.get(4)?,
                    criterion_type: row.get(5)?,
                    passed: row.get::<_, i32>(6)? != 0,
                    is_critical: row.get::<_, i32>(7)? != 0,
                    confidence: row.get(8).ok(),
                    observations: row
                        .get::<_, Option<String>>(9)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    issues: row
                        .get::<_, Option<String>>(10)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    suggestions: row
                        .get::<_, Option<String>>(11)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    raw_output: row.get(12).ok(),
                    created_at: row.get(13)?,
                })
            })
            .map_err(|e| format!("Failed to query verification results: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }

    /// Get the latest verification results for a task (most recent iteration).
    pub fn get_latest_verification_results(
        &self,
        task_run_id: &str,
    ) -> Result<Vec<StoredVerificationResult>, String> {
        let conn = self.get_conn()?;

        // First get the max iteration
        let max_iteration: Option<i64> = conn
            .query_row(
                "SELECT MAX(iteration) FROM orchestrator_verification_results WHERE task_run_id = ?1",
                params![task_run_id],
                |row| row.get(0),
            )
            .ok();

        match max_iteration {
            Some(iteration) => {
                self.get_iteration_verification_results(task_run_id, iteration as u32)
            }
            None => Ok(vec![]),
        }
    }

    // ========================================================================
    // Workflow Verification Phase Results (Step-Executor Based)
    // ========================================================================

    /// Store a verification phase result from unified workflow execution.
    ///
    /// This stores the results from `execute_verification_steps` in the step_executor,
    /// which uses the workflow's explicit `verification_steps` (tests, checks) rather
    /// than the orchestrator's AI-generated verification criteria.
    ///
    /// Uses upsert semantics: if a result already exists for (task_run_id, iteration),
    /// it will be updated with the new data while preserving the original id and created_at.
    pub fn store_verification_phase_result(
        &self,
        task_run_id: &str,
        iteration: u32,
        result: &serde_json::Value,
    ) -> Result<String, String> {
        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();

        // Extract summary fields from the result
        let all_passed = result
            .get("all_passed")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let total_steps = result
            .get("total_steps")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as i32;
        let passed_steps = result
            .get("passed_steps")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as i32;
        let failed_steps = result
            .get("failed_steps")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as i32;
        let skipped_steps = result
            .get("skipped_steps")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as i32;
        let total_duration_ms = result
            .get("total_duration_ms")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as i64;
        let critical_failure = result
            .get("critical_failure")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let result_json = serde_json::to_string(result)
            .map_err(|e| format!("Failed to serialize verification result: {}", e))?;

        // Check if a result already exists for this task_run_id and iteration
        let existing_id: Option<String> = conn
            .query_row(
                "SELECT id FROM workflow_verification_phase_results WHERE task_run_id = ?1 AND iteration = ?2",
                params![task_run_id, iteration],
                |row| row.get(0),
            )
            .ok();

        let id = if let Some(existing_id) = existing_id {
            // Update existing record, preserving id and created_at
            conn.execute(
                r#"
                UPDATE workflow_verification_phase_results
                SET all_passed = ?1, total_steps = ?2, passed_steps = ?3, failed_steps = ?4,
                    skipped_steps = ?5, total_duration_ms = ?6, critical_failure = ?7, result_json = ?8
                WHERE task_run_id = ?9 AND iteration = ?10
                "#,
                params![
                    all_passed as i32,
                    total_steps,
                    passed_steps,
                    failed_steps,
                    skipped_steps,
                    total_duration_ms,
                    critical_failure as i32,
                    result_json,
                    task_run_id,
                    iteration,
                ],
            )
            .map_err(|e| format!("Failed to update verification phase result: {}", e))?;

            info!(
                "Updated verification phase result for task {} iteration {}: all_passed={}, {}/{} steps",
                task_run_id, iteration, all_passed, passed_steps, total_steps
            );

            existing_id
        } else {
            // Insert new record
            let new_id = uuid::Uuid::new_v4().to_string();

            conn.execute(
                r#"
                INSERT INTO workflow_verification_phase_results (
                    id, task_run_id, iteration,
                    all_passed, total_steps, passed_steps, failed_steps, skipped_steps,
                    total_duration_ms, critical_failure, result_json, created_at
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
                "#,
                params![
                    new_id,
                    task_run_id,
                    iteration,
                    all_passed as i32,
                    total_steps,
                    passed_steps,
                    failed_steps,
                    skipped_steps,
                    total_duration_ms,
                    critical_failure as i32,
                    result_json,
                    now,
                ],
            )
            .map_err(|e| format!("Failed to store verification phase result: {}", e))?;

            info!(
                "Stored verification phase result for task {} iteration {}: all_passed={}, {}/{} steps",
                task_run_id, iteration, all_passed, passed_steps, total_steps
            );

            new_id
        };

        Ok(id)
    }

    /// Delete all verification phase results for a task run.
    /// Used when starting a fresh run to clear stale data from previous interrupted runs.
    pub fn delete_verification_phase_results(&self, task_run_id: &str) -> Result<(), String> {
        let conn = self.get_conn()?;

        conn.execute(
            "DELETE FROM workflow_verification_phase_results WHERE task_run_id = ?1",
            params![task_run_id],
        )
        .map_err(|e| format!("Failed to delete verification phase results: {}", e))?;

        info!(
            "Deleted verification phase results for task {}",
            task_run_id
        );
        Ok(())
    }

    /// Get verification phase results for a specific iteration.
    pub fn get_verification_phase_result(
        &self,
        task_run_id: &str,
        iteration: u32,
    ) -> Result<Option<serde_json::Value>, String> {
        let conn = self.get_conn()?;

        let result: SqliteResult<String> = conn.query_row(
            r#"
            SELECT result_json FROM workflow_verification_phase_results
            WHERE task_run_id = ?1 AND iteration = ?2
            LIMIT 1
            "#,
            params![task_run_id, iteration],
            |row| row.get(0),
        );

        match result {
            Ok(json_str) => {
                let parsed: serde_json::Value = serde_json::from_str(&json_str)
                    .map_err(|e| format!("Failed to parse verification result JSON: {}", e))?;
                Ok(Some(parsed))
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("Failed to get verification phase result: {}", e)),
        }
    }

    /// Get all verification phase results for a task run.
    /// With the unique constraint on (task_run_id, iteration), there's exactly one result per iteration.
    pub fn get_all_verification_phase_results(
        &self,
        task_run_id: &str,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT result_json FROM workflow_verification_phase_results
                WHERE task_run_id = ?1
                ORDER BY iteration ASC
                "#,
            )
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        let results: Vec<serde_json::Value> = stmt
            .query_map(params![task_run_id], |row| {
                let json_str: String = row.get(0)?;
                Ok(json_str)
            })
            .map_err(|e| format!("Failed to query verification results: {}", e))?
            .filter_map(|r| r.ok())
            .filter_map(|json_str| serde_json::from_str(&json_str).ok())
            .collect();

        Ok(results)
    }

    /// Get the first running task along with its step execution events in a single call.
    /// Returns None if no tasks are running.
    /// This is an optimized batch query that avoids two separate round-trips.
    pub fn get_running_task_step_data(
        &self,
    ) -> Result<Option<(TaskRun, Vec<TaskRunEvent>)>, String> {
        let running = self.get_running_task_runs()?;
        let task = match running.into_iter().next() {
            Some(t) => t,
            None => return Ok(None),
        };

        let events = self.get_task_run_events(&task.id, None, None)?;
        Ok(Some((task, events)))
    }

    /// Get the set of iteration numbers that have completed verification phase results.
    /// Returns only the iteration integers, not the full result payloads.
    pub fn get_completed_verification_iterations(
        &self,
        task_run_id: &str,
    ) -> Result<Vec<i64>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT iteration FROM workflow_verification_phase_results
                WHERE task_run_id = ?1
                ORDER BY iteration ASC
                "#,
            )
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        let iterations: Vec<i64> = stmt
            .query_map(params![task_run_id], |row| row.get(0))
            .map_err(|e| format!("Failed to query completed iterations: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(iterations)
    }

    // ========================================================================
    // Workflow Constraint Results
    // ========================================================================

    /// Store constraint evaluation results for a given iteration.
    ///
    /// Each `ConstraintResult` is stored as a separate row, enabling per-constraint
    /// queries. Violations are serialized as a JSON array.
    pub fn store_constraint_results(
        &self,
        task_run_id: &str,
        iteration: u32,
        results: &[crate::constraint_engine::ConstraintResult],
    ) -> Result<(), String> {
        let conn = self.get_conn()?;
        let now = chrono::Utc::now().to_rfc3339();

        // Delete any existing results for this (task_run_id, iteration) to support upsert semantics
        conn.execute(
            "DELETE FROM workflow_constraint_results WHERE task_run_id = ?1 AND iteration = ?2",
            params![task_run_id, iteration as i64],
        )
        .map_err(|e| format!("Failed to delete old constraint results: {}", e))?;

        for result in results {
            let severity_str = serde_json::to_value(result.severity)
                .ok()
                .and_then(|v| v.as_str().map(|s| s.to_string()))
                .unwrap_or_else(|| format!("{:?}", result.severity).to_lowercase());

            let violations_json = if result.violations.is_empty() {
                None
            } else {
                Some(
                    serde_json::to_string(&result.violations)
                        .map_err(|e| format!("Failed to serialize constraint violations: {}", e))?,
                )
            };

            conn.execute(
                r#"
                INSERT INTO workflow_constraint_results (
                    task_run_id, iteration, constraint_id, constraint_name,
                    passed, severity, violations_json, created_at
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                "#,
                params![
                    task_run_id,
                    iteration as i64,
                    result.constraint_id,
                    result.constraint_name,
                    result.passed as i32,
                    severity_str,
                    violations_json,
                    now,
                ],
            )
            .map_err(|e| format!("Failed to store constraint result: {}", e))?;
        }

        let failed_count = results.iter().filter(|r| !r.passed).count();
        info!(
            "Stored {} constraint results for task {} iteration {} ({} failed)",
            results.len(),
            task_run_id,
            iteration,
            failed_count
        );

        Ok(())
    }

    /// Delete all constraint results for a task run.
    /// Used when starting a fresh run to clear stale data from previous interrupted runs.
    pub fn delete_constraint_results(&self, task_run_id: &str) -> Result<(), String> {
        let conn = self.get_conn()?;

        conn.execute(
            "DELETE FROM workflow_constraint_results WHERE task_run_id = ?1",
            params![task_run_id],
        )
        .map_err(|e| format!("Failed to delete constraint results: {}", e))?;

        info!("Deleted constraint results for task {}", task_run_id);
        Ok(())
    }

    /// Get constraint results for a task run, optionally filtered by iteration.
    /// Returns results as JSON values for flexibility.
    pub fn get_constraint_results(
        &self,
        task_run_id: &str,
        iteration: Option<u32>,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self.get_conn()?;

        // Row mapper shared by both query branches
        let map_row = |row: &rusqlite::Row| -> rusqlite::Result<serde_json::Value> {
            let constraint_id: String = row.get(0)?;
            let constraint_name: String = row.get(1)?;
            let passed: i32 = row.get(2)?;
            let severity: String = row.get(3)?;
            let violations_json: Option<String> = row.get(4)?;
            let iteration: i64 = row.get(5)?;
            let created_at: String = row.get(6)?;

            let violations: serde_json::Value = violations_json
                .as_deref()
                .and_then(|s| serde_json::from_str(s).ok())
                .unwrap_or(serde_json::Value::Array(vec![]));

            Ok(serde_json::json!({
                "constraint_id": constraint_id,
                "constraint_name": constraint_name,
                "passed": passed != 0,
                "severity": severity,
                "violations": violations,
                "iteration": iteration,
                "created_at": created_at,
            }))
        };

        let rows: Vec<serde_json::Value> = if let Some(iter) = iteration {
            let mut stmt = conn
                .prepare(
                    r#"
                    SELECT constraint_id, constraint_name, passed, severity, violations_json, iteration, created_at
                    FROM workflow_constraint_results
                    WHERE task_run_id = ?1 AND iteration = ?2
                    ORDER BY id ASC
                    "#,
                )
                .map_err(|e| format!("Failed to prepare constraint results query: {}", e))?;

            let results = stmt
                .query_map(params![task_run_id, iter as i64], &map_row)
                .map_err(|e| format!("Failed to query constraint results: {}", e))?
                .filter_map(|r| r.ok())
                .collect();
            results
        } else {
            let mut stmt = conn
                .prepare(
                    r#"
                    SELECT constraint_id, constraint_name, passed, severity, violations_json, iteration, created_at
                    FROM workflow_constraint_results
                    WHERE task_run_id = ?1
                    ORDER BY iteration ASC, id ASC
                    "#,
                )
                .map_err(|e| format!("Failed to prepare constraint results query: {}", e))?;

            let results = stmt
                .query_map(params![task_run_id], &map_row)
                .map_err(|e| format!("Failed to query constraint results: {}", e))?
                .filter_map(|r| r.ok())
                .collect();
            results
        };

        Ok(rows)
    }

    // ========================================================================
    // Saved API Requests Operations
    // ========================================================================

    /// List all saved API requests
    pub fn list_saved_api_requests(
        &self,
    ) -> Result<Vec<crate::saved_api_requests::SavedApiRequest>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, name, description, category, tags, method, url, headers, body,
                       body_content_type, timeout_ms, follow_redirects, variable_extractions,
                       assertions, credential_id, created_at, updated_at
                FROM saved_api_requests
                ORDER BY updated_at DESC
                "#,
            )
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let requests = stmt
            .query_map([], |row| {
                use crate::api_request::types::HttpMethod;

                let method_str: String = row.get(5)?;
                let method = match method_str.to_uppercase().as_str() {
                    "GET" => HttpMethod::Get,
                    "POST" => HttpMethod::Post,
                    "PUT" => HttpMethod::Put,
                    "PATCH" => HttpMethod::Patch,
                    "DELETE" => HttpMethod::Delete,
                    _ => HttpMethod::Get,
                };

                Ok(crate::saved_api_requests::SavedApiRequest {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    description: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                    category: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
                    tags: row
                        .get::<_, Option<String>>(4)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    method,
                    url: row.get(6)?,
                    headers: row
                        .get::<_, Option<String>>(7)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    body: row.get(8)?,
                    body_content_type: row.get(9)?,
                    timeout_ms: row.get::<_, i64>(10)? as u64,
                    follow_redirects: row.get::<_, i32>(11)? != 0,
                    variable_extractions: row
                        .get::<_, Option<String>>(12)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    assertions: row
                        .get::<_, Option<String>>(13)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    credential_id: row.get(14)?,
                    created_at: row.get(15)?,
                    updated_at: row.get(16)?,
                })
            })
            .map_err(|e| format!("Failed to query saved API requests: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(requests)
    }

    /// Get a single saved API request by ID
    pub fn get_saved_api_request(
        &self,
        id: &str,
    ) -> Result<Option<crate::saved_api_requests::SavedApiRequest>, String> {
        let conn = self.get_conn()?;

        let result = conn.query_row(
            r#"
            SELECT id, name, description, category, tags, method, url, headers, body,
                   body_content_type, timeout_ms, follow_redirects, variable_extractions,
                   assertions, credential_id, created_at, updated_at
            FROM saved_api_requests
            WHERE id = ?1
            "#,
            params![id],
            |row| {
                use crate::api_request::types::HttpMethod;

                let method_str: String = row.get(5)?;
                let method = match method_str.to_uppercase().as_str() {
                    "GET" => HttpMethod::Get,
                    "POST" => HttpMethod::Post,
                    "PUT" => HttpMethod::Put,
                    "PATCH" => HttpMethod::Patch,
                    "DELETE" => HttpMethod::Delete,
                    _ => HttpMethod::Get,
                };

                Ok(crate::saved_api_requests::SavedApiRequest {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    description: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                    category: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
                    tags: row
                        .get::<_, Option<String>>(4)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    method,
                    url: row.get(6)?,
                    headers: row
                        .get::<_, Option<String>>(7)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    body: row.get(8)?,
                    body_content_type: row.get(9)?,
                    timeout_ms: row.get::<_, i64>(10)? as u64,
                    follow_redirects: row.get::<_, i32>(11)? != 0,
                    variable_extractions: row
                        .get::<_, Option<String>>(12)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    assertions: row
                        .get::<_, Option<String>>(13)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    credential_id: row.get(14)?,
                    created_at: row.get(15)?,
                    updated_at: row.get(16)?,
                })
            },
        );

        match result {
            Ok(request) => Ok(Some(request)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("Failed to get saved API request: {}", e)),
        }
    }

    /// Create a new saved API request
    pub fn create_saved_api_request(
        &self,
        request: &crate::saved_api_requests::CreateSavedApiRequestRequest,
    ) -> Result<crate::saved_api_requests::SavedApiRequest, String> {
        let conn = self.get_conn()?;
        let id = uuid::Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();

        let method_str = format!("{}", request.method);
        let tags_json = serde_json::to_string(&request.tags).unwrap_or_else(|_| "[]".to_string());
        let headers_json =
            serde_json::to_string(&request.headers).unwrap_or_else(|_| "{}".to_string());
        let extractions_json = serde_json::to_string(&request.variable_extractions)
            .unwrap_or_else(|_| "[]".to_string());
        let assertions_json =
            serde_json::to_string(&request.assertions).unwrap_or_else(|_| "[]".to_string());

        conn.execute(
            r#"
            INSERT INTO saved_api_requests (
                id, name, description, category, tags, method, url, headers, body,
                body_content_type, timeout_ms, follow_redirects, variable_extractions,
                assertions, credential_id, created_at, updated_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)
            "#,
            params![
                id,
                request.name,
                request.description,
                request.category,
                tags_json,
                method_str,
                request.url,
                headers_json,
                request.body,
                request.body_content_type,
                request.timeout_ms as i64,
                request.follow_redirects as i32,
                extractions_json,
                assertions_json,
                request.credential_id,
                now,
                now,
            ],
        )
        .map_err(|e| format!("Failed to create saved API request: {}", e))?;

        self.get_saved_api_request(&id)?
            .ok_or_else(|| "Failed to retrieve created request".to_string())
    }

    /// Update a saved API request
    pub fn update_saved_api_request(
        &self,
        id: &str,
        request: &crate::saved_api_requests::UpdateSavedApiRequestRequest,
    ) -> Result<crate::saved_api_requests::SavedApiRequest, String> {
        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();

        // Get current values
        let current = self
            .get_saved_api_request(id)?
            .ok_or_else(|| format!("Saved API request not found: {}", id))?;

        let name = request.name.as_ref().unwrap_or(&current.name);
        let description = request.description.as_ref().unwrap_or(&current.description);
        let category = request.category.as_ref().unwrap_or(&current.category);
        let tags = request.tags.as_ref().unwrap_or(&current.tags);
        let method = request.method.unwrap_or(current.method);
        let url = request.url.as_ref().unwrap_or(&current.url);
        let headers = request.headers.as_ref().unwrap_or(&current.headers);
        let body = request.body.as_ref().or(current.body.as_ref());
        let body_content_type = request
            .body_content_type
            .as_ref()
            .or(current.body_content_type.as_ref());
        let timeout_ms = request.timeout_ms.unwrap_or(current.timeout_ms);
        let follow_redirects = request.follow_redirects.unwrap_or(current.follow_redirects);
        let variable_extractions = request
            .variable_extractions
            .as_ref()
            .unwrap_or(&current.variable_extractions);
        let assertions = request.assertions.as_ref().unwrap_or(&current.assertions);
        let credential_id = request
            .credential_id
            .as_ref()
            .or(current.credential_id.as_ref());

        let method_str = format!("{}", method);
        let tags_json = serde_json::to_string(tags).unwrap_or_else(|_| "[]".to_string());
        let headers_json = serde_json::to_string(headers).unwrap_or_else(|_| "{}".to_string());
        let extractions_json =
            serde_json::to_string(variable_extractions).unwrap_or_else(|_| "[]".to_string());
        let assertions_json =
            serde_json::to_string(assertions).unwrap_or_else(|_| "[]".to_string());

        conn.execute(
            r#"
            UPDATE saved_api_requests SET
                name = ?1, description = ?2, category = ?3, tags = ?4, method = ?5,
                url = ?6, headers = ?7, body = ?8, body_content_type = ?9, timeout_ms = ?10,
                follow_redirects = ?11, variable_extractions = ?12, assertions = ?13,
                credential_id = ?14, updated_at = ?15
            WHERE id = ?16
            "#,
            params![
                name,
                description,
                category,
                tags_json,
                method_str,
                url,
                headers_json,
                body,
                body_content_type,
                timeout_ms as i64,
                follow_redirects as i32,
                extractions_json,
                assertions_json,
                credential_id,
                now,
                id,
            ],
        )
        .map_err(|e| format!("Failed to update saved API request: {}", e))?;

        self.get_saved_api_request(id)?
            .ok_or_else(|| "Failed to retrieve updated request".to_string())
    }

    /// Delete a saved API request
    pub fn delete_saved_api_request(&self, id: &str) -> Result<bool, String> {
        let conn = self.get_conn()?;

        let deleted = conn
            .execute("DELETE FROM saved_api_requests WHERE id = ?1", params![id])
            .map_err(|e| format!("Failed to delete saved API request: {}", e))?;

        Ok(deleted > 0)
    }

    /// Search saved API requests
    pub fn search_saved_api_requests(
        &self,
        query: &crate::saved_api_requests::SearchSavedApiRequestsQuery,
    ) -> Result<Vec<crate::saved_api_requests::SavedApiRequest>, String> {
        let conn = self.get_conn()?;

        let mut sql = String::from(
            r#"
            SELECT id, name, description, category, tags, method, url, headers, body,
                   body_content_type, timeout_ms, follow_redirects, variable_extractions,
                   assertions, credential_id, created_at, updated_at
            FROM saved_api_requests
            WHERE 1=1
            "#,
        );

        let mut params_vec: Vec<String> = vec![];

        if let Some(q) = &query.q {
            sql.push_str(" AND (name LIKE ?1 OR description LIKE ?1 OR url LIKE ?1)");
            params_vec.push(format!("%{}%", q));
        }

        if let Some(category) = &query.category {
            let idx = params_vec.len() + 1;
            sql.push_str(&format!(" AND category = ?{}", idx));
            params_vec.push(category.clone());
        }

        if let Some(tag) = &query.tag {
            let idx = params_vec.len() + 1;
            sql.push_str(&format!(" AND tags LIKE ?{}", idx));
            params_vec.push(format!("%\"{}%", tag));
        }

        sql.push_str(" ORDER BY updated_at DESC");

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let params_refs: Vec<&dyn rusqlite::ToSql> = params_vec
            .iter()
            .map(|s| s as &dyn rusqlite::ToSql)
            .collect();

        let requests = stmt
            .query_map(params_refs.as_slice(), |row| {
                use crate::api_request::types::HttpMethod;

                let method_str: String = row.get(5)?;
                let method = match method_str.to_uppercase().as_str() {
                    "GET" => HttpMethod::Get,
                    "POST" => HttpMethod::Post,
                    "PUT" => HttpMethod::Put,
                    "PATCH" => HttpMethod::Patch,
                    "DELETE" => HttpMethod::Delete,
                    _ => HttpMethod::Get,
                };

                Ok(crate::saved_api_requests::SavedApiRequest {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    description: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                    category: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
                    tags: row
                        .get::<_, Option<String>>(4)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    method,
                    url: row.get(6)?,
                    headers: row
                        .get::<_, Option<String>>(7)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    body: row.get(8)?,
                    body_content_type: row.get(9)?,
                    timeout_ms: row.get::<_, i64>(10)? as u64,
                    follow_redirects: row.get::<_, i32>(11)? != 0,
                    variable_extractions: row
                        .get::<_, Option<String>>(12)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    assertions: row
                        .get::<_, Option<String>>(13)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    credential_id: row.get(14)?,
                    created_at: row.get(15)?,
                    updated_at: row.get(16)?,
                })
            })
            .map_err(|e| format!("Failed to search saved API requests: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(requests)
    }

    /// Get all unique categories from saved API requests
    pub fn get_saved_api_request_categories(&self) -> Result<Vec<String>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare("SELECT DISTINCT category FROM saved_api_requests WHERE category != '' ORDER BY category")
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let categories = stmt
            .query_map([], |row| row.get(0))
            .map_err(|e| format!("Failed to query categories: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(categories)
    }

    /// Get all unique tags from saved API requests
    pub fn get_saved_api_request_tags(&self) -> Result<Vec<String>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare("SELECT tags FROM saved_api_requests WHERE tags != '[]'")
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let mut all_tags: std::collections::HashSet<String> = std::collections::HashSet::new();

        let tags_strings: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|e| format!("Failed to query tags: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        for tags_json in tags_strings {
            if let Ok(tags) = serde_json::from_str::<Vec<String>>(&tags_json) {
                for tag in tags {
                    all_tags.insert(tag);
                }
            }
        }

        let mut result: Vec<String> = all_tags.into_iter().collect();
        result.sort();
        Ok(result)
    }

    /// Duplicate a saved API request
    pub fn duplicate_saved_api_request(
        &self,
        id: &str,
    ) -> Result<crate::saved_api_requests::SavedApiRequest, String> {
        let original = self
            .get_saved_api_request(id)?
            .ok_or_else(|| format!("Saved API request not found: {}", id))?;

        let create_request = crate::saved_api_requests::CreateSavedApiRequestRequest {
            name: format!("{} (Copy)", original.name),
            description: original.description,
            category: original.category,
            tags: original.tags,
            method: original.method,
            url: original.url,
            headers: original.headers,
            body: original.body,
            body_content_type: original.body_content_type,
            timeout_ms: original.timeout_ms,
            follow_redirects: original.follow_redirects,
            variable_extractions: original.variable_extractions,
            assertions: original.assertions,
            credential_id: original.credential_id,
        };

        self.create_saved_api_request(&create_request)
    }

    // ========================================================================
    // Unified Workflows Operations
    // ========================================================================

    /// List all unified workflows
    pub fn list_unified_workflows(
        &self,
    ) -> Result<Vec<crate::unified_workflows::UnifiedWorkflow>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, name, description, category, tags, setup_steps, verification_steps,
                       agentic_steps, completion_steps, max_iterations, provider, model,
                       skip_ai_summary, created_at, updated_at, log_source_selection,
                       context_ids, disabled_context_ids, auto_include_contexts, prompt_template,
                       log_watch_enabled, health_check_enabled, health_check_urls, timeout_seconds,
                       preflight_check_enabled, generated_by_task_run_id, enable_sweep, max_sweep_iterations,
                       stages, stop_on_failure, reflection_mode, model_overrides, approval_gate,
                       completion_prompts_first, is_favorite, dependency_graph, cost_annotations,
                       quality_report, constraint_overrides
                FROM unified_workflows
                ORDER BY is_favorite DESC, updated_at DESC
                "#,
            )
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let workflows = stmt
            .query_map([], |row| {
                Ok(crate::unified_workflows::UnifiedWorkflow {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    description: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                    category: row
                        .get::<_, Option<String>>(3)?
                        .unwrap_or_else(|| "general".to_string()),
                    tags: row
                        .get::<_, Option<String>>(4)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    setup_steps: row
                        .get::<_, Option<String>>(5)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    verification_steps: row
                        .get::<_, Option<String>>(6)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    agentic_steps: row
                        .get::<_, Option<String>>(7)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    completion_steps: row
                        .get::<_, Option<String>>(8)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    max_iterations: row.get::<_, i64>(9)? as u32,
                    provider: row.get(10)?,
                    model: row.get(11)?,
                    skip_ai_summary: row.get::<_, i32>(12)? != 0,
                    created_at: row.get(13)?,
                    updated_at: row.get(14)?,
                    log_source_selection: row
                        .get::<_, Option<String>>(15)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    context_ids: row
                        .get::<_, Option<String>>(16)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    disabled_context_ids: row
                        .get::<_, Option<String>>(17)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    auto_include_contexts: row.get::<_, Option<i32>>(18)?.unwrap_or(1) != 0,
                    prompt_template: row.get(19)?,
                    log_watch_enabled: row.get::<_, Option<i32>>(20)?.unwrap_or(1) != 0,
                    health_check_enabled: row.get::<_, Option<i32>>(21)?.unwrap_or(1) != 0,
                    health_check_urls: row
                        .get::<_, Option<String>>(22)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    timeout_seconds: row.get::<_, Option<i64>>(23)?.map(|v| v as u64),
                    preflight_check_enabled: row.get::<_, Option<i32>>(24)?.unwrap_or(1) != 0,
                    generated_by_task_run_id: row.get(25)?,
                    enable_sweep: row.get::<_, Option<i32>>(26)?.unwrap_or(0) != 0,
                    max_sweep_iterations: row.get::<_, Option<i32>>(27)?.unwrap_or(5) as u32,
                    stages: row
                        .get::<_, Option<String>>(28)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    stop_on_failure: row.get::<_, Option<i32>>(29)?.unwrap_or(0) != 0,
                    reflection_mode: row.get::<_, Option<i32>>(30)?.unwrap_or(1) != 0,
                    model_overrides: {
                        let json_str: String = row
                            .get::<_, Option<String>>(31)?
                            .unwrap_or_else(|| "{}".to_string());
                        serde_json::from_str(&json_str).unwrap_or_default()
                    },
                    approval_gate: row.get::<_, Option<i32>>(32)?.unwrap_or(0) != 0,
                    completion_prompts_first: row.get::<_, Option<i32>>(33)?.unwrap_or(0) != 0,
                    is_favorite: row.get::<_, Option<i32>>(34)?.unwrap_or(0) != 0,
                    dependency_graph: row
                        .get::<_, Option<String>>(35)?
                        .and_then(|s| serde_json::from_str(&s).ok()),
                    cost_annotations: row
                        .get::<_, Option<String>>(36)?
                        .and_then(|s| serde_json::from_str(&s).ok()),
                    quality_report: row
                        .get::<_, Option<String>>(37)?
                        .and_then(|s| serde_json::from_str(&s).ok()),
                    constraint_overrides: row
                        .get::<_, Option<String>>(38)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    // targeted_error_ids is a runtime field, not stored in DB
                    targeted_error_ids: vec![],
                })
            })
            .map_err(|e| format!("Failed to query unified workflows: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(workflows)
    }

    /// Get a single unified workflow by ID
    pub fn get_unified_workflow(
        &self,
        id: &str,
    ) -> Result<Option<crate::unified_workflows::UnifiedWorkflow>, String> {
        let conn = self.get_conn()?;

        let result = conn.query_row(
            r#"
            SELECT id, name, description, category, tags, setup_steps, verification_steps,
                   agentic_steps, completion_steps, max_iterations, provider, model,
                   skip_ai_summary, created_at, updated_at, log_source_selection,
                   context_ids, disabled_context_ids, auto_include_contexts, prompt_template,
                   log_watch_enabled, health_check_enabled, health_check_urls, timeout_seconds,
                   preflight_check_enabled, generated_by_task_run_id, enable_sweep, max_sweep_iterations,
                   stages, stop_on_failure, reflection_mode, model_overrides, approval_gate,
                   completion_prompts_first, is_favorite, dependency_graph, cost_annotations,
                   quality_report, constraint_overrides
            FROM unified_workflows
            WHERE id = ?1
            "#,
            params![id],
            |row| {
                Ok(crate::unified_workflows::UnifiedWorkflow {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    description: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                    category: row
                        .get::<_, Option<String>>(3)?
                        .unwrap_or_else(|| "general".to_string()),
                    tags: row
                        .get::<_, Option<String>>(4)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    setup_steps: row
                        .get::<_, Option<String>>(5)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    verification_steps: row
                        .get::<_, Option<String>>(6)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    agentic_steps: row
                        .get::<_, Option<String>>(7)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    completion_steps: row
                        .get::<_, Option<String>>(8)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    max_iterations: row.get::<_, i64>(9)? as u32,
                    provider: row.get(10)?,
                    model: row.get(11)?,
                    skip_ai_summary: row.get::<_, i32>(12)? != 0,
                    created_at: row.get(13)?,
                    updated_at: row.get(14)?,
                    log_source_selection: row
                        .get::<_, Option<String>>(15)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    context_ids: row
                        .get::<_, Option<String>>(16)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    disabled_context_ids: row
                        .get::<_, Option<String>>(17)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    auto_include_contexts: row.get::<_, Option<i32>>(18)?.unwrap_or(1) != 0,
                    prompt_template: row.get(19)?,
                    log_watch_enabled: row.get::<_, Option<i32>>(20)?.unwrap_or(1) != 0,
                    health_check_enabled: row.get::<_, Option<i32>>(21)?.unwrap_or(1) != 0,
                    health_check_urls: row
                        .get::<_, Option<String>>(22)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    timeout_seconds: row.get::<_, Option<i64>>(23)?.map(|v| v as u64),
                    preflight_check_enabled: row.get::<_, Option<i32>>(24)?.unwrap_or(1) != 0,
                    generated_by_task_run_id: row.get(25)?,
                    enable_sweep: row.get::<_, Option<i32>>(26)?.unwrap_or(0) != 0,
                    max_sweep_iterations: row.get::<_, Option<i32>>(27)?.unwrap_or(5) as u32,
                    stages: row
                        .get::<_, Option<String>>(28)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    stop_on_failure: row.get::<_, Option<i32>>(29)?.unwrap_or(0) != 0,
                    reflection_mode: row.get::<_, Option<i32>>(30)?.unwrap_or(1) != 0,
                    model_overrides: {
                        let json_str: String = row.get::<_, Option<String>>(31)?.unwrap_or_else(|| "{}".to_string());
                        serde_json::from_str(&json_str).unwrap_or_default()
                    },
                    approval_gate: row.get::<_, Option<i32>>(32)?.unwrap_or(0) != 0,
                    completion_prompts_first: row.get::<_, Option<i32>>(33)?.unwrap_or(0) != 0,
                    is_favorite: row.get::<_, Option<i32>>(34)?.unwrap_or(0) != 0,
                    dependency_graph: row.get::<_, Option<String>>(35)?.and_then(|s| serde_json::from_str(&s).ok()),
                    cost_annotations: row.get::<_, Option<String>>(36)?.and_then(|s| serde_json::from_str(&s).ok()),
                    quality_report: row.get::<_, Option<String>>(37)?.and_then(|s| serde_json::from_str(&s).ok()),
                    constraint_overrides: row.get::<_, Option<String>>(38)?.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default(),
                    // targeted_error_ids is a runtime field, not stored in DB
                    targeted_error_ids: vec![],
                })
            },
        );

        match result {
            Ok(workflow) => Ok(Some(workflow)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("Failed to get unified workflow: {}", e)),
        }
    }

    /// Get a unified workflow by name (returns the first match if multiple exist)
    pub fn get_unified_workflow_by_name(
        &self,
        name: &str,
    ) -> Result<Option<crate::unified_workflows::UnifiedWorkflow>, String> {
        let conn = self.get_conn()?;

        let result = conn.query_row(
            r#"
            SELECT id, name, description, category, tags, setup_steps, verification_steps,
                   agentic_steps, completion_steps, max_iterations, provider, model,
                   skip_ai_summary, created_at, updated_at, log_source_selection,
                   context_ids, disabled_context_ids, auto_include_contexts, prompt_template,
                   log_watch_enabled, health_check_enabled, health_check_urls, timeout_seconds,
                   preflight_check_enabled, generated_by_task_run_id, enable_sweep, max_sweep_iterations,
                   stages, stop_on_failure, reflection_mode, model_overrides, approval_gate,
                   completion_prompts_first, is_favorite, dependency_graph, cost_annotations,
                   quality_report, constraint_overrides
            FROM unified_workflows
            WHERE name = ?1
            ORDER BY updated_at DESC
            LIMIT 1
            "#,
            params![name],
            |row| {
                Ok(crate::unified_workflows::UnifiedWorkflow {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    description: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                    category: row
                        .get::<_, Option<String>>(3)?
                        .unwrap_or_else(|| "general".to_string()),
                    tags: row
                        .get::<_, Option<String>>(4)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    setup_steps: row
                        .get::<_, Option<String>>(5)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    verification_steps: row
                        .get::<_, Option<String>>(6)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    agentic_steps: row
                        .get::<_, Option<String>>(7)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    completion_steps: row
                        .get::<_, Option<String>>(8)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    max_iterations: row.get::<_, i64>(9)? as u32,
                    provider: row.get(10)?,
                    model: row.get(11)?,
                    skip_ai_summary: row.get::<_, i32>(12)? != 0,
                    created_at: row.get(13)?,
                    updated_at: row.get(14)?,
                    log_source_selection: row
                        .get::<_, Option<String>>(15)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    context_ids: row
                        .get::<_, Option<String>>(16)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    disabled_context_ids: row
                        .get::<_, Option<String>>(17)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    auto_include_contexts: row.get::<_, Option<i32>>(18)?.unwrap_or(1) != 0,
                    prompt_template: row.get(19)?,
                    log_watch_enabled: row.get::<_, Option<i32>>(20)?.unwrap_or(1) != 0,
                    health_check_enabled: row.get::<_, Option<i32>>(21)?.unwrap_or(1) != 0,
                    health_check_urls: row
                        .get::<_, Option<String>>(22)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    timeout_seconds: row.get::<_, Option<i64>>(23)?.map(|v| v as u64),
                    preflight_check_enabled: row.get::<_, Option<i32>>(24)?.unwrap_or(1) != 0,
                    generated_by_task_run_id: row.get(25)?,
                    enable_sweep: row.get::<_, Option<i32>>(26)?.unwrap_or(0) != 0,
                    max_sweep_iterations: row.get::<_, Option<i32>>(27)?.unwrap_or(5) as u32,
                    stages: row
                        .get::<_, Option<String>>(28)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    stop_on_failure: row.get::<_, Option<i32>>(29)?.unwrap_or(0) != 0,
                    reflection_mode: row.get::<_, Option<i32>>(30)?.unwrap_or(1) != 0,
                    model_overrides: {
                        let json_str: String = row.get::<_, Option<String>>(31)?.unwrap_or_else(|| "{}".to_string());
                        serde_json::from_str(&json_str).unwrap_or_default()
                    },
                    approval_gate: row.get::<_, Option<i32>>(32)?.unwrap_or(0) != 0,
                    completion_prompts_first: row.get::<_, Option<i32>>(33)?.unwrap_or(0) != 0,
                    is_favorite: row.get::<_, Option<i32>>(34)?.unwrap_or(0) != 0,
                    dependency_graph: row.get::<_, Option<String>>(35)?.and_then(|s| serde_json::from_str(&s).ok()),
                    cost_annotations: row.get::<_, Option<String>>(36)?.and_then(|s| serde_json::from_str(&s).ok()),
                    quality_report: row.get::<_, Option<String>>(37)?.and_then(|s| serde_json::from_str(&s).ok()),
                    constraint_overrides: row.get::<_, Option<String>>(38)?.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default(),
                    // targeted_error_ids is a runtime field, not stored in DB
                    targeted_error_ids: vec![],
                })
            },
        );

        match result {
            Ok(workflow) => Ok(Some(workflow)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("Failed to get unified workflow by name: {}", e)),
        }
    }

    /// Create a new unified workflow
    pub fn create_unified_workflow(
        &self,
        request: &crate::unified_workflows::CreateUnifiedWorkflowRequest,
    ) -> Result<crate::unified_workflows::UnifiedWorkflow, String> {
        let conn = self.get_conn()?;
        let id = uuid::Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();

        let tags_json = serde_json::to_string(&request.tags).unwrap_or_else(|_| "[]".to_string());
        let setup_steps_json =
            serde_json::to_string(&request.setup_steps).unwrap_or_else(|_| "[]".to_string());
        let verification_steps_json =
            serde_json::to_string(&request.verification_steps).unwrap_or_else(|_| "[]".to_string());
        let agentic_steps_json =
            serde_json::to_string(&request.agentic_steps).unwrap_or_else(|_| "[]".to_string());
        let completion_steps_json =
            serde_json::to_string(&request.completion_steps).unwrap_or_else(|_| "[]".to_string());
        let log_source_selection_json = request
            .log_source_selection
            .as_ref()
            .map(|ls| serde_json::to_string(ls).unwrap_or_else(|_| "\"default\"".to_string()))
            .unwrap_or_else(|| "\"default\"".to_string());
        let context_ids_json = request
            .context_ids
            .as_ref()
            .map(|ids| serde_json::to_string(ids).unwrap_or_else(|_| "[]".to_string()))
            .unwrap_or_else(|| "[]".to_string());
        let disabled_context_ids_json = request
            .disabled_context_ids
            .as_ref()
            .map(|ids| serde_json::to_string(ids).unwrap_or_else(|_| "[]".to_string()))
            .unwrap_or_else(|| "[]".to_string());
        let health_check_urls_json = request
            .health_check_urls
            .as_ref()
            .map(|urls| serde_json::to_string(urls).unwrap_or_else(|_| "[]".to_string()))
            .unwrap_or_else(|| "[]".to_string());
        let auto_include_contexts = request.auto_include_contexts.unwrap_or(true);
        let log_watch_enabled = request.log_watch_enabled.unwrap_or(true);
        let health_check_enabled = request.health_check_enabled.unwrap_or(true);
        let preflight_check_enabled = request.preflight_check_enabled.unwrap_or(true);

        conn.execute(
            r#"
            INSERT INTO unified_workflows (
                id, name, description, category, tags, setup_steps, verification_steps,
                agentic_steps, completion_steps, max_iterations, timeout_seconds, provider, model,
                skip_ai_summary, created_at, updated_at, log_source_selection,
                context_ids, disabled_context_ids, auto_include_contexts, prompt_template,
                log_watch_enabled, health_check_enabled, health_check_urls, preflight_check_enabled,
                generated_by_task_run_id, enable_sweep, max_sweep_iterations,
                stages, stop_on_failure, reflection_mode, model_overrides, approval_gate,
                completion_prompts_first, dependency_graph, cost_annotations, quality_report,
                constraint_overrides
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?29, ?30, ?31, ?32, ?33, ?34, ?35, ?36, ?37, ?38)
            "#,
            params![
                id,
                request.name,
                request.description,
                request.category,
                tags_json,
                setup_steps_json,
                verification_steps_json,
                agentic_steps_json,
                completion_steps_json,
                request.max_iterations as i64,
                request.timeout_seconds.map(|t| t as i64),
                request.provider,
                request.model,
                request.skip_ai_summary,
                now,
                now,
                log_source_selection_json,
                context_ids_json,
                disabled_context_ids_json,
                auto_include_contexts,
                request.prompt_template,
                log_watch_enabled,
                health_check_enabled,
                health_check_urls_json,
                preflight_check_enabled,
                request.generated_by_task_run_id,
                request.enable_sweep.unwrap_or(false),
                request.max_sweep_iterations.unwrap_or(5) as i64,
                serde_json::to_string(&request.stages).unwrap_or_else(|_| "[]".to_string()),
                request.stop_on_failure.unwrap_or(false),
                request.reflection_mode.unwrap_or(true),
                serde_json::to_string(&request.model_overrides.clone().unwrap_or_default()).unwrap_or_else(|_| "{}".to_string()),
                request.approval_gate.unwrap_or(false),
                request.completion_prompts_first.unwrap_or(false),
                request.dependency_graph.as_ref().map(|v| v.to_string()),
                request.cost_annotations.as_ref().map(|v| v.to_string()),
                request.quality_report.as_ref().map(|v| v.to_string()),
                serde_json::to_string(&request.constraint_overrides.clone().unwrap_or_default()).unwrap_or_else(|_| "{}".to_string()),
            ],
        )
        .map_err(|e| format!("Failed to create unified workflow: {}", e))?;

        self.get_unified_workflow(&id)?
            .ok_or_else(|| "Failed to retrieve created workflow".to_string())
    }

    /// Create a new unified workflow with a specific ID (for imports)
    pub fn create_unified_workflow_with_id(
        &self,
        id: &str,
        request: &crate::unified_workflows::CreateUnifiedWorkflowRequest,
    ) -> Result<crate::unified_workflows::UnifiedWorkflow, String> {
        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();

        let tags_json = serde_json::to_string(&request.tags).unwrap_or_else(|_| "[]".to_string());
        let setup_steps_json =
            serde_json::to_string(&request.setup_steps).unwrap_or_else(|_| "[]".to_string());
        let verification_steps_json =
            serde_json::to_string(&request.verification_steps).unwrap_or_else(|_| "[]".to_string());
        let agentic_steps_json =
            serde_json::to_string(&request.agentic_steps).unwrap_or_else(|_| "[]".to_string());
        let completion_steps_json =
            serde_json::to_string(&request.completion_steps).unwrap_or_else(|_| "[]".to_string());
        let log_source_selection_json = request
            .log_source_selection
            .as_ref()
            .map(|ls| serde_json::to_string(ls).unwrap_or_else(|_| "\"default\"".to_string()))
            .unwrap_or_else(|| "\"default\"".to_string());
        let context_ids_json = request
            .context_ids
            .as_ref()
            .map(|ids| serde_json::to_string(ids).unwrap_or_else(|_| "[]".to_string()))
            .unwrap_or_else(|| "[]".to_string());
        let disabled_context_ids_json = request
            .disabled_context_ids
            .as_ref()
            .map(|ids| serde_json::to_string(ids).unwrap_or_else(|_| "[]".to_string()))
            .unwrap_or_else(|| "[]".to_string());
        let health_check_urls_json = request
            .health_check_urls
            .as_ref()
            .map(|urls| serde_json::to_string(urls).unwrap_or_else(|_| "[]".to_string()))
            .unwrap_or_else(|| "[]".to_string());
        let auto_include_contexts = request.auto_include_contexts.unwrap_or(true);
        let log_watch_enabled = request.log_watch_enabled.unwrap_or(true);
        let health_check_enabled = request.health_check_enabled.unwrap_or(true);
        let preflight_check_enabled = request.preflight_check_enabled.unwrap_or(true);

        conn.execute(
            r#"
            INSERT INTO unified_workflows (
                id, name, description, category, tags, setup_steps, verification_steps,
                agentic_steps, completion_steps, max_iterations, timeout_seconds, provider, model,
                skip_ai_summary, created_at, updated_at, log_source_selection,
                context_ids, disabled_context_ids, auto_include_contexts, prompt_template,
                log_watch_enabled, health_check_enabled, health_check_urls, preflight_check_enabled,
                generated_by_task_run_id, enable_sweep, max_sweep_iterations,
                stages, stop_on_failure, reflection_mode, model_overrides, approval_gate,
                completion_prompts_first, dependency_graph, cost_annotations, quality_report,
                constraint_overrides
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?29, ?30, ?31, ?32, ?33, ?34, ?35, ?36, ?37, ?38)
            "#,
            params![
                id,
                request.name,
                request.description,
                request.category,
                tags_json,
                setup_steps_json,
                verification_steps_json,
                agentic_steps_json,
                completion_steps_json,
                request.max_iterations as i64,
                request.timeout_seconds.map(|t| t as i64),
                request.provider,
                request.model,
                request.skip_ai_summary,
                now,
                now,
                log_source_selection_json,
                context_ids_json,
                disabled_context_ids_json,
                auto_include_contexts,
                request.prompt_template,
                log_watch_enabled,
                health_check_enabled,
                health_check_urls_json,
                preflight_check_enabled,
                request.generated_by_task_run_id,
                request.enable_sweep.unwrap_or(false),
                request.max_sweep_iterations.unwrap_or(5) as i64,
                serde_json::to_string(&request.stages).unwrap_or_else(|_| "[]".to_string()),
                request.stop_on_failure.unwrap_or(false),
                request.reflection_mode.unwrap_or(true),
                serde_json::to_string(&request.model_overrides.clone().unwrap_or_default()).unwrap_or_else(|_| "{}".to_string()),
                request.approval_gate.unwrap_or(false),
                request.completion_prompts_first.unwrap_or(false),
                request.dependency_graph.as_ref().map(|v| v.to_string()),
                request.cost_annotations.as_ref().map(|v| v.to_string()),
                request.quality_report.as_ref().map(|v| v.to_string()),
                serde_json::to_string(&request.constraint_overrides.clone().unwrap_or_default()).unwrap_or_else(|_| "{}".to_string()),
            ],
        )
        .map_err(|e| format!("Failed to create unified workflow: {}", e))?;

        self.get_unified_workflow(id)?
            .ok_or_else(|| "Failed to retrieve created workflow".to_string())
    }

    /// Update an existing unified workflow
    pub fn update_unified_workflow(
        &self,
        id: &str,
        request: &crate::unified_workflows::UpdateUnifiedWorkflowRequest,
    ) -> Result<crate::unified_workflows::UnifiedWorkflow, String> {
        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();

        // Get existing workflow
        let existing = self
            .get_unified_workflow(id)?
            .ok_or_else(|| format!("Unified workflow not found: {}", id))?;

        // Merge updates
        let name = request.name.as_ref().unwrap_or(&existing.name);
        let description = request
            .description
            .as_ref()
            .unwrap_or(&existing.description);
        let category = request.category.as_ref().unwrap_or(&existing.category);
        let tags = request.tags.as_ref().unwrap_or(&existing.tags);
        let setup_steps = request
            .setup_steps
            .as_ref()
            .unwrap_or(&existing.setup_steps);
        let verification_steps = request
            .verification_steps
            .as_ref()
            .unwrap_or(&existing.verification_steps);
        let agentic_steps = request
            .agentic_steps
            .as_ref()
            .unwrap_or(&existing.agentic_steps);
        let completion_steps = request
            .completion_steps
            .as_ref()
            .unwrap_or(&existing.completion_steps);
        let max_iterations = request.max_iterations.unwrap_or(existing.max_iterations);
        // For timeout_seconds: Option<Option<u64>> where:
        // - None: Not updating, keep existing
        // - Some(None): Explicitly disable timeout
        // - Some(Some(N)): Set timeout to N seconds
        let timeout_seconds: Option<u64> = match &request.timeout_seconds {
            Some(val) => *val,                // Use provided value (including None to disable)
            None => existing.timeout_seconds, // Not provided, keep existing
        };
        let provider = request.provider.as_ref().or(existing.provider.as_ref());
        let model = request.model.as_ref().or(existing.model.as_ref());
        let skip_ai_summary = request.skip_ai_summary.unwrap_or(existing.skip_ai_summary);
        let log_source_selection = request
            .log_source_selection
            .as_ref()
            .unwrap_or(&existing.log_source_selection);
        // For prompt_template: if request has a value, use it; otherwise keep existing
        // Empty string means clear the template (set to NULL)
        let prompt_template: Option<&str> = match &request.prompt_template {
            Some(val) if val.is_empty() => None, // Empty string clears the template
            Some(val) => Some(val.as_str()),     // Non-empty string sets the template
            None => existing.prompt_template.as_deref(), // Not provided keeps existing
        };
        let context_ids = request
            .context_ids
            .as_ref()
            .unwrap_or(&existing.context_ids);
        let disabled_context_ids = request
            .disabled_context_ids
            .as_ref()
            .unwrap_or(&existing.disabled_context_ids);
        let auto_include_contexts = request
            .auto_include_contexts
            .unwrap_or(existing.auto_include_contexts);
        let log_watch_enabled = request
            .log_watch_enabled
            .unwrap_or(existing.log_watch_enabled);
        let health_check_enabled = request
            .health_check_enabled
            .unwrap_or(existing.health_check_enabled);
        let health_check_urls = request
            .health_check_urls
            .as_ref()
            .unwrap_or(&existing.health_check_urls);
        let preflight_check_enabled = request
            .preflight_check_enabled
            .unwrap_or(existing.preflight_check_enabled);
        let enable_sweep = request.enable_sweep.unwrap_or(existing.enable_sweep);
        let max_sweep_iterations = request
            .max_sweep_iterations
            .unwrap_or(existing.max_sweep_iterations);
        let stages = request.stages.as_ref().unwrap_or(&existing.stages);
        let stop_on_failure = request.stop_on_failure.unwrap_or(existing.stop_on_failure);
        let constraint_overrides = request
            .constraint_overrides
            .as_ref()
            .unwrap_or(&existing.constraint_overrides);
        let approval_gate = request.approval_gate.unwrap_or(existing.approval_gate);
        let reflection_mode = request.reflection_mode.unwrap_or(existing.reflection_mode);
        let completion_prompts_first = request
            .completion_prompts_first
            .unwrap_or(existing.completion_prompts_first);
        let model_overrides = request
            .model_overrides
            .as_ref()
            .unwrap_or(&existing.model_overrides);
        let dependency_graph = request
            .dependency_graph
            .as_ref()
            .or(existing.dependency_graph.as_ref());
        let cost_annotations = request
            .cost_annotations
            .as_ref()
            .or(existing.cost_annotations.as_ref());
        let quality_report = request
            .quality_report
            .as_ref()
            .or(existing.quality_report.as_ref());

        let tags_json = serde_json::to_string(tags).unwrap_or_else(|_| "[]".to_string());
        let setup_steps_json =
            serde_json::to_string(setup_steps).unwrap_or_else(|_| "[]".to_string());
        let verification_steps_json =
            serde_json::to_string(verification_steps).unwrap_or_else(|_| "[]".to_string());
        let agentic_steps_json =
            serde_json::to_string(agentic_steps).unwrap_or_else(|_| "[]".to_string());
        let completion_steps_json =
            serde_json::to_string(completion_steps).unwrap_or_else(|_| "[]".to_string());
        let log_source_selection_json = serde_json::to_string(log_source_selection)
            .unwrap_or_else(|_| "\"default\"".to_string());
        let context_ids_json =
            serde_json::to_string(context_ids).unwrap_or_else(|_| "[]".to_string());
        let disabled_context_ids_json =
            serde_json::to_string(disabled_context_ids).unwrap_or_else(|_| "[]".to_string());
        let health_check_urls_json =
            serde_json::to_string(health_check_urls).unwrap_or_else(|_| "[]".to_string());
        let stages_json = serde_json::to_string(stages).unwrap_or_else(|_| "[]".to_string());
        let model_overrides_json =
            serde_json::to_string(model_overrides).unwrap_or_else(|_| "{}".to_string());

        conn.execute(
            r#"
            UPDATE unified_workflows SET
                name = ?1,
                description = ?2,
                category = ?3,
                tags = ?4,
                setup_steps = ?5,
                verification_steps = ?6,
                agentic_steps = ?7,
                completion_steps = ?8,
                max_iterations = ?9,
                timeout_seconds = ?10,
                provider = ?11,
                model = ?12,
                skip_ai_summary = ?13,
                updated_at = ?14,
                log_source_selection = ?15,
                prompt_template = ?16,
                context_ids = ?17,
                disabled_context_ids = ?18,
                auto_include_contexts = ?19,
                log_watch_enabled = ?20,
                health_check_enabled = ?21,
                health_check_urls = ?22,
                preflight_check_enabled = ?23,
                enable_sweep = ?24,
                max_sweep_iterations = ?25,
                stages = ?26,
                stop_on_failure = ?27,
                approval_gate = ?28,
                reflection_mode = ?29,
                model_overrides = ?30,
                completion_prompts_first = ?31,
                dependency_graph = ?32,
                cost_annotations = ?33,
                quality_report = ?34,
                constraint_overrides = ?35
            WHERE id = ?36
            "#,
            params![
                name,
                description,
                category,
                tags_json,
                setup_steps_json,
                verification_steps_json,
                agentic_steps_json,
                completion_steps_json,
                max_iterations as i64,
                timeout_seconds.map(|v| v as i64),
                provider,
                model,
                skip_ai_summary,
                now,
                log_source_selection_json,
                prompt_template,
                context_ids_json,
                disabled_context_ids_json,
                auto_include_contexts,
                log_watch_enabled,
                health_check_enabled,
                health_check_urls_json,
                preflight_check_enabled,
                enable_sweep,
                max_sweep_iterations as i64,
                stages_json,
                stop_on_failure,
                approval_gate,
                reflection_mode,
                model_overrides_json,
                completion_prompts_first,
                dependency_graph.map(|v| v.to_string()),
                cost_annotations.map(|v| v.to_string()),
                quality_report.map(|v| v.to_string()),
                serde_json::to_string(constraint_overrides).unwrap_or_else(|_| "{}".to_string()),
                id,
            ],
        )
        .map_err(|e| format!("Failed to update unified workflow: {}", e))?;

        self.get_unified_workflow(id)?
            .ok_or_else(|| "Failed to retrieve updated workflow".to_string())
    }

    /// Delete a unified workflow
    pub fn delete_unified_workflow(&self, id: &str) -> Result<bool, String> {
        let conn = self.get_conn()?;

        let rows_affected = conn
            .execute("DELETE FROM unified_workflows WHERE id = ?1", params![id])
            .map_err(|e| format!("Failed to delete unified workflow: {}", e))?;

        Ok(rows_affected > 0)
    }

    /// Search unified workflows with filters
    pub fn search_unified_workflows(
        &self,
        query: &crate::unified_workflows::SearchUnifiedWorkflowsQuery,
    ) -> Result<Vec<crate::unified_workflows::UnifiedWorkflow>, String> {
        let conn = self.get_conn()?;

        let mut sql = String::from(
            r#"
            SELECT id, name, description, category, tags, setup_steps, verification_steps,
                   agentic_steps, completion_steps, max_iterations, provider, model,
                   skip_ai_summary, created_at, updated_at, log_source_selection,
                   context_ids, disabled_context_ids, auto_include_contexts, prompt_template,
                   log_watch_enabled, health_check_enabled, health_check_urls, timeout_seconds,
                   preflight_check_enabled, generated_by_task_run_id, enable_sweep, max_sweep_iterations,
                   stages, stop_on_failure, reflection_mode, model_overrides, approval_gate,
                   completion_prompts_first, is_favorite, dependency_graph, cost_annotations,
                   quality_report, constraint_overrides
            FROM unified_workflows
            WHERE 1=1
            "#,
        );

        let mut params_vec: Vec<String> = Vec::new();

        if let Some(q) = &query.q {
            sql.push_str(" AND (name LIKE ? OR description LIKE ?)");
            let pattern = format!("%{}%", q);
            params_vec.push(pattern.clone());
            params_vec.push(pattern);
        }

        if let Some(category) = &query.category {
            sql.push_str(" AND category = ?");
            params_vec.push(category.clone());
        }

        if let Some(tag) = &query.tag {
            sql.push_str(" AND tags LIKE ?");
            params_vec.push(format!("%\"{}%", tag));
        }

        sql.push_str(" ORDER BY is_favorite DESC, updated_at DESC");

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let params: Vec<&dyn rusqlite::ToSql> = params_vec
            .iter()
            .map(|s| s as &dyn rusqlite::ToSql)
            .collect();

        let workflows = stmt
            .query_map(params.as_slice(), |row| {
                Ok(crate::unified_workflows::UnifiedWorkflow {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    description: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                    category: row
                        .get::<_, Option<String>>(3)?
                        .unwrap_or_else(|| "general".to_string()),
                    tags: row
                        .get::<_, Option<String>>(4)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    setup_steps: row
                        .get::<_, Option<String>>(5)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    verification_steps: row
                        .get::<_, Option<String>>(6)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    agentic_steps: row
                        .get::<_, Option<String>>(7)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    completion_steps: row
                        .get::<_, Option<String>>(8)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    max_iterations: row.get::<_, i64>(9)? as u32,
                    provider: row.get(10)?,
                    model: row.get(11)?,
                    skip_ai_summary: row.get::<_, i32>(12)? != 0,
                    created_at: row.get(13)?,
                    updated_at: row.get(14)?,
                    log_source_selection: row
                        .get::<_, Option<String>>(15)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    context_ids: row
                        .get::<_, Option<String>>(16)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    disabled_context_ids: row
                        .get::<_, Option<String>>(17)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    auto_include_contexts: row.get::<_, Option<i32>>(18)?.unwrap_or(1) != 0,
                    prompt_template: row.get(19)?,
                    log_watch_enabled: row.get::<_, Option<i32>>(20)?.unwrap_or(1) != 0,
                    health_check_enabled: row.get::<_, Option<i32>>(21)?.unwrap_or(1) != 0,
                    health_check_urls: row
                        .get::<_, Option<String>>(22)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    timeout_seconds: row.get::<_, Option<i64>>(23)?.map(|v| v as u64),
                    preflight_check_enabled: row.get::<_, Option<i32>>(24)?.unwrap_or(1) != 0,
                    generated_by_task_run_id: row.get(25)?,
                    enable_sweep: row.get::<_, Option<i32>>(26)?.unwrap_or(0) != 0,
                    max_sweep_iterations: row.get::<_, Option<i32>>(27)?.unwrap_or(5) as u32,
                    stages: row
                        .get::<_, Option<String>>(28)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    stop_on_failure: row.get::<_, Option<i32>>(29)?.unwrap_or(0) != 0,
                    reflection_mode: row.get::<_, Option<i32>>(30)?.unwrap_or(1) != 0,
                    model_overrides: {
                        let json_str: String = row
                            .get::<_, Option<String>>(31)?
                            .unwrap_or_else(|| "{}".to_string());
                        serde_json::from_str(&json_str).unwrap_or_default()
                    },
                    approval_gate: row.get::<_, Option<i32>>(32)?.unwrap_or(0) != 0,
                    completion_prompts_first: row.get::<_, Option<i32>>(33)?.unwrap_or(0) != 0,
                    is_favorite: row.get::<_, Option<i32>>(34)?.unwrap_or(0) != 0,
                    dependency_graph: row
                        .get::<_, Option<String>>(35)?
                        .and_then(|s| serde_json::from_str(&s).ok()),
                    cost_annotations: row
                        .get::<_, Option<String>>(36)?
                        .and_then(|s| serde_json::from_str(&s).ok()),
                    quality_report: row
                        .get::<_, Option<String>>(37)?
                        .and_then(|s| serde_json::from_str(&s).ok()),
                    constraint_overrides: row
                        .get::<_, Option<String>>(38)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    // targeted_error_ids is a runtime field, not stored in DB
                    targeted_error_ids: vec![],
                })
            })
            .map_err(|e| format!("Failed to search unified workflows: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(workflows)
    }

    /// Duplicate a unified workflow
    pub fn duplicate_unified_workflow(
        &self,
        id: &str,
    ) -> Result<crate::unified_workflows::UnifiedWorkflow, String> {
        let original = self
            .get_unified_workflow(id)?
            .ok_or_else(|| format!("Unified workflow not found: {}", id))?;

        let create_request = crate::unified_workflows::CreateUnifiedWorkflowRequest {
            name: format!("{} (Copy)", original.name),
            description: original.description,
            category: original.category,
            tags: original.tags,
            setup_steps: original.setup_steps,
            verification_steps: original.verification_steps,
            agentic_steps: original.agentic_steps,
            completion_steps: original.completion_steps,
            max_iterations: original.max_iterations,
            timeout_seconds: original.timeout_seconds,
            provider: original.provider,
            model: original.model,
            skip_ai_summary: original.skip_ai_summary,
            log_source_selection: Some(original.log_source_selection),
            context_ids: Some(original.context_ids),
            disabled_context_ids: Some(original.disabled_context_ids),
            auto_include_contexts: Some(original.auto_include_contexts),
            prompt_template: original.prompt_template,
            log_watch_enabled: Some(original.log_watch_enabled),
            health_check_enabled: Some(original.health_check_enabled),
            health_check_urls: Some(original.health_check_urls),
            preflight_check_enabled: Some(original.preflight_check_enabled),
            targeted_error_ids: None,
            generated_by_task_run_id: None, // Don't copy the generator reference
            enable_sweep: Some(original.enable_sweep),
            max_sweep_iterations: Some(original.max_sweep_iterations),
            stages: Some(original.stages),
            stop_on_failure: Some(original.stop_on_failure),
            constraint_overrides: Some(original.constraint_overrides),
            approval_gate: Some(original.approval_gate),
            reflection_mode: Some(original.reflection_mode),
            completion_prompts_first: Some(original.completion_prompts_first),
            model_overrides: Some(original.model_overrides),
            dependency_graph: original.dependency_graph,
            cost_annotations: original.cost_annotations,
            quality_report: original.quality_report,
        };

        self.create_unified_workflow(&create_request)
    }

    /// Toggle the is_favorite flag on a unified workflow.
    /// Returns the new favorite state, or an error if the workflow doesn't exist.
    pub fn toggle_unified_workflow_favorite(&self, id: &str) -> Result<bool, String> {
        let conn = self.get_conn()?;

        let rows_affected = conn
            .execute(
                "UPDATE unified_workflows SET is_favorite = CASE WHEN is_favorite = 1 THEN 0 ELSE 1 END WHERE id = ?1",
                params![id],
            )
            .map_err(|e| format!("Failed to toggle favorite: {}", e))?;

        if rows_affected == 0 {
            return Err(format!("Workflow not found: {}", id));
        }

        let new_state: bool = conn
            .query_row(
                "SELECT is_favorite FROM unified_workflows WHERE id = ?1",
                params![id],
                |row| Ok(row.get::<_, i32>(0)? != 0),
            )
            .map_err(|e| format!("Failed to read favorite state: {}", e))?;

        Ok(new_state)
    }

    // ========================================================================
    // Workflow Sync Helpers
    // ========================================================================

    /// Get all workflows with sync_pending = 1 (created/modified offline).
    pub fn get_pending_sync_workflows(
        &self,
    ) -> Result<Vec<crate::unified_workflows::UnifiedWorkflow>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, name, description, category, tags, setup_steps, verification_steps,
                       agentic_steps, completion_steps, max_iterations, provider, model,
                       skip_ai_summary, created_at, updated_at, log_source_selection,
                       context_ids, disabled_context_ids, auto_include_contexts, prompt_template,
                       log_watch_enabled, health_check_enabled, health_check_urls, timeout_seconds,
                       preflight_check_enabled, generated_by_task_run_id, enable_sweep,
                       max_sweep_iterations, stages, stop_on_failure, reflection_mode, model_overrides,
                       approval_gate, completion_prompts_first, is_favorite, dependency_graph,
                       cost_annotations, quality_report, constraint_overrides
                FROM unified_workflows
                WHERE sync_pending = 1
                "#,
            )
            .map_err(|e| format!("Failed to prepare pending sync query: {}", e))?;

        let workflows = stmt
            .query_map([], |row| {
                Ok(crate::unified_workflows::UnifiedWorkflow {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    description: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                    category: row
                        .get::<_, Option<String>>(3)?
                        .unwrap_or_else(|| "general".to_string()),
                    tags: row
                        .get::<_, Option<String>>(4)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    setup_steps: row
                        .get::<_, Option<String>>(5)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    verification_steps: row
                        .get::<_, Option<String>>(6)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    agentic_steps: row
                        .get::<_, Option<String>>(7)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    completion_steps: row
                        .get::<_, Option<String>>(8)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    max_iterations: row.get::<_, i64>(9)? as u32,
                    provider: row.get(10)?,
                    model: row.get(11)?,
                    skip_ai_summary: row.get::<_, i32>(12)? != 0,
                    created_at: row.get(13)?,
                    updated_at: row.get(14)?,
                    log_source_selection: row
                        .get::<_, Option<String>>(15)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    context_ids: row
                        .get::<_, Option<String>>(16)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    disabled_context_ids: row
                        .get::<_, Option<String>>(17)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    auto_include_contexts: row.get::<_, Option<i32>>(18)?.unwrap_or(1) != 0,
                    prompt_template: row.get(19)?,
                    log_watch_enabled: row.get::<_, Option<i32>>(20)?.unwrap_or(1) != 0,
                    health_check_enabled: row.get::<_, Option<i32>>(21)?.unwrap_or(1) != 0,
                    health_check_urls: row
                        .get::<_, Option<String>>(22)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    timeout_seconds: row.get::<_, Option<i64>>(23)?.map(|v| v as u64),
                    preflight_check_enabled: row.get::<_, Option<i32>>(24)?.unwrap_or(1) != 0,
                    generated_by_task_run_id: row.get(25)?,
                    enable_sweep: row.get::<_, Option<i32>>(26)?.unwrap_or(0) != 0,
                    max_sweep_iterations: row.get::<_, Option<i32>>(27)?.unwrap_or(5) as u32,
                    stages: row
                        .get::<_, Option<String>>(28)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    stop_on_failure: row.get::<_, Option<i32>>(29)?.unwrap_or(0) != 0,
                    reflection_mode: row.get::<_, Option<i32>>(30)?.unwrap_or(1) != 0,
                    model_overrides: {
                        let json_str: String = row
                            .get::<_, Option<String>>(31)?
                            .unwrap_or_else(|| "{}".to_string());
                        serde_json::from_str(&json_str).unwrap_or_default()
                    },
                    approval_gate: row.get::<_, Option<i32>>(32)?.unwrap_or(0) != 0,
                    completion_prompts_first: row.get::<_, Option<i32>>(33)?.unwrap_or(0) != 0,
                    is_favorite: row.get::<_, Option<i32>>(34)?.unwrap_or(0) != 0,
                    dependency_graph: row
                        .get::<_, Option<String>>(35)?
                        .and_then(|s| serde_json::from_str(&s).ok()),
                    cost_annotations: row
                        .get::<_, Option<String>>(36)?
                        .and_then(|s| serde_json::from_str(&s).ok()),
                    quality_report: row
                        .get::<_, Option<String>>(37)?
                        .and_then(|s| serde_json::from_str(&s).ok()),
                    constraint_overrides: row
                        .get::<_, Option<String>>(38)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    targeted_error_ids: vec![],
                })
            })
            .map_err(|e| format!("Failed to query pending sync workflows: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(workflows)
    }

    /// Clear the sync_pending flag for a workflow after successful push to backend.
    pub fn clear_sync_pending(&self, id: &str) -> Result<(), String> {
        let conn = self.get_conn()?;
        conn.execute(
            "UPDATE unified_workflows SET sync_pending = 0 WHERE id = ?1",
            params![id],
        )
        .map_err(|e| format!("Failed to clear sync_pending: {}", e))?;
        Ok(())
    }

    /// Set the sync_pending flag for a workflow (created/modified while offline).
    pub fn set_sync_pending(&self, id: &str) -> Result<(), String> {
        let conn = self.get_conn()?;
        conn.execute(
            "UPDATE unified_workflows SET sync_pending = 1 WHERE id = ?1",
            params![id],
        )
        .map_err(|e| format!("Failed to set sync_pending: {}", e))?;
        Ok(())
    }

    // ========================================================================
    // Hybrid Logging Operations (Phase 10)
    // ========================================================================

    /// Create a task run event (batch insert for migration from JSONL).
    pub fn create_task_run_event(&self, input: &CreateTaskRunEventInput) -> Result<i64, String> {
        let conn = self.get_conn()?;

        conn.execute(
            r#"
            INSERT INTO task_run_events (
                task_run_id, event_type, event_subtype, message, data,
                workflow_name, state_name, action_id, timestamp, duration_ms
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
            "#,
            params![
                input.task_run_id,
                input.event_type,
                input.event_subtype,
                input.message,
                input.data,
                input.workflow_name,
                input.state_name,
                input.action_id,
                input.timestamp,
                input.duration_ms,
            ],
        )
        .map_err(|e| format!("Failed to create task run event: {}", e))?;

        let id = conn.last_insert_rowid();
        Ok(id)
    }

    /// Batch insert task run events (efficient for JSONL migration).
    pub fn batch_create_task_run_events(
        &self,
        events: &[CreateTaskRunEventInput],
    ) -> Result<usize, String> {
        if events.is_empty() {
            return Ok(0);
        }

        let conn = self.get_conn()?;
        let mut count = 0;

        for event in events {
            conn.execute(
                r#"
                INSERT INTO task_run_events (
                    task_run_id, event_type, event_subtype, message, data,
                    workflow_name, state_name, action_id, timestamp, duration_ms
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                "#,
                params![
                    event.task_run_id,
                    event.event_type,
                    event.event_subtype,
                    event.message,
                    event.data,
                    event.workflow_name,
                    event.state_name,
                    event.action_id,
                    event.timestamp,
                    event.duration_ms,
                ],
            )
            .map_err(|e| format!("Failed to create task run event: {}", e))?;
            count += 1;
        }

        Ok(count)
    }

    /// Get events for a task run with optional filtering.
    pub fn get_task_run_events(
        &self,
        task_run_id: &str,
        event_type: Option<&str>,
        limit: Option<u32>,
    ) -> Result<Vec<TaskRunEvent>, String> {
        let conn = self.get_conn()?;

        let mut sql = String::from(
            r#"
            SELECT id, task_run_id, event_type, event_subtype, message, data,
                   workflow_name, state_name, action_id, timestamp, duration_ms
            FROM task_run_events
            WHERE task_run_id = ?1
            "#,
        );

        let mut params_vec: Vec<String> = vec![task_run_id.to_string()];

        if let Some(et) = event_type {
            sql.push_str(" AND event_type = ?2");
            params_vec.push(et.to_string());
        }

        sql.push_str(" ORDER BY timestamp ASC");

        if let Some(lim) = limit {
            sql.push_str(&format!(" LIMIT {}", lim));
        }

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let params: Vec<&dyn rusqlite::ToSql> = params_vec
            .iter()
            .map(|s| s as &dyn rusqlite::ToSql)
            .collect();

        let events = stmt
            .query_map(params.as_slice(), |row| {
                Ok(TaskRunEvent {
                    id: row.get(0)?,
                    task_run_id: row.get(1)?,
                    event_type: row.get(2)?,
                    event_subtype: row.get(3)?,
                    message: row.get(4)?,
                    data: row.get(5)?,
                    workflow_name: row.get(6)?,
                    state_name: row.get(7)?,
                    action_id: row.get(8)?,
                    timestamp: row.get(9)?,
                    duration_ms: row.get(10)?,
                })
            })
            .map_err(|e| format!("Failed to get task run events: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(events)
    }

    /// Create a task run screenshot record.
    pub fn create_task_run_screenshot(
        &self,
        input: &CreateTaskRunScreenshotInput,
    ) -> Result<String, String> {
        let conn = self.get_conn()?;
        let id = uuid::Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();

        conn.execute(
            r#"
            INSERT INTO task_run_screenshots (
                id, task_run_id, event_id, file_path, screenshot_type,
                template_name, confidence, match_location,
                width, height, file_size_bytes, created_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
            "#,
            params![
                id,
                input.task_run_id,
                input.event_id,
                input.file_path,
                input.screenshot_type,
                input.template_name,
                input.confidence,
                input.match_location,
                input.width,
                input.height,
                input.file_size_bytes,
                now,
            ],
        )
        .map_err(|e| format!("Failed to create task run screenshot: {}", e))?;

        Ok(id)
    }

    /// Get screenshots for a task run.
    pub fn get_task_run_screenshots(
        &self,
        task_run_id: &str,
        screenshot_type: Option<&str>,
    ) -> Result<Vec<TaskRunScreenshot>, String> {
        let conn = self.get_conn()?;

        let mut sql = String::from(
            r#"
            SELECT id, task_run_id, event_id, file_path, screenshot_type,
                   template_name, confidence, match_location,
                   width, height, file_size_bytes, created_at
            FROM task_run_screenshots
            WHERE task_run_id = ?1
            "#,
        );

        let mut params_vec: Vec<String> = vec![task_run_id.to_string()];

        if let Some(st) = screenshot_type {
            sql.push_str(" AND screenshot_type = ?2");
            params_vec.push(st.to_string());
        }

        sql.push_str(" ORDER BY created_at ASC");

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let params: Vec<&dyn rusqlite::ToSql> = params_vec
            .iter()
            .map(|s| s as &dyn rusqlite::ToSql)
            .collect();

        let screenshots = stmt
            .query_map(params.as_slice(), |row| {
                Ok(TaskRunScreenshot {
                    id: row.get(0)?,
                    task_run_id: row.get(1)?,
                    event_id: row.get(2)?,
                    file_path: row.get(3)?,
                    screenshot_type: row.get(4)?,
                    template_name: row.get(5)?,
                    confidence: row.get(6)?,
                    match_location: row.get(7)?,
                    width: row.get(8)?,
                    height: row.get(9)?,
                    file_size_bytes: row.get(10)?,
                    created_at: row.get(11)?,
                })
            })
            .map_err(|e| format!("Failed to get task run screenshots: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(screenshots)
    }

    /// Create a Playwright test result.
    pub fn create_task_run_playwright_result(
        &self,
        input: &CreateTaskRunPlaywrightResultInput,
    ) -> Result<String, String> {
        let conn = self.get_conn()?;
        let id = uuid::Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();

        conn.execute(
            r#"
            INSERT INTO task_run_playwright_results (
                id, task_run_id, test_name, spec_file, status, duration_ms,
                stdout, stderr, console_output, page_snapshot,
                error_message, failure_screenshot_path,
                assertions_passed, assertions_failed, created_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
            "#,
            params![
                id,
                input.task_run_id,
                input.test_name,
                input.spec_file,
                input.status,
                input.duration_ms,
                input.stdout,
                input.stderr,
                input.console_output,
                input.page_snapshot,
                input.error_message,
                input.failure_screenshot_path,
                input.assertions_passed,
                input.assertions_failed,
                now,
            ],
        )
        .map_err(|e| format!("Failed to create Playwright result: {}", e))?;

        Ok(id)
    }

    /// Get Playwright results for a task run.
    pub fn get_task_run_playwright_results(
        &self,
        task_run_id: &str,
        status_filter: Option<&str>,
    ) -> Result<Vec<TaskRunPlaywrightResult>, String> {
        let conn = self.get_conn()?;

        let mut sql = String::from(
            r#"
            SELECT id, task_run_id, test_name, spec_file, status, duration_ms,
                   stdout, stderr, console_output, page_snapshot,
                   error_message, failure_screenshot_path,
                   assertions_passed, assertions_failed, created_at
            FROM task_run_playwright_results
            WHERE task_run_id = ?1
            "#,
        );

        let mut params_vec: Vec<String> = vec![task_run_id.to_string()];

        if let Some(status) = status_filter {
            sql.push_str(" AND status = ?2");
            params_vec.push(status.to_string());
        }

        sql.push_str(" ORDER BY created_at ASC");

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let params: Vec<&dyn rusqlite::ToSql> = params_vec
            .iter()
            .map(|s| s as &dyn rusqlite::ToSql)
            .collect();

        let results = stmt
            .query_map(params.as_slice(), |row| {
                Ok(TaskRunPlaywrightResult {
                    id: row.get(0)?,
                    task_run_id: row.get(1)?,
                    test_name: row.get(2)?,
                    spec_file: row.get(3)?,
                    status: row.get(4)?,
                    duration_ms: row.get(5)?,
                    stdout: row.get(6)?,
                    stderr: row.get(7)?,
                    console_output: row.get(8)?,
                    page_snapshot: row.get(9)?,
                    error_message: row.get(10)?,
                    failure_screenshot_path: row.get(11)?,
                    assertions_passed: row.get(12)?,
                    assertions_failed: row.get(13)?,
                    created_at: row.get(14)?,
                })
            })
            .map_err(|e| format!("Failed to get Playwright results: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }

    // ========================================================================
    // Execution Spans Operations
    // ========================================================================

    /// Get execution spans with optional filtering.
    ///
    /// Supports filtering by:
    /// - execution_id: Filter by task/execution ID
    /// - name_pattern: Filter span names using SQL LIKE pattern (e.g., "workflow.%")
    /// - min_duration_ms: Filter spans with duration >= this value
    /// - limit: Maximum number of spans to return (default: 100)
    pub fn get_execution_spans(
        &self,
        execution_id: Option<&str>,
        name_pattern: Option<&str>,
        min_duration_ms: Option<i64>,
        limit: Option<u32>,
    ) -> Result<Vec<ExecutionSpan>, String> {
        let conn = self.get_conn()?;

        let mut sql = String::from(
            r#"
            SELECT id, execution_id, trace_id, span_id, parent_span_id, name,
                   start_ts, end_ts, duration_ms, attributes, success, error, created_at
            FROM execution_spans
            WHERE 1=1
            "#,
        );

        let mut param_values: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        let mut param_idx = 1;

        if let Some(exec_id) = execution_id {
            sql.push_str(&format!(" AND execution_id = ?{}", param_idx));
            param_values.push(Box::new(exec_id.to_string()));
            param_idx += 1;
        }

        if let Some(pattern) = name_pattern {
            sql.push_str(&format!(" AND name LIKE ?{}", param_idx));
            param_values.push(Box::new(pattern.to_string()));
            param_idx += 1;
        }

        if let Some(min_dur) = min_duration_ms {
            sql.push_str(&format!(" AND duration_ms >= ?{}", param_idx));
            param_values.push(Box::new(min_dur));
            // param_idx += 1; // Not needed as it's the last
        }

        sql.push_str(" ORDER BY start_ts DESC");

        let lim = limit.unwrap_or(100);
        sql.push_str(&format!(" LIMIT {}", lim));

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let params: Vec<&dyn rusqlite::ToSql> = param_values
            .iter()
            .map(|p| p.as_ref() as &dyn rusqlite::ToSql)
            .collect();

        let spans = stmt
            .query_map(params.as_slice(), |row| {
                Ok(ExecutionSpan {
                    id: row.get(0)?,
                    execution_id: row.get(1)?,
                    trace_id: row.get(2)?,
                    span_id: row.get(3)?,
                    parent_span_id: row.get(4)?,
                    name: row.get(5)?,
                    start_ts: row.get(6)?,
                    end_ts: row.get(7)?,
                    duration_ms: row.get(8)?,
                    attributes: row.get(9)?,
                    success: row.get::<_, i32>(10)? == 1,
                    error: row.get(11)?,
                    created_at: row.get(12)?,
                })
            })
            .map_err(|e| format!("Failed to get execution spans: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(spans)
    }

    // ========================================================================
    // Task Run API Request Operations
    // ========================================================================

    /// Create a task run API request record.
    pub fn create_task_run_api_request(
        &self,
        input: &CreateTaskRunApiRequestInput,
    ) -> Result<String, String> {
        let conn = self.get_conn()?;
        let id = uuid::Uuid::new_v4().to_string();

        conn.execute(
            r#"
            INSERT INTO task_run_api_requests (
                id, task_run_id, step_id, step_name,
                method, url, resolved_url, request_headers, request_body,
                status_code, status_text, response_headers, response_time_ms,
                response_body_type, response_body, response_size_bytes,
                extractions, assertions, success, error_message, created_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21)
            "#,
            params![
                id,
                input.task_run_id,
                input.step_id,
                input.step_name,
                input.method,
                input.url,
                input.resolved_url,
                input.request_headers,
                input.request_body,
                input.status_code,
                input.status_text,
                input.response_headers,
                input.response_time_ms,
                input.response_body_type,
                input.response_body,
                input.response_size_bytes,
                input.extractions,
                input.assertions,
                input.success,
                input.error_message,
                input.timestamp,
            ],
        )
        .map_err(|e| format!("Failed to create task run API request: {}", e))?;

        Ok(id)
    }

    /// Batch insert task run API requests (efficient for JSONL migration).
    pub fn batch_create_task_run_api_requests(
        &self,
        requests: &[CreateTaskRunApiRequestInput],
    ) -> Result<usize, String> {
        if requests.is_empty() {
            return Ok(0);
        }

        let conn = self.get_conn()?;
        let mut count = 0;

        for input in requests {
            let id = uuid::Uuid::new_v4().to_string();

            conn.execute(
                r#"
                INSERT INTO task_run_api_requests (
                    id, task_run_id, step_id, step_name,
                    method, url, resolved_url, request_headers, request_body,
                    status_code, status_text, response_headers, response_time_ms,
                    response_body_type, response_body, response_size_bytes,
                    extractions, assertions, success, error_message, created_at
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21)
                "#,
                params![
                    id,
                    input.task_run_id,
                    input.step_id,
                    input.step_name,
                    input.method,
                    input.url,
                    input.resolved_url,
                    input.request_headers,
                    input.request_body,
                    input.status_code,
                    input.status_text,
                    input.response_headers,
                    input.response_time_ms,
                    input.response_body_type,
                    input.response_body,
                    input.response_size_bytes,
                    input.extractions,
                    input.assertions,
                    input.success,
                    input.error_message,
                    input.timestamp,
                ],
            )
            .map_err(|e| format!("Failed to create task run API request: {}", e))?;
            count += 1;
        }

        Ok(count)
    }

    /// Get API requests for a task run.
    pub fn get_task_run_api_requests(
        &self,
        task_run_id: &str,
        success_filter: Option<bool>,
    ) -> Result<Vec<TaskRunApiRequest>, String> {
        let conn = self.get_conn()?;

        let mut sql = String::from(
            r#"
            SELECT id, task_run_id, step_id, step_name,
                   method, url, resolved_url, request_headers, request_body,
                   status_code, status_text, response_headers, response_time_ms,
                   response_body_type, response_body, response_size_bytes,
                   extractions, assertions, success, error_message, created_at
            FROM task_run_api_requests
            WHERE task_run_id = ?1
            "#,
        );

        let mut params_vec: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(task_run_id.to_string())];

        if let Some(success) = success_filter {
            sql.push_str(" AND success = ?2");
            params_vec.push(Box::new(success));
        }

        sql.push_str(" ORDER BY created_at ASC");

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let params: Vec<&dyn rusqlite::ToSql> = params_vec.iter().map(|b| b.as_ref()).collect();

        let results = stmt
            .query_map(params.as_slice(), |row| {
                Ok(TaskRunApiRequest {
                    id: row.get(0)?,
                    task_run_id: row.get(1)?,
                    step_id: row.get(2)?,
                    step_name: row.get(3)?,
                    method: row.get(4)?,
                    url: row.get(5)?,
                    resolved_url: row.get(6)?,
                    request_headers: row.get(7)?,
                    request_body: row.get(8)?,
                    status_code: row.get(9)?,
                    status_text: row.get(10)?,
                    response_headers: row.get(11)?,
                    response_time_ms: row.get(12)?,
                    response_body_type: row.get(13)?,
                    response_body: row.get(14)?,
                    response_size_bytes: row.get(15)?,
                    extractions: row.get(16)?,
                    assertions: row.get(17)?,
                    success: row.get(18)?,
                    error_message: row.get(19)?,
                    created_at: row.get(20)?,
                })
            })
            .map_err(|e| format!("Failed to get API requests: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }

    /// Delete all events for a task run (used when clearing/re-importing).
    pub fn delete_task_run_events(&self, task_run_id: &str) -> Result<usize, String> {
        let conn = self.get_conn()?;

        let rows_affected = conn
            .execute(
                "DELETE FROM task_run_events WHERE task_run_id = ?1",
                params![task_run_id],
            )
            .map_err(|e| format!("Failed to delete task run events: {}", e))?;

        Ok(rows_affected)
    }

    // ========================================================================
    // Task Run AWAS Step Operations
    // ========================================================================

    /// Create a task run AWAS step record.
    pub fn create_task_run_awas_step(
        &self,
        input: &CreateTaskRunAwasStepInput,
    ) -> Result<String, String> {
        let conn = self.get_conn()?;
        let id = uuid::Uuid::new_v4().to_string();

        conn.execute(
            r#"
            INSERT INTO task_run_awas_steps (
                id, task_run_id, step_id, step_name, step_type,
                url, action_id, parameters, response_data,
                success, error_message, duration_ms, created_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
            "#,
            params![
                id,
                input.task_run_id,
                input.step_id,
                input.step_name,
                input.step_type,
                input.url,
                input.action_id,
                input.parameters,
                input.response_data,
                input.success,
                input.error_message,
                input.duration_ms,
                input.timestamp,
            ],
        )
        .map_err(|e| format!("Failed to create task run AWAS step: {}", e))?;

        Ok(id)
    }

    /// Get AWAS steps for a task run.
    pub fn get_task_run_awas_steps(
        &self,
        task_run_id: &str,
    ) -> Result<Vec<TaskRunAwasStep>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, task_run_id, step_id, step_name, step_type,
                       url, action_id, parameters, response_data,
                       success, error_message, duration_ms, created_at
                FROM task_run_awas_steps
                WHERE task_run_id = ?1
                ORDER BY created_at ASC
                "#,
            )
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let results = stmt
            .query_map(params![task_run_id], |row| {
                Ok(TaskRunAwasStep {
                    id: row.get(0)?,
                    task_run_id: row.get(1)?,
                    step_id: row.get(2)?,
                    step_name: row.get(3)?,
                    step_type: row.get(4)?,
                    url: row.get(5)?,
                    action_id: row.get(6)?,
                    parameters: row.get(7)?,
                    response_data: row.get(8)?,
                    success: row.get(9)?,
                    error_message: row.get(10)?,
                    duration_ms: row.get(11)?,
                    created_at: row.get(12)?,
                })
            })
            .map_err(|e| format!("Failed to get AWAS steps: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }

    // ========================================================================
    // Learning System Operations
    // ========================================================================

    /// Record a task outcome for learning
    pub fn record_learning_outcome(
        &self,
        task_id: &str,
        status: &str,
        duration_secs: Option<f64>,
        iterations: Option<u32>,
        strategy: Option<&str>,
        tools_used: Option<&[String]>,
        files_modified: Option<&[String]>,
        error_type: Option<&str>,
        error_message: Option<&str>,
        feedback: Option<&serde_json::Value>,
    ) -> Result<String, String> {
        let conn = self.get_conn()?;
        let id = format!("lo-{}", uuid::Uuid::new_v4());
        let now = Utc::now().to_rfc3339();

        let tools_json = tools_used.map(|t| serde_json::to_string(t).unwrap_or_default());
        let files_json = files_modified.map(|f| serde_json::to_string(f).unwrap_or_default());
        let feedback_json = feedback.map(|f| f.to_string());

        conn.execute(
            r#"
            INSERT INTO learning_outcomes (
                id, task_id, status, duration_secs, iterations, strategy,
                tools_used, files_modified, error_type, error_message, feedback, created_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
            "#,
            params![
                id,
                task_id,
                status,
                duration_secs,
                iterations,
                strategy,
                tools_json,
                files_json,
                error_type,
                error_message,
                feedback_json,
                now
            ],
        )
        .map_err(|e| format!("Failed to record learning outcome: {}", e))?;

        Ok(id)
    }

    /// Get learning outcomes for analysis
    pub fn get_learning_outcomes(
        &self,
        limit: Option<u32>,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self.get_conn()?;
        let limit_val = limit.unwrap_or(100) as i64;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, task_id, status, duration_secs, iterations, strategy,
                       tools_used, files_modified, error_type, error_message, feedback, created_at
                FROM learning_outcomes
                ORDER BY created_at DESC
                LIMIT ?1
                "#,
            )
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let results = stmt
            .query_map(params![limit_val], |row| {
                Ok(serde_json::json!({
                    "id": row.get::<_, String>(0)?,
                    "task_id": row.get::<_, String>(1)?,
                    "status": row.get::<_, String>(2)?,
                    "duration_secs": row.get::<_, Option<f64>>(3)?,
                    "iterations": row.get::<_, Option<i64>>(4)?,
                    "strategy": row.get::<_, Option<String>>(5)?,
                    "tools_used": row.get::<_, Option<String>>(6)?.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                    "files_modified": row.get::<_, Option<String>>(7)?.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                    "error_type": row.get::<_, Option<String>>(8)?,
                    "error_message": row.get::<_, Option<String>>(9)?,
                    "feedback": row.get::<_, Option<String>>(10)?.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                    "created_at": row.get::<_, String>(11)?,
                }))
            })
            .map_err(|e| format!("Failed to get learning outcomes: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }

    /// Save or update a learning pattern
    pub fn save_learning_pattern(
        &self,
        id: &str,
        pattern_type: &str,
        description: &str,
        confidence: f64,
        occurrences: u32,
        context: Option<&serde_json::Value>,
    ) -> Result<(), String> {
        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();
        let context_json = context.map(|c| c.to_string());

        conn.execute(
            r#"
            INSERT INTO learning_patterns (id, pattern_type, description, confidence, occurrences, context, created_at, updated_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)
            ON CONFLICT(id) DO UPDATE SET
                description = ?3,
                confidence = ?4,
                occurrences = ?5,
                context = ?6,
                updated_at = ?7
            "#,
            params![id, pattern_type, description, confidence, occurrences, context_json, now],
        )
        .map_err(|e| format!("Failed to save learning pattern: {}", e))?;

        Ok(())
    }

    /// Get all learning patterns
    pub fn get_learning_patterns(&self) -> Result<Vec<serde_json::Value>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, pattern_type, description, confidence, occurrences, context, created_at, updated_at
                FROM learning_patterns
                ORDER BY confidence DESC
                "#,
            )
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let results = stmt
            .query_map([], |row| {
                Ok(serde_json::json!({
                    "id": row.get::<_, String>(0)?,
                    "pattern_type": row.get::<_, String>(1)?,
                    "description": row.get::<_, String>(2)?,
                    "confidence": row.get::<_, f64>(3)?,
                    "occurrences": row.get::<_, i64>(4)?,
                    "context": row.get::<_, Option<String>>(5)?.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                    "created_at": row.get::<_, String>(6)?,
                    "updated_at": row.get::<_, String>(7)?,
                }))
            })
            .map_err(|e| format!("Failed to get learning patterns: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }

    // ========================================================================
    // Orchestrator Checkpoint Operations
    // ========================================================================

    /// Save an orchestrator checkpoint
    pub fn save_orchestrator_checkpoint(
        &self,
        id: &str,
        task_id: &str,
        iteration: u32,
        trigger: &str,
        state: &serde_json::Value,
        name: Option<&str>,
    ) -> Result<(), String> {
        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();
        let state_json = state.to_string();

        conn.execute(
            r#"
            INSERT INTO orchestrator_checkpoints (id, task_id, iteration, trigger, state, name, created_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            "#,
            params![id, task_id, iteration, trigger, state_json, name, now],
        )
        .map_err(|e| format!("Failed to save orchestrator checkpoint: {}", e))?;

        Ok(())
    }

    /// Get checkpoints for a task
    pub fn get_orchestrator_checkpoints(
        &self,
        task_id: Option<&str>,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self.get_conn()?;

        let query = if task_id.is_some() {
            r#"
            SELECT id, task_id, iteration, trigger, state, name, created_at
            FROM orchestrator_checkpoints
            WHERE task_id = ?1
            ORDER BY iteration ASC
            "#
        } else {
            r#"
            SELECT id, task_id, iteration, trigger, state, name, created_at
            FROM orchestrator_checkpoints
            ORDER BY created_at DESC
            LIMIT 100
            "#
        };

        let mut stmt = conn
            .prepare(query)
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let results = if let Some(tid) = task_id {
            stmt.query_map(params![tid], |row| {
                Ok(serde_json::json!({
                    "id": row.get::<_, String>(0)?,
                    "task_id": row.get::<_, String>(1)?,
                    "iteration": row.get::<_, i64>(2)?,
                    "trigger": row.get::<_, String>(3)?,
                    "state": row.get::<_, String>(4)?.parse::<serde_json::Value>().ok(),
                    "name": row.get::<_, Option<String>>(5)?,
                    "created_at": row.get::<_, String>(6)?,
                }))
            })
            .map_err(|e| format!("Failed to get checkpoints: {}", e))?
            .filter_map(|r| r.ok())
            .collect()
        } else {
            stmt.query_map([], |row| {
                Ok(serde_json::json!({
                    "id": row.get::<_, String>(0)?,
                    "task_id": row.get::<_, String>(1)?,
                    "iteration": row.get::<_, i64>(2)?,
                    "trigger": row.get::<_, String>(3)?,
                    "state": row.get::<_, String>(4)?.parse::<serde_json::Value>().ok(),
                    "name": row.get::<_, Option<String>>(5)?,
                    "created_at": row.get::<_, String>(6)?,
                }))
            })
            .map_err(|e| format!("Failed to get checkpoints: {}", e))?
            .filter_map(|r| r.ok())
            .collect()
        };

        Ok(results)
    }

    /// Get a single checkpoint by ID
    pub fn get_orchestrator_checkpoint(
        &self,
        id: &str,
    ) -> Result<Option<serde_json::Value>, String> {
        let conn = self.get_conn()?;

        let result: SqliteResult<serde_json::Value> = conn.query_row(
            r#"
            SELECT id, task_id, iteration, trigger, state, name, created_at
            FROM orchestrator_checkpoints
            WHERE id = ?1
            "#,
            params![id],
            |row| {
                Ok(serde_json::json!({
                    "id": row.get::<_, String>(0)?,
                    "task_id": row.get::<_, String>(1)?,
                    "iteration": row.get::<_, i64>(2)?,
                    "trigger": row.get::<_, String>(3)?,
                    "state": row.get::<_, String>(4)?.parse::<serde_json::Value>().ok(),
                    "name": row.get::<_, Option<String>>(5)?,
                    "created_at": row.get::<_, String>(6)?,
                }))
            },
        );

        match result {
            Ok(cp) => Ok(Some(cp)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("Failed to get checkpoint: {}", e)),
        }
    }

    /// Delete a checkpoint
    pub fn delete_orchestrator_checkpoint(&self, id: &str) -> Result<bool, String> {
        let conn = self.get_conn()?;

        let affected = conn
            .execute(
                "DELETE FROM orchestrator_checkpoints WHERE id = ?1",
                params![id],
            )
            .map_err(|e| format!("Failed to delete checkpoint: {}", e))?;

        Ok(affected > 0)
    }

    /// Get all unique task IDs that have checkpoints
    pub fn get_checkpoint_task_ids(&self) -> Result<Vec<String>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare("SELECT DISTINCT task_id FROM orchestrator_checkpoints ORDER BY task_id")
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let results = stmt
            .query_map([], |row| row.get(0))
            .map_err(|e| format!("Failed to get task IDs: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }

    // ========================================================================
    // Flow Designer Operations
    // ========================================================================

    /// Save a flow definition
    pub fn save_flow(&self, flow: &serde_json::Value) -> Result<String, String> {
        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();

        let id = flow["id"].as_str().ok_or("Flow must have an id")?;
        let name = flow["name"].as_str().ok_or("Flow must have a name")?;
        let description = flow["description"].as_str();
        let steps = serde_json::to_string(&flow["steps"]).map_err(|e| e.to_string())?;
        let start_step = flow["start_step"].as_str();
        let timeout_secs = flow["timeout_secs"].as_i64().map(|v| v as i32);
        let inputs = serde_json::to_string(&flow["inputs"]).ok();
        let outputs = serde_json::to_string(&flow["outputs"]).ok();
        let tags = serde_json::to_string(&flow["tags"]).ok();
        let version = flow["version"].as_str().unwrap_or("1.0.0");

        conn.execute(
            r#"
            INSERT INTO orchestrator_flows (
                id, name, description, steps, start_step, timeout_secs,
                inputs, outputs, tags, version, created_at, updated_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?11)
            ON CONFLICT(id) DO UPDATE SET
                name = ?2,
                description = ?3,
                steps = ?4,
                start_step = ?5,
                timeout_secs = ?6,
                inputs = ?7,
                outputs = ?8,
                tags = ?9,
                version = ?10,
                updated_at = ?11
            "#,
            params![
                id,
                name,
                description,
                steps,
                start_step,
                timeout_secs,
                inputs,
                outputs,
                tags,
                version,
                now
            ],
        )
        .map_err(|e| format!("Failed to save flow: {}", e))?;

        Ok(id.to_string())
    }

    /// Get a flow by ID
    pub fn get_flow(&self, id: &str) -> Result<Option<serde_json::Value>, String> {
        let conn = self.get_conn()?;

        let result: SqliteResult<serde_json::Value> = conn.query_row(
            r#"
            SELECT id, name, description, steps, start_step, timeout_secs,
                   inputs, outputs, tags, version, created_at, updated_at
            FROM orchestrator_flows
            WHERE id = ?1
            "#,
            params![id],
            |row| {
                Ok(serde_json::json!({
                    "id": row.get::<_, String>(0)?,
                    "name": row.get::<_, String>(1)?,
                    "description": row.get::<_, Option<String>>(2)?,
                    "steps": row.get::<_, String>(3)?.parse::<serde_json::Value>().ok(),
                    "start_step": row.get::<_, Option<String>>(4)?,
                    "timeout_secs": row.get::<_, Option<i32>>(5)?,
                    "inputs": row.get::<_, Option<String>>(6)?.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                    "outputs": row.get::<_, Option<String>>(7)?.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                    "tags": row.get::<_, Option<String>>(8)?.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                    "version": row.get::<_, String>(9)?,
                    "created_at": row.get::<_, String>(10)?,
                    "updated_at": row.get::<_, String>(11)?,
                }))
            },
        );

        match result {
            Ok(flow) => Ok(Some(flow)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("Failed to get flow: {}", e)),
        }
    }

    /// List all flows (summaries)
    pub fn list_flows(&self) -> Result<Vec<serde_json::Value>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, name, description,
                       json_array_length(json_extract(steps, '$')) as step_count,
                       tags, version
                FROM orchestrator_flows
                ORDER BY updated_at DESC
                "#,
            )
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let results = stmt
            .query_map([], |row| {
                Ok(serde_json::json!({
                    "id": row.get::<_, String>(0)?,
                    "name": row.get::<_, String>(1)?,
                    "description": row.get::<_, Option<String>>(2)?,
                    "step_count": row.get::<_, Option<i64>>(3)?.unwrap_or(0),
                    "tags": row.get::<_, Option<String>>(4)?.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                    "version": row.get::<_, String>(5)?,
                }))
            })
            .map_err(|e| format!("Failed to list flows: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }

    /// Delete a flow
    pub fn delete_flow(&self, id: &str) -> Result<bool, String> {
        let conn = self.get_conn()?;

        let affected = conn
            .execute("DELETE FROM orchestrator_flows WHERE id = ?1", params![id])
            .map_err(|e| format!("Failed to delete flow: {}", e))?;

        Ok(affected > 0)
    }

    // ========================================================================
    // Flow Version History
    // ========================================================================

    /// Create a new version snapshot of a flow.
    /// Automatically increments version number.
    pub fn create_flow_version(
        &self,
        flow_id: &str,
        definition: &serde_json::Value,
        message: Option<&str>,
        created_by: Option<&str>,
    ) -> Result<serde_json::Value, String> {
        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();

        // Get the next version number for this flow
        let next_version: i32 = conn
            .query_row(
                "SELECT COALESCE(MAX(version), 0) + 1 FROM flow_versions WHERE flow_id = ?1",
                params![flow_id],
                |row| row.get(0),
            )
            .map_err(|e| format!("Failed to get next version number: {}", e))?;

        let id = format!("{}_v{}", flow_id, next_version);
        let definition_json = serde_json::to_string(definition)
            .map_err(|e| format!("Failed to serialize flow definition: {}", e))?;

        conn.execute(
            r#"
            INSERT INTO flow_versions (id, flow_id, version, definition, message, created_by, created_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            "#,
            params![id, flow_id, next_version, definition_json, message, created_by, now],
        )
        .map_err(|e| format!("Failed to create flow version: {}", e))?;

        Ok(serde_json::json!({
            "id": id,
            "flow_id": flow_id,
            "version": next_version,
            "message": message,
            "created_by": created_by,
            "created_at": now
        }))
    }

    /// List all versions of a flow.
    pub fn list_flow_versions(&self, flow_id: &str) -> Result<Vec<serde_json::Value>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, flow_id, version, message, created_by, created_at
                FROM flow_versions
                WHERE flow_id = ?1
                ORDER BY version DESC
                "#,
            )
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let results = stmt
            .query_map(params![flow_id], |row| {
                Ok(serde_json::json!({
                    "id": row.get::<_, String>(0)?,
                    "flow_id": row.get::<_, String>(1)?,
                    "version": row.get::<_, i32>(2)?,
                    "message": row.get::<_, Option<String>>(3)?,
                    "created_by": row.get::<_, Option<String>>(4)?,
                    "created_at": row.get::<_, String>(5)?,
                }))
            })
            .map_err(|e| format!("Failed to list flow versions: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }

    /// Get a specific version of a flow (full definition).
    pub fn get_flow_version(
        &self,
        flow_id: &str,
        version: i32,
    ) -> Result<Option<serde_json::Value>, String> {
        let conn = self.get_conn()?;

        let result: SqliteResult<serde_json::Value> = conn.query_row(
            r#"
            SELECT id, flow_id, version, definition, message, created_by, created_at
            FROM flow_versions
            WHERE flow_id = ?1 AND version = ?2
            "#,
            params![flow_id, version],
            |row| {
                let definition_str: String = row.get(3)?;
                let definition: serde_json::Value =
                    serde_json::from_str(&definition_str).unwrap_or(serde_json::Value::Null);

                Ok(serde_json::json!({
                    "id": row.get::<_, String>(0)?,
                    "flow_id": row.get::<_, String>(1)?,
                    "version": row.get::<_, i32>(2)?,
                    "definition": definition,
                    "message": row.get::<_, Option<String>>(4)?,
                    "created_by": row.get::<_, Option<String>>(5)?,
                    "created_at": row.get::<_, String>(6)?,
                }))
            },
        );

        match result {
            Ok(version_data) => Ok(Some(version_data)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("Failed to get flow version: {}", e)),
        }
    }

    /// Restore a flow to a specific version.
    /// Creates a new version first as backup, then updates the flow.
    pub fn restore_flow_version(
        &self,
        flow_id: &str,
        version: i32,
        created_by: Option<&str>,
    ) -> Result<serde_json::Value, String> {
        // Get the version to restore
        let version_data = self
            .get_flow_version(flow_id, version)?
            .ok_or_else(|| format!("Version {} not found for flow {}", version, flow_id))?;

        let definition = version_data["definition"].clone();

        // Get current flow for backup
        let current_flow = self.get_flow(flow_id)?;

        // Create backup version of current state before restoring
        if let Some(current) = current_flow {
            self.create_flow_version(
                flow_id,
                &current,
                Some(&format!("Backup before restoring to version {}", version)),
                created_by,
            )?;
        }

        // Update the flow with the restored definition
        self.save_flow(&definition)?;

        // Create a new version marking the restore
        let new_version = self.create_flow_version(
            flow_id,
            &definition,
            Some(&format!("Restored from version {}", version)),
            created_by,
        )?;

        Ok(serde_json::json!({
            "flow_id": flow_id,
            "restored_from_version": version,
            "new_version": new_version["version"],
            "definition": definition
        }))
    }

    /// Compare two versions of a flow.
    /// Returns the definitions of both versions for client-side diff.
    pub fn compare_flow_versions(
        &self,
        flow_id: &str,
        version1: i32,
        version2: i32,
    ) -> Result<serde_json::Value, String> {
        let v1 = self
            .get_flow_version(flow_id, version1)?
            .ok_or_else(|| format!("Version {} not found", version1))?;

        let v2 = self
            .get_flow_version(flow_id, version2)?
            .ok_or_else(|| format!("Version {} not found", version2))?;

        Ok(serde_json::json!({
            "flow_id": flow_id,
            "version1": {
                "version": version1,
                "definition": v1["definition"],
                "message": v1["message"],
                "created_at": v1["created_at"],
                "created_by": v1["created_by"]
            },
            "version2": {
                "version": version2,
                "definition": v2["definition"],
                "message": v2["message"],
                "created_at": v2["created_at"],
                "created_by": v2["created_by"]
            }
        }))
    }

    /// Delete a specific version of a flow.
    pub fn delete_flow_version(&self, flow_id: &str, version: i32) -> Result<bool, String> {
        let conn = self.get_conn()?;

        let affected = conn
            .execute(
                "DELETE FROM flow_versions WHERE flow_id = ?1 AND version = ?2",
                params![flow_id, version],
            )
            .map_err(|e| format!("Failed to delete flow version: {}", e))?;

        Ok(affected > 0)
    }

    /// Get the latest version number of a flow.
    pub fn get_latest_flow_version(&self, flow_id: &str) -> Result<Option<i32>, String> {
        let conn = self.get_conn()?;

        let result: SqliteResult<i32> = conn.query_row(
            "SELECT MAX(version) FROM flow_versions WHERE flow_id = ?1",
            params![flow_id],
            |row| row.get(0),
        );

        match result {
            Ok(version) => Ok(Some(version)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("Failed to get latest version: {}", e)),
        }
    }

    /// Save flow execution state
    pub fn save_flow_execution(&self, execution: &serde_json::Value) -> Result<(), String> {
        let conn = self.get_conn()?;

        let instance_id = execution["instance_id"]
            .as_str()
            .ok_or("Execution must have instance_id")?;
        let flow_id = execution["flow_id"]
            .as_str()
            .ok_or("Execution must have flow_id")?;
        let current_step = execution["current_step"].as_str();
        let status = execution["status"].as_str().unwrap_or("pending");
        let context = serde_json::to_string(&execution["context"]).ok();
        let history = serde_json::to_string(&execution["history"]).ok();
        let error = execution["error"].as_str();
        let default_started_at = Utc::now().to_rfc3339();
        let started_at = execution["started_at"]
            .as_str()
            .unwrap_or(&default_started_at);
        let completed_at = execution["completed_at"].as_str();

        conn.execute(
            r#"
            INSERT INTO flow_executions (
                instance_id, flow_id, current_step, status, context, history, error, started_at, completed_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
            ON CONFLICT(instance_id) DO UPDATE SET
                current_step = ?3,
                status = ?4,
                context = ?5,
                history = ?6,
                error = ?7,
                completed_at = ?9
            "#,
            params![instance_id, flow_id, current_step, status, context, history, error, started_at, completed_at],
        )
        .map_err(|e| format!("Failed to save flow execution: {}", e))?;

        Ok(())
    }

    /// Get flow execution by instance ID
    pub fn get_flow_execution(
        &self,
        instance_id: &str,
    ) -> Result<Option<serde_json::Value>, String> {
        let conn = self.get_conn()?;

        let result: SqliteResult<serde_json::Value> = conn.query_row(
            r#"
            SELECT instance_id, flow_id, current_step, status, context, history, error, started_at, completed_at
            FROM flow_executions
            WHERE instance_id = ?1
            "#,
            params![instance_id],
            |row| {
                Ok(serde_json::json!({
                    "instance_id": row.get::<_, String>(0)?,
                    "flow_id": row.get::<_, String>(1)?,
                    "current_step": row.get::<_, Option<String>>(2)?,
                    "status": row.get::<_, String>(3)?,
                    "context": row.get::<_, Option<String>>(4)?.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                    "history": row.get::<_, Option<String>>(5)?.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                    "error": row.get::<_, Option<String>>(6)?,
                    "started_at": row.get::<_, String>(7)?,
                    "completed_at": row.get::<_, Option<String>>(8)?,
                }))
            },
        );

        match result {
            Ok(exec) => Ok(Some(exec)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("Failed to get flow execution: {}", e)),
        }
    }

    /// List flow executions
    pub fn list_flow_executions(&self) -> Result<Vec<serde_json::Value>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT instance_id, flow_id, current_step, status, started_at, completed_at
                FROM flow_executions
                ORDER BY started_at DESC
                LIMIT 50
                "#,
            )
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let results = stmt
            .query_map([], |row| {
                Ok(serde_json::json!({
                    "instance_id": row.get::<_, String>(0)?,
                    "flow_id": row.get::<_, String>(1)?,
                    "current_step": row.get::<_, Option<String>>(2)?,
                    "status": row.get::<_, String>(3)?,
                    "started_at": row.get::<_, String>(4)?,
                    "completed_at": row.get::<_, Option<String>>(5)?,
                }))
            })
            .map_err(|e| format!("Failed to list flow executions: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }

    // ========================================================================
    // Enhanced Learning Queries (Filtering, Pagination, Date Ranges)
    // ========================================================================

    /// Get learning outcomes with optional filtering by status, strategy, and date.
    pub fn get_learning_outcomes_filtered(
        &self,
        status: Option<&str>,
        strategy: Option<&str>,
        since: Option<&str>,
        limit: Option<i64>,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self.get_conn()?;
        let limit_val = limit.unwrap_or(100);

        // Build dynamic WHERE clause
        let mut conditions = Vec::new();
        if status.is_some() {
            conditions.push("status = ?1");
        }
        if strategy.is_some() {
            conditions.push("strategy = ?2");
        }
        if since.is_some() {
            conditions.push("created_at >= ?3");
        }

        let where_clause = if conditions.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", conditions.join(" AND "))
        };

        let query = format!(
            r#"
            SELECT id, task_id, status, duration_secs, iterations, strategy,
                   tools_used, files_modified, error_type, error_message, feedback, created_at
            FROM learning_outcomes
            {}
            ORDER BY created_at DESC
            LIMIT ?4
            "#,
            where_clause
        );

        let mut stmt = conn
            .prepare(&query)
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let results = stmt
            .query_map(
                params![
                    status.unwrap_or(""),
                    strategy.unwrap_or(""),
                    since.unwrap_or(""),
                    limit_val
                ],
                |row| {
                    Ok(serde_json::json!({
                        "id": row.get::<_, String>(0)?,
                        "task_id": row.get::<_, String>(1)?,
                        "status": row.get::<_, String>(2)?,
                        "duration_secs": row.get::<_, Option<f64>>(3)?,
                        "iterations": row.get::<_, Option<i64>>(4)?,
                        "strategy": row.get::<_, Option<String>>(5)?,
                        "tools_used": row.get::<_, Option<String>>(6)?.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                        "files_modified": row.get::<_, Option<String>>(7)?.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                        "error_type": row.get::<_, Option<String>>(8)?,
                        "error_message": row.get::<_, Option<String>>(9)?,
                        "feedback": row.get::<_, Option<String>>(10)?.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                        "created_at": row.get::<_, String>(11)?,
                    }))
                },
            )
            .map_err(|e| format!("Failed to get learning outcomes: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }

    /// Get learning outcomes with pagination support.
    pub fn get_learning_outcomes_paginated(
        &self,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, task_id, status, duration_secs, iterations, strategy,
                       tools_used, files_modified, error_type, error_message, feedback, created_at
                FROM learning_outcomes
                ORDER BY created_at DESC
                LIMIT ?1 OFFSET ?2
                "#,
            )
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let results = stmt
            .query_map(params![limit, offset], |row| {
                Ok(serde_json::json!({
                    "id": row.get::<_, String>(0)?,
                    "task_id": row.get::<_, String>(1)?,
                    "status": row.get::<_, String>(2)?,
                    "duration_secs": row.get::<_, Option<f64>>(3)?,
                    "iterations": row.get::<_, Option<i64>>(4)?,
                    "strategy": row.get::<_, Option<String>>(5)?,
                    "tools_used": row.get::<_, Option<String>>(6)?.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                    "files_modified": row.get::<_, Option<String>>(7)?.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                    "error_type": row.get::<_, Option<String>>(8)?,
                    "error_message": row.get::<_, Option<String>>(9)?,
                    "feedback": row.get::<_, Option<String>>(10)?.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                    "created_at": row.get::<_, String>(11)?,
                }))
            })
            .map_err(|e| format!("Failed to get learning outcomes: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }

    /// Get learning statistics for a date range.
    pub fn get_learning_stats_by_date_range(
        &self,
        start: &str,
        end: &str,
    ) -> Result<serde_json::Value, String> {
        let conn = self.get_conn()?;

        // Get counts by status
        let mut status_stmt = conn
            .prepare(
                r#"
                SELECT status, COUNT(*) as count
                FROM learning_outcomes
                WHERE created_at >= ?1 AND created_at <= ?2
                GROUP BY status
                "#,
            )
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let status_counts: Vec<(String, i64)> = status_stmt
            .query_map(params![start, end], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })
            .map_err(|e| format!("Failed to get status counts: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        // Get counts by strategy
        let mut strategy_stmt = conn
            .prepare(
                r#"
                SELECT strategy, COUNT(*) as count
                FROM learning_outcomes
                WHERE created_at >= ?1 AND created_at <= ?2 AND strategy IS NOT NULL
                GROUP BY strategy
                ORDER BY count DESC
                "#,
            )
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let strategy_counts: Vec<(String, i64)> = strategy_stmt
            .query_map(params![start, end], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })
            .map_err(|e| format!("Failed to get strategy counts: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        // Get average duration and iterations
        let avg_stats: (Option<f64>, Option<f64>) = conn
            .query_row(
                r#"
                SELECT AVG(duration_secs), AVG(iterations)
                FROM learning_outcomes
                WHERE created_at >= ?1 AND created_at <= ?2
                "#,
                params![start, end],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap_or((None, None));

        // Get total count
        let total: i64 = conn
            .query_row(
                r#"
                SELECT COUNT(*)
                FROM learning_outcomes
                WHERE created_at >= ?1 AND created_at <= ?2
                "#,
                params![start, end],
                |row| row.get(0),
            )
            .unwrap_or(0);

        // Convert to JSON
        let mut status_map = serde_json::Map::new();
        for (status, count) in status_counts {
            status_map.insert(status, serde_json::json!(count));
        }

        let mut strategy_map = serde_json::Map::new();
        for (strategy, count) in strategy_counts {
            strategy_map.insert(strategy, serde_json::json!(count));
        }

        Ok(serde_json::json!({
            "total": total,
            "by_status": status_map,
            "by_strategy": strategy_map,
            "avg_duration_secs": avg_stats.0,
            "avg_iterations": avg_stats.1,
            "date_range": {
                "start": start,
                "end": end
            }
        }))
    }

    /// Get total count of learning outcomes (for pagination).
    pub fn get_learning_outcomes_count(&self) -> Result<i64, String> {
        let conn = self.get_conn()?;
        conn.query_row("SELECT COUNT(*) FROM learning_outcomes", [], |row| {
            row.get(0)
        })
        .map_err(|e| format!("Failed to get count: {}", e))
    }

    // ========================================================================
    // Enhanced Checkpoint Queries (Filtering, Pagination)
    // ========================================================================

    /// Get checkpoints with optional filtering by task ID, trigger type, and date.
    pub fn get_checkpoints_filtered(
        &self,
        task_id: Option<&str>,
        trigger: Option<&str>,
        since: Option<&str>,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self.get_conn()?;

        // Build dynamic WHERE clause
        let mut conditions = Vec::new();
        if task_id.is_some() {
            conditions.push("task_id = ?1");
        }
        if trigger.is_some() {
            conditions.push("trigger = ?2");
        }
        if since.is_some() {
            conditions.push("created_at >= ?3");
        }

        let where_clause = if conditions.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", conditions.join(" AND "))
        };

        let query = format!(
            r#"
            SELECT id, task_id, iteration, trigger, state, name, created_at
            FROM orchestrator_checkpoints
            {}
            ORDER BY created_at DESC
            LIMIT 100
            "#,
            where_clause
        );

        let mut stmt = conn
            .prepare(&query)
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let results = stmt
            .query_map(
                params![
                    task_id.unwrap_or(""),
                    trigger.unwrap_or(""),
                    since.unwrap_or("")
                ],
                |row| {
                    Ok(serde_json::json!({
                        "id": row.get::<_, String>(0)?,
                        "task_id": row.get::<_, String>(1)?,
                        "iteration": row.get::<_, i64>(2)?,
                        "trigger": row.get::<_, String>(3)?,
                        "state": row.get::<_, String>(4)?.parse::<serde_json::Value>().ok(),
                        "name": row.get::<_, Option<String>>(5)?,
                        "created_at": row.get::<_, String>(6)?,
                    }))
                },
            )
            .map_err(|e| format!("Failed to get checkpoints: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }

    /// Get checkpoints with pagination support.
    pub fn get_checkpoints_paginated(
        &self,
        task_id: Option<&str>,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self.get_conn()?;

        let query = if task_id.is_some() {
            r#"
            SELECT id, task_id, iteration, trigger, state, name, created_at
            FROM orchestrator_checkpoints
            WHERE task_id = ?1
            ORDER BY iteration ASC
            LIMIT ?2 OFFSET ?3
            "#
        } else {
            r#"
            SELECT id, task_id, iteration, trigger, state, name, created_at
            FROM orchestrator_checkpoints
            ORDER BY created_at DESC
            LIMIT ?2 OFFSET ?3
            "#
        };

        let mut stmt = conn
            .prepare(query)
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let results = if let Some(tid) = task_id {
            stmt.query_map(params![tid, limit, offset], |row| {
                Ok(serde_json::json!({
                    "id": row.get::<_, String>(0)?,
                    "task_id": row.get::<_, String>(1)?,
                    "iteration": row.get::<_, i64>(2)?,
                    "trigger": row.get::<_, String>(3)?,
                    "state": row.get::<_, String>(4)?.parse::<serde_json::Value>().ok(),
                    "name": row.get::<_, Option<String>>(5)?,
                    "created_at": row.get::<_, String>(6)?,
                }))
            })
            .map_err(|e| format!("Failed to get checkpoints: {}", e))?
            .filter_map(|r| r.ok())
            .collect()
        } else {
            stmt.query_map(params!["", limit, offset], |row| {
                Ok(serde_json::json!({
                    "id": row.get::<_, String>(0)?,
                    "task_id": row.get::<_, String>(1)?,
                    "iteration": row.get::<_, i64>(2)?,
                    "trigger": row.get::<_, String>(3)?,
                    "state": row.get::<_, String>(4)?.parse::<serde_json::Value>().ok(),
                    "name": row.get::<_, Option<String>>(5)?,
                    "created_at": row.get::<_, String>(6)?,
                }))
            })
            .map_err(|e| format!("Failed to get checkpoints: {}", e))?
            .filter_map(|r| r.ok())
            .collect()
        };

        Ok(results)
    }

    /// Get total count of checkpoints (for pagination).
    pub fn get_checkpoints_count(&self, task_id: Option<&str>) -> Result<i64, String> {
        let conn = self.get_conn()?;
        if let Some(tid) = task_id {
            conn.query_row(
                "SELECT COUNT(*) FROM orchestrator_checkpoints WHERE task_id = ?1",
                params![tid],
                |row| row.get(0),
            )
            .map_err(|e| format!("Failed to get count: {}", e))
        } else {
            conn.query_row("SELECT COUNT(*) FROM orchestrator_checkpoints", [], |row| {
                row.get(0)
            })
            .map_err(|e| format!("Failed to get count: {}", e))
        }
    }

    // ========================================================================
    // Enhanced Flow Queries (Tag Filtering, Execution Filtering)
    // ========================================================================

    /// Get flows filtered by tag.
    pub fn get_flows_by_tag(&self, tag: &str) -> Result<Vec<serde_json::Value>, String> {
        let conn = self.get_conn()?;

        // Use JSON search to find flows with the specified tag
        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, name, description,
                       json_array_length(json_extract(steps, '$')) as step_count,
                       tags, version
                FROM orchestrator_flows
                WHERE json_extract(tags, '$') LIKE ?1
                ORDER BY updated_at DESC
                "#,
            )
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let search_pattern = format!("%\"{}%", tag);
        let results = stmt
            .query_map(params![search_pattern], |row| {
                Ok(serde_json::json!({
                    "id": row.get::<_, String>(0)?,
                    "name": row.get::<_, String>(1)?,
                    "description": row.get::<_, Option<String>>(2)?,
                    "step_count": row.get::<_, Option<i64>>(3)?.unwrap_or(0),
                    "tags": row.get::<_, Option<String>>(4)?.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                    "version": row.get::<_, String>(5)?,
                }))
            })
            .map_err(|e| format!("Failed to get flows by tag: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }

    /// Get flow executions filtered by flow ID and/or status.
    pub fn get_flow_executions_filtered(
        &self,
        flow_id: Option<&str>,
        status: Option<&str>,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self.get_conn()?;

        // Build dynamic WHERE clause
        let mut conditions = Vec::new();
        if flow_id.is_some() {
            conditions.push("flow_id = ?1");
        }
        if status.is_some() {
            conditions.push("status = ?2");
        }

        let where_clause = if conditions.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", conditions.join(" AND "))
        };

        let query = format!(
            r#"
            SELECT instance_id, flow_id, current_step, status, started_at, completed_at
            FROM flow_executions
            {}
            ORDER BY started_at DESC
            LIMIT 100
            "#,
            where_clause
        );

        let mut stmt = conn
            .prepare(&query)
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let results = stmt
            .query_map(
                params![flow_id.unwrap_or(""), status.unwrap_or("")],
                |row| {
                    Ok(serde_json::json!({
                        "instance_id": row.get::<_, String>(0)?,
                        "flow_id": row.get::<_, String>(1)?,
                        "current_step": row.get::<_, Option<String>>(2)?,
                        "status": row.get::<_, String>(3)?,
                        "started_at": row.get::<_, String>(4)?,
                        "completed_at": row.get::<_, Option<String>>(5)?,
                    }))
                },
            )
            .map_err(|e| format!("Failed to get flow executions: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }

    /// Get flow executions with pagination support.
    pub fn get_flow_executions_paginated(
        &self,
        flow_id: Option<&str>,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self.get_conn()?;

        let query = if flow_id.is_some() {
            r#"
            SELECT instance_id, flow_id, current_step, status, started_at, completed_at
            FROM flow_executions
            WHERE flow_id = ?1
            ORDER BY started_at DESC
            LIMIT ?2 OFFSET ?3
            "#
        } else {
            r#"
            SELECT instance_id, flow_id, current_step, status, started_at, completed_at
            FROM flow_executions
            ORDER BY started_at DESC
            LIMIT ?2 OFFSET ?3
            "#
        };

        let mut stmt = conn
            .prepare(query)
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let results = if let Some(fid) = flow_id {
            stmt.query_map(params![fid, limit, offset], |row| {
                Ok(serde_json::json!({
                    "instance_id": row.get::<_, String>(0)?,
                    "flow_id": row.get::<_, String>(1)?,
                    "current_step": row.get::<_, Option<String>>(2)?,
                    "status": row.get::<_, String>(3)?,
                    "started_at": row.get::<_, String>(4)?,
                    "completed_at": row.get::<_, Option<String>>(5)?,
                }))
            })
            .map_err(|e| format!("Failed to get flow executions: {}", e))?
            .filter_map(|r| r.ok())
            .collect()
        } else {
            stmt.query_map(params!["", limit, offset], |row| {
                Ok(serde_json::json!({
                    "instance_id": row.get::<_, String>(0)?,
                    "flow_id": row.get::<_, String>(1)?,
                    "current_step": row.get::<_, Option<String>>(2)?,
                    "status": row.get::<_, String>(3)?,
                    "started_at": row.get::<_, String>(4)?,
                    "completed_at": row.get::<_, Option<String>>(5)?,
                }))
            })
            .map_err(|e| format!("Failed to get flow executions: {}", e))?
            .filter_map(|r| r.ok())
            .collect()
        };

        Ok(results)
    }

    /// Get total count of flow executions (for pagination).
    pub fn get_flow_executions_count(&self, flow_id: Option<&str>) -> Result<i64, String> {
        let conn = self.get_conn()?;
        if let Some(fid) = flow_id {
            conn.query_row(
                "SELECT COUNT(*) FROM flow_executions WHERE flow_id = ?1",
                params![fid],
                |row| row.get(0),
            )
            .map_err(|e| format!("Failed to get count: {}", e))
        } else {
            conn.query_row("SELECT COUNT(*) FROM flow_executions", [], |row| row.get(0))
                .map_err(|e| format!("Failed to get count: {}", e))
        }
    }

    // ========================================================================
    // Task Run with Learning Outcome Queries (for Dashboard Integration)
    // ========================================================================

    /// Get recent task runs with their learning outcomes joined.
    /// Returns task runs along with any associated learning outcome data.
    pub fn get_recent_task_runs_with_outcomes(
        &self,
        limit: u32,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT
                    t.id, t.task_name, t.prompt, t.task_type, t.status,
                    t.sessions_count, t.max_sessions, t.error_message,
                    COALESCE(t.summary, t.ai_summary) as summary,
                    t.goal_achieved, t.remaining_work,
                    t.created_at, t.updated_at, t.completed_at,
                    l.id as outcome_id, l.status as outcome_status,
                    l.duration_secs, l.iterations, l.strategy,
                    l.tools_used, l.files_modified, l.error_type, l.error_message as outcome_error,
                    l.feedback, l.created_at as outcome_created_at
                FROM task_runs t
                LEFT JOIN learning_outcomes l ON t.id = l.task_id
                ORDER BY t.updated_at DESC
                LIMIT ?1
                "#,
            )
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        let results = stmt
            .query_map(params![limit], |row| {
                // Parse tools_used JSON if present
                let tools_used: Option<serde_json::Value> = row
                    .get::<_, Option<String>>(19)?
                    .and_then(|s| serde_json::from_str(&s).ok());

                // Parse files_modified JSON if present
                let files_modified: Option<serde_json::Value> = row
                    .get::<_, Option<String>>(20)?
                    .and_then(|s| serde_json::from_str(&s).ok());

                // Parse feedback JSON if present
                let feedback: Option<serde_json::Value> = row
                    .get::<_, Option<String>>(23)?
                    .and_then(|s| serde_json::from_str(&s).ok());

                Ok(serde_json::json!({
                    "task": {
                        "id": row.get::<_, String>(0)?,
                        "task_name": row.get::<_, String>(1)?,
                        "prompt": row.get::<_, Option<String>>(2)?,
                        "task_type": row.get::<_, Option<String>>(3)?,
                        "status": row.get::<_, String>(4)?,
                        "sessions_count": row.get::<_, i64>(5)?,
                        "max_sessions": row.get::<_, Option<i64>>(6)?,
                        "error_message": row.get::<_, Option<String>>(7)?,
                        "summary": row.get::<_, Option<String>>(8)?,
                        "goal_achieved": row.get::<_, Option<i32>>(9)?.map(|v| v != 0),
                        "remaining_work": row.get::<_, Option<String>>(10)?,
                        "created_at": row.get::<_, String>(11)?,
                        "updated_at": row.get::<_, String>(12)?,
                        "completed_at": row.get::<_, Option<String>>(13)?,
                    },
                    "learning_outcome": if row.get::<_, Option<String>>(14)?.is_some() {
                        Some(serde_json::json!({
                            "id": row.get::<_, Option<String>>(14)?,
                            "status": row.get::<_, Option<String>>(15)?,
                            "duration_secs": row.get::<_, Option<f64>>(16)?,
                            "iterations": row.get::<_, Option<i64>>(17)?,
                            "strategy": row.get::<_, Option<String>>(18)?,
                            "tools_used": tools_used,
                            "files_modified": files_modified,
                            "error_type": row.get::<_, Option<String>>(21)?,
                            "error_message": row.get::<_, Option<String>>(22)?,
                            "feedback": feedback,
                            "created_at": row.get::<_, Option<String>>(24)?,
                        }))
                    } else {
                        None
                    }
                }))
            })
            .map_err(|e| format!("Failed to query task runs with outcomes: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }

    /// Get the most recent task run with checkpoints (for auto-selection in checkpoint browser).
    pub fn get_most_recent_task_with_checkpoints(&self) -> Result<Option<String>, String> {
        let conn = self.get_conn()?;

        let result: Result<String, _> = conn.query_row(
            r#"
            SELECT DISTINCT t.id
            FROM task_runs t
            INNER JOIN orchestrator_checkpoints c ON t.id = c.task_id
            ORDER BY t.updated_at DESC
            LIMIT 1
            "#,
            [],
            |row| row.get(0),
        );

        match result {
            Ok(task_id) => Ok(Some(task_id)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!(
                "Failed to get most recent task with checkpoints: {}",
                e
            )),
        }
    }

    /// Get learning statistics summary (for dashboard cards).
    pub fn get_learning_stats_summary(&self) -> Result<serde_json::Value, String> {
        let conn = self.get_conn()?;

        // Get counts by status
        let mut stmt = conn
            .prepare(
                r#"
                SELECT
                    COUNT(*) as total,
                    SUM(CASE WHEN status = 'success' THEN 1 ELSE 0 END) as success_count,
                    SUM(CASE WHEN status = 'failure' THEN 1 ELSE 0 END) as failure_count,
                    SUM(CASE WHEN status = 'partial' THEN 1 ELSE 0 END) as partial_count,
                    AVG(duration_secs) as avg_duration,
                    AVG(iterations) as avg_iterations
                FROM learning_outcomes
                "#,
            )
            .map_err(|e| format!("Failed to prepare stats query: {}", e))?;

        let stats = stmt
            .query_row([], |row| {
                let total: i64 = row.get(0)?;
                let success: i64 = row.get(1)?;
                let failure: i64 = row.get(2)?;
                let partial: i64 = row.get(3)?;
                let avg_duration: Option<f64> = row.get(4)?;
                let avg_iterations: Option<f64> = row.get(5)?;

                let success_rate = if total > 0 {
                    (success as f64 / total as f64) * 100.0
                } else {
                    0.0
                };

                Ok(serde_json::json!({
                    "total_tasks": total,
                    "success_count": success,
                    "failure_count": failure,
                    "partial_count": partial,
                    "success_rate": success_rate,
                    "avg_duration_secs": avg_duration,
                    "avg_iterations": avg_iterations,
                }))
            })
            .map_err(|e| format!("Failed to get learning stats: {}", e))?;

        Ok(stats)
    }

    // ========================================================================
    // Comprehensive Data Export (for Backup)
    // ========================================================================

    /// Get a summary of all exportable data counts.
    pub fn get_export_summary(&self) -> Result<serde_json::Value, String> {
        let conn = self.get_conn()?;

        let flows_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM orchestrator_flows", [], |row| {
                row.get(0)
            })
            .unwrap_or(0);

        let flow_executions_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM flow_executions", [], |row| row.get(0))
            .unwrap_or(0);

        let checkpoints_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM orchestrator_checkpoints", [], |row| {
                row.get(0)
            })
            .unwrap_or(0);

        let learning_outcomes_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM learning_outcomes", [], |row| {
                row.get(0)
            })
            .unwrap_or(0);

        let learning_patterns_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM learning_patterns", [], |row| {
                row.get(0)
            })
            .unwrap_or(0);

        let settings_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM settings", [], |row| row.get(0))
            .unwrap_or(0);

        let prompts_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM prompts", [], |row| row.get(0))
            .unwrap_or(0);

        let ai_workflows_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM ai_workflows", [], |row| row.get(0))
            .unwrap_or(0);

        let unified_workflows_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM unified_workflows", [], |row| {
                row.get(0)
            })
            .unwrap_or(0);

        let verification_tests_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM verification_tests", [], |row| {
                row.get(0)
            })
            .unwrap_or(0);

        let task_hooks_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM task_hooks", [], |row| row.get(0))
            .unwrap_or(0);

        let scheduled_tasks_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM scheduled_tasks", [], |row| row.get(0))
            .unwrap_or(0);

        let saved_api_requests_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM saved_api_requests", [], |row| {
                row.get(0)
            })
            .unwrap_or(0);

        let configs_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM configs", [], |row| row.get(0))
            .unwrap_or(0);

        Ok(serde_json::json!({
            "flows": flows_count,
            "flow_executions": flow_executions_count,
            "checkpoints": checkpoints_count,
            "learning_outcomes": learning_outcomes_count,
            "learning_patterns": learning_patterns_count,
            "settings": settings_count,
            "prompts": prompts_count,
            "ai_workflows": ai_workflows_count,
            "unified_workflows": unified_workflows_count,
            "verification_tests": verification_tests_count,
            "task_hooks": task_hooks_count,
            "scheduled_tasks": scheduled_tasks_count,
            "saved_api_requests": saved_api_requests_count,
            "configs": configs_count,
        }))
    }

    /// Export all flows (orchestrator_flows table).
    pub fn export_all_flows(&self) -> Result<Vec<serde_json::Value>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, name, description, definition_json, tags, version, created_at, updated_at
                FROM orchestrator_flows
                ORDER BY updated_at DESC
                "#,
            )
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let results = stmt
            .query_map([], |row| {
                let tags_str: String = row.get(4)?;
                let tags: serde_json::Value =
                    serde_json::from_str(&tags_str).unwrap_or(serde_json::json!([]));
                let definition_str: String = row.get(3)?;
                let definition: serde_json::Value =
                    serde_json::from_str(&definition_str).unwrap_or(serde_json::json!({}));

                Ok(serde_json::json!({
                    "id": row.get::<_, String>(0)?,
                    "name": row.get::<_, String>(1)?,
                    "description": row.get::<_, Option<String>>(2)?,
                    "definition": definition,
                    "tags": tags,
                    "version": row.get::<_, String>(5)?,
                    "created_at": row.get::<_, String>(6)?,
                    "updated_at": row.get::<_, String>(7)?,
                }))
            })
            .map_err(|e| format!("Failed to export flows: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }

    /// Export all flow executions (flow_executions table).
    pub fn export_all_flow_executions(&self) -> Result<Vec<serde_json::Value>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT instance_id, flow_id, current_step, context_json, status, error, step_results_json, started_at, completed_at
                FROM flow_executions
                ORDER BY started_at DESC
                "#,
            )
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let results = stmt
            .query_map([], |row| {
                let context_str: Option<String> = row.get(3)?;
                let context: serde_json::Value = context_str
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or(serde_json::json!({}));
                let step_results_str: Option<String> = row.get(6)?;
                let step_results: serde_json::Value = step_results_str
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or(serde_json::json!([]));

                Ok(serde_json::json!({
                    "instance_id": row.get::<_, String>(0)?,
                    "flow_id": row.get::<_, String>(1)?,
                    "current_step": row.get::<_, Option<String>>(2)?,
                    "context": context,
                    "status": row.get::<_, String>(4)?,
                    "error": row.get::<_, Option<String>>(5)?,
                    "step_results": step_results,
                    "started_at": row.get::<_, String>(7)?,
                    "completed_at": row.get::<_, Option<String>>(8)?,
                }))
            })
            .map_err(|e| format!("Failed to export flow executions: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }

    /// Export all orchestrator checkpoints.
    pub fn export_all_orchestrator_checkpoints(&self) -> Result<Vec<serde_json::Value>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, task_id, iteration, trigger, state, name, created_at
                FROM orchestrator_checkpoints
                ORDER BY created_at DESC
                "#,
            )
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let results = stmt
            .query_map([], |row| {
                let state_str: String = row.get(4)?;
                let state: serde_json::Value =
                    serde_json::from_str(&state_str).unwrap_or(serde_json::json!({}));

                Ok(serde_json::json!({
                    "id": row.get::<_, String>(0)?,
                    "task_id": row.get::<_, String>(1)?,
                    "iteration": row.get::<_, i64>(2)?,
                    "trigger": row.get::<_, String>(3)?,
                    "state": state,
                    "name": row.get::<_, Option<String>>(5)?,
                    "created_at": row.get::<_, String>(6)?,
                }))
            })
            .map_err(|e| format!("Failed to export checkpoints: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }

    /// Export all settings.
    pub fn export_all_settings(&self) -> Result<Vec<serde_json::Value>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare("SELECT key, value, updated_at FROM settings ORDER BY key")
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let results = stmt
            .query_map([], |row| {
                let value_str: String = row.get(1)?;
                let value: serde_json::Value =
                    serde_json::from_str(&value_str).unwrap_or(serde_json::Value::Null);

                Ok(serde_json::json!({
                    "key": row.get::<_, String>(0)?,
                    "value": value,
                    "updated_at": row.get::<_, String>(2)?,
                }))
            })
            .map_err(|e| format!("Failed to export settings: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }

    /// Export all prompts.
    pub fn export_all_prompts(&self) -> Result<Vec<serde_json::Value>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, name, category, content, variables, created_at, updated_at
                FROM prompts
                ORDER BY updated_at DESC
                "#,
            )
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let results = stmt
            .query_map([], |row| {
                let vars_str: String = row.get(4)?;
                let variables: serde_json::Value =
                    serde_json::from_str(&vars_str).unwrap_or(serde_json::json!([]));

                Ok(serde_json::json!({
                    "id": row.get::<_, String>(0)?,
                    "name": row.get::<_, String>(1)?,
                    "category": row.get::<_, Option<String>>(2)?,
                    "content": row.get::<_, String>(3)?,
                    "variables": variables,
                    "created_at": row.get::<_, String>(5)?,
                    "updated_at": row.get::<_, String>(6)?,
                }))
            })
            .map_err(|e| format!("Failed to export prompts: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }

    /// Export all AI workflows.
    pub fn export_all_ai_workflows(&self) -> Result<Vec<serde_json::Value>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, name, description, config, created_at, updated_at
                FROM ai_workflows
                ORDER BY updated_at DESC
                "#,
            )
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let results = stmt
            .query_map([], |row| {
                let config_str: String = row.get(3)?;
                let config: serde_json::Value =
                    serde_json::from_str(&config_str).unwrap_or(serde_json::json!({}));

                Ok(serde_json::json!({
                    "id": row.get::<_, String>(0)?,
                    "name": row.get::<_, String>(1)?,
                    "description": row.get::<_, Option<String>>(2)?,
                    "config": config,
                    "created_at": row.get::<_, String>(4)?,
                    "updated_at": row.get::<_, String>(5)?,
                }))
            })
            .map_err(|e| format!("Failed to export AI workflows: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }

    /// Export all unified workflows.
    pub fn export_all_unified_workflows(&self) -> Result<Vec<serde_json::Value>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, name, description, category, tags, setup_steps, verification_steps,
                       agentic_steps, max_iterations, provider, model, created_at, updated_at,
                       completion_steps, skip_ai_summary, timeout_seconds,
                       log_watch_enabled, health_check_enabled, health_check_urls,
                       preflight_check_enabled, log_source_selection, context_ids,
                       disabled_context_ids, auto_include_contexts, prompt_template,
                       generated_by_task_run_id, enable_sweep, max_sweep_iterations,
                       stages, stop_on_failure, reflection_mode, sync_pending, example_status,
                       model_overrides, approval_gate, completion_prompts_first, is_favorite,
                       dependency_graph, cost_annotations, quality_report, constraint_overrides
                FROM unified_workflows
                ORDER BY updated_at DESC
                "#,
            )
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let results = stmt
            .query_map([], |row| {
                // Parse JSON text columns
                let tags_str: String = row.get(4)?;
                let tags: serde_json::Value =
                    serde_json::from_str(&tags_str).unwrap_or(serde_json::json!([]));
                let setup_str: String = row.get(5)?;
                let setup: serde_json::Value =
                    serde_json::from_str(&setup_str).unwrap_or(serde_json::json!([]));
                let verif_str: String = row.get(6)?;
                let verification: serde_json::Value =
                    serde_json::from_str(&verif_str).unwrap_or(serde_json::json!([]));
                let agent_str: String = row.get(7)?;
                let agentic: serde_json::Value =
                    serde_json::from_str(&agent_str).unwrap_or(serde_json::json!([]));

                // Post-v19 JSON text columns
                let completion_str: Option<String> = row.get(13)?;
                let completion: serde_json::Value = completion_str
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or(serde_json::json!([]));
                let health_check_urls_str: Option<String> = row.get(18)?;
                let health_check_urls: serde_json::Value = health_check_urls_str
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or(serde_json::json!([]));
                let log_source_str: Option<String> = row.get(20)?;
                let log_source: serde_json::Value = log_source_str
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or(serde_json::json!("default"));
                let context_ids_str: Option<String> = row.get(21)?;
                let context_ids: serde_json::Value = context_ids_str
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or(serde_json::json!([]));
                let disabled_context_ids_str: Option<String> = row.get(22)?;
                let disabled_context_ids: serde_json::Value = disabled_context_ids_str
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or(serde_json::json!([]));
                let stages_str: Option<String> = row.get(28)?;
                let stages: serde_json::Value = stages_str
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or(serde_json::json!([]));
                let model_overrides_str: Option<String> = row.get(33)?;
                let model_overrides: serde_json::Value = model_overrides_str
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or(serde_json::json!({}));

                Ok(serde_json::json!({
                    "id": row.get::<_, String>(0)?,
                    "name": row.get::<_, String>(1)?,
                    "description": row.get::<_, Option<String>>(2)?,
                    "category": row.get::<_, Option<String>>(3)?,
                    "tags": tags,
                    "setup_steps": setup,
                    "verification_steps": verification,
                    "agentic_steps": agentic,
                    "max_iterations": row.get::<_, Option<i64>>(8)?,
                    "provider": row.get::<_, Option<String>>(9)?,
                    "model": row.get::<_, Option<String>>(10)?,
                    "created_at": row.get::<_, String>(11)?,
                    "updated_at": row.get::<_, String>(12)?,
                    "completion_steps": completion,
                    "skip_ai_summary": row.get::<_, Option<bool>>(14)?,
                    "timeout_seconds": row.get::<_, Option<i64>>(15)?,
                    "log_watch_enabled": row.get::<_, Option<i64>>(16)?,
                    "health_check_enabled": row.get::<_, Option<i64>>(17)?,
                    "health_check_urls": health_check_urls,
                    "preflight_check_enabled": row.get::<_, Option<i64>>(19)?,
                    "log_source_selection": log_source,
                    "context_ids": context_ids,
                    "disabled_context_ids": disabled_context_ids,
                    "auto_include_contexts": row.get::<_, Option<i64>>(23)?,
                    "prompt_template": row.get::<_, Option<String>>(24)?,
                    "generated_by_task_run_id": row.get::<_, Option<String>>(25)?,
                    "enable_sweep": row.get::<_, Option<i64>>(26)?,
                    "max_sweep_iterations": row.get::<_, Option<i64>>(27)?,
                    "stages": stages,
                    "stop_on_failure": row.get::<_, Option<i64>>(29)?,
                    "reflection_mode": row.get::<_, Option<i64>>(30)?,
                    "sync_pending": row.get::<_, Option<i64>>(31)?,
                    "example_status": row.get::<_, Option<String>>(32)?,
                    "model_overrides": model_overrides,
                    "approval_gate": row.get::<_, Option<i64>>(34)?,
                    "completion_prompts_first": row.get::<_, Option<i64>>(35)?,
                    "is_favorite": row.get::<_, Option<i64>>(36)?,
                    "dependency_graph": row.get::<_, Option<String>>(37)?.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                    "cost_annotations": row.get::<_, Option<String>>(38)?.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                    "quality_report": row.get::<_, Option<String>>(39)?.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                    "constraint_overrides": row.get::<_, Option<String>>(40)?.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()).unwrap_or(serde_json::json!({})),
                }))
            })
            .map_err(|e| format!("Failed to export unified workflows: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }

    /// Export all verification tests.
    pub fn export_all_verification_tests(&self) -> Result<Vec<serde_json::Value>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, name, description, test_type, category, playwright_code, vision_config,
                       python_code, repo_test_config, success_criteria, config, timeout_seconds,
                       is_critical, enabled, ai_generated, ai_generation_prompt, tags,
                       source_file, last_exported_at, created_at, updated_at
                FROM verification_tests
                ORDER BY updated_at DESC
                "#,
            )
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let results = stmt
            .query_map([], |row| {
                let vision_str: Option<String> = row.get(6)?;
                let vision: serde_json::Value = vision_str
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or(serde_json::Value::Null);
                let repo_str: Option<String> = row.get(8)?;
                let repo: serde_json::Value = repo_str
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or(serde_json::Value::Null);
                let config_str: String = row.get(10)?;
                let config: serde_json::Value =
                    serde_json::from_str(&config_str).unwrap_or(serde_json::json!({}));
                let tags_str: String = row.get(16)?;
                let tags: serde_json::Value =
                    serde_json::from_str(&tags_str).unwrap_or(serde_json::json!([]));

                Ok(serde_json::json!({
                    "id": row.get::<_, String>(0)?,
                    "name": row.get::<_, String>(1)?,
                    "description": row.get::<_, Option<String>>(2)?,
                    "test_type": row.get::<_, String>(3)?,
                    "category": row.get::<_, Option<String>>(4)?,
                    "playwright_code": row.get::<_, Option<String>>(5)?,
                    "vision_config": vision,
                    "python_code": row.get::<_, Option<String>>(7)?,
                    "repo_test_config": repo,
                    "success_criteria": row.get::<_, Option<String>>(9)?,
                    "config": config,
                    "timeout_seconds": row.get::<_, i64>(11)?,
                    "is_critical": row.get::<_, bool>(12)?,
                    "enabled": row.get::<_, bool>(13)?,
                    "ai_generated": row.get::<_, bool>(14)?,
                    "ai_generation_prompt": row.get::<_, Option<String>>(15)?,
                    "tags": tags,
                    "source_file": row.get::<_, Option<String>>(17)?,
                    "last_exported_at": row.get::<_, Option<String>>(18)?,
                    "created_at": row.get::<_, String>(19)?,
                    "updated_at": row.get::<_, String>(20)?,
                }))
            })
            .map_err(|e| format!("Failed to export verification tests: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }

    /// Export all task hooks.
    pub fn export_all_task_hooks(&self) -> Result<Vec<serde_json::Value>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, name, description, trigger, action_type, action_config,
                       enabled, execution_order, continue_on_failure, conditions,
                       task_run_id, created_at, updated_at
                FROM task_hooks
                ORDER BY updated_at DESC
                "#,
            )
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let results = stmt
            .query_map([], |row| {
                let action_str: String = row.get(5)?;
                let action_config: serde_json::Value =
                    serde_json::from_str(&action_str).unwrap_or(serde_json::json!({}));
                let cond_str: String = row.get(9)?;
                let conditions: serde_json::Value =
                    serde_json::from_str(&cond_str).unwrap_or(serde_json::json!([]));

                Ok(serde_json::json!({
                    "id": row.get::<_, String>(0)?,
                    "name": row.get::<_, String>(1)?,
                    "description": row.get::<_, Option<String>>(2)?,
                    "trigger": row.get::<_, String>(3)?,
                    "action_type": row.get::<_, String>(4)?,
                    "action_config": action_config,
                    "enabled": row.get::<_, bool>(6)?,
                    "execution_order": row.get::<_, i64>(7)?,
                    "continue_on_failure": row.get::<_, bool>(8)?,
                    "conditions": conditions,
                    "task_run_id": row.get::<_, Option<String>>(10)?,
                    "created_at": row.get::<_, String>(11)?,
                    "updated_at": row.get::<_, String>(12)?,
                }))
            })
            .map_err(|e| format!("Failed to export task hooks: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }

    /// Export all scheduled tasks.
    pub fn export_all_scheduled_tasks(&self) -> Result<Vec<serde_json::Value>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, name, description, enabled, schedule_type, schedule_value,
                       task_config, skip_if_completed, auto_fix_on_failure, success_criteria,
                       created_at, modified_at, next_run, last_run_id
                FROM scheduled_tasks
                ORDER BY modified_at DESC
                "#,
            )
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let results = stmt
            .query_map([], |row| {
                let config_str: String = row.get(6)?;
                let task_config: serde_json::Value =
                    serde_json::from_str(&config_str).unwrap_or(serde_json::json!({}));

                Ok(serde_json::json!({
                    "id": row.get::<_, String>(0)?,
                    "name": row.get::<_, String>(1)?,
                    "description": row.get::<_, Option<String>>(2)?,
                    "enabled": row.get::<_, bool>(3)?,
                    "schedule_type": row.get::<_, String>(4)?,
                    "schedule_value": row.get::<_, String>(5)?,
                    "task_config": task_config,
                    "skip_if_completed": row.get::<_, bool>(7)?,
                    "auto_fix_on_failure": row.get::<_, bool>(8)?,
                    "success_criteria": row.get::<_, Option<String>>(9)?,
                    "created_at": row.get::<_, String>(10)?,
                    "modified_at": row.get::<_, String>(11)?,
                    "next_run": row.get::<_, Option<String>>(12)?,
                    "last_run_id": row.get::<_, Option<String>>(13)?,
                }))
            })
            .map_err(|e| format!("Failed to export scheduled tasks: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }

    /// Export all saved API requests.
    pub fn export_all_saved_api_requests(&self) -> Result<Vec<serde_json::Value>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, name, description, category, tags, method, url, headers,
                       body, body_content_type, timeout_ms, follow_redirects,
                       variable_extractions, assertions, credential_id, created_at, updated_at
                FROM saved_api_requests
                ORDER BY updated_at DESC
                "#,
            )
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let results = stmt
            .query_map([], |row| {
                let tags_str: String = row.get(4)?;
                let tags: serde_json::Value =
                    serde_json::from_str(&tags_str).unwrap_or(serde_json::json!([]));
                let headers_str: String = row.get(7)?;
                let headers: serde_json::Value =
                    serde_json::from_str(&headers_str).unwrap_or(serde_json::json!({}));
                let extractions_str: String = row.get(12)?;
                let extractions: serde_json::Value =
                    serde_json::from_str(&extractions_str).unwrap_or(serde_json::json!([]));
                let assertions_str: String = row.get(13)?;
                let assertions: serde_json::Value =
                    serde_json::from_str(&assertions_str).unwrap_or(serde_json::json!([]));

                Ok(serde_json::json!({
                    "id": row.get::<_, String>(0)?,
                    "name": row.get::<_, String>(1)?,
                    "description": row.get::<_, Option<String>>(2)?,
                    "category": row.get::<_, Option<String>>(3)?,
                    "tags": tags,
                    "method": row.get::<_, String>(5)?,
                    "url": row.get::<_, String>(6)?,
                    "headers": headers,
                    "body": row.get::<_, Option<String>>(8)?,
                    "body_content_type": row.get::<_, Option<String>>(9)?,
                    "timeout_ms": row.get::<_, Option<i64>>(10)?,
                    "follow_redirects": row.get::<_, bool>(11)?,
                    "variable_extractions": extractions,
                    "assertions": assertions,
                    "credential_id": row.get::<_, Option<String>>(14)?,
                    "created_at": row.get::<_, String>(15)?,
                    "updated_at": row.get::<_, String>(16)?,
                }))
            })
            .map_err(|e| format!("Failed to export saved API requests: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }

    /// Export all configs.
    pub fn export_all_configs(&self) -> Result<Vec<serde_json::Value>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, name, config_json, source_type, source_path, created_at, updated_at
                FROM configs
                ORDER BY updated_at DESC
                "#,
            )
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let results = stmt
            .query_map([], |row| {
                let config_str: String = row.get(2)?;
                let config: serde_json::Value =
                    serde_json::from_str(&config_str).unwrap_or(serde_json::json!({}));

                Ok(serde_json::json!({
                    "id": row.get::<_, String>(0)?,
                    "name": row.get::<_, String>(1)?,
                    "config": config,
                    "source_type": row.get::<_, String>(3)?,
                    "source_path": row.get::<_, Option<String>>(4)?,
                    "created_at": row.get::<_, String>(5)?,
                    "updated_at": row.get::<_, String>(6)?,
                }))
            })
            .map_err(|e| format!("Failed to export configs: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }

    /// Import flows (with conflict handling).
    pub fn import_flows(
        &self,
        flows: &[serde_json::Value],
        conflict_mode: &str,
    ) -> Result<ImportResult, String> {
        let conn = self.get_conn()?;
        let mut imported = 0;
        let mut skipped = 0;
        let mut errors = Vec::new();
        let now = Utc::now().to_rfc3339();

        for flow in flows {
            let id = flow["id"].as_str().unwrap_or("");
            if id.is_empty() {
                errors.push("Flow missing ID".to_string());
                continue;
            }

            // Check if exists
            let exists: bool = conn
                .query_row(
                    "SELECT 1 FROM orchestrator_flows WHERE id = ?1",
                    params![id],
                    |_| Ok(true),
                )
                .unwrap_or(false);

            if exists && conflict_mode == "skip" {
                skipped += 1;
                continue;
            }

            let name = flow["name"].as_str().unwrap_or("Unnamed");
            let description = flow["description"].as_str();
            let definition = serde_json::to_string(&flow["definition"]).unwrap_or("{}".to_string());
            let tags = serde_json::to_string(&flow["tags"]).unwrap_or("[]".to_string());
            let version = flow["version"].as_str().unwrap_or("1.0.0");

            let result = conn.execute(
                r#"
                INSERT OR REPLACE INTO orchestrator_flows (id, name, description, definition_json, tags, version, created_at, updated_at)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                "#,
                params![id, name, description, definition, tags, version, now, now],
            );

            match result {
                Ok(_) => imported += 1,
                Err(e) => errors.push(format!("Failed to import flow {}: {}", id, e)),
            }
        }

        Ok(ImportResult {
            imported,
            skipped,
            errors,
        })
    }

    /// Import prompts (with conflict handling).
    pub fn import_prompts(
        &self,
        prompts: &[serde_json::Value],
        conflict_mode: &str,
    ) -> Result<ImportResult, String> {
        let conn = self.get_conn()?;
        let mut imported = 0;
        let mut skipped = 0;
        let mut errors = Vec::new();
        let now = Utc::now().to_rfc3339();

        for prompt in prompts {
            let id = prompt["id"].as_str().unwrap_or("");
            if id.is_empty() {
                errors.push("Prompt missing ID".to_string());
                continue;
            }

            let exists: bool = conn
                .query_row("SELECT 1 FROM prompts WHERE id = ?1", params![id], |_| {
                    Ok(true)
                })
                .unwrap_or(false);

            if exists && conflict_mode == "skip" {
                skipped += 1;
                continue;
            }

            let name = prompt["name"].as_str().unwrap_or("Unnamed");
            let category = prompt["category"].as_str();
            let content = prompt["content"].as_str().unwrap_or("");
            let variables = serde_json::to_string(&prompt["variables"]).unwrap_or("[]".to_string());

            let result = conn.execute(
                r#"
                INSERT OR REPLACE INTO prompts (id, name, category, content, variables, created_at, updated_at)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                "#,
                params![id, name, category, content, variables, now, now],
            );

            match result {
                Ok(_) => imported += 1,
                Err(e) => errors.push(format!("Failed to import prompt {}: {}", id, e)),
            }
        }

        Ok(ImportResult {
            imported,
            skipped,
            errors,
        })
    }

    /// Import settings (with conflict handling).
    pub fn import_settings(
        &self,
        settings: &[serde_json::Value],
        conflict_mode: &str,
    ) -> Result<ImportResult, String> {
        let conn = self.get_conn()?;
        let mut imported = 0;
        let mut skipped = 0;
        let mut errors = Vec::new();
        let now = Utc::now().to_rfc3339();

        for setting in settings {
            let key = setting["key"].as_str().unwrap_or("");
            if key.is_empty() {
                errors.push("Setting missing key".to_string());
                continue;
            }

            let exists: bool = conn
                .query_row(
                    "SELECT 1 FROM settings WHERE key = ?1",
                    params![key],
                    |_| Ok(true),
                )
                .unwrap_or(false);

            if exists && conflict_mode == "skip" {
                skipped += 1;
                continue;
            }

            let value = serde_json::to_string(&setting["value"]).unwrap_or("null".to_string());

            let result = conn.execute(
                "INSERT OR REPLACE INTO settings (key, value, updated_at) VALUES (?1, ?2, ?3)",
                params![key, value, now],
            );

            match result {
                Ok(_) => imported += 1,
                Err(e) => errors.push(format!("Failed to import setting {}: {}", key, e)),
            }
        }

        Ok(ImportResult {
            imported,
            skipped,
            errors,
        })
    }

    /// Import unified workflows (with conflict handling).
    pub fn import_unified_workflows(
        &self,
        workflows: &[serde_json::Value],
        conflict_mode: &str,
    ) -> Result<ImportResult, String> {
        let conn = self.get_conn()?;
        let mut imported = 0;
        let mut skipped = 0;
        let mut errors = Vec::new();
        let now = Utc::now().to_rfc3339();

        for workflow in workflows {
            let id = workflow["id"].as_str().unwrap_or("");
            if id.is_empty() {
                errors.push("Workflow missing ID".to_string());
                continue;
            }

            let exists: bool = conn
                .query_row(
                    "SELECT 1 FROM unified_workflows WHERE id = ?1",
                    params![id],
                    |_| Ok(true),
                )
                .unwrap_or(false);

            if exists && conflict_mode == "skip" {
                skipped += 1;
                continue;
            }

            let name = workflow["name"].as_str().unwrap_or("Unnamed");
            let description = workflow["description"].as_str();
            let category = workflow["category"].as_str();
            let tags = serde_json::to_string(&workflow["tags"]).unwrap_or("[]".to_string());
            let setup = serde_json::to_string(&workflow["setup_steps"]).unwrap_or("[]".to_string());
            let verif =
                serde_json::to_string(&workflow["verification_steps"]).unwrap_or("[]".to_string());
            let agent =
                serde_json::to_string(&workflow["agentic_steps"]).unwrap_or("[]".to_string());
            let max_iter = workflow["max_iterations"].as_i64();
            let provider = workflow["provider"].as_str();
            let model = workflow["model"].as_str();

            // Post-v19 columns
            let completion =
                serde_json::to_string(&workflow["completion_steps"]).unwrap_or("[]".to_string());
            let skip_ai_summary = workflow["skip_ai_summary"].as_bool().unwrap_or(false);
            let timeout_seconds = workflow["timeout_seconds"].as_i64();
            let log_watch_enabled = workflow["log_watch_enabled"].as_i64().unwrap_or(1);
            let health_check_enabled = workflow["health_check_enabled"].as_i64().unwrap_or(1);
            let health_check_urls =
                serde_json::to_string(&workflow["health_check_urls"]).unwrap_or("[]".to_string());
            let preflight_check_enabled = workflow["preflight_check_enabled"].as_i64().unwrap_or(1);
            let log_source_selection = serde_json::to_string(&workflow["log_source_selection"])
                .unwrap_or("\"default\"".to_string());
            let context_ids =
                serde_json::to_string(&workflow["context_ids"]).unwrap_or("[]".to_string());
            let disabled_context_ids = serde_json::to_string(&workflow["disabled_context_ids"])
                .unwrap_or("[]".to_string());
            let auto_include_contexts = workflow["auto_include_contexts"].as_i64().unwrap_or(1);
            let prompt_template = workflow["prompt_template"].as_str();
            let generated_by_task_run_id = workflow["generated_by_task_run_id"].as_str();
            let enable_sweep = workflow["enable_sweep"].as_i64().unwrap_or(0);
            let max_sweep_iterations = workflow["max_sweep_iterations"].as_i64().unwrap_or(5);
            let stages = serde_json::to_string(&workflow["stages"]).unwrap_or("[]".to_string());
            let stop_on_failure = workflow["stop_on_failure"].as_i64().unwrap_or(0);
            let approval_gate = workflow["approval_gate"].as_i64().unwrap_or(0);
            let reflection_mode = workflow["reflection_mode"].as_i64().unwrap_or(1);
            let completion_prompts_first =
                workflow["completion_prompts_first"].as_i64().unwrap_or(0);
            let is_favorite = workflow["is_favorite"].as_i64().unwrap_or(0);
            let sync_pending = workflow["sync_pending"].as_i64().unwrap_or(0);
            let example_status = workflow["example_status"].as_str().unwrap_or("pending");
            let constraint_overrides = serde_json::to_string(&workflow["constraint_overrides"])
                .unwrap_or_else(|_| "{}".to_string());

            let result = conn.execute(
                r#"
                INSERT OR REPLACE INTO unified_workflows (
                    id, name, description, category, tags, setup_steps, verification_steps,
                    agentic_steps, max_iterations, provider, model, created_at, updated_at,
                    completion_steps, skip_ai_summary, timeout_seconds,
                    log_watch_enabled, health_check_enabled, health_check_urls,
                    preflight_check_enabled, log_source_selection, context_ids,
                    disabled_context_ids, auto_include_contexts, prompt_template,
                    generated_by_task_run_id, enable_sweep, max_sweep_iterations,
                    stages, stop_on_failure, approval_gate, reflection_mode, completion_prompts_first,
                    is_favorite, sync_pending, example_status, constraint_overrides
                )
                VALUES (
                    ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
                    ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25,
                    ?26, ?27, ?28, ?29, ?30, ?31, ?32, ?33, ?34, ?35, ?36, ?37
                )
                "#,
                params![
                    id,
                    name,
                    description,
                    category,
                    tags,
                    setup,
                    verif,
                    agent,
                    max_iter,
                    provider,
                    model,
                    now,
                    now,
                    completion,
                    skip_ai_summary,
                    timeout_seconds,
                    log_watch_enabled,
                    health_check_enabled,
                    health_check_urls,
                    preflight_check_enabled,
                    log_source_selection,
                    context_ids,
                    disabled_context_ids,
                    auto_include_contexts,
                    prompt_template,
                    generated_by_task_run_id,
                    enable_sweep,
                    max_sweep_iterations,
                    stages,
                    stop_on_failure,
                    approval_gate,
                    reflection_mode,
                    completion_prompts_first,
                    is_favorite,
                    sync_pending,
                    example_status,
                    constraint_overrides
                ],
            );

            match result {
                Ok(_) => imported += 1,
                Err(e) => errors.push(format!("Failed to import workflow {}: {}", id, e)),
            }
        }

        Ok(ImportResult {
            imported,
            skipped,
            errors,
        })
    }

    /// Import learning outcomes (with conflict handling).
    pub fn import_learning_outcomes(
        &self,
        outcomes: &[serde_json::Value],
        conflict_mode: &str,
    ) -> Result<ImportResult, String> {
        let conn = self.get_conn()?;
        let mut imported = 0;
        let mut skipped = 0;
        let mut errors = Vec::new();

        for outcome in outcomes {
            let id = outcome["id"].as_i64();
            let task_id = outcome["task_id"].as_str().unwrap_or("");

            if task_id.is_empty() {
                errors.push("Learning outcome missing task_id".to_string());
                continue;
            }

            // For learning outcomes, check by task_id since id is auto-generated
            let exists: bool = if let Some(outcome_id) = id {
                conn.query_row(
                    "SELECT 1 FROM learning_outcomes WHERE id = ?1",
                    params![outcome_id],
                    |_| Ok(true),
                )
                .unwrap_or(false)
            } else {
                false
            };

            if exists && conflict_mode == "skip" {
                skipped += 1;
                continue;
            }

            let status = outcome["status"].as_str().unwrap_or("unknown");
            let duration = outcome["duration_secs"].as_f64();
            let iterations = outcome["iterations"].as_i64().map(|i| i as i32);
            let strategy = outcome["strategy"].as_str();
            let tools_json = serde_json::to_string(&outcome["tools_used"]).ok();
            let agents_json = serde_json::to_string(&outcome["agents_involved"]).ok();
            let error_type = outcome["error_type"].as_str();
            let error_msg = outcome["error_message"].as_str();
            let feedback_json = serde_json::to_string(&outcome["feedback"]).ok();

            let result = conn.execute(
                r#"
                INSERT INTO learning_outcomes (task_id, status, duration_secs, iterations, strategy, tools_used, agents_involved, error_type, error_message, feedback, created_at)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, datetime('now'))
                "#,
                params![task_id, status, duration, iterations, strategy, tools_json, agents_json, error_type, error_msg, feedback_json],
            );

            match result {
                Ok(_) => imported += 1,
                Err(e) => errors.push(format!("Failed to import learning outcome: {}", e)),
            }
        }

        Ok(ImportResult {
            imported,
            skipped,
            errors,
        })
    }

    /// Import learning patterns (with conflict handling).
    pub fn import_learning_patterns(
        &self,
        patterns: &[serde_json::Value],
        conflict_mode: &str,
    ) -> Result<ImportResult, String> {
        let conn = self.get_conn()?;
        let mut imported = 0;
        let mut skipped = 0;
        let mut errors = Vec::new();
        let now = Utc::now().to_rfc3339();

        for pattern in patterns {
            let id = pattern["id"].as_str().unwrap_or("");
            if id.is_empty() {
                errors.push("Pattern missing ID".to_string());
                continue;
            }

            let exists: bool = conn
                .query_row(
                    "SELECT 1 FROM learning_patterns WHERE id = ?1",
                    params![id],
                    |_| Ok(true),
                )
                .unwrap_or(false);

            if exists && conflict_mode == "skip" {
                skipped += 1;
                continue;
            }

            let pattern_type = pattern["pattern_type"].as_str().unwrap_or("unknown");
            let description = pattern["description"].as_str().unwrap_or("");
            let confidence = pattern["confidence"].as_f64().unwrap_or(0.0);
            let occurrences = pattern["occurrences"].as_i64().unwrap_or(0) as i32;
            let context = serde_json::to_string(&pattern["context"]).ok();

            let result = conn.execute(
                r#"
                INSERT OR REPLACE INTO learning_patterns (id, pattern_type, description, confidence, occurrences, context, created_at)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                "#,
                params![id, pattern_type, description, confidence, occurrences, context, now],
            );

            match result {
                Ok(_) => imported += 1,
                Err(e) => errors.push(format!("Failed to import pattern {}: {}", id, e)),
            }
        }

        Ok(ImportResult {
            imported,
            skipped,
            errors,
        })
    }

    // ==================== Mobile Development Feedback ====================

    /// Helper to map a row to MobileState.
    fn row_to_mobile_state(row: &rusqlite::Row) -> SqliteResult<MobileState> {
        Ok(MobileState {
            id: row.get(0)?,
            task_run_id: row.get(1)?,
            timestamp: row.get(2)?,
            device_id: row.get(3).ok(),
            device_type: row.get(4).ok(),
            device_model: row.get(5).ok(),
            app_package: row.get(6).ok(),
            app_activity: row.get(7).ok(),
            app_state: row.get(8).ok(),
            metro_connected: row.get::<_, i32>(9)? != 0,
            bundle_status: row.get(10).ok(),
            last_reload_type: row.get(11).ok(),
            last_reload_time: row.get(12).ok(),
            screenshot_path: row.get(13).ok(),
            logcat_path: row.get(14).ok(),
            has_errors: row.get::<_, i32>(15)? != 0,
            error_summary: row.get(16).ok(),
            created_at: row.get(17)?,
        })
    }

    /// Helper to map a row to MobileLog.
    fn row_to_mobile_log(row: &rusqlite::Row) -> SqliteResult<MobileLog> {
        Ok(MobileLog {
            id: row.get(0)?,
            task_run_id: row.get(1)?,
            mobile_state_id: row.get(2).ok(),
            log_source: row.get(3)?,
            log_level: row.get(4).ok(),
            log_tag: row.get(5).ok(),
            message: row.get(6)?,
            raw_line: row.get(7).ok(),
            data: row.get(8).ok(),
            error_type: row.get(9).ok(),
            error_code: row.get(10).ok(),
            stack_trace: row.get(11).ok(),
            file_path: row.get(12).ok(),
            line_number: row.get(13).ok(),
            column_number: row.get(14).ok(),
            timestamp: row.get(15)?,
            device_timestamp: row.get(16).ok(),
            created_at: row.get(17)?,
        })
    }

    /// Create a new mobile state capture.
    pub fn create_mobile_state(
        &self,
        input: &CreateMobileStateInput,
    ) -> Result<MobileState, String> {
        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();

        conn.execute(
            r#"
            INSERT INTO task_run_mobile_state (
                task_run_id, timestamp, device_id, device_type, device_model,
                app_package, app_activity, app_state, metro_connected, bundle_status,
                last_reload_type, last_reload_time, screenshot_path, logcat_path,
                has_errors, error_summary, created_at
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5,
                ?6, ?7, ?8, ?9, ?10,
                ?11, ?12, ?13, ?14,
                ?15, ?16, ?17
            )
            "#,
            params![
                input.task_run_id,
                now,
                input.device_id,
                input.device_type,
                input.device_model,
                input.app_package,
                input.app_activity,
                input.app_state,
                input.metro_connected as i32,
                input.bundle_status,
                input.last_reload_type,
                input.last_reload_time,
                input.screenshot_path,
                input.logcat_path,
                input.has_errors as i32,
                input.error_summary,
                now,
            ],
        )
        .map_err(|e| format!("Failed to create mobile state: {}", e))?;

        let id = conn.last_insert_rowid();
        self.get_mobile_state(id)?
            .ok_or_else(|| "Failed to retrieve created mobile state".to_string())
    }

    /// Get a mobile state by ID.
    pub fn get_mobile_state(&self, id: i64) -> Result<Option<MobileState>, String> {
        let conn = self.get_conn()?;

        let result: SqliteResult<MobileState> = conn.query_row(
            r#"
            SELECT
                id, task_run_id, timestamp, device_id, device_type, device_model,
                app_package, app_activity, app_state, metro_connected, bundle_status,
                last_reload_type, last_reload_time, screenshot_path, logcat_path,
                has_errors, error_summary, created_at
            FROM task_run_mobile_state
            WHERE id = ?1
            "#,
            params![id],
            Self::row_to_mobile_state,
        );

        match result {
            Ok(state) => Ok(Some(state)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("Failed to get mobile state: {}", e)),
        }
    }

    /// Get mobile state captures for a task run.
    pub fn get_mobile_states(
        &self,
        task_run_id: &str,
        limit: Option<u32>,
    ) -> Result<Vec<MobileState>, String> {
        let conn = self.get_conn()?;
        let limit_val = limit.unwrap_or(100);

        let sql = format!(
            r#"
            SELECT
                id, task_run_id, timestamp, device_id, device_type, device_model,
                app_package, app_activity, app_state, metro_connected, bundle_status,
                last_reload_type, last_reload_time, screenshot_path, logcat_path,
                has_errors, error_summary, created_at
            FROM task_run_mobile_state
            WHERE task_run_id = ?1
            ORDER BY timestamp DESC
            LIMIT {}
            "#,
            limit_val
        );

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        let states: Vec<MobileState> = stmt
            .query_map(params![task_run_id], Self::row_to_mobile_state)
            .map_err(|e| format!("Failed to query mobile states: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(states)
    }

    /// Get the latest mobile state for a task run.
    pub fn get_latest_mobile_state(
        &self,
        task_run_id: &str,
    ) -> Result<Option<MobileState>, String> {
        let states = self.get_mobile_states(task_run_id, Some(1))?;
        Ok(states.into_iter().next())
    }

    /// Create a new mobile log entry.
    pub fn create_mobile_log(&self, input: &CreateMobileLogInput) -> Result<MobileLog, String> {
        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();

        conn.execute(
            r#"
            INSERT INTO task_run_mobile_logs (
                task_run_id, mobile_state_id, log_source, log_level, log_tag,
                message, raw_line, data, error_type, error_code,
                stack_trace, file_path, line_number, column_number,
                timestamp, device_timestamp, created_at
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5,
                ?6, ?7, ?8, ?9, ?10,
                ?11, ?12, ?13, ?14,
                ?15, ?16, ?17
            )
            "#,
            params![
                input.task_run_id,
                input.mobile_state_id,
                input.log_source,
                input.log_level,
                input.log_tag,
                input.message,
                input.raw_line,
                input.data,
                input.error_type,
                input.error_code,
                input.stack_trace,
                input.file_path,
                input.line_number,
                input.column_number,
                now,
                input.device_timestamp,
                now,
            ],
        )
        .map_err(|e| format!("Failed to create mobile log: {}", e))?;

        let id = conn.last_insert_rowid();
        self.get_mobile_log(id)?
            .ok_or_else(|| "Failed to retrieve created mobile log".to_string())
    }

    /// Get a mobile log by ID.
    pub fn get_mobile_log(&self, id: i64) -> Result<Option<MobileLog>, String> {
        let conn = self.get_conn()?;

        let result: SqliteResult<MobileLog> = conn.query_row(
            r#"
            SELECT
                id, task_run_id, mobile_state_id, log_source, log_level, log_tag,
                message, raw_line, data, error_type, error_code,
                stack_trace, file_path, line_number, column_number,
                timestamp, device_timestamp, created_at
            FROM task_run_mobile_logs
            WHERE id = ?1
            "#,
            params![id],
            Self::row_to_mobile_log,
        );

        match result {
            Ok(log) => Ok(Some(log)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("Failed to get mobile log: {}", e)),
        }
    }

    /// Get mobile logs for a task run with optional filtering.
    pub fn get_mobile_logs(
        &self,
        task_run_id: &str,
        log_source: Option<&str>,
        errors_only: bool,
        limit: Option<u32>,
    ) -> Result<Vec<MobileLog>, String> {
        let conn = self.get_conn()?;
        let limit_val = limit.unwrap_or(500);

        let mut conditions = vec!["task_run_id = ?1".to_string()];
        let mut params: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(task_run_id.to_string())];

        if let Some(source) = log_source {
            conditions.push(format!("log_source = ?{}", params.len() + 1));
            params.push(Box::new(source.to_string()));
        }

        if errors_only {
            conditions.push("log_level IN ('error', 'fatal', 'ERROR', 'FATAL', 'E')".to_string());
        }

        let sql = format!(
            r#"
            SELECT
                id, task_run_id, mobile_state_id, log_source, log_level, log_tag,
                message, raw_line, data, error_type, error_code,
                stack_trace, file_path, line_number, column_number,
                timestamp, device_timestamp, created_at
            FROM task_run_mobile_logs
            WHERE {}
            ORDER BY timestamp DESC
            LIMIT {}
            "#,
            conditions.join(" AND "),
            limit_val
        );

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        let params_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();

        let logs: Vec<MobileLog> = stmt
            .query_map(params_refs.as_slice(), Self::row_to_mobile_log)
            .map_err(|e| format!("Failed to query mobile logs: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(logs)
    }

    /// Get mobile error logs for a task run.
    pub fn get_mobile_errors(
        &self,
        task_run_id: &str,
        limit: Option<u32>,
    ) -> Result<Vec<MobileLog>, String> {
        self.get_mobile_logs(task_run_id, None, true, limit)
    }

    /// Delete all mobile data for a task run.
    pub fn delete_mobile_data_for_task(&self, task_run_id: &str) -> Result<(usize, usize), String> {
        let conn = self.get_conn()?;

        let logs_deleted = conn
            .execute(
                "DELETE FROM task_run_mobile_logs WHERE task_run_id = ?1",
                params![task_run_id],
            )
            .map_err(|e| format!("Failed to delete mobile logs: {}", e))?;

        let states_deleted = conn
            .execute(
                "DELETE FROM task_run_mobile_state WHERE task_run_id = ?1",
                params![task_run_id],
            )
            .map_err(|e| format!("Failed to delete mobile states: {}", e))?;

        Ok((states_deleted, logs_deleted))
    }

    // ========================================================================
    // MCP Server Operations
    // ========================================================================

    /// List all MCP server configurations.
    pub fn list_mcp_servers(&self) -> Result<Vec<crate::mcp_client::McpServerConfig>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, name, description, transport,
                       stdio_config, http_config,
                       enabled, auto_start, timeout_seconds,
                       cached_tools, tools_cached_at,
                       created_at, updated_at
                FROM mcp_servers
                ORDER BY name ASC
                "#,
            )
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let results = stmt
            .query_map([], |row| {
                let transport_str: String = row.get(3)?;
                let transport = match transport_str.as_str() {
                    "http" => crate::mcp_client::McpTransport::Http,
                    _ => crate::mcp_client::McpTransport::Stdio,
                };

                let stdio_config: Option<String> = row.get(4)?;
                let http_config: Option<String> = row.get(5)?;

                Ok(crate::mcp_client::McpServerConfig {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    description: row.get(2)?,
                    transport,
                    stdio_config: stdio_config.and_then(|s| serde_json::from_str(&s).ok()),
                    http_config: http_config.and_then(|s| serde_json::from_str(&s).ok()),
                    enabled: row.get(6)?,
                    auto_start: row.get(7)?,
                    timeout_seconds: row.get(8)?,
                    cached_tools: row.get(9)?,
                    tools_cached_at: row.get(10)?,
                    created_at: row.get(11)?,
                    updated_at: row.get(12)?,
                })
            })
            .map_err(|e| format!("Failed to list MCP servers: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }

    /// Get a specific MCP server by ID.
    pub fn get_mcp_server(
        &self,
        id: &str,
    ) -> Result<Option<crate::mcp_client::McpServerConfig>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, name, description, transport,
                       stdio_config, http_config,
                       enabled, auto_start, timeout_seconds,
                       cached_tools, tools_cached_at,
                       created_at, updated_at
                FROM mcp_servers
                WHERE id = ?1
                "#,
            )
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let result = stmt
            .query_row(params![id], |row| {
                let transport_str: String = row.get(3)?;
                let transport = match transport_str.as_str() {
                    "http" => crate::mcp_client::McpTransport::Http,
                    _ => crate::mcp_client::McpTransport::Stdio,
                };

                let stdio_config: Option<String> = row.get(4)?;
                let http_config: Option<String> = row.get(5)?;

                Ok(crate::mcp_client::McpServerConfig {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    description: row.get(2)?,
                    transport,
                    stdio_config: stdio_config.and_then(|s| serde_json::from_str(&s).ok()),
                    http_config: http_config.and_then(|s| serde_json::from_str(&s).ok()),
                    enabled: row.get(6)?,
                    auto_start: row.get(7)?,
                    timeout_seconds: row.get(8)?,
                    cached_tools: row.get(9)?,
                    tools_cached_at: row.get(10)?,
                    created_at: row.get(11)?,
                    updated_at: row.get(12)?,
                })
            })
            .optional()
            .map_err(|e| format!("Failed to get MCP server: {}", e))?;

        Ok(result)
    }

    /// Create a new MCP server configuration.
    pub fn create_mcp_server(
        &self,
        input: crate::mcp_client::CreateMcpServerInput,
    ) -> Result<crate::mcp_client::McpServerConfig, String> {
        let conn = self.get_conn()?;
        let id = uuid::Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();

        let transport_str = match input.transport {
            crate::mcp_client::McpTransport::Http => "http",
            crate::mcp_client::McpTransport::Stdio => "stdio",
        };

        let stdio_config_json = input
            .stdio_config
            .as_ref()
            .map(|c| serde_json::to_string(c).unwrap_or_default());
        let http_config_json = input
            .http_config
            .as_ref()
            .map(|c| serde_json::to_string(c).unwrap_or_default());

        conn.execute(
            r#"
            INSERT INTO mcp_servers (
                id, name, description, transport,
                stdio_config, http_config,
                enabled, auto_start, timeout_seconds,
                created_at, updated_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
            "#,
            params![
                id,
                input.name,
                input.description,
                transport_str,
                stdio_config_json,
                http_config_json,
                input.enabled.unwrap_or(true),
                input.auto_start.unwrap_or(false),
                input.timeout_seconds.unwrap_or(30) as i64,
                now,
                now,
            ],
        )
        .map_err(|e| format!("Failed to create MCP server: {}", e))?;

        self.get_mcp_server(&id)?
            .ok_or_else(|| "Failed to retrieve created MCP server".to_string())
    }

    /// Update an MCP server configuration.
    pub fn update_mcp_server(
        &self,
        id: &str,
        input: crate::mcp_client::UpdateMcpServerInput,
    ) -> Result<crate::mcp_client::McpServerConfig, String> {
        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();

        // Get existing record and merge with input
        let existing = self
            .get_mcp_server(id)?
            .ok_or_else(|| format!("MCP server not found: {}", id))?;

        let name = input.name.unwrap_or(existing.name);
        let description = input.description.or(existing.description);
        let transport = input.transport.unwrap_or(existing.transport);
        let stdio_config = input.stdio_config.or(existing.stdio_config);
        let http_config = input.http_config.or(existing.http_config);
        let enabled = input.enabled.unwrap_or(existing.enabled);
        let auto_start = input.auto_start.unwrap_or(existing.auto_start);
        let timeout_seconds = input.timeout_seconds.unwrap_or(existing.timeout_seconds);

        let transport_str = match transport {
            crate::mcp_client::McpTransport::Http => "http",
            crate::mcp_client::McpTransport::Stdio => "stdio",
        };
        let stdio_config_json = stdio_config
            .as_ref()
            .map(|c| serde_json::to_string(c).unwrap_or_default());
        let http_config_json = http_config
            .as_ref()
            .map(|c| serde_json::to_string(c).unwrap_or_default());

        conn.execute(
            r#"
            UPDATE mcp_servers SET
                name = ?1, description = ?2, transport = ?3,
                stdio_config = ?4, http_config = ?5,
                enabled = ?6, auto_start = ?7, timeout_seconds = ?8,
                updated_at = ?9
            WHERE id = ?10
            "#,
            params![
                name,
                description,
                transport_str,
                stdio_config_json,
                http_config_json,
                enabled,
                auto_start,
                timeout_seconds as i64,
                now,
                id,
            ],
        )
        .map_err(|e| format!("Failed to update MCP server: {}", e))?;

        self.get_mcp_server(id)?
            .ok_or_else(|| "Failed to retrieve updated MCP server".to_string())
    }

    /// Delete an MCP server configuration.
    pub fn delete_mcp_server(&self, id: &str) -> Result<(), String> {
        let conn = self.get_conn()?;

        conn.execute("DELETE FROM mcp_servers WHERE id = ?1", params![id])
            .map_err(|e| format!("Failed to delete MCP server: {}", e))?;

        Ok(())
    }

    /// Update the cached tools for an MCP server.
    pub fn update_mcp_server_tools_cache(
        &self,
        id: &str,
        tools_json: &str,
        cached_at: &str,
    ) -> Result<(), String> {
        let conn = self.get_conn()?;

        conn.execute(
            r#"
            UPDATE mcp_servers SET
                cached_tools = ?1,
                tools_cached_at = ?2,
                updated_at = ?3
            WHERE id = ?4
            "#,
            params![tools_json, cached_at, cached_at, id],
        )
        .map_err(|e| format!("Failed to update MCP server tools cache: {}", e))?;

        Ok(())
    }

    // ========================================================================
    // MCP Call Operations
    // ========================================================================

    /// Create a task run MCP call record.
    pub fn create_task_run_mcp_call(
        &self,
        input: &crate::mcp_client::CreateMcpCallInput,
    ) -> Result<String, String> {
        let conn = self.get_conn()?;
        let id = uuid::Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();

        conn.execute(
            r#"
            INSERT INTO task_run_mcp_calls (
                id, task_run_id, step_id, step_name,
                server_id, server_name, tool_name,
                arguments, resolved_arguments,
                response, response_type, duration_ms,
                extractions, assertions,
                success, error_message, created_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)
            "#,
            params![
                id,
                input.task_run_id,
                input.step_id,
                input.step_name,
                input.server_id,
                input.server_name,
                input.tool_name,
                input.arguments,
                input.resolved_arguments,
                input.response,
                input.response_type,
                input.duration_ms,
                input.extractions,
                input.assertions,
                input.success,
                input.error_message,
                now,
            ],
        )
        .map_err(|e| format!("Failed to create task run MCP call: {}", e))?;

        Ok(id)
    }

    /// Get MCP calls for a task run.
    pub fn get_task_run_mcp_calls(
        &self,
        task_run_id: &str,
        success_filter: Option<bool>,
    ) -> Result<crate::mcp_client::McpCallsResult, String> {
        let conn = self.get_conn()?;

        let query = if success_filter.is_some() {
            r#"
            SELECT id, task_run_id, step_id, step_name,
                   server_id, server_name, tool_name,
                   arguments, resolved_arguments,
                   response, response_type, duration_ms,
                   extractions, assertions,
                   success, error_message, created_at
            FROM task_run_mcp_calls
            WHERE task_run_id = ?1 AND success = ?2
            ORDER BY created_at ASC
            "#
        } else {
            r#"
            SELECT id, task_run_id, step_id, step_name,
                   server_id, server_name, tool_name,
                   arguments, resolved_arguments,
                   response, response_type, duration_ms,
                   extractions, assertions,
                   success, error_message, created_at
            FROM task_run_mcp_calls
            WHERE task_run_id = ?1
            ORDER BY created_at ASC
            "#
        };

        let mut stmt = conn
            .prepare(query)
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let row_mapper =
            |row: &rusqlite::Row| -> rusqlite::Result<crate::mcp_client::McpCallRecord> {
                Ok(crate::mcp_client::McpCallRecord {
                    id: row.get(0)?,
                    task_run_id: row.get(1)?,
                    step_id: row.get(2)?,
                    step_name: row.get(3)?,
                    server_id: row.get(4)?,
                    server_name: row.get(5)?,
                    tool_name: row.get(6)?,
                    arguments: row.get(7)?,
                    resolved_arguments: row.get(8)?,
                    response: row.get(9)?,
                    response_type: row.get(10)?,
                    duration_ms: row.get(11)?,
                    extractions: row.get(12)?,
                    assertions: row.get(13)?,
                    success: row.get(14)?,
                    error_message: row.get(15)?,
                    created_at: row.get(16)?,
                })
            };

        let calls: Vec<crate::mcp_client::McpCallRecord> = if let Some(success) = success_filter {
            stmt.query_map(params![task_run_id, success], row_mapper)
                .map_err(|e| format!("Failed to get MCP calls: {}", e))?
                .filter_map(|r| r.ok())
                .collect()
        } else {
            stmt.query_map(params![task_run_id], row_mapper)
                .map_err(|e| format!("Failed to get MCP calls: {}", e))?
                .filter_map(|r| r.ok())
                .collect()
        };

        let success_count = calls.iter().filter(|c| c.success).count();
        let failed_count = calls.iter().filter(|c| !c.success).count();

        Ok(crate::mcp_client::McpCallsResult {
            task_run_id: task_run_id.to_string(),
            calls: calls.clone(),
            count: calls.len(),
            success_count,
            failed_count,
        })
    }

    // ========================================================================
    // Workflow State Management Operations
    // ========================================================================

    /// Save or update workflow execution state.
    pub fn save_workflow_execution_state(
        &self,
        execution_id: &str,
        workflow_type: &str,
        state_name: &str,
        state_data: Option<&str>,
        phase: Option<&str>,
        iteration: Option<u32>,
    ) -> Result<(), String> {
        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();

        conn.execute(
            r#"
            INSERT INTO workflow_execution_state (
                execution_id, workflow_type, state_name, state_data, phase, iteration, updated_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            ON CONFLICT(execution_id) DO UPDATE SET
                workflow_type = ?2,
                state_name = ?3,
                state_data = ?4,
                phase = ?5,
                iteration = ?6,
                updated_at = ?7
            "#,
            params![
                execution_id,
                workflow_type,
                state_name,
                state_data,
                phase,
                iteration.map(|i| i as i64),
                now,
            ],
        )
        .map_err(|e| format!("Failed to save workflow execution state: {}", e))?;

        Ok(())
    }

    /// Get workflow execution state by execution_id.
    pub fn get_workflow_execution_state(
        &self,
        execution_id: &str,
    ) -> Result<Option<crate::workflow_state::WorkflowExecutionStateRecord>, String> {
        let conn = self.get_conn()?;

        let result = conn
            .query_row(
                r#"
                SELECT execution_id, workflow_type, state_name, state_data, phase, iteration, updated_at
                FROM workflow_execution_state
                WHERE execution_id = ?1
                "#,
                params![execution_id],
                |row| {
                    Ok(crate::workflow_state::WorkflowExecutionStateRecord {
                        execution_id: row.get(0)?,
                        workflow_type: row.get(1)?,
                        state_name: row.get(2)?,
                        state_data: row.get(3)?,
                        phase: row.get(4)?,
                        iteration: row.get::<_, Option<i64>>(5)?.map(|i| i as u32),
                        updated_at: row.get(6)?,
                    })
                },
            )
            .optional()
            .map_err(|e| format!("Failed to get workflow execution state: {}", e))?;

        Ok(result)
    }

    /// Delete workflow execution state.
    pub fn delete_workflow_execution_state(&self, execution_id: &str) -> Result<(), String> {
        let conn = self.get_conn()?;

        conn.execute(
            "DELETE FROM workflow_execution_state WHERE execution_id = ?1",
            params![execution_id],
        )
        .map_err(|e| format!("Failed to delete workflow execution state: {}", e))?;

        Ok(())
    }

    // ========================================================================
    // Workflow Step Checkpoint Operations
    // ========================================================================

    /// Save or update a workflow step checkpoint.
    pub fn save_workflow_step_checkpoint(
        &self,
        checkpoint: &crate::workflow_state::StepCheckpoint,
    ) -> Result<(), String> {
        let conn = self.get_conn()?;

        conn.execute(
            r#"
            INSERT INTO workflow_step_checkpoints (
                id, execution_id, workflow_type, phase, iteration, step_index,
                stage_index, step_type, step_name, status, result_json, step_config_json,
                started_at, completed_at, duration_ms, error
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, COALESCE(?7, 0), ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)
            ON CONFLICT(execution_id, phase, iteration, step_index, stage_index) DO UPDATE SET
                status = ?10,
                result_json = ?11,
                step_config_json = COALESCE(?12, step_config_json),
                started_at = COALESCE(?13, started_at),
                completed_at = ?14,
                duration_ms = ?15,
                error = ?16
            "#,
            params![
                checkpoint.id,
                checkpoint.execution_id,
                checkpoint.workflow_type,
                checkpoint.phase,
                checkpoint.iteration.map(|i| i as i64),
                checkpoint.step_index as i64,
                checkpoint.stage_index.map(|i| i as i64),
                checkpoint.step_type,
                checkpoint.step_name,
                checkpoint.status.to_string(),
                checkpoint.result_json,
                checkpoint.step_config_json,
                checkpoint.started_at,
                checkpoint.completed_at,
                checkpoint.duration_ms,
                checkpoint.error,
            ],
        )
        .map_err(|e| format!("Failed to save workflow step checkpoint: {}", e))?;

        Ok(())
    }

    /// Atomically save both workflow execution state and a step checkpoint.
    ///
    /// This is critical for ensuring data consistency when a step completes and the
    /// workflow state advances. If either operation fails, both are rolled back.
    ///
    /// # Arguments
    /// * `execution_id` - The execution/task run ID
    /// * `workflow_type` - Type of workflow (e.g., "unified")
    /// * `state_name` - Name of the new workflow state
    /// * `state_data` - Serialized state data (JSON)
    /// * `phase` - Current phase name
    /// * `iteration` - Current iteration number
    /// * `checkpoint` - The step checkpoint to save
    pub fn save_state_and_checkpoint_atomic(
        &self,
        execution_id: &str,
        workflow_type: &str,
        state_name: &str,
        state_data: Option<&str>,
        phase: Option<&str>,
        iteration: Option<u32>,
        checkpoint: &crate::workflow_state::StepCheckpoint,
    ) -> Result<(), String> {
        self.transaction(|conn| {
            let now = chrono::Utc::now().to_rfc3339();

            // Save workflow execution state
            conn.execute(
                r#"
                INSERT INTO workflow_execution_state (
                    execution_id, workflow_type, state_name, state_data, phase, iteration, updated_at
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                ON CONFLICT(execution_id) DO UPDATE SET
                    workflow_type = ?2,
                    state_name = ?3,
                    state_data = ?4,
                    phase = ?5,
                    iteration = ?6,
                    updated_at = ?7
                "#,
                params![
                    execution_id,
                    workflow_type,
                    state_name,
                    state_data,
                    phase,
                    iteration.map(|i| i as i64),
                    now,
                ],
            )?;

            // Save step checkpoint
            conn.execute(
                r#"
                INSERT INTO workflow_step_checkpoints (
                    id, execution_id, workflow_type, phase, iteration, step_index,
                    stage_index, step_type, step_name, status, result_json, step_config_json,
                    started_at, completed_at, duration_ms, error
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, COALESCE(?7, 0), ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)
                ON CONFLICT(execution_id, phase, iteration, step_index, stage_index) DO UPDATE SET
                    status = ?10,
                    result_json = ?11,
                    step_config_json = COALESCE(?12, step_config_json),
                    started_at = COALESCE(?13, started_at),
                    completed_at = ?14,
                    duration_ms = ?15,
                    error = ?16
                "#,
                params![
                    checkpoint.id,
                    checkpoint.execution_id,
                    checkpoint.workflow_type,
                    checkpoint.phase,
                    checkpoint.iteration.map(|i| i as i64),
                    checkpoint.step_index as i64,
                    checkpoint.stage_index.map(|i| i as i64),
                    checkpoint.step_type,
                    checkpoint.step_name,
                    checkpoint.status.to_string(),
                    checkpoint.result_json,
                    checkpoint.step_config_json,
                    checkpoint.started_at,
                    checkpoint.completed_at,
                    checkpoint.duration_ms,
                    checkpoint.error,
                ],
            )?;

            Ok(())
        })
    }

    /// Get workflow step checkpoints for a given execution, phase, and iteration.
    pub fn get_workflow_step_checkpoints(
        &self,
        execution_id: &str,
        phase: &str,
        iteration: Option<u32>,
    ) -> Result<Vec<crate::workflow_state::StepCheckpoint>, String> {
        let conn = self.get_conn()?;

        let query = if iteration.is_some() {
            r#"
            SELECT id, execution_id, workflow_type, phase, iteration, step_index,
                   stage_index, step_type, step_name, status, result_json, step_config_json,
                   started_at, completed_at, duration_ms, error
            FROM workflow_step_checkpoints
            WHERE execution_id = ?1 AND phase = ?2 AND iteration = ?3
            ORDER BY step_index ASC
            "#
        } else {
            r#"
            SELECT id, execution_id, workflow_type, phase, iteration, step_index,
                   stage_index, step_type, step_name, status, result_json, step_config_json,
                   started_at, completed_at, duration_ms, error
            FROM workflow_step_checkpoints
            WHERE execution_id = ?1 AND phase = ?2 AND iteration IS NULL
            ORDER BY step_index ASC
            "#
        };

        let mut stmt = conn
            .prepare(query)
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let row_mapper =
            |row: &rusqlite::Row| -> rusqlite::Result<crate::workflow_state::StepCheckpoint> {
                let status_str: String = row.get(9)?;
                let status = status_str
                    .parse()
                    .unwrap_or(crate::workflow_state::StepCheckpointStatus::Pending);

                Ok(crate::workflow_state::StepCheckpoint {
                    id: row.get(0)?,
                    execution_id: row.get(1)?,
                    workflow_type: row.get(2)?,
                    phase: row.get(3)?,
                    iteration: row.get::<_, Option<i64>>(4)?.map(|i| i as u32),
                    step_index: row.get::<_, i64>(5)? as usize,
                    stage_index: row.get::<_, Option<i64>>(6)?.map(|i| i as u32),
                    step_type: row.get(7)?,
                    step_name: row.get(8)?,
                    status,
                    result_json: row.get(10)?,
                    step_config_json: row.get(11)?,
                    started_at: row.get(12)?,
                    completed_at: row.get(13)?,
                    duration_ms: row.get(14)?,
                    error: row.get(15)?,
                })
            };

        let checkpoints: Vec<crate::workflow_state::StepCheckpoint> = if let Some(iter) = iteration
        {
            stmt.query_map(params![execution_id, phase, iter as i64], row_mapper)
                .map_err(|e| format!("Failed to get step checkpoints: {}", e))?
                .filter_map(|r| r.ok())
                .collect()
        } else {
            stmt.query_map(params![execution_id, phase], row_mapper)
                .map_err(|e| format!("Failed to get step checkpoints: {}", e))?
                .filter_map(|r| r.ok())
                .collect()
        };

        Ok(checkpoints)
    }

    /// Get workflow step checkpoints with cursor-based pagination.
    ///
    /// This is optimized for handling runs with 1000+ steps without loading all data at once.
    /// Uses step_index as the cursor for efficient pagination.
    ///
    /// # Arguments
    /// * `execution_id` - The execution/task run ID
    /// * `cursor` - Optional step_index to start from (exclusive). None means start from beginning.
    /// * `limit` - Maximum number of checkpoints to return
    ///
    /// # Returns
    /// A tuple of (checkpoints, next_cursor). If next_cursor is Some, there are more results.
    pub fn get_workflow_step_checkpoints_paginated(
        &self,
        execution_id: &str,
        cursor: Option<i64>,
        limit: usize,
    ) -> Result<(Vec<crate::workflow_state::StepCheckpoint>, Option<i64>), String> {
        let conn = self.get_conn()?;

        // Use cursor-based pagination for efficiency
        // The idx_step_checkpoints_cursor index on (execution_id, step_index) makes this fast
        let query = if cursor.is_some() {
            r#"
            SELECT id, execution_id, workflow_type, phase, iteration, step_index,
                   stage_index, step_type, step_name, status, result_json, step_config_json,
                   started_at, completed_at, duration_ms, error
            FROM workflow_step_checkpoints
            WHERE execution_id = ?1 AND step_index > ?2
            ORDER BY step_index ASC
            LIMIT ?3
            "#
        } else {
            r#"
            SELECT id, execution_id, workflow_type, phase, iteration, step_index,
                   stage_index, step_type, step_name, status, result_json, step_config_json,
                   started_at, completed_at, duration_ms, error
            FROM workflow_step_checkpoints
            WHERE execution_id = ?1
            ORDER BY step_index ASC
            LIMIT ?2
            "#
        };

        let mut stmt = conn
            .prepare(query)
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let row_mapper =
            |row: &rusqlite::Row| -> rusqlite::Result<crate::workflow_state::StepCheckpoint> {
                let status_str: String = row.get(9)?;
                let status = status_str
                    .parse()
                    .unwrap_or(crate::workflow_state::StepCheckpointStatus::Pending);

                Ok(crate::workflow_state::StepCheckpoint {
                    id: row.get(0)?,
                    execution_id: row.get(1)?,
                    workflow_type: row.get(2)?,
                    phase: row.get(3)?,
                    iteration: row.get::<_, Option<i64>>(4)?.map(|i| i as u32),
                    step_index: row.get::<_, i64>(5)? as usize,
                    stage_index: row.get::<_, Option<i64>>(6)?.map(|i| i as u32),
                    step_type: row.get(7)?,
                    step_name: row.get(8)?,
                    status,
                    result_json: row.get(10)?,
                    step_config_json: row.get(11)?,
                    started_at: row.get(12)?,
                    completed_at: row.get(13)?,
                    duration_ms: row.get(14)?,
                    error: row.get(15)?,
                })
            };

        // Request one more than limit to check if there are more results
        let fetch_limit = (limit + 1) as i64;

        let checkpoints: Vec<crate::workflow_state::StepCheckpoint> =
            if let Some(cursor_val) = cursor {
                stmt.query_map(params![execution_id, cursor_val, fetch_limit], row_mapper)
                    .map_err(|e| format!("Failed to get step checkpoints: {}", e))?
                    .filter_map(|r| r.ok())
                    .collect()
            } else {
                stmt.query_map(params![execution_id, fetch_limit], row_mapper)
                    .map_err(|e| format!("Failed to get step checkpoints: {}", e))?
                    .filter_map(|r| r.ok())
                    .collect()
            };

        // Determine if there are more results and what the next cursor should be
        let (result_checkpoints, next_cursor) = if checkpoints.len() > limit {
            // There are more results; return only `limit` items
            let mut result = checkpoints;
            result.truncate(limit);
            let last_step_index = result.last().map(|cp| cp.step_index as i64);
            (result, last_step_index)
        } else {
            // No more results
            (checkpoints, None)
        };

        Ok((result_checkpoints, next_cursor))
    }

    /// Delete workflow step checkpoints.
    pub fn delete_workflow_step_checkpoints(
        &self,
        execution_id: &str,
        phase: Option<&str>,
        iteration: Option<u32>,
    ) -> Result<(), String> {
        let conn = self.get_conn()?;

        match (phase, iteration) {
            (Some(p), Some(i)) => {
                conn.execute(
                    "DELETE FROM workflow_step_checkpoints WHERE execution_id = ?1 AND phase = ?2 AND iteration = ?3",
                    params![execution_id, p, i as i64],
                )
            }
            (Some(p), None) => {
                conn.execute(
                    "DELETE FROM workflow_step_checkpoints WHERE execution_id = ?1 AND phase = ?2",
                    params![execution_id, p],
                )
            }
            (None, _) => {
                conn.execute(
                    "DELETE FROM workflow_step_checkpoints WHERE execution_id = ?1",
                    params![execution_id],
                )
            }
        }
        .map_err(|e| format!("Failed to delete step checkpoints: {}", e))?;

        Ok(())
    }

    // ========================================================================
    // Step Progress Marker Operations
    // ========================================================================

    /// Save a progress marker for a step checkpoint.
    ///
    /// Progress markers track intra-step progress, such as "analyzed 50/100 files".
    /// This is useful for long-running AI operations where you want to show progress
    /// and enable resume from the last known position.
    pub fn save_step_progress_marker(
        &self,
        checkpoint_id: &str,
        marker_type: &str,
        current_value: u64,
        total_value: Option<u64>,
        description: Option<&str>,
        data_json: Option<&str>,
    ) -> Result<i64, String> {
        let conn = self.get_conn()?;
        let now = chrono::Utc::now().to_rfc3339();

        conn.execute(
            r#"
            INSERT INTO step_progress_markers (
                checkpoint_id, marker_type, current_value, total_value,
                description, data_json, created_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            "#,
            params![
                checkpoint_id,
                marker_type,
                current_value as i64,
                total_value.map(|v| v as i64),
                description,
                data_json,
                now,
            ],
        )
        .map_err(|e| format!("Failed to save step progress marker: {}", e))?;

        Ok(conn.last_insert_rowid())
    }

    /// Get the latest progress marker for a step checkpoint.
    ///
    /// Returns the most recent progress marker for the given checkpoint_id,
    /// which can be used to resume from the last known position.
    pub fn get_latest_step_progress_marker(
        &self,
        checkpoint_id: &str,
    ) -> Result<Option<crate::workflow_state::StepProgressMarker>, String> {
        let conn = self.get_conn()?;

        let result = conn.query_row(
            r#"
                SELECT id, checkpoint_id, marker_type, current_value, total_value,
                       description, data_json, created_at
                FROM step_progress_markers
                WHERE checkpoint_id = ?1
                ORDER BY id DESC
                LIMIT 1
                "#,
            params![checkpoint_id],
            |row| {
                Ok(crate::workflow_state::StepProgressMarker {
                    id: row.get(0)?,
                    checkpoint_id: row.get(1)?,
                    marker_type: row.get(2)?,
                    current_value: row.get::<_, i64>(3)? as u64,
                    total_value: row.get::<_, Option<i64>>(4)?.map(|v| v as u64),
                    description: row.get(5)?,
                    data_json: row.get(6)?,
                    created_at: row.get(7)?,
                })
            },
        );

        match result {
            Ok(marker) => Ok(Some(marker)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("Failed to get step progress marker: {}", e)),
        }
    }

    /// Get all progress markers for a step checkpoint.
    ///
    /// Returns all progress markers in order of creation (oldest first).
    pub fn get_step_progress_markers(
        &self,
        checkpoint_id: &str,
    ) -> Result<Vec<crate::workflow_state::StepProgressMarker>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, checkpoint_id, marker_type, current_value, total_value,
                       description, data_json, created_at
                FROM step_progress_markers
                WHERE checkpoint_id = ?1
                ORDER BY id ASC
                "#,
            )
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let markers = stmt
            .query_map(params![checkpoint_id], |row| {
                Ok(crate::workflow_state::StepProgressMarker {
                    id: row.get(0)?,
                    checkpoint_id: row.get(1)?,
                    marker_type: row.get(2)?,
                    current_value: row.get::<_, i64>(3)? as u64,
                    total_value: row.get::<_, Option<i64>>(4)?.map(|v| v as u64),
                    description: row.get(5)?,
                    data_json: row.get(6)?,
                    created_at: row.get(7)?,
                })
            })
            .map_err(|e| format!("Failed to get step progress markers: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(markers)
    }

    /// Delete all progress markers for a step checkpoint.
    pub fn delete_step_progress_markers(&self, checkpoint_id: &str) -> Result<usize, String> {
        let conn = self.get_conn()?;

        let deleted = conn
            .execute(
                "DELETE FROM step_progress_markers WHERE checkpoint_id = ?1",
                params![checkpoint_id],
            )
            .map_err(|e| format!("Failed to delete step progress markers: {}", e))?;

        Ok(deleted)
    }

    // ========================================================================
    // Full Workflow State (for frontend restart recovery)
    // ========================================================================

    /// Get all workflow step checkpoints for an execution (all phases).
    ///
    /// This is used by the full-state endpoint to return all checkpoints for restart recovery.
    pub fn get_all_workflow_step_checkpoints(
        &self,
        execution_id: &str,
    ) -> Result<Vec<crate::workflow_state::StepCheckpoint>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, execution_id, workflow_type, phase, iteration, step_index,
                       stage_index, step_type, step_name, status, result_json, step_config_json,
                       started_at, completed_at, duration_ms, error
                FROM workflow_step_checkpoints
                WHERE execution_id = ?1
                ORDER BY COALESCE(stage_index, 0), phase, COALESCE(iteration, 0), step_index ASC
                "#,
            )
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let row_mapper =
            |row: &rusqlite::Row| -> rusqlite::Result<crate::workflow_state::StepCheckpoint> {
                let status_str: String = row.get(9)?;
                let status = status_str
                    .parse()
                    .unwrap_or(crate::workflow_state::StepCheckpointStatus::Pending);

                Ok(crate::workflow_state::StepCheckpoint {
                    id: row.get(0)?,
                    execution_id: row.get(1)?,
                    workflow_type: row.get(2)?,
                    phase: row.get(3)?,
                    iteration: row.get::<_, Option<i64>>(4)?.map(|i| i as u32),
                    step_index: row.get::<_, i64>(5)? as usize,
                    stage_index: row.get::<_, Option<i64>>(6)?.map(|i| i as u32),
                    step_type: row.get(7)?,
                    step_name: row.get(8)?,
                    status,
                    result_json: row.get(10)?,
                    step_config_json: row.get(11)?,
                    started_at: row.get(12)?,
                    completed_at: row.get(13)?,
                    duration_ms: row.get(14)?,
                    error: row.get(15)?,
                })
            };

        let checkpoints: Vec<crate::workflow_state::StepCheckpoint> = stmt
            .query_map(params![execution_id], row_mapper)
            .map_err(|e| format!("Failed to get all step checkpoints: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(checkpoints)
    }

    /// Get the progress marker for the currently running step (if any).
    ///
    /// Finds the checkpoint that is in "running" status and returns its latest progress marker.
    pub fn get_current_step_progress(
        &self,
        execution_id: &str,
    ) -> Result<Option<crate::workflow_state::StepProgressMarker>, String> {
        let conn = self.get_conn()?;

        // First find the running checkpoint
        let running_checkpoint_id: Option<String> = conn
            .query_row(
                r#"
                SELECT id FROM workflow_step_checkpoints
                WHERE execution_id = ?1 AND status = 'running'
                ORDER BY step_index DESC
                LIMIT 1
                "#,
                params![execution_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| format!("Failed to find running checkpoint: {}", e))?;

        match running_checkpoint_id {
            Some(checkpoint_id) => self.get_latest_step_progress_marker(&checkpoint_id),
            None => Ok(None),
        }
    }

    // ========================================================================
    // Cached App Specs
    // ========================================================================

    /// Upsert a cached spec for an external app.
    pub fn upsert_cached_spec(
        &self,
        app_url: &str,
        app_name: &str,
        spec_id: &str,
        spec_json: &str,
        page_url: Option<&str>,
    ) -> Result<(), String> {
        let conn = self.get_conn()?;
        let id = format!("{}:{}", app_url, spec_id);
        let now = Utc::now().to_rfc3339();

        conn.execute(
            r#"
            INSERT INTO cached_app_specs (id, app_url, app_name, spec_id, spec_json, discovered_at, page_url)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            ON CONFLICT(id) DO UPDATE SET
                app_name = excluded.app_name,
                spec_json = excluded.spec_json,
                discovered_at = excluded.discovered_at,
                page_url = excluded.page_url
            "#,
            params![id, app_url, app_name, spec_id, spec_json, now, page_url],
        )
        .map_err(|e| format!("Failed to upsert cached spec: {}", e))?;

        Ok(())
    }

    /// Get all cached specs for a specific app URL.
    pub fn get_cached_specs_for_app(&self, app_url: &str) -> Result<Vec<CachedAppSpec>, String> {
        let conn = self.get_conn()?;
        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, app_url, app_name, spec_id, spec_json, discovered_at, page_url
                FROM cached_app_specs
                WHERE app_url = ?1
                ORDER BY spec_id
                "#,
            )
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        let rows = stmt
            .query_map(params![app_url], |row| {
                Ok(CachedAppSpec {
                    id: row.get(0)?,
                    app_url: row.get(1)?,
                    app_name: row.get(2)?,
                    spec_id: row.get(3)?,
                    spec_json: row.get(4)?,
                    discovered_at: row.get(5)?,
                    page_url: row.get(6)?,
                })
            })
            .map_err(|e| format!("Failed to query cached specs: {}", e))?;

        let mut result = Vec::new();
        for row in rows {
            result.push(row.map_err(|e| format!("Failed to read row: {}", e))?);
        }
        Ok(result)
    }

    /// Get all cached specs across all apps.
    pub fn get_all_cached_specs(&self) -> Result<Vec<CachedAppSpec>, String> {
        let conn = self.get_conn()?;
        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, app_url, app_name, spec_id, spec_json, discovered_at, page_url
                FROM cached_app_specs
                ORDER BY app_url, spec_id
                "#,
            )
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        let rows = stmt
            .query_map([], |row| {
                Ok(CachedAppSpec {
                    id: row.get(0)?,
                    app_url: row.get(1)?,
                    app_name: row.get(2)?,
                    spec_id: row.get(3)?,
                    spec_json: row.get(4)?,
                    discovered_at: row.get(5)?,
                    page_url: row.get(6)?,
                })
            })
            .map_err(|e| format!("Failed to query cached specs: {}", e))?;

        let mut result = Vec::new();
        for row in rows {
            result.push(row.map_err(|e| format!("Failed to read row: {}", e))?);
        }
        Ok(result)
    }

    /// Delete all cached specs for a specific app URL.
    pub fn delete_cached_specs_for_app(&self, app_url: &str) -> Result<u64, String> {
        let conn = self.get_conn()?;
        let deleted = conn
            .execute(
                "DELETE FROM cached_app_specs WHERE app_url = ?1",
                params![app_url],
            )
            .map_err(|e| format!("Failed to delete cached specs: {}", e))?;

        Ok(deleted as u64)
    }

    // ========================================================================
    // Process Session Persistence
    // ========================================================================

    /// Create a new process session record.
    pub fn create_process_session(
        &self,
        id: &str,
        config_id: &str,
        name: &str,
    ) -> Result<(), String> {
        let conn = self.get_conn()?;
        conn.execute(
            "INSERT INTO process_sessions (id, process_config_id, process_name, started_at, state)
             VALUES (?1, ?2, ?3, ?4, 'running')",
            params![id, config_id, name, Utc::now().to_rfc3339()],
        )
        .map_err(|e| format!("Failed to create process session: {}", e))?;
        Ok(())
    }

    /// Update a process session (on stop/exit).
    pub fn update_process_session(
        &self,
        session_id: &str,
        state: &str,
        exit_code: Option<i32>,
        error_count: u32,
    ) -> Result<(), String> {
        let conn = self.get_conn()?;
        conn.execute(
            "UPDATE process_sessions SET stopped_at = ?1, state = ?2, exit_code = ?3, error_count = ?4
             WHERE id = ?5",
            params![
                Utc::now().to_rfc3339(),
                state,
                exit_code,
                error_count,
                session_id,
            ],
        )
        .map_err(|e| format!("Failed to update process session: {}", e))?;
        Ok(())
    }

    /// Get process sessions, optionally filtered by config_id.
    pub fn get_process_sessions(
        &self,
        config_id: Option<&str>,
        limit: u32,
    ) -> Result<Vec<ProcessSession>, String> {
        let conn = self.get_conn()?;
        let mut sessions = Vec::new();

        if let Some(cid) = config_id {
            let mut stmt = conn
                .prepare(
                    "SELECT id, process_config_id, process_name, started_at, stopped_at, exit_code, state, error_count
                     FROM process_sessions
                     WHERE process_config_id = ?1
                     ORDER BY started_at DESC
                     LIMIT ?2",
                )
                .map_err(|e| format!("Failed to prepare query: {}", e))?;

            let rows = stmt
                .query_map(params![cid, limit], |row| {
                    Ok(ProcessSession {
                        id: row.get(0)?,
                        process_config_id: row.get(1)?,
                        process_name: row.get(2)?,
                        started_at: row.get(3)?,
                        stopped_at: row.get(4)?,
                        exit_code: row.get(5)?,
                        state: row.get(6)?,
                        error_count: row.get(7)?,
                    })
                })
                .map_err(|e| format!("Failed to query sessions: {}", e))?;

            for row in rows {
                sessions.push(row.map_err(|e| format!("Failed to read session row: {}", e))?);
            }
        } else {
            let mut stmt = conn
                .prepare(
                    "SELECT id, process_config_id, process_name, started_at, stopped_at, exit_code, state, error_count
                     FROM process_sessions
                     ORDER BY started_at DESC
                     LIMIT ?1",
                )
                .map_err(|e| format!("Failed to prepare query: {}", e))?;

            let rows = stmt
                .query_map(params![limit], |row| {
                    Ok(ProcessSession {
                        id: row.get(0)?,
                        process_config_id: row.get(1)?,
                        process_name: row.get(2)?,
                        started_at: row.get(3)?,
                        stopped_at: row.get(4)?,
                        exit_code: row.get(5)?,
                        state: row.get(6)?,
                        error_count: row.get(7)?,
                    })
                })
                .map_err(|e| format!("Failed to query sessions: {}", e))?;

            for row in rows {
                sessions.push(row.map_err(|e| format!("Failed to read session row: {}", e))?);
            }
        }

        Ok(sessions)
    }

    /// Batch insert process session output lines.
    pub fn insert_process_session_output(
        &self,
        session_id: &str,
        lines: &[(String, String, String)], // (timestamp, stream, line)
    ) -> Result<(), String> {
        if lines.is_empty() {
            return Ok(());
        }
        let conn = self.get_conn()?;
        let tx = conn
            .unchecked_transaction()
            .map_err(|e| format!("Failed to begin transaction: {}", e))?;

        {
            let mut stmt = tx
                .prepare(
                    "INSERT INTO process_session_output (session_id, timestamp, stream, line)
                     VALUES (?1, ?2, ?3, ?4)",
                )
                .map_err(|e| format!("Failed to prepare insert: {}", e))?;

            for (timestamp, stream, line) in lines {
                stmt.execute(params![session_id, timestamp, stream, line])
                    .map_err(|e| format!("Failed to insert output line: {}", e))?;
            }
        }

        tx.commit()
            .map_err(|e| format!("Failed to commit output lines: {}", e))?;
        Ok(())
    }

    /// Get process session output lines.
    pub fn get_process_session_output(
        &self,
        session_id: &str,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<ProcessSessionOutputLine>, String> {
        let conn = self.get_conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT id, session_id, timestamp, stream, line
                 FROM process_session_output
                 WHERE session_id = ?1
                 ORDER BY id ASC
                 LIMIT ?2 OFFSET ?3",
            )
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        let rows = stmt
            .query_map(params![session_id, limit, offset], |row| {
                Ok(ProcessSessionOutputLine {
                    id: row.get(0)?,
                    session_id: row.get(1)?,
                    timestamp: row.get(2)?,
                    stream: row.get(3)?,
                    line: row.get(4)?,
                })
            })
            .map_err(|e| format!("Failed to query output: {}", e))?;

        let mut output = Vec::new();
        for row in rows {
            output.push(row.map_err(|e| format!("Failed to read output row: {}", e))?);
        }
        Ok(output)
    }

    /// Prune output lines for a session, keeping only the most recent `max_lines`.
    pub fn prune_session_output_lines(
        &self,
        session_id: &str,
        max_lines: u32,
    ) -> Result<u32, String> {
        let conn = self.get_conn()?;
        let deleted: usize = conn
            .execute(
                "DELETE FROM process_session_output
                 WHERE session_id = ?1
                 AND id NOT IN (
                     SELECT id FROM process_session_output
                     WHERE session_id = ?1
                     ORDER BY id DESC
                     LIMIT ?2
                 )",
                params![session_id, max_lines],
            )
            .map_err(|e| format!("Failed to prune session output: {}", e))?;
        Ok(deleted as u32)
    }

    /// Prune old sessions for a config, keeping the most recent `keep_count`.
    pub fn prune_old_process_sessions(
        &self,
        config_id: &str,
        keep_count: u32,
    ) -> Result<u32, String> {
        let conn = self.get_conn()?;
        let deleted: usize = conn
            .execute(
                "DELETE FROM process_sessions
                 WHERE process_config_id = ?1
                 AND id NOT IN (
                     SELECT id FROM process_sessions
                     WHERE process_config_id = ?1
                     ORDER BY started_at DESC
                     LIMIT ?2
                 )",
                params![config_id, keep_count],
            )
            .map_err(|e| format!("Failed to prune sessions: {}", e))?;
        Ok(deleted as u32)
    }

    // ========================================================================
    // Generator Evaluation - Pipeline Artifacts
    // ========================================================================

    /// Save a pipeline artifact from a generation run.
    pub fn save_pipeline_artifact(
        &self,
        artifact: &crate::workflow_generation::pipeline_artifacts::PipelineArtifact,
    ) -> Result<(), String> {
        let conn = self.get_conn()?;
        conn.execute(
            r#"INSERT INTO generation_pipeline_artifacts
                (id, workflow_id, task_run_id, description, category, created_at,
                 investigation_duration_ms, investigation_enriched_description,
                 specification_duration_ms, specification_criteria,
                 specification_prompt, builder_prompt, verification_prompts, hardener_prompt,
                 discovery_duration_ms, builder_duration_ms, autofix_duration_ms,
                 verification_duration_ms, hardener_duration_ms, total_duration_ms,
                 discovery_calls, builder_raw_output, builder_parsed_json, autofix_diff,
                 verification_iterations, fixer_snapshots, hardening_summary,
                 hardened_json, final_json, validation_errors,
                 success, error_message, model_used,
                 revision_duration_ms, quality_report, revision_cycles,
                 confidence_score)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                    ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27,
                    ?28, ?29, ?30, ?31, ?32, ?33, ?34, ?35, ?36, ?37)"#,
            params![
                artifact.id,
                artifact.workflow_id,
                artifact.task_run_id,
                artifact.description,
                artifact.category,
                artifact.created_at,
                artifact.investigation_duration_ms.map(|v| v as i64),
                artifact.investigation_enriched_description,
                artifact.specification_duration_ms.map(|v| v as i64),
                artifact
                    .specification_criteria
                    .as_ref()
                    .map(|v| v.to_string()),
                artifact.specification_prompt,
                artifact.builder_prompt,
                artifact
                    .verification_prompts
                    .as_ref()
                    .map(|v| v.to_string()),
                artifact.hardener_prompt,
                artifact.discovery_duration_ms.map(|v| v as i64),
                artifact.builder_duration_ms.map(|v| v as i64),
                artifact.autofix_duration_ms.map(|v| v as i64),
                artifact.verification_duration_ms.map(|v| v as i64),
                artifact.hardener_duration_ms.map(|v| v as i64),
                artifact.total_duration_ms.map(|v| v as i64),
                artifact.discovery_calls.as_ref().map(|v| v.to_string()),
                artifact.builder_raw_output,
                artifact.builder_parsed_json.as_ref().map(|v| v.to_string()),
                artifact.autofix_diff.as_ref().map(|v| v.to_string()),
                artifact
                    .verification_iterations
                    .as_ref()
                    .map(|v| v.to_string()),
                artifact
                    .fixer_snapshots
                    .as_ref()
                    .map(|v| serde_json::to_string(v).unwrap_or_default()),
                artifact.hardening_summary.as_ref().map(|v| v.to_string()),
                artifact.hardened_json.as_ref().map(|v| v.to_string()),
                artifact.final_json.as_ref().map(|v| v.to_string()),
                artifact.validation_errors.as_ref().map(|v| v.to_string()),
                artifact.success,
                artifact.error_message,
                artifact.model_used,
                artifact.revision_duration_ms.map(|v| v as i64),
                artifact.quality_report.as_ref().map(|v| v.to_string()),
                artifact.revision_cycles.map(|v| v as i32),
                artifact.confidence_score.map(|v| v as f64),
            ],
        )
        .map_err(|e| format!("Failed to save pipeline artifact: {}", e))?;
        Ok(())
    }

    /// Get a pipeline artifact by ID.
    pub fn get_pipeline_artifact(
        &self,
        id: &str,
    ) -> Result<Option<crate::workflow_generation::pipeline_artifacts::PipelineArtifact>, String>
    {
        let conn = self.get_conn()?;
        let result = conn.query_row(
            r#"SELECT id, workflow_id, task_run_id, description, category, created_at,
                      investigation_duration_ms, investigation_enriched_description,
                      specification_duration_ms, specification_criteria,
                      specification_prompt, builder_prompt, verification_prompts, hardener_prompt,
                      discovery_duration_ms, builder_duration_ms, autofix_duration_ms,
                      verification_duration_ms, hardener_duration_ms, total_duration_ms,
                      discovery_calls, builder_raw_output, builder_parsed_json, autofix_diff,
                      verification_iterations, fixer_snapshots, hardening_summary,
                      hardened_json, final_json, validation_errors,
                      success, error_message, model_used,
                      revision_duration_ms, quality_report, revision_cycles,
                      confidence_score
               FROM generation_pipeline_artifacts WHERE id = ?1"#,
            params![id],
            |row| Ok(Self::row_to_pipeline_artifact(row)),
        );
        match result {
            Ok(artifact) => Ok(Some(artifact)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("Failed to get pipeline artifact: {}", e)),
        }
    }

    /// List pipeline artifacts (paginated, newest first).
    pub fn list_pipeline_artifacts(
        &self,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<crate::workflow_generation::pipeline_artifacts::PipelineArtifactSummary>, String>
    {
        let conn = self.get_conn()?;
        let mut stmt = conn
            .prepare(
                r#"SELECT id, workflow_id, description, category, created_at,
                          total_duration_ms, success, model_used,
                          verification_iterations, hardening_summary
                   FROM generation_pipeline_artifacts
                   ORDER BY created_at DESC
                   LIMIT ?1 OFFSET ?2"#,
            )
            .map_err(|e| format!("Failed to prepare list query: {}", e))?;

        let rows = stmt
            .query_map(params![limit, offset], |row| {
                let verification_json: Option<String> = row.get(8)?;
                let hardening_json: Option<String> = row.get(9)?;

                let verification_count = verification_json
                    .and_then(|j| serde_json::from_str::<Vec<serde_json::Value>>(&j).ok())
                    .map(|v| v.len() as u32)
                    .unwrap_or(0);

                let hardener_count = hardening_json
                    .and_then(|j| serde_json::from_str::<serde_json::Value>(&j).ok())
                    .and_then(|v| v.get("converted_count")?.as_u64())
                    .unwrap_or(0) as u32;

                Ok(
                    crate::workflow_generation::pipeline_artifacts::PipelineArtifactSummary {
                        id: row.get(0)?,
                        workflow_id: row.get(1)?,
                        description: row.get(2)?,
                        category: row.get(3)?,
                        created_at: row.get(4)?,
                        total_duration_ms: row.get::<_, Option<i64>>(5)?.map(|v| v as u64),
                        success: row.get(6)?,
                        model_used: row.get(7)?,
                        verification_iteration_count: verification_count,
                        hardener_converted_count: hardener_count,
                    },
                )
            })
            .map_err(|e| format!("Failed to list pipeline artifacts: {}", e))?;

        let mut artifacts = Vec::new();
        for row in rows {
            artifacts.push(row.map_err(|e| format!("Row error: {}", e))?);
        }
        Ok(artifacts)
    }

    /// Get pipeline artifact for a specific workflow.
    pub fn get_pipeline_artifact_for_workflow(
        &self,
        workflow_id: &str,
    ) -> Result<Option<crate::workflow_generation::pipeline_artifacts::PipelineArtifact>, String>
    {
        let conn = self.get_conn()?;
        let result = conn.query_row(
            r#"SELECT id, workflow_id, task_run_id, description, category, created_at,
                      investigation_duration_ms, investigation_enriched_description,
                      specification_duration_ms, specification_criteria,
                      specification_prompt, builder_prompt, verification_prompts, hardener_prompt,
                      discovery_duration_ms, builder_duration_ms, autofix_duration_ms,
                      verification_duration_ms, hardener_duration_ms, total_duration_ms,
                      discovery_calls, builder_raw_output, builder_parsed_json, autofix_diff,
                      verification_iterations, fixer_snapshots, hardening_summary,
                      hardened_json, final_json, validation_errors,
                      success, error_message, model_used,
                      revision_duration_ms, quality_report, revision_cycles,
                      confidence_score
               FROM generation_pipeline_artifacts
               WHERE workflow_id = ?1
               ORDER BY created_at DESC LIMIT 1"#,
            params![workflow_id],
            |row| Ok(Self::row_to_pipeline_artifact(row)),
        );
        match result {
            Ok(artifact) => Ok(Some(artifact)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("Failed to get artifact for workflow: {}", e)),
        }
    }

    /// Delete pipeline artifacts older than N days.
    pub fn delete_pipeline_artifacts_older_than(&self, days: u32) -> Result<u32, String> {
        let conn = self.get_conn()?;
        let deleted = conn
            .execute(
                "DELETE FROM generation_pipeline_artifacts WHERE created_at < datetime('now', ?1)",
                params![format!("-{} days", days)],
            )
            .map_err(|e| format!("Failed to prune artifacts: {}", e))?;
        Ok(deleted as u32)
    }

    fn row_to_pipeline_artifact(
        row: &rusqlite::Row,
    ) -> crate::workflow_generation::pipeline_artifacts::PipelineArtifact {
        let parse_json = |s: Option<String>| -> Option<serde_json::Value> {
            s.and_then(|j| serde_json::from_str(&j).ok())
        };
        let parse_json_vec = |s: Option<String>| -> Option<Vec<serde_json::Value>> {
            s.and_then(|j| serde_json::from_str(&j).ok())
        };

        crate::workflow_generation::pipeline_artifacts::PipelineArtifact {
            id: row.get(0).unwrap_or_default(),
            workflow_id: row.get(1).unwrap_or(None),
            task_run_id: row.get(2).unwrap_or(None),
            description: row.get(3).unwrap_or_default(),
            category: row.get(4).unwrap_or(None),
            created_at: row.get(5).unwrap_or_default(),
            investigation_duration_ms: row
                .get::<_, Option<i64>>(6)
                .unwrap_or(None)
                .map(|v| v as u64),
            investigation_enriched_description: row.get(7).unwrap_or(None),
            specification_duration_ms: row
                .get::<_, Option<i64>>(8)
                .unwrap_or(None)
                .map(|v| v as u64),
            specification_criteria: parse_json(row.get(9).unwrap_or(None)),
            specification_prompt: row.get(10).unwrap_or(None),
            builder_prompt: row.get(11).unwrap_or(None),
            verification_prompts: parse_json(row.get(12).unwrap_or(None)),
            hardener_prompt: row.get(13).unwrap_or(None),
            discovery_duration_ms: row
                .get::<_, Option<i64>>(14)
                .unwrap_or(None)
                .map(|v| v as u64),
            builder_duration_ms: row
                .get::<_, Option<i64>>(15)
                .unwrap_or(None)
                .map(|v| v as u64),
            autofix_duration_ms: row
                .get::<_, Option<i64>>(16)
                .unwrap_or(None)
                .map(|v| v as u64),
            verification_duration_ms: row
                .get::<_, Option<i64>>(17)
                .unwrap_or(None)
                .map(|v| v as u64),
            hardener_duration_ms: row
                .get::<_, Option<i64>>(18)
                .unwrap_or(None)
                .map(|v| v as u64),
            total_duration_ms: row
                .get::<_, Option<i64>>(19)
                .unwrap_or(None)
                .map(|v| v as u64),
            discovery_calls: parse_json(row.get(20).unwrap_or(None)),
            builder_raw_output: row.get(21).unwrap_or(None),
            builder_parsed_json: parse_json(row.get(22).unwrap_or(None)),
            autofix_diff: parse_json(row.get(23).unwrap_or(None)),
            verification_iterations: parse_json(row.get(24).unwrap_or(None)),
            fixer_snapshots: parse_json_vec(row.get(25).unwrap_or(None)),
            hardening_summary: parse_json(row.get(26).unwrap_or(None)),
            hardened_json: parse_json(row.get(27).unwrap_or(None)),
            final_json: parse_json(row.get(28).unwrap_or(None)),
            validation_errors: parse_json(row.get(29).unwrap_or(None)),
            success: row.get(30).unwrap_or(true),
            error_message: row.get(31).unwrap_or(None),
            model_used: row.get(32).unwrap_or(None),
            revision_duration_ms: row
                .get::<_, Option<i64>>(33)
                .unwrap_or(None)
                .map(|v| v as u64),
            quality_report: parse_json(row.get(34).unwrap_or(None)),
            revision_cycles: row
                .get::<_, Option<i32>>(35)
                .unwrap_or(None)
                .map(|v| v as u32),
            confidence_score: row
                .get::<_, Option<f64>>(36)
                .unwrap_or(None)
                .map(|v| v as f32),
        }
    }

    // ========================================================================
    // Generator Evaluation - Dashboard Metrics
    // ========================================================================

    /// Get aggregated dashboard metrics for generator evaluation.
    pub fn get_generation_dashboard_metrics(&self) -> Result<GeneratorDashboardMetrics, String> {
        let conn = self.get_conn()?;

        // Total generations and success rate from pipeline artifacts
        let (total_generations, successful_generations, avg_total_duration): (
            i64,
            i64,
            Option<f64>,
        ) = conn
            .query_row(
                r#"SELECT
                    COUNT(*) as total,
                    SUM(CASE WHEN success = 1 THEN 1 ELSE 0 END) as successes,
                    AVG(total_duration_ms) as avg_duration
                FROM generation_pipeline_artifacts"#,
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap_or((0, 0, None));

        // Average verification iterations
        let avg_verification_iterations: Option<f64> = conn
            .query_row(
                r#"SELECT AVG(json_array_length(verification_iterations))
                FROM generation_pipeline_artifacts
                WHERE verification_iterations IS NOT NULL AND verification_iterations != '[]'"#,
                [],
                |row| row.get(0),
            )
            .unwrap_or(None);

        // Verification first-pass rate (iterations with 0 issues on first pass)
        let first_pass_rate: Option<f64> = conn
            .query_row(
                r#"SELECT
                    CAST(SUM(CASE
                        WHEN json_extract(verification_iterations, '$[0].issues') = '[]'
                        THEN 1 ELSE 0
                    END) AS REAL) / NULLIF(COUNT(*), 0)
                FROM generation_pipeline_artifacts
                WHERE verification_iterations IS NOT NULL AND verification_iterations != '[]'"#,
                [],
                |row| row.get(0),
            )
            .unwrap_or(None);

        // Hardener conversion rate
        let (total_hardened, total_converted): (i64, i64) = conn
            .query_row(
                r#"SELECT
                    COUNT(*) as total,
                    SUM(COALESCE(json_extract(hardening_summary, '$.converted_count'), 0)) as converted
                FROM generation_pipeline_artifacts
                WHERE hardening_summary IS NOT NULL"#,
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap_or((0, 0));

        // User feedback metrics from workflow_generation_feedback
        let (total_edits, total_deletes, total_ratings, avg_rating): (i64, i64, i64, Option<f64>) =
            conn.query_row(
                r#"SELECT
                    SUM(CASE WHEN feedback_type = 'edit' THEN 1 ELSE 0 END),
                    SUM(CASE WHEN feedback_type = 'delete' THEN 1 ELSE 0 END),
                    SUM(CASE WHEN feedback_type = 'rating' THEN 1 ELSE 0 END),
                    AVG(CASE WHEN feedback_type = 'rating' THEN rating ELSE NULL END)
                FROM workflow_generation_feedback"#,
                [],
                |row| {
                    Ok((
                        row.get(0).unwrap_or(0),
                        row.get(1).unwrap_or(0),
                        row.get(2).unwrap_or(0),
                        row.get(3)?,
                    ))
                },
            )
            .unwrap_or((0, 0, 0, None));

        Ok(GeneratorDashboardMetrics {
            total_generations,
            successful_generations,
            success_rate: if total_generations > 0 {
                successful_generations as f64 / total_generations as f64
            } else {
                0.0
            },
            avg_total_duration_ms: avg_total_duration,
            avg_verification_iterations,
            first_pass_rate,
            hardener_total_processed: total_hardened,
            hardener_total_converted: total_converted,
            total_edits,
            total_deletes,
            total_ratings,
            avg_rating,
        })
    }

    /// Get generation metrics over time (daily aggregates).
    pub fn get_generation_metrics_over_time(
        &self,
        days: u32,
    ) -> Result<Vec<GeneratorTimeSeriesPoint>, String> {
        let conn = self.get_conn()?;
        let mut stmt = conn
            .prepare(
                r#"SELECT
                    date(created_at) as day,
                    COUNT(*) as total,
                    SUM(CASE WHEN success = 1 THEN 1 ELSE 0 END) as successes,
                    AVG(total_duration_ms) as avg_duration,
                    AVG(json_array_length(verification_iterations)) as avg_iterations
                FROM generation_pipeline_artifacts
                WHERE created_at >= datetime('now', ?1)
                GROUP BY date(created_at)
                ORDER BY day ASC"#,
            )
            .map_err(|e| format!("Failed to prepare trends query: {}", e))?;

        let rows = stmt
            .query_map(params![format!("-{} days", days)], |row| {
                Ok(GeneratorTimeSeriesPoint {
                    date: row.get(0)?,
                    total_generations: row.get(1)?,
                    successful_generations: row.get(2)?,
                    avg_duration_ms: row.get(3)?,
                    avg_verification_iterations: row.get(4)?,
                })
            })
            .map_err(|e| format!("Failed to query trends: {}", e))?;

        let mut points = Vec::new();
        for row in rows {
            points.push(row.map_err(|e| format!("Row error: {}", e))?);
        }
        Ok(points)
    }

    // ========================================================================
    // Generator Evaluation - Benchmarks
    // ========================================================================

    /// Save a new benchmark.
    pub fn save_benchmark(&self, benchmark: &GeneratorBenchmark) -> Result<(), String> {
        let conn = self.get_conn()?;
        conn.execute(
            r#"INSERT INTO generator_benchmarks
                (id, name, description, category, tags, expected_structure, created_at, updated_at, enabled)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)"#,
            params![
                benchmark.id,
                benchmark.name,
                benchmark.description,
                benchmark.category,
                benchmark.tags.as_ref().map(|t| serde_json::to_string(t).unwrap_or_default()),
                serde_json::to_string(&benchmark.expected_structure).unwrap_or_default(),
                benchmark.created_at,
                benchmark.updated_at,
                benchmark.enabled,
            ],
        )
        .map_err(|e| format!("Failed to save benchmark: {}", e))?;
        Ok(())
    }

    /// Get a benchmark by ID.
    pub fn get_benchmark(&self, id: &str) -> Result<Option<GeneratorBenchmark>, String> {
        let conn = self.get_conn()?;
        let result = conn.query_row(
            r#"SELECT id, name, description, category, tags, expected_structure, created_at, updated_at, enabled
               FROM generator_benchmarks WHERE id = ?1"#,
            params![id],
            |row| Ok(Self::row_to_benchmark(row)),
        );
        match result {
            Ok(b) => Ok(Some(b)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("Failed to get benchmark: {}", e)),
        }
    }

    /// List all benchmarks.
    pub fn list_benchmarks(&self) -> Result<Vec<GeneratorBenchmark>, String> {
        let conn = self.get_conn()?;
        let mut stmt = conn
            .prepare(
                r#"SELECT id, name, description, category, tags, expected_structure, created_at, updated_at, enabled
                   FROM generator_benchmarks ORDER BY created_at DESC"#,
            )
            .map_err(|e| format!("Failed to prepare: {}", e))?;

        let rows = stmt
            .query_map([], |row| Ok(Self::row_to_benchmark(row)))
            .map_err(|e| format!("Failed to list benchmarks: {}", e))?;

        let mut benchmarks = Vec::new();
        for row in rows {
            benchmarks.push(row.map_err(|e| format!("Row error: {}", e))?);
        }
        Ok(benchmarks)
    }

    /// Check if a benchmark with the given name already exists.
    pub fn benchmark_exists_by_name(&self, name: &str) -> Result<bool, String> {
        let conn = self.get_conn()?;
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM generator_benchmarks WHERE name = ?1",
                params![name],
                |row| row.get(0),
            )
            .map_err(|e| format!("Failed to check benchmark: {}", e))?;
        Ok(count > 0)
    }

    /// Update a benchmark.
    pub fn update_benchmark(&self, id: &str, update: &BenchmarkUpdate) -> Result<(), String> {
        let conn = self.get_conn()?;
        let now = chrono::Utc::now().to_rfc3339();

        if let Some(ref name) = update.name {
            conn.execute(
                "UPDATE generator_benchmarks SET name = ?1, updated_at = ?2 WHERE id = ?3",
                params![name, now, id],
            )
            .map_err(|e| format!("Failed to update name: {}", e))?;
        }
        if let Some(ref description) = update.description {
            conn.execute(
                "UPDATE generator_benchmarks SET description = ?1, updated_at = ?2 WHERE id = ?3",
                params![description, now, id],
            )
            .map_err(|e| format!("Failed to update description: {}", e))?;
        }
        if let Some(ref expected) = update.expected_structure {
            let json = serde_json::to_string(expected).unwrap_or_default();
            conn.execute(
                "UPDATE generator_benchmarks SET expected_structure = ?1, updated_at = ?2 WHERE id = ?3",
                params![json, now, id],
            )
            .map_err(|e| format!("Failed to update expected_structure: {}", e))?;
        }
        if let Some(enabled) = update.enabled {
            conn.execute(
                "UPDATE generator_benchmarks SET enabled = ?1, updated_at = ?2 WHERE id = ?3",
                params![enabled, now, id],
            )
            .map_err(|e| format!("Failed to update enabled: {}", e))?;
        }
        Ok(())
    }

    /// Delete a benchmark and its results.
    pub fn delete_benchmark(&self, id: &str) -> Result<(), String> {
        let conn = self.get_conn()?;
        conn.execute(
            "DELETE FROM generator_benchmark_results WHERE benchmark_id = ?1",
            params![id],
        )
        .map_err(|e| format!("Failed to delete results: {}", e))?;
        conn.execute(
            "DELETE FROM generator_benchmarks WHERE id = ?1",
            params![id],
        )
        .map_err(|e| format!("Failed to delete benchmark: {}", e))?;
        Ok(())
    }

    /// Save a benchmark result.
    pub fn save_benchmark_result(&self, result: &GeneratorBenchmarkResult) -> Result<(), String> {
        let conn = self.get_conn()?;
        conn.execute(
            r#"INSERT INTO generator_benchmark_results
                (id, benchmark_id, artifact_id, run_at, model_used,
                 structure_score, content_score, step_type_score, overall_score,
                 score_breakdown, generated_json, duration_ms, passed, notes)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)"#,
            params![
                result.id,
                result.benchmark_id,
                result.artifact_id,
                result.run_at,
                result.model_used,
                result.structure_score,
                result.content_score,
                result.step_type_score,
                result.overall_score,
                result.score_breakdown.as_ref().map(|v| v.to_string()),
                result.generated_json.as_ref().map(|v| v.to_string()),
                result.duration_ms.map(|v| v as i64),
                result.passed,
                result.notes,
            ],
        )
        .map_err(|e| format!("Failed to save benchmark result: {}", e))?;
        Ok(())
    }

    /// List results for a specific benchmark.
    pub fn list_benchmark_results(
        &self,
        benchmark_id: &str,
        limit: u32,
    ) -> Result<Vec<GeneratorBenchmarkResult>, String> {
        let conn = self.get_conn()?;
        let mut stmt = conn
            .prepare(
                r#"SELECT id, benchmark_id, artifact_id, run_at, model_used,
                          structure_score, content_score, step_type_score, overall_score,
                          score_breakdown, generated_json, duration_ms, passed, notes
                   FROM generator_benchmark_results
                   WHERE benchmark_id = ?1
                   ORDER BY run_at DESC
                   LIMIT ?2"#,
            )
            .map_err(|e| format!("Failed to prepare: {}", e))?;

        let rows = stmt
            .query_map(params![benchmark_id, limit], |row| {
                Ok(GeneratorBenchmarkResult {
                    id: row.get(0)?,
                    benchmark_id: row.get(1)?,
                    artifact_id: row.get(2)?,
                    run_at: row.get(3)?,
                    model_used: row.get(4)?,
                    structure_score: row.get(5)?,
                    content_score: row.get(6)?,
                    step_type_score: row.get(7)?,
                    overall_score: row.get(8)?,
                    score_breakdown: row
                        .get::<_, Option<String>>(9)?
                        .and_then(|s| serde_json::from_str(&s).ok()),
                    generated_json: row
                        .get::<_, Option<String>>(10)?
                        .and_then(|s| serde_json::from_str(&s).ok()),
                    duration_ms: row.get::<_, Option<i64>>(11)?.map(|v| v as u64),
                    passed: row.get(12)?,
                    notes: row.get(13)?,
                })
            })
            .map_err(|e| format!("Failed to list results: {}", e))?;

        let mut results = Vec::new();
        for row in rows {
            results.push(row.map_err(|e| format!("Row error: {}", e))?);
        }
        Ok(results)
    }

    fn row_to_benchmark(row: &rusqlite::Row) -> GeneratorBenchmark {
        let tags_json: Option<String> = row.get(4).unwrap_or(None);
        let expected_json: String = row.get(5).unwrap_or_default();
        GeneratorBenchmark {
            id: row.get(0).unwrap_or_default(),
            name: row.get(1).unwrap_or_default(),
            description: row.get(2).unwrap_or_default(),
            category: row.get(3).unwrap_or(None),
            tags: tags_json.and_then(|j| serde_json::from_str(&j).ok()),
            expected_structure: serde_json::from_str(&expected_json).unwrap_or_default(),
            created_at: row.get(6).unwrap_or_default(),
            updated_at: row.get(7).unwrap_or_default(),
            enabled: row.get(8).unwrap_or(true),
        }
    }

    // ========================================================================
    // Generator Evaluation - Edit Analysis
    // ========================================================================

    /// Get aggregated edit analysis from workflow_generation_feedback.
    pub fn get_edit_analysis(&self) -> Result<EditAnalysis, String> {
        let conn = self.get_conn()?;

        // Most commonly edited fields
        let mut stmt = conn
            .prepare(
                r#"SELECT edited_field, COUNT(*) as cnt
                   FROM workflow_generation_feedback
                   WHERE feedback_type = 'edit' AND edited_field IS NOT NULL
                   GROUP BY edited_field
                   ORDER BY cnt DESC
                   LIMIT 20"#,
            )
            .map_err(|e| format!("Failed to prepare: {}", e))?;

        let edited_fields: Vec<(String, i64)> = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .map_err(|e| format!("Failed to query: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        // Feedback type distribution
        let mut stmt = conn
            .prepare(
                r#"SELECT feedback_type, COUNT(*) as cnt
                   FROM workflow_generation_feedback
                   GROUP BY feedback_type
                   ORDER BY cnt DESC"#,
            )
            .map_err(|e| format!("Failed to prepare: {}", e))?;

        let type_distribution: Vec<(String, i64)> = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .map_err(|e| format!("Failed to query: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        // Rating distribution
        let mut stmt = conn
            .prepare(
                r#"SELECT rating, COUNT(*) as cnt
                   FROM workflow_generation_feedback
                   WHERE feedback_type = 'rating' AND rating IS NOT NULL
                   GROUP BY rating
                   ORDER BY rating"#,
            )
            .map_err(|e| format!("Failed to prepare: {}", e))?;

        let rating_distribution: Vec<(i32, i64)> = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .map_err(|e| format!("Failed to query: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        // Recent edits
        let mut stmt = conn
            .prepare(
                r#"SELECT f.id, f.workflow_id, f.feedback_type, f.edited_field,
                          f.old_value, f.new_value, f.created_at,
                          w.name as workflow_name
                   FROM workflow_generation_feedback f
                   LEFT JOIN unified_workflows w ON f.workflow_id = w.id
                   ORDER BY f.created_at DESC
                   LIMIT 50"#,
            )
            .map_err(|e| format!("Failed to prepare: {}", e))?;

        let recent_feedback: Vec<RecentFeedback> = stmt
            .query_map([], |row| {
                Ok(RecentFeedback {
                    id: row.get(0)?,
                    workflow_id: row.get(1)?,
                    feedback_type: row.get(2)?,
                    edited_field: row.get(3)?,
                    old_value: row.get(4)?,
                    new_value: row.get(5)?,
                    created_at: row.get(6)?,
                    workflow_name: row.get(7)?,
                })
            })
            .map_err(|e| format!("Failed to query: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(EditAnalysis {
            edited_fields,
            type_distribution,
            rating_distribution,
            recent_feedback,
        })
    }

    /// Save a feedback entry to workflow_generation_feedback.
    pub fn save_generator_feedback(
        &self,
        id: &str,
        workflow_id: &str,
        workflow_name: Option<&str>,
        feedback_type: &str,
        edited_field: Option<&str>,
        old_value: Option<&str>,
        new_value: Option<&str>,
        rating: Option<i32>,
        created_at: &str,
    ) -> Result<(), String> {
        let conn = self.get_conn()?;
        conn.execute(
            r#"INSERT INTO workflow_generation_feedback
                (id, workflow_id, feedback_type, edited_field, old_value, new_value, rating, workflow_description, created_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)"#,
            params![
                id,
                workflow_id,
                feedback_type,
                edited_field,
                old_value,
                new_value,
                rating,
                workflow_name,
                created_at,
            ],
        )
        .map_err(|e| format!("Failed to save generator feedback: {}", e))?;
        Ok(())
    }

    // ========================================================================
    // Generator Evaluation - Example Library
    // ========================================================================

    /// List workflows that have example_status set.
    pub fn list_example_workflows(&self) -> Result<Vec<ExampleWorkflowSummary>, String> {
        let conn = self.get_conn()?;
        let mut stmt = conn
            .prepare(
                r#"SELECT id, name, description, category, example_status, created_at
                   FROM unified_workflows
                   WHERE example_status IS NOT NULL AND example_status != ''
                   ORDER BY example_status, name"#,
            )
            .map_err(|e| format!("Failed to prepare: {}", e))?;

        let rows = stmt
            .query_map([], |row| {
                Ok(ExampleWorkflowSummary {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    description: row.get(2)?,
                    category: row.get(3)?,
                    example_status: row.get(4)?,
                    created_at: row.get(5)?,
                })
            })
            .map_err(|e| format!("Failed to query: {}", e))?;

        let mut examples = Vec::new();
        for row in rows {
            examples.push(row.map_err(|e| format!("Row error: {}", e))?);
        }
        Ok(examples)
    }

    /// Update a workflow's example_status.
    pub fn update_example_status(
        &self,
        workflow_id: &str,
        status: Option<&str>,
    ) -> Result<(), String> {
        let conn = self.get_conn()?;
        conn.execute(
            "UPDATE unified_workflows SET example_status = ?1 WHERE id = ?2",
            params![status, workflow_id],
        )
        .map_err(|e| format!("Failed to update example_status: {}", e))?;
        Ok(())
    }

    // ========================================================================
    // Canvas Panels (A2UI)
    // ========================================================================

    /// Insert or update a canvas panel.
    pub fn insert_or_update_canvas_panel(
        &self,
        panel: &crate::mcp::canvas::StoredPanel,
    ) -> Result<(), String> {
        let conn = self.get_conn()?;
        let data_json = panel.data.to_string();

        conn.execute(
            r#"
            INSERT INTO canvas_panels (id, task_run_id, component, title, data_json, priority, size, group_name, created_at, updated_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
            ON CONFLICT(id) DO UPDATE SET
                component = ?3,
                title = ?4,
                data_json = ?5,
                priority = ?6,
                size = ?7,
                group_name = ?8,
                updated_at = ?10
            "#,
            params![
                panel.panel_id,
                panel.task_run_id,
                panel.component,
                panel.title,
                data_json,
                panel.priority,
                panel.size,
                panel.group,
                panel.created_at,
                panel.updated_at,
            ],
        )
        .map_err(|e| format!("Failed to upsert canvas panel: {}", e))?;

        Ok(())
    }

    /// Get all canvas panels for a task run.
    pub fn get_canvas_panels_for_task_run(
        &self,
        task_run_id: &str,
    ) -> Result<Vec<crate::mcp::canvas::StoredPanel>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, task_run_id, component, title, data_json, priority, size, group_name, created_at, updated_at
                FROM canvas_panels
                WHERE task_run_id = ?1
                ORDER BY priority ASC, created_at ASC
                "#,
            )
            .map_err(|e| format!("Failed to prepare canvas panels query: {}", e))?;

        let panels = stmt
            .query_map(params![task_run_id], |row| {
                let data_json: String = row.get(4)?;
                let data: serde_json::Value =
                    serde_json::from_str(&data_json).unwrap_or(serde_json::json!({}));
                Ok(crate::mcp::canvas::StoredPanel {
                    panel_id: row.get(0)?,
                    task_run_id: row.get(1)?,
                    component: row.get(2)?,
                    title: row.get(3)?,
                    data,
                    priority: row.get(5)?,
                    size: row.get(6)?,
                    group: row.get(7)?,
                    created_at: row.get(8)?,
                    updated_at: row.get(9)?,
                })
            })
            .map_err(|e| format!("Failed to query canvas panels: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(panels)
    }

    /// Delete a single canvas panel.
    pub fn delete_canvas_panel(&self, panel_id: &str) -> Result<bool, String> {
        let conn = self.get_conn()?;
        let rows = conn
            .execute("DELETE FROM canvas_panels WHERE id = ?1", params![panel_id])
            .map_err(|e| format!("Failed to delete canvas panel: {}", e))?;
        Ok(rows > 0)
    }

    /// Clear all canvas panels for a task run.
    pub fn clear_canvas_panels_for_task_run(&self, task_run_id: &str) -> Result<usize, String> {
        let conn = self.get_conn()?;
        let rows = conn
            .execute(
                "DELETE FROM canvas_panels WHERE task_run_id = ?1",
                params![task_run_id],
            )
            .map_err(|e| format!("Failed to clear canvas panels: {}", e))?;
        Ok(rows)
    }

    // ========================================================================
    // Approval Gate Operations
    // ========================================================================

    /// Record a new approval gate request (audit trail).
    pub fn insert_approval_gate(
        &self,
        id: &str,
        task_run_id: &str,
        iteration: u32,
        prompt: &str,
        context_json: &str,
    ) -> Result<(), String> {
        let conn = self.get_conn()?;
        conn.execute(
            "INSERT INTO approval_gates (id, task_run_id, iteration, prompt, context_json, status, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, 'pending', datetime('now'))",
            rusqlite::params![id, task_run_id, iteration as i64, prompt, context_json],
        )
        .map_err(|e| format!("Failed to insert approval gate: {}", e))?;
        Ok(())
    }

    /// Resolve an approval gate (record human response).
    pub fn resolve_approval_gate(
        &self,
        id: &str,
        action: &str,
        comment: Option<&str>,
    ) -> Result<(), String> {
        let status = match action {
            "approve" => "approved",
            "reject" => "rejected",
            "abort" => "aborted",
            _ => action,
        };
        let conn = self.get_conn()?;
        conn.execute(
            "UPDATE approval_gates SET action = ?1, comment = ?2, status = ?3, resolved_at = datetime('now') WHERE id = ?4",
            rusqlite::params![action, comment, status, id],
        )
        .map_err(|e| format!("Failed to resolve approval gate: {}", e))?;
        Ok(())
    }

    /// Get approval gate history for a task run.
    pub fn get_approval_gates_for_task_run(
        &self,
        task_run_id: &str,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self.get_conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT id, task_run_id, iteration, prompt, context_json, action, comment, status, created_at, resolved_at \
                 FROM approval_gates WHERE task_run_id = ?1 ORDER BY created_at ASC",
            )
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        let rows = stmt
            .query_map(rusqlite::params![task_run_id], |row| {
                Ok(serde_json::json!({
                    "id": row.get::<_, String>(0)?,
                    "task_run_id": row.get::<_, String>(1)?,
                    "iteration": row.get::<_, i64>(2)?,
                    "prompt": row.get::<_, String>(3)?,
                    "context_json": row.get::<_, String>(4).unwrap_or_default(),
                    "action": row.get::<_, Option<String>>(5)?,
                    "comment": row.get::<_, Option<String>>(6)?,
                    "status": row.get::<_, String>(7)?,
                    "created_at": row.get::<_, String>(8)?,
                    "resolved_at": row.get::<_, Option<String>>(9)?,
                }))
            })
            .map_err(|e| format!("Failed to query approval gates: {}", e))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("Failed to collect approval gates: {}", e))?;

        Ok(rows)
    }

    // ========================================================================
    // User Skills CRUD
    // ========================================================================

    /// List all user-created skills.
    pub fn list_user_skills(&self) -> Result<Vec<crate::skills::SkillDefinition>, String> {
        let conn = self.get_conn()?;

        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, name, slug, description, category, tags, icon, color,
                       allowed_phases, parameters, template, source,
                       version, author, checksum, depends_on, usage_count, approval_status, forked_from,
                       created_at, updated_at
                FROM user_skills
                ORDER BY updated_at DESC
                "#,
            )
            .map_err(|e| format!("Failed to prepare user_skills query: {}", e))?;

        let skills = stmt
            .query_map([], |row| {
                let tags_json: String =
                    row.get::<_, String>(5).unwrap_or_else(|_| "[]".to_string());
                let allowed_phases_json: String = row
                    .get::<_, String>(8)
                    .unwrap_or_else(|_| "[\"setup\"]".to_string());
                let parameters_json: String =
                    row.get::<_, String>(9).unwrap_or_else(|_| "[]".to_string());
                let template_json: String = row.get(10)?;

                Ok(crate::skills::SkillDefinition {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    slug: row.get(2)?,
                    description: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
                    category: row
                        .get::<_, Option<String>>(4)?
                        .unwrap_or_else(|| "custom".to_string()),
                    tags: serde_json::from_str(&tags_json).unwrap_or_default(),
                    icon: row
                        .get::<_, Option<String>>(6)?
                        .unwrap_or_else(|| "puzzle".to_string()),
                    color: row
                        .get::<_, Option<String>>(7)?
                        .unwrap_or_else(|| "gray".to_string()),
                    allowed_phases: serde_json::from_str(&allowed_phases_json)
                        .unwrap_or_else(|_| vec!["setup".to_string()]),
                    parameters: serde_json::from_str(&parameters_json).unwrap_or_default(),
                    template: serde_json::from_str(&template_json).unwrap_or(
                        crate::skills::SkillTemplate::SingleStep {
                            step: std::collections::HashMap::new(),
                        },
                    ),
                    source: row
                        .get::<_, Option<String>>(11)?
                        .unwrap_or_else(|| "user".to_string()),
                    version: row.get::<_, Option<String>>(12)?,
                    author: row
                        .get::<_, Option<String>>(13)?
                        .and_then(|s| serde_json::from_str(&s).ok()),
                    checksum: row.get::<_, Option<String>>(14)?,
                    depends_on: row
                        .get::<_, Option<String>>(15)?
                        .and_then(|s| serde_json::from_str(&s).ok()),
                    usage_count: row.get::<_, Option<u64>>(16)?,
                    approval_status: row.get::<_, Option<String>>(17)?,
                    forked_from: row.get::<_, Option<String>>(18)?,
                })
            })
            .map_err(|e| format!("Failed to query user skills: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(skills)
    }

    /// Get a single user skill by ID.
    pub fn get_user_skill(
        &self,
        id: &str,
    ) -> Result<Option<crate::skills::SkillDefinition>, String> {
        let conn = self.get_conn()?;

        let result = conn.query_row(
            r#"
            SELECT id, name, slug, description, category, tags, icon, color,
                   allowed_phases, parameters, template, source,
                   version, author, checksum, depends_on, usage_count, approval_status, forked_from,
                   created_at, updated_at
            FROM user_skills
            WHERE id = ?1
            "#,
            params![id],
            |row| {
                let tags_json: String =
                    row.get::<_, String>(5).unwrap_or_else(|_| "[]".to_string());
                let allowed_phases_json: String = row
                    .get::<_, String>(8)
                    .unwrap_or_else(|_| "[\"setup\"]".to_string());
                let parameters_json: String =
                    row.get::<_, String>(9).unwrap_or_else(|_| "[]".to_string());
                let template_json: String = row.get(10)?;

                Ok(crate::skills::SkillDefinition {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    slug: row.get(2)?,
                    description: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
                    category: row
                        .get::<_, Option<String>>(4)?
                        .unwrap_or_else(|| "custom".to_string()),
                    tags: serde_json::from_str(&tags_json).unwrap_or_default(),
                    icon: row
                        .get::<_, Option<String>>(6)?
                        .unwrap_or_else(|| "puzzle".to_string()),
                    color: row
                        .get::<_, Option<String>>(7)?
                        .unwrap_or_else(|| "gray".to_string()),
                    allowed_phases: serde_json::from_str(&allowed_phases_json)
                        .unwrap_or_else(|_| vec!["setup".to_string()]),
                    parameters: serde_json::from_str(&parameters_json).unwrap_or_default(),
                    template: serde_json::from_str(&template_json).unwrap_or(
                        crate::skills::SkillTemplate::SingleStep {
                            step: std::collections::HashMap::new(),
                        },
                    ),
                    source: row
                        .get::<_, Option<String>>(11)?
                        .unwrap_or_else(|| "user".to_string()),
                    version: row.get::<_, Option<String>>(12)?,
                    author: row
                        .get::<_, Option<String>>(13)?
                        .and_then(|s| serde_json::from_str(&s).ok()),
                    checksum: row.get::<_, Option<String>>(14)?,
                    depends_on: row
                        .get::<_, Option<String>>(15)?
                        .and_then(|s| serde_json::from_str(&s).ok()),
                    usage_count: row.get::<_, Option<u64>>(16)?,
                    approval_status: row.get::<_, Option<String>>(17)?,
                    forked_from: row.get::<_, Option<String>>(18)?,
                })
            },
        );

        match result {
            Ok(skill) => Ok(Some(skill)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("Failed to get user skill: {}", e)),
        }
    }

    /// Create a new user skill.
    pub fn create_user_skill(
        &self,
        request: &crate::mcp::skills::CreateSkillRequest,
    ) -> Result<crate::skills::SkillDefinition, String> {
        let conn = self.get_conn()?;
        let id = format!("user:{}", request.slug);
        let now = Utc::now().to_rfc3339();

        let tags_json = serde_json::to_string(&request.tags).unwrap_or_else(|_| "[]".to_string());
        let allowed_phases_json =
            serde_json::to_string(&request.allowed_phases).unwrap_or_else(|_| "[]".to_string());
        let parameters_json =
            serde_json::to_string(&request.parameters).unwrap_or_else(|_| "[]".to_string());
        let template_json = serde_json::to_string(&request.template)
            .map_err(|e| format!("Failed to serialize template: {}", e))?;

        conn.execute(
            r#"
            INSERT INTO user_skills (
                id, name, slug, description, category, tags, icon, color,
                allowed_phases, parameters, template, source,
                version, author, checksum, depends_on, usage_count, approval_status, forked_from,
                created_at, updated_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21)
            "#,
            params![
                id,
                request.name,
                request.slug,
                request.description,
                request.category,
                tags_json,
                request.icon,
                request.color,
                allowed_phases_json,
                parameters_json,
                template_json,
                "user",
                "1.0.0",                    // version
                Option::<String>::None,      // author
                Option::<String>::None,      // checksum
                "[]",                        // depends_on
                0i64,                        // usage_count
                Option::<String>::None,      // approval_status
                Option::<String>::None,      // forked_from
                now,
                now,
            ],
        )
        .map_err(|e| format!("Failed to create user skill: {}", e))?;

        self.get_user_skill(&id)?
            .ok_or_else(|| "Failed to retrieve created skill".to_string())
    }

    /// Update a user skill.
    pub fn update_user_skill(
        &self,
        id: &str,
        request: &crate::mcp::skills::UpdateSkillRequest,
    ) -> Result<crate::skills::SkillDefinition, String> {
        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();

        let current = self
            .get_user_skill(id)?
            .ok_or_else(|| format!("User skill not found: {}", id))?;

        let name = request.name.as_ref().unwrap_or(&current.name);
        let slug = request.slug.as_ref().unwrap_or(&current.slug);
        let description = request.description.as_ref().unwrap_or(&current.description);
        let category = request.category.as_ref().unwrap_or(&current.category);
        let tags = request.tags.as_ref().unwrap_or(&current.tags);
        let icon = request.icon.as_ref().unwrap_or(&current.icon);
        let color = request.color.as_ref().unwrap_or(&current.color);
        let allowed_phases = request
            .allowed_phases
            .as_ref()
            .unwrap_or(&current.allowed_phases);
        let parameters = request.parameters.as_ref().unwrap_or(&current.parameters);
        let template = request.template.as_ref().unwrap_or(&current.template);

        let tags_json = serde_json::to_string(tags).unwrap_or_else(|_| "[]".to_string());
        let allowed_phases_json =
            serde_json::to_string(allowed_phases).unwrap_or_else(|_| "[]".to_string());
        let parameters_json =
            serde_json::to_string(parameters).unwrap_or_else(|_| "[]".to_string());
        let template_json = serde_json::to_string(template)
            .map_err(|e| format!("Failed to serialize template: {}", e))?;

        // Update the ID if slug changed
        let new_id = format!("user:{}", slug);

        conn.execute(
            r#"
            UPDATE user_skills SET
                id = ?1, name = ?2, slug = ?3, description = ?4, category = ?5,
                tags = ?6, icon = ?7, color = ?8, allowed_phases = ?9,
                parameters = ?10, template = ?11, updated_at = ?12
            WHERE id = ?13
            "#,
            params![
                new_id,
                name,
                slug,
                description,
                category,
                tags_json,
                icon,
                color,
                allowed_phases_json,
                parameters_json,
                template_json,
                now,
                id,
            ],
        )
        .map_err(|e| format!("Failed to update user skill: {}", e))?;

        self.get_user_skill(&new_id)?
            .ok_or_else(|| "Failed to retrieve updated skill".to_string())
    }

    /// Export user skills for sharing.
    /// If `ids` is empty, exports all non-builtin skills.
    pub fn export_user_skills(
        &self,
        ids: &[String],
    ) -> Result<Vec<crate::skills::SkillDefinition>, String> {
        if ids.is_empty() {
            return self.list_user_skills();
        }

        let conn = self.get_conn()?;
        let placeholders: Vec<String> = (1..=ids.len()).map(|i| format!("?{}", i)).collect();
        let query = format!(
            r#"SELECT id, name, slug, description, category, tags, icon, color,
                      allowed_phases, parameters, template, source,
                      version, author, checksum, depends_on, usage_count, approval_status, forked_from,
                      created_at, updated_at
               FROM user_skills
               WHERE id IN ({})
               ORDER BY updated_at DESC"#,
            placeholders.join(", ")
        );

        let mut stmt = conn
            .prepare(&query)
            .map_err(|e| format!("Failed to prepare export query: {}", e))?;

        let params: Vec<&dyn rusqlite::types::ToSql> = ids
            .iter()
            .map(|id| id as &dyn rusqlite::types::ToSql)
            .collect();

        let skills = stmt
            .query_map(params.as_slice(), |row| {
                let tags_json: String =
                    row.get::<_, String>(5).unwrap_or_else(|_| "[]".to_string());
                let allowed_phases_json: String = row
                    .get::<_, String>(8)
                    .unwrap_or_else(|_| "[\"setup\"]".to_string());
                let parameters_json: String =
                    row.get::<_, String>(9).unwrap_or_else(|_| "[]".to_string());
                let template_json: String = row.get(10)?;

                Ok(crate::skills::SkillDefinition {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    slug: row.get(2)?,
                    description: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
                    category: row
                        .get::<_, Option<String>>(4)?
                        .unwrap_or_else(|| "custom".to_string()),
                    tags: serde_json::from_str(&tags_json).unwrap_or_default(),
                    icon: row
                        .get::<_, Option<String>>(6)?
                        .unwrap_or_else(|| "puzzle".to_string()),
                    color: row
                        .get::<_, Option<String>>(7)?
                        .unwrap_or_else(|| "gray".to_string()),
                    allowed_phases: serde_json::from_str(&allowed_phases_json)
                        .unwrap_or_else(|_| vec!["setup".to_string()]),
                    parameters: serde_json::from_str(&parameters_json).unwrap_or_default(),
                    template: serde_json::from_str(&template_json).unwrap_or(
                        crate::skills::SkillTemplate::SingleStep {
                            step: std::collections::HashMap::new(),
                        },
                    ),
                    source: row
                        .get::<_, Option<String>>(11)?
                        .unwrap_or_else(|| "user".to_string()),
                    version: row.get::<_, Option<String>>(12)?,
                    author: row
                        .get::<_, Option<String>>(13)?
                        .and_then(|s| serde_json::from_str(&s).ok()),
                    checksum: row.get::<_, Option<String>>(14)?,
                    depends_on: row
                        .get::<_, Option<String>>(15)?
                        .and_then(|s| serde_json::from_str(&s).ok()),
                    usage_count: row.get::<_, Option<u64>>(16)?,
                    approval_status: row.get::<_, Option<String>>(17)?,
                    forked_from: row.get::<_, Option<String>>(18)?,
                })
            })
            .map_err(|e| format!("Failed to export skills: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(skills)
    }

    /// Import skills from an export. Sets source to "community" and id to "community:<slug>".
    /// `conflict_mode` is "skip" or "overwrite".
    pub fn import_skills(
        &self,
        skills: &[crate::skills::SkillDefinition],
        conflict_mode: &str,
    ) -> Result<crate::skills::SkillImportResult, String> {
        let conn = self.get_conn()?;
        let now = chrono::Utc::now().to_rfc3339();
        let mut imported = 0usize;
        let mut skipped = 0usize;
        let mut overwritten = 0usize;
        let mut errors = Vec::new();

        for skill in skills {
            let slug = &skill.slug;
            let id = format!("community:{}", slug);

            let tags_json = serde_json::to_string(&skill.tags).unwrap_or_else(|_| "[]".to_string());
            let allowed_phases_json =
                serde_json::to_string(&skill.allowed_phases).unwrap_or_else(|_| "[]".to_string());
            let parameters_json =
                serde_json::to_string(&skill.parameters).unwrap_or_else(|_| "[]".to_string());
            let template_json = match serde_json::to_string(&skill.template) {
                Ok(j) => j,
                Err(e) => {
                    errors.push(format!(
                        "Failed to serialize template for '{}': {}",
                        slug, e
                    ));
                    continue;
                }
            };

            // Check if slug already exists
            let exists: bool = conn
                .query_row(
                    "SELECT COUNT(*) FROM user_skills WHERE slug = ?1",
                    params![slug],
                    |row| row.get::<_, i64>(0),
                )
                .map(|count| count > 0)
                .unwrap_or(false);

            if exists {
                if conflict_mode == "overwrite" {
                    let overwrite_author_json = skill
                        .author
                        .as_ref()
                        .map(|a| serde_json::to_string(a).unwrap_or_default());
                    let overwrite_depends_on_json =
                        serde_json::to_string(&skill.depends_on.as_deref().unwrap_or(&[]))
                            .unwrap_or_else(|_| "[]".to_string());

                    match conn.execute(
                        r#"UPDATE user_skills SET
                            id = ?1, name = ?2, description = ?3, category = ?4,
                            tags = ?5, icon = ?6, color = ?7, allowed_phases = ?8,
                            parameters = ?9, template = ?10, source = ?11, updated_at = ?12,
                            version = ?13, author = ?14, checksum = ?15, depends_on = ?16,
                            usage_count = ?17, approval_status = ?18, forked_from = ?19
                        WHERE slug = ?20"#,
                        params![
                            id,
                            skill.name,
                            skill.description,
                            skill.category,
                            tags_json,
                            skill.icon,
                            skill.color,
                            allowed_phases_json,
                            parameters_json,
                            template_json,
                            "community",
                            now,
                            skill.version.as_deref().unwrap_or("1.0.0"),
                            overwrite_author_json,
                            skill.checksum.as_deref(),
                            overwrite_depends_on_json,
                            skill.usage_count.unwrap_or(0) as i64,
                            skill.approval_status.as_deref(),
                            skill.forked_from.as_deref(),
                            slug,
                        ],
                    ) {
                        Ok(_) => overwritten += 1,
                        Err(e) => errors.push(format!("Failed to overwrite '{}': {}", slug, e)),
                    }
                } else {
                    skipped += 1;
                }
            } else {
                let author_json = skill
                    .author
                    .as_ref()
                    .map(|a| serde_json::to_string(a).unwrap_or_default());
                let depends_on_json =
                    serde_json::to_string(&skill.depends_on.as_deref().unwrap_or(&[]))
                        .unwrap_or_else(|_| "[]".to_string());

                match conn.execute(
                    r#"INSERT INTO user_skills (
                        id, name, slug, description, category, tags, icon, color,
                        allowed_phases, parameters, template, source,
                        version, author, checksum, depends_on, usage_count, approval_status, forked_from,
                        created_at, updated_at
                    ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21)"#,
                    params![
                        id,
                        skill.name,
                        slug,
                        skill.description,
                        skill.category,
                        tags_json,
                        skill.icon,
                        skill.color,
                        allowed_phases_json,
                        parameters_json,
                        template_json,
                        "community",
                        skill.version.as_deref().unwrap_or("1.0.0"),
                        author_json,
                        skill.checksum.as_deref(),
                        depends_on_json,
                        skill.usage_count.unwrap_or(0) as i64,
                        skill.approval_status.as_deref(),
                        skill.forked_from.as_deref(),
                        now,
                        now,
                    ],
                ) {
                    Ok(_) => imported += 1,
                    Err(e) => errors.push(format!("Failed to import '{}': {}", slug, e)),
                }
            }
        }

        Ok(crate::skills::SkillImportResult {
            imported,
            skipped,
            overwritten,
            errors,
            warnings: vec![],
        })
    }

    /// Delete a user skill by ID.
    pub fn delete_user_skill(&self, id: &str) -> Result<bool, String> {
        let conn = self.get_conn()?;

        let deleted = conn
            .execute("DELETE FROM user_skills WHERE id = ?1", params![id])
            .map_err(|e| format!("Failed to delete user skill: {}", e))?;

        Ok(deleted > 0)
    }

    /// Update the approval status of a skill.
    pub fn update_skill_approval(&self, skill_id: &str, status: &str) -> Result<(), String> {
        let conn = self.get_conn()?;
        let rows = conn
            .execute(
                "UPDATE user_skills SET approval_status = ?1 WHERE id = ?2",
                params![status, skill_id],
            )
            .map_err(|e| format!("Failed to update approval status: {}", e))?;

        if rows == 0 {
            return Err(format!("Skill not found: {}", skill_id));
        }
        Ok(())
    }

    /// Update the version and checksum of a skill.
    pub fn update_skill_version(
        &self,
        skill_id: &str,
        version: &str,
        checksum: &str,
    ) -> Result<(), String> {
        let conn = self.get_conn()?;
        let rows = conn
            .execute(
                "UPDATE user_skills SET version = ?1, checksum = ?2, updated_at = datetime('now') WHERE id = ?3",
                params![version, checksum, skill_id],
            )
            .map_err(|e| format!("Failed to update skill version: {}", e))?;

        if rows == 0 {
            return Err(format!("Skill not found: {}", skill_id));
        }
        Ok(())
    }

    /// Fork a skill by creating a copy with a new ID.
    pub fn fork_skill(
        &self,
        skill_id: &str,
        new_name: Option<&str>,
    ) -> Result<crate::skills::SkillDefinition, String> {
        let original = self
            .get_user_skill(skill_id)?
            .ok_or_else(|| format!("Skill not found: {}", skill_id))?;

        let fork_name = new_name
            .map(|n| n.to_string())
            .unwrap_or_else(|| format!("{} (fork)", original.name));
        let fork_slug = format!(
            "{}-fork-{}",
            original.slug,
            &uuid::Uuid::new_v4().to_string()[..8]
        );
        let fork_id = format!("user:{}", fork_slug);
        let now = Utc::now().to_rfc3339();

        let tags_json = serde_json::to_string(&original.tags).unwrap_or_else(|_| "[]".to_string());
        let allowed_phases_json =
            serde_json::to_string(&original.allowed_phases).unwrap_or_else(|_| "[]".to_string());
        let parameters_json =
            serde_json::to_string(&original.parameters).unwrap_or_else(|_| "[]".to_string());
        let template_json = serde_json::to_string(&original.template)
            .map_err(|e| format!("Failed to serialize template: {}", e))?;
        let author_json = original
            .author
            .as_ref()
            .map(|a| serde_json::to_string(a).unwrap_or_default());
        let depends_on_json = serde_json::to_string(&original.depends_on.as_deref().unwrap_or(&[]))
            .unwrap_or_else(|_| "[]".to_string());

        let conn = self.get_conn()?;
        conn.execute(
            r#"INSERT INTO user_skills (
                id, name, slug, description, category, tags, icon, color,
                allowed_phases, parameters, template, source,
                version, author, checksum, depends_on, usage_count, approval_status, forked_from,
                created_at, updated_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21)"#,
            params![
                fork_id,
                fork_name,
                fork_slug,
                original.description,
                original.category,
                tags_json,
                original.icon,
                original.color,
                allowed_phases_json,
                parameters_json,
                template_json,
                "user",
                "1.0.0",
                author_json,
                Option::<String>::None, // checksum
                depends_on_json,
                0i64,                   // usage_count
                Option::<String>::None, // approval_status
                skill_id,               // forked_from
                now,
                now,
            ],
        )
        .map_err(|e| format!("Failed to create forked skill: {}", e))?;

        self.get_user_skill(&fork_id)?
            .ok_or_else(|| "Failed to retrieve forked skill".to_string())
    }

    /// Increment the usage count of a skill and return the new count.
    pub fn increment_skill_usage(&self, skill_id: &str) -> Result<u64, String> {
        let conn = self.get_conn()?;
        let rows = conn
            .execute(
                "UPDATE user_skills SET usage_count = COALESCE(usage_count, 0) + 1 WHERE id = ?1",
                params![skill_id],
            )
            .map_err(|e| format!("Failed to increment usage count: {}", e))?;

        if rows == 0 {
            return Err(format!("Skill not found: {}", skill_id));
        }

        let count: u64 = conn
            .query_row(
                "SELECT COALESCE(usage_count, 0) FROM user_skills WHERE id = ?1",
                params![skill_id],
                |row| row.get(0),
            )
            .map_err(|e| format!("Failed to read usage count: {}", e))?;

        Ok(count)
    }

    // ========================================================================
    // Phase Token Usage Operations
    // ========================================================================

    /// Record token usage for a single AI call within a workflow phase.
    pub fn create_phase_token_usage(
        &self,
        task_run_id: &str,
        phase: &str,
        stage_index: Option<u32>,
        iteration: Option<u32>,
        model_used: Option<&str>,
        provider_used: Option<&str>,
        input_tokens: u64,
        output_tokens: u64,
        cost_cents: u64,
        duration_ms: Option<u64>,
    ) -> Result<(), String> {
        let conn = self.get_conn()?;
        conn.execute(
            r#"INSERT INTO phase_token_usage
                (task_run_id, phase, stage_index, iteration, model_used, provider_used,
                 input_tokens, output_tokens, cost_cents, duration_ms)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)"#,
            params![
                task_run_id,
                phase,
                stage_index.map(|v| v as i64),
                iteration.map(|v| v as i64),
                model_used,
                provider_used,
                input_tokens as i64,
                output_tokens as i64,
                cost_cents as i64,
                duration_ms.map(|v| v as i64),
            ],
        )
        .map_err(|e| format!("Failed to insert phase token usage: {}", e))?;
        Ok(())
    }

    /// Get per-phase token usage breakdown for a task run.
    pub fn get_phase_token_usage(
        &self,
        task_run_id: &str,
    ) -> Result<Vec<PhaseTokenUsageRow>, String> {
        let conn = self.get_conn()?;
        let mut stmt = conn
            .prepare(
                r#"SELECT phase, stage_index, iteration, model_used, provider_used,
                       input_tokens, output_tokens, cost_cents, duration_ms, created_at
                FROM phase_token_usage
                WHERE task_run_id = ?1
                ORDER BY created_at ASC"#,
            )
            .map_err(|e| format!("Failed to prepare phase token usage query: {}", e))?;

        let rows = stmt
            .query_map(params![task_run_id], |row| {
                Ok(PhaseTokenUsageRow {
                    phase: row.get(0)?,
                    stage_index: row.get::<_, Option<i64>>(1)?.map(|v| v as u32),
                    iteration: row.get::<_, Option<i64>>(2)?.map(|v| v as u32),
                    model_used: row.get(3)?,
                    provider_used: row.get(4)?,
                    input_tokens: row.get::<_, i64>(5)? as u64,
                    output_tokens: row.get::<_, i64>(6)? as u64,
                    cost_cents: row.get::<_, i64>(7)? as u64,
                    duration_ms: row.get::<_, Option<i64>>(8)?.map(|v| v as u64),
                    created_at: row.get(9)?,
                })
            })
            .map_err(|e| format!("Failed to query phase token usage: {}", e))?;

        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("Failed to collect phase token usage rows: {}", e))
    }

    /// Update the aggregate token totals on a task run.
    pub fn update_task_run_token_totals(&self, task_run_id: &str) -> Result<(), String> {
        let conn = self.get_conn()?;
        conn.execute(
            r#"UPDATE task_runs SET
                total_input_tokens = COALESCE((SELECT SUM(input_tokens) FROM phase_token_usage WHERE task_run_id = ?1), 0),
                total_output_tokens = COALESCE((SELECT SUM(output_tokens) FROM phase_token_usage WHERE task_run_id = ?1), 0),
                total_cost_cents = COALESCE((SELECT SUM(cost_cents) FROM phase_token_usage WHERE task_run_id = ?1), 0)
            WHERE id = ?1"#,
            params![task_run_id],
        )
        .map_err(|e| format!("Failed to update task run token totals: {}", e))?;
        Ok(())
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn create_test_db() -> (CheckpointDb, TempDir) {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test.db");
        let db = CheckpointDb::new_at_path(db_path).unwrap();
        (db, temp_dir)
    }

    #[test]
    fn test_checkpoint_crud() {
        let (db, _temp) = create_test_db();

        // Create
        let checkpoint = CheckpointData {
            session_id: None,
            workflow_name: Some("test-workflow".to_string()),
            current_phase: 2,
            total_phases: Some(12),
            completed: false,
            restart_permitted: true,
            status: Some("running".to_string()),
            run_id: Some("test-run-123".to_string()),
            repos_to_process: Some(vec!["repo1".to_string(), "repo2".to_string()]),
            work_completed: None,
            items_needing_user_input: None,
            created_at: None,
            updated_at: None,
            error_message: None,
            extra: None,
        };

        db.save_checkpoint(&checkpoint).unwrap();

        // Read
        let loaded = db.get_checkpoint("test-workflow").unwrap().unwrap();
        assert_eq!(loaded.current_phase, 2);
        assert!(!loaded.completed);

        // Update
        let updated = CheckpointData {
            current_phase: 5,
            completed: true,
            ..checkpoint.clone()
        };
        db.save_checkpoint(&updated).unwrap();

        let reloaded = db.get_checkpoint("test-workflow").unwrap().unwrap();
        assert_eq!(reloaded.current_phase, 5);
        assert!(reloaded.completed);

        // Delete
        let deleted = db.delete_checkpoint("test-workflow").unwrap();
        assert!(deleted);

        let not_found = db.get_checkpoint("test-workflow").unwrap();
        assert!(not_found.is_none());
    }

    #[test]
    fn test_check_checkpoint_status() {
        let (db, _temp) = create_test_db();

        let checkpoint = CheckpointData {
            session_id: None,
            workflow_name: Some("status-test".to_string()),
            current_phase: 5,
            total_phases: Some(12),
            completed: false,
            restart_permitted: true,
            status: None,
            run_id: None,
            repos_to_process: None,
            work_completed: None,
            items_needing_user_input: None,
            created_at: None,
            updated_at: None,
            error_message: None,
            extra: None,
        };

        db.save_checkpoint(&checkpoint).unwrap();

        // Not complete yet (5 < 12)
        let (is_complete, phase) = db
            .check_checkpoint_status("status-test", 12)
            .unwrap()
            .unwrap();
        assert!(!is_complete);
        assert_eq!(phase, 5);

        // Complete when threshold is 5
        let (is_complete, _) = db
            .check_checkpoint_status("status-test", 5)
            .unwrap()
            .unwrap();
        assert!(is_complete);
    }

    #[test]
    fn test_settings() {
        let (db, _temp) = create_test_db();

        let value = serde_json::json!({"timeout": 300, "enabled": true});
        db.set_setting("ai_config", &value).unwrap();

        let loaded = db.get_setting("ai_config").unwrap().unwrap();
        assert_eq!(loaded["timeout"], 300);
        assert_eq!(loaded["enabled"], true);

        let all = db.get_all_settings().unwrap();
        assert!(all.get("ai_config").is_some());
    }

    #[test]
    fn test_task_run_auto_continue() {
        let (db, _temp) = create_test_db();

        // Create task run with default auto_continue (true)
        let input = CreateTaskRunInput::new("test-task-1", "Test Task").with_prompt("Do something");
        let task_run = db.create_task_run(&input).unwrap();
        assert!(task_run.auto_continue);

        // Create task run with explicit auto_continue = false
        let input = CreateTaskRunInput::new("test-task-2", "Test Task 2")
            .with_prompt("Do something else")
            .with_auto_continue(false);
        let task_run_disabled = db.create_task_run(&input).unwrap();
        assert!(!task_run_disabled.auto_continue);

        // Get auto_continue setting
        let auto_continue = db.get_task_auto_continue("test-task-1").unwrap();
        assert!(auto_continue);

        let auto_continue_disabled = db.get_task_auto_continue("test-task-2").unwrap();
        assert!(!auto_continue_disabled);

        // Set auto_continue setting
        db.set_task_auto_continue("test-task-1", false).unwrap();
        let updated = db.get_task_auto_continue("test-task-1").unwrap();
        assert!(!updated);

        // Verify via get_task_run
        let loaded = db.get_task_run("test-task-1").unwrap().unwrap();
        assert!(!loaded.auto_continue);
    }

    #[test]
    fn test_get_running_task_step_data_no_running_tasks() {
        let (db, _temp) = create_test_db();

        // No tasks at all
        let result = db.get_running_task_step_data().unwrap();
        assert!(result.is_none());

        // Create a completed task (not running)
        let input = CreateTaskRunInput::new("done-1", "Done Task").with_prompt("finished");
        db.create_task_run(&input).unwrap();
        db.update_task_run_status("done-1", "complete").unwrap();

        let result = db.get_running_task_step_data().unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_get_running_task_step_data_with_events() {
        let (db, _temp) = create_test_db();

        let input = CreateTaskRunInput::new("run-1", "Running Task").with_prompt("do stuff");
        db.create_task_run(&input).unwrap();

        // Insert step events
        let start_event = CreateTaskRunEventInput {
            task_run_id: "run-1".to_string(),
            event_type: "step_execution".to_string(),
            event_subtype: Some("start".to_string()),
            message: "Run npm test".to_string(),
            data: Some(
                serde_json::json!({
                    "step_name": "npm test",
                    "step_index": 0,
                    "phase": "setup"
                })
                .to_string(),
            ),
            workflow_name: None,
            state_name: None,
            action_id: Some("action-1".to_string()),
            timestamp: "2026-01-01T00:00:00Z".to_string(),
            duration_ms: None,
        };
        db.create_task_run_event(&start_event).unwrap();

        let complete_event = CreateTaskRunEventInput {
            task_run_id: "run-1".to_string(),
            event_type: "step_execution".to_string(),
            event_subtype: Some("complete".to_string()),
            message: "Run npm test".to_string(),
            data: Some(
                serde_json::json!({
                    "step_name": "npm test",
                    "step_index": 0,
                    "phase": "setup",
                    "duration_ms": 1500
                })
                .to_string(),
            ),
            workflow_name: None,
            state_name: None,
            action_id: Some("action-1".to_string()),
            timestamp: "2026-01-01T00:00:02Z".to_string(),
            duration_ms: Some(1500),
        };
        db.create_task_run_event(&complete_event).unwrap();

        let result = db.get_running_task_step_data().unwrap();
        assert!(result.is_some());

        let (task, events) = result.unwrap();
        assert_eq!(task.id, "run-1");
        assert_eq!(task.status, "running");
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].event_subtype.as_deref(), Some("start"));
        assert_eq!(events[1].event_subtype.as_deref(), Some("complete"));
    }

    #[test]
    fn test_get_completed_verification_iterations() {
        let (db, _temp) = create_test_db();

        let input = CreateTaskRunInput::new("task-v1", "Verification Task").with_prompt("verify");
        db.create_task_run(&input).unwrap();

        // Store verification results for iterations 1, 2, 3
        for i in 1..=3 {
            let result = serde_json::json!({
                "all_passed": i == 3,
                "total_steps": 5,
                "passed_steps": if i == 3 { 5 } else { 3 },
                "failed_steps": if i == 3 { 0 } else { 2 },
                "skipped_steps": 0,
                "total_duration_ms": 1000 * i,
                "critical_failure": false,
                "iteration": i
            });
            db.store_verification_phase_result("task-v1", i as u32, &result)
                .unwrap();
        }

        let iterations = db.get_completed_verification_iterations("task-v1").unwrap();
        assert_eq!(iterations, vec![1, 2, 3]);
    }

    #[test]
    fn test_get_completed_verification_iterations_empty() {
        let (db, _temp) = create_test_db();

        // No verification results for a nonexistent task
        let iterations = db
            .get_completed_verification_iterations("nonexistent-task")
            .unwrap();
        assert!(iterations.is_empty());

        // Create a task but don't add verification results
        let input =
            CreateTaskRunInput::new("task-empty", "Empty Task").with_prompt("no verification");
        db.create_task_run(&input).unwrap();

        let iterations = db
            .get_completed_verification_iterations("task-empty")
            .unwrap();
        assert!(iterations.is_empty());
    }

    #[test]
    fn test_process_session_crud() {
        let (db, _temp) = create_test_db();

        // Create a session
        db.create_process_session("sess-1", "config-a", "My Process")
            .unwrap();

        // Verify it appears in get_process_sessions (filtered by config_id)
        let sessions = db.get_process_sessions(Some("config-a"), 100).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, "sess-1");
        assert_eq!(sessions[0].process_config_id, "config-a");
        assert_eq!(sessions[0].process_name, "My Process");
        assert_eq!(sessions[0].state, "running");
        assert!(sessions[0].stopped_at.is_none());
        assert!(sessions[0].exit_code.is_none());
        assert_eq!(sessions[0].error_count, 0);

        // Verify it also appears when querying without filter
        let all_sessions = db.get_process_sessions(None, 100).unwrap();
        assert_eq!(all_sessions.len(), 1);
        assert_eq!(all_sessions[0].id, "sess-1");

        // Update the session (simulate process exit)
        db.update_process_session("sess-1", "exited", Some(0), 3)
            .unwrap();

        // Verify the update
        let sessions = db.get_process_sessions(Some("config-a"), 100).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].state, "exited");
        assert_eq!(sessions[0].exit_code, Some(0));
        assert_eq!(sessions[0].error_count, 3);
        assert!(sessions[0].stopped_at.is_some());

        // Create a second session for a different config and verify filtering
        db.create_process_session("sess-2", "config-b", "Other Process")
            .unwrap();

        let config_a = db.get_process_sessions(Some("config-a"), 100).unwrap();
        assert_eq!(config_a.len(), 1);
        assert_eq!(config_a[0].id, "sess-1");

        let config_b = db.get_process_sessions(Some("config-b"), 100).unwrap();
        assert_eq!(config_b.len(), 1);
        assert_eq!(config_b[0].id, "sess-2");

        let all = db.get_process_sessions(None, 100).unwrap();
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn test_process_session_output() {
        let (db, _temp) = create_test_db();

        // Create a session to attach output to
        db.create_process_session("sess-out-1", "config-x", "Output Test")
            .unwrap();

        // Insert output lines
        let lines = vec![
            (
                "2026-01-01T00:00:01Z".to_string(),
                "stdout".to_string(),
                "Starting server...".to_string(),
            ),
            (
                "2026-01-01T00:00:02Z".to_string(),
                "stdout".to_string(),
                "Listening on port 8080".to_string(),
            ),
            (
                "2026-01-01T00:00:03Z".to_string(),
                "stderr".to_string(),
                "Warning: deprecated config option".to_string(),
            ),
        ];
        db.insert_process_session_output("sess-out-1", &lines)
            .unwrap();

        // Retrieve all output lines
        let output = db.get_process_session_output("sess-out-1", 100, 0).unwrap();
        assert_eq!(output.len(), 3);

        // Verify order (should be ordered by id ASC, matching insertion order)
        assert_eq!(output[0].line, "Starting server...");
        assert_eq!(output[0].stream, "stdout");
        assert_eq!(output[0].session_id, "sess-out-1");

        assert_eq!(output[1].line, "Listening on port 8080");
        assert_eq!(output[1].stream, "stdout");

        assert_eq!(output[2].line, "Warning: deprecated config option");
        assert_eq!(output[2].stream, "stderr");

        // Verify limit works
        let limited = db.get_process_session_output("sess-out-1", 2, 0).unwrap();
        assert_eq!(limited.len(), 2);
        assert_eq!(limited[0].line, "Starting server...");
        assert_eq!(limited[1].line, "Listening on port 8080");

        // Verify offset works
        let offset = db.get_process_session_output("sess-out-1", 100, 1).unwrap();
        assert_eq!(offset.len(), 2);
        assert_eq!(offset[0].line, "Listening on port 8080");
        assert_eq!(offset[1].line, "Warning: deprecated config option");

        // Verify empty insert is a no-op
        db.insert_process_session_output("sess-out-1", &[]).unwrap();
        let output_after = db.get_process_session_output("sess-out-1", 100, 0).unwrap();
        assert_eq!(output_after.len(), 3);

        // Verify output for a different session is isolated
        db.create_process_session("sess-out-2", "config-x", "Output Test 2")
            .unwrap();
        let other_output = db.get_process_session_output("sess-out-2", 100, 0).unwrap();
        assert!(other_output.is_empty());
    }

    #[test]
    fn test_process_session_pruning() {
        let (db, _temp) = create_test_db();

        // Create 12 sessions for the same config_id with staggered timestamps.
        // create_process_session uses Utc::now() so we insert them sequentially;
        // the order in the DB will match insertion order.
        for i in 1..=12 {
            let session_id = format!("prune-sess-{}", i);
            db.create_process_session(&session_id, "config-prune", &format!("Process {}", i))
                .unwrap();
        }

        // Verify all 12 exist
        let before = db.get_process_sessions(Some("config-prune"), 100).unwrap();
        assert_eq!(before.len(), 12);

        // Prune, keeping only the 10 most recent
        let deleted = db.prune_old_process_sessions("config-prune", 10).unwrap();
        assert_eq!(deleted, 2);

        // Verify 10 remain
        let after = db.get_process_sessions(Some("config-prune"), 100).unwrap();
        assert_eq!(after.len(), 10);

        // The most recent sessions should survive (ordered DESC by started_at).
        // Sessions 3..=12 should remain; 1 and 2 should be pruned.
        // get_process_sessions returns DESC order, so first result is newest.
        let remaining_ids: Vec<String> = after.iter().map(|s| s.id.clone()).collect();
        assert!(!remaining_ids.contains(&"prune-sess-1".to_string()));
        assert!(!remaining_ids.contains(&"prune-sess-2".to_string()));
        assert!(remaining_ids.contains(&"prune-sess-12".to_string()));
        assert!(remaining_ids.contains(&"prune-sess-3".to_string()));

        // Pruning a different config should not affect these sessions
        let deleted_other = db.prune_old_process_sessions("config-other", 5).unwrap();
        assert_eq!(deleted_other, 0);

        let still_ten = db.get_process_sessions(Some("config-prune"), 100).unwrap();
        assert_eq!(still_ten.len(), 10);

        // Pruning again with same keep_count should delete nothing
        let deleted_again = db.prune_old_process_sessions("config-prune", 10).unwrap();
        assert_eq!(deleted_again, 0);
    }
}

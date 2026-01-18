//! SQLite database for qontinui-runner persistence.
//!
//! Provides transaction-safe storage for sessions, checkpoints, settings,
//! prompts, workflows, and scheduler state.

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

    pub timeout_seconds: u32,
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
    #[serde(default = "default_timeout")]
    pub timeout_seconds: u32,
    #[serde(default = "default_true")]
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
    /// Timeout in seconds
    pub timeout_seconds: u32,
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
    #[serde(default = "default_timeout")]
    pub timeout_seconds: u32,
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

    /// Get the database path.
    pub fn path(&self) -> &PathBuf {
        &self.db_path
    }

    /// Get a connection from the pool.
    fn get_conn(&self) -> Result<PooledConnection<SqliteConnectionManager>, String> {
        self.pool
            .get()
            .map_err(|e| format!("Failed to get connection from pool: {}", e))
    }

    /// Get a reference to the underlying connection for direct rusqlite operations.
    /// This is useful for modules that need raw rusqlite::Connection access (e.g., findings storage).
    ///
    /// Note: The returned reference borrows the PooledConnection, so the caller must
    /// ensure the PooledConnection stays alive while using the Connection.
    pub fn connection(&self) -> Result<PooledConnection<SqliteConnectionManager>, String> {
        self.get_conn()
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
            .query_row("SELECT page_count * page_size FROM pragma_page_count(), pragma_page_size()", [], |row| row.get(0))
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
            .query_row("SELECT page_count * page_size FROM pragma_page_count(), pragma_page_size()", [], |row| row.get(0))
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
                .query_row(&format!("SELECT COUNT(*) FROM \"{}\"", table), [], |row| row.get(0))
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
            info!("Creating database schema (version 5)");
            conn.execute_batch(include_str!("schema.sql"))
                .map_err(|e| format!("Failed to create schema: {}", e))?;
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

        // Migration 23: Add task_knowledge_summaries and retry_state_json
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

            // Add retry_state_json column (ALTER TABLE must be separate)
            let _ = conn.execute(
                "ALTER TABLE task_runs ADD COLUMN retry_state_json TEXT",
                [],
            );

            info!("Successfully migrated to version 23 (task_knowledge_summaries, retry_state_json)");
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

    /// Create a new task run.
    /// If `auto_continue` is None, defaults to true.
    /// `execution_steps_json` and `log_sources_json` are optional JSON strings
    /// that store the deterministic steps to re-execute on session resume.
    pub fn create_task_run(
        &self,
        id: &str,
        task_name: &str,
        prompt: Option<&str>,
        max_sessions: Option<u32>,
        auto_continue: Option<bool>,
        execution_steps_json: Option<String>,
        log_sources_json: Option<String>,
    ) -> Result<TaskRun, String> {
        self.create_task_run_with_config(
            id,
            task_name,
            prompt,
            "task", // default task_type
            None,   // no config_id
            None,   // no workflow_name
            max_sessions,
            auto_continue,
            execution_steps_json,
            log_sources_json,
        )
    }

    /// Create a new task run with full configuration options.
    /// This is the unified entry point for creating any type of task run.
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
        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();
        let auto_continue_val = auto_continue.unwrap_or(true);

        conn.execute(
            r#"
            INSERT INTO task_runs (id, task_name, prompt, task_type, status, sessions_count, max_sessions,
                                   output_log, auto_continue, execution_steps_json, log_sources_json,
                                   config_id, workflow_name, created_at, updated_at)
            VALUES (?1, ?2, ?3, ?4, 'running', 0, ?5, '', ?6, ?7, ?8, ?9, ?10, ?11, ?11)
            "#,
            params![
                id,
                task_name,
                prompt,
                task_type,
                max_sessions.map(|v| v as i64),
                auto_continue_val as i32,
                execution_steps_json,
                log_sources_json,
                config_id,
                workflow_name,
                now
            ],
        )
        .map_err(|e| format!("Failed to create task run: {}", e))?;

        Ok(TaskRun {
            id: id.to_string(),
            task_name: task_name.to_string(),
            prompt: prompt.map(|s| s.to_string()),
            task_type: task_type.to_string(),
            status: "running".to_string(),
            sessions_count: 0,
            max_sessions,
            output_log: String::new(),
            error_message: None,
            auto_continue: auto_continue_val,
            execution_steps_json,
            log_sources_json,
            config_id: config_id.map(|s| s.to_string()),
            workflow_name: workflow_name.map(|s| s.to_string()),
            summary: None,
            ai_summary: None,
            goal_achieved: None,
            remaining_work: None,
            summary_generated_at: None,
            created_at: now.clone(),
            updated_at: now,
            completed_at: None,
        })
    }

    /// Get a task run by ID.
    /// Note: output_log is reconstructed from chunks table for backward compatibility.
    pub fn get_task_run(&self, id: &str) -> Result<Option<TaskRun>, String> {
        let conn = self.get_conn()?;

        // Get the task_run metadata including all fields
        let result: SqliteResult<TaskRun> = conn.query_row(
            r#"
            SELECT id, task_name, prompt, task_type, status, sessions_count, max_sessions, error_message, auto_continue,
                   execution_steps_json, log_sources_json, config_id, workflow_name,
                   COALESCE(summary, ai_summary) as summary, ai_summary, goal_achieved, remaining_work,
                   summary_generated_at, created_at, updated_at, completed_at
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
                    summary: row.get(13)?,
                    ai_summary: row.get(14)?,
                    goal_achieved: row.get::<_, Option<i32>>(15)?.map(|v| v != 0),
                    remaining_work: row.get(16)?,
                    summary_generated_at: row.get(17)?,
                    created_at: row.get(18)?,
                    updated_at: row.get(19)?,
                    completed_at: row.get(20)?,
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

    /// Append output to a task run and increment session count.
    /// Returns true if [TASK_COMPLETE] marker was found in the appended text.
    ///
    /// Uses O(1) chunk insertion instead of O(n) string concatenation.
    /// Output is stored in the task_run_output_chunks table for efficient appending.
    ///
    /// NOTE: This method handles task completion inline to avoid multiple connection acquisitions.
    pub fn append_task_output(
        &self,
        id: &str,
        output: &str,
        increment_session: bool,
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

        // Check if task is complete - marker must be on its own line (not embedded in text)
        // This prevents false positives like "I should NOT output [TASK_COMPLETE] yet"
        let is_complete = output
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

    /// Mark a task run as complete.
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

    /// Mark a task run as failed.
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

        Ok(())
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
                       created_at, updated_at, completed_at,
                       task_type, config_id, workflow_name
                FROM task_runs
                WHERE status = 'running'
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
                    created_at: row.get(15)?,
                    updated_at: row.get(16)?,
                    completed_at: row.get(17)?,
                    task_type: row.get(18)?,
                    config_id: row.get(19)?,
                    workflow_name: row.get(20)?,
                })
            })
            .map_err(|e| format!("Failed to execute query: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(task_runs)
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
                       goal_achieved, remaining_work, summary_generated_at, created_at, updated_at, completed_at
                FROM task_runs
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
                    summary: row.get(11)?,
                    ai_summary: row.get(12)?,
                    goal_achieved: row.get::<_, Option<i32>>(13)?.map(|v| v != 0),
                    remaining_work: row.get(14)?,
                    summary_generated_at: row.get(15)?,
                    created_at: row.get(16)?,
                    updated_at: row.get(17)?,
                    completed_at: row.get(18)?,
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
            Ok(output[output.len() - chars..].to_string())
        }
    }

    /// Check if a task run should continue (not complete, not stopped, not at max sessions, auto_continue enabled).
    pub fn should_continue_task(&self, id: &str) -> Result<bool, String> {
        let task_run = self
            .get_task_run(id)?
            .ok_or_else(|| format!("Task run not found: {}", id))?;

        // Already complete or stopped
        if task_run.status != "running" {
            return Ok(false);
        }

        // Check if auto_continue is enabled for this task
        if !task_run.auto_continue {
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
                    timeout_seconds: row.get::<_, i64>(11)? as u32,
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
            timeout_seconds: row.get::<_, i64>(11)? as u32,
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
            timeout_seconds: row.get::<_, i64>(10)? as u32,
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
        let existing = self.get_check(id)?
            .ok_or_else(|| format!("Check not found: {}", id))?;

        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();

        // Build the SET clause dynamically based on which fields are present
        let name = input.name.as_ref().unwrap_or(&existing.name);
        let description = input.description.clone().or(existing.description);
        let check_type = input.check_type.as_ref().unwrap_or(&existing.check_type);
        let tool = input.tool.as_ref().unwrap_or(&existing.tool);
        let command = input.command.clone().or(existing.command);
        let working_directory = input.working_directory.clone().or(existing.working_directory);
        let config_path = input.config_path.clone().or(existing.config_path);
        let auto_fix = input.auto_fix.unwrap_or(existing.auto_fix);
        let fail_on_warning = input.fail_on_warning.unwrap_or(existing.fail_on_warning);
        let timeout_seconds = input.timeout_seconds.unwrap_or(existing.timeout_seconds);
        let is_critical = input.is_critical.unwrap_or(existing.is_critical);
        let enabled = input.enabled.unwrap_or(existing.enabled);
        let tags = input.tags.clone().unwrap_or(existing.tags);

        let tags_json = serde_json::to_string(&tags)
            .map_err(|e| format!("Failed to serialize tags: {}", e))?;

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

    /// Get check results for a specific check.
    pub fn get_check_results(&self, check_id: &str, limit: u32) -> Result<Vec<CheckResult>, String> {
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
            category: row.get::<_, Option<String>>(7)?.unwrap_or_else(|| "general".to_string()),
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
    pub fn create_shell_command(&self, input: &CreateShellCommandInput) -> Result<ShellCommand, String> {
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
    pub fn update_shell_command(&self, id: &str, input: &UpdateShellCommandInput) -> Result<ShellCommand, String> {
        // First verify the shell command exists
        let existing = self.get_shell_command(id)?
            .ok_or_else(|| format!("Shell command not found: {}", id))?;

        let conn = self.get_conn()?;
        let now = Utc::now().to_rfc3339();

        // Build the SET clause based on which fields are present
        let name = input.name.as_ref().unwrap_or(&existing.name);
        let description = input.description.clone().or(existing.description);
        let command = input.command.as_ref().unwrap_or(&existing.command);
        let working_directory = input.working_directory.clone().or(existing.working_directory);
        let timeout_seconds = input.timeout_seconds.unwrap_or(existing.timeout_seconds);
        let fail_on_error = input.fail_on_error.unwrap_or(existing.fail_on_error);
        let category = input.category.as_ref().unwrap_or(&existing.category);
        let tags = input.tags.clone().unwrap_or(existing.tags);
        let enabled = input.enabled.unwrap_or(existing.enabled);

        let tags_json = serde_json::to_string(&tags)
            .map_err(|e| format!("Failed to serialize tags: {}", e))?;

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
    pub fn get_shell_command_results(&self, shell_command_id: &str, limit: u32) -> Result<Vec<ShellCommandResult>, String> {
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
        use std::process::Command;
        use std::time::Instant;

        // Get the shell command
        let cmd = self.get_shell_command(id)?
            .ok_or_else(|| format!("Shell command not found: {}", id))?;

        if !cmd.enabled {
            return Err(format!("Shell command '{}' is disabled", cmd.name));
        }

        let start_time = Instant::now();
        let started_at = Utc::now().to_rfc3339();

        // Build the command - use shell to execute the command string
        #[cfg(target_os = "windows")]
        let mut process = Command::new("cmd");
        #[cfg(target_os = "windows")]
        process.args(["/C", &cmd.command]);

        #[cfg(not(target_os = "windows"))]
        let mut process = Command::new("sh");
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
    pub fn get_verification_plan(&self, id: &str) -> Result<Option<StoredVerificationPlan>, String> {
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

    /// Helper function to convert a row to StoredTaskKnowledge.
    fn row_to_task_knowledge(
        row: &rusqlite::Row,
    ) -> rusqlite::Result<StoredTaskKnowledge> {
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
            Some(iteration) => self.get_iteration_verification_results(task_run_id, iteration as u32),
            None => Ok(vec![]),
        }
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
        let credential_id = request.credential_id.as_ref().or(current.credential_id.as_ref());

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

        let params_refs: Vec<&dyn rusqlite::ToSql> =
            params_vec.iter().map(|s| s as &dyn rusqlite::ToSql).collect();

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
                       skip_ai_summary, created_at, updated_at, log_source_selection
                FROM unified_workflows
                ORDER BY updated_at DESC
                "#,
            )
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let workflows = stmt
            .query_map([], |row| {
                Ok(crate::unified_workflows::UnifiedWorkflow {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    description: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                    category: row.get::<_, Option<String>>(3)?.unwrap_or_else(|| "general".to_string()),
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
                   skip_ai_summary, created_at, updated_at, log_source_selection
            FROM unified_workflows
            WHERE id = ?1
            "#,
            params![id],
            |row| {
                Ok(crate::unified_workflows::UnifiedWorkflow {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    description: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                    category: row.get::<_, Option<String>>(3)?.unwrap_or_else(|| "general".to_string()),
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
                })
            },
        );

        match result {
            Ok(workflow) => Ok(Some(workflow)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("Failed to get unified workflow: {}", e)),
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
        let setup_steps_json = serde_json::to_string(&request.setup_steps).unwrap_or_else(|_| "[]".to_string());
        let verification_steps_json = serde_json::to_string(&request.verification_steps).unwrap_or_else(|_| "[]".to_string());
        let agentic_steps_json = serde_json::to_string(&request.agentic_steps).unwrap_or_else(|_| "[]".to_string());
        let completion_steps_json = serde_json::to_string(&request.completion_steps).unwrap_or_else(|_| "[]".to_string());
        let log_source_selection_json = serde_json::to_string(&request.log_source_selection).unwrap_or_else(|_| "\"default\"".to_string());

        conn.execute(
            r#"
            INSERT INTO unified_workflows (
                id, name, description, category, tags, setup_steps, verification_steps,
                agentic_steps, completion_steps, max_iterations, provider, model,
                skip_ai_summary, created_at, updated_at, log_source_selection
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)
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
                request.provider,
                request.model,
                request.skip_ai_summary,
                now,
                now,
                log_source_selection_json,
            ],
        )
        .map_err(|e| format!("Failed to create unified workflow: {}", e))?;

        self.get_unified_workflow(&id)?
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
        let description = request.description.as_ref().unwrap_or(&existing.description);
        let category = request.category.as_ref().unwrap_or(&existing.category);
        let tags = request.tags.as_ref().unwrap_or(&existing.tags);
        let setup_steps = request.setup_steps.as_ref().unwrap_or(&existing.setup_steps);
        let verification_steps = request.verification_steps.as_ref().unwrap_or(&existing.verification_steps);
        let agentic_steps = request.agentic_steps.as_ref().unwrap_or(&existing.agentic_steps);
        let completion_steps = request.completion_steps.as_ref().unwrap_or(&existing.completion_steps);
        let max_iterations = request.max_iterations.unwrap_or(existing.max_iterations);
        let provider = request.provider.as_ref().or(existing.provider.as_ref());
        let model = request.model.as_ref().or(existing.model.as_ref());
        let skip_ai_summary = request.skip_ai_summary.unwrap_or(existing.skip_ai_summary);
        let log_source_selection = request.log_source_selection.as_ref().unwrap_or(&existing.log_source_selection);

        let tags_json = serde_json::to_string(tags).unwrap_or_else(|_| "[]".to_string());
        let setup_steps_json = serde_json::to_string(setup_steps).unwrap_or_else(|_| "[]".to_string());
        let verification_steps_json = serde_json::to_string(verification_steps).unwrap_or_else(|_| "[]".to_string());
        let agentic_steps_json = serde_json::to_string(agentic_steps).unwrap_or_else(|_| "[]".to_string());
        let completion_steps_json = serde_json::to_string(completion_steps).unwrap_or_else(|_| "[]".to_string());
        let log_source_selection_json = serde_json::to_string(log_source_selection).unwrap_or_else(|_| "\"default\"".to_string());

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
                provider = ?10,
                model = ?11,
                skip_ai_summary = ?12,
                updated_at = ?13,
                log_source_selection = ?14
            WHERE id = ?15
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
                provider,
                model,
                skip_ai_summary,
                now,
                log_source_selection_json,
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
                   skip_ai_summary, created_at, updated_at, log_source_selection
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

        sql.push_str(" ORDER BY updated_at DESC");

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let params: Vec<&dyn rusqlite::ToSql> =
            params_vec.iter().map(|s| s as &dyn rusqlite::ToSql).collect();

        let workflows = stmt
            .query_map(params.as_slice(), |row| {
                Ok(crate::unified_workflows::UnifiedWorkflow {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    description: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                    category: row.get::<_, Option<String>>(3)?.unwrap_or_else(|| "general".to_string()),
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
            provider: original.provider,
            model: original.model,
            skip_ai_summary: original.skip_ai_summary,
            log_source_selection: original.log_source_selection,
        };

        self.create_unified_workflow(&create_request)
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

        let params: Vec<&dyn rusqlite::ToSql> =
            params_vec.iter().map(|s| s as &dyn rusqlite::ToSql).collect();

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

        let params: Vec<&dyn rusqlite::ToSql> =
            params_vec.iter().map(|s| s as &dyn rusqlite::ToSql).collect();

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

        let params: Vec<&dyn rusqlite::ToSql> =
            params_vec.iter().map(|s| s as &dyn rusqlite::ToSql).collect();

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

        let params: Vec<&dyn rusqlite::ToSql> =
            params_vec.iter().map(|b| b.as_ref()).collect();

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
            params![id, task_id, status, duration_secs, iterations, strategy,
                    tools_json, files_json, error_type, error_message, feedback_json, now],
        )
        .map_err(|e| format!("Failed to record learning outcome: {}", e))?;

        Ok(id)
    }

    /// Get learning outcomes for analysis
    pub fn get_learning_outcomes(&self, limit: Option<u32>) -> Result<Vec<serde_json::Value>, String> {
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
    pub fn get_orchestrator_checkpoints(&self, task_id: Option<&str>) -> Result<Vec<serde_json::Value>, String> {
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
    pub fn get_orchestrator_checkpoint(&self, id: &str) -> Result<Option<serde_json::Value>, String> {
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
            .execute("DELETE FROM orchestrator_checkpoints WHERE id = ?1", params![id])
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
            params![id, name, description, steps, start_step, timeout_secs,
                    inputs, outputs, tags, version, now],
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

    /// Save flow execution state
    pub fn save_flow_execution(&self, execution: &serde_json::Value) -> Result<(), String> {
        let conn = self.get_conn()?;

        let instance_id = execution["instance_id"].as_str().ok_or("Execution must have instance_id")?;
        let flow_id = execution["flow_id"].as_str().ok_or("Execution must have flow_id")?;
        let current_step = execution["current_step"].as_str();
        let status = execution["status"].as_str().unwrap_or("pending");
        let context = serde_json::to_string(&execution["context"]).ok();
        let history = serde_json::to_string(&execution["history"]).ok();
        let error = execution["error"].as_str();
        let default_started_at = Utc::now().to_rfc3339();
        let started_at = execution["started_at"].as_str().unwrap_or(&default_started_at);
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
    pub fn get_flow_execution(&self, instance_id: &str) -> Result<Option<serde_json::Value>, String> {
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
        conn.query_row(
            "SELECT COUNT(*) FROM learning_outcomes",
            [],
            |row| row.get(0),
        )
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
            conn.query_row(
                "SELECT COUNT(*) FROM orchestrator_checkpoints",
                [],
                |row| row.get(0),
            )
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
            conn.query_row(
                "SELECT COUNT(*) FROM flow_executions",
                [],
                |row| row.get(0),
            )
            .map_err(|e| format!("Failed to get count: {}", e))
        }
    }

    // ========================================================================
    // Task Run with Learning Outcome Queries (for Dashboard Integration)
    // ========================================================================

    /// Get recent task runs with their learning outcomes joined.
    /// Returns task runs along with any associated learning outcome data.
    pub fn get_recent_task_runs_with_outcomes(&self, limit: u32) -> Result<Vec<serde_json::Value>, String> {
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
            Err(e) => Err(format!("Failed to get most recent task with checkpoints: {}", e)),
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
            .query_row("SELECT COUNT(*) FROM orchestrator_flows", [], |row| row.get(0))
            .unwrap_or(0);

        let flow_executions_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM flow_executions", [], |row| row.get(0))
            .unwrap_or(0);

        let checkpoints_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM orchestrator_checkpoints", [], |row| row.get(0))
            .unwrap_or(0);

        let learning_outcomes_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM learning_outcomes", [], |row| row.get(0))
            .unwrap_or(0);

        let learning_patterns_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM learning_patterns", [], |row| row.get(0))
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
            .query_row("SELECT COUNT(*) FROM unified_workflows", [], |row| row.get(0))
            .unwrap_or(0);

        let verification_tests_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM verification_tests", [], |row| row.get(0))
            .unwrap_or(0);

        let task_hooks_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM task_hooks", [], |row| row.get(0))
            .unwrap_or(0);

        let scheduled_tasks_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM scheduled_tasks", [], |row| row.get(0))
            .unwrap_or(0);

        let saved_api_requests_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM saved_api_requests", [], |row| row.get(0))
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
                let tags: serde_json::Value = serde_json::from_str(&tags_str).unwrap_or(serde_json::json!([]));
                let definition_str: String = row.get(3)?;
                let definition: serde_json::Value = serde_json::from_str(&definition_str).unwrap_or(serde_json::json!({}));

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
                let state: serde_json::Value = serde_json::from_str(&state_str).unwrap_or(serde_json::json!({}));

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
                let value: serde_json::Value = serde_json::from_str(&value_str).unwrap_or(serde_json::Value::Null);

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
                let variables: serde_json::Value = serde_json::from_str(&vars_str).unwrap_or(serde_json::json!([]));

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
                let config: serde_json::Value = serde_json::from_str(&config_str).unwrap_or(serde_json::json!({}));

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
                       agentic_steps, max_iterations, provider, model, created_at, updated_at
                FROM unified_workflows
                ORDER BY updated_at DESC
                "#,
            )
            .map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let results = stmt
            .query_map([], |row| {
                let tags_str: String = row.get(4)?;
                let tags: serde_json::Value = serde_json::from_str(&tags_str).unwrap_or(serde_json::json!([]));
                let setup_str: String = row.get(5)?;
                let setup: serde_json::Value = serde_json::from_str(&setup_str).unwrap_or(serde_json::json!([]));
                let verif_str: String = row.get(6)?;
                let verification: serde_json::Value = serde_json::from_str(&verif_str).unwrap_or(serde_json::json!([]));
                let agent_str: String = row.get(7)?;
                let agentic: serde_json::Value = serde_json::from_str(&agent_str).unwrap_or(serde_json::json!([]));

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
                let config: serde_json::Value = serde_json::from_str(&config_str).unwrap_or(serde_json::json!({}));
                let tags_str: String = row.get(16)?;
                let tags: serde_json::Value = serde_json::from_str(&tags_str).unwrap_or(serde_json::json!([]));

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
                let action_config: serde_json::Value = serde_json::from_str(&action_str).unwrap_or(serde_json::json!({}));
                let cond_str: String = row.get(9)?;
                let conditions: serde_json::Value = serde_json::from_str(&cond_str).unwrap_or(serde_json::json!([]));

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
                let task_config: serde_json::Value = serde_json::from_str(&config_str).unwrap_or(serde_json::json!({}));

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
                let tags: serde_json::Value = serde_json::from_str(&tags_str).unwrap_or(serde_json::json!([]));
                let headers_str: String = row.get(7)?;
                let headers: serde_json::Value = serde_json::from_str(&headers_str).unwrap_or(serde_json::json!({}));
                let extractions_str: String = row.get(12)?;
                let extractions: serde_json::Value = serde_json::from_str(&extractions_str).unwrap_or(serde_json::json!([]));
                let assertions_str: String = row.get(13)?;
                let assertions: serde_json::Value = serde_json::from_str(&assertions_str).unwrap_or(serde_json::json!([]));

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
                let config: serde_json::Value = serde_json::from_str(&config_str).unwrap_or(serde_json::json!({}));

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
    pub fn import_flows(&self, flows: &[serde_json::Value], conflict_mode: &str) -> Result<ImportResult, String> {
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

        Ok(ImportResult { imported, skipped, errors })
    }

    /// Import prompts (with conflict handling).
    pub fn import_prompts(&self, prompts: &[serde_json::Value], conflict_mode: &str) -> Result<ImportResult, String> {
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
                .query_row("SELECT 1 FROM prompts WHERE id = ?1", params![id], |_| Ok(true))
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

        Ok(ImportResult { imported, skipped, errors })
    }

    /// Import settings (with conflict handling).
    pub fn import_settings(&self, settings: &[serde_json::Value], conflict_mode: &str) -> Result<ImportResult, String> {
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
                .query_row("SELECT 1 FROM settings WHERE key = ?1", params![key], |_| Ok(true))
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

        Ok(ImportResult { imported, skipped, errors })
    }

    /// Import unified workflows (with conflict handling).
    pub fn import_unified_workflows(&self, workflows: &[serde_json::Value], conflict_mode: &str) -> Result<ImportResult, String> {
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
                .query_row("SELECT 1 FROM unified_workflows WHERE id = ?1", params![id], |_| Ok(true))
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
            let verif = serde_json::to_string(&workflow["verification_steps"]).unwrap_or("[]".to_string());
            let agent = serde_json::to_string(&workflow["agentic_steps"]).unwrap_or("[]".to_string());
            let max_iter = workflow["max_iterations"].as_i64();
            let provider = workflow["provider"].as_str();
            let model = workflow["model"].as_str();

            let result = conn.execute(
                r#"
                INSERT OR REPLACE INTO unified_workflows (id, name, description, category, tags, setup_steps, verification_steps, agentic_steps, max_iterations, provider, model, created_at, updated_at)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
                "#,
                params![id, name, description, category, tags, setup, verif, agent, max_iter, provider, model, now, now],
            );

            match result {
                Ok(_) => imported += 1,
                Err(e) => errors.push(format!("Failed to import workflow {}: {}", id, e)),
            }
        }

        Ok(ImportResult { imported, skipped, errors })
    }

    /// Import learning outcomes (with conflict handling).
    pub fn import_learning_outcomes(&self, outcomes: &[serde_json::Value], conflict_mode: &str) -> Result<ImportResult, String> {
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

        Ok(ImportResult { imported, skipped, errors })
    }

    /// Import learning patterns (with conflict handling).
    pub fn import_learning_patterns(&self, patterns: &[serde_json::Value], conflict_mode: &str) -> Result<ImportResult, String> {
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
                .query_row("SELECT 1 FROM learning_patterns WHERE id = ?1", params![id], |_| Ok(true))
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

        Ok(ImportResult { imported, skipped, errors })
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
    pub fn create_mobile_state(&self, input: &CreateMobileStateInput) -> Result<MobileState, String> {
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
    pub fn get_mobile_states(&self, task_run_id: &str, limit: Option<u32>) -> Result<Vec<MobileState>, String> {
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
    pub fn get_latest_mobile_state(&self, task_run_id: &str) -> Result<Option<MobileState>, String> {
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
    pub fn get_mobile_errors(&self, task_run_id: &str, limit: Option<u32>) -> Result<Vec<MobileLog>, String> {
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
    pub fn get_mcp_server(&self, id: &str) -> Result<Option<crate::mcp_client::McpServerConfig>, String> {
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

        let stdio_config_json = input.stdio_config.as_ref()
            .map(|c| serde_json::to_string(c).unwrap_or_default());
        let http_config_json = input.http_config.as_ref()
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
        let existing = self.get_mcp_server(id)?
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
        let stdio_config_json = stdio_config.as_ref()
            .map(|c| serde_json::to_string(c).unwrap_or_default());
        let http_config_json = http_config.as_ref()
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

        conn.execute(
            "DELETE FROM mcp_servers WHERE id = ?1",
            params![id],
        )
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

        let row_mapper = |row: &rusqlite::Row| -> rusqlite::Result<crate::mcp_client::McpCallRecord> {
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
        assert_eq!(loaded.completed, false);

        // Update
        let updated = CheckpointData {
            current_phase: 5,
            completed: true,
            ..checkpoint.clone()
        };
        db.save_checkpoint(&updated).unwrap();

        let reloaded = db.get_checkpoint("test-workflow").unwrap().unwrap();
        assert_eq!(reloaded.current_phase, 5);
        assert_eq!(reloaded.completed, true);

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
        assert_eq!(is_complete, false);
        assert_eq!(phase, 5);

        // Complete when threshold is 5
        let (is_complete, _) = db
            .check_checkpoint_status("status-test", 5)
            .unwrap()
            .unwrap();
        assert_eq!(is_complete, true);
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
        let task_run = db
            .create_task_run(
                "test-task-1",
                "Test Task",
                Some("Do something"),
                None,
                None,
                None,
                None,
            )
            .unwrap();
        assert_eq!(task_run.auto_continue, true);

        // Create task run with explicit auto_continue = false
        let task_run_disabled = db
            .create_task_run(
                "test-task-2",
                "Test Task 2",
                Some("Do something else"),
                None,
                Some(false),
                None,
                None,
            )
            .unwrap();
        assert_eq!(task_run_disabled.auto_continue, false);

        // Get auto_continue setting
        let auto_continue = db.get_task_auto_continue("test-task-1").unwrap();
        assert_eq!(auto_continue, true);

        let auto_continue_disabled = db.get_task_auto_continue("test-task-2").unwrap();
        assert_eq!(auto_continue_disabled, false);

        // Set auto_continue setting
        db.set_task_auto_continue("test-task-1", false).unwrap();
        let updated = db.get_task_auto_continue("test-task-1").unwrap();
        assert_eq!(updated, false);

        // Verify via get_task_run
        let loaded = db.get_task_run("test-task-1").unwrap().unwrap();
        assert_eq!(loaded.auto_continue, false);
    }
}

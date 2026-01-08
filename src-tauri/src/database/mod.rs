//! SQLite database for qontinui-runner persistence.
//!
//! Provides transaction-safe storage for sessions, checkpoints, settings,
//! prompts, workflows, and scheduler state.

use chrono::Utc;
use r2d2::{Pool, PooledConnection};
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::{params, Connection, Result as SqliteResult};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tracing::{info, warn};

/// Database handle for checkpoint and session persistence.
pub struct CheckpointDb {
    pool: Pool<SqliteConnectionManager>,
    db_path: PathBuf,
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

/// Completion marker that AI uses to signal task is done
pub const TASK_COMPLETE_MARKER: &str = "[TASK_COMPLETE]";

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
                    status = 'complete',
                    updated_at = ?1,
                    completed_at = ?1
                WHERE id = ?2 AND status = 'running'
                "#,
                params![now, id],
            )
            .map_err(|e| format!("Failed to complete task run: {}", e))?;

            info!("Task run {} marked complete via append_task_output", id);
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
                status = 'complete',
                updated_at = ?1,
                completed_at = ?1
            WHERE id = ?2
            "#,
            params![now, id],
        )
        .map_err(|e| format!("Failed to complete task run: {}", e))?;

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

        conn.execute(
            r#"
            INSERT INTO verification_tests (
                id, name, description, test_type, category,
                playwright_code, vision_config, python_code, repo_test_config,
                success_criteria, config, timeout_seconds, is_critical, enabled,
                ai_generated, ai_generation_prompt, tags, source_file,
                created_at, updated_at
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5,
                ?6, ?7, ?8, ?9,
                ?10, ?11, ?12, ?13, ?14,
                ?15, ?16, ?17, ?18,
                ?19, ?19
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
                ai_generated, ai_generation_prompt, tags, source_file, last_exported_at,
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
                    tags: row
                        .get::<_, Option<String>>(16)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    source_file: row.get(17).ok(),
                    last_exported_at: row.get(18).ok(),
                    created_at: row.get(19)?,
                    updated_at: row.get(20)?,
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
                ai_generated, ai_generation_prompt, tags, source_file, last_exported_at,
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
            tags: row
                .get::<_, Option<String>>(16)?
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default(),
            source_file: row.get(17).ok(),
            last_exported_at: row.get(18).ok(),
            created_at: row.get(19)?,
            updated_at: row.get(20)?,
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
                tags = ?17,
                source_file = ?18,
                updated_at = ?19
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
                screenshots, ai_analysis, created_at
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
            ai_analysis: row.get(13).ok(),
            created_at: row.get(14)?,
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
                screenshots, ai_analysis, created_at
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
                    screenshots, ai_analysis, created_at
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
                            screenshots, ai_analysis, created_at
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
                        screenshots, ai_analysis, created_at
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
                "Do something",
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
                "Do something else",
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

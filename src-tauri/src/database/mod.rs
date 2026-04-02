//! Database module for qontinui-runner persistence.
//!
//! The SQLite (CheckpointDb) layer has been removed. All persistence now goes
//! through the PostgreSQL layer in `pg/`.
//!
//! Retained submodules contain shared type definitions and stub functions.

// PostgreSQL layer (Clorinde-generated queries -- sole persistence backend)
pub mod pg;

// Shared domain types
pub mod types;
pub use types::*;

// Stub types to keep dead code compiling after rusqlite removal.
// These are referenced in function signatures of SQLite-only code paths
// that have been stubbed out but whose signatures remain for API compat.

/// Placeholder for removed `rusqlite::Connection`.
/// All functions taking this parameter are stubbed (body returns error/no-op).
pub struct CheckpointDb;
impl CheckpointDb {
    pub fn global() -> std::sync::Arc<Self> { std::sync::Arc::new(Self) }
    pub fn try_global() -> Option<std::sync::Arc<Self>> { None }
    pub fn new() -> Result<Self, String> { Ok(Self) }
    pub fn new_in_memory() -> Result<Self, String> { Ok(Self) }
    pub fn set_global(_db: std::sync::Arc<Self>) {}
    pub fn get_pool(&self) -> () {}
    pub fn set_runner_port(&self, _port: u16) {}
    pub fn get_runner_port(&self) -> Option<u16> { None }
    pub fn path(&self) -> &std::path::PathBuf {
        static P: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
        P.get_or_init(|| std::path::PathBuf::from(":memory:"))
    }
    // ========================================================================
    // Stub methods for callers that haven't migrated to PG yet.
    // All return Err("SQLite removed") — callers handle the error gracefully.
    // ========================================================================
    pub fn get_conn(&self) -> Result<Connection, String> { Err("SQLite removed".into()) }
    pub fn get_conn_string(&self) -> Result<Connection, String> { Err("SQLite removed".into()) }
}

// Stub methods on CheckpointDb for callers not yet migrated to PG.
// Uses crate types where callers expect them.
use crate::mcp_client::types::{McpServerConfig, CreateMcpServerInput, UpdateMcpServerInput};

impl CheckpointDb {
    pub fn get_all_phase_token_usage_for_migration(&self) -> Result<Vec<PhaseTokenRow>, String> { Err("SQLite removed".into()) }
    pub fn list_mcp_servers(&self) -> Result<Vec<McpServerConfig>, String> { Err("SQLite removed".into()) }
    pub fn get_mcp_server(&self, _id: &str) -> Result<Option<McpServerConfig>, String> { Err("SQLite removed".into()) }
    pub fn create_mcp_server(&self, _input: CreateMcpServerInput) -> Result<McpServerConfig, String> { Err("SQLite removed".into()) }
    pub fn update_mcp_server(&self, _id: &str, _input: UpdateMcpServerInput) -> Result<McpServerConfig, String> { Err("SQLite removed".into()) }
    pub fn delete_mcp_server(&self, _id: &str) -> Result<(), String> { Err("SQLite removed".into()) }
    pub fn update_mcp_server_tools_cache(&self, _id: &str, _tools: &str, _now: &str) -> Result<(), String> { Err("SQLite removed".into()) }
    pub fn flush_partial_ai_output(&self, _task_run_id: &str, _output: &str, _iteration: i32) -> Result<(), String> { Err("SQLite removed".into()) }
    pub fn create_task_knowledge(&self, _task_run_id: &str, _category: &str, _agent: &str, _iteration: u32, _content: &str, _evidence: Option<&str>, _confidence: &str, _related: &[String]) -> Result<types::StoredTaskKnowledge, String> { Err("SQLite removed".into()) }
    pub fn list_task_knowledge(&self, _task_run_id: &str, _category: Option<&str>, _active_only: bool) -> Result<Vec<types::StoredTaskKnowledge>, String> { Err("SQLite removed".into()) }
    pub fn resolve_task_knowledge(&self, _id: &str, _notes: Option<&str>) -> Result<(), String> { Err("SQLite removed".into()) }
    pub fn update_task_run_transition_history(&self, _task_run_id: &str, _json: &str) -> Result<(), String> { Err("SQLite removed".into()) }
    pub fn save_orchestrator_checkpoint(&self, _id: &str, _task_run_id: &str, _iteration: u32, _trigger: &str, _state_json: &serde_json::Value, _name: Option<&str>) -> Result<(), String> { Err("SQLite removed".into()) }
    #[allow(clippy::too_many_arguments)]
    pub fn record_learning_outcome(
        &self, _task_run_id: &str, _status: &str, _duration: Option<f64>, _iterations: Option<u32>,
        _strategy: Option<&str>, _tools: Option<&[String]>, _files: Option<&[String]>,
        _error_type: Option<&str>, _error_msg: Option<&str>, _feedback: Option<&serde_json::Value>,
        _arch: Option<&str>, _step_count: Option<u32>, _verif_count: Option<u32>,
        _agentic_count: Option<u32>, _has_ui_bridge: bool, _tokens: Option<u64>, _cost: Option<f64>,
    ) -> Result<(), String> { Err("SQLite removed".into()) }
    pub fn create_orchestrator_verification_result<A, B, C>(&self, _task_run_id: &str, _plan_id: A, _iteration: B, _result: C, _is_critical: bool) -> Result<(), String> { Err("SQLite removed".into()) }
    pub fn get_iteration_verification_results(&self, _task_run_id: &str, _iteration: u32) -> Result<Vec<types::StoredVerificationResult>, String> { Err("SQLite removed".into()) }
    pub fn get_latest_verification_results(&self, _task_run_id: &str) -> Result<Vec<types::StoredVerificationResult>, String> { Err("SQLite removed".into()) }
    pub fn append_task_output(&self, _task_run_id: &str, _output: &str, _is_error: bool) -> Result<bool, String> { Err("SQLite removed".into()) }
    pub fn complete_task_run(&self, _task_run_id: &str) -> Result<(), String> { Err("SQLite removed".into()) }
    pub fn fail_task_run(&self, _task_run_id: &str, _error: &str) -> Result<(), String> { Err("SQLite removed".into()) }
    pub fn stop_task_run(&self, _task_run_id: &str) -> Result<(), String> { Err("SQLite removed".into()) }
    pub fn update_task_summary(&self, _task_run_id: &str, _summary: &str, _goal_achieved: bool, _remaining: Option<&str>) -> Result<(), String> { Err("SQLite removed".into()) }
    pub fn create_task_run(&self, _input: &types::CreateTaskRunInput) -> Result<(), String> { Err("SQLite removed".into()) }
    pub fn get_task_run(&self, _id: &str) -> Result<Option<serde_json::Value>, String> { Err("SQLite removed".into()) }
    pub fn create_process_session(&self, _session_id: &str, _config_id: &str, _process_name: &str) -> Result<String, String> { Err("SQLite removed".into()) }
    pub fn prune_old_process_sessions(&self, _config_id: &str, _max_count: i32) -> Result<u32, String> { Err("SQLite removed".into()) }
}

/// Placeholder for phase token usage migration data.
#[derive(Default)]
pub struct PhaseTokenRow {
    pub task_run_id: String,
    pub iteration: Option<u32>,
    pub phase: String,
    pub stage_index: Option<u32>,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub model_used: String,
    pub provider_used: String,
    pub duration_ms: Option<i64>,
    pub cost_cents: i64,
    pub created_at: String,
}

/// Placeholder for verification result data.
#[derive(Default)]
pub struct VerificationResultRow {
    pub task_run_id: String,
    pub iteration: u32,
    pub phase: String,
    pub stage_index: u32,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub model_used: String,
    pub provider_used: String,
}

/// Placeholder for task knowledge entries returned by CheckpointDb stubs.
#[derive(Default, Clone)]
pub struct TaskKnowledgeEntry {
    pub id: String,
    pub task_run_id: String,
    pub category: String,
    pub content: String,
    pub source: String,
    pub tags: Vec<String>,
    pub confidence: String,
    pub related_files: Vec<String>,
    pub priority: Option<String>,
    pub iteration: u32,
    pub evidence: Option<String>,
    pub name: String,
    pub enabled: bool,
    pub status: String,
    pub created_at: String,
}

/// Placeholder for `rusqlite::Connection`. All functions taking this are stubbed.
pub struct Connection;

/// Placeholder module mimicking `rusqlite` for dead code that still references it.
pub mod rusqlite_stub {
    /// Dummy params macro.
    #[macro_export]
    macro_rules! params {
        ($($x:expr),* $(,)?) => { &[] as &[&str] };
    }
    pub use params;
    pub mod types {
        pub trait ToSql {}
        impl<T> ToSql for T {}
    }
    pub struct Error;
    impl std::fmt::Display for Error {
        fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result { write!(f, "SQLite removed") }
    }
    pub type Result<T> = std::result::Result<T, Error>;
    pub trait OptionalExtension<T> {
        fn optional(self) -> std::result::Result<Option<T>, Error>;
    }
    impl<T> OptionalExtension<T> for Result<T> {
        fn optional(self) -> std::result::Result<Option<T>, Error> {
            match self {
                Ok(v) => Ok(Some(v)),
                Err(_) => Ok(None),
            }
        }
    }
    pub struct Row;
}

// Type-only submodules (SQLite impl removed, types retained for external use)
pub mod agentic_metrics_ops;
pub mod cross_run_ops;
pub mod embedding_client;
pub mod embeddings;
pub mod graph_ops;
pub mod hybrid_search;
pub mod pipeline_traces;
pub mod queue_ops;
pub mod token_analytics;
pub mod ui_bridge_ops;

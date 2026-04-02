//! Database module for qontinui-runner persistence.
//!
//! The SQLite (CheckpointDb) layer has been removed. All persistence now goes
//! through the PostgreSQL layer in `pg/`.
//!
//! `CheckpointDb` and `Connection` are retained as zero-size marker types so
//! that function signatures across the codebase (already stubbed to no-op)
//! continue to compile. No methods remain — all persistence is via PG.

// PostgreSQL layer (Clorinde-generated queries -- sole persistence backend)
pub mod pg;

// Shared domain types
pub mod types;
pub use types::*;

/// Zero-size placeholder for the removed SQLite database.
/// Retained only so function signatures that accept `&CheckpointDb` compile.
/// Methods below are stubs returning `Err("SQLite removed")` — callers handle
/// errors gracefully. These survive only because the callers have not been
/// migrated to PG yet.
pub struct CheckpointDb;

impl CheckpointDb {
    // Task knowledge stubs (used by orchestrator::knowledge)
    #[allow(clippy::too_many_arguments)]
    pub fn create_task_knowledge(&self, _task_run_id: &str, _category: &str, _agent: &str, _iteration: u32, _content: &str, _evidence: Option<&str>, _confidence: &str, _related: &[String]) -> Result<types::StoredTaskKnowledge, String> { Err("SQLite removed".into()) }
    pub fn list_task_knowledge(&self, _task_run_id: &str, _category: Option<&str>, _active_only: bool) -> Result<Vec<types::StoredTaskKnowledge>, String> { Err("SQLite removed".into()) }
    pub fn resolve_task_knowledge(&self, _id: &str, _notes: Option<&str>) -> Result<(), String> { Err("SQLite removed".into()) }

    // Verification result stubs (used by orchestrator::knowledge)
    pub fn get_iteration_verification_results(&self, _task_run_id: &str, _iteration: u32) -> Result<Vec<types::StoredVerificationResult>, String> { Err("SQLite removed".into()) }
    pub fn get_latest_verification_results(&self, _task_run_id: &str) -> Result<Vec<types::StoredVerificationResult>, String> { Err("SQLite removed".into()) }
}

/// Zero-size placeholder for the removed `rusqlite::Connection`.
/// Retained only so function signatures that accept `&Connection` compile.
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

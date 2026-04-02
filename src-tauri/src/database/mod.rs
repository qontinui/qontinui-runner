//! Database module for qontinui-runner persistence.
//!
//! All persistence goes through the PostgreSQL layer in `pg/`.

// PostgreSQL layer (Clorinde-generated queries -- sole persistence backend)
pub mod pg;

// Shared domain types
pub mod types;
pub use types::*;

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

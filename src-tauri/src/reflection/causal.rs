//! Causal Chain types and legacy insert stub.
//!
//! The PG implementations live on `PgDb` in `database/pg/reflection.rs`.
//! Only the shared types and the still-called `insert_causal_event` stub
//! remain here; the latter is a no-op pending a proper PG port and is
//! tracked as deferred work.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// A single causal edge: cause → effect.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CausalEvent {
    pub id: String,
    pub cause_event_type: String,
    pub cause_event_id: String,
    pub effect_event_type: String,
    pub effect_event_id: String,
    pub relationship: String,
    pub confidence: String,
    pub source: String,
    pub task_run_id: Option<String>,
    pub workflow_name: Option<String>,
    pub description: Option<String>,
    pub created_at: String,
}

/// A traced chain of connected causal events.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CausalChain {
    pub events: Vec<CausalEvent>,
    pub root_cause_type: String,
    pub root_cause_id: String,
    pub terminal_type: String,
    pub terminal_id: String,
    pub chain_length: usize,
}

/// Aggregate statistics for causal events in a workflow.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CausalSummary {
    pub total_links: u32,
    pub by_relationship: HashMap<String, u32>,
    pub by_cause_type: HashMap<String, u32>,
    pub avg_chain_length: f64,
}

// ---------------------------------------------------------------------------
// Insert (legacy stub — pending PG port, called from blame engine)
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
pub fn insert_causal_event(
    _cause_type: &str,
    _cause_id: &str,
    _effect_type: &str,
    _effect_id: &str,
    _relationship: &str,
    _confidence: &str,
    _source: &str,
    _task_run_id: Option<&str>,
    _workflow_name: Option<&str>,
    _description: Option<&str>,
) -> Result<String, String> {
    Err("SQLite removed".to_string())
}

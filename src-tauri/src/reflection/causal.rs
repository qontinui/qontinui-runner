//! Causal chain types shared across the reflection and blame-engine paths.
//!
//! The actual insert/query implementations live on `PgDb` in
//! `database/pg/spec_experimentation.rs` (`insert_causal_event`,
//! `get_causal_events_for_workflow`, etc.). This module only carries the
//! wire types.

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

//! Persistence layer for pipeline agent traces.
//!
//! Previously used SQLite for pipeline trace storage. All SQLite code has been
//! removed; functions are now stubs that return empty/Ok results. The PG
//! equivalents in `database::pg::pipeline_traces` are the primary path.

use tracing::debug;

use crate::autoresearch::agentic_verification::{
    PipelineAgentTrace, PipelineCheckpoint,
};

/// Save a batch of pipeline agent traces from a completed pipeline run.
pub fn save_pipeline_agent_traces(
    _task_run_id: &str,
    traces: &[PipelineAgentTrace],
) -> Result<usize, String> {
    debug!("save_pipeline_agent_traces stub: {} traces (no-op, use PG)", traces.len());
    Ok(traces.len())
}

/// Save a single pipeline agent trace incrementally (as each agent completes).
pub fn save_pipeline_agent_trace(
    _task_run_id: &str,
    _trace: &PipelineAgentTrace,
) -> Result<(), String> {
    debug!("save_pipeline_agent_trace stub (no-op, use PG)");
    Ok(())
}

/// Backfill `downstream_success` on all traces for a completed execution.
pub fn backfill_downstream_success(
    _task_run_id: &str,
    _success: bool,
) -> Result<usize, String> {
    debug!("backfill_downstream_success stub (no-op, use PG)");
    Ok(0)
}

/// L0: Get one-line summaries of agent traces grouped by agent_type.
pub fn get_trace_summaries_l0(
    _limit: u32,
) -> Result<Vec<crate::meta_optimizer::types::TraceSummaryL0>, String> {
    debug!("get_trace_summaries_l0 stub (no-op, use PG)");
    Ok(vec![])
}

/// L1: Get core trace fields for a given agent type (no snapshots/config).
pub fn get_trace_details_l1(
    _agent_type: Option<&str>,
    _limit: u32,
) -> Result<Vec<crate::meta_optimizer::types::TraceDetailL1>, String> {
    debug!("get_trace_details_l1 stub (no-op, use PG)");
    Ok(vec![])
}

/// Get aggregate metrics for pipeline agent traces, grouped by agent type.
pub fn get_agent_trace_aggregates(
    _limit: u32,
) -> Result<Vec<AgentTraceAggregate>, String> {
    debug!("get_agent_trace_aggregates stub (no-op, use PG)");
    Ok(vec![])
}

/// L2: Retrieve a single pipeline agent trace by its ID with all fields.
pub fn get_trace_full_l2(
    _trace_id: &str,
) -> Result<Option<PipelineAgentTrace>, String> {
    debug!("get_trace_full_l2 stub (no-op, use PG)");
    Ok(None)
}

/// Retrieve all pipeline agent traces for a given task run ID.
pub fn get_traces_for_task_run(
    _task_run_id: &str,
) -> Result<Vec<PipelineAgentTrace>, String> {
    debug!("get_traces_for_task_run stub (no-op, use PG)");
    Ok(vec![])
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AgentTraceAggregate {
    pub agent_type: String,
    pub run_count: i64,
    pub avg_duration_ms: f64,
    pub avg_cost_usd: f64,
    pub success_count: i64,
    pub failure_count: i64,
    pub avg_tokens_in: f64,
    pub avg_tokens_out: f64,
}

/// Get aggregate metrics for a specific agent type within a time window.
pub fn get_agent_aggregates_for_period(
    _agent_type: &str,
    _start: &str,
    _end: &str,
) -> Result<Option<AgentTraceAggregate>, String> {
    debug!("get_agent_aggregates_for_period stub (no-op, use PG)");
    Ok(None)
}

/// Cost-effectiveness data point for a specific agent and time period.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CostEffectivenessPoint {
    pub agent_type: String,
    pub period_label: String,
    pub avg_cost_usd: f64,
    pub success_rate: f64,
    pub run_count: i64,
}

/// Get cost-effectiveness data points grouped by agent type and ISO week.
pub fn get_agent_cost_effectiveness(
    _agent_type: Option<&str>,
    _days: i64,
) -> Result<Vec<CostEffectivenessPoint>, String> {
    debug!("get_agent_cost_effectiveness stub (no-op, use PG)");
    Ok(vec![])
}

/// Interaction between two agent types based on correlation of quality/success.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AgentInteraction {
    pub source_agent: String,
    pub target_agent: String,
    pub correlation: f64,
    pub sample_size: i64,
}

/// Get the interaction matrix showing correlations between agent pairs.
pub fn get_agent_interaction_matrix(
    _days: i64,
) -> Result<Vec<AgentInteraction>, String> {
    debug!("get_agent_interaction_matrix stub (no-op, use PG)");
    Ok(vec![])
}

/// Effect of a recommendation on a downstream agent.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CascadeEffect {
    pub affected_agent: String,
    pub before_success_rate: f64,
    pub after_success_rate: f64,
    pub delta: f64,
    pub sample_size_before: i64,
    pub sample_size_after: i64,
}

/// Get cascade effects: how an applied recommendation on one agent affected others.
pub fn get_agent_cascade_effect(
    _recommendation_id: &str,
) -> Result<Vec<CascadeEffect>, String> {
    debug!("get_agent_cascade_effect stub (no-op, use PG)");
    Ok(vec![])
}

/// Save (upsert) a pipeline checkpoint for a task run.
pub fn save_pipeline_checkpoint(
    _task_run_id: &str,
    _checkpoint: &PipelineCheckpoint,
) -> Result<(), String> {
    debug!("save_pipeline_checkpoint stub (no-op, use PG)");
    Ok(())
}

/// Load a pipeline checkpoint for a task run, if one exists.
pub fn get_pipeline_checkpoint(
    _task_run_id: &str,
) -> Result<Option<PipelineCheckpoint>, String> {
    debug!("get_pipeline_checkpoint stub (no-op, use PG)");
    Ok(None)
}

/// Clear the pipeline checkpoint for a task run.
pub fn clear_pipeline_checkpoint(_task_run_id: &str) -> Result<(), String> {
    debug!("clear_pipeline_checkpoint stub (no-op, use PG)");
    Ok(())
}

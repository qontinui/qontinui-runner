//! Token usage analytics queries.
//!
//! Provides aggregated analytics on AI token usage and costs from the
//! `phase_token_usage` table. Used by the LLM Observability dashboard.

use serde::{Deserialize, Serialize};

/// Daily cost aggregation row.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DailyCostRow {
    pub date: String,
    pub total_cost_cents: u64,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub call_count: u64,
}

/// Cost aggregation by model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelCostRow {
    pub model_used: String,
    pub total_cost_cents: u64,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub call_count: u64,
    pub avg_duration_ms: Option<u64>,
}

/// Cost aggregation by workflow phase.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhaseCostRow {
    pub phase: String,
    pub total_cost_cents: u64,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
}

/// Latency stats by provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderLatencyRow {
    pub provider_used: String,
    pub avg_duration_ms: u64,
    pub min_duration_ms: u64,
    pub max_duration_ms: u64,
    pub call_count: u64,
}

/// Per-task-run cost aggregation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRunCostRow {
    pub task_run_id: String,
    pub total_cost_cents: u64,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub call_count: u64,
    pub started_at: String,
}

/// Cost aggregation by target app (UI Bridge automation target).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TargetAppCostRow {
    pub target_app: String,
    pub total_cost_cents: u64,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub call_count: u64,
    pub avg_duration_ms: Option<u64>,
}

/// Cost aggregation by target page URL.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TargetPageCostRow {
    pub target_page_url: String,
    pub total_cost_cents: u64,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub call_count: u64,
}

/// Cost per successful UI Bridge interaction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CostPerInteractionRow {
    pub task_run_id: String,
    pub total_cost_cents: u64,
    pub successful_interactions: u64,
    pub cost_per_interaction_cents: f64,
}

/// Page complexity: cost breakdown by page URL.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageComplexityRow {
    pub target_page_url: String,
    pub call_count: u64,
    pub total_cost_cents: u64,
    pub avg_cost_per_call_cents: f64,
}

/// Model × action type success rate and cost.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelActionRow {
    pub model: String,
    pub action: String,
    pub total: u64,
    pub success_rate: f64,
    pub avg_cost_cents: f64,
}

/// Overall token usage summary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenUsageSummary {
    pub total_cost_cents: u64,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub total_calls: u64,
    pub unique_models: u64,
    pub unique_providers: u64,
    pub avg_cost_per_call_cents: f64,
    pub avg_duration_ms: Option<f64>,
}

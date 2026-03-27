//! Core data types for the autoresearch experiment loop.

use super::agentic_verification::{
    AgenticVerificationConfig, MultiAgentPipelineConfig, WorkflowArchitecture,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Top-level research configuration — the "program.md" equivalent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResearchConfig {
    /// Human-readable name for this research campaign.
    pub name: String,
    /// Workflow ID to use as the benchmark (single-workflow mode).
    #[serde(default)]
    pub benchmark_workflow_id: String,
    /// Multiple workflow IDs for multi-workflow campaigns.
    /// When non-empty, overrides `benchmark_workflow_id`.
    #[serde(default)]
    pub benchmark_workflow_ids: Vec<String>,
    /// Number of trials per experiment (for statistical robustness).
    #[serde(default = "default_trials")]
    pub trials_per_experiment: u32,
    /// The control (baseline) configuration.
    pub control_config: ExperimentConfig,
    /// Dimensions to search over.
    pub search_dimensions: Vec<SearchDimension>,
    /// How to pick the next experiment.
    #[serde(default)]
    pub mutation_strategy: MutationStrategy,
    /// How to decide whether to accept an experiment.
    #[serde(default)]
    pub acceptance_criteria: AcceptanceCriteria,
    /// Target runner port (defaults to 4321).
    #[serde(default = "default_runner_port")]
    pub target_runner_port: u16,
    /// Optional path to a YAML config file for hot-reload.
    #[serde(default)]
    pub config_file_path: Option<String>,
    /// How often to re-evaluate the control baseline (in number of experiments).
    /// 0 = every experiment, N = every N experiments.
    #[serde(default)]
    pub control_reeval_interval: u32,
    /// Whether to run each trial in an isolated git worktree.
    #[serde(default)]
    pub use_worktree: bool,
    /// Convergence detection configuration.
    #[serde(default)]
    pub convergence: ConvergenceConfig,
    /// AI-guided mutation configuration (only used when mutation_strategy = ai_guided).
    #[serde(default)]
    pub ai_guidance: AiGuidanceConfig,
    /// How to aggregate results across multiple workflows.
    #[serde(default)]
    pub multi_workflow_aggregation: MultiWorkflowAggregation,
}

impl ResearchConfig {
    /// Get the effective list of workflow IDs to benchmark.
    pub fn effective_workflow_ids(&self) -> Vec<String> {
        if !self.benchmark_workflow_ids.is_empty() {
            self.benchmark_workflow_ids.clone()
        } else if !self.benchmark_workflow_id.is_empty() {
            vec![self.benchmark_workflow_id.clone()]
        } else {
            vec![]
        }
    }
}

fn default_trials() -> u32 {
    3
}

fn default_runner_port() -> u16 {
    4321
}

/// A point in the search space: the knobs being tuned.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExperimentConfig {
    /// AI model to use (e.g. "claude-sonnet-4-20250514").
    #[serde(default)]
    pub model: Option<String>,
    /// Maximum iterations for the verification-agentic loop.
    #[serde(default)]
    pub max_iterations: Option<u32>,
    /// Whether to enable multi-agent fixer mode.
    #[serde(default)]
    pub multi_agent_mode: Option<bool>,
    /// Maximum context tokens for iteration context building.
    #[serde(default)]
    pub max_context_tokens: Option<usize>,
    /// Workflow execution architecture to use.
    #[serde(default)]
    pub workflow_architecture: Option<WorkflowArchitecture>,
    /// Configuration for the agentic verification loop.
    #[serde(default)]
    pub agentic_verification_config: Option<AgenticVerificationConfig>,
    /// Configuration for the multi-agent pipeline architecture.
    #[serde(default)]
    pub multi_agent_pipeline_config: Option<MultiAgentPipelineConfig>,
    /// Arbitrary additional overrides (passed as JSON to the /run endpoint).
    #[serde(default)]
    pub extra: HashMap<String, serde_json::Value>,
}

impl ExperimentConfig {
    /// Convert to a flat JSON overrides object for the /run endpoint.
    pub fn to_overrides(&self) -> serde_json::Value {
        let mut map = serde_json::Map::new();
        if let Some(ref model) = self.model {
            map.insert("model".to_string(), serde_json::json!(model));
        }
        if let Some(max_iter) = self.max_iterations {
            map.insert("max_iterations".to_string(), serde_json::json!(max_iter));
        }
        if let Some(multi) = self.multi_agent_mode {
            map.insert("multi_agent_mode".to_string(), serde_json::json!(multi));
        }
        if let Some(tokens) = self.max_context_tokens {
            map.insert("max_context_tokens".to_string(), serde_json::json!(tokens));
        }
        if let Some(ref arch) = self.workflow_architecture {
            map.insert("workflow_architecture".to_string(), serde_json::json!(arch));
        }
        if let Some(ref av_config) = self.agentic_verification_config {
            map.insert(
                "agentic_verification_config".to_string(),
                serde_json::json!(av_config),
            );
        }
        if let Some(ref map_config) = self.multi_agent_pipeline_config {
            map.insert(
                "multi_agent_pipeline_config".to_string(),
                serde_json::json!(map_config),
            );
        }
        for (k, v) in &self.extra {
            map.insert(k.clone(), v.clone());
        }

        // Merge pipeline_agent.<agent>.<field> keys into multi_agent_pipeline_config.
        // This allows Custom search dimensions to seamlessly vary per-agent fields.
        let agent_prefix = "pipeline_agent.";
        let agent_keys: Vec<(String, String, serde_json::Value)> = self
            .extra
            .iter()
            .filter_map(|(k, v)| {
                if let Some(rest) = k.strip_prefix(agent_prefix) {
                    let parts: Vec<&str> = rest.splitn(2, '.').collect();
                    if parts.len() == 2 {
                        return Some((parts[0].to_string(), parts[1].to_string(), v.clone()));
                    }
                }
                None
            })
            .collect();

        if !agent_keys.is_empty() {
            // Start from existing pipeline config or build a fresh one
            let mut pipeline_obj = match map.get("multi_agent_pipeline_config") {
                Some(serde_json::Value::Object(existing)) => existing.clone(),
                _ => serde_json::Map::new(),
            };

            for (agent, field, value) in agent_keys {
                let agent_obj = pipeline_obj
                    .entry(&agent)
                    .or_insert_with(|| serde_json::json!({}));
                if let serde_json::Value::Object(ref mut agent_map) = agent_obj {
                    agent_map.insert(field, value);
                }
            }

            map.insert(
                "multi_agent_pipeline_config".to_string(),
                serde_json::Value::Object(pipeline_obj),
            );

            // Remove the pipeline_agent.* keys from top-level — they've been merged
            let keys_to_remove: Vec<String> = map
                .keys()
                .filter(|k| k.starts_with(agent_prefix))
                .cloned()
                .collect();
            for k in keys_to_remove {
                map.remove(&k);
            }
        }

        serde_json::Value::Object(map)
    }

    /// Human-readable summary of this config (for logging/display).
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        if let Some(ref arch) = self.workflow_architecture {
            parts.push(format!("arch={}", arch));
        }
        if let Some(ref m) = self.model {
            parts.push(format!("model={}", m));
        }
        if let Some(i) = self.max_iterations {
            parts.push(format!("max_iter={}", i));
        }
        if let Some(ma) = self.multi_agent_mode {
            parts.push(format!("multi_agent={}", ma));
        }
        if let Some(t) = self.max_context_tokens {
            parts.push(format!("ctx_tokens={}", t));
        }
        for (k, v) in &self.extra {
            parts.push(format!("{}={}", k, v));
        }
        if parts.is_empty() {
            "(default)".to_string()
        } else {
            parts.join(", ")
        }
    }
}

/// An axis to explore in the search space.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SearchDimension {
    /// Model selection.
    Model { candidates: Vec<String> },
    /// Toggle multi-agent mode on/off.
    MultiAgentMode,
    /// Max iterations range.
    MaxIterations { range: [u32; 2] },
    /// Context token budget candidates.
    ContextTokens { candidates: Vec<usize> },
    /// Workflow execution architecture (traditional vs agentic_verification).
    WorkflowArchitecture,
    /// Arbitrary named dimension with string candidates.
    Custom {
        name: String,
        candidates: Vec<serde_json::Value>,
    },
}

impl SearchDimension {
    /// Human-readable label for this dimension.
    pub fn label(&self) -> &str {
        match self {
            SearchDimension::Model { .. } => "model",
            SearchDimension::MultiAgentMode => "multi_agent_mode",
            SearchDimension::MaxIterations { .. } => "max_iterations",
            SearchDimension::ContextTokens { .. } => "context_tokens",
            SearchDimension::WorkflowArchitecture => "workflow_architecture",
            SearchDimension::Custom { name, .. } => name,
        }
    }

    /// Number of candidates in this dimension.
    pub fn candidate_count(&self) -> usize {
        match self {
            SearchDimension::Model { candidates } => candidates.len(),
            SearchDimension::MultiAgentMode => 2,
            SearchDimension::MaxIterations { range } => (range[1] - range[0] + 1) as usize,
            SearchDimension::ContextTokens { candidates } => candidates.len(),
            SearchDimension::WorkflowArchitecture => 3,
            SearchDimension::Custom { candidates, .. } => candidates.len(),
        }
    }
}

/// How to pick the next experiment.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum MutationStrategy {
    /// Grid search one dimension at a time.
    #[default]
    Sequential,
    /// Randomly perturb 1-2 dimensions of the current control.
    RandomPerturbation,
    /// Consult an LLM after every N experiments to decide what to try next.
    AiGuided,
    /// Q-learning router: selects WorkflowArchitecture based on learned
    /// task-feature→architecture Q-values. Falls back to grid search when
    /// insufficient data exists for the current task state.
    QLearning,
}

/// Configuration for AI-guided mutation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiGuidanceConfig {
    /// How many experiments between LLM guidance calls.
    #[serde(default = "default_batch_size")]
    pub batch_size: u32,
    /// Model to use for guidance (None = use default AI settings).
    #[serde(default)]
    pub model: Option<String>,
}

impl Default for AiGuidanceConfig {
    fn default() -> Self {
        Self {
            batch_size: 5,
            model: None,
        }
    }
}

fn default_batch_size() -> u32 {
    5
}

/// Convergence detection configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConvergenceConfig {
    /// Whether convergence detection is enabled.
    #[serde(default)]
    pub enabled: bool,
    /// Auto-pause after this many consecutive rejected experiments.
    #[serde(default = "default_no_improvement_threshold")]
    pub no_improvement_threshold: u32,
}

impl Default for ConvergenceConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            no_improvement_threshold: 10,
        }
    }
}

fn default_no_improvement_threshold() -> u32 {
    10
}

/// How to aggregate results when running the same config across multiple workflows.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum MultiWorkflowAggregation {
    /// Average pass rates across all workflows (default).
    #[default]
    Average,
    /// Experiment must pass all workflows to be accepted.
    AllMustPass,
    /// Experiment accepted if any workflow passes.
    AnyPass,
}

/// How to decide whether to accept an experiment's result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcceptanceCriteria {
    /// Primary metric to optimize.
    #[serde(default = "default_primary_metric")]
    pub primary_metric: PrimaryMetric,
    /// Minimum improvement ratio to accept (1.0 = must be strictly better).
    #[serde(default = "default_min_improvement")]
    pub min_improvement_ratio: f64,
    /// Minimum p-value threshold for statistical significance.
    #[serde(default = "default_significance")]
    pub significance_threshold: f64,
}

impl Default for AcceptanceCriteria {
    fn default() -> Self {
        Self {
            primary_metric: PrimaryMetric::PassRate,
            min_improvement_ratio: 1.0,
            significance_threshold: 0.05,
        }
    }
}

fn default_primary_metric() -> PrimaryMetric {
    PrimaryMetric::PassRate
}
fn default_min_improvement() -> f64 {
    1.0
}
fn default_significance() -> f64 {
    0.05
}

/// Which metric to optimize for.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PrimaryMetric {
    #[default]
    PassRate,
    MeanIterations,
    MeanDuration,
    SpecCompliance,
    /// Optimize for composite agentic metric score (weighted blend of all scored metrics).
    CompositeAgentic,
}

/// Outcome of a single trial (one workflow run).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrialResult {
    /// The task_run_id assigned by the runner.
    pub task_run_id: String,
    /// Whether the workflow passed verification.
    pub passed: bool,
    /// Number of iterations the loop used.
    pub iterations_used: u32,
    /// Wall-clock duration in milliseconds.
    pub duration_ms: u64,
    /// Which workflow was tested (for multi-workflow campaigns).
    #[serde(default)]
    pub workflow_id: Option<String>,
    /// Spec compliance score (0.0-1.0) if this was a spec-generated workflow.
    #[serde(default)]
    pub spec_compliance_score: Option<f64>,
    /// Number of spec assertions that passed.
    #[serde(default)]
    pub spec_assertions_passed: Option<u32>,
    /// Total number of spec assertions.
    #[serde(default)]
    pub spec_assertions_total: Option<u32>,
    /// Composite agentic metric score (0.0-1.0) from agentic_metric_scores.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub composite_agentic_score: Option<f64>,
    /// Per-metric agentic scores (metric_type -> score). Enables per-dimension
    /// analysis (e.g., "this config improved tool_correctness but degraded step_efficiency").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agentic_scores: Option<HashMap<String, f64>>,
}

/// Aggregate metrics across multiple trials.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AggregateMetrics {
    pub pass_rate: f64,
    pub mean_iterations: f64,
    pub mean_duration_ms: f64,
    pub trial_count: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mean_spec_compliance: Option<f64>,
    /// Mean composite agentic score across trials that have one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mean_composite_agentic_score: Option<f64>,
    /// Per-metric mean scores across trials (metric_type -> mean_score).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mean_agentic_scores: Option<HashMap<String, f64>>,
}

/// Full result of one experiment (one config tested, possibly multiple trials).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExperimentResult {
    /// Which config was tested.
    pub config: ExperimentConfig,
    /// Individual trial results.
    pub trials: Vec<TrialResult>,
    /// Aggregate metrics.
    pub aggregate: AggregateMetrics,
    /// Whether this experiment was accepted as the new control.
    pub accepted: bool,
    /// Human-readable reason for accept/reject.
    pub reason: String,
    /// Statistical significance p-value (None if only 1 trial).
    #[serde(default)]
    pub p_value: Option<f64>,
    /// AI recommendation that led to this experiment (if ai_guided).
    #[serde(default)]
    pub ai_recommendation: Option<String>,
}

/// Summary of a historical campaign (for listing past campaigns).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CampaignSummary {
    pub id: String,
    pub name: String,
    pub status: String,
    pub experiment_count: i64,
    pub accepted_count: i64,
    pub config_json: String,
    pub created_at: String,
}

/// Status of a running campaign.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CampaignStatus {
    pub campaign_id: String,
    pub name: String,
    pub status: CampaignState,
    pub experiment_count: u32,
    pub accepted_count: u32,
    pub current_control: ExperimentConfig,
    pub current_experiment: Option<ExperimentConfig>,
    pub started_at: Option<String>,
    pub error: Option<String>,
    /// Number of consecutive experiments without acceptance (for convergence tracking).
    #[serde(default)]
    pub experiments_since_last_accept: u32,
}

/// Campaign lifecycle state.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum CampaignState {
    Running,
    Stopped,
    Completed,
    Failed,
    /// Paused due to convergence detection.
    Converged,
}

impl std::fmt::Display for CampaignState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CampaignState::Running => write!(f, "running"),
            CampaignState::Stopped => write!(f, "stopped"),
            CampaignState::Completed => write!(f, "completed"),
            CampaignState::Failed => write!(f, "failed"),
            CampaignState::Converged => write!(f, "converged"),
        }
    }
}

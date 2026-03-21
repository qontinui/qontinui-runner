//! Shared types for the meta-optimizer system.

use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::config_storage::ConfigStorage;
use crate::AppState;

/// Dependencies required to launch a meta-optimizer workflow.
/// Mirrors `FixerDeps` from the fixer module.
pub struct MetaOptimizerDeps {
    pub app_state: Arc<AppState>,
    pub config_storage: Arc<tokio::sync::Mutex<ConfigStorage>>,
    pub app_handle: tauri::AppHandle,
    pub pid_tracker: Arc<std::sync::Mutex<Vec<u32>>>,
    pub session_manager: Option<Arc<crate::claude_session::SessionManager>>,
}

/// Workflow category filter for progress tracking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowCategory {
    /// Only main workflows (not reflection/fixer/follow-up/meta-optimizer)
    Main,
    /// Only reflection runs
    Reflections,
    /// Only fixer runs
    Fixers,
    /// Only follow-up runs
    FollowUps,
    /// Only meta-optimizer runs
    MetaOptimizer,
}

impl WorkflowCategory {
    /// Returns a SQL WHERE clause fragment to filter task_runs by this category.
    /// `alias` is the table alias for task_runs (e.g., "tr").
    pub fn sql_filter(&self, alias: &str) -> String {
        match self {
            Self::Main => format!(
                " AND COALESCE({a}.is_fixer, 0) = 0 \
                 AND COALESCE({a}.is_reflection, 0) = 0 \
                 AND COALESCE({a}.is_follow_up, 0) = 0 \
                 AND COALESCE({a}.is_meta_optimizer, 0) = 0",
                a = alias
            ),
            Self::Reflections => format!(" AND COALESCE({}.is_reflection, 0) = 1", alias),
            Self::Fixers => format!(" AND COALESCE({}.is_fixer, 0) = 1", alias),
            Self::FollowUps => format!(" AND COALESCE({}.is_follow_up, 0) = 1", alias),
            Self::MetaOptimizer => format!(" AND COALESCE({}.is_meta_optimizer, 0) = 1", alias),
        }
    }

    /// Returns the snapshot type suffix for this category.
    pub fn snapshot_suffix(&self) -> &'static str {
        match self {
            Self::Main => "",
            Self::Reflections => "_reflection",
            Self::Fixers => "_fixer",
            Self::FollowUps => "_followup",
            Self::MetaOptimizer => "_metaopt",
        }
    }

    /// Parse from string (for Tauri command parameters).
    pub fn from_str_opt(s: Option<&str>) -> Self {
        match s {
            Some("reflections") => Self::Reflections,
            Some("fixers") => Self::Fixers,
            Some("follow_ups") => Self::FollowUps,
            Some("meta_optimizer") => Self::MetaOptimizer,
            _ => Self::Main,
        }
    }
}

/// Which optimizer to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OptimizerType {
    PipelinePrompt,
    Architecture,
    GenerationTemplate,
}

impl OptimizerType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::PipelinePrompt => "pipeline_prompt",
            Self::Architecture => "architecture",
            Self::GenerationTemplate => "generation_template",
        }
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            Self::PipelinePrompt => "Pipeline Prompt Optimizer",
            Self::Architecture => "Architecture Optimizer",
            Self::GenerationTemplate => "Generation Template Optimizer",
        }
    }

    pub fn all() -> &'static [OptimizerType] {
        &[
            OptimizerType::PipelinePrompt,
            OptimizerType::Architecture,
            OptimizerType::GenerationTemplate,
        ]
    }
}

impl std::fmt::Display for OptimizerType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Tiered context loading level.
///
/// L0 = one-line summaries (index), L1 = core fields for planning,
/// L2 = full records with all fields (existing behavior).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ContextTier {
    #[default]
    L0,
    L1,
    L2,
}

/// L0 summary of agent trace aggregates: agent_type + count + success percentage.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct TraceSummaryL0 {
    pub agent_type: String,
    pub run_count: i64,
    pub success_pct: f64,
}

/// L1 detail of a single agent trace: core fields without snapshots/config.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct TraceDetailL1 {
    pub id: String,
    pub agent_type: String,
    pub duration_ms: i64,
    pub downstream_success: Option<bool>,
    pub created_at: String,
}

/// L0 summary of reflection fixes: counts grouped by fix_type and effectiveness.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ReflectionFixSummaryL0 {
    pub fix_type: String,
    pub total_count: i64,
    pub effective_count: i64,
}

/// L1 detail of a reflection fix: core fields without old_value, new_value,
/// file_changed, reasoning, embeddings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ReflectionFixDetailL1 {
    pub id: String,
    pub fix_type: String,
    pub fix_description: String,
    pub confidence: String,
    pub effectiveness: Option<String>,
    pub source_agent: Option<String>,
    pub created_at: String,
}

/// A recommendation produced by a meta-optimizer run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Recommendation {
    pub id: String,
    pub optimizer_type: String,
    pub recommendation_type: String,
    pub target_agent: Option<String>,
    pub title: String,
    pub description: String,
    pub current_value: Option<String>,
    pub recommended_value: Option<String>,
    pub evidence: Option<String>,
    pub confidence: f64,
    pub status: String,
    pub applied_at: Option<String>,
    pub outcome_after_apply: Option<String>,
    pub optimizer_run_id: Option<String>,
    pub created_at: String,
}

/// A prompt variant stored in the prompt registry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptVariant {
    pub id: String,
    pub agent_type: String,
    pub variant_name: String,
    pub prompt_content: String,
    pub version: i32,
    pub is_active: bool,
    pub source_recommendation_id: Option<String>,
    pub performance_metrics: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// A meta-optimizer run record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetaOptimizerRun {
    pub id: String,
    pub optimizer_type: String,
    pub trigger_type: String,
    pub runs_analyzed: i64,
    pub recommendations_produced: i64,
    pub task_run_id: Option<String>,
    pub status: String,
    pub created_at: String,
    pub completed_at: Option<String>,
}

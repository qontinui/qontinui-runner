//! Learning Orchestrator
//!
//! Ties together all adaptive learning components into a unified post-run hook.
//! After every workflow run completes, the orchestrator:
//!
//! 1. **Always** reflects on the run and curates lessons into the playbook
//! 2. **Always** tracks template usage and performance
//! 3. **Always** considers extracting few-shot examples
//! 4. **Periodically** extracts new templates from successful patterns
//! 5. **Periodically** triggers GEPA prompt optimization
//!
//! All learning is async and non-blocking — it never delays workflow execution.

use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};

use super::few_shot_curator::FewShotCurator;
use super::gepa_optimizer::GepaIntegration;
use super::reflector::{CurationResult, PlaybookCurator, StepEvaluationSummary};
use super::template_lifecycle::TemplateLifecycleManager;

// ============================================================================
// Configuration
// ============================================================================

/// Configuration for the learning orchestrator.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LearningConfig {
    /// How many runs between GEPA optimization rounds.
    pub gepa_trigger_interval: usize,
    /// Minimum time between GEPA runs.
    pub gepa_cooldown_secs: u64,
    /// How many runs between template extraction checks.
    pub template_extraction_interval: usize,
    /// Whether GEPA integration is enabled.
    pub gepa_enabled: bool,
    /// URL of the GEPA Python sidecar service.
    pub gepa_url: String,
    /// URL of the evaluation API (for GEPA metrics).
    pub evaluation_api_url: String,
    /// Minimum new PRM training examples before triggering a retrain.
    pub prm_retrain_threshold: usize,
    /// URL of the PRM stats endpoint on the runner.
    pub prm_stats_url: String,
    /// URL of the PRM retrain endpoint (CLI or API).
    pub prm_export_url: String,
    /// URL of the PRM scoring service (qontinui-prm FastAPI server).
    pub prm_service_url: String,
}

impl Default for LearningConfig {
    fn default() -> Self {
        Self {
            gepa_trigger_interval: 20,
            gepa_cooldown_secs: 3600,
            template_extraction_interval: 10,
            gepa_enabled: false,
            gepa_url: "http://localhost:8400/gepa".to_string(),
            evaluation_api_url: "http://localhost:1420".to_string(),
            prm_retrain_threshold: 100,
            prm_stats_url: "http://localhost:1420/prm/stats".to_string(),
            prm_export_url: "http://localhost:1420/prm/export".to_string(),
            prm_service_url: "http://localhost:8400".to_string(),
        }
    }
}

// ============================================================================
// Orchestrator
// ============================================================================

/// Central orchestrator for all adaptive learning components.
pub struct LearningOrchestrator {
    pub curator: PlaybookCurator,
    pub few_shot_curator: FewShotCurator,
    pub template_manager: TemplateLifecycleManager,
    pub gepa: GepaIntegration,
    pub config: LearningConfig,
    /// Tracks runs since last GEPA optimization.
    runs_since_gepa: usize,
    /// Tracks runs since last template extraction.
    runs_since_extraction: usize,
    /// Total runs processed (for PRM retrain threshold).
    total_runs: usize,
    /// Runs at last PRM retrain trigger.
    runs_at_last_prm_retrain: usize,
    /// Last GEPA optimization timestamp.
    last_gepa_time: Option<Instant>,
}

impl LearningOrchestrator {
    /// Create a new orchestrator with default configuration.
    pub fn new(config: LearningConfig) -> Self {
        let gepa = GepaIntegration::new(&config.gepa_url);

        Self {
            curator: PlaybookCurator::new(),
            few_shot_curator: FewShotCurator::new(),
            template_manager: TemplateLifecycleManager::new(),
            gepa,
            config,
            runs_since_gepa: 0,
            runs_since_extraction: 0,
            total_runs: 0,
            runs_at_last_prm_retrain: 0,
            last_gepa_time: None,
        }
    }

    /// Called after every workflow run completes.
    ///
    /// Runs all learning components in sequence. Each component is independent
    /// and failures in one don't affect others.
    pub fn on_run_complete(&mut self, run_context: &RunContext) -> LearningActions {
        // SQLite removed — return empty actions (no-op)
        LearningActions::default()
    }

    /// Check if enough time has passed since last GEPA optimization.
    fn gepa_cooldown_elapsed(&self) -> bool {
        match self.last_gepa_time {
            None => true,
            Some(last) => last.elapsed() >= Duration::from_secs(self.config.gepa_cooldown_secs),
        }
    }
}

// ============================================================================
// Context types
// ============================================================================

/// Context about a completed workflow run, passed to the learning orchestrator.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunContext {
    /// Unique run ID.
    pub run_id: String,
    /// Overall evaluation score (0.0-1.0).
    pub overall_score: f64,
    /// Number of fix iterations performed (0 = first-attempt success).
    pub iterations: u32,
    /// Verification domain of the run.
    pub domain: String,
    /// Step evaluation summaries: (step_id, criterion_desc, score, steps_json).
    #[serde(default)]
    pub step_evaluation_summaries: Option<Vec<(String, String, f64, String)>>,
    /// Templates used: (template_id, template_name, passed, quality_score).
    #[serde(default)]
    pub used_templates: Option<Vec<(String, String, bool, f64)>>,
    /// Workflow description.
    #[serde(default)]
    pub description: Option<String>,
}

// ============================================================================
// Actions result
// ============================================================================

/// Summary of all learning actions taken after a run.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LearningActions {
    /// Number of new playbook lessons added.
    pub lessons_added: usize,
    /// Number of lessons merged with existing entries.
    pub lessons_merged: usize,
    /// Number of lessons retired.
    pub lessons_retired: usize,
    /// Number of few-shot examples extracted.
    pub examples_extracted: usize,
    /// Number of templates whose usage was tracked.
    pub templates_tracked: usize,
    /// Number of template lifecycle transitions applied.
    pub lifecycle_actions: usize,
    /// Whether GEPA optimization was triggered this run.
    pub gepa_triggered: bool,
    /// Whether PRM retraining was triggered this run.
    pub prm_retrain_triggered: bool,
}

impl LearningActions {
    /// Whether any learning activity occurred.
    pub fn has_activity(&self) -> bool {
        self.lessons_added > 0
            || self.lessons_merged > 0
            || self.lessons_retired > 0
            || self.examples_extracted > 0
            || self.lifecycle_actions > 0
            || self.gepa_triggered
            || self.prm_retrain_triggered
    }
}

// ============================================================================
// Prompt injection helpers
// ============================================================================

/// Build the full adaptive learning context for injection into generation prompts.
///
/// Combines playbook lessons and few-shot examples into a single markdown block.
pub fn build_learning_context(
    domain: Option<&str>,
    max_lessons: usize,
    max_examples: usize,
) -> String {
    String::new()
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = LearningConfig::default();
        assert_eq!(config.gepa_trigger_interval, 20);
        assert_eq!(config.template_extraction_interval, 10);
        assert!(!config.gepa_enabled);
    }

    #[test]
    fn test_learning_actions_has_activity() {
        let empty = LearningActions::default();
        assert!(!empty.has_activity());

        let with_lessons = LearningActions {
            lessons_added: 1,
            ..Default::default()
        };
        assert!(with_lessons.has_activity());
    }

    #[test]
    fn test_run_context_serialization() {
        let ctx = RunContext {
            run_id: "test-run-1".to_string(),
            overall_score: 0.85,
            iterations: 0,
            domain: "api".to_string(),
            step_evaluation_summaries: None,
            used_templates: None,
            description: Some("Test workflow".to_string()),
        };
        let json = serde_json::to_string(&ctx).unwrap();
        let parsed: RunContext = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.run_id, "test-run-1");
        assert_eq!(parsed.overall_score, 0.85);
    }
}

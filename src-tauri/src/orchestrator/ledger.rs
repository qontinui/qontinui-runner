//! Task Ledger Pattern
//!
//! Inspired by AutoGen's Magentic-One orchestrator, this module implements
//! a task ledger that tracks:
//!
//! - **Facts**: What we've learned during task execution
//! - **Progress**: Metrics showing work completion
//! - **Stall Detection**: Identifying when the task is spinning without progress
//! - **Plan Evolution**: Dynamic plan adjustments without full replanning
//!
//! # Design Goals
//!
//! 1. **Progress Visibility**: Know how far along the task is
//! 2. **Stall Recovery**: Detect and recover from spinning loops
//! 3. **Adaptive Planning**: Update the plan based on discovered facts
//! 4. **Context Efficiency**: Provide focused context to workers
//!
//! # Usage
//!
//! The ledger is updated after each iteration and used to:
//! - Decide whether to continue, replan, or abort
//! - Build context for workers
//! - Track success across iterations

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

// ============================================================================
// Task Ledger
// ============================================================================

/// The task ledger maintains state across iterations.
///
/// This is the central data structure for tracking task progress,
/// accumulated knowledge, and detecting issues like stalls.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskLedger {
    /// The original goal/task description.
    pub original_goal: String,

    /// Facts discovered during execution.
    /// These are things we've learned that are confirmed true.
    pub facts: Vec<Fact>,

    /// The current execution plan (can evolve).
    pub plan: ExecutionPlan,

    /// Progress tracking metrics.
    pub progress: ProgressTracker,

    /// Stall detection state.
    pub stall_detector: StallDetector,

    /// Timestamp when the ledger was created (RFC3339).
    pub created_at: String,

    /// Timestamp of last update (RFC3339).
    pub last_updated: String,

    /// Number of times the plan has been revised.
    pub plan_revisions: u32,
}

impl TaskLedger {
    /// Create a new task ledger for a goal.
    pub fn new(goal: impl Into<String>) -> Self {
        let now = chrono::Utc::now().to_rfc3339();
        Self {
            original_goal: goal.into(),
            facts: Vec::new(),
            plan: ExecutionPlan::default(),
            progress: ProgressTracker::new(),
            stall_detector: StallDetector::new(),
            created_at: now.clone(),
            last_updated: now,
            plan_revisions: 0,
        }
    }

    /// Initialize the ledger with a verification plan.
    pub fn with_plan(mut self, plan: ExecutionPlan) -> Self {
        self.plan = plan;
        self
    }

    /// Record a new fact discovered during execution.
    pub fn record_fact(&mut self, fact: Fact) {
        // Check for contradicting facts
        if let Some(existing) = self.facts.iter_mut().find(|f| f.key == fact.key) {
            if existing.value != fact.value {
                info!(
                    "Fact '{}' updated: '{}' -> '{}'",
                    fact.key, existing.value, fact.value
                );
                existing.value = fact.value.clone();
                existing.updated_at = Some(chrono::Utc::now().to_rfc3339());
                existing.confidence = fact.confidence;
            }
        } else {
            self.facts.push(fact);
        }
        self.last_updated = chrono::Utc::now().to_rfc3339();
    }

    /// Record multiple facts at once.
    pub fn record_facts(&mut self, facts: Vec<Fact>) {
        for fact in facts {
            self.record_fact(fact);
        }
    }

    /// Update progress after an iteration.
    pub fn update_progress(&mut self, update: ProgressUpdate) {
        self.progress.apply_update(&update);
        self.stall_detector.record_iteration(&update);
        self.last_updated = chrono::Utc::now().to_rfc3339();
    }

    /// Check if the task appears to be stalled.
    pub fn is_stalled(&self) -> bool {
        self.stall_detector.is_stalled()
    }

    /// Get the stall reason if stalled.
    pub fn stall_reason(&self) -> Option<String> {
        self.stall_detector.stall_reason()
    }

    /// Get facts relevant to a specific topic.
    pub fn facts_for_topic(&self, topic: &str) -> Vec<&Fact> {
        self.facts
            .iter()
            .filter(|f| f.key.contains(topic) || f.tags.iter().any(|t| t.contains(topic)))
            .collect()
    }

    /// Get high-confidence facts only.
    pub fn confirmed_facts(&self) -> Vec<&Fact> {
        self.facts
            .iter()
            .filter(|f| f.confidence == FactConfidence::Confirmed)
            .collect()
    }

    /// Mark a plan step as completed.
    pub fn complete_step(&mut self, step_id: &str) {
        if let Some(step) = self.plan.steps.iter_mut().find(|s| s.id == step_id) {
            step.status = StepStatus::Completed;
            self.progress.steps_completed += 1;
        }
        self.last_updated = chrono::Utc::now().to_rfc3339();
    }

    /// Mark a plan step as blocked.
    pub fn block_step(&mut self, step_id: &str, reason: impl Into<String>) {
        if let Some(step) = self.plan.steps.iter_mut().find(|s| s.id == step_id) {
            step.status = StepStatus::Blocked;
            step.notes = Some(reason.into());
        }
        self.last_updated = chrono::Utc::now().to_rfc3339();
    }

    /// Add a new step to the plan.
    pub fn add_step(&mut self, step: PlanStep) {
        self.plan.steps.push(step);
        self.plan_revisions += 1;
        self.last_updated = chrono::Utc::now().to_rfc3339();
    }

    /// Build context for workers based on the ledger.
    pub fn build_worker_context(&self) -> String {
        let mut context = String::new();

        // Goal reminder
        context.push_str(&format!("## Task Goal\n\n{}\n\n", self.original_goal));

        // Key facts
        let confirmed = self.confirmed_facts();
        if !confirmed.is_empty() {
            context.push_str("## Confirmed Facts\n\n");
            for fact in confirmed.iter().take(10) {
                context.push_str(&format!("- **{}**: {}\n", fact.key, fact.value));
            }
            context.push('\n');
        }

        // Current plan status
        let pending: Vec<_> = self
            .plan
            .steps
            .iter()
            .filter(|s| s.status == StepStatus::Pending || s.status == StepStatus::InProgress)
            .collect();

        if !pending.is_empty() {
            context.push_str("## Remaining Work\n\n");
            for step in pending.iter().take(5) {
                let status = match step.status {
                    StepStatus::InProgress => "🔄",
                    _ => "⏳",
                };
                context.push_str(&format!("{} {}\n", status, step.description));
            }
            context.push('\n');
        }

        // Progress summary
        context.push_str(&format!(
            "## Progress\n\n- Iteration: {}\n- Criteria passing: {}/{}\n- Estimated completion: {:.0}%\n\n",
            self.progress.current_iteration,
            self.progress.criteria_passing,
            self.progress.criteria_total,
            self.progress.estimated_completion() * 100.0
        ));

        // Stall warning if applicable
        if self.stall_detector.warning_signs() {
            context.push_str(
                "⚠️ **Warning**: Progress appears slow. Focus on the most impactful changes.\n\n",
            );
        }

        context
    }

    /// Serialize the ledger for persistence.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }

    /// Deserialize a ledger from JSON.
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }
}

// ============================================================================
// Facts
// ============================================================================

/// A fact discovered during task execution.
///
/// Facts are things we've learned that inform our understanding
/// of the problem and solution space.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Fact {
    /// Unique key for this fact (for deduplication/updates).
    pub key: String,

    /// The fact value/description.
    pub value: String,

    /// Confidence level in this fact.
    pub confidence: FactConfidence,

    /// When this fact was discovered (RFC3339).
    pub discovered_at: String,

    /// When this fact was last updated (RFC3339, if ever).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,

    /// Iteration when discovered.
    pub iteration: u32,

    /// Tags for categorization.
    #[serde(default)]
    pub tags: Vec<String>,

    /// Source of this fact (e.g., "verification", "worker", "analysis").
    pub source: String,
}

impl Fact {
    /// Create a new fact.
    pub fn new(key: impl Into<String>, value: impl Into<String>, iteration: u32) -> Self {
        Self {
            key: key.into(),
            value: value.into(),
            confidence: FactConfidence::Likely,
            discovered_at: chrono::Utc::now().to_rfc3339(),
            updated_at: None,
            iteration,
            tags: Vec::new(),
            source: "worker".to_string(),
        }
    }

    /// Create a confirmed fact.
    pub fn confirmed(key: impl Into<String>, value: impl Into<String>, iteration: u32) -> Self {
        Self {
            confidence: FactConfidence::Confirmed,
            ..Self::new(key, value, iteration)
        }
    }

    /// Create a fact from verification results.
    pub fn from_verification(
        key: impl Into<String>,
        value: impl Into<String>,
        iteration: u32,
    ) -> Self {
        Self {
            confidence: FactConfidence::Confirmed,
            source: "verification".to_string(),
            ..Self::new(key, value, iteration)
        }
    }

    /// Add a tag to the fact.
    pub fn with_tag(mut self, tag: impl Into<String>) -> Self {
        self.tags.push(tag.into());
        self
    }

    /// Set the source.
    pub fn with_source(mut self, source: impl Into<String>) -> Self {
        self.source = source.into();
        self
    }
}

/// Confidence level for facts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FactConfidence {
    /// Confirmed by verification or evidence.
    Confirmed,
    /// Likely true based on analysis.
    Likely,
    /// Hypothesis that needs verification.
    Hypothesis,
    /// Superseded or incorrect (kept for history).
    Superseded,
}

// ============================================================================
// Execution Plan
// ============================================================================

/// The execution plan with tracked steps.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExecutionPlan {
    /// Plan steps in order.
    pub steps: Vec<PlanStep>,

    /// Overall plan status.
    pub status: PlanStatus,

    /// Notes about the plan.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

impl ExecutionPlan {
    /// Create a new plan from steps.
    pub fn from_steps(steps: Vec<PlanStep>) -> Self {
        Self {
            steps,
            status: PlanStatus::Active,
            notes: None,
        }
    }

    /// Get the number of completed steps.
    pub fn completed_count(&self) -> usize {
        self.steps
            .iter()
            .filter(|s| s.status == StepStatus::Completed)
            .count()
    }

    /// Get the number of pending steps.
    pub fn pending_count(&self) -> usize {
        self.steps
            .iter()
            .filter(|s| s.status == StepStatus::Pending || s.status == StepStatus::InProgress)
            .count()
    }

    /// Get completion percentage.
    pub fn completion_percentage(&self) -> f32 {
        if self.steps.is_empty() {
            return 0.0;
        }
        self.completed_count() as f32 / self.steps.len() as f32
    }
}

/// A step in the execution plan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanStep {
    /// Unique identifier for this step.
    pub id: String,

    /// Description of what needs to be done.
    pub description: String,

    /// Current status.
    pub status: StepStatus,

    /// Optional notes about this step.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,

    /// Related criteria IDs.
    #[serde(default)]
    pub related_criteria: Vec<String>,
}

impl PlanStep {
    /// Create a new plan step.
    pub fn new(id: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            description: description.into(),
            status: StepStatus::Pending,
            notes: None,
            related_criteria: Vec::new(),
        }
    }
}

/// Status of a plan step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    #[default]
    Pending,
    InProgress,
    Completed,
    Blocked,
    Skipped,
}

/// Overall plan status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanStatus {
    #[default]
    Active,
    Completed,
    NeedsRevision,
    Abandoned,
}

// ============================================================================
// Progress Tracking
// ============================================================================

/// Tracks progress across iterations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProgressTracker {
    /// Current iteration number.
    pub current_iteration: u32,

    /// Maximum allowed iterations.
    pub max_iterations: u32,

    /// Number of criteria currently passing.
    pub criteria_passing: u32,

    /// Total number of criteria.
    pub criteria_total: u32,

    /// Number of plan steps completed.
    pub steps_completed: u32,

    /// Total number of plan steps.
    pub steps_total: u32,

    /// Files modified across all iterations.
    pub files_modified: Vec<String>,

    /// Number of findings recorded.
    pub findings_count: u32,

    /// History of criteria passing per iteration.
    pub criteria_history: Vec<u32>,
}

impl ProgressTracker {
    /// Create a new progress tracker.
    pub fn new() -> Self {
        Self {
            current_iteration: 0,
            max_iterations: 10,
            criteria_passing: 0,
            criteria_total: 0,
            steps_completed: 0,
            steps_total: 0,
            files_modified: Vec::new(),
            findings_count: 0,
            criteria_history: Vec::new(),
        }
    }

    /// Apply an iteration update.
    pub fn apply_update(&mut self, update: &ProgressUpdate) {
        self.current_iteration = update.iteration;
        self.criteria_passing = update.criteria_passing;
        self.criteria_total = update.criteria_total;
        self.findings_count += update.new_findings;

        // Track files
        for file in &update.files_modified {
            if !self.files_modified.contains(file) {
                self.files_modified.push(file.clone());
            }
        }

        // Record history
        self.criteria_history.push(update.criteria_passing);
    }

    /// Calculate estimated completion percentage.
    pub fn estimated_completion(&self) -> f32 {
        if self.criteria_total == 0 {
            return 0.0;
        }
        self.criteria_passing as f32 / self.criteria_total as f32
    }

    /// Calculate rate of progress (criteria fixed per iteration).
    pub fn progress_rate(&self) -> f32 {
        if self.criteria_history.len() < 2 {
            return 0.0;
        }

        let recent: Vec<_> = self.criteria_history.iter().rev().take(3).collect();
        if recent.len() < 2 {
            return 0.0;
        }

        // Calculate average improvement
        let mut total_improvement = 0i32;
        for i in 0..recent.len() - 1 {
            total_improvement += *recent[i] as i32 - *recent[i + 1] as i32;
        }

        total_improvement as f32 / (recent.len() - 1) as f32
    }
}

impl Default for ProgressTracker {
    fn default() -> Self {
        Self::new()
    }
}

/// Update to apply to progress tracker after an iteration.
#[derive(Debug, Clone)]
pub struct ProgressUpdate {
    /// Iteration number.
    pub iteration: u32,

    /// Number of criteria now passing.
    pub criteria_passing: u32,

    /// Total criteria count.
    pub criteria_total: u32,

    /// New findings in this iteration.
    pub new_findings: u32,

    /// Files modified in this iteration.
    pub files_modified: Vec<String>,

    /// Whether work was completed.
    pub work_complete: bool,
}

// ============================================================================
// Stall Detection
// ============================================================================

/// Detects when the task is stalled (not making progress).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StallDetector {
    /// Configuration for stall detection.
    pub config: StallConfig,

    /// Iterations without improvement.
    pub iterations_without_progress: u32,

    /// Last criteria passing count.
    pub last_criteria_passing: u32,

    /// Same error seen consecutively.
    pub repeated_error_count: u32,

    /// Last error signature (for deduplication).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error_signature: Option<String>,

    /// Whether we've detected a stall.
    pub is_stalled: bool,

    /// Reason for stall if detected.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detected_reason: Option<String>,
}

impl StallDetector {
    /// Create a new stall detector with default config.
    pub fn new() -> Self {
        Self {
            config: StallConfig::default(),
            iterations_without_progress: 0,
            last_criteria_passing: 0,
            repeated_error_count: 0,
            last_error_signature: None,
            is_stalled: false,
            detected_reason: None,
        }
    }

    /// Create with custom config.
    pub fn with_config(config: StallConfig) -> Self {
        Self {
            config,
            ..Self::new()
        }
    }

    /// Record an iteration and check for stall.
    pub fn record_iteration(&mut self, update: &ProgressUpdate) {
        // Check for progress
        if update.criteria_passing > self.last_criteria_passing {
            // Progress made - reset counters
            self.iterations_without_progress = 0;
            self.repeated_error_count = 0;
            self.last_error_signature = None;
            self.is_stalled = false;
            self.detected_reason = None;
        } else if update.criteria_passing == self.last_criteria_passing {
            // No progress
            self.iterations_without_progress += 1;

            // Check if we've hit the threshold
            if self.iterations_without_progress >= self.config.no_progress_threshold {
                self.is_stalled = true;
                self.detected_reason = Some(format!(
                    "No progress for {} iterations (criteria stuck at {}/{})",
                    self.iterations_without_progress,
                    update.criteria_passing,
                    update.criteria_total
                ));
                warn!("Stall detected: {:?}", self.detected_reason);
            }
        } else {
            // Regression - also concerning
            self.iterations_without_progress += 1;
        }

        self.last_criteria_passing = update.criteria_passing;
    }

    /// Record a repeated error.
    pub fn record_error(&mut self, error_signature: &str) {
        if Some(error_signature.to_string()) == self.last_error_signature {
            self.repeated_error_count += 1;

            if self.repeated_error_count >= self.config.repeated_error_threshold {
                self.is_stalled = true;
                self.detected_reason = Some(format!(
                    "Same error repeated {} times: {}",
                    self.repeated_error_count, error_signature
                ));
                warn!("Stall detected: {:?}", self.detected_reason);
            }
        } else {
            self.repeated_error_count = 1;
            self.last_error_signature = Some(error_signature.to_string());
        }
    }

    /// Check if currently stalled.
    pub fn is_stalled(&self) -> bool {
        self.is_stalled
    }

    /// Get the stall reason if stalled.
    pub fn stall_reason(&self) -> Option<String> {
        if self.is_stalled {
            self.detected_reason.clone()
        } else {
            None
        }
    }

    /// Check if there are warning signs (not stalled yet but concerning).
    pub fn warning_signs(&self) -> bool {
        self.iterations_without_progress >= self.config.no_progress_threshold / 2
            || self.repeated_error_count >= self.config.repeated_error_threshold / 2
    }

    /// Reset the stall detector.
    pub fn reset(&mut self) {
        self.iterations_without_progress = 0;
        self.repeated_error_count = 0;
        self.last_error_signature = None;
        self.is_stalled = false;
        self.detected_reason = None;
    }
}

impl Default for StallDetector {
    fn default() -> Self {
        Self::new()
    }
}

/// Configuration for stall detection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StallConfig {
    /// Number of iterations without progress before declaring stall.
    pub no_progress_threshold: u32,

    /// Number of times the same error can repeat before declaring stall.
    pub repeated_error_threshold: u32,

    /// Whether to auto-trigger replan on stall.
    pub auto_replan_on_stall: bool,
}

impl Default for StallConfig {
    fn default() -> Self {
        Self {
            no_progress_threshold: 3,
            repeated_error_threshold: 2,
            auto_replan_on_stall: true,
        }
    }
}

// ============================================================================
// Ledger Actions
// ============================================================================

/// Actions that can be taken based on ledger state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LedgerAction {
    /// Continue with the current plan.
    Continue,

    /// Replan is recommended due to stall or new information.
    Replan { reason: String },

    /// Abort the task (unrecoverable).
    Abort { reason: String },

    /// Pause and wait for human input.
    PauseForHuman { prompt: String },
}

impl TaskLedger {
    /// Determine the recommended action based on current state.
    pub fn recommended_action(&self) -> LedgerAction {
        // Check for stall
        if self.is_stalled() {
            if self.stall_detector.config.auto_replan_on_stall {
                return LedgerAction::Replan {
                    reason: self
                        .stall_reason()
                        .unwrap_or_else(|| "Stall detected".to_string()),
                };
            } else {
                return LedgerAction::PauseForHuman {
                    prompt: format!(
                        "Task appears stalled: {}. Continue or abort?",
                        self.stall_reason()
                            .unwrap_or_else(|| "Unknown reason".to_string())
                    ),
                };
            }
        }

        // Check if max iterations reached
        if self.progress.current_iteration >= self.progress.max_iterations {
            return LedgerAction::PauseForHuman {
                prompt: format!(
                    "Reached {} iterations. Extend or abort?",
                    self.progress.max_iterations
                ),
            };
        }

        // Check for plan completion
        if self.plan.status == PlanStatus::Completed {
            return LedgerAction::Continue; // Will naturally complete
        }

        // Check for plan needing revision
        if self.plan.status == PlanStatus::NeedsRevision {
            return LedgerAction::Replan {
                reason: "Plan marked as needing revision".to_string(),
            };
        }

        LedgerAction::Continue
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ledger_creation() {
        let ledger = TaskLedger::new("Fix all type errors");
        assert_eq!(ledger.original_goal, "Fix all type errors");
        assert!(ledger.facts.is_empty());
        assert!(!ledger.is_stalled());
    }

    #[test]
    fn test_fact_recording() {
        let mut ledger = TaskLedger::new("Test task");

        ledger.record_fact(Fact::new("error_count", "5 type errors found", 1));
        assert_eq!(ledger.facts.len(), 1);

        // Update existing fact
        ledger.record_fact(Fact::new("error_count", "3 type errors remaining", 2));
        assert_eq!(ledger.facts.len(), 1);
        assert_eq!(ledger.facts[0].value, "3 type errors remaining");
    }

    #[test]
    fn test_stall_detection() {
        let mut ledger = TaskLedger::new("Test task");
        ledger.progress.criteria_total = 5;

        // No progress for multiple iterations
        for i in 1..=4 {
            ledger.update_progress(ProgressUpdate {
                iteration: i,
                criteria_passing: 2,
                criteria_total: 5,
                new_findings: 0,
                files_modified: vec![],
                work_complete: false,
            });
        }

        assert!(ledger.is_stalled());
        assert!(ledger.stall_reason().is_some());
    }

    #[test]
    fn test_progress_tracking() {
        let mut ledger = TaskLedger::new("Test task");

        ledger.update_progress(ProgressUpdate {
            iteration: 1,
            criteria_passing: 2,
            criteria_total: 5,
            new_findings: 1,
            files_modified: vec!["file1.ts".to_string()],
            work_complete: false,
        });

        assert_eq!(ledger.progress.current_iteration, 1);
        assert_eq!(ledger.progress.criteria_passing, 2);
        assert_eq!(ledger.progress.files_modified.len(), 1);
    }

    #[test]
    fn test_recommended_action_continue() {
        let ledger = TaskLedger::new("Test task");
        assert_eq!(ledger.recommended_action(), LedgerAction::Continue);
    }

    #[test]
    fn test_recommended_action_stall() {
        let mut ledger = TaskLedger::new("Test task");

        // Trigger stall
        for i in 1..=4 {
            ledger.update_progress(ProgressUpdate {
                iteration: i,
                criteria_passing: 0,
                criteria_total: 5,
                new_findings: 0,
                files_modified: vec![],
                work_complete: false,
            });
        }

        match ledger.recommended_action() {
            LedgerAction::Replan { reason } => {
                assert!(reason.contains("progress"));
            }
            _ => panic!("Expected Replan action"),
        }
    }

    #[test]
    fn test_plan_step_tracking() {
        let mut ledger = TaskLedger::new("Test task");

        ledger
            .plan
            .steps
            .push(PlanStep::new("step1", "Fix type errors"));
        ledger.plan.steps.push(PlanStep::new("step2", "Run tests"));

        ledger.complete_step("step1");

        assert_eq!(ledger.plan.steps[0].status, StepStatus::Completed);
        assert_eq!(ledger.plan.steps[1].status, StepStatus::Pending);
        assert_eq!(ledger.plan.completion_percentage(), 0.5);
    }

    #[test]
    fn test_worker_context_generation() {
        let mut ledger = TaskLedger::new("Fix authentication bug");

        ledger.record_fact(Fact::confirmed(
            "root_cause",
            "Missing null check in validateToken",
            1,
        ));
        ledger.progress.current_iteration = 2;
        ledger.progress.criteria_passing = 3;
        ledger.progress.criteria_total = 5;

        let context = ledger.build_worker_context();

        assert!(context.contains("Fix authentication bug"));
        assert!(context.contains("Missing null check"));
        assert!(context.contains("3/5"));
    }
}

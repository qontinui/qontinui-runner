//! Core types for the task orchestrator.
//!
//! These types define the structure of verification plans, success criteria,
//! and the data exchanged between agents.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};

/// A success criterion that must be met for task completion.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SuccessCriterion {
    /// Unique identifier for this criterion
    pub id: String,

    /// Human-readable description
    pub description: String,

    /// Type of verification required
    #[serde(rename = "type")]
    pub criterion_type: CriterionType,

    /// For deterministic criteria: the verification method
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verification_method: Option<VerificationMethod>,

    /// Configuration for the verification method
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verification_config: Option<serde_json::Value>,

    /// For AI-evaluated criteria: the evaluation prompt
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evaluation_prompt: Option<String>,

    /// Whether this criterion must pass for task completion
    #[serde(default = "default_true")]
    pub required: bool,

    /// Whether failure of this criterion blocks task completion.
    /// - true (default): Failure blocks completion, worker must fix
    /// - false: Failure is informational, doesn't block completion
    #[serde(default = "default_true")]
    pub is_critical: bool,

    /// Optional weight for partial success scoring
    #[serde(skip_serializing_if = "Option::is_none")]
    pub weight: Option<f64>,

    /// Domain this criterion belongs to (for multi-worker verification)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
}

fn default_true() -> bool {
    true
}

/// Type of verification for a criterion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CriterionType {
    /// Can be verified programmatically without AI
    Deterministic,
    /// Requires AI evaluation (e.g., screenshot review)
    AiEvaluated,
}

/// Methods for deterministic verification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationMethod {
    /// Build must succeed
    BuildSuccess,
    /// Unit tests must pass
    UnitTest,
    /// Integration tests must pass
    IntegrationTest,
    /// Playwright script must pass
    Playwright,
    /// Log pattern must match (or not match)
    LogPattern,
    /// GUI automation workflow must succeed
    GuiAutomation,
    /// Type check must pass
    TypeCheck,
    /// Lint check must pass
    LintCheck,
    /// Custom command must succeed
    CustomCommand,
}

/// The verification plan created by the planning agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationPlan {
    /// Summary of the goal
    pub goal_summary: String,

    /// All success criteria that must be verified
    pub success_criteria: Vec<SuccessCriterion>,

    /// Execution steps to run before verification (GUI automation)
    #[serde(default)]
    pub execution_steps: Vec<serde_json::Value>,

    /// Suggested number of worker agents
    #[serde(default = "default_worker_count")]
    pub suggested_worker_count: u32,

    /// Domain assignments for multiple workers
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worker_domains: Option<Vec<WorkerDomain>>,

    /// Plan version (incremented on replan)
    #[serde(default = "default_version")]
    pub version: u32,
}

fn default_worker_count() -> u32 {
    1
}

fn default_version() -> u32 {
    1
}

impl VerificationPlan {
    /// Check if this plan has any AI-evaluated criteria
    pub fn has_ai_criteria(&self) -> bool {
        self.success_criteria
            .iter()
            .any(|c| c.criterion_type == CriterionType::AiEvaluated)
    }

    /// Get all deterministic criteria
    pub fn deterministic_criteria(&self) -> Vec<&SuccessCriterion> {
        self.success_criteria
            .iter()
            .filter(|c| c.criterion_type == CriterionType::Deterministic)
            .collect()
    }

    /// Get all AI-evaluated criteria
    pub fn ai_criteria(&self) -> Vec<&SuccessCriterion> {
        self.success_criteria
            .iter()
            .filter(|c| c.criterion_type == CriterionType::AiEvaluated)
            .collect()
    }

    /// Get all criteria for a specific domain.
    pub fn criteria_for_domain(&self, domain_id: &str) -> Vec<&SuccessCriterion> {
        self.success_criteria
            .iter()
            .filter(|c| c.domain.as_deref() == Some(domain_id))
            .collect()
    }

    /// Get deterministic criteria for a specific domain.
    pub fn deterministic_criteria_for_domain(&self, domain_id: &str) -> Vec<&SuccessCriterion> {
        self.success_criteria
            .iter()
            .filter(|c| {
                c.criterion_type == CriterionType::Deterministic
                    && c.domain.as_deref() == Some(domain_id)
            })
            .collect()
    }

    /// Get AI criteria for a specific domain.
    pub fn ai_criteria_for_domain(&self, domain_id: &str) -> Vec<&SuccessCriterion> {
        self.success_criteria
            .iter()
            .filter(|c| {
                c.criterion_type == CriterionType::AiEvaluated
                    && c.domain.as_deref() == Some(domain_id)
            })
            .collect()
    }

    /// Get all unique domains in this plan.
    pub fn get_domains(&self) -> Vec<String> {
        let mut domains: Vec<String> = self
            .success_criteria
            .iter()
            .filter_map(|c| c.domain.clone())
            .collect();
        domains.sort();
        domains.dedup();
        domains
    }

    /// Check if this plan has multiple domains.
    pub fn is_multi_domain(&self) -> bool {
        self.get_domains().len() > 1
    }
}

/// Domain assignment for a worker agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerDomain {
    /// Worker identifier
    pub worker_id: String,

    /// Specialization type
    pub specialization: Option<String>,

    /// Files/paths this worker owns
    pub file_patterns: Vec<String>,

    /// Additional system prompt for this worker
    pub system_prompt_additions: Option<String>,
}

// ============================================================================
// Multi-Worker Support Types (Phase 5)
// ============================================================================

/// Configuration for a domain that workers can be assigned to.
///
/// Domains represent logical areas of responsibility within a project
/// (e.g., "frontend", "backend", "database", "api").
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DomainAssignment {
    /// Unique identifier for this domain
    pub domain_id: String,

    /// Human-readable name for the domain
    pub name: String,

    /// Description of what this domain covers
    pub description: String,

    /// File patterns that belong to this domain (e.g., "src/frontend/**/*.ts")
    #[serde(default)]
    pub file_patterns: Vec<String>,

    /// Keywords that help identify this domain
    #[serde(default)]
    pub keywords: Vec<String>,

    /// Workers currently assigned to this domain
    #[serde(default)]
    pub assigned_workers: Vec<String>,

    /// Success criteria IDs that are specific to this domain
    #[serde(default)]
    pub domain_criteria: Vec<String>,

    /// Additional system prompt context for workers in this domain
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_prompt_context: Option<String>,
}

impl DomainAssignment {
    /// Create a new domain assignment.
    pub fn new(domain_id: &str, name: &str, description: &str) -> Self {
        Self {
            domain_id: domain_id.to_string(),
            name: name.to_string(),
            description: description.to_string(),
            file_patterns: Vec::new(),
            keywords: Vec::new(),
            assigned_workers: Vec::new(),
            domain_criteria: Vec::new(),
            system_prompt_context: None,
        }
    }

    /// Add a file pattern to this domain.
    pub fn with_file_pattern(mut self, pattern: &str) -> Self {
        self.file_patterns.push(pattern.to_string());
        self
    }

    /// Add a keyword to this domain.
    pub fn with_keyword(mut self, keyword: &str) -> Self {
        self.keywords.push(keyword.to_string());
        self
    }

    /// Add system prompt context for this domain.
    pub fn with_system_prompt(mut self, prompt: &str) -> Self {
        self.system_prompt_context = Some(prompt.to_string());
        self
    }

    /// Check if a file path belongs to this domain.
    pub fn matches_file(&self, file_path: &str) -> bool {
        for pattern in &self.file_patterns {
            if glob_match::glob_match(pattern, file_path) {
                return true;
            }
        }
        false
    }

    /// Assign a worker to this domain.
    pub fn assign_worker(&mut self, worker_id: &str) {
        if !self.assigned_workers.contains(&worker_id.to_string()) {
            self.assigned_workers.push(worker_id.to_string());
        }
    }

    /// Remove a worker from this domain.
    pub fn unassign_worker(&mut self, worker_id: &str) {
        self.assigned_workers.retain(|id| id != worker_id);
    }
}

/// Current state of an individual worker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerStatus {
    /// Worker is idle, waiting for assignment
    Idle,
    /// Worker is actively executing
    Active,
    /// Worker signaled work complete, awaiting verification
    AwaitingVerification,
    /// Worker is paused
    Paused,
    /// Worker has completed its work
    Completed,
    /// Worker encountered an error
    Error,
}

/// Instance tracking for an individual worker.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerInstance {
    /// Unique identifier for this worker
    pub worker_id: String,

    /// Human-readable name
    pub name: String,

    /// Current status
    pub status: WorkerStatus,

    /// Domain this worker is assigned to (if any)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,

    /// Current iteration for this worker
    pub iteration: u32,

    /// Maximum iterations for this worker
    pub max_iterations: u32,

    /// Last signal received from this worker
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_signal: Option<WorkerSignal>,

    /// Findings recorded by this worker
    #[serde(default)]
    pub findings: Vec<Finding>,

    /// Files this worker has touched
    #[serde(default)]
    pub touched_files: Vec<String>,

    /// Timestamp when worker started (ISO 8601)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,

    /// Timestamp when worker completed (ISO 8601)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,

    /// Error message if worker is in error state
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
}

impl WorkerInstance {
    /// Create a new worker instance.
    pub fn new(worker_id: &str, name: &str, max_iterations: u32) -> Self {
        Self {
            worker_id: worker_id.to_string(),
            name: name.to_string(),
            status: WorkerStatus::Idle,
            domain: None,
            iteration: 0,
            max_iterations,
            last_signal: None,
            findings: Vec::new(),
            touched_files: Vec::new(),
            started_at: None,
            completed_at: None,
            error_message: None,
        }
    }

    /// Start this worker.
    pub fn start(&mut self) {
        self.status = WorkerStatus::Active;
        self.started_at = Some(chrono::Utc::now().to_rfc3339());
    }

    /// Mark this worker as awaiting verification.
    pub fn await_verification(&mut self) {
        self.status = WorkerStatus::AwaitingVerification;
    }

    /// Mark this worker as completed.
    pub fn complete(&mut self) {
        self.status = WorkerStatus::Completed;
        self.completed_at = Some(chrono::Utc::now().to_rfc3339());
    }

    /// Mark this worker as errored.
    pub fn error(&mut self, message: &str) {
        self.status = WorkerStatus::Error;
        self.error_message = Some(message.to_string());
        self.completed_at = Some(chrono::Utc::now().to_rfc3339());
    }

    /// Assign this worker to a domain.
    pub fn assign_to_domain(&mut self, domain_id: &str) {
        self.domain = Some(domain_id.to_string());
    }

    /// Record a finding from this worker.
    pub fn record_finding(&mut self, finding: Finding) {
        self.findings.push(finding);
    }

    /// Record a file that this worker touched.
    pub fn record_touched_file(&mut self, file_path: &str) {
        if !self.touched_files.contains(&file_path.to_string()) {
            self.touched_files.push(file_path.to_string());
        }
    }

    /// Increment iteration and check if max reached.
    pub fn increment_iteration(&mut self) -> bool {
        self.iteration += 1;
        self.iteration >= self.max_iterations
    }

    /// Check if this worker is still active.
    pub fn is_active(&self) -> bool {
        matches!(
            self.status,
            WorkerStatus::Active | WorkerStatus::AwaitingVerification
        )
    }
}

/// Coordination message between workers.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum WorkerCoordinationMessage {
    /// Worker completed work on specific files
    #[serde(rename = "files_modified")]
    FilesModified {
        worker_id: String,
        files: Vec<String>,
    },

    /// Worker found an issue that may affect other workers
    #[serde(rename = "shared_finding")]
    SharedFinding { worker_id: String, finding: Finding },

    /// Worker is blocked waiting for another worker
    #[serde(rename = "blocked")]
    Blocked {
        worker_id: String,
        waiting_for: String,
        reason: String,
    },

    /// Worker is ready for verification
    #[serde(rename = "ready_for_verification")]
    ReadyForVerification {
        worker_id: String,
        domain: Option<String>,
    },

    /// Coordination signal to synchronize workers
    #[serde(rename = "sync_point")]
    SyncPoint {
        worker_ids: Vec<String>,
        reason: String,
    },
}

/// Result of domain-scoped verification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DomainVerificationResult {
    /// Domain that was verified
    pub domain_id: String,

    /// Workers that contributed to this domain
    pub worker_ids: Vec<String>,

    /// Verification results for domain-specific criteria
    pub results: Vec<VerificationResult>,

    /// Whether all domain criteria passed
    pub all_passed: bool,

    /// Summary of any failures
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_summary: Option<String>,
}

/// Signal emitted by a worker agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "signal", content = "data")]
pub enum WorkerSignal {
    /// Worker believes the work is complete, ready for verification
    #[serde(rename = "work_complete")]
    WorkComplete { reason: Option<String> },

    /// Worker needs the plan to be revised
    #[serde(rename = "need_replan")]
    NeedReplan { reason: String },

    /// Worker is continuing work (default)
    #[serde(rename = "continue")]
    Continue,

    /// Worker has recorded a finding
    #[serde(rename = "finding")]
    Finding(Finding),
}

impl WorkerSignal {
    /// Parse a worker signal from AI output text.
    pub fn parse_from_output(output: &str) -> Option<Self> {
        // Check for [WORK_COMPLETE] marker
        if output.contains("[WORK_COMPLETE]") {
            // Extract reason if present
            let reason = if let Some(start) = output.find("[WORK_COMPLETE]") {
                let after = &output[start + 15..];
                let reason_text = after.lines().next().unwrap_or("").trim();
                if reason_text.is_empty() {
                    None
                } else {
                    Some(reason_text.to_string())
                }
            } else {
                None
            };
            return Some(WorkerSignal::WorkComplete { reason });
        }

        // Check for [NEED_REPLAN] marker
        if output.contains("[NEED_REPLAN]") {
            // Extract reason
            let reason = if let Some(start) = output.find("[NEED_REPLAN]") {
                let after = &output[start + 13..];
                // Take everything until next marker or end
                let end = after.find('[').unwrap_or(after.len());
                after[..end].trim().to_string()
            } else {
                "Plan revision requested".to_string()
            };
            return Some(WorkerSignal::NeedReplan { reason });
        }

        // Check for [FINDING:...] marker
        if let Some(start) = output.find("[FINDING:") {
            if let Some(end) = output[start..].find(']') {
                let finding_type = &output[start + 9..start + end];
                // Extract description (next line or content after marker)
                let after = &output[start + end + 1..];
                let description = after.lines().next().unwrap_or("").trim().to_string();

                return Some(WorkerSignal::Finding(Finding {
                    id: uuid::Uuid::new_v4().to_string(),
                    finding_type: finding_type.to_string(),
                    description,
                    evidence: None,
                    confidence: Confidence::Medium,
                    related_files: vec![],
                }));
            }
        }

        // Default: continue working
        None // Return None to indicate no explicit signal (implies Continue)
    }
}

/// A finding recorded by a worker agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    /// Unique identifier
    pub id: String,

    /// Type of finding
    pub finding_type: String, // "bug", "root_cause", "observation", "hypothesis", "solution"

    /// Description of the finding
    pub description: String,

    /// Supporting evidence (file paths, log excerpts, etc.)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence: Option<String>,

    /// Confidence level
    pub confidence: Confidence,

    /// Related file paths
    #[serde(default)]
    pub related_files: Vec<String>,
}

/// Confidence level for findings and verification results.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Confidence {
    High,
    Medium,
    Low,
}

/// Result of a single verification check.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationResult {
    /// The criterion that was checked
    pub criterion_id: String,

    /// Whether the check passed
    pub passed: bool,

    /// Type of verification performed
    pub criterion_type: CriterionType,

    /// Confidence level (for AI verification)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<Confidence>,

    /// What was observed
    #[serde(default)]
    pub observations: Vec<String>,

    /// Issues found (if failed)
    #[serde(default)]
    pub issues: Vec<String>,

    /// Suggestions for fixing (if failed)
    #[serde(default)]
    pub suggestions: Vec<String>,

    /// Raw output/details
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_output: Option<String>,
}

/// Aggregated verification results for an iteration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IterationVerificationResults {
    /// Iteration number
    pub iteration: u32,

    /// Deterministic verification results
    pub deterministic_results: Vec<VerificationResult>,

    /// AI verification results (empty if skipped)
    pub ai_results: Vec<VerificationResult>,

    /// Whether all required deterministic checks passed
    pub deterministic_passed: bool,

    /// Whether all required AI checks passed (true if no AI criteria)
    pub ai_passed: bool,

    /// Overall pass/fail
    pub all_passed: bool,

    /// Human-readable summary of failures
    pub failure_summary: Option<String>,

    /// Criterion overrides applied in this iteration
    #[serde(default)]
    pub applied_overrides: Vec<CriterionOverride>,

    /// Criteria that failed but were accepted due to overrides
    #[serde(default)]
    pub overridden_criteria: Vec<String>,
}

impl IterationVerificationResults {
    /// Create results indicating all verification passed.
    pub fn all_passed(iteration: u32) -> Self {
        Self {
            iteration,
            deterministic_results: vec![],
            ai_results: vec![],
            deterministic_passed: true,
            ai_passed: true,
            all_passed: true,
            failure_summary: None,
            applied_overrides: vec![],
            overridden_criteria: vec![],
        }
    }

    /// Apply overrides to the verification results.
    ///
    /// This updates the pass/fail status based on any overrides that cover
    /// failing criteria. A criterion with an override is treated as "passed
    /// with justification".
    pub fn apply_overrides(&mut self, overrides: &OverrideCollection) {
        let mut newly_overridden = Vec::new();

        // Check deterministic results
        for result in &self.deterministic_results {
            if !result.passed && overrides.has_override(&result.criterion_id) {
                newly_overridden.push(result.criterion_id.clone());
                // Store the override details
                for override_ in overrides.get_overrides(&result.criterion_id) {
                    self.applied_overrides.push(override_.clone());
                }
            }
        }

        // Check AI results
        for result in &self.ai_results {
            if !result.passed && overrides.has_override(&result.criterion_id) {
                newly_overridden.push(result.criterion_id.clone());
                for override_ in overrides.get_overrides(&result.criterion_id) {
                    self.applied_overrides.push(override_.clone());
                }
            }
        }

        self.overridden_criteria = newly_overridden;

        // Recalculate pass status considering overrides
        self.recalculate_pass_status();
    }

    /// Recalculate pass/fail status considering overrides.
    fn recalculate_pass_status(&mut self) {
        // Deterministic: passed if all passed OR all failures are overridden
        self.deterministic_passed = self
            .deterministic_results
            .iter()
            .all(|r| r.passed || self.overridden_criteria.contains(&r.criterion_id));

        // AI: passed if all passed OR all failures are overridden
        self.ai_passed = self
            .ai_results
            .iter()
            .all(|r| r.passed || self.overridden_criteria.contains(&r.criterion_id));

        self.all_passed = self.deterministic_passed && self.ai_passed;

        // Update failure summary if we now pass due to overrides
        if self.all_passed && !self.overridden_criteria.is_empty() {
            let override_note = format!(
                "\n\n**Note:** {} criteria passed via override:\n{}",
                self.overridden_criteria.len(),
                self.applied_overrides
                    .iter()
                    .map(|o| format!("- {}: {} ({})", o.criterion_id, o.item, o.justification))
                    .collect::<Vec<_>>()
                    .join("\n")
            );
            if let Some(ref mut summary) = self.failure_summary {
                summary.push_str(&override_note);
            } else {
                self.failure_summary = Some(override_note);
            }
        }
    }

    /// Check if a specific criterion was overridden.
    pub fn is_overridden(&self, criterion_id: &str) -> bool {
        self.overridden_criteria.contains(&criterion_id.to_string())
    }

    /// Get the override justification for a criterion.
    pub fn get_override_justification(&self, criterion_id: &str) -> Option<String> {
        self.applied_overrides
            .iter()
            .filter(|o| o.criterion_id == criterion_id)
            .map(|o| format!("{}: {}", o.item, o.justification))
            .collect::<Vec<_>>()
            .first()
            .cloned()
    }

    /// Build a feedback string for failed verification.
    pub fn build_feedback(&self) -> String {
        let mut feedback = String::new();

        // Show overridden criteria first (if any)
        if !self.overridden_criteria.is_empty() {
            feedback.push_str("## Criteria Accepted via Override\n\n");
            for override_ in &self.applied_overrides {
                feedback.push_str(&format!(
                    "### ✓ {} (Override)\n- **Item:** {}\n- **Justification:** {}\n\n",
                    override_.criterion_id, override_.item, override_.justification
                ));
            }
        }

        if !self.deterministic_passed {
            feedback.push_str("## Deterministic Verification Failed\n\n");
            for result in &self.deterministic_results {
                if !result.passed && !self.is_overridden(&result.criterion_id) {
                    feedback.push_str(&format!("### ❌ {}\n", result.criterion_id));
                    for issue in &result.issues {
                        feedback.push_str(&format!("- {}\n", issue));
                    }
                    if let Some(output) = &result.raw_output {
                        feedback.push_str(&format!("\n```\n{}\n```\n\n", output));
                    }
                }
            }
        }

        if !self.ai_passed {
            feedback.push_str("## AI Verification Failed\n\n");
            for result in &self.ai_results {
                if !result.passed && !self.is_overridden(&result.criterion_id) {
                    feedback.push_str(&format!("### ❌ {}\n", result.criterion_id));
                    feedback.push_str("**Observations:**\n");
                    for obs in &result.observations {
                        feedback.push_str(&format!("- {}\n", obs));
                    }
                    feedback.push_str("\n**Issues:**\n");
                    for issue in &result.issues {
                        feedback.push_str(&format!("- {}\n", issue));
                    }
                    if !result.suggestions.is_empty() {
                        feedback.push_str("\n**Suggestions:**\n");
                        for sug in &result.suggestions {
                            feedback.push_str(&format!("- {}\n", sug));
                        }
                    }
                    feedback.push('\n');
                }
            }
        }

        feedback
    }
}

/// Context passed to the verification agent (intentionally limited).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationAgentContext {
    /// The screenshot to evaluate
    pub screenshot_base64: String,

    /// The evaluation prompt from the criterion
    pub evaluation_prompt: String,

    /// Brief goal context (NOT work history)
    pub goal_context: String,
}

/// Request to extend iterations after max reached.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtendIterationsRequest {
    /// Additional iterations to add
    pub additional_iterations: u32,

    /// Optional guidance for the worker
    pub guidance: Option<String>,
}

// ============================================================================
// Criterion Override Types
// ============================================================================

/// An override for a verification criterion.
///
/// Workers can emit these to indicate that a failing criterion should be
/// accepted as-is with justification, rather than requiring a fix.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CriterionOverride {
    /// The criterion ID being overridden
    pub criterion_id: String,

    /// What specifically is being overridden (e.g., class name, file path)
    pub item: String,

    /// Justification for why this override is acceptable
    pub justification: String,

    /// Iteration when this override was recorded
    pub iteration: u32,

    /// Worker ID that provided the override (if multi-worker)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worker_id: Option<String>,

    /// Timestamp when override was recorded
    pub recorded_at: String,
}

impl CriterionOverride {
    /// Create a new criterion override.
    pub fn new(criterion_id: &str, item: &str, justification: &str, iteration: u32) -> Self {
        Self {
            criterion_id: criterion_id.to_string(),
            item: item.to_string(),
            justification: justification.to_string(),
            iteration,
            worker_id: None,
            recorded_at: chrono::Utc::now().to_rfc3339(),
        }
    }

    /// Set the worker ID for this override.
    pub fn with_worker_id(mut self, worker_id: &str) -> Self {
        self.worker_id = Some(worker_id.to_string());
        self
    }

    /// Parse criterion overrides from worker output text.
    ///
    /// Looks for markers in the format:
    /// ```text
    /// [CRITERION_OVERRIDE:criterion_id]
    /// Item: ClassName or file/path
    /// Justification: Why this is acceptable
    /// [/CRITERION_OVERRIDE]
    /// ```
    pub fn parse_from_output(output: &str, iteration: u32) -> Vec<Self> {
        let mut overrides = Vec::new();
        let mut remaining = output;

        while let Some(start) = remaining.find("[CRITERION_OVERRIDE:") {
            // Find the criterion ID
            let after_start = &remaining[start + 20..];
            if let Some(id_end) = after_start.find(']') {
                let criterion_id = after_start[..id_end].trim();

                // Find the closing marker
                let content_start = start + 20 + id_end + 1;
                if let Some(end) = remaining[content_start..].find("[/CRITERION_OVERRIDE]") {
                    let content = &remaining[content_start..content_start + end];

                    // Parse Item and Justification from content
                    let item = Self::extract_field(content, "Item")
                        .or_else(|| Self::extract_field(content, "Class"))
                        .or_else(|| Self::extract_field(content, "File"))
                        .unwrap_or_else(|| "unspecified".to_string());

                    let justification = Self::extract_field(content, "Justification")
                        .or_else(|| Self::extract_field(content, "Reason"))
                        .unwrap_or_else(|| content.trim().to_string());

                    overrides.push(CriterionOverride::new(
                        criterion_id,
                        &item,
                        &justification,
                        iteration,
                    ));

                    remaining = &remaining[content_start + end + 21..];
                } else {
                    // No closing marker, skip this one
                    remaining = &remaining[start + 20..];
                }
            } else {
                // Malformed, skip
                remaining = &remaining[start + 20..];
            }
        }

        overrides
    }

    /// Extract a field value from content (e.g., "Item: value")
    fn extract_field(content: &str, field_name: &str) -> Option<String> {
        let pattern = format!("{}:", field_name);
        if let Some(start) = content.find(&pattern) {
            let after = &content[start + pattern.len()..];
            // Take until newline or end
            let value = after.lines().next().unwrap_or("").trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
        None
    }
}

/// Collection of overrides for a task run.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OverrideCollection {
    /// All recorded overrides
    pub overrides: Vec<CriterionOverride>,
}

impl OverrideCollection {
    /// Create a new empty override collection.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add an override to the collection.
    pub fn add(&mut self, override_: CriterionOverride) {
        self.overrides.push(override_);
    }

    /// Add multiple overrides to the collection.
    pub fn add_all(&mut self, overrides: Vec<CriterionOverride>) {
        self.overrides.extend(overrides);
    }

    /// Check if a criterion has any overrides.
    pub fn has_override(&self, criterion_id: &str) -> bool {
        self.overrides
            .iter()
            .any(|o| o.criterion_id == criterion_id)
    }

    /// Get all overrides for a specific criterion.
    pub fn get_overrides(&self, criterion_id: &str) -> Vec<&CriterionOverride> {
        self.overrides
            .iter()
            .filter(|o| o.criterion_id == criterion_id)
            .collect()
    }

    /// Get the justification summary for a criterion (concatenated if multiple).
    pub fn get_justification_summary(&self, criterion_id: &str) -> Option<String> {
        let overrides = self.get_overrides(criterion_id);
        if overrides.is_empty() {
            return None;
        }

        let summaries: Vec<String> = overrides
            .iter()
            .map(|o| format!("- {}: {}", o.item, o.justification))
            .collect();

        Some(summaries.join("\n"))
    }

    /// Get all unique criterion IDs that have overrides.
    pub fn overridden_criteria(&self) -> Vec<String> {
        let mut ids: Vec<String> = self
            .overrides
            .iter()
            .map(|o| o.criterion_id.clone())
            .collect();
        ids.sort();
        ids.dedup();
        ids
    }
}

/// Task completion result.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status")]
pub enum TaskCompletionResult {
    /// Task completed successfully
    #[serde(rename = "success")]
    Success {
        iterations: u32,
        findings: Vec<Finding>,
        verification_results: IterationVerificationResults,
    },

    /// Task failed (max iterations or unrecoverable error)
    #[serde(rename = "failed")]
    Failed {
        reason: String,
        iterations: u32,
        last_results: Option<IterationVerificationResults>,
        findings: Vec<Finding>,
    },

    /// Task stopped by user or system
    #[serde(rename = "stopped")]
    Stopped {
        at_iteration: u32,
        findings: Vec<Finding>,
        can_resume: bool,
    },

    /// Task paused at max iterations, awaiting user decision
    #[serde(rename = "paused")]
    Paused {
        at_iteration: u32,
        max_iterations: u32,
        last_results: Option<IterationVerificationResults>,
        findings: Vec<Finding>,
    },
}

//! Deferred human-in-the-loop feedback system.
//!
//! When workflows run autonomously (the default), approval gates are non-blocking.
//! Instead of pausing execution for human review, the system:
//!
//! 1. Records the question it WOULD have asked
//! 2. Records the decision it MADE (and why)
//! 3. Emits the question as a visible event (so a watching user sees it)
//! 4. Continues execution immediately
//! 5. Tags downstream iterations as contingent on this decision
//!
//! Users can review deferred questions post-run (or mid-run if watching) and
//! reject decisions, triggering targeted rework via git compensation rollback.
//!
//! ## Confidence Gate
//!
//! Not every approval point generates a deferred question. The system computes
//! a confidence score from meta-optimizer metrics and iteration history:
//!
//! - High confidence + low risk → silently auto-approved (no question)
//! - Low confidence or high risk → deferred question emitted
//! - Irreversible actions → always emit a question regardless of confidence
//!
//! Over time, as the system learns which workflows succeed, confidence rises
//! and fewer questions are emitted.

use serde::{Deserialize, Serialize};

use super::approval::ApprovalContext;

// =============================================================================
// Core Types
// =============================================================================

/// A question that was deferred during autonomous execution.
///
/// The system asked this question (visibly, in case a user is watching)
/// but continued execution without waiting for a response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeferredQuestion {
    /// Unique identifier.
    pub id: String,
    /// Task run this question belongs to.
    pub execution_id: String,
    /// Iteration when the question was raised.
    pub iteration: u32,
    /// The question/prompt that would have been shown in blocking mode.
    pub question: String,
    /// Context about what the AI did (reuses existing approval context).
    pub context: ApprovalContext,
    /// What the system decided to do instead of waiting.
    pub auto_decision: AutoDecision,
    /// Confidence score (0.0-1.0) at the time of the decision.
    pub confidence: f64,
    /// Risk classification of the action.
    pub risk_level: RiskLevel,
    /// Review status.
    pub status: DeferredStatus,
    /// Git commit hash before this decision (for potential rollback).
    pub git_checkpoint: Option<String>,
    /// Iterations that executed after this decision (populated post-hoc).
    #[serde(default)]
    pub contingent_iterations: Vec<u32>,
    /// When the question was created (RFC3339).
    pub created_at: String,
    /// When a human reviewed this question (RFC3339).
    pub reviewed_at: Option<String>,
    /// Comment from the reviewer.
    pub reviewer_comment: Option<String>,
}

impl DeferredQuestion {
    /// Create a new deferred question.
    pub fn new(
        execution_id: impl Into<String>,
        iteration: u32,
        question: impl Into<String>,
        context: ApprovalContext,
        auto_decision: AutoDecision,
        confidence: f64,
        risk_level: RiskLevel,
        git_checkpoint: Option<String>,
    ) -> Self {
        Self {
            id: format!("dq-{}", uuid::Uuid::new_v4()),
            execution_id: execution_id.into(),
            iteration,
            question: question.into(),
            context,
            auto_decision,
            confidence,
            risk_level,
            status: DeferredStatus::Pending,
            git_checkpoint,
            contingent_iterations: Vec::new(),
            created_at: chrono::Utc::now().to_rfc3339(),
            reviewed_at: None,
            reviewer_comment: None,
        }
    }

    /// Format the question for display in terminal output.
    pub fn display_block(&self) -> String {
        let risk_badge = match self.risk_level {
            RiskLevel::Low => "LOW",
            RiskLevel::Medium => "MEDIUM",
            RiskLevel::High => "HIGH",
            RiskLevel::Irreversible => "IRREVERSIBLE",
        };
        let decision_text = match &self.auto_decision {
            AutoDecision::Proceeded => "Continued as planned".to_string(),
            AutoDecision::BestGuess { chosen, reasoning } => {
                format!("Chose: {} ({})", chosen, reasoning)
            }
        };
        format!(
            "\n=== DEFERRED QUESTION (Iteration {}) ===\n\
             Risk: {} | Confidence: {:.0}%\n\
             \n\
             {}\n\
             \n\
             Auto-decision: {}\n\
             This decision can be reviewed post-run or responded to now.\n\
             Question ID: {}\n\
             ================================================\n",
            self.iteration,
            risk_badge,
            self.confidence * 100.0,
            self.question,
            decision_text,
            self.id,
        )
    }
}

/// What the system decided to do at a deferred question point.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AutoDecision {
    /// Continued with the planned action (equivalent to "approve").
    Proceeded,
    /// Made a specific choice among alternatives.
    BestGuess {
        /// The option that was chosen.
        chosen: String,
        /// Why this option was selected.
        reasoning: String,
    },
}

/// Risk classification for a deferred decision point.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskLevel {
    /// Reversible code changes only.
    Low,
    /// Config changes, test modifications.
    Medium,
    /// External API calls, data writes.
    High,
    /// Schema migrations, destructive UI actions, production deploys.
    Irreversible,
}

impl RiskLevel {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Irreversible => "irreversible",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s {
            "medium" => Self::Medium,
            "high" => Self::High,
            "irreversible" => Self::Irreversible,
            _ => Self::Low,
        }
    }
}

/// Review status of a deferred question.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeferredStatus {
    /// Awaiting human review.
    Pending,
    /// User confirmed the auto-decision was correct.
    Approved,
    /// User disagrees — rework is needed.
    Rejected,
    /// Review window expired — treated as approved.
    Expired,
}

impl DeferredStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Approved => "approved",
            Self::Rejected => "rejected",
            Self::Expired => "expired",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s {
            "approved" => Self::Approved,
            "rejected" => Self::Rejected,
            "expired" => Self::Expired,
            _ => Self::Pending,
        }
    }
}

// =============================================================================
// Confidence Computation
// =============================================================================

/// Compute a confidence score for the current decision point.
///
/// Sources:
/// - Recent iteration pass/fail rate
/// - Whether convergence is healthy
///
/// Returns 0.0 (no confidence, always ask) to 1.0 (full confidence, auto-approve).
/// First-run workflows default to 0.0 (conservative).
pub fn compute_decision_confidence(
    iteration_results: &[super::types::IterationResult],
    convergence_is_healthy: bool,
) -> f64 {
    if iteration_results.is_empty() {
        // First iteration, no history — be conservative
        return 0.0;
    }

    // Base: pass rate of recent iterations (last 5)
    let recent = &iteration_results[iteration_results.len().saturating_sub(5)..];
    let pass_count = recent.iter().filter(|r| r.verification_passed).count();
    let pass_rate = pass_count as f64 / recent.len() as f64;

    // Convergence health bonus/penalty
    let convergence_modifier = if convergence_is_healthy { 0.1 } else { -0.2 };

    // More iterations = more data = slightly more confidence
    let data_bonus = (iteration_results.len() as f64 * 0.02).min(0.15);

    (pass_rate + convergence_modifier + data_bonus).clamp(0.0, 1.0)
}

// =============================================================================
// Risk Classification
// =============================================================================

/// Classify the risk level of the current decision point.
///
/// Inspects git diff content and AI output for signals of irreversible
/// or high-risk operations.
pub fn classify_risk(
    git_diff: Option<&str>,
    ai_output: Option<&str>,
    files_modified: &[String],
) -> RiskLevel {
    // Check for irreversible patterns in git diff
    if let Some(diff) = git_diff {
        let diff_lower = diff.to_lowercase();
        if diff_lower.contains("drop table")
            || diff_lower.contains("drop column")
            || diff_lower.contains("alter table")
            || diff_lower.contains("truncate")
            || diff_lower.contains("delete from")
        {
            return RiskLevel::Irreversible;
        }
    }

    // Check AI output for explicit approval gate marker
    if let Some(output) = ai_output {
        if output.contains("[APPROVAL_GATE]") {
            return RiskLevel::High;
        }
    }

    // Check file paths for sensitive files
    let has_sensitive_files = files_modified.iter().any(|f| {
        let f_lower = f.to_lowercase();
        f_lower.contains(".env")
            || f_lower.contains("dockerfile")
            || f_lower.contains("docker-compose")
            || (f_lower.ends_with(".yml") && (f_lower.contains("ci") || f_lower.contains("deploy")))
            || f_lower.contains("migration")
            || f_lower.contains("schema")
    });

    if has_sensitive_files {
        return RiskLevel::High;
    }

    // Check for config-level changes
    let has_config_changes = files_modified.iter().any(|f| {
        let f_lower = f.to_lowercase();
        f_lower.ends_with(".toml")
            || (f_lower.ends_with(".json") && !f_lower.contains("package-lock"))
            || f_lower.ends_with(".yaml")
            || f_lower.ends_with(".yml")
    });

    if has_config_changes {
        return RiskLevel::Medium;
    }

    RiskLevel::Low
}

/// Determine whether a deferred question should be emitted at this decision point.
///
/// Returns `true` if a question should be recorded (low confidence or high risk).
/// Returns `false` if the decision can be silently auto-approved.
pub fn should_emit_question(confidence: f64, risk_level: RiskLevel, threshold: f64) -> bool {
    // Irreversible actions always get a question
    if risk_level == RiskLevel::Irreversible {
        return true;
    }

    // High risk lowers the effective threshold
    let effective_threshold = match risk_level {
        RiskLevel::High => threshold * 0.7,
        RiskLevel::Medium => threshold * 0.85,
        RiskLevel::Low => threshold,
        RiskLevel::Irreversible => 0.0, // unreachable, handled above
    };

    confidence < effective_threshold
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_deferred_question_display_block() {
        let q = DeferredQuestion {
            id: "dq-test-123".to_string(),
            execution_id: "exec-1".to_string(),
            iteration: 3,
            question: "Should I refactor the auth module?".to_string(),
            context: ApprovalContext {
                summary: "Refactored auth".to_string(),
                files_modified: vec!["src/auth.rs".to_string()],
                git_diff_stat: None,
                git_diff: None,
            },
            auto_decision: AutoDecision::Proceeded,
            confidence: 0.45,
            risk_level: RiskLevel::Medium,
            status: DeferredStatus::Pending,
            git_checkpoint: Some("abc123".to_string()),
            contingent_iterations: vec![],
            created_at: "2026-04-01T00:00:00Z".to_string(),
            reviewed_at: None,
            reviewer_comment: None,
        };

        let block = q.display_block();
        assert!(block.contains("Iteration 3"));
        assert!(block.contains("MEDIUM"));
        assert!(block.contains("45%"));
        assert!(block.contains("Should I refactor the auth module?"));
        assert!(block.contains("dq-test-123"));
    }

    #[test]
    fn test_confidence_no_history() {
        let confidence = compute_decision_confidence(&[], true);
        assert_eq!(confidence, 0.0);
    }

    #[test]
    fn test_confidence_all_passing() {
        let results: Vec<_> = (1..=5)
            .map(|i| super::super::types::IterationResult {
                iteration: i,
                verification_passed: true,
                critical_failure: false,
                passed_checks: 5,
                failed_checks: 0,
                failure_context: String::new(),
                agentic_phase_ran: true,
                agentic_phase_success: Some(true),
                blame_json: None,
                contingent_on: Vec::new(),
            })
            .collect();

        let confidence = compute_decision_confidence(&results, true);
        // 1.0 pass_rate + 0.1 convergence + 0.10 data_bonus = 1.0 (clamped)
        assert!(confidence > 0.9);
    }

    #[test]
    fn test_confidence_mixed_results() {
        let results = vec![
            super::super::types::IterationResult {
                iteration: 1,
                verification_passed: true,
                critical_failure: false,
                passed_checks: 3,
                failed_checks: 2,
                failure_context: String::new(),
                agentic_phase_ran: true,
                agentic_phase_success: Some(true),
                blame_json: None,
                contingent_on: Vec::new(),
            },
            super::super::types::IterationResult {
                iteration: 2,
                verification_passed: false,
                critical_failure: false,
                passed_checks: 2,
                failed_checks: 3,
                failure_context: String::new(),
                agentic_phase_ran: true,
                agentic_phase_success: Some(false),
                blame_json: None,
                contingent_on: Vec::new(),
            },
        ];

        let confidence = compute_decision_confidence(&results, true);
        // 0.5 pass_rate + 0.1 convergence + 0.04 data_bonus = 0.64
        assert!(confidence > 0.5);
        assert!(confidence < 0.8);
    }

    #[test]
    fn test_confidence_unhealthy_convergence() {
        let results = vec![super::super::types::IterationResult {
            iteration: 1,
            verification_passed: true,
            critical_failure: false,
            passed_checks: 5,
            failed_checks: 0,
            failure_context: String::new(),
            agentic_phase_ran: true,
            agentic_phase_success: Some(true),
            blame_json: None,
            contingent_on: Vec::new(),
        }];

        let healthy = compute_decision_confidence(&results, true);
        let unhealthy = compute_decision_confidence(&results, false);
        assert!(healthy > unhealthy);
    }

    #[test]
    fn test_risk_irreversible_sql() {
        let risk = classify_risk(
            Some("+DROP TABLE users;"),
            None,
            &["migrations/001.sql".to_string()],
        );
        assert_eq!(risk, RiskLevel::Irreversible);
    }

    #[test]
    fn test_risk_high_sensitive_files() {
        let risk = classify_risk(None, None, &[".env.production".to_string()]);
        assert_eq!(risk, RiskLevel::High);
    }

    #[test]
    fn test_risk_high_approval_marker() {
        let risk = classify_risk(
            None,
            Some("I've completed the changes. [APPROVAL_GATE]"),
            &["src/main.rs".to_string()],
        );
        assert_eq!(risk, RiskLevel::High);
    }

    #[test]
    fn test_risk_medium_config() {
        let risk = classify_risk(None, None, &["config/settings.toml".to_string()]);
        assert_eq!(risk, RiskLevel::Medium);
    }

    #[test]
    fn test_risk_low_code() {
        let risk = classify_risk(None, None, &["src/lib.rs".to_string()]);
        assert_eq!(risk, RiskLevel::Low);
    }

    #[test]
    fn test_should_emit_irreversible_always() {
        assert!(should_emit_question(1.0, RiskLevel::Irreversible, 0.85));
    }

    #[test]
    fn test_should_emit_high_confidence_low_risk() {
        assert!(!should_emit_question(0.9, RiskLevel::Low, 0.85));
    }

    #[test]
    fn test_should_emit_low_confidence() {
        assert!(should_emit_question(0.3, RiskLevel::Low, 0.85));
    }

    #[test]
    fn test_should_emit_high_risk_moderate_confidence() {
        // High risk lowers threshold: 0.85 * 0.7 = 0.595
        // Confidence 0.5 < 0.595, so should emit
        assert!(should_emit_question(0.5, RiskLevel::High, 0.85));
    }

    #[test]
    fn test_auto_decision_serialization() {
        let proceeded = AutoDecision::Proceeded;
        let json = serde_json::to_string(&proceeded).unwrap();
        assert!(json.contains("proceeded"));

        let best_guess = AutoDecision::BestGuess {
            chosen: "Option A".to_string(),
            reasoning: "Highest test coverage".to_string(),
        };
        let json = serde_json::to_string(&best_guess).unwrap();
        assert!(json.contains("best_guess"));
        assert!(json.contains("Option A"));
    }

    #[test]
    fn test_risk_level_roundtrip() {
        for level in [
            RiskLevel::Low,
            RiskLevel::Medium,
            RiskLevel::High,
            RiskLevel::Irreversible,
        ] {
            assert_eq!(RiskLevel::from_str(level.as_str()), level);
        }
    }

    #[test]
    fn test_deferred_status_roundtrip() {
        for status in [
            DeferredStatus::Pending,
            DeferredStatus::Approved,
            DeferredStatus::Rejected,
            DeferredStatus::Expired,
        ] {
            assert_eq!(DeferredStatus::from_str(status.as_str()), status);
        }
    }
}

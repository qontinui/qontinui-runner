//! Interrupt/Resume Protocol
//!
//! Inspired by LangGraph's interrupt mechanism, this module provides first-class
//! human-in-the-loop support for the orchestrator:
//!
//! - **Explicit Interrupts**: Pause workflow at defined points
//! - **State Preservation**: Full state captured for resumption
//! - **Resume Handlers**: Type-safe resumption with user input
//! - **Audit Trail**: Track all interrupt/resume decisions
//!
//! # Design Goals
//!
//! 1. **Predictability**: Clear interrupt points, no surprise pauses
//! 2. **Resumability**: Can always pick up where we left off
//! 3. **Observability**: Full visibility into what triggered interrupts
//! 4. **Flexibility**: Multiple interrupt types for different scenarios
//!
//! # Usage
//!
//! ```rust,ignore
//! // Trigger an interrupt
//! let interrupt = Interrupt::human_approval("Deploy to production?")
//!     .with_options(vec!["Approve", "Reject", "Review changes"]);
//!
//! interrupt_manager.raise(interrupt)?;
//!
//! // Later, resume with user input
//! interrupt_manager.resume(interrupt_id, ResumeValue::Selected("Approve"))?;
//! ```

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tracing::{info, warn};

// ============================================================================
// Interrupt Types
// ============================================================================

/// Reason for an interrupt.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InterruptReason {
    /// Requires human approval to continue.
    HumanApproval {
        /// Prompt to show the user.
        prompt: String,
        /// Available options (if any).
        #[serde(default)]
        options: Vec<String>,
        /// Default option index.
        #[serde(skip_serializing_if = "Option::is_none")]
        default_option: Option<usize>,
    },

    /// Requires an external resource.
    ResourceRequired {
        /// Description of the resource needed.
        resource: String,
        /// Instructions for providing the resource.
        #[serde(skip_serializing_if = "Option::is_none")]
        instructions: Option<String>,
    },

    /// Critical decision point.
    CriticalDecision {
        /// Context for the decision.
        context: String,
        /// Available choices.
        choices: Vec<DecisionChoice>,
    },

    /// Verification failed and needs human review.
    VerificationFailed {
        /// ID of the failing criterion.
        criterion_id: String,
        /// Details of the failure.
        failure_details: String,
        /// Possible actions.
        actions: Vec<FailureAction>,
    },

    /// Maximum iterations reached.
    MaxIterationsReached {
        /// Current iteration count.
        iterations: u32,
        /// Maximum allowed.
        max_iterations: u32,
        /// Summary of progress so far.
        progress_summary: String,
    },

    /// Stall detected.
    StallDetected {
        /// Description of the stall.
        reason: String,
        /// Iterations without progress.
        iterations_stuck: u32,
        /// Suggested actions.
        suggestions: Vec<String>,
    },

    /// Custom interrupt reason.
    Custom {
        /// Type identifier.
        interrupt_type: String,
        /// Custom data.
        data: serde_json::Value,
    },
}

/// A choice for a critical decision.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecisionChoice {
    /// Choice identifier.
    pub id: String,
    /// Display label.
    pub label: String,
    /// Description of what this choice means.
    pub description: String,
    /// Whether this is the recommended choice.
    #[serde(default)]
    pub recommended: bool,
}

impl DecisionChoice {
    /// Create a new decision choice.
    pub fn new(
        id: impl Into<String>,
        label: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            description: description.into(),
            recommended: false,
        }
    }

    /// Mark as recommended.
    pub fn recommended(mut self) -> Self {
        self.recommended = true;
        self
    }
}

/// Action for handling a verification failure.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureAction {
    /// Continue trying to fix.
    ContinueFix,
    /// Skip this criterion with justification.
    Skip { justification: String },
    /// Abort the task.
    Abort,
    /// Request manual intervention.
    ManualIntervention,
    /// Override the criterion.
    Override { justification: String },
}

// ============================================================================
// Interrupt
// ============================================================================

/// An interrupt that pauses workflow execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Interrupt {
    /// Unique identifier for this interrupt.
    pub id: String,

    /// Why the interrupt was raised.
    pub reason: InterruptReason,

    /// Snapshot of state when interrupted.
    pub state_snapshot: StateSnapshot,

    /// When the interrupt was raised (RFC3339).
    pub raised_at: String,

    /// Whether this interrupt has been resolved.
    pub resolved: bool,

    /// Resolution details (if resolved).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolution: Option<InterruptResolution>,

    /// Timeout for auto-resolution (seconds).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,

    /// Priority level.
    pub priority: InterruptPriority,

    /// Metadata.
    #[serde(default)]
    pub metadata: HashMap<String, serde_json::Value>,
}

impl Interrupt {
    /// Create a new interrupt.
    pub fn new(reason: InterruptReason) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            reason,
            state_snapshot: StateSnapshot::empty(),
            raised_at: chrono::Utc::now().to_rfc3339(),
            resolved: false,
            resolution: None,
            timeout_secs: None,
            priority: InterruptPriority::Normal,
            metadata: HashMap::new(),
        }
    }

    /// Create a human approval interrupt.
    pub fn human_approval(prompt: impl Into<String>) -> Self {
        Self::new(InterruptReason::HumanApproval {
            prompt: prompt.into(),
            options: Vec::new(),
            default_option: None,
        })
    }

    /// Create a critical decision interrupt.
    pub fn critical_decision(context: impl Into<String>, choices: Vec<DecisionChoice>) -> Self {
        Self::new(InterruptReason::CriticalDecision {
            context: context.into(),
            choices,
        })
    }

    /// Create a verification failed interrupt.
    pub fn verification_failed(
        criterion_id: impl Into<String>,
        failure_details: impl Into<String>,
    ) -> Self {
        Self::new(InterruptReason::VerificationFailed {
            criterion_id: criterion_id.into(),
            failure_details: failure_details.into(),
            actions: vec![
                FailureAction::ContinueFix,
                FailureAction::Skip {
                    justification: String::new(),
                },
                FailureAction::Abort,
            ],
        })
    }

    /// Create a max iterations interrupt.
    pub fn max_iterations(iterations: u32, max: u32, summary: impl Into<String>) -> Self {
        Self::new(InterruptReason::MaxIterationsReached {
            iterations,
            max_iterations: max,
            progress_summary: summary.into(),
        })
        .with_priority(InterruptPriority::High)
    }

    /// Create a stall detected interrupt.
    pub fn stall_detected(reason: impl Into<String>, iterations_stuck: u32) -> Self {
        Self::new(InterruptReason::StallDetected {
            reason: reason.into(),
            iterations_stuck,
            suggestions: vec![
                "Try a different approach".to_string(),
                "Request replan".to_string(),
                "Abort and start fresh".to_string(),
            ],
        })
    }

    /// Add options to a human approval interrupt.
    pub fn with_options(mut self, options: Vec<impl Into<String>>) -> Self {
        if let InterruptReason::HumanApproval {
            options: ref mut opts,
            ..
        } = self.reason
        {
            *opts = options.into_iter().map(|o| o.into()).collect();
        }
        self
    }

    /// Set the state snapshot.
    pub fn with_state(mut self, snapshot: StateSnapshot) -> Self {
        self.state_snapshot = snapshot;
        self
    }

    /// Set the timeout.
    pub fn with_timeout(mut self, secs: u64) -> Self {
        self.timeout_secs = Some(secs);
        self
    }

    /// Set the priority.
    pub fn with_priority(mut self, priority: InterruptPriority) -> Self {
        self.priority = priority;
        self
    }

    /// Add metadata.
    pub fn with_metadata(mut self, key: impl Into<String>, value: serde_json::Value) -> Self {
        self.metadata.insert(key.into(), value);
        self
    }

    /// Check if this interrupt has timed out.
    pub fn is_timed_out(&self) -> bool {
        if let Some(timeout) = self.timeout_secs {
            if let Ok(raised) = chrono::DateTime::parse_from_rfc3339(&self.raised_at) {
                let elapsed = chrono::Utc::now()
                    .signed_duration_since(raised)
                    .num_seconds() as u64;
                return elapsed >= timeout;
            }
        }
        false
    }

    /// Resolve this interrupt.
    pub fn resolve(&mut self, resolution: InterruptResolution) {
        self.resolved = true;
        self.resolution = Some(resolution);
    }

    /// Get the prompt for display.
    pub fn display_prompt(&self) -> String {
        match &self.reason {
            InterruptReason::HumanApproval {
                prompt, options, ..
            } => {
                let mut display = prompt.clone();
                if !options.is_empty() {
                    display.push_str("\n\nOptions:");
                    for (i, opt) in options.iter().enumerate() {
                        display.push_str(&format!("\n  {}. {}", i + 1, opt));
                    }
                }
                display
            }
            InterruptReason::ResourceRequired {
                resource,
                instructions,
            } => {
                let mut display = format!("Resource required: {}", resource);
                if let Some(instr) = instructions {
                    display.push_str(&format!("\n\nInstructions: {}", instr));
                }
                display
            }
            InterruptReason::CriticalDecision { context, choices } => {
                let mut display = format!("Decision required:\n{}\n\nChoices:", context);
                for choice in choices {
                    let rec = if choice.recommended {
                        " (recommended)"
                    } else {
                        ""
                    };
                    display.push_str(&format!(
                        "\n  - {}: {}{}",
                        choice.label, choice.description, rec
                    ));
                }
                display
            }
            InterruptReason::VerificationFailed {
                criterion_id,
                failure_details,
                ..
            } => {
                format!(
                    "Verification failed for '{}':\n{}\n\nHow would you like to proceed?",
                    criterion_id, failure_details
                )
            }
            InterruptReason::MaxIterationsReached {
                iterations,
                max_iterations,
                progress_summary,
            } => {
                format!(
                    "Maximum iterations ({}/{}) reached.\n\nProgress:\n{}\n\nExtend iterations or stop?",
                    iterations, max_iterations, progress_summary
                )
            }
            InterruptReason::StallDetected {
                reason,
                iterations_stuck,
                suggestions,
            } => {
                let mut display = format!(
                    "Stall detected: {} ({} iterations without progress)\n\nSuggestions:",
                    reason, iterations_stuck
                );
                for sug in suggestions {
                    display.push_str(&format!("\n  - {}", sug));
                }
                display
            }
            InterruptReason::Custom {
                interrupt_type,
                data,
            } => {
                format!("Custom interrupt ({}): {:?}", interrupt_type, data)
            }
        }
    }
}

/// Priority of an interrupt.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum InterruptPriority {
    /// Low priority, can wait.
    Low,
    /// Normal priority.
    #[default]
    Normal,
    /// High priority, should address soon.
    High,
    /// Critical, must address immediately.
    Critical,
}

// ============================================================================
// State Snapshot
// ============================================================================

/// Snapshot of orchestrator state when interrupted.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StateSnapshot {
    /// Task run ID.
    pub task_id: String,

    /// Current iteration.
    pub iteration: u32,

    /// Current phase.
    pub phase: String,

    /// Verification results from last iteration.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_verification: Option<serde_json::Value>,

    /// Current knowledge count.
    pub knowledge_count: usize,

    /// Files modified so far.
    #[serde(default)]
    pub files_modified: Vec<String>,

    /// Custom state data.
    #[serde(default)]
    pub custom: HashMap<String, serde_json::Value>,
}

impl StateSnapshot {
    /// Create an empty snapshot.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Create a snapshot with basic info.
    pub fn new(task_id: impl Into<String>, iteration: u32, phase: impl Into<String>) -> Self {
        Self {
            task_id: task_id.into(),
            iteration,
            phase: phase.into(),
            ..Default::default()
        }
    }

    /// Add verification results.
    pub fn with_verification(mut self, results: serde_json::Value) -> Self {
        self.last_verification = Some(results);
        self
    }

    /// Add files modified.
    pub fn with_files(mut self, files: Vec<String>) -> Self {
        self.files_modified = files;
        self
    }
}

// ============================================================================
// Resume Types
// ============================================================================

/// Resolution of an interrupt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InterruptResolution {
    /// How it was resolved.
    pub action: ResumeAction,

    /// Value provided by user (if any).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<ResumeValue>,

    /// Who resolved it.
    pub resolved_by: String,

    /// When it was resolved (RFC3339).
    pub resolved_at: String,

    /// Additional notes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

impl InterruptResolution {
    /// Create a resolution.
    pub fn new(action: ResumeAction, resolved_by: impl Into<String>) -> Self {
        Self {
            action,
            value: None,
            resolved_by: resolved_by.into(),
            resolved_at: chrono::Utc::now().to_rfc3339(),
            notes: None,
        }
    }

    /// Add a value.
    pub fn with_value(mut self, value: ResumeValue) -> Self {
        self.value = Some(value);
        self
    }

    /// Add notes.
    pub fn with_notes(mut self, notes: impl Into<String>) -> Self {
        self.notes = Some(notes.into());
        self
    }
}

/// Action taken to resume from an interrupt.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResumeAction {
    /// Continue with the provided input.
    Continue,
    /// Skip and continue.
    Skip,
    /// Abort the task.
    Abort,
    /// Retry the operation.
    Retry,
    /// Take a custom action.
    Custom(String),
}

/// Value provided when resuming.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResumeValue {
    /// Selected option by index.
    Selected { index: usize },

    /// Selected option by label.
    SelectedLabel { label: String },

    /// Text input.
    Text { value: String },

    /// Boolean decision.
    Boolean { value: bool },

    /// Number input.
    Number { value: f64 },

    /// JSON data.
    Json { value: serde_json::Value },

    /// Multiple selections.
    MultiSelect { indices: Vec<usize> },
}

impl ResumeValue {
    /// Create a selected value by index.
    pub fn selected(index: usize) -> Self {
        Self::Selected { index }
    }

    /// Create a selected value by label.
    pub fn selected_label(label: impl Into<String>) -> Self {
        Self::SelectedLabel {
            label: label.into(),
        }
    }

    /// Create a text value.
    pub fn text(value: impl Into<String>) -> Self {
        Self::Text {
            value: value.into(),
        }
    }

    /// Create a boolean value.
    pub fn boolean(value: bool) -> Self {
        Self::Boolean { value }
    }

    /// Create a number value.
    pub fn number(value: f64) -> Self {
        Self::Number { value }
    }

    /// Get as text if applicable.
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text { value } => Some(value),
            Self::SelectedLabel { label } => Some(label),
            _ => None,
        }
    }

    /// Get as index if applicable.
    pub fn as_index(&self) -> Option<usize> {
        match self {
            Self::Selected { index } => Some(*index),
            _ => None,
        }
    }

    /// Get as boolean if applicable.
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Boolean { value } => Some(*value),
            _ => None,
        }
    }
}

// ============================================================================
// Interrupt Manager
// ============================================================================

/// Manages interrupts for a task.
#[derive(Debug, Default)]
pub struct InterruptManager {
    /// Pending interrupts.
    pending: Vec<Interrupt>,

    /// Resolved interrupts (for audit trail).
    resolved: Vec<Interrupt>,

    /// Maximum pending interrupts before forcing resolution.
    max_pending: usize,
}

impl InterruptManager {
    /// Create a new interrupt manager.
    pub fn new() -> Self {
        Self {
            pending: Vec::new(),
            resolved: Vec::new(),
            max_pending: 10,
        }
    }

    /// Raise an interrupt.
    pub fn raise(&mut self, interrupt: Interrupt) -> Result<String, String> {
        if self.pending.len() >= self.max_pending {
            return Err(format!(
                "Too many pending interrupts ({})",
                self.max_pending
            ));
        }

        let id = interrupt.id.clone();
        info!(
            interrupt_id = %id,
            reason = ?interrupt.reason,
            "Interrupt raised"
        );
        self.pending.push(interrupt);
        Ok(id)
    }

    /// Resume from an interrupt.
    pub fn resume(
        &mut self,
        interrupt_id: &str,
        action: ResumeAction,
        value: Option<ResumeValue>,
        resolved_by: impl Into<String>,
    ) -> Result<Interrupt, String> {
        let idx = self
            .pending
            .iter()
            .position(|i| i.id == interrupt_id)
            .ok_or_else(|| format!("Interrupt {} not found", interrupt_id))?;

        let mut interrupt = self.pending.remove(idx);

        let mut resolution = InterruptResolution::new(action, resolved_by);
        if let Some(v) = value {
            resolution = resolution.with_value(v);
        }

        interrupt.resolve(resolution);
        info!(
            interrupt_id = %interrupt_id,
            "Interrupt resolved"
        );

        self.resolved.push(interrupt.clone());
        Ok(interrupt)
    }

    /// Get all pending interrupts.
    pub fn pending(&self) -> &[Interrupt] {
        &self.pending
    }

    /// Get the highest priority pending interrupt.
    pub fn highest_priority(&self) -> Option<&Interrupt> {
        self.pending.iter().max_by_key(|i| i.priority)
    }

    /// Check if there are any pending interrupts.
    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    /// Get resolved interrupts for audit.
    pub fn resolved(&self) -> &[Interrupt] {
        &self.resolved
    }

    /// Clear resolved interrupts.
    pub fn clear_resolved(&mut self) {
        self.resolved.clear();
    }

    /// Check for and remove timed-out interrupts.
    pub fn handle_timeouts(&mut self) -> Vec<Interrupt> {
        let (timed_out, remaining): (Vec<_>, Vec<_>) =
            self.pending.drain(..).partition(|i| i.is_timed_out());

        self.pending = remaining;

        for mut interrupt in timed_out.iter().cloned() {
            warn!(
                interrupt_id = %interrupt.id,
                "Interrupt timed out"
            );
            interrupt.resolve(InterruptResolution::new(
                ResumeAction::Skip,
                "system:timeout",
            ));
            self.resolved.push(interrupt);
        }

        timed_out
    }

    /// Get count of pending interrupts by priority.
    pub fn pending_by_priority(&self) -> HashMap<InterruptPriority, usize> {
        let mut counts = HashMap::new();
        for interrupt in &self.pending {
            *counts.entry(interrupt.priority).or_insert(0) += 1;
        }
        counts
    }
}

// ============================================================================
// Interrupt Handler Trait
// ============================================================================

/// Trait for handling interrupts.
pub trait InterruptHandler: Send + Sync {
    /// Handle an interrupt and return the resolution.
    fn handle(&self, interrupt: &Interrupt) -> Option<InterruptResolution>;

    /// Get the handler name.
    fn name(&self) -> &'static str;
}

/// Auto-approve handler for non-critical interrupts.
pub struct AutoApproveHandler {
    /// Priorities to auto-approve.
    pub auto_approve_priorities: Vec<InterruptPriority>,
}

impl AutoApproveHandler {
    /// Create a handler that auto-approves low priority interrupts.
    pub fn low_priority() -> Self {
        Self {
            auto_approve_priorities: vec![InterruptPriority::Low],
        }
    }
}

impl InterruptHandler for AutoApproveHandler {
    fn handle(&self, interrupt: &Interrupt) -> Option<InterruptResolution> {
        if self.auto_approve_priorities.contains(&interrupt.priority) {
            info!(
                interrupt_id = %interrupt.id,
                "Auto-approving low priority interrupt"
            );
            Some(
                InterruptResolution::new(ResumeAction::Continue, "system:auto_approve")
                    .with_notes("Auto-approved due to low priority"),
            )
        } else {
            None
        }
    }

    fn name(&self) -> &'static str {
        "auto_approve"
    }
}

/// Timeout handler that skips on timeout.
pub struct TimeoutHandler;

impl InterruptHandler for TimeoutHandler {
    fn handle(&self, interrupt: &Interrupt) -> Option<InterruptResolution> {
        if interrupt.is_timed_out() {
            Some(
                InterruptResolution::new(ResumeAction::Skip, "system:timeout")
                    .with_notes("Skipped due to timeout"),
            )
        } else {
            None
        }
    }

    fn name(&self) -> &'static str {
        "timeout"
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_interrupt_creation() {
        let interrupt =
            Interrupt::human_approval("Continue with deployment?").with_options(vec!["Yes", "No"]);

        assert!(!interrupt.resolved);
        assert!(matches!(
            interrupt.reason,
            InterruptReason::HumanApproval { .. }
        ));
    }

    #[test]
    fn test_interrupt_manager_raise_and_resume() {
        let mut manager = InterruptManager::new();

        let interrupt = Interrupt::human_approval("Test?");
        let id = manager.raise(interrupt).unwrap();

        assert!(manager.has_pending());
        assert_eq!(manager.pending().len(), 1);

        let resolved = manager
            .resume(
                &id,
                ResumeAction::Continue,
                Some(ResumeValue::boolean(true)),
                "test_user",
            )
            .unwrap();

        assert!(resolved.resolved);
        assert!(!manager.has_pending());
        assert_eq!(manager.resolved().len(), 1);
    }

    #[test]
    fn test_interrupt_priority() {
        let low = Interrupt::human_approval("Low").with_priority(InterruptPriority::Low);
        let high = Interrupt::human_approval("High").with_priority(InterruptPriority::High);

        let mut manager = InterruptManager::new();
        manager.raise(low).unwrap();
        manager.raise(high).unwrap();

        let highest = manager.highest_priority().unwrap();
        assert_eq!(highest.priority, InterruptPriority::High);
    }

    #[test]
    fn test_display_prompt() {
        let interrupt = Interrupt::human_approval("Deploy to production?")
            .with_options(vec!["Yes", "No", "Review"]);

        let prompt = interrupt.display_prompt();
        assert!(prompt.contains("Deploy to production"));
        assert!(prompt.contains("Yes"));
        assert!(prompt.contains("No"));
    }

    #[test]
    fn test_decision_choice() {
        let choices = vec![
            DecisionChoice::new("a", "Option A", "Do A").recommended(),
            DecisionChoice::new("b", "Option B", "Do B"),
        ];

        let interrupt = Interrupt::critical_decision("Which option?", choices);

        if let InterruptReason::CriticalDecision { choices, .. } = interrupt.reason {
            assert!(choices[0].recommended);
            assert!(!choices[1].recommended);
        }
    }

    #[test]
    fn test_resume_value() {
        let text = ResumeValue::text("hello");
        assert_eq!(text.as_text(), Some("hello"));

        let selected = ResumeValue::selected(1);
        assert_eq!(selected.as_index(), Some(1));

        let boolean = ResumeValue::boolean(true);
        assert_eq!(boolean.as_bool(), Some(true));
    }

    #[test]
    fn test_state_snapshot() {
        let snapshot = StateSnapshot::new("task-1", 5, "verification")
            .with_files(vec!["file1.ts".to_string()]);

        assert_eq!(snapshot.task_id, "task-1");
        assert_eq!(snapshot.iteration, 5);
        assert_eq!(snapshot.files_modified.len(), 1);
    }

    #[test]
    fn test_auto_approve_handler() {
        let handler = AutoApproveHandler::low_priority();

        let low = Interrupt::human_approval("Low").with_priority(InterruptPriority::Low);
        let high = Interrupt::human_approval("High").with_priority(InterruptPriority::High);

        assert!(handler.handle(&low).is_some());
        assert!(handler.handle(&high).is_none());
    }
}

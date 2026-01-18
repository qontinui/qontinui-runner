//! Execution Status Events Module
//!
//! Provides event emission for the agentic features to enable real-time
//! visibility in the frontend. Emits events for:
//! - Task routing decisions
//! - Retry attempts and status
//! - Memory compression operations
//! - Lifecycle hook executions

use serde::{Deserialize, Serialize};
use tauri::Emitter;
use tracing::{debug, info};

use super::compression::{CompressionResult, TokenCount};
use super::hooks::{HookResult, HookTrigger};
use super::retry::{RetryAttempt, RetryState};
use crate::ai_router::{ComplexityAssessment, TaskComplexity};

// ============================================================================
// Event Types
// ============================================================================

/// Base event fields
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventBase {
    /// Event type identifier
    #[serde(rename = "type")]
    pub event_type: String,
    /// Task run ID
    pub task_run_id: String,
    /// Unix timestamp in milliseconds
    pub timestamp: i64,
}

/// Routing decision event payload
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutingDecisionPayload {
    /// The assessed complexity level
    pub complexity: String,
    /// Confidence in the assessment (0-1)
    pub confidence: f32,
    /// Factors that contributed to this assessment
    pub factors: Vec<String>,
    /// The model selected
    pub selected_model: String,
    /// Task prompt preview (truncated)
    pub prompt_preview: Option<String>,
    /// File count if analyzed
    pub file_count: Option<usize>,
    /// Criteria count if analyzed
    pub criteria_count: Option<usize>,
}

/// Routing decision event
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutingDecisionEvent {
    #[serde(flatten)]
    pub base: EventBase,
    pub decision: RoutingDecisionPayload,
}

/// Retry attempt event payload
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryAttemptPayload {
    /// Attempt number (1-indexed)
    pub attempt_number: u32,
    /// Error that triggered this retry
    pub error: String,
    /// Timestamp of the attempt
    pub attempt_timestamp: String,
    /// Delay before this retry (ms)
    pub delay_ms: u64,
    /// Whether feedback was injected
    pub feedback_injected: bool,
}

/// Retry state payload
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryStatePayload {
    /// Current attempt number
    pub attempt: u32,
    /// Last error
    pub last_error: Option<String>,
    /// Last attempt timestamp
    pub last_attempt_at: Option<String>,
    /// Total delay accumulated (ms)
    pub total_delay_ms: u64,
    /// Error history
    pub error_history: Vec<RetryAttemptPayload>,
}

/// Retry attempt event
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryAttemptEvent {
    #[serde(flatten)]
    pub base: EventBase,
    pub attempt: RetryAttemptPayload,
    pub state: RetryStatePayload,
    pub exhausted: bool,
    pub next_retry_delay_ms: Option<u64>,
}

/// Token count payload
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenCountPayload {
    pub total: usize,
    pub findings: usize,
    pub observations: usize,
    pub feedback: usize,
    pub solutions: usize,
    pub other: usize,
    pub entry_count: usize,
}

/// Compression result payload
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompressionResultPayload {
    pub original_tokens: usize,
    pub compressed_tokens: usize,
    pub items_summarized: usize,
    pub summary_entries_created: usize,
    pub compressed_categories: Vec<String>,
    pub timestamp: String,
}

/// Compression event
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompressionEvent {
    #[serde(flatten)]
    pub base: EventBase,
    pub result: CompressionResultPayload,
    pub current_token_count: TokenCountPayload,
}

/// Token count update event
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenCountUpdateEvent {
    #[serde(flatten)]
    pub base: EventBase,
    pub token_count: TokenCountPayload,
    pub threshold_percentage: f32,
    pub compression_imminent: bool,
}

/// Hook execution result payload
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookExecutionPayload {
    pub hook_id: String,
    pub hook_name: String,
    pub trigger: String,
    pub success: bool,
    pub output: Option<String>,
    pub error: Option<String>,
    pub duration_ms: u64,
    pub timestamp: String,
}

/// Hook execution event
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookExecutionEvent {
    #[serde(flatten)]
    pub base: EventBase,
    pub result: HookExecutionPayload,
}

/// Hook started event
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookStartedEvent {
    #[serde(flatten)]
    pub base: EventBase,
    pub hook_id: String,
    pub hook_name: String,
    pub trigger: String,
}

/// Status change event
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusChangeEvent {
    #[serde(flatten)]
    pub base: EventBase,
    pub status: String,
    pub iteration: u32,
    pub task_name: Option<String>,
}

// ============================================================================
// Event Emitter
// ============================================================================

/// Emits execution status events to the frontend
pub struct StatusEventEmitter;

impl StatusEventEmitter {
    const EVENT_CHANNEL: &'static str = "execution-status";

    /// Get current timestamp in milliseconds
    fn now_ms() -> i64 {
        chrono::Utc::now().timestamp_millis()
    }

    /// Create base event fields
    fn create_base(event_type: &str, task_run_id: &str) -> EventBase {
        EventBase {
            event_type: event_type.to_string(),
            task_run_id: task_run_id.to_string(),
            timestamp: Self::now_ms(),
        }
    }

    /// Emit a routing decision event
    pub fn emit_routing_decision(
        app_handle: &tauri::AppHandle,
        task_run_id: &str,
        assessment: &ComplexityAssessment,
        prompt_preview: Option<&str>,
        file_count: Option<usize>,
        criteria_count: Option<usize>,
    ) {
        let complexity_str = match assessment.complexity {
            TaskComplexity::Simple => "simple",
            TaskComplexity::Medium => "medium",
            TaskComplexity::Complex => "complex",
        };

        let event = RoutingDecisionEvent {
            base: Self::create_base("routing_decision", task_run_id),
            decision: RoutingDecisionPayload {
                complexity: complexity_str.to_string(),
                confidence: assessment.confidence,
                factors: assessment.factors.clone(),
                selected_model: assessment.selected_model.clone(),
                prompt_preview: prompt_preview.map(|s| truncate_string(s, 100)),
                file_count,
                criteria_count,
            },
        };

        info!(
            "Emitting routing decision: {} -> {} (confidence: {:.2})",
            complexity_str, assessment.selected_model, assessment.confidence
        );

        if let Err(e) = app_handle.emit(Self::EVENT_CHANNEL, &event) {
            debug!("Failed to emit routing decision event: {}", e);
        }
    }

    /// Emit a retry attempt event
    pub fn emit_retry_attempt(
        app_handle: &tauri::AppHandle,
        task_run_id: &str,
        attempt: &RetryAttempt,
        state: &RetryState,
        exhausted: bool,
        next_delay_ms: Option<u64>,
    ) {
        let event = RetryAttemptEvent {
            base: Self::create_base("retry_attempt", task_run_id),
            attempt: RetryAttemptPayload {
                attempt_number: attempt.attempt_number,
                error: truncate_string(&attempt.error, 500),
                attempt_timestamp: attempt.timestamp.clone(),
                delay_ms: attempt.delay_ms,
                feedback_injected: attempt.feedback_injected,
            },
            state: RetryStatePayload {
                attempt: state.attempt,
                last_error: state.last_error.clone().map(|e| truncate_string(&e, 200)),
                last_attempt_at: state.last_attempt_at.clone(),
                total_delay_ms: state.total_delay_ms,
                error_history: state
                    .error_history
                    .iter()
                    .map(|a| RetryAttemptPayload {
                        attempt_number: a.attempt_number,
                        error: truncate_string(&a.error, 200),
                        attempt_timestamp: a.timestamp.clone(),
                        delay_ms: a.delay_ms,
                        feedback_injected: a.feedback_injected,
                    })
                    .collect(),
            },
            exhausted,
            next_retry_delay_ms: next_delay_ms,
        };

        info!(
            "Emitting retry attempt: attempt {} of history, exhausted={}",
            attempt.attempt_number, exhausted
        );

        if let Err(e) = app_handle.emit(Self::EVENT_CHANNEL, &event) {
            debug!("Failed to emit retry attempt event: {}", e);
        }
    }

    /// Emit a compression event
    pub fn emit_compression(
        app_handle: &tauri::AppHandle,
        task_run_id: &str,
        result: &CompressionResult,
        current_tokens: &TokenCount,
    ) {
        let event = CompressionEvent {
            base: Self::create_base("compression", task_run_id),
            result: CompressionResultPayload {
                original_tokens: result.original_tokens,
                compressed_tokens: result.compressed_tokens,
                items_summarized: result.items_summarized,
                summary_entries_created: result.summary_entries_created,
                compressed_categories: result.compressed_categories.clone(),
                timestamp: chrono::Utc::now().to_rfc3339(),
            },
            current_token_count: TokenCountPayload {
                total: current_tokens.total,
                findings: current_tokens.findings,
                observations: current_tokens.observations,
                feedback: current_tokens.feedback,
                solutions: current_tokens.solutions,
                other: current_tokens.other,
                entry_count: current_tokens.entry_count,
            },
        };

        info!(
            "Emitting compression event: {} -> {} tokens ({} items summarized)",
            result.original_tokens, result.compressed_tokens, result.items_summarized
        );

        if let Err(e) = app_handle.emit(Self::EVENT_CHANNEL, &event) {
            debug!("Failed to emit compression event: {}", e);
        }
    }

    /// Emit a token count update event
    pub fn emit_token_count_update(
        app_handle: &tauri::AppHandle,
        task_run_id: &str,
        tokens: &TokenCount,
        threshold: usize,
    ) {
        let threshold_percentage = if threshold > 0 {
            (tokens.total as f32 / threshold as f32) * 100.0
        } else {
            0.0
        };
        let compression_imminent = threshold_percentage >= 80.0;

        let event = TokenCountUpdateEvent {
            base: Self::create_base("token_count_update", task_run_id),
            token_count: TokenCountPayload {
                total: tokens.total,
                findings: tokens.findings,
                observations: tokens.observations,
                feedback: tokens.feedback,
                solutions: tokens.solutions,
                other: tokens.other,
                entry_count: tokens.entry_count,
            },
            threshold_percentage,
            compression_imminent,
        };

        debug!(
            "Emitting token count update: {} tokens ({:.1}% of threshold)",
            tokens.total, threshold_percentage
        );

        if let Err(e) = app_handle.emit(Self::EVENT_CHANNEL, &event) {
            debug!("Failed to emit token count update event: {}", e);
        }
    }

    /// Emit a hook execution event
    pub fn emit_hook_execution(
        app_handle: &tauri::AppHandle,
        task_run_id: &str,
        result: &HookResult,
        trigger: HookTrigger,
    ) {
        let trigger_str = trigger_to_string(trigger);

        let event = HookExecutionEvent {
            base: Self::create_base("hook_execution", task_run_id),
            result: HookExecutionPayload {
                hook_id: result.hook_id.clone(),
                hook_name: result.hook_name.clone(),
                trigger: trigger_str.clone(),
                success: result.success,
                output: result.output.clone().map(|s| truncate_string(&s, 500)),
                error: result.error.clone().map(|s| truncate_string(&s, 500)),
                duration_ms: result.duration_ms,
                timestamp: chrono::Utc::now().to_rfc3339(),
            },
        };

        info!(
            "Emitting hook execution: {} ({}) - success={}",
            result.hook_name, trigger_str, result.success
        );

        if let Err(e) = app_handle.emit(Self::EVENT_CHANNEL, &event) {
            debug!("Failed to emit hook execution event: {}", e);
        }
    }

    /// Emit a hook started event
    pub fn emit_hook_started(
        app_handle: &tauri::AppHandle,
        task_run_id: &str,
        hook_id: &str,
        hook_name: &str,
        trigger: HookTrigger,
    ) {
        let trigger_str = trigger_to_string(trigger);

        let event = HookStartedEvent {
            base: Self::create_base("hook_started", task_run_id),
            hook_id: hook_id.to_string(),
            hook_name: hook_name.to_string(),
            trigger: trigger_str.clone(),
        };

        debug!("Emitting hook started: {} ({})", hook_name, trigger_str);

        if let Err(e) = app_handle.emit(Self::EVENT_CHANNEL, &event) {
            debug!("Failed to emit hook started event: {}", e);
        }
    }

    /// Emit a status change event
    pub fn emit_status_change(
        app_handle: &tauri::AppHandle,
        task_run_id: &str,
        status: &str,
        iteration: u32,
        task_name: Option<&str>,
    ) {
        let event = StatusChangeEvent {
            base: Self::create_base("status_change", task_run_id),
            status: status.to_string(),
            iteration,
            task_name: task_name.map(|s| s.to_string()),
        };

        info!(
            "Emitting status change: {} (iteration {}, task: {:?})",
            status, iteration, task_name
        );

        if let Err(e) = app_handle.emit(Self::EVENT_CHANNEL, &event) {
            debug!("Failed to emit status change event: {}", e);
        }
    }
}

// ============================================================================
// Helper Functions
// ============================================================================

/// Convert HookTrigger to string
fn trigger_to_string(trigger: HookTrigger) -> String {
    match trigger {
        HookTrigger::PreExecution => "pre_execution".to_string(),
        HookTrigger::PostExecution => "post_execution".to_string(),
        HookTrigger::OnError => "on_error".to_string(),
        HookTrigger::OnVerificationFail => "on_verification_fail".to_string(),
        HookTrigger::OnComplete => "on_complete".to_string(),
        HookTrigger::PreIteration => "pre_iteration".to_string(),
        HookTrigger::PostIteration => "post_iteration".to_string(),
    }
}

/// Truncate a string to a maximum length
fn truncate_string(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}...", &s[..max_len])
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_truncate_string() {
        assert_eq!(truncate_string("short", 10), "short");
        assert_eq!(truncate_string("this is longer", 10), "this is lo...");
    }

    #[test]
    fn test_trigger_to_string() {
        assert_eq!(trigger_to_string(HookTrigger::PreExecution), "pre_execution");
        assert_eq!(trigger_to_string(HookTrigger::OnError), "on_error");
    }
}

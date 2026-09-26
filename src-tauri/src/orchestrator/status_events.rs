//! Execution Status Events Module
//!
//! Provides event emission for the agentic features to enable real-time
//! visibility in the frontend. Emits events for:
//! - Task routing decisions
//! - Retry attempts
//! - Memory compression operations
//! - Lifecycle hook executions

#![allow(dead_code)]

use tauri::Emitter;
use tracing::{debug, info};

use super::compression::{CompressionResult, TokenCount};
use super::hooks::{HookResult, HookTrigger};
use super::retry::RetryState;
use crate::ai_router::ComplexityAssessment;
use crate::str_utils::truncate_str_ellipsis;

// ============================================================================
// Event Types
// ============================================================================
//
// The wire types live in the lib crate so the schema-export pipeline can see
// them; see `qontinui_runner_lib::tauri_event_payloads` ("execution-status
// channel"). They are generated into `qontinui-schemas` as the `Raw*` types.
pub use qontinui_runner_lib::tauri_event_payloads::{
    CompressionEvent, CompressionResultPayload, ExecutionStatusEvent, HookExecutionEvent,
    HookExecutionPayload, HookStartedEvent, RetryAttemptEvent, RetryAttemptPayload,
    RetryStatePayload, RoutingDecisionEvent, RoutingDecisionPayload, StatusChangeEvent,
    TokenCountPayload, TokenCountUpdateEvent,
};

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

    /// Emit one event on the channel, logging (not propagating) a failure.
    fn emit(app_handle: &tauri::AppHandle, event: &ExecutionStatusEvent) {
        if let Err(e) = app_handle.emit(Self::EVENT_CHANNEL, event) {
            debug!("Failed to emit {} event: {}", event.type_tag(), e);
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
        let event = RoutingDecisionEvent {
            task_run_id: task_run_id.to_string(),
            timestamp: Self::now_ms(),
            decision: RoutingDecisionPayload {
                complexity: assessment.complexity,
                confidence: assessment.confidence,
                factors: assessment.factors.clone(),
                selected_model: assessment.selected_model.clone(),
                prompt_preview: prompt_preview.map(|s| truncate_str_ellipsis(s, 100)),
                file_count,
                criteria_count,
            },
        };

        info!(
            "Emitting routing decision: {} -> {} (confidence: {:.2})",
            assessment.complexity.display_name(),
            assessment.selected_model,
            assessment.confidence
        );

        Self::emit(app_handle, &ExecutionStatusEvent::RoutingDecision(event));
    }

    /// Emit a retry attempt event
    pub fn emit_retry_attempt(
        app_handle: &tauri::AppHandle,
        task_run_id: &str,
        state: &RetryState,
        exhausted: bool,
        next_retry_delay_ms: Option<u64>,
        max_retries: u32,
    ) {
        // Get the latest attempt from error history
        let latest_attempt = state.error_history.last().map(|a| RetryAttemptPayload {
            attempt_number: a.attempt_number,
            error: truncate_str_ellipsis(&a.error, 500),
            attempt_timestamp: a.timestamp.clone(),
            delay_ms: a.delay_ms,
            feedback_injected: a.feedback_injected,
        });

        // Build the full state payload
        let state_payload = RetryStatePayload {
            attempt: state.attempt,
            last_error: state
                .last_error
                .as_ref()
                .map(|e| truncate_str_ellipsis(e, 500)),
            last_attempt_at: state.last_attempt_at.clone(),
            total_delay_ms: state.total_delay_ms,
            error_history: state
                .error_history
                .iter()
                .map(|a| RetryAttemptPayload {
                    attempt_number: a.attempt_number,
                    error: truncate_str_ellipsis(&a.error, 200),
                    attempt_timestamp: a.timestamp.clone(),
                    delay_ms: a.delay_ms,
                    feedback_injected: a.feedback_injected,
                })
                .collect(),
        };

        // Use the latest attempt or a placeholder
        let attempt_payload = latest_attempt.unwrap_or(RetryAttemptPayload {
            attempt_number: state.attempt,
            error: state
                .last_error
                .as_ref()
                .map(|e| truncate_str_ellipsis(e, 500))
                .unwrap_or_default(),
            attempt_timestamp: state
                .last_attempt_at
                .clone()
                .unwrap_or_else(|| chrono::Utc::now().to_rfc3339()),
            delay_ms: 0,
            feedback_injected: false,
        });

        let event = RetryAttemptEvent {
            task_run_id: task_run_id.to_string(),
            timestamp: Self::now_ms(),
            attempt: attempt_payload,
            state: state_payload,
            exhausted,
            next_retry_delay_ms,
        };

        info!(
            "Emitting retry attempt: attempt {} of {}, exhausted={}",
            state.attempt, max_retries, exhausted
        );

        Self::emit(app_handle, &ExecutionStatusEvent::RetryAttempt(event));
    }

    /// Emit a compression event
    pub fn emit_compression(
        app_handle: &tauri::AppHandle,
        task_run_id: &str,
        result: &CompressionResult,
        current_tokens: &TokenCount,
    ) {
        let event = CompressionEvent {
            task_run_id: task_run_id.to_string(),
            timestamp: Self::now_ms(),
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

        Self::emit(app_handle, &ExecutionStatusEvent::Compression(event));
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
            task_run_id: task_run_id.to_string(),
            timestamp: Self::now_ms(),
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

        Self::emit(app_handle, &ExecutionStatusEvent::TokenCountUpdate(event));
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
            task_run_id: task_run_id.to_string(),
            timestamp: Self::now_ms(),
            result: HookExecutionPayload {
                hook_id: result.hook_id.clone(),
                hook_name: result.hook_name.clone(),
                trigger,
                success: result.success,
                output: result
                    .output
                    .clone()
                    .map(|s| truncate_str_ellipsis(&s, 500)),
                error: result.error.clone().map(|s| truncate_str_ellipsis(&s, 500)),
                duration_ms: result.duration_ms,
                timestamp: chrono::Utc::now().to_rfc3339(),
            },
        };

        info!(
            "Emitting hook execution: {} ({}) - success={}",
            result.hook_name, trigger_str, result.success
        );

        Self::emit(app_handle, &ExecutionStatusEvent::HookExecution(event));
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
            task_run_id: task_run_id.to_string(),
            timestamp: Self::now_ms(),
            hook_id: hook_id.to_string(),
            hook_name: hook_name.to_string(),
            trigger,
        };

        debug!("Emitting hook started: {} ({})", hook_name, trigger_str);

        Self::emit(app_handle, &ExecutionStatusEvent::HookStarted(event));
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
            task_run_id: task_run_id.to_string(),
            timestamp: Self::now_ms(),
            status: status.to_string(),
            iteration,
            task_name: task_name.map(|s| s.to_string()),
        };

        info!(
            "Emitting status change: {} (iteration {}, task: {:?})",
            status, iteration, task_name
        );

        Self::emit(app_handle, &ExecutionStatusEvent::StatusChange(event));
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

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_trigger_to_string() {
        assert_eq!(
            trigger_to_string(HookTrigger::PreExecution),
            "pre_execution"
        );
        assert_eq!(trigger_to_string(HookTrigger::OnError), "on_error");
    }
}

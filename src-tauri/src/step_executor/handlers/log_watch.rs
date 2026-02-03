//! Log Watch Step Handler
//!
//! Handles log_watch steps that scan log files for errors within a time window.
//! This provides deterministic error detection from log files for verification steps.

use async_trait::async_trait;
use tracing::info;

use super::{HandlerContext, StepHandler, StepHandlerResult};
use crate::step_executor::executor::{
    collect_recent_log_errors, format_log_errors_for_ai, get_default_log_source_names,
    ExecutionStepConfig, DEFAULT_TIME_WINDOW_SECONDS,
};

/// Handler for log_watch steps.
pub struct LogWatchHandler;

#[async_trait]
impl StepHandler for LogWatchHandler {
    fn step_type(&self) -> &'static str {
        "log_watch"
    }

    fn display_name(&self) -> &'static str {
        "Log Watch"
    }

    async fn execute(
        &self,
        step: &ExecutionStepConfig,
        _context: &HandlerContext,
    ) -> StepHandlerResult {
        let step_name = step.name.as_deref().unwrap_or("Log Watch");
        let time_window = step
            .time_window_seconds
            .unwrap_or(DEFAULT_TIME_WINDOW_SECONDS);

        // Get log sources - use configured or global settings defaults
        let log_sources: Vec<String> = step
            .log_sources
            .clone()
            .unwrap_or_else(get_default_log_source_names);

        // Get custom patterns (will be combined with defaults)
        let custom_patterns = step.error_patterns.as_deref();

        info!(
            "Executing log_watch step '{}': sources={:?}, time_window={}s",
            step_name, log_sources, time_window
        );

        // Collect errors from all log sources
        let errors = collect_recent_log_errors(&log_sources, time_window, custom_patterns).await;

        if errors.is_empty() {
            info!("Log watch '{}': No errors detected in logs", step_name);
            StepHandlerResult::success_with_data(serde_json::json!({
                "message": "No errors detected in logs"
            }))
        } else {
            let error_count = errors.len();
            let formatted_report = format_log_errors_for_ai(&errors);

            tracing::warn!(
                "Log watch '{}': Found {} error(s) in logs",
                step_name,
                error_count
            );

            // Return failure with the formatted report in output_data
            StepHandlerResult::failure_with_data(
                format!("Found {} error(s) in logs", error_count),
                serde_json::json!({
                    "error_count": error_count,
                    "report": formatted_report,
                }),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_log_watch_handler_step_type() {
        let handler = LogWatchHandler;
        assert_eq!(handler.step_type(), "log_watch");
        assert_eq!(handler.display_name(), "Log Watch");
    }
}

//! Error Resolved Step Handler
//!
//! Handles error_resolved steps that check if a specific error from the
//! error monitor database is still present in logs. This provides targeted
//! verification for error-fix workflows.
//!
//! Unlike log_watch which checks for any errors, this handler verifies
//! that a SPECIFIC error (identified by pattern or ID) no longer appears.

use async_trait::async_trait;
use regex::Regex;
use tracing::{debug, info, warn};

use super::{HandlerContext, StepHandler, StepHandlerResult};
use crate::step_executor::executor::{
    collect_recent_log_errors, ExecutionStepConfig, DEFAULT_TIME_WINDOW_SECONDS,
};

/// Handler for error_resolved verification steps.
///
/// This handler checks if a specific error pattern no longer appears in logs,
/// indicating that the error has been resolved.
pub struct ErrorResolvedHandler;

#[async_trait]
impl StepHandler for ErrorResolvedHandler {
    fn step_type(&self) -> &'static str {
        "error_resolved"
    }

    fn display_name(&self) -> &'static str {
        "Error Resolved Check"
    }

    async fn execute(
        &self,
        step: &ExecutionStepConfig,
        _context: &HandlerContext,
    ) -> StepHandlerResult {
        let step_name = step.name.as_deref().unwrap_or("Error Resolved Check");

        // Get the error pattern to check for
        let error_pattern = match &step.error_pattern {
            Some(pattern) if !pattern.is_empty() => pattern.clone(),
            _ => {
                return StepHandlerResult::failure(
                    "error_resolved step requires error_pattern to be set",
                );
            }
        };

        // Get optional error ID for reporting
        let error_id = step.error_id;

        // Get time window for log scanning
        let time_window = step
            .time_window_seconds
            .unwrap_or(DEFAULT_TIME_WINDOW_SECONDS);

        // Get log sources - use error_source if specified, otherwise all sources
        let log_sources: Vec<String> = if let Some(ref source) = step.error_source {
            vec![source.clone()]
        } else {
            step.log_sources
                .clone()
                .unwrap_or_else(|| vec!["backend.log".to_string(), "frontend.log".to_string()])
        };

        info!(
            "Checking if error is resolved: pattern='{}', error_id={:?}, sources={:?}, time_window={}s",
            error_pattern, error_id, log_sources, time_window
        );

        // Collect recent errors from logs
        let errors = collect_recent_log_errors(&log_sources, time_window, None).await;

        // Build regex from the error pattern
        // Escape special regex characters but allow basic patterns
        let pattern_regex = match build_error_regex(&error_pattern) {
            Ok(re) => re,
            Err(e) => {
                warn!("Invalid error pattern '{}': {}", error_pattern, e);
                // Fall back to literal string matching
                match Regex::new(&regex::escape(&error_pattern)) {
                    Ok(re) => re,
                    Err(_) => {
                        return StepHandlerResult::failure(format!(
                            "Failed to compile error pattern: {}",
                            error_pattern
                        ));
                    }
                }
            }
        };

        // Check if any of the recent errors match our pattern
        // Check both the message and context lines for the pattern
        let matching_errors: Vec<_> = errors
            .iter()
            .filter(|err| {
                // Check the main message
                if pattern_regex.is_match(&err.message) {
                    return true;
                }
                // Check context lines before
                for line in &err.context_before {
                    if pattern_regex.is_match(line) {
                        return true;
                    }
                }
                // Check context lines after
                for line in &err.context_after {
                    if pattern_regex.is_match(line) {
                        return true;
                    }
                }
                false
            })
            .collect();

        if matching_errors.is_empty() {
            info!(
                "Error resolved check '{}': Error pattern not found in recent logs - RESOLVED",
                step_name
            );

            // If we have an error_id, we could optionally mark it as resolved here
            // For now, that's handled by the workflow completion logic

            StepHandlerResult::success_with_data(serde_json::json!({
                "message": "Error no longer appears in logs",
                "error_id": error_id,
                "pattern": error_pattern,
                "status": "resolved"
            }))
        } else {
            let error_count = matching_errors.len();
            let sample_messages: Vec<&str> = matching_errors
                .iter()
                .take(3)
                .map(|e| e.message.as_str())
                .collect();

            warn!(
                "Error resolved check '{}': Found {} occurrence(s) of error pattern - NOT RESOLVED",
                step_name, error_count
            );

            debug!("Sample matching errors: {:?}", sample_messages);

            StepHandlerResult::failure_with_data(
                format!("Error still present: {} occurrence(s) found", error_count),
                serde_json::json!({
                    "error_id": error_id,
                    "pattern": error_pattern,
                    "occurrence_count": error_count,
                    "sample_messages": sample_messages,
                    "status": "unresolved"
                }),
            )
        }
    }
}

/// Build a regex from an error pattern.
///
/// This handles common error message patterns while being lenient about
/// special characters that might appear in error messages.
fn build_error_regex(pattern: &str) -> Result<Regex, regex::Error> {
    // Check if the pattern looks like it's already a regex (contains unescaped regex chars)
    let has_regex_chars = pattern.contains(".*")
        || pattern.contains(".+")
        || pattern.contains("\\d")
        || pattern.contains("\\w")
        || pattern.contains("[")
        || pattern.contains("(")
        || pattern.contains("^")
        || pattern.contains("$");

    if has_regex_chars {
        // Assume it's a regex pattern, try to compile as-is
        Regex::new(pattern)
    } else {
        // Treat as a literal string, escape and make case-insensitive
        let escaped = regex::escape(pattern);
        Regex::new(&format!("(?i){}", escaped))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_resolved_handler_step_type() {
        let handler = ErrorResolvedHandler;
        assert_eq!(handler.step_type(), "error_resolved");
        assert_eq!(handler.display_name(), "Error Resolved Check");
    }

    #[test]
    fn test_build_error_regex_literal() {
        let re = build_error_regex("TypeError: Cannot read property 'foo'").unwrap();
        assert!(re.is_match("TypeError: Cannot read property 'foo'"));
        assert!(re.is_match("typeerror: cannot read property 'foo'")); // Case insensitive
    }

    #[test]
    fn test_build_error_regex_pattern() {
        let re = build_error_regex("Error.*connection.*refused").unwrap();
        assert!(re.is_match("Error: connection was refused"));
        assert!(re.is_match("Error connecting: connection refused"));
    }
}

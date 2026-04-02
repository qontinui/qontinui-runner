//! Storage operations for the Tiered Information Model.
//!
//! Provides CRUD operations for task_run_automation and config_statistics tables.

use crate::str_utils::truncate_str;
use crate::database::Connection;

use super::types::{
    ConfigStatistics, DebuggingContext, FlakyItem, RunDetails, RunFailureSummary, RunStatus,
};

// ==============================================================================
// Task Run Automation Operations (Unified TaskRun Architecture)
// ==============================================================================

/// Get recent automation runs from task_run_automation table for a config.
/// Joins with task_runs to get config_id.
pub fn get_recent_automation_runs(
    conn: &Connection,
    config_id: &str,
    limit: u32,
) -> Result<Vec<RunDetails>, String> {
    Err("SQLite removed".to_string())
}

/// Get failed automation runs from task_run_automation table for a config.
pub fn get_failed_automation_runs(
    conn: &Connection,
    config_id: &str,
    limit: u32,
) -> Result<Vec<RunDetails>, String> {
    Err("SQLite removed".to_string())
}

/// Get all recent runs from task_run_automation.
pub fn get_all_recent_runs(
    conn: &Connection,
    config_id: &str,
    limit: u32,
) -> Result<Vec<RunDetails>, String> {
    Err("SQLite removed".to_string())
}

/// Get all failed runs from task_run_automation.
pub fn get_all_failed_runs(
    conn: &Connection,
    config_id: &str,
    limit: u32,
) -> Result<Vec<RunDetails>, String> {
    Err("SQLite removed".to_string())
}

// ==============================================================================
// Config Statistics Operations (Tier 4)
// ==============================================================================

/// Get or create config statistics.
pub fn get_or_create_config_statistics(
    conn: &Connection,
    config_id: &str,
) -> Result<ConfigStatistics, String> {
    Err("SQLite removed".to_string())
}

/// Get config statistics by config_id.
pub fn get_config_statistics(
    conn: &Connection,
    config_id: &str,
) -> Result<Option<ConfigStatistics>, String> {
    Err("SQLite removed".to_string())
}

/// Insert new config statistics.
pub fn insert_config_statistics(conn: &Connection, stats: &ConfigStatistics) -> Result<(), String> {
    Err("SQLite removed".to_string())
}

/// Update config statistics.
pub fn update_config_statistics(conn: &Connection, stats: &ConfigStatistics) -> Result<(), String> {
    Err("SQLite removed".to_string())
}

/// Update statistics after a run completes.
/// This is the main entry point for updating Tier 4 data.
pub fn update_statistics_after_run(conn: &Connection, run: &RunDetails) -> Result<(), String> {
    Err("SQLite removed".to_string())
}

// ==============================================================================
// Debugging Context (for AI prompts)
// ==============================================================================

/// Get flaky transitions with details.
pub fn get_flaky_transitions(
    conn: &Connection,
    config_id: &str,
    threshold: f64,
) -> Result<Vec<FlakyItem>, String> {
    Err("SQLite removed".to_string())
}

/// Get flaky templates with details.
pub fn get_flaky_templates(
    conn: &Connection,
    config_id: &str,
    threshold: f64,
) -> Result<Vec<FlakyItem>, String> {
    Err("SQLite removed".to_string())
}

/// Build debugging context for AI prompts.
/// This formats Tier 4 data in a way that's useful for AI analysis.
/// Uses data from task_run_automation table.
pub fn get_debugging_context(
    conn: &Connection,
    config_id: &str,
    config_name: Option<String>,
) -> Result<DebuggingContext, String> {
    Err("SQLite removed".to_string())
}

/// Format debugging context as a markdown string for AI prompts.
pub fn format_debugging_context_for_prompt(context: &DebuggingContext) -> String {
    let mut sections = Vec::new();

    sections.push("## Automation Run Statistics\n".to_string());

    // Header
    let config_display = context.config_name.as_ref().unwrap_or(&context.config_id);
    sections.push(format!("**Config:** {}\n", config_display));
    sections.push(format!(
        "**Total Runs:** {} | **Success Rate:** {:.1}%\n",
        context.total_runs,
        context.success_rate * 100.0
    ));

    if let Some(recent_rate) = context.recent_success_rate {
        sections.push(format!(
            "**Recent Success Rate (last 10):** {:.1}%\n",
            recent_rate * 100.0
        ));
    }

    if let Some(avg_ms) = context.avg_duration_ms {
        sections.push(format!("**Avg Duration:** {}ms\n", avg_ms));
    }

    sections.push(String::new());

    // Flaky transitions
    if !context.flaky_transitions.is_empty() {
        sections.push("### Flaky Transitions (unreliable - 20-80% success rate)\n".to_string());
        for item in &context.flaky_transitions {
            sections.push(format!(
                "- **{}**: {:.1}% success ({} attempts)\n",
                item.name,
                item.success_rate * 100.0,
                item.total_attempts
            ));
        }
        sections.push(String::new());
    }

    // Flaky templates
    if !context.flaky_templates.is_empty() {
        sections.push("### Flaky Templates (unreliable matching)\n".to_string());
        for item in &context.flaky_templates {
            sections.push(format!(
                "- **{}**: {:.1}% match rate ({} attempts)\n",
                item.name,
                item.success_rate * 100.0,
                item.total_attempts
            ));
        }
        sections.push(String::new());
    }

    // Common errors
    if !context.common_errors.is_empty() {
        sections.push("### Common Errors\n".to_string());
        for (error, count) in &context.common_errors {
            sections.push(format!("- **{}**: {} occurrences\n", error, count));
        }
        sections.push(String::new());
    }

    // Recent failures
    if !context.recent_failures.is_empty() {
        sections.push("### Recent Failures\n".to_string());
        for failure in &context.recent_failures {
            sections.push(format!(
                "- **{}** ({})\n",
                failure.run_id, failure.timestamp
            ));
            if let Some(err_type) = &failure.error_type {
                sections.push(format!("  Type: {}\n", err_type));
            }
            if let Some(msg) = &failure.error_message {
                let truncated = if msg.len() > 200 {
                    format!("{}...", truncate_str(msg, 200))
                } else {
                    msg.clone()
                };
                sections.push(format!("  Message: {}\n", truncated));
            }
            if let Some(trans) = &failure.failed_transition {
                sections.push(format!("  Failed Transition: {}\n", trans));
            }
            if let Some(templ) = &failure.failed_template {
                sections.push(format!("  Failed Template: {}\n", templ));
            }
        }
    }

    sections.join("")
}

/// Get a single run by ID from run_details table (used by tests).
/// This function works with the legacy run_details table schema.
pub fn get_run_details(conn: &Connection, run_id: &str) -> Result<Option<RunDetails>, String> {
    Err("SQLite removed".to_string())
}

// ==============================================================================
// Helper Functions
// ==============================================================================

// row_to_run_details_from_automation and row_to_config_statistics removed (SQLite dead code)

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tiered_info::{TemplateMatchRecord, TransitionRecord, TransitionStats};

    #[test]
    fn test_config_statistics() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_update_statistics_after_run() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_flaky_detection() {
        let mut stats = TransitionStats::default();
        stats.total = 10;
        stats.success = 5;
        stats.failure = 5;

        // 50% success rate should be flaky
        assert!(stats.is_flaky(0.2));

        stats.success = 9;
        stats.failure = 1;
        // 90% success rate should not be flaky
        assert!(!stats.is_flaky(0.2));

        stats.success = 1;
        stats.failure = 9;
        // 10% success rate should not be flaky (consistently failing)
        assert!(!stats.is_flaky(0.2));
    }

    #[test]
    fn test_debugging_context() {
        // SQLite removed - no-op
    }
}

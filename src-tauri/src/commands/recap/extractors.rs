//! Data extractors for the Recap module.
//!
//! Functions for extracting failure info, statistics, and summaries from task run data.

use super::types::{FailureInfo, RecapStats};
use super::utils::parse_actions_summary;
use crate::database::{TaskRun, TaskRunAutomation, TaskRunEvent};

/// Extract failure info from task run and automation records.
pub fn extract_failure_info(
    task_run: &TaskRun,
    automations: &[TaskRunAutomation],
) -> Option<FailureInfo> {
    if task_run.status != "failed" {
        return None;
    }

    // Check for error in task run itself
    if let Some(ref error_msg) = task_run.error_message {
        let failed_automation = automations.iter().find(|a| a.automation_status == "failed");
        let failed_step = failed_automation.and_then(|a| a.workflow_name.clone());
        let error_type = failed_automation.and_then(|a| a.error_type.clone());

        return Some(FailureInfo {
            reason: error_msg.clone(),
            failed_step,
            error_details: failed_automation.and_then(|a| a.error_message.clone()),
            error_type,
        });
    }

    // Check automations for failures
    if let Some(failed) = automations.iter().find(|a| a.automation_status == "failed") {
        let reason = failed
            .error_message
            .clone()
            .unwrap_or_else(|| "Workflow execution failed".to_string());

        return Some(FailureInfo {
            reason,
            failed_step: failed.workflow_name.clone(),
            error_details: failed.error_message.clone(),
            error_type: failed.error_type.clone(),
        });
    }

    // Generic failure
    Some(FailureInfo {
        reason: "Task failed".to_string(),
        failed_step: None,
        error_details: None,
        error_type: None,
    })
}

/// Calculate statistics from automation records.
pub fn calculate_stats(
    task_run: &TaskRun,
    automations: &[TaskRunAutomation],
    _events: &[TaskRunEvent],
) -> RecapStats {
    let mut stats = RecapStats::default();

    for automation in automations {
        if let Some(ref summary) = automation.actions_summary {
            let (total, success, failed, skipped) = parse_actions_summary(summary);
            stats.total_actions += total;
            stats.successful_actions += success;
            stats.failed_actions += failed;
            stats.skipped_actions += skipped;
        }
    }

    stats.ai_sessions = task_run.sessions_count as i32;

    stats
}

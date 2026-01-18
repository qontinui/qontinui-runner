//! Recap commands for the Session Recap page.
//!
//! Provides a quick overview of what happened during a task run,
//! prominently showing failure reasons and displaying all steps with brief summaries.

use crate::commands::AppState;
use crate::database::{TaskRun, TaskRunAutomation, TaskRunEvent};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tauri::State;
use tracing::{info, warn};

/// A step in the recap timeline.
#[derive(Debug, Serialize, Clone)]
pub struct RecapStep {
    /// Step name
    pub name: String,
    /// Step type: "workflow", "action", "ai_session", "test", "check"
    pub step_type: String,
    /// Status: "success", "failed", "running", "skipped"
    pub status: String,
    /// Brief summary of what happened
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// Duration in milliseconds
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<i64>,
    /// Error message if failed
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Nested steps (for workflows containing actions)
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<RecapStep>,
}

/// Information about why a run failed.
#[derive(Debug, Serialize)]
pub struct FailureInfo {
    /// Primary reason for failure
    pub reason: String,
    /// Name of the step that failed
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failed_step: Option<String>,
    /// Detailed error message or stack trace
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_details: Option<String>,
    /// Error type category
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_type: Option<String>,
}

/// Quick statistics about the run.
#[derive(Debug, Serialize, Default)]
pub struct RecapStats {
    /// Total number of actions executed
    pub total_actions: i32,
    /// Number of successful actions
    pub successful_actions: i32,
    /// Number of failed actions
    pub failed_actions: i32,
    /// Number of skipped actions
    pub skipped_actions: i32,
    /// Total number of AI sessions
    pub ai_sessions: i32,
    /// Number of tests run
    pub tests_run: i32,
    /// Number of tests passed
    pub tests_passed: i32,
}

/// Complete recap data for a task run.
#[derive(Debug, Serialize)]
pub struct RecapData {
    /// Task run ID
    pub task_run_id: String,
    /// Task name
    pub task_name: String,
    /// Overall status: "running", "complete", "failed", "stopped"
    pub status: String,
    /// Total duration in milliseconds
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<i64>,
    /// When the run started
    pub created_at: String,
    /// When the run completed
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,

    /// Failure info (prominent if failed)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_info: Option<FailureInfo>,

    /// AI-generated or extracted summary
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,

    /// Whether the goal was achieved (from orchestrator)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub goal_achieved: Option<bool>,

    /// Steps overview (timeline)
    pub steps: Vec<RecapStep>,

    /// Quick statistics
    pub stats: RecapStats,
}

/// Response wrapper for recap commands.
#[derive(Debug, Serialize)]
pub struct RecapResponse<T> {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl<T> RecapResponse<T> {
    pub fn ok(data: T) -> Self {
        Self {
            success: true,
            data: Some(data),
            error: None,
        }
    }

    pub fn err(message: String) -> Self {
        Self {
            success: false,
            data: None,
            error: Some(message),
        }
    }
}

/// Parse actions summary JSON to get counts.
fn parse_actions_summary(json_str: &str) -> (i32, i32, i32, i32) {
    #[derive(Deserialize)]
    struct ActionsSummary {
        total: Option<i32>,
        success: Option<i32>,
        failed: Option<i32>,
        skipped: Option<i32>,
    }

    if let Ok(summary) = serde_json::from_str::<ActionsSummary>(json_str) {
        (
            summary.total.unwrap_or(0),
            summary.success.unwrap_or(0),
            summary.failed.unwrap_or(0),
            summary.skipped.unwrap_or(0),
        )
    } else {
        (0, 0, 0, 0)
    }
}

/// Calculate duration between two timestamps (ISO 8601 format).
fn calculate_duration_ms(start: &str, end: &str) -> Option<i64> {
    use chrono::DateTime;

    let start_dt = DateTime::parse_from_rfc3339(start).ok()?;
    let end_dt = DateTime::parse_from_rfc3339(end).ok()?;

    Some((end_dt - start_dt).num_milliseconds())
}

/// Build steps from automation records and events.
fn build_steps(
    automations: &[TaskRunAutomation],
    events: &[TaskRunEvent],
) -> Vec<RecapStep> {
    let mut steps = Vec::new();

    // Add workflow steps from automation records
    for automation in automations {
        let workflow_name = automation.workflow_name.clone().unwrap_or_else(|| "Workflow".to_string());

        // Parse actions summary for this workflow
        let (total, success, failed, _skipped) = automation
            .actions_summary
            .as_ref()
            .map(|s| parse_actions_summary(s))
            .unwrap_or((0, 0, 0, 0));

        let summary = if total > 0 {
            Some(format!("{} actions ({} success, {} failed)", total, success, failed))
        } else {
            None
        };

        let status = match automation.automation_status.as_str() {
            "success" => "success",
            "failed" => "failed",
            "running" => "running",
            "timeout" => "failed",
            "cancelled" => "skipped",
            _ => "running",
        };

        steps.push(RecapStep {
            name: workflow_name,
            step_type: "workflow".to_string(),
            status: status.to_string(),
            summary,
            duration_ms: automation.duration_ms,
            error: automation.error_message.clone(),
            children: Vec::new(),
        });
    }

    // If no automations, build steps from events
    if automations.is_empty() && !events.is_empty() {
        // Group events by workflow/action
        let mut workflow_events: std::collections::HashMap<String, Vec<&TaskRunEvent>> =
            std::collections::HashMap::new();

        for event in events {
            let key = event.workflow_name.clone().unwrap_or_else(|| "Main".to_string());
            workflow_events.entry(key).or_default().push(event);
        }

        for (workflow_name, workflow_events) in workflow_events {
            let has_error = workflow_events.iter().any(|e|
                e.event_subtype.as_deref() == Some("error") ||
                e.event_subtype.as_deref() == Some("failed")
            );

            let status = if has_error { "failed" } else { "success" };
            let event_count = workflow_events.len();

            steps.push(RecapStep {
                name: workflow_name,
                step_type: "workflow".to_string(),
                status: status.to_string(),
                summary: Some(format!("{} events", event_count)),
                duration_ms: None,
                error: None,
                children: Vec::new(),
            });
        }
    }

    // Add AI session steps from session events
    let session_events: Vec<&TaskRunEvent> = events
        .iter()
        .filter(|e| e.event_type == "ai_session" || e.event_subtype.as_deref() == Some("session_start"))
        .collect();

    for (i, event) in session_events.iter().enumerate() {
        let session_name = format!("AI Session {}", i + 1);
        let status = if event.event_subtype.as_deref() == Some("error") {
            "failed"
        } else {
            "success"
        };

        steps.push(RecapStep {
            name: session_name,
            step_type: "ai_session".to_string(),
            status: status.to_string(),
            summary: Some(event.message.clone()),
            duration_ms: event.duration_ms,
            error: None,
            children: Vec::new(),
        });
    }

    steps
}

/// Extract failure info from task run and automation records.
fn extract_failure_info(
    task_run: &TaskRun,
    automations: &[TaskRunAutomation],
) -> Option<FailureInfo> {
    if task_run.status != "failed" {
        return None;
    }

    // Check for error in task run itself
    if let Some(ref error_msg) = task_run.error_message {
        // Find which step failed from automations
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
        let reason = failed.error_message.clone()
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
fn calculate_stats(
    task_run: &TaskRun,
    automations: &[TaskRunAutomation],
    _events: &[TaskRunEvent],
) -> RecapStats {
    let mut stats = RecapStats::default();

    // Count from automations
    for automation in automations {
        if let Some(ref summary) = automation.actions_summary {
            let (total, success, failed, skipped) = parse_actions_summary(summary);
            stats.total_actions += total;
            stats.successful_actions += success;
            stats.failed_actions += failed;
            stats.skipped_actions += skipped;
        }
    }

    // AI sessions count
    stats.ai_sessions = task_run.sessions_count as i32;

    stats
}

/// Get recap data for a specific task run.
#[tauri::command]
pub async fn get_task_run_recap(
    state: State<'_, Arc<AppState>>,
    task_run_id: String,
) -> Result<RecapResponse<RecapData>, String> {
    info!("Getting recap for task run: {}", task_run_id);

    // Get the task run
    let task_run = match state.checkpoint_db.get_task_run(&task_run_id) {
        Ok(Some(run)) => run,
        Ok(None) => {
            return Ok(RecapResponse::err(format!(
                "Task run not found: {}",
                task_run_id
            )));
        }
        Err(e) => {
            warn!("Failed to get task run {}: {}", task_run_id, e);
            return Ok(RecapResponse::err(e));
        }
    };

    // Get automation records
    let automations = state
        .checkpoint_db
        .get_task_run_automations(&task_run_id)
        .unwrap_or_else(|e| {
            warn!("Failed to get automations for {}: {}", task_run_id, e);
            Vec::new()
        });

    // Get events (limited for performance)
    let events = state
        .checkpoint_db
        .get_task_run_events(&task_run_id, None, Some(500))
        .unwrap_or_else(|e| {
            warn!("Failed to get events for {}: {}", task_run_id, e);
            Vec::new()
        });

    // Build steps
    let steps = build_steps(&automations, &events);

    // Extract failure info
    let failure_info = extract_failure_info(&task_run, &automations);

    // Calculate stats
    let stats = calculate_stats(&task_run, &automations, &events);

    // Calculate duration
    let duration_ms = if let Some(ref completed) = task_run.completed_at {
        calculate_duration_ms(&task_run.created_at, completed)
    } else {
        None
    };

    // Extract summary from output log (first meaningful line after session markers)
    let summary = extract_summary(&task_run.output_log);

    let recap = RecapData {
        task_run_id: task_run.id.clone(),
        task_name: task_run.task_name.clone(),
        status: task_run.status.clone(),
        duration_ms,
        created_at: task_run.created_at.clone(),
        completed_at: task_run.completed_at.clone(),
        failure_info,
        summary,
        goal_achieved: task_run.goal_achieved,
        steps,
        stats,
    };

    Ok(RecapResponse::ok(recap))
}

/// Extract a brief summary from the output log.
fn extract_summary(output_log: &str) -> Option<String> {
    // Skip session markers and find first substantive content
    let lines: Vec<&str> = output_log
        .lines()
        .filter(|line| {
            let trimmed = line.trim();
            !trimmed.is_empty()
                && !trimmed.starts_with("[SESSION_START:")
                && !trimmed.starts_with("---")
                && !trimmed.starts_with("===")
        })
        .collect();

    if lines.is_empty() {
        return None;
    }

    // Take first few meaningful lines
    let summary: String = lines
        .into_iter()
        .take(3)
        .collect::<Vec<_>>()
        .join(" ");

    // Truncate if too long
    let truncated = if summary.len() > 200 {
        format!("{}...", &summary[..200])
    } else {
        summary
    };

    Some(truncated)
}

//! Recap commands for the Session Recap page.
//!
//! Provides a quick overview of what happened during a task run,
//! prominently showing failure reasons and displaying all steps with brief summaries.

use crate::commands::AppState;
use crate::database::{StoredVerificationResult, TaskRun, TaskRunAutomation, TaskRunEvent};
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

/// Build steps from automation records, events, task run metadata, and verification results.
fn build_steps(
    task_run: &TaskRun,
    automations: &[TaskRunAutomation],
    events: &[TaskRunEvent],
    verification_results: &[StoredVerificationResult],
) -> Vec<RecapStep> {
    let mut steps = Vec::new();

    // PRIORITY 1: Use verification results as the authoritative source for step status
    // This is the most reliable data as it comes directly from the verification system
    if !verification_results.is_empty() {
        info!(
            "Building steps from {} verification results",
            verification_results.len()
        );

        for result in verification_results {
            let status = if result.passed { "success" } else { "failed" };
            let error = if !result.passed {
                if !result.issues.is_empty() {
                    Some(result.issues.join("; "))
                } else {
                    None
                }
            } else {
                None
            };

            let summary = if !result.observations.is_empty() {
                Some(result.observations.join("; "))
            } else {
                None
            };

            steps.push(RecapStep {
                name: result.criterion_id.clone(),
                step_type: if result.criterion_type == "deterministic" {
                    "check".to_string()
                } else {
                    "ai_session".to_string()
                },
                status: status.to_string(),
                summary,
                duration_ms: None,
                error,
                children: Vec::new(),
            });
        }

        return steps;
    }

    // PRIORITY 2: Use automation records for workflow steps
    for automation in automations {
        let workflow_name = automation
            .workflow_name
            .clone()
            .unwrap_or_else(|| "Workflow".to_string());

        // Parse actions summary for this workflow
        let (total, success, failed, _skipped) = automation
            .actions_summary
            .as_ref()
            .map(|s| parse_actions_summary(s))
            .unwrap_or((0, 0, 0, 0));

        let summary = if total > 0 {
            Some(format!(
                "{} actions ({} success, {} failed)",
                total, success, failed
            ))
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

    if !steps.is_empty() {
        return steps;
    }

    // PRIORITY 3: Use execution_steps_json with task status inference
    if let Some(ref exec_steps_json) = task_run.execution_steps_json {
        if let Some(exec_steps) = parse_execution_steps_with_task_status(exec_steps_json, task_run)
        {
            if !exec_steps.is_empty() {
                return exec_steps;
            }
        }
    }

    // PRIORITY 4: Build from events
    if !events.is_empty() {
        let mut workflow_events: std::collections::HashMap<String, Vec<&TaskRunEvent>> =
            std::collections::HashMap::new();

        for event in events {
            let key = event
                .workflow_name
                .clone()
                .unwrap_or_else(|| "Main".to_string());
            workflow_events.entry(key).or_default().push(event);
        }

        let has_named_workflows = workflow_events.keys().any(|k| k != "Main");

        if has_named_workflows {
            for (workflow_name, wf_events) in workflow_events {
                if workflow_name == "Main" {
                    continue;
                }

                let has_error = wf_events.iter().any(|e| {
                    e.event_subtype.as_deref() == Some("error")
                        || e.event_subtype.as_deref() == Some("failed")
                });

                let status = if has_error { "failed" } else { "success" };
                let event_count = wf_events.len();

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
            .filter(|e| {
                e.event_type == "ai_session" || e.event_subtype.as_deref() == Some("session_start")
            })
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

        if !steps.is_empty() {
            return steps;
        }
    }

    // PRIORITY 5: Build from output_log session markers
    if task_run.sessions_count > 0 {
        let session_steps = build_steps_from_output_log(task_run);
        if !session_steps.is_empty() {
            return session_steps;
        }
    }

    steps
}

/// Parse execution_steps_json to extract steps, inferring status from task status.
/// Used as a fallback when no verification results are available.
fn parse_execution_steps_with_task_status(
    json_str: &str,
    task_run: &TaskRun,
) -> Option<Vec<RecapStep>> {
    #[derive(Deserialize)]
    struct ExecutionStep {
        #[serde(default)]
        name: Option<String>,
        #[serde(default, alias = "id")]
        step_id: Option<String>,
        #[serde(default, rename = "type")]
        step_type: Option<String>,
        #[serde(default)]
        status: Option<String>,
    }

    if let Ok(steps) = serde_json::from_str::<Vec<ExecutionStep>>(json_str) {
        let total_steps = steps.len();
        let recap_steps: Vec<RecapStep> = steps
            .into_iter()
            .enumerate()
            .filter_map(|(idx, s)| {
                let name = s.name.or(s.step_id)?;

                // Determine status from explicit status or infer from task status
                let (status, error) = if let Some(explicit_status) = s.status {
                    let is_failed = explicit_status == "failed" || explicit_status == "error";
                    (
                        explicit_status,
                        if is_failed {
                            task_run.error_message.clone()
                        } else {
                            None
                        },
                    )
                } else {
                    // If task failed, mark the last step(s) as failed
                    let is_near_end = idx >= total_steps.saturating_sub(2);
                    if task_run.status == "failed" && is_near_end {
                        ("failed".to_string(), task_run.error_message.clone())
                    } else {
                        ("success".to_string(), None)
                    }
                };

                Some(RecapStep {
                    name,
                    step_type: s.step_type.unwrap_or_else(|| "check".to_string()),
                    status,
                    summary: None,
                    duration_ms: None,
                    error,
                    children: Vec::new(),
                })
            })
            .collect();

        if !recap_steps.is_empty() {
            return Some(recap_steps);
        }
    }

    None
}

/// Build steps from the output_log by parsing [SESSION_START:N] markers.
fn build_steps_from_output_log(task_run: &TaskRun) -> Vec<RecapStep> {
    let mut steps = Vec::new();
    let output = &task_run.output_log;

    // Count session markers in output
    let session_count = output.matches("[SESSION_START:").count();

    if session_count > 0 {
        // Parse session sections from output
        let parts: Vec<&str> = output.split("[SESSION_START:").collect();

        for (i, part) in parts.iter().enumerate().skip(1) {
            // Extract session number from marker
            let session_num = if let Some(end) = part.find(']') {
                part[..end].parse::<u32>().unwrap_or(i as u32)
            } else {
                i as u32
            };

            // Get a brief summary from the session content
            let content_start = part.find(']').map(|i| i + 1).unwrap_or(0);
            let content = &part[content_start..];
            let summary = extract_session_summary(content);

            // Determine status based on content
            let is_last = i == parts.len() - 1;
            let has_error = content.to_lowercase().contains("error")
                || content.to_lowercase().contains("failed")
                || content.to_lowercase().contains("timed out");

            let status = if is_last && task_run.status == "failed" {
                "failed"
            } else if has_error {
                "failed"
            } else {
                "success"
            };

            steps.push(RecapStep {
                name: format!("AI Session {}", session_num),
                step_type: "ai_session".to_string(),
                status: status.to_string(),
                summary,
                duration_ms: None,
                error: if status == "failed" && is_last {
                    task_run.error_message.clone()
                } else {
                    None
                },
                children: Vec::new(),
            });
        }
    } else if task_run.sessions_count > 0 {
        // No markers found but we have session count - create generic sessions
        for i in 1..=task_run.sessions_count {
            let is_last = i == task_run.sessions_count;
            let status = if is_last && task_run.status == "failed" {
                "failed"
            } else {
                "success"
            };

            steps.push(RecapStep {
                name: format!("AI Session {}", i),
                step_type: "ai_session".to_string(),
                status: status.to_string(),
                summary: None,
                duration_ms: None,
                error: if status == "failed" {
                    task_run.error_message.clone()
                } else {
                    None
                },
                children: Vec::new(),
            });
        }
    }

    steps
}

/// Extract a brief summary from session content.
fn extract_session_summary(content: &str) -> Option<String> {
    // Get first meaningful line
    let lines: Vec<&str> = content
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && !l.starts_with("---") && !l.starts_with("==="))
        .take(2)
        .collect();

    if lines.is_empty() {
        return None;
    }

    let summary = lines.join(" ");

    // Truncate if too long
    if summary.len() > 100 {
        Some(format!("{}...", &summary[..100]))
    } else {
        Some(summary)
    }
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

    // Get verification results for this task run
    let verification_results = state
        .checkpoint_db
        .get_latest_verification_results(&task_run_id)
        .unwrap_or_else(|e| {
            warn!(
                "Failed to get verification results for {}: {}",
                task_run_id, e
            );
            Vec::new()
        });

    // Build steps
    let steps = build_steps(&task_run, &automations, &events, &verification_results);

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
    let summary: String = lines.into_iter().take(3).collect::<Vec<_>>().join(" ");

    // Truncate if too long
    let truncated = if summary.len() > 200 {
        format!("{}...", &summary[..200])
    } else {
        summary
    };

    Some(truncated)
}

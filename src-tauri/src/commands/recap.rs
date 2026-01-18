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

/// A stage in the recap timeline, grouping related steps.
/// Stages represent the 4 workflow phases: setup, agentic, verification, completion.
#[derive(Debug, Serialize, Clone)]
pub struct StageRecap {
    /// Stage identifier: "setup", "agentic", "verification", "completion"
    pub stage: String,
    /// Display name: "Setup", "Agentic", "Verification", "Completion"
    pub display_name: String,
    /// Status: "success", "failed", "running", "skipped", "pending"
    pub status: String,
    /// When this stage started (ISO 8601)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    /// When this stage ended (ISO 8601)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<String>,
    /// Duration in milliseconds
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<i64>,
    /// Steps in this stage
    pub steps: Vec<RecapStep>,
    /// Iteration number (for agentic/verification in loop)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub iteration: Option<u32>,
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

    /// Steps grouped by stage (with timing from transition_history)
    pub stages: Vec<StageRecap>,

    /// Steps overview (timeline) - flat list for backwards compatibility
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

/// Stage transition data parsed from transition_history_json.
#[derive(Debug, Deserialize)]
struct StageTransition {
    from: String,
    to: String,
    timestamp: String,
    iteration: u32,
}

/// Build stages from transition history or heuristically from steps.
fn build_stages(
    task_run: &TaskRun,
    steps: &[RecapStep],
    _verification_results: &[StoredVerificationResult],
) -> Vec<StageRecap> {
    // Try to parse transition history
    if let Some(ref history_json) = task_run.transition_history_json {
        if let Ok(transitions) = serde_json::from_str::<Vec<StageTransition>>(history_json) {
            if !transitions.is_empty() {
                return build_stages_from_history(&transitions, steps, task_run);
            }
        }
    }

    // Fallback: build stages heuristically from steps
    build_stages_heuristic(task_run, steps)
}

/// Build stages from persisted transition history.
fn build_stages_from_history(
    transitions: &[StageTransition],
    steps: &[RecapStep],
    task_run: &TaskRun,
) -> Vec<StageRecap> {
    let mut stages: Vec<StageRecap> = Vec::new();

    // Group transitions by target stage
    let mut current_stage: Option<&str> = None;
    let mut stage_start: Option<&str> = None;
    let mut stage_iteration: Option<u32> = None;

    for (i, transition) in transitions.iter().enumerate() {
        // When stage changes, close previous and start new
        if current_stage != Some(&transition.to) {
            // Close previous stage if exists
            if let Some(prev_stage) = current_stage {
                let stage_steps = filter_steps_for_stage(prev_stage, steps);
                let status = compute_stage_status(&stage_steps, task_run);
                let duration = stage_start
                    .and_then(|start| calculate_duration_ms(start, &transition.timestamp));

                stages.push(StageRecap {
                    stage: prev_stage.to_string(),
                    display_name: get_stage_display_name(prev_stage),
                    status,
                    started_at: stage_start.map(String::from),
                    ended_at: Some(transition.timestamp.clone()),
                    duration_ms: duration,
                    steps: stage_steps,
                    iteration: stage_iteration,
                });
            }

            // Start new stage
            current_stage = Some(&transition.to);
            stage_start = Some(&transition.timestamp);
            stage_iteration = if transition.to == "agentic" || transition.to == "verification" {
                Some(transition.iteration)
            } else {
                None
            };
        }
    }

    // Close final stage
    if let Some(final_stage) = current_stage {
        let stage_steps = filter_steps_for_stage(final_stage, steps);
        let status = compute_stage_status(&stage_steps, task_run);

        stages.push(StageRecap {
            stage: final_stage.to_string(),
            display_name: get_stage_display_name(final_stage),
            status,
            started_at: stage_start.map(String::from),
            ended_at: task_run.completed_at.clone(),
            duration_ms: None,
            steps: stage_steps,
            iteration: stage_iteration,
        });
    }

    stages
}

/// Build stages heuristically from steps when no transition history is available.
fn build_stages_heuristic(task_run: &TaskRun, steps: &[RecapStep]) -> Vec<StageRecap> {
    let mut stages: Vec<StageRecap> = Vec::new();

    // Group steps by inferred stage
    let mut setup_steps: Vec<RecapStep> = Vec::new();
    let mut verification_steps: Vec<RecapStep> = Vec::new();
    let mut agentic_steps: Vec<RecapStep> = Vec::new();

    for step in steps {
        match step.step_type.as_str() {
            "check" | "test" => verification_steps.push(step.clone()),
            "ai_session" => agentic_steps.push(step.clone()),
            "workflow" => {
                // Classify workflows by name pattern
                let name_lower = step.name.to_lowercase();
                if name_lower.contains("setup")
                    || name_lower.contains("init")
                    || name_lower.contains("plan")
                {
                    setup_steps.push(step.clone());
                } else {
                    agentic_steps.push(step.clone());
                }
            }
            "action" => agentic_steps.push(step.clone()),
            _ => agentic_steps.push(step.clone()),
        }
    }

    // Create stages for non-empty groups
    if !setup_steps.is_empty() {
        let status = compute_stage_status(&setup_steps, task_run);
        stages.push(StageRecap {
            stage: "setup".to_string(),
            display_name: "Setup".to_string(),
            status,
            started_at: Some(task_run.created_at.clone()),
            ended_at: None,
            duration_ms: None,
            steps: setup_steps,
            iteration: None,
        });
    }

    if !agentic_steps.is_empty() {
        let status = compute_stage_status(&agentic_steps, task_run);
        stages.push(StageRecap {
            stage: "agentic".to_string(),
            display_name: "Agentic".to_string(),
            status,
            started_at: None,
            ended_at: None,
            duration_ms: None,
            steps: agentic_steps,
            iteration: None,
        });
    }

    if !verification_steps.is_empty() {
        let status = compute_stage_status(&verification_steps, task_run);
        stages.push(StageRecap {
            stage: "verification".to_string(),
            display_name: "Verification".to_string(),
            status,
            started_at: None,
            ended_at: None,
            duration_ms: None,
            steps: verification_steps,
            iteration: None,
        });
    }

    // Always add completion stage for completed tasks
    if task_run.status == "complete" || task_run.status == "failed" {
        let completion_status = if task_run.status == "complete" {
            "success"
        } else {
            "failed"
        };

        stages.push(StageRecap {
            stage: "completion".to_string(),
            display_name: "Completion".to_string(),
            status: completion_status.to_string(),
            started_at: None,
            ended_at: task_run.completed_at.clone(),
            duration_ms: None,
            steps: Vec::new(),
            iteration: None,
        });
    }

    stages
}

/// Filter steps that belong to a particular stage.
fn filter_steps_for_stage(stage: &str, steps: &[RecapStep]) -> Vec<RecapStep> {
    steps
        .iter()
        .filter(|s| {
            match stage {
                "setup" => {
                    s.step_type == "workflow" && is_setup_step(s)
                }
                "verification" => {
                    s.step_type == "check" || s.step_type == "test"
                }
                "agentic" => {
                    s.step_type == "ai_session"
                        || s.step_type == "action"
                        || (s.step_type == "workflow" && !is_setup_step(s))
                }
                "completion" => false, // Completion usually has no steps
                _ => false,
            }
        })
        .cloned()
        .collect()
}

/// Check if a step is a setup-related step based on name patterns.
fn is_setup_step(step: &RecapStep) -> bool {
    let name_lower = step.name.to_lowercase();
    name_lower.contains("setup")
        || name_lower.contains("init")
        || name_lower.contains("plan")
        || name_lower.contains("configure")
}

/// Compute overall status for a stage based on its steps.
fn compute_stage_status(steps: &[RecapStep], task_run: &TaskRun) -> String {
    if steps.is_empty() {
        return if task_run.status == "running" {
            "pending".to_string()
        } else {
            "skipped".to_string()
        };
    }

    // Check for any running steps
    if steps.iter().any(|s| s.status == "running") {
        return "running".to_string();
    }

    // Check for any failed steps
    if steps.iter().any(|s| s.status == "failed") {
        return "failed".to_string();
    }

    // All steps succeeded or skipped
    "success".to_string()
}

/// Get display name for a stage.
fn get_stage_display_name(stage: &str) -> String {
    match stage {
        "setup" => "Setup",
        "agentic" => "Agentic",
        "verification" => "Verification",
        "completion" => "Completion",
        _ => stage,
    }
    .to_string()
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

    // Build stages from transition history or heuristically
    let stages = build_stages(&task_run, &steps, &verification_results);

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
        stages,
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

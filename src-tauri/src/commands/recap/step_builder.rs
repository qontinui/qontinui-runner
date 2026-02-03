//! Step builder for the Recap module.
//!
//! Functions for building recap steps from various data sources.

use super::types::{ExecutionStep, RecapStep};
use super::utils::{
    determine_workflow_phase, extract_session_summary, extract_work_summary_for_iteration,
    get_icon_type, is_ai_step_type, parse_actions_summary,
};
use crate::database::{StoredVerificationResult, TaskRun, TaskRunAutomation, TaskRunEvent};
use chrono::{DateTime, Duration};
use std::collections::HashSet;
use tracing::info;

/// Calculate started_at timestamp from ended_at and duration_ms.
/// Returns (started_at, ended_at) tuple.
fn calculate_timestamps(
    ended_at: Option<&str>,
    duration_ms: Option<i64>,
) -> (Option<String>, Option<String>) {
    let ended = ended_at.map(|s| s.to_string());

    let started = match (ended_at, duration_ms) {
        (Some(end_str), Some(dur)) if dur > 0 => {
            // Try to parse the ended_at timestamp and subtract duration
            if let Ok(end_dt) = DateTime::parse_from_rfc3339(end_str) {
                let start_dt = end_dt - Duration::milliseconds(dur);
                Some(start_dt.to_rfc3339())
            } else {
                None
            }
        }
        _ => None,
    };

    (started, ended)
}

/// Strip "(iteration N)" suffix from a step name.
/// The iteration info is shown in the phase container header, so it's redundant in step names.
fn strip_iteration_suffix(name: &str) -> String {
    if let Some(idx) = name.rfind(" (iteration ") {
        name[..idx].to_string()
    } else {
        name.to_string()
    }
}

/// Generate a human-readable summary for an automation workflow step.
pub fn generate_automation_summary(
    automation: &TaskRunAutomation,
    total: i32,
    success: i32,
    failed: i32,
) -> String {
    if total > 0 {
        return format!("{} actions ({} success, {} failed)", total, success, failed);
    }

    match automation.automation_status.as_str() {
        "success" => {
            if let Some(duration) = automation.duration_ms {
                format!("Completed in {}ms", duration)
            } else {
                "Workflow completed successfully".to_string()
            }
        }
        "failed" => {
            if let Some(ref error) = automation.error_message {
                if error.len() > 60 {
                    format!("Failed: {}...", &error[..60])
                } else {
                    format!("Failed: {}", error)
                }
            } else {
                "Workflow failed".to_string()
            }
        }
        "running" => "Workflow in progress...".to_string(),
        "timeout" => "Workflow timed out".to_string(),
        "cancelled" => "Workflow cancelled".to_string(),
        _ => {
            if let Some(duration) = automation.duration_ms {
                format!("Executed in {}ms", duration)
            } else {
                "Workflow executed".to_string()
            }
        }
    }
}

/// Generate a summary for a verification result step.
pub fn generate_verification_summary(result: &StoredVerificationResult) -> String {
    if !result.observations.is_empty() {
        return result.observations.join("; ");
    }

    if result.passed {
        format!("{} check passed", result.criterion_type)
    } else if !result.issues.is_empty() {
        let first_issue = &result.issues[0];
        if first_issue.len() > 60 {
            format!("{}...", &first_issue[..60])
        } else {
            first_issue.clone()
        }
    } else {
        format!("{} check failed", result.criterion_type)
    }
}

/// Generate a summary for an AI session step.
pub fn generate_ai_session_summary(event_message: &str, session_num: u32, status: &str) -> String {
    let trimmed = event_message.trim();
    if !trimmed.is_empty() && trimmed.len() > 5 {
        if trimmed.len() > 80 {
            return format!("{}...", &trimmed[..80]);
        }
        return trimmed.to_string();
    }

    match status {
        "success" => format!("Session {} completed", session_num),
        "failed" => format!("Session {} encountered errors", session_num),
        _ => format!("Session {} executed", session_num),
    }
}

/// Extract agentic step name from execution_steps_json.
fn extract_agentic_step_name(task_run: &TaskRun) -> Option<String> {
    let json_str = task_run.execution_steps_json.as_ref()?;

    if let Ok(steps) = serde_json::from_str::<Vec<ExecutionStep>>(json_str) {
        for step in steps {
            if step.phase.as_deref() == Some("agentic") {
                return step.name.or(step.step_id);
            }
        }
    }

    None
}

/// Get all AI step names from execution_steps_json.
fn get_ai_step_names(task_run: &TaskRun) -> HashSet<String> {
    let mut names = HashSet::new();

    let json_str = match task_run.execution_steps_json.as_ref() {
        Some(s) => s,
        None => return names,
    };

    if let Ok(exec_steps) = serde_json::from_str::<Vec<ExecutionStep>>(json_str) {
        for s in exec_steps {
            let step_type = s.step_type.as_deref().unwrap_or("");
            if is_ai_step_type(step_type) {
                if let Some(name) = s.name.or(s.step_id) {
                    names.insert(name);
                }
            }
        }
    }

    names
}

/// Check if an automation workflow represents an AI task.
fn is_automation_ai_task(
    workflow_name: &str,
    ai_step_names: &HashSet<String>,
    task_run: &TaskRun,
) -> bool {
    if ai_step_names.contains(workflow_name) {
        return true;
    }

    if task_run.sessions_count > 0 {
        if task_run.task_name == workflow_name {
            return true;
        }
        if let Some(ref task_workflow) = task_run.workflow_name {
            if task_workflow == workflow_name {
                return true;
            }
        }
    }

    false
}

/// Build AI session steps from task run data and events.
pub fn build_ai_session_steps(
    task_run: &TaskRun,
    events: &[TaskRunEvent],
    _verification_results: &[StoredVerificationResult],
) -> Vec<RecapStep> {
    let mut steps = Vec::new();

    let base_step_name =
        extract_agentic_step_name(task_run).unwrap_or_else(|| "Execute task".to_string());

    let session_events: Vec<&TaskRunEvent> = events
        .iter()
        .filter(|e| {
            e.event_type == "ai_session" || e.event_subtype.as_deref() == Some("session_start")
        })
        .collect();

    let total_sessions = if !session_events.is_empty() {
        session_events.len()
    } else {
        task_run.output_log.matches("[SESSION_START:").count()
    };

    if !session_events.is_empty() {
        for (i, event) in session_events.iter().enumerate() {
            let session_num = (i + 1) as u32;
            let session_name = if total_sessions > 1 {
                format!("{} (session {})", base_step_name, session_num)
            } else {
                base_step_name.clone()
            };

            let status = if event.event_subtype.as_deref() == Some("error") {
                "failed"
            } else {
                "success"
            };

            let work_summary =
                extract_work_summary_for_iteration(&task_run.output_log, session_num);
            let summary = Some(generate_ai_session_summary(
                &event.message,
                session_num,
                status,
            ));

            let (started_at, ended_at) =
                calculate_timestamps(Some(&event.timestamp), event.duration_ms);

            steps.push(RecapStep {
                name: session_name,
                step_type: "ai_session".to_string(),
                status: status.to_string(),
                phase: Some("agentic".to_string()),
                iteration: Some(session_num),
                icon_type: Some("prompt".to_string()),
                work_summary,
                summary,
                started_at,
                ended_at,
                duration_ms: event.duration_ms,
                error: None,
                children: Vec::new(),
            });
        }
        return steps;
    }

    // Fall back to output_log parsing
    let output = &task_run.output_log;
    let session_count = output.matches("[SESSION_START:").count();

    if session_count > 0 {
        let parts: Vec<&str> = output.split("[SESSION_START:").collect();

        for (i, part) in parts.iter().enumerate().skip(1) {
            let session_num = if let Some(end) = part.find(']') {
                part[..end].parse::<u32>().unwrap_or(i as u32)
            } else {
                i as u32
            };

            let content_start = part.find(']').map(|idx| idx + 1).unwrap_or(0);
            let content = &part[content_start..];

            let work_summary = extract_work_summary_for_iteration(output, session_num);

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

            let session_name = if session_count > 1 {
                format!("{} (session {})", base_step_name, session_num)
            } else {
                base_step_name.clone()
            };

            let summary = extract_session_summary(content)
                .or_else(|| Some(generate_ai_session_summary("", session_num, status)));

            steps.push(RecapStep {
                name: session_name,
                step_type: "ai_session".to_string(),
                status: status.to_string(),
                phase: Some("agentic".to_string()),
                iteration: Some(session_num),
                icon_type: Some("prompt".to_string()),
                work_summary,
                summary,
                started_at: None, // No timestamp available from output_log parsing
                ended_at: None,
                duration_ms: None,
                error: if status == "failed" && is_last {
                    task_run.error_message.clone()
                } else {
                    None
                },
                children: Vec::new(),
            });
        }
    }
    // NOTE: We intentionally do NOT fall back to creating AI steps based purely on sessions_count.
    // The sessions_count can be incremented even when no actual AI session ran (e.g., during
    // workflow setup before verification fails). Only create AI session steps when there's
    // actual evidence: session events or [SESSION_START:] markers in the output.

    steps
}

/// Build steps from EXECUTED records only.
pub fn build_steps(
    task_run: &TaskRun,
    automations: &[TaskRunAutomation],
    events: &[TaskRunEvent],
    verification_results: &[StoredVerificationResult],
    workflow_verification_results: &[serde_json::Value],
) -> Vec<RecapStep> {
    let mut steps = Vec::new();
    let mut seen_step_names: HashSet<String> = HashSet::new();

    let ai_step_names = get_ai_step_names(task_run);

    // 1. Add workflow verification phase results (step-based, from execute_verification_steps)
    // These are more detailed than orchestrator criterion-based results
    if !workflow_verification_results.is_empty() {
        info!(
            "Adding {} workflow verification phase results to steps",
            workflow_verification_results.len()
        );

        for phase_result in workflow_verification_results {
            let iteration = phase_result
                .get("iteration")
                .and_then(|v| v.as_u64())
                .unwrap_or(1) as u32;

            if let Some(step_results) = phase_result.get("step_results").and_then(|v| v.as_array())
            {
                for step_result in step_results {
                    let step_name = step_result
                        .get("step_name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("Verification Step");

                    // Display name doesn't need iteration - the phase container shows it
                    let display_name = step_name.to_string();

                    // Use iteration-qualified key for deduplication across iterations
                    let dedup_key = format!("{}:iter{}", step_name, iteration);
                    if seen_step_names.contains(&dedup_key) {
                        continue;
                    }

                    let success = step_result
                        .get("success")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    let status = if success { "success" } else { "failed" };

                    let error = step_result
                        .get("error")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());

                    let duration_ms = step_result.get("duration_ms").and_then(|v| v.as_i64());

                    let step_type_raw = step_result
                        .get("step_type")
                        .and_then(|v| v.as_str())
                        .unwrap_or("check");
                    let step_type = step_type_raw.to_string();

                    // Extract verification details if present
                    let verification_details = step_result.get("verification_details");
                    let summary = if let Some(details) = verification_details {
                        let passed = details
                            .get("assertions_passed")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0);
                        let total = details
                            .get("assertions_total")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0);
                        if total > 0 {
                            Some(format!("{}/{} assertions passed", passed, total))
                        } else if success {
                            Some("Check passed".to_string())
                        } else {
                            error.clone().or_else(|| Some("Check failed".to_string()))
                        }
                    } else if success {
                        Some("Verification passed".to_string())
                    } else {
                        error
                            .clone()
                            .or_else(|| Some("Verification failed".to_string()))
                    };

                    let icon_type = get_icon_type(&step_type, step_name);

                    // Extract timestamps from step_result if available
                    let step_started_at = step_result
                        .get("started_at")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                    let step_ended_at = step_result
                        .get("ended_at")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());

                    // If no explicit timestamps, try to calculate from duration
                    let (started_at, ended_at) =
                        if step_started_at.is_some() || step_ended_at.is_some() {
                            (step_started_at, step_ended_at)
                        } else {
                            (None, None) // No timestamps available in verification results
                        };

                    seen_step_names.insert(dedup_key);
                    steps.push(RecapStep {
                        name: display_name,
                        step_type,
                        status: status.to_string(),
                        phase: Some("verification".to_string()),
                        iteration: Some(iteration),
                        icon_type,
                        work_summary: None,
                        summary,
                        started_at,
                        ended_at,
                        duration_ms,
                        error,
                        children: Vec::new(),
                    });
                }
            }
        }
    }

    // 2. Add orchestrator verification results (criterion-based) if no workflow results
    // These are fallback for older/different verification systems
    if steps.is_empty() && !verification_results.is_empty() {
        info!(
            "Adding {} orchestrator verification results to steps",
            verification_results.len()
        );

        for result in verification_results {
            if seen_step_names.contains(&result.criterion_id) {
                continue;
            }

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

            let summary = Some(generate_verification_summary(result));

            let step_type = if result.criterion_type == "deterministic" {
                "check".to_string()
            } else {
                "test".to_string()
            };
            let icon_type = get_icon_type(&step_type, &result.criterion_id);

            seen_step_names.insert(result.criterion_id.clone());
            steps.push(RecapStep {
                name: result.criterion_id.clone(),
                step_type,
                status: status.to_string(),
                phase: Some("verification".to_string()),
                iteration: None, // Orchestrator results don't have iteration info
                icon_type,
                work_summary: None,
                summary,
                started_at: None, // Orchestrator results don't have timestamps
                ended_at: None,
                duration_ms: None,
                error,
                children: Vec::new(),
            });
        }
    }

    // 2. Add automation records as workflow steps
    // Collect names of verification steps to detect duplicates
    // (check groups are tracked in both workflow_verification_phase_results and task_run_automation)
    let verification_step_names: HashSet<String> = steps
        .iter()
        .filter(|s| s.phase.as_deref() == Some("verification"))
        .map(|s| {
            // Remove iteration suffix like " (iteration 1)" to get the base name
            let name = &s.name;
            if let Some(idx) = name.rfind(" (iteration ") {
                name[..idx].to_string()
            } else {
                name.clone()
            }
        })
        .collect();
    let has_verification_steps_from_results = !verification_step_names.is_empty();

    for automation in automations {
        let workflow_name = automation
            .workflow_name
            .clone()
            .unwrap_or_else(|| "Workflow".to_string());

        if seen_step_names.contains(&workflow_name) {
            continue;
        }

        // Determine what phase this automation would be assigned to
        let phase = determine_workflow_phase(&workflow_name);

        // Skip automations that match verification step names - these are duplicates
        // because check groups are tracked in both workflow_verification_phase_results
        // and task_run_automation tables. The verification results are more detailed,
        // so we prefer those.
        let matches_verification_step = verification_step_names.contains(&workflow_name)
            || verification_step_names.iter().any(|step_name| {
                // Also check if workflow_name is contained in step_name or vice versa
                // e.g., step_name = "Check group 'backend'" vs workflow_name = "backend"
                let wn_lower = workflow_name.to_lowercase();
                let sn_lower = step_name.to_lowercase();
                wn_lower.contains(&sn_lower) || sn_lower.contains(&wn_lower)
            });

        if matches_verification_step {
            info!(
                "Skipping automation '{}' - matches verification step name",
                workflow_name
            );
            continue;
        }

        // Also skip verification-phase automations if we have verification step results
        if has_verification_steps_from_results && phase == "verification" {
            info!(
                "Skipping automation '{}' - verification phase already has steps from verification results",
                workflow_name
            );
            continue;
        }

        let (total, success, failed, _skipped) = automation
            .actions_summary
            .as_ref()
            .map(|s| parse_actions_summary(s))
            .unwrap_or((0, 0, 0, 0));

        let summary = Some(generate_automation_summary(
            automation, total, success, failed,
        ));

        let status = match automation.automation_status.as_str() {
            "success" => "success",
            "failed" => "failed",
            "running" => "running",
            "timeout" => "failed",
            "cancelled" => "skipped",
            _ => "running",
        };

        let is_ai_task = is_automation_ai_task(&workflow_name, &ai_step_names, task_run);
        let (step_type, icon_type) = if is_ai_task {
            ("ai_session".to_string(), Some("prompt".to_string()))
        } else {
            (
                "workflow".to_string(),
                get_icon_type("workflow", &workflow_name),
            )
        };

        // Get timestamps directly from automation record (it has started_at and ended_at)
        let started_at = Some(automation.started_at.clone());
        let ended_at = automation.ended_at.clone();

        seen_step_names.insert(workflow_name.clone());
        steps.push(RecapStep {
            name: workflow_name.clone(),
            step_type,
            status: status.to_string(),
            phase: Some(phase),
            iteration: None, // Automation records don't have iteration info
            icon_type,
            work_summary: None,
            summary,
            started_at,
            ended_at,
            duration_ms: automation.duration_ms,
            error: automation.error_message.clone(),
            children: Vec::new(),
        });
    }

    // 3. Add AI sessions from task run
    if task_run.sessions_count > 0 {
        let ai_steps = build_ai_session_steps(task_run, events, verification_results);
        for step in ai_steps {
            steps.push(step);
        }
    }

    // 4. Build steps from step_execution events for setup, agentic, and completion phases
    // Note: verification steps are captured from workflow_verification_phase_results when available,
    // but if verification was interrupted (no results stored), we fall back to events.
    let has_verification_results = !workflow_verification_results.is_empty();

    info!(
        "Processing {} events for steps (has_verification_results={})",
        events.len(),
        has_verification_results
    );

    for event in events {
        if event.event_type != "step_execution" {
            continue;
        }

        // Parse the event data to get step details
        let data: Option<serde_json::Value> = event
            .data
            .as_ref()
            .and_then(|d| serde_json::from_str(d).ok());

        if let Some(data) = data {
            let step_name = data
                .get("step_name")
                .and_then(|v| v.as_str())
                .unwrap_or("Step");
            let phase = data.get("phase").and_then(|v| v.as_str());
            let step_type = data
                .get("step_type")
                .and_then(|v| v.as_str())
                .unwrap_or("step");
            let iteration = data
                .get("iteration")
                .and_then(|v| v.as_u64())
                .map(|i| i as u32);

            // Process events based on phase
            // Skip verification phase events only if we already have verification results
            // (workflow_verification_phase_results is more detailed)
            match phase {
                Some("setup") | Some("agentic") | Some("completion") => {}
                Some("verification") => {
                    if has_verification_results {
                        // Skip - already handled by more detailed verification results above
                        continue;
                    }
                    // No verification results stored (e.g., interrupted) - use events as fallback
                }
                _ => continue,
            }

            // Build display name - strip iteration suffix since the phase container already shows it
            let display_name = strip_iteration_suffix(step_name);

            // Create a unique key that includes iteration to handle multiple iterations
            let dedup_key = if let Some(iter) = iteration {
                format!(
                    "{}:iter{}:{}",
                    display_name,
                    iter,
                    phase.unwrap_or("unknown")
                )
            } else {
                format!("{}:{}", display_name, phase.unwrap_or("unknown"))
            };

            // Skip if we already have this step
            if seen_step_names.contains(&dedup_key) {
                continue;
            }

            // Process "complete", "error", or "start" events
            // For interrupted workflows, "start" events without completion show as "running"
            let event_subtype = event.event_subtype.as_deref();

            let status = match event_subtype {
                Some("complete") => "success",
                Some("error") => "failed",
                Some("start") => "running",
                _ => continue, // Skip unknown event subtypes
            };

            // For "start" events, only include them if we don't have a corresponding complete/error event
            // We track this by checking if the step was already added with a terminal status
            if event_subtype == Some("start") {
                // Check if we already have a completed version of this step
                // If so, skip the start event (we prefer showing final state)
                let has_completion = events.iter().any(|e| {
                    if e.event_type != "step_execution" {
                        return false;
                    }
                    let e_subtype = e.event_subtype.as_deref();
                    if e_subtype != Some("complete") && e_subtype != Some("error") {
                        return false;
                    }
                    // Check if this is the same step by comparing action_id or step details
                    if let (Some(ref e_action_id), Some(ref this_action_id)) =
                        (&e.action_id, &event.action_id)
                    {
                        return e_action_id == this_action_id;
                    }
                    // Fallback: compare by parsing data
                    if let Some(e_data) = e
                        .data
                        .as_ref()
                        .and_then(|d| serde_json::from_str::<serde_json::Value>(d).ok())
                    {
                        let e_step_name = e_data
                            .get("step_name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        let e_phase = e_data.get("phase").and_then(|v| v.as_str()).unwrap_or("");
                        let e_iteration = e_data.get("iteration").and_then(|v| v.as_u64());
                        return e_step_name == step_name
                            && e_phase == phase.unwrap_or("")
                            && e_iteration == iteration.map(|i| i as u64);
                    }
                    false
                });
                if has_completion {
                    continue;
                }
            }

            let error = data
                .get("error")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let duration_ms = event.duration_ms;

            let icon_type = get_icon_type(step_type, step_name);

            // For step_execution events, timestamp is when the event was recorded (completion time)
            // Calculate started_at from ended_at - duration
            let (started_at, ended_at) = calculate_timestamps(Some(&event.timestamp), duration_ms);

            info!(
                "Adding step from event: name='{}', phase={:?}, status='{}', step_type='{}'",
                display_name, phase, status, step_type
            );
            seen_step_names.insert(dedup_key);
            steps.push(RecapStep {
                name: display_name,
                step_type: step_type.to_string(),
                status: status.to_string(),
                phase: phase.map(|p| p.to_string()),
                iteration,
                icon_type,
                work_summary: None,
                summary: Some(event.message.clone()),
                started_at,
                ended_at,
                duration_ms,
                error,
                children: Vec::new(),
            });
        }
    }

    info!("Total steps after events processing: {}", steps.len());

    // 5. Fallback: if no steps yet, try to build from events with named workflows
    if steps.is_empty() && !events.is_empty() {
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

                if seen_step_names.contains(&workflow_name) {
                    continue;
                }

                let has_error = wf_events.iter().any(|e| {
                    e.event_subtype.as_deref() == Some("error")
                        || e.event_subtype.as_deref() == Some("failed")
                });

                let status = if has_error { "failed" } else { "success" };
                let event_count = wf_events.len();
                let phase = determine_workflow_phase(&workflow_name);

                let is_ai_task = is_automation_ai_task(&workflow_name, &ai_step_names, task_run);
                let (step_type, icon_type) = if is_ai_task {
                    ("ai_session".to_string(), Some("prompt".to_string()))
                } else {
                    (
                        "workflow".to_string(),
                        get_icon_type("workflow", &workflow_name),
                    )
                };

                // Try to get timestamps from first and last events
                let started_at = wf_events.first().map(|e| e.timestamp.clone());
                let ended_at = wf_events.last().map(|e| e.timestamp.clone());

                seen_step_names.insert(workflow_name.clone());
                steps.push(RecapStep {
                    name: workflow_name.clone(),
                    step_type,
                    status: status.to_string(),
                    phase: Some(phase),
                    iteration: None, // Fallback doesn't have iteration info
                    icon_type,
                    work_summary: None,
                    summary: Some(format!("{} events", event_count)),
                    started_at,
                    ended_at,
                    duration_ms: None,
                    error: None,
                    children: Vec::new(),
                });
            }
        }
    }

    steps
}

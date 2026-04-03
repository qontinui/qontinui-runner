//! Task Summary Generator
//!
//! Generates AI-powered summaries for completed task runs.
//! Uses the configured AI provider (Claude CLI, Claude API, Gemini CLI, or Gemini API)
//! to analyze the task output and produce:
//! - A paragraph summary of what was accomplished across all workflow phases
//! - Whether the stated goal was achieved
//! - What remaining work exists (if goal not achieved)

use crate::ai_provider;
use crate::ai_router::TaskContext;
use crate::database::pg::PgDb;
use crate::doctor::DoctorHandle;
use crate::findings::types::{Finding, FindingStatus};
use crate::str_utils::truncate_str;
use tracing::{debug, error, info, warn};

/// Maximum output length to include in summary prompt (characters)
/// Too much output can exceed context limits
const MAX_OUTPUT_FOR_SUMMARY: usize = 50000;

/// Prompt template for summary generation (successful/completed tasks)
const SUMMARY_PROMPT_TEMPLATE: &str = r#"You are summarizing a completed workflow run. The workflow follows a multi-phase structure:

1. **Setup** - Initial configuration and preparation steps
2. **Verification/Agentic loop** - Iterative cycles where automated checks run (verification), failures are sent to an AI agent for fixing (agentic), and checks re-run until all pass or max iterations reached
3. **Completion** - Final wrap-up steps

Your job is to produce a SHORT, COMPREHENSIVE summary of the workflow's outcome. Focus on the FINAL STATE and the journey to get there.

**Structure your summary as:**
1. The FINAL OUTCOME (did all checks pass? how many iterations were needed?)
2. What issues the AI agent fixed across iterations (specific: file names, error types, tools involved)
3. What happened in the completion phase (if anything)

**Important:**
- Lead with the final result, not the initial failures
- Describe the full progression (e.g., "After 4 iterations: iteration 1 had 3 failures, the AI fixed formatting and type errors, and all 15 checks passed by iteration 4")
- Be specific about what was accomplished (files changed, error types fixed, check groups that passed)
- Do NOT include internal markers like `[STEP_COMPLETE:...]`, `[SESSION_START:...]`, `[TASK_COMPLETE]`, or `[FINDING:...]`
- Write in plain prose, no markdown formatting
- If verification passed on the first iteration with no agentic work needed, say so clearly

Respond in this exact JSON format only, with no other text:
```json
{
  "summary": "Your 2-5 sentence summary focusing on the final outcome and what was accomplished...",
  "goal_achieved": true,
  "remaining_work": null
}
```

If the goal was not achieved, set goal_achieved to false and provide remaining_work as a string describing what's left to do.

## Task Name
{task_name}

## Task Prompt
{task_prompt}
{workflow_metadata}{findings_section}
## Task Output (Last {output_chars} characters)

The output below shows the workflow's progression through iterations. Each iteration has a verification phase (automated checks) followed by an agentic phase (AI fixing issues). The LAST iteration's verification result is the final outcome.

{task_output}
"#;

/// Prompt template for summary generation of failed/stopped tasks
const FAILURE_SUMMARY_PROMPT_TEMPLATE: &str = r#"You are summarizing a workflow run that **{task_status}**. The workflow follows a multi-phase structure:

1. **Setup** - Initial configuration and preparation steps
2. **Verification/Agentic loop** - Iterative cycles of AI work (agentic) and automated checks (verification)
3. **Completion** - Final wrap-up steps (only runs if verification passes)

The workflow did NOT complete successfully. Your job is to produce a SHORT, CLEAR explanation of what happened and why it failed/stopped.

**Important:**
- Focus on explaining WHY the workflow failed or was stopped
- Describe what was attempted before the failure
- Be specific about error messages, failed steps, or missing prerequisites
- Do NOT include internal markers like `[STEP_COMPLETE:...]`, `[SESSION_START:...]`, `[TASK_COMPLETE]`, or `[FINDING:...]` in your summary text
- Write in plain prose, no markdown formatting

Respond in this exact JSON format only, with no other text:
```json
{
  "summary": "Your 2-5 sentence explanation of what happened and why the workflow failed/stopped...",
  "goal_achieved": false,
  "remaining_work": "What needs to be done to succeed next time..."
}
```

## Task Name
{task_name}

## Task Status
{task_status}

## Error Message
{error_message}

## Task Prompt
{task_prompt}
{workflow_metadata}{findings_section}
## Task Output (Last {output_chars} characters)

The output below contains `[SESSION_START:N]` markers indicating different AI sessions/phases. Earlier sessions are typically setup or agentic work; later sessions are verification and completion. `[USER_MESSAGE]...[/USER_MESSAGE]` blocks indicate messages sent by the user during interactive sessions.

{task_output}
"#;

/// Result of summary generation
#[derive(Debug)]
pub struct SummaryResult {
    pub summary: String,
    pub goal_achieved: bool,
    pub remaining_work: Option<String>,
}

/// Strip internal markers from task output before sending to the summary AI.
///
/// Removes:
/// - `[STEP_COMPLETE:...]` markers
/// - `[TASK_COMPLETE]` markers
/// - `[FINDING:...] ... [/FINDING]` blocks (findings are passed separately)
pub(crate) fn strip_output_markers(output: &str) -> String {
    let mut result = String::with_capacity(output.len());
    let mut in_finding_block = false;

    for line in output.lines() {
        let trimmed = line.trim();

        // Skip [STEP_COMPLETE:...] markers
        if trimmed.starts_with("[STEP_COMPLETE:") && trimmed.ends_with(']') {
            continue;
        }

        // Skip [TASK_COMPLETE] markers
        if trimmed == "[TASK_COMPLETE]" || trimmed.starts_with("[TASK_COMPLETE]") {
            continue;
        }

        // Track [FINDING:...] ... [/FINDING] blocks
        if trimmed.starts_with("[FINDING:") {
            in_finding_block = true;
            continue;
        }
        if trimmed == "[/FINDING]" {
            in_finding_block = false;
            continue;
        }
        if in_finding_block {
            continue;
        }

        result.push_str(line);
        result.push('\n');
    }

    result
}

/// Format findings from the database into a section for the summary prompt.
pub(crate) fn format_findings_for_summary(findings: &[Finding]) -> String {
    if findings.is_empty() {
        return String::new();
    }

    let mut parts = Vec::new();
    parts.push("\n## Findings Detected During Execution\n".to_string());
    parts.push("These findings were detected by the AI during execution. Include notable ones in the summary.\n".to_string());

    for (i, finding) in findings.iter().enumerate() {
        let status_str = match finding.status {
            FindingStatus::Resolved => " (resolved)",
            FindingStatus::NeedsInput => " (needs user input)",
            FindingStatus::WontFix => " (won't fix)",
            FindingStatus::Deferred => " (deferred)",
            _ => "",
        };

        parts.push(format!(
            "{}. [{}:{}{}] {}: {}",
            i + 1,
            finding.category.as_str(),
            finding.severity.as_str(),
            status_str,
            finding.title,
            finding.description
        ));

        if let Some(ref resolution) = finding.resolution {
            parts.push(format!("   Resolution: {}", resolution));
        }
    }

    parts.join("\n")
}

/// Assemble AI output from task_run_events when output_log is empty.
///
/// For unified workflow runs, the AI conversation output is stored in task_run_events
/// (event_type='ai_output') rather than in the output_log field. This function
/// reconstructs the output from those events for summary generation.
async fn assemble_output_from_events(pg: &PgDb, task_run_id: &str) -> String {
    let events = pg
        .get_task_run_events(task_run_id, Some("ai_output"), None)
        .await
        .unwrap_or_default();

    if events.is_empty() {
        return String::new();
    }

    let mut parts = Vec::new();
    for event in &events {
        let phase = event.event_subtype.as_deref().unwrap_or("unknown");
        let iteration_label = &event.message;

        // Extract the "output" field from the event's JSON data
        if let Some(ref data_str) = event.data {
            if let Ok(data) = serde_json::from_str::<serde_json::Value>(data_str) {
                if let Some(output) = data.get("output").and_then(|v| v.as_str()) {
                    let iteration = data.get("iteration").and_then(|v| v.as_u64()).unwrap_or(0);
                    parts.push(format!(
                        "[SESSION_START:{} - {} phase]\n{}",
                        iteration, phase, output
                    ));
                } else {
                    // Fallback: use the message as context
                    parts.push(format!("[SESSION: {}]", iteration_label));
                }
            }
        }
    }

    parts.join("\n\n")
}

/// Build a workflow metadata section for the summary prompt.
///
/// Includes verification phase results and transition history so the summary AI
/// has structured context beyond just the raw output log.
async fn build_workflow_metadata(
    pg: &PgDb,
    task_run_id: &str,
    task: &crate::database::TaskRun,
) -> String {
    let mut parts = Vec::new();

    // Add verification phase results (structured data from database)
    let verification_results = pg
        .get_all_verification_phase_results(task_run_id)
        .await
        .unwrap_or_default();

    if !verification_results.is_empty() {
        let last_iteration_idx = verification_results.len() - 1;
        parts.push("\n## Verification Results\n".to_string());
        for (idx, result) in verification_results.iter().enumerate() {
            let iteration = result
                .get("iteration")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let all_passed = result
                .get("all_passed")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let passed = result
                .get("passed_steps")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let failed = result
                .get("failed_steps")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let total = result
                .get("total_steps")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);

            let status = if all_passed { "PASSED" } else { "FAILED" };
            parts.push(format!(
                "- Iteration {}: {} ({} passed, {} failed out of {} steps)",
                iteration, status, passed, failed, total
            ));

            let is_last = idx == last_iteration_idx;

            if let Some(step_results) = result.get("step_results").and_then(|v| v.as_array()) {
                for sr in step_results {
                    let success = sr.get("success").and_then(|v| v.as_bool()).unwrap_or(true);
                    let name = sr
                        .get("step_name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown");

                    if !success {
                        let err = sr.get("error").and_then(|v| v.as_str()).unwrap_or("");
                        let truncated = if err.len() > 150 {
                            truncate_str(err, 150)
                        } else {
                            err
                        };
                        parts.push(format!("  - FAILED: {} - {}", name, truncated));
                    } else if is_last {
                        parts.push(format!("  - PASSED: {}", name));
                    }
                }
            }
        }
        parts.push(String::new());
    }

    // Add transition history (phases that ran and in what order)
    if let Some(ref history_json) = task.transition_history_json {
        if let Ok(transitions) = serde_json::from_str::<Vec<serde_json::Value>>(history_json) {
            if !transitions.is_empty() {
                parts.push("## Workflow Phases Executed\n".to_string());
                for t in &transitions {
                    let phase = t.get("stage").and_then(|v| v.as_str()).unwrap_or("unknown");
                    let iteration = t.get("iteration").and_then(|v| v.as_u64());
                    let iter_str = iteration
                        .map(|i| format!(" (iteration {})", i))
                        .unwrap_or_default();
                    parts.push(format!("- {}{}", phase, iter_str));
                }
                parts.push(String::new());
            }
        }
    }

    if parts.is_empty() {
        String::new()
    } else {
        parts.join("\n")
    }
}

/// Generate a summary for a completed task run
///
/// This function:
/// 1. Fetches the task run from the database
/// 2. Fetches any findings detected during execution
/// 3. Strips internal markers from the output
/// 4. Uses the configured AI provider to generate a summary
/// 5. Parses the response
/// 6. Updates the database with the summary
///
/// # Arguments
/// * `db` - Database connection
/// * `task_run_id` - ID of the completed task run
///
/// # Returns
/// Ok(SummaryResult) on success, Err on failure
pub async fn generate_task_summary(
    task_run_id: &str,
    doctor_handle: Option<&DoctorHandle>,
    model_override: Option<&str>,
    provider_override: Option<&str>,
) -> Result<SummaryResult, String> {
    info!("Generating summary for task run: {}", task_run_id);

    let pg = PgDb::try_global().ok_or_else(|| "PgDb not initialized".to_string())?;

    // Get the task run from the database
    let task = pg
        .get_task_run(task_run_id)
        .await?
        .ok_or_else(|| format!("Task run not found: {}", task_run_id))?;

    // Determine if this is a failed/stopped task (affects prompt template choice)
    let is_failure = task.status == "failed" || task.status == "stopped";

    // Fetch findings for this task run
    let findings = pg
        .get_findings_for_task(task_run_id)
        .await
        .unwrap_or_else(|e| {
            warn!("Failed to fetch findings for summary: {}", e);
            Vec::new()
        });
    let findings_section = format_findings_for_summary(&findings);

    // Build structured workflow metadata (verification results, transition history)
    let workflow_metadata = build_workflow_metadata(&pg, task_run_id, &task).await;

    // Prepare the output for summarization.
    // For unified workflow runs, output_log is often empty because AI output is stored
    // in task_run_events instead. Fall back to assembling from events.
    let raw_output = if task.output_log.trim().is_empty() {
        info!("output_log is empty, assembling from task_run_events");
        assemble_output_from_events(&pg, task_run_id).await
    } else {
        task.output_log.clone()
    };

    let cleaned_output = strip_output_markers(&raw_output);
    let truncated_output = if cleaned_output.len() > MAX_OUTPUT_FOR_SUMMARY {
        // Take the last N characters (most recent output is usually most relevant)
        let start = cleaned_output.len() - MAX_OUTPUT_FOR_SUMMARY;
        format!("...[truncated]...\n{}", &cleaned_output[start..])
    } else {
        cleaned_output
    };

    // Get the task prompt (may be empty for some task types)
    let task_prompt = task.prompt.as_deref().unwrap_or("(No prompt recorded)");

    // Build the summary prompt using appropriate template
    let prompt = if is_failure {
        let error_message = task
            .error_message
            .as_deref()
            .unwrap_or("(No error message recorded)");
        FAILURE_SUMMARY_PROMPT_TEMPLATE
            .replace("{task_name}", &task.task_name)
            .replace("{task_status}", &task.status)
            .replace("{error_message}", error_message)
            .replace("{task_prompt}", task_prompt)
            .replace("{workflow_metadata}", &workflow_metadata)
            .replace("{findings_section}", &findings_section)
            .replace("{output_chars}", &truncated_output.len().to_string())
            .replace("{task_output}", &truncated_output)
    } else {
        SUMMARY_PROMPT_TEMPLATE
            .replace("{task_name}", &task.task_name)
            .replace("{task_prompt}", task_prompt)
            .replace("{workflow_metadata}", &workflow_metadata)
            .replace("{findings_section}", &findings_section)
            .replace("{output_chars}", &truncated_output.len().to_string())
            .replace("{task_output}", &truncated_output)
    };

    debug!(
        "Summary prompt length: {} chars, output length: {} chars, findings: {}",
        prompt.len(),
        truncated_output.len(),
        findings.len()
    );

    // Build task context for routing (summary generation is typically simple)
    let task_context = TaskContext::from_prompt(&prompt);

    // Use the AI provider module to run the prompt with model override support
    // run_prompt_with_model_override is sync, so run in spawn_blocking
    let prompt_clone = prompt.clone();
    let doctor_clone = doctor_handle.cloned();
    let model_clone = model_override.map(|s| s.to_string());
    let provider_clone = provider_override.map(|s| s.to_string());

    let response = tokio::task::spawn_blocking(move || {
        ai_provider::run_prompt_with_model_override(
            &prompt_clone,
            &task_context,
            doctor_clone.as_ref(),
            model_clone.as_deref(),
            provider_clone.as_deref(),
            None,
            None,
            None,
            None,
        )
    })
    .await
    .map_err(|e| format!("Task spawn error: {}", e))?;

    if !response.success {
        let err = response
            .error
            .unwrap_or_else(|| "Unknown error".to_string());
        error!("AI provider failed for summary generation: {}", err);
        return Err(err);
    }

    // Parse the JSON response
    let result = parse_summary_response(&response.output)?;

    // Update the database
    let now = chrono::Utc::now().to_rfc3339();
    pg.update_task_summary(
        task_run_id,
        Some(&result.summary),
        Some(result.goal_achieved),
        result.remaining_work.as_deref(),
        &now,
    )
    .await?;

    info!(
        "Generated summary for task {}: goal_achieved={}",
        task_run_id, result.goal_achieved
    );

    Ok(result)
}

/// Parse the JSON response from the AI
fn parse_summary_response(response: &str) -> Result<SummaryResult, String> {
    // Try to extract JSON from the response
    // AI might include markdown code blocks or other text
    let json_str = extract_json_from_response(response)?;

    // Parse the JSON
    let parsed: serde_json::Value =
        serde_json::from_str(&json_str).map_err(|e| format!("Failed to parse JSON: {}", e))?;

    // Strip any output markers the AI might have included in its response.
    // The prompt instructs the AI not to include these, but it sometimes does anyway.
    let raw_summary = parsed
        .get("summary")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'summary' field in response")?;
    let summary = strip_output_markers(raw_summary).trim().to_string();

    let goal_achieved = parsed
        .get("goal_achieved")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let remaining_work = parsed.get("remaining_work").and_then(|v| {
        if v.is_null() {
            None
        } else {
            v.as_str()
                .map(|s| strip_output_markers(s).trim().to_string())
        }
    });

    Ok(SummaryResult {
        summary,
        goal_achieved,
        remaining_work,
    })
}

/// Extract JSON from a response that might contain markdown or other text
fn extract_json_from_response(response: &str) -> Result<String, String> {
    // Try to find JSON in code blocks first
    if let Some(start) = response.find("```json") {
        let json_start = start + 7;
        if let Some(end) = response[json_start..].find("```") {
            return Ok(response[json_start..json_start + end].trim().to_string());
        }
    }

    // Try to find JSON in generic code blocks
    if let Some(start) = response.find("```") {
        let block_start = start + 3;
        // Skip any language identifier on the same line
        let content_start = response[block_start..]
            .find('\n')
            .map(|i| block_start + i + 1)
            .unwrap_or(block_start);
        if let Some(end) = response[content_start..].find("```") {
            let json_candidate = response[content_start..content_start + end].trim();
            if json_candidate.starts_with('{') {
                return Ok(json_candidate.to_string());
            }
        }
    }

    // Try to find raw JSON object
    if let Some(start) = response.find('{') {
        if let Some(end) = response.rfind('}') {
            if end > start {
                return Ok(response[start..=end].to_string());
            }
        }
    }

    Err("Could not find JSON in response".to_string())
}

/// Async entry point for generate_task_summary, using PgDb::global().
///
/// Called from loop_controller.rs and unified_workflows.rs.
pub async fn generate_task_summary_async(
    task_run_id: String,
    doctor_handle: Option<DoctorHandle>,
    model_override: Option<String>,
    provider_override: Option<String>,
) -> Result<SummaryResult, String> {
    generate_task_summary(
        &task_run_id,
        doctor_handle.as_ref(),
        model_override.as_deref(),
        provider_override.as_deref(),
    )
    .await
}

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
use crate::doctor::DoctorHandle;
use crate::findings::types::{Finding, FindingStatus};
use crate::str_utils::truncate_str;
use std::sync::Arc;
use tracing::{debug, error, info, warn};
use crate::database::CheckpointDb;

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
fn assemble_output_from_events(db: &CheckpointDb, task_run_id: &str) -> String {
    String::new()
}

/// Build a workflow metadata section for the summary prompt.
///
/// Includes verification phase results and transition history so the summary AI
/// has structured context beyond just the raw output log.
fn build_workflow_metadata(
    db: &CheckpointDb,
    task_run_id: &str,
    task: &crate::database::TaskRun,
) -> String {
    String::new()
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
pub fn generate_task_summary(
    db: &CheckpointDb,
    task_run_id: &str,
    doctor_handle: Option<&DoctorHandle>,
    model_override: Option<&str>,
    provider_override: Option<&str>,
) -> Result<SummaryResult, String> {
    Err("SQLite removed".to_string())
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
    Err("SQLite removed".to_string())
}
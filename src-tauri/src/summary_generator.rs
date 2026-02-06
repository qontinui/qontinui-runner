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
use crate::database::CheckpointDb;
use crate::findings::types::{Finding, FindingStatus};
use std::sync::Arc;
use tracing::{debug, error, info, warn};

/// Maximum output length to include in summary prompt (characters)
/// Too much output can exceed context limits
const MAX_OUTPUT_FOR_SUMMARY: usize = 50000;

/// Prompt template for summary generation
const SUMMARY_PROMPT_TEMPLATE: &str = r#"You are summarizing a completed workflow run. The workflow follows a multi-phase structure:

1. **Setup** - Initial configuration and preparation steps
2. **Verification/Agentic loop** - Iterative cycles of AI work (agentic) and automated checks (verification)
3. **Completion** - Final wrap-up steps

Your job is to produce a SHORT, COMPREHENSIVE recap of the ENTIRE workflow - not just the last phase. Cover what was done across all phases. The summary should read like a changelog entry or status report.

**Important:**
- Summarize the full workflow, including setup actions, what the AI agent did, verification results, and completion steps
- Do NOT include internal markers like `[STEP_COMPLETE:...]`, `[SESSION_START:...]`, `[TASK_COMPLETE]`, or `[FINDING:...]` in your summary text
- Write in plain prose, no markdown formatting
- Be specific about what was accomplished (files changed, tests run, repos pushed, etc.)

Respond in this exact JSON format only, with no other text:
```json
{
  "summary": "Your 2-5 sentence summary of the entire workflow here...",
  "goal_achieved": true,
  "remaining_work": null
}
```

If the goal was not achieved, set goal_achieved to false and provide remaining_work as a string describing what's left to do.

## Task Name
{task_name}

## Task Prompt
{task_prompt}
{findings_section}
## Task Output (Last {output_chars} characters)

The output below contains `[SESSION_START:N]` markers indicating different AI sessions/phases. Earlier sessions are typically setup or agentic work; later sessions are verification and completion.

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
) -> Result<SummaryResult, String> {
    info!("Generating summary for task run: {}", task_run_id);

    // Get the task run from the database
    let task = db
        .get_task_run(task_run_id)?
        .ok_or_else(|| format!("Task run not found: {}", task_run_id))?;

    // Skip if already has a summary
    if task.summary.is_some() {
        info!("Task {} already has a summary, skipping", task_run_id);
        return Err("Task already has a summary".to_string());
    }

    // Skip if task is in a terminal error state
    // Note: We allow generation while task is "running" since the Summary step
    // in the completion phase may call this before the task is marked complete
    if task.status == "failed" || task.status == "stopped" {
        warn!(
            "Task {} has terminal status ({}), skipping summary",
            task_run_id, task.status
        );
        return Err(format!("Task has terminal status: {}", task.status));
    }

    // Fetch findings for this task run
    let findings = db.get_findings_for_task(task_run_id).unwrap_or_else(|e| {
        warn!("Failed to fetch findings for summary: {}", e);
        Vec::new()
    });
    let findings_section = format_findings_for_summary(&findings);

    // Prepare the output for summarization: strip markers first, then truncate
    let cleaned_output = strip_output_markers(&task.output_log);
    let truncated_output = if cleaned_output.len() > MAX_OUTPUT_FOR_SUMMARY {
        // Take the last N characters (most recent output is usually most relevant)
        let start = cleaned_output.len() - MAX_OUTPUT_FOR_SUMMARY;
        format!("...[truncated]...\n{}", &cleaned_output[start..])
    } else {
        cleaned_output
    };

    // Get the task prompt (may be empty for some task types)
    let task_prompt = task.prompt.as_deref().unwrap_or("(No prompt recorded)");

    // Build the summary prompt
    let prompt = SUMMARY_PROMPT_TEMPLATE
        .replace("{task_name}", &task.task_name)
        .replace("{task_prompt}", task_prompt)
        .replace("{findings_section}", &findings_section)
        .replace("{output_chars}", &truncated_output.len().to_string())
        .replace("{task_output}", &truncated_output);

    debug!(
        "Summary prompt length: {} chars, output length: {} chars, findings: {}",
        prompt.len(),
        truncated_output.len(),
        findings.len()
    );

    // Build task context for routing (summary generation is typically simple)
    let task_context = TaskContext::from_prompt(&prompt);

    // Use the AI provider module to run the prompt with routing
    let response = ai_provider::run_prompt_with_routing(&prompt, &task_context, 0);

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
    db.update_task_summary(
        task_run_id,
        &result.summary,
        result.goal_achieved,
        result.remaining_work.as_deref(),
    )?;

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

    let summary = parsed
        .get("summary")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'summary' field in response")?
        .to_string();

    let goal_achieved = parsed
        .get("goal_achieved")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let remaining_work = parsed.get("remaining_work").and_then(|v| {
        if v.is_null() {
            None
        } else {
            v.as_str().map(String::from)
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

/// Async wrapper for generate_task_summary
pub async fn generate_task_summary_async(
    db: Arc<CheckpointDb>,
    task_run_id: String,
) -> Result<SummaryResult, String> {
    tokio::task::spawn_blocking(move || generate_task_summary(&db, &task_run_id))
        .await
        .map_err(|e| format!("Task spawn error: {}", e))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_json_from_response() {
        // JSON in code block
        let response = r#"Here's the summary:
```json
{
  "summary": "Test summary",
  "goal_achieved": true,
  "remaining_work": null
}
```
"#;
        let result = extract_json_from_response(response).unwrap();
        assert!(result.contains("\"summary\""));

        // Raw JSON
        let response2 =
            r#"{"summary": "Test", "goal_achieved": false, "remaining_work": "More work"}"#;
        let result2 = extract_json_from_response(response2).unwrap();
        assert!(result2.contains("\"summary\""));
    }

    #[test]
    fn test_parse_summary_response() {
        let json = r#"{"summary": "Task completed successfully", "goal_achieved": true, "remaining_work": null}"#;
        let result = parse_summary_response(json).unwrap();
        assert_eq!(result.summary, "Task completed successfully");
        assert!(result.goal_achieved);
        assert!(result.remaining_work.is_none());

        let json2 = r#"{"summary": "Partially done", "goal_achieved": false, "remaining_work": "Need to fix tests"}"#;
        let result2 = parse_summary_response(json2).unwrap();
        assert!(!result2.goal_achieved);
        assert_eq!(result2.remaining_work.as_deref(), Some("Need to fix tests"));
    }

    #[test]
    fn test_strip_output_markers() {
        let output = "Starting work...\n\
                       [SESSION_START:1]\n\
                       Did some setup work\n\
                       [STEP_COMPLETE:setup-0]\n\
                       [SESSION_START:2]\n\
                       Running verification\n\
                       [FINDING:config_issue:medium]\n\
                       Title: URL moved\n\
                       Description: Remote URL has changed\n\
                       [/FINDING]\n\
                       More output here\n\
                       [STEP_COMPLETE:completion-0]\n\
                       [TASK_COMPLETE]\n\
                       Final line\n";

        let stripped = strip_output_markers(output);
        assert!(!stripped.contains("[STEP_COMPLETE:"));
        assert!(!stripped.contains("[TASK_COMPLETE]"));
        assert!(!stripped.contains("[FINDING:"));
        assert!(!stripped.contains("[/FINDING]"));
        assert!(!stripped.contains("Title: URL moved"));
        // Session markers are kept (they help the AI understand phase boundaries)
        assert!(stripped.contains("[SESSION_START:1]"));
        assert!(stripped.contains("[SESSION_START:2]"));
        assert!(stripped.contains("Starting work..."));
        assert!(stripped.contains("Did some setup work"));
        assert!(stripped.contains("More output here"));
        assert!(stripped.contains("Final line"));
    }

    #[test]
    fn test_format_findings_for_summary_empty() {
        let findings: Vec<Finding> = vec![];
        assert_eq!(format_findings_for_summary(&findings), "");
    }
}

//! Task Summary Generator
//!
//! Generates AI-powered summaries for completed task runs.
//! Uses the configured AI provider (Claude CLI, Claude API, Gemini CLI, or Gemini API)
//! to analyze the task output and produce:
//! - A paragraph summary of what was accomplished
//! - Whether the stated goal was achieved
//! - What remaining work exists (if goal not achieved)

use crate::ai_provider;
use crate::ai_router::TaskContext;
use crate::database::CheckpointDb;
use std::sync::Arc;
use tracing::{debug, error, info, warn};

/// Maximum output length to include in summary prompt (characters)
/// Too much output can exceed context limits
const MAX_OUTPUT_FOR_SUMMARY: usize = 50000;

/// Prompt template for summary generation
const SUMMARY_PROMPT_TEMPLATE: &str = r#"You are analyzing a completed AI task run. Based on the task output below, provide:

1. A concise paragraph summary (2-4 sentences) of what was accomplished
2. Whether the stated goal was achieved (true/false)
3. If the goal was NOT achieved, what remaining work exists

Respond in this exact JSON format only, with no other text:
```json
{
  "summary": "Your paragraph summary here...",
  "goal_achieved": true,
  "remaining_work": null
}
```

If the goal was not achieved, set goal_achieved to false and provide remaining_work as a string describing what's left to do.

## Task Name
{task_name}

## Task Prompt
{task_prompt}

## Task Output (Last {output_chars} characters)
{task_output}
"#;

/// Result of summary generation
#[derive(Debug)]
pub struct SummaryResult {
    pub summary: String,
    pub goal_achieved: bool,
    pub remaining_work: Option<String>,
}

/// Generate a summary for a completed task run
///
/// This function:
/// 1. Fetches the task run from the database
/// 2. Uses the configured AI provider to generate a summary
/// 3. Parses the response
/// 4. Updates the database with the summary
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

    // Prepare the output for summarization
    let output = &task.output_log;
    let truncated_output = if output.len() > MAX_OUTPUT_FOR_SUMMARY {
        // Take the last N characters (most recent output is usually most relevant)
        let start = output.len() - MAX_OUTPUT_FOR_SUMMARY;
        format!("...[truncated]...\n{}", &output[start..])
    } else {
        output.clone()
    };

    // Get the task prompt (may be empty for some task types)
    let task_prompt = task.prompt.as_deref().unwrap_or("(No prompt recorded)");

    // Build the summary prompt
    let prompt = SUMMARY_PROMPT_TEMPLATE
        .replace("{task_name}", &task.task_name)
        .replace("{task_prompt}", task_prompt)
        .replace("{output_chars}", &truncated_output.len().to_string())
        .replace("{task_output}", &truncated_output);

    debug!(
        "Summary prompt length: {} chars, output length: {} chars",
        prompt.len(),
        truncated_output.len()
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
}

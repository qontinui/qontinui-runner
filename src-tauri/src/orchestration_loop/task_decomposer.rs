//! Task decomposition for the orchestration loop planner agent.
//!
//! Decomposes a high-level goal into ordered subtasks with dependency tracking.
//! The AI prompt asks for a structured JSON plan; this module parses and validates it.

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use super::types::DecomposerConfig;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SubTaskStatus {
    Pending,
    Running,
    Completed {
        task_run_id: String,
        output_summary: String,
    },
    Failed {
        task_run_id: String,
        error: String,
    },
    Skipped {
        reason: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubTask {
    pub id: String,
    pub index: u32,
    pub title: String,
    pub description: String,
    pub depends_on: Vec<String>,
    pub expected_output: String,
    pub workflow_id: Option<String>,
    pub status: SubTaskStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecompositionPlan {
    pub id: String,
    pub original_goal: String,
    pub goal_summary: String,
    pub subtasks: Vec<SubTask>,
    pub created_at: String,
}

/// Build the AI prompt to decompose a goal into subtasks.
pub fn build_decomposition_prompt(goal: &str, config: &DecomposerConfig) -> String {
    format!(
        "You are a task planner. Decompose the following goal into {}-{} specific, actionable sub-tasks.\n\
        \n\
        Goal: {}\n\
        \n\
        Requirements:\n\
        - Each sub-task should be independently executable\n\
        - Sub-tasks should be ordered by dependency (earlier tasks first)\n\
        - Each sub-task should have a clear expected output\n\
        - Use depends_on to reference sub-task IDs that must complete first\n\
        \n\
        Respond with a JSON object:\n\
        {{\n\
          \"goal_summary\": \"one-line summary of the goal\",\n\
          \"subtasks\": [\n\
            {{\n\
              \"id\": \"subtask_1\",\n\
              \"title\": \"short title\",\n\
              \"description\": \"detailed description of what to do\",\n\
              \"depends_on\": [],\n\
              \"expected_output\": \"what successful completion looks like\"\n\
            }}\n\
          ]\n\
        }}",
        config.min_subtasks, config.max_subtasks, goal
    )
}

/// Parse the AI response into a DecompositionPlan.
pub fn parse_decomposition_response(
    goal: &str,
    response: &str,
    config: &DecomposerConfig,
) -> Result<DecompositionPlan, String> {
    // Try to extract JSON from response (may have markdown wrapping)
    let json_str = extract_json_from_response(response)?;

    let parsed: serde_json::Value = serde_json::from_str(&json_str)
        .map_err(|e| format!("Failed to parse decomposition response as JSON: {}", e))?;

    let goal_summary = parsed
        .get("goal_summary")
        .and_then(|v| v.as_str())
        .unwrap_or("No summary provided")
        .to_string();

    let subtasks_arr = parsed
        .get("subtasks")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "Missing 'subtasks' array in response".to_string())?;

    if subtasks_arr.len() < config.min_subtasks as usize {
        return Err(format!(
            "Too few subtasks: got {}, minimum {}",
            subtasks_arr.len(),
            config.min_subtasks
        ));
    }

    let mut subtasks: Vec<SubTask> = Vec::new();
    for (i, item) in subtasks_arr.iter().enumerate() {
        if i >= config.max_subtasks as usize {
            break;
        }

        let default_id = format!("subtask_{}", i + 1);
        let id = item
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or(&default_id)
            .to_string();

        let title = item
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("Untitled subtask")
            .to_string();

        let description = item
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let depends_on: Vec<String> = item
            .get("depends_on")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();

        let expected_output = item
            .get("expected_output")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        subtasks.push(SubTask {
            id,
            index: i as u32,
            title,
            description,
            depends_on,
            expected_output,
            workflow_id: None,
            status: SubTaskStatus::Pending,
        });
    }

    info!(
        "Decomposed goal into {} subtasks: {}",
        subtasks.len(),
        subtasks
            .iter()
            .map(|s| s.title.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );

    Ok(DecompositionPlan {
        id: uuid::Uuid::new_v4().to_string(),
        original_goal: goal.to_string(),
        goal_summary,
        subtasks,
        created_at: chrono::Utc::now().to_rfc3339(),
    })
}

/// Extract JSON object from a response that may contain markdown code fences.
fn extract_json_from_response(response: &str) -> Result<String, String> {
    // Try direct parse first
    if serde_json::from_str::<serde_json::Value>(response).is_ok() {
        return Ok(response.to_string());
    }

    // Try extracting from code fences
    if let Some(start) = response.find("```json") {
        let content = &response[start + 7..];
        if let Some(end) = content.find("```") {
            let json_str = content[..end].trim();
            if serde_json::from_str::<serde_json::Value>(json_str).is_ok() {
                return Ok(json_str.to_string());
            }
        }
    }

    // Try finding first { to last }
    if let (Some(start), Some(end)) = (response.find('{'), response.rfind('}')) {
        let json_str = &response[start..=end];
        if serde_json::from_str::<serde_json::Value>(json_str).is_ok() {
            return Ok(json_str.to_string());
        }
    }

    Err(format!(
        "Could not extract valid JSON from response: {}...",
        &response[..response.len().min(200)]
    ))
}

/// Check if a subtask's dependencies are all satisfied.
pub fn dependencies_satisfied(subtask: &SubTask, completed: &[String]) -> bool {
    subtask.depends_on.iter().all(|dep| completed.contains(dep))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> DecomposerConfig {
        DecomposerConfig {
            enabled: true,
            min_subtasks: 2,
            max_subtasks: 5,
            model_override: None,
        }
    }

    #[test]
    fn build_decomposition_prompt_includes_goal_and_min_max() {
        let config = test_config();
        let prompt = build_decomposition_prompt("Build a REST API", &config);
        assert!(prompt.contains("Build a REST API"));
        assert!(prompt.contains("2-5"));
    }

    #[test]
    fn parse_decomposition_response_valid_json() {
        let config = test_config();
        let response = r#"{
            "goal_summary": "Build a REST API",
            "subtasks": [
                {"id": "s1", "title": "Design schema", "description": "Create DB schema", "depends_on": [], "expected_output": "schema.sql"},
                {"id": "s2", "title": "Implement endpoints", "description": "Build routes", "depends_on": ["s1"], "expected_output": "routes.rs"}
            ]
        }"#;
        let plan = parse_decomposition_response("Build a REST API", response, &config).unwrap();
        assert_eq!(plan.goal_summary, "Build a REST API");
        assert_eq!(plan.subtasks.len(), 2);
        assert_eq!(plan.subtasks[0].id, "s1");
        assert_eq!(plan.subtasks[1].depends_on, vec!["s1".to_string()]);
    }

    #[test]
    fn parse_decomposition_response_with_markdown_fences() {
        let config = test_config();
        let response = "Here is the plan:\n```json\n{\n  \"goal_summary\": \"Do stuff\",\n  \"subtasks\": [\n    {\"id\": \"s1\", \"title\": \"A\", \"description\": \"a\", \"depends_on\": [], \"expected_output\": \"x\"},\n    {\"id\": \"s2\", \"title\": \"B\", \"description\": \"b\", \"depends_on\": [], \"expected_output\": \"y\"}\n  ]\n}\n```\nDone.";
        let plan = parse_decomposition_response("Do stuff", response, &config).unwrap();
        assert_eq!(plan.subtasks.len(), 2);
    }

    #[test]
    fn parse_decomposition_response_rejects_too_few_subtasks() {
        let config = test_config(); // min_subtasks = 2
        let response = r#"{
            "goal_summary": "Small task",
            "subtasks": [
                {"id": "s1", "title": "Only one", "description": "Just one task", "depends_on": [], "expected_output": "done"}
            ]
        }"#;
        let result = parse_decomposition_response("Small task", response, &config);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Too few subtasks"));
    }

    #[test]
    fn parse_decomposition_response_caps_at_max_subtasks() {
        let config = DecomposerConfig {
            enabled: true,
            min_subtasks: 1,
            max_subtasks: 3,
            model_override: None,
        };
        let response = r#"{
            "goal_summary": "Big task",
            "subtasks": [
                {"id": "s1", "title": "A", "description": "", "depends_on": [], "expected_output": ""},
                {"id": "s2", "title": "B", "description": "", "depends_on": [], "expected_output": ""},
                {"id": "s3", "title": "C", "description": "", "depends_on": [], "expected_output": ""},
                {"id": "s4", "title": "D", "description": "", "depends_on": [], "expected_output": ""},
                {"id": "s5", "title": "E", "description": "", "depends_on": [], "expected_output": ""}
            ]
        }"#;
        let plan = parse_decomposition_response("Big task", response, &config).unwrap();
        assert_eq!(plan.subtasks.len(), 3); // capped at max_subtasks
    }

    #[test]
    fn extract_json_from_response_with_raw_json() {
        let json = r#"{"key": "value"}"#;
        let result = extract_json_from_response(json).unwrap();
        assert_eq!(result, json);
    }

    #[test]
    fn extract_json_from_response_with_code_fences() {
        let response = "Some text\n```json\n{\"key\": \"value\"}\n```\nMore text";
        let result = extract_json_from_response(response).unwrap();
        assert_eq!(result, "{\"key\": \"value\"}");
    }

    #[test]
    fn extract_json_from_response_with_embedded_json() {
        let response = "Here is the result: {\"key\": \"value\"} and that's it.";
        let result = extract_json_from_response(response).unwrap();
        assert_eq!(result, "{\"key\": \"value\"}");
    }

    #[test]
    fn dependencies_satisfied_with_no_deps() {
        let subtask = SubTask {
            id: "s1".to_string(),
            index: 0,
            title: "Task".to_string(),
            description: "".to_string(),
            depends_on: vec![],
            expected_output: "".to_string(),
            workflow_id: None,
            status: SubTaskStatus::Pending,
        };
        assert!(dependencies_satisfied(&subtask, &[]));
    }

    #[test]
    fn dependencies_satisfied_with_met_deps() {
        let subtask = SubTask {
            id: "s2".to_string(),
            index: 1,
            title: "Task".to_string(),
            description: "".to_string(),
            depends_on: vec!["s1".to_string()],
            expected_output: "".to_string(),
            workflow_id: None,
            status: SubTaskStatus::Pending,
        };
        assert!(dependencies_satisfied(
            &subtask,
            &["s1".to_string()]
        ));
    }

    #[test]
    fn dependencies_satisfied_with_unmet_deps() {
        let subtask = SubTask {
            id: "s3".to_string(),
            index: 2,
            title: "Task".to_string(),
            description: "".to_string(),
            depends_on: vec!["s1".to_string(), "s2".to_string()],
            expected_output: "".to_string(),
            workflow_id: None,
            status: SubTaskStatus::Pending,
        };
        // Only s1 is completed, s2 is not
        assert!(!dependencies_satisfied(
            &subtask,
            &["s1".to_string()]
        ));
    }
}

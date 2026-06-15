//! Task decomposition for the orchestration loop planner agent.
//!
//! Decomposes a high-level goal into ordered subtasks with dependency tracking.
//! The AI prompt asks for a structured JSON plan; this module parses and validates it.

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use super::org_chart::OrgChartSeed;
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

// ============================================================================
// Approach-D org-chart planner (DESIGN step) — contract §3 shape
// ============================================================================
//
// The conductor's DESIGN bootstrap (Phase 4) reuses the decomposition idiom
// but emits the richer §3 org-chart seed shape (`phase`/`repo`/`emits_subtasks`
// in addition to `id`/`title`/`brief`/`depends_on`/`expected_output`). The
// shared `super::org_chart::splice_org_chart` then materializes these into
// ledger rows with `produced_by = None`. The AI call MUST route through
// `AiProvider::ClaudeCli` (subscription-billed) per the §3 CONSTRAINT — see
// `loop_engine::run_design_bootstrap`.

/// Build the org-chart DESIGN prompt. Like [`build_decomposition_prompt`] but
/// asks for the contract §3 shape so each seed carries `phase`, `repo`, and
/// `emits_subtasks` (progressive-elaboration flag). `phases` is the run's
/// ordered phase list (e.g. `["plan","implement","test"]`) — each seed's
/// `phase` MUST be one of these.
pub fn build_org_chart_prompt(goal: &str, phases: &[String], config: &DecomposerConfig) -> String {
    let phases_list = phases.join(", ");
    format!(
        "You are an orchestration planner producing an ORG-CHART of subtasks for a \
        multi-phase run. Decompose the goal into {}-{} dependency-ordered subtasks \
        spanning the phases: [{}].\n\
        \n\
        Goal: {}\n\
        \n\
        Requirements:\n\
        - Order subtasks by dependency (a subtask lists upstream ids in depends_on).\n\
        - Each subtask names the phase it belongs to (one of: {}).\n\
        - `repo` is the worktree target repository, or null for read-only tasks.\n\
        - `brief` is the FULL prompt/instructions handed to the worker for that subtask.\n\
        - `expected_output` is the success criterion / artifact contract.\n\
        - Set `emits_subtasks: true` ONLY for an *elaborating* subtask (e.g. a \
          comprehension/spec pass) whose own output will later produce more \
          subtasks; otherwise false. A run whose downstream work cannot be \
          enumerated until an upstream comprehension subtask finishes SHOULD mark \
          that comprehension subtask emits_subtasks: true and omit the not-yet-\
          knowable downstream subtasks (they will be added later).\n\
        \n\
        Respond with a JSON object:\n\
        {{\n\
          \"goal_summary\": \"one-line summary\",\n\
          \"subtasks\": [\n\
            {{\n\
              \"id\": \"st-001\",\n\
              \"title\": \"short title\",\n\
              \"brief\": \"full instructions for the worker\",\n\
              \"phase\": \"implement\",\n\
              \"depends_on\": [],\n\
              \"expected_output\": \"what success looks like\",\n\
              \"repo\": \"qontinui-runner\",\n\
              \"emits_subtasks\": false\n\
            }}\n\
          ]\n\
        }}",
        config.min_subtasks, config.max_subtasks, phases_list, goal, phases_list
    )
}

/// Parse an org-chart DESIGN response into [`OrgChartSeed`]s (contract §3
/// shape). Reuses [`extract_json_from_response`] for the markdown-fence-tolerant
/// extraction. Tolerant of the existing decomposer field name `description` as
/// an alias for `brief` (so the simpler [`build_decomposition_prompt`] output
/// also parses); fills sensible defaults for absent optional fields.
///
/// Enforces `config.min_subtasks` like [`parse_decomposition_response`] and
/// caps at `config.max_subtasks`. Returns `Err` on un-extractable JSON or a
/// missing `subtasks` array.
pub fn parse_org_chart_response(
    response: &str,
    phases: &[String],
    config: &DecomposerConfig,
) -> Result<Vec<OrgChartSeed>, String> {
    let json_str = extract_json_from_response(response)?;
    let parsed: serde_json::Value = serde_json::from_str(&json_str)
        .map_err(|e| format!("Failed to parse org-chart response as JSON: {}", e))?;

    let subtasks_arr = parsed
        .get("subtasks")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "Missing 'subtasks' array in org-chart response".to_string())?;

    if subtasks_arr.len() < config.min_subtasks as usize {
        return Err(format!(
            "Too few subtasks in org-chart: got {}, minimum {}",
            subtasks_arr.len(),
            config.min_subtasks
        ));
    }

    // Default phase = first declared phase, or "implement" if none supplied.
    let default_phase = phases
        .first()
        .cloned()
        .unwrap_or_else(|| "implement".to_string());

    let mut seeds: Vec<OrgChartSeed> = Vec::new();
    for (i, item) in subtasks_arr.iter().enumerate() {
        if i >= config.max_subtasks as usize {
            break;
        }
        let default_id = format!("st-{:03}", i + 1);
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
        // `brief` is canonical; accept `description` as the §3 alias.
        let brief = item
            .get("brief")
            .or_else(|| item.get("description"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let phase = item
            .get("phase")
            .and_then(|v| v.as_str())
            .unwrap_or(&default_phase)
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
        let repo = item
            .get("repo")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let emits_subtasks = item
            .get("emits_subtasks")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        seeds.push(OrgChartSeed {
            id,
            title,
            brief,
            phase,
            depends_on,
            expected_output,
            repo,
            emits_subtasks,
        });
    }

    info!(
        "Parsed org-chart into {} seeds (elaborators: {})",
        seeds.len(),
        seeds.iter().filter(|s| s.emits_subtasks).count()
    );
    Ok(seeds)
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

    // --- org-chart DESIGN prompt + parse (contract §3 shape) ---------------

    #[test]
    fn build_org_chart_prompt_includes_phases_and_emits_subtasks() {
        let config = test_config();
        let phases = vec![
            "plan".to_string(),
            "implement".to_string(),
            "test".to_string(),
        ];
        let prompt = build_org_chart_prompt("Regenerate the app", &phases, &config);
        assert!(prompt.contains("Regenerate the app"));
        assert!(prompt.contains("plan, implement, test"));
        assert!(prompt.contains("emits_subtasks"));
        assert!(prompt.contains("\"phase\""));
        assert!(prompt.contains("\"repo\""));
    }

    #[test]
    fn parse_org_chart_response_full_shape() {
        let config = test_config();
        let phases = vec!["plan".to_string(), "implement".to_string()];
        let response = r#"{
            "goal_summary": "Regenerate",
            "subtasks": [
                {"id": "st-001", "title": "Comprehend", "brief": "read the site",
                 "phase": "plan", "depends_on": [], "expected_output": "a spec",
                 "repo": null, "emits_subtasks": true},
                {"id": "st-002", "title": "Implement", "brief": "build it",
                 "phase": "implement", "depends_on": ["st-001"],
                 "expected_output": "a green PR", "repo": "qontinui-mobile"}
            ]
        }"#;
        let seeds = parse_org_chart_response(response, &phases, &config).unwrap();
        assert_eq!(seeds.len(), 2);
        assert_eq!(seeds[0].id, "st-001");
        assert!(
            seeds[0].emits_subtasks,
            "comprehension seed is an elaborator"
        );
        assert_eq!(seeds[0].phase, "plan");
        assert!(seeds[0].repo.is_none());
        assert_eq!(seeds[1].depends_on, vec!["st-001".to_string()]);
        assert_eq!(seeds[1].repo.as_deref(), Some("qontinui-mobile"));
        assert!(!seeds[1].emits_subtasks, "default false");
    }

    #[test]
    fn parse_org_chart_response_accepts_description_alias_and_defaults_phase() {
        // A response using the simpler decomposer `description` field + omitting
        // phase/repo/emits_subtasks must still parse (brief ← description,
        // phase ← first declared phase, emits_subtasks ← false).
        let config = test_config();
        let phases = vec!["implement".to_string(), "test".to_string()];
        let response = r#"{
            "goal_summary": "x",
            "subtasks": [
                {"id": "a", "title": "A", "description": "do a", "depends_on": [], "expected_output": "x"},
                {"id": "b", "title": "B", "description": "do b", "depends_on": ["a"], "expected_output": "y"}
            ]
        }"#;
        let seeds = parse_org_chart_response(response, &phases, &config).unwrap();
        assert_eq!(seeds.len(), 2);
        assert_eq!(seeds[0].brief, "do a", "description aliases brief");
        assert_eq!(
            seeds[0].phase, "implement",
            "phase defaults to first declared"
        );
        assert!(!seeds[0].emits_subtasks);
        assert!(seeds[0].repo.is_none());
    }

    #[test]
    fn parse_org_chart_response_rejects_too_few() {
        let config = test_config(); // min 2
        let phases = vec!["implement".to_string()];
        let response = r#"{"subtasks":[{"id":"a","title":"A","brief":"b","phase":"implement","depends_on":[],"expected_output":"x"}]}"#;
        assert!(parse_org_chart_response(response, &phases, &config).is_err());
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
        assert!(dependencies_satisfied(&subtask, &["s1".to_string()]));
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
        assert!(!dependencies_satisfied(&subtask, &["s1".to_string()]));
    }
}

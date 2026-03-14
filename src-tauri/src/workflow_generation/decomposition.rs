//! Hierarchical Workflow Decomposition
//!
//! Breaks complex workflow descriptions into sub-tasks, generates sub-workflows
//! independently, and composes them into a single unified workflow.
//!
//! Pipeline integration point:
//!   Discovery → Investigation → **Decomposition** → (per sub-task: Builder → Verify → Fix) → Compose → Hardener → Validate

use crate::ai_provider::AiResponse;
use crate::ai_router::TaskContext;
use crate::doctor::DoctorHandle;
use crate::workflow_generation::generator::extract_json_from_response;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Instant;
use tracing::{debug, info, warn};

// ============================================================================
// Public types
// ============================================================================

/// Result of analyzing description complexity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComplexityAnalysis {
    /// Estimated complexity score (0.0 = trivial, 1.0 = very complex)
    pub score: f32,
    /// Whether decomposition is recommended
    pub should_decompose: bool,
    /// Identified sub-tasks (if decomposition recommended)
    pub sub_tasks: Vec<SubTask>,
    /// Duration of analysis in ms
    pub duration_ms: u64,
}

/// A sub-task identified during complexity analysis.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubTask {
    /// Unique identifier for this sub-task
    pub id: String,
    /// Natural language description of the sub-task
    pub description: String,
    /// Dependencies on other sub-task IDs (ordering)
    pub depends_on: Vec<String>,
    /// Which workflow phase this sub-task primarily belongs to
    pub primary_phase: String,
    /// Data outputs this sub-task produces (for downstream consumption)
    pub outputs: Vec<String>,
}

/// Result of composing sub-workflows.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompositionResult {
    /// The composed workflow (steps merged in dependency order)
    pub workflow: serde_json::Value,
    /// Mapping from sub-task ID to step IDs in the composed workflow
    pub sub_task_step_map: HashMap<String, Vec<String>>,
    /// Whether composition was successful
    pub success: bool,
    /// Error message if composition failed
    pub error: Option<String>,
}

// ============================================================================
// Raw AI response shape (internal)
// ============================================================================

/// Shape of the JSON returned by the complexity-analysis AI call.
#[derive(Debug, Deserialize)]
struct AiComplexityResponse {
    score: f32,
    #[serde(default)]
    sub_tasks: Vec<SubTask>,
}

// ============================================================================
// Complexity Analysis
// ============================================================================

/// Analyze a workflow description to determine its complexity and, if complex
/// enough, identify sub-tasks suitable for independent generation.
///
/// Threshold: `score > 0.6` means `should_decompose = true`.
///
/// Uses the same AI calling pattern as [`crate::workflow_generation::investigator`].
pub fn analyze_complexity(
    description: &str,
    discovery_context: &str,
    doctor_handle: Option<&DoctorHandle>,
    model: Option<&str>,
    provider: Option<&str>,
) -> ComplexityAnalysis {
    let start = Instant::now();

    let prompt = build_complexity_prompt(description, discovery_context);
    let task_context = TaskContext::from_prompt(&prompt);
    let ai_result: AiResponse = crate::ai_provider::run_prompt_with_model_override(
        &prompt,
        &task_context,
        doctor_handle,
        model,
        provider,
        None,
        None,
        None,
        None,
    );

    let duration_ms = start.elapsed().as_millis() as u64;

    if !ai_result.success {
        warn!(
            "Complexity analysis failed: {}",
            ai_result.error.as_deref().unwrap_or("unknown error")
        );
        return ComplexityAnalysis {
            score: 0.0,
            should_decompose: false,
            sub_tasks: vec![],
            duration_ms,
        };
    }

    let output = ai_result.output.trim().to_string();

    if output.is_empty() || output.len() < 10 {
        warn!(
            "Complexity analysis produced too-short output ({} chars), falling back",
            output.len()
        );
        return ComplexityAnalysis {
            score: 0.0,
            should_decompose: false,
            sub_tasks: vec![],
            duration_ms,
        };
    }

    let json_text = extract_json_from_response(&output);

    match serde_json::from_str::<AiComplexityResponse>(&json_text) {
        Ok(parsed) => {
            let score = parsed.score.clamp(0.0, 1.0);
            let should_decompose = score > 0.6;

            let sub_tasks = if should_decompose {
                parsed.sub_tasks
            } else {
                vec![]
            };

            info!(
                "Complexity analysis completed in {}ms: score={:.2}, should_decompose={}, sub_tasks={}",
                duration_ms,
                score,
                should_decompose,
                sub_tasks.len()
            );
            debug!(
                "Sub-task IDs: {:?}",
                sub_tasks.iter().map(|s| &s.id).collect::<Vec<_>>()
            );

            ComplexityAnalysis {
                score,
                should_decompose,
                sub_tasks,
                duration_ms,
            }
        }
        Err(e) => {
            warn!("Failed to parse complexity analysis output as JSON: {}", e);
            debug!(
                "Complexity analysis raw output: {}",
                &output[..output.len().min(500)]
            );
            ComplexityAnalysis {
                score: 0.0,
                should_decompose: false,
                sub_tasks: vec![],
                duration_ms,
            }
        }
    }
}

// ============================================================================
// Sub-Workflow Composition
// ============================================================================

/// Compose multiple sub-workflows into a single unified workflow.
///
/// Takes a list of `(sub_task_id, workflow_json)` pairs and merges their steps
/// into one workflow, respecting the dependency order defined by `sub_tasks`.
///
/// Step IDs are prefixed with their sub-task ID to avoid collisions, and
/// `depends_on` references are updated to use the prefixed IDs. Steps are
/// deduplicated across sub-workflows by command (for setup steps).
pub fn compose_sub_workflows(
    sub_task_workflows: &[(String, serde_json::Value)],
    sub_tasks: &[SubTask],
) -> CompositionResult {
    if sub_task_workflows.is_empty() {
        return CompositionResult {
            workflow: serde_json::json!({}),
            sub_task_step_map: HashMap::new(),
            success: false,
            error: Some("No sub-task workflows provided".to_string()),
        };
    }

    // Build a lookup from sub-task ID → SubTask for dependency info.
    let _sub_task_map: HashMap<&str, &SubTask> =
        sub_tasks.iter().map(|st| (st.id.as_str(), st)).collect();

    // Determine topological order based on dependencies.
    let ordered_ids = match topological_sort(sub_tasks) {
        Ok(ids) => ids,
        Err(e) => {
            warn!("Topological sort of sub-tasks failed: {}", e);
            return CompositionResult {
                workflow: serde_json::json!({}),
                sub_task_step_map: HashMap::new(),
                success: false,
                error: Some(format!("Dependency cycle detected: {}", e)),
            };
        }
    };

    // Build workflow-id lookup for quick access.
    let workflow_map: HashMap<&str, &serde_json::Value> = sub_task_workflows
        .iter()
        .map(|(id, wf)| (id.as_str(), wf))
        .collect();

    let mut composed_setup_steps: Vec<serde_json::Value> = Vec::new();
    let mut composed_verification_steps: Vec<serde_json::Value> = Vec::new();
    let mut composed_agentic_steps: Vec<serde_json::Value> = Vec::new();
    let mut sub_task_step_map: HashMap<String, Vec<String>> = HashMap::new();

    // Track setup commands we have already seen (for deduplication).
    let mut seen_setup_commands: std::collections::HashSet<String> =
        std::collections::HashSet::new();

    for sub_task_id in &ordered_ids {
        let workflow_json = match workflow_map.get(sub_task_id.as_str()) {
            Some(wf) => *wf,
            None => {
                warn!("No workflow found for sub-task '{}'", sub_task_id);
                continue;
            }
        };

        let mut step_ids_for_task: Vec<String> = Vec::new();

        // ── Setup steps ──────────────────────────────────────────────────
        if let Some(steps) = workflow_json.get("setup_steps").and_then(|v| v.as_array()) {
            for step in steps {
                let command = step.get("command").and_then(|v| v.as_str()).unwrap_or("");
                // Deduplicate setup steps by command string.
                if !command.is_empty() && !seen_setup_commands.insert(command.to_string()) {
                    debug!(
                        "Deduplicating setup step with command '{}' from sub-task '{}'",
                        command, sub_task_id
                    );
                    continue;
                }

                let prefixed = prefix_step(step, sub_task_id);
                if let Some(id) = prefixed.get("id").and_then(|v| v.as_str()) {
                    step_ids_for_task.push(id.to_string());
                }
                composed_setup_steps.push(prefixed);
            }
        }

        // ── Verification steps ───────────────────────────────────────────
        if let Some(steps) = workflow_json
            .get("verification_steps")
            .and_then(|v| v.as_array())
        {
            for step in steps {
                let prefixed = prefix_step(step, sub_task_id);
                if let Some(id) = prefixed.get("id").and_then(|v| v.as_str()) {
                    step_ids_for_task.push(id.to_string());
                }
                composed_verification_steps.push(prefixed);
            }
        }

        // ── Agentic steps ────────────────────────────────────────────────
        if let Some(steps) = workflow_json
            .get("agentic_steps")
            .and_then(|v| v.as_array())
        {
            for step in steps {
                let prefixed = prefix_step(step, sub_task_id);
                if let Some(id) = prefixed.get("id").and_then(|v| v.as_str()) {
                    step_ids_for_task.push(id.to_string());
                }
                composed_agentic_steps.push(prefixed);
            }
        }

        sub_task_step_map.insert(sub_task_id.clone(), step_ids_for_task);
    }

    // Merge any top-level workflow metadata from the first sub-workflow.
    let first_wf = sub_task_workflows
        .first()
        .map(|(_, wf)| wf.clone())
        .unwrap_or(serde_json::json!({}));

    let mut composed = if let Some(obj) = first_wf.as_object() {
        let mut base = obj.clone();
        // Remove step arrays; we'll insert the composed ones.
        base.remove("setup_steps");
        base.remove("verification_steps");
        base.remove("agentic_steps");
        serde_json::Value::Object(base)
    } else {
        serde_json::json!({})
    };

    composed["setup_steps"] = serde_json::Value::Array(composed_setup_steps);
    composed["verification_steps"] = serde_json::Value::Array(composed_verification_steps);
    composed["agentic_steps"] = serde_json::Value::Array(composed_agentic_steps);

    info!(
        "Composed workflow from {} sub-tasks: {} setup, {} verification, {} agentic steps",
        ordered_ids.len(),
        composed["setup_steps"].as_array().map_or(0, |a| a.len()),
        composed["verification_steps"]
            .as_array()
            .map_or(0, |a| a.len()),
        composed["agentic_steps"].as_array().map_or(0, |a| a.len()),
    );

    CompositionResult {
        workflow: composed,
        sub_task_step_map,
        success: true,
        error: None,
    }
}

// ============================================================================
// Internal helpers
// ============================================================================

/// Prefix a step's `id` and update its `depends_on` references with the
/// sub-task ID to avoid collisions across sub-workflows.
fn prefix_step(step: &serde_json::Value, sub_task_id: &str) -> serde_json::Value {
    let mut step = step.clone();

    if let Some(obj) = step.as_object_mut() {
        // Prefix the step ID.
        if let Some(id_val) = obj.get("id").and_then(|v| v.as_str()) {
            obj.insert(
                "id".to_string(),
                serde_json::Value::String(format!("{}_{}", sub_task_id, id_val)),
            );
        }

        // Prefix depends_on references.
        if let Some(deps) = obj.get("depends_on").and_then(|v| v.as_array()) {
            let prefixed_deps: Vec<serde_json::Value> = deps
                .iter()
                .map(|d| {
                    if let Some(dep_str) = d.as_str() {
                        serde_json::Value::String(format!("{}_{}", sub_task_id, dep_str))
                    } else {
                        d.clone()
                    }
                })
                .collect();
            obj.insert(
                "depends_on".to_string(),
                serde_json::Value::Array(prefixed_deps),
            );
        }
    }

    step
}

/// Topological sort of sub-tasks based on their `depends_on` fields.
/// Returns sub-task IDs in dependency order (dependencies first).
fn topological_sort(sub_tasks: &[SubTask]) -> Result<Vec<String>, String> {
    let ids: std::collections::HashSet<&str> = sub_tasks.iter().map(|s| s.id.as_str()).collect();
    let mut in_degree: HashMap<&str, usize> = HashMap::new();
    let mut adjacency: HashMap<&str, Vec<&str>> = HashMap::new();

    for st in sub_tasks {
        in_degree.entry(st.id.as_str()).or_insert(0);
        adjacency.entry(st.id.as_str()).or_default();
        for dep in &st.depends_on {
            if ids.contains(dep.as_str()) {
                adjacency.entry(dep.as_str()).or_default().push(&st.id);
                *in_degree.entry(st.id.as_str()).or_insert(0) += 1;
            }
        }
    }

    let mut queue: std::collections::VecDeque<&str> = in_degree
        .iter()
        .filter(|(_, &deg)| deg == 0)
        .map(|(&id, _)| id)
        .collect();

    let mut result: Vec<String> = Vec::new();

    while let Some(node) = queue.pop_front() {
        result.push(node.to_string());
        if let Some(neighbors) = adjacency.get(node) {
            for &neighbor in neighbors {
                if let Some(deg) = in_degree.get_mut(neighbor) {
                    *deg -= 1;
                    if *deg == 0 {
                        queue.push_back(neighbor);
                    }
                }
            }
        }
    }

    if result.len() != sub_tasks.len() {
        return Err("cycle in sub-task dependencies".to_string());
    }

    Ok(result)
}

// ============================================================================
// Prompt Builder
// ============================================================================

/// Build the AI prompt for complexity analysis.
fn build_complexity_prompt(description: &str, discovery_context: &str) -> String {
    format!(
        r#"You are a workflow complexity analysis AI. Your job is to analyze a task description and determine whether it is complex enough to benefit from being decomposed into independent sub-tasks.

## Task Description

{description}

## Project Discovery Context

{discovery}

## Your Task

Analyze the task description and determine:

1. **Complexity score** (0.0 to 1.0):
   - 0.0–0.3: Simple, single-concern task (e.g., "fix a typo", "add a CSS class")
   - 0.3–0.6: Moderate task with a few related steps (e.g., "add a form with validation")
   - 0.6–0.8: Complex multi-concern task (e.g., "add authentication with login page, API endpoints, and session management")
   - 0.8–1.0: Very complex task spanning multiple systems (e.g., "build a complete CRUD feature with database, API, UI, and tests")

2. **Sub-tasks** (if complexity > 0.6): Identify distinct, independently-generatable sub-tasks. Each sub-task should:
   - Be self-contained enough to generate a workflow for independently
   - Have clear inputs/outputs for composition with other sub-tasks
   - Map to a primary workflow phase (setup, verification, agentic)
   - Declare dependencies on other sub-tasks if ordering matters

Consider these factors for complexity:
- Number of distinct concerns (UI, API, database, infrastructure)
- Number of expected workflow steps (>10 suggests decomposition)
- Multi-phase requirements (setup + multiple agentic tasks + verification)
- Cross-cutting concerns (auth, logging, error handling that span multiple components)

## Output Format

Return ONLY valid JSON matching this structure. No markdown code blocks, no explanations.

{{
  "score": 0.7,
  "sub_tasks": [
    {{
      "id": "setup-db",
      "description": "Set up database connection and verify schema",
      "depends_on": [],
      "primary_phase": "setup",
      "outputs": ["db_connection_string"]
    }},
    {{
      "id": "build-api",
      "description": "Implement API endpoints for CRUD operations",
      "depends_on": ["setup-db"],
      "primary_phase": "agentic",
      "outputs": ["api_endpoints"]
    }}
  ]
}}"#,
        description = description,
        discovery = if discovery_context.is_empty() {
            "(No discovery data available)"
        } else {
            discovery_context
        },
    )
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_sub_tasks() -> Vec<SubTask> {
        vec![
            SubTask {
                id: "setup-db".to_string(),
                description: "Set up database".to_string(),
                depends_on: vec![],
                primary_phase: "setup".to_string(),
                outputs: vec!["db_url".to_string()],
            },
            SubTask {
                id: "build-api".to_string(),
                description: "Build API endpoints".to_string(),
                depends_on: vec!["setup-db".to_string()],
                primary_phase: "agentic".to_string(),
                outputs: vec!["api_endpoints".to_string()],
            },
            SubTask {
                id: "build-ui".to_string(),
                description: "Build UI components".to_string(),
                depends_on: vec!["build-api".to_string()],
                primary_phase: "agentic".to_string(),
                outputs: vec!["ui_components".to_string()],
            },
        ]
    }

    fn sample_workflow(prefix: &str) -> serde_json::Value {
        serde_json::json!({
            "name": format!("{} workflow", prefix),
            "setup_steps": [
                {
                    "id": "install-deps",
                    "command": "npm install",
                    "description": "Install dependencies"
                }
            ],
            "verification_steps": [
                {
                    "id": "check-build",
                    "command": format!("{} build check", prefix),
                    "description": "Verify build",
                    "depends_on": ["install-deps"]
                }
            ],
            "agentic_steps": [
                {
                    "id": "implement",
                    "prompt": format!("Implement {}", prefix),
                    "description": format!("Implement {} logic", prefix)
                }
            ]
        })
    }

    #[test]
    fn test_topological_sort_simple() {
        let tasks = sample_sub_tasks();
        let order = topological_sort(&tasks).unwrap();
        assert_eq!(order.len(), 3);
        // setup-db must come before build-api, build-api before build-ui
        let pos = |id: &str| order.iter().position(|x| x == id).unwrap();
        assert!(pos("setup-db") < pos("build-api"));
        assert!(pos("build-api") < pos("build-ui"));
    }

    #[test]
    fn test_topological_sort_cycle() {
        let tasks = vec![
            SubTask {
                id: "a".to_string(),
                description: "Task A".to_string(),
                depends_on: vec!["b".to_string()],
                primary_phase: "setup".to_string(),
                outputs: vec![],
            },
            SubTask {
                id: "b".to_string(),
                description: "Task B".to_string(),
                depends_on: vec!["a".to_string()],
                primary_phase: "setup".to_string(),
                outputs: vec![],
            },
        ];
        let result = topological_sort(&tasks);
        assert!(result.is_err());
    }

    #[test]
    fn test_topological_sort_no_deps() {
        let tasks = vec![
            SubTask {
                id: "x".to_string(),
                description: "X".to_string(),
                depends_on: vec![],
                primary_phase: "setup".to_string(),
                outputs: vec![],
            },
            SubTask {
                id: "y".to_string(),
                description: "Y".to_string(),
                depends_on: vec![],
                primary_phase: "setup".to_string(),
                outputs: vec![],
            },
        ];
        let order = topological_sort(&tasks).unwrap();
        assert_eq!(order.len(), 2);
    }

    #[test]
    fn test_compose_sub_workflows_basic() {
        let tasks = sample_sub_tasks();
        let sub_task_workflows = vec![
            ("setup-db".to_string(), sample_workflow("db")),
            ("build-api".to_string(), sample_workflow("api")),
            ("build-ui".to_string(), sample_workflow("ui")),
        ];

        let result = compose_sub_workflows(&sub_task_workflows, &tasks);
        assert!(result.success);
        assert!(result.error.is_none());

        // Setup steps should be deduplicated (all three have "npm install")
        let setup = result.workflow["setup_steps"].as_array().unwrap();
        assert_eq!(setup.len(), 1, "npm install should be deduplicated");

        // Verification and agentic steps should all be present (3 each)
        let verification = result.workflow["verification_steps"].as_array().unwrap();
        assert_eq!(verification.len(), 3);

        let agentic = result.workflow["agentic_steps"].as_array().unwrap();
        assert_eq!(agentic.len(), 3);

        // Step IDs should be prefixed
        assert_eq!(setup[0]["id"].as_str().unwrap(), "setup-db_install-deps");
        assert_eq!(
            verification[0]["id"].as_str().unwrap(),
            "setup-db_check-build"
        );

        // sub_task_step_map should have entries for all sub-tasks
        assert_eq!(result.sub_task_step_map.len(), 3);
        assert!(result.sub_task_step_map.contains_key("setup-db"));
        assert!(result.sub_task_step_map.contains_key("build-api"));
        assert!(result.sub_task_step_map.contains_key("build-ui"));
    }

    #[test]
    fn test_compose_sub_workflows_empty() {
        let result = compose_sub_workflows(&[], &[]);
        assert!(!result.success);
        assert!(result.error.is_some());
    }

    #[test]
    fn test_prefix_step() {
        let step = serde_json::json!({
            "id": "my-step",
            "command": "echo hello",
            "depends_on": ["prev-step"]
        });
        let prefixed = prefix_step(&step, "task-1");
        assert_eq!(prefixed["id"].as_str().unwrap(), "task-1_my-step");
        let deps = prefixed["depends_on"].as_array().unwrap();
        assert_eq!(deps[0].as_str().unwrap(), "task-1_prev-step");
    }

    #[test]
    fn test_complexity_analysis_serde_roundtrip() {
        let analysis = ComplexityAnalysis {
            score: 0.75,
            should_decompose: true,
            sub_tasks: sample_sub_tasks(),
            duration_ms: 1234,
        };
        let json = serde_json::to_string(&analysis).unwrap();
        let parsed: ComplexityAnalysis = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.score, 0.75);
        assert!(parsed.should_decompose);
        assert_eq!(parsed.sub_tasks.len(), 3);
        assert_eq!(parsed.sub_tasks[0].id, "setup-db");
    }

    #[test]
    fn test_composition_result_serde() {
        let result = CompositionResult {
            workflow: serde_json::json!({"name": "test"}),
            sub_task_step_map: {
                let mut m = HashMap::new();
                m.insert("t1".to_string(), vec!["t1_s1".to_string()]);
                m
            },
            success: true,
            error: None,
        };
        let json = serde_json::to_string(&result).unwrap();
        let parsed: CompositionResult = serde_json::from_str(&json).unwrap();
        assert!(parsed.success);
        assert_eq!(parsed.sub_task_step_map["t1"], vec!["t1_s1"]);
    }
}

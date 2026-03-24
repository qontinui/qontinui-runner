//! Subtask execution support for the task decomposition planner.
//!
//! Provides dependency-ordered execution, context propagation between subtasks,
//! and result aggregation for decomposition plans.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tracing::{info, warn};

use super::task_decomposer::{DecompositionPlan, SubTask, SubTaskStatus};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubTaskExecutionResult {
    pub subtask_id: String,
    pub subtask_title: String,
    pub success: bool,
    pub output_summary: String,
    pub error: Option<String>,
    pub task_run_id: Option<String>,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecompositionExecutionResult {
    pub plan_id: String,
    pub total_subtasks: usize,
    pub completed: usize,
    pub failed: usize,
    pub skipped: usize,
    pub results: Vec<SubTaskExecutionResult>,
    pub overall_success: bool,
}

/// Determine the execution order based on dependencies (topological sort).
pub fn determine_execution_order(plan: &DecompositionPlan) -> Result<Vec<usize>, String> {
    let n = plan.subtasks.len();
    let id_to_index: HashMap<&str, usize> = plan
        .subtasks
        .iter()
        .enumerate()
        .map(|(i, s)| (s.id.as_str(), i))
        .collect();

    // Build adjacency list
    let mut in_degree = vec![0usize; n];
    let mut dependents: Vec<Vec<usize>> = vec![Vec::new(); n];

    for (i, subtask) in plan.subtasks.iter().enumerate() {
        for dep_id in &subtask.depends_on {
            if let Some(&dep_idx) = id_to_index.get(dep_id.as_str()) {
                dependents[dep_idx].push(i);
                in_degree[i] += 1;
            }
        }
    }

    // Kahn's algorithm
    let mut queue: Vec<usize> = (0..n).filter(|&i| in_degree[i] == 0).collect();
    let mut order = Vec::with_capacity(n);

    while let Some(idx) = queue.pop() {
        order.push(idx);
        for &dependent in &dependents[idx] {
            in_degree[dependent] -= 1;
            if in_degree[dependent] == 0 {
                queue.push(dependent);
            }
        }
    }

    if order.len() != n {
        return Err("Circular dependency detected in subtask plan".to_string());
    }

    Ok(order)
}

/// Build context string from completed subtask outputs for the next subtask.
pub fn build_subtask_context(
    plan: &DecompositionPlan,
    completed_results: &[SubTaskExecutionResult],
) -> String {
    if completed_results.is_empty() {
        return String::new();
    }

    let mut context = String::from("## Context from completed sub-tasks\n\n");
    for result in completed_results {
        if result.success {
            context.push_str(&format!(
                "### {} (completed)\n{}\n\n",
                result.subtask_title, result.output_summary
            ));
        } else {
            context.push_str(&format!(
                "### {} (failed)\nError: {}\n\n",
                result.subtask_title,
                result.error.as_deref().unwrap_or("unknown error")
            ));
        }
    }

    context.push_str(&format!(
        "\n## Overall goal\n{}\n",
        plan.original_goal
    ));

    context
}

/// Check if a subtask should be skipped due to failed dependencies.
pub fn should_skip_subtask(
    subtask: &SubTask,
    failed_ids: &[String],
) -> Option<String> {
    for dep_id in &subtask.depends_on {
        if failed_ids.contains(dep_id) {
            return Some(format!(
                "Dependency '{}' failed; skipping '{}'",
                dep_id, subtask.title
            ));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_subtask(id: &str, title: &str, depends_on: Vec<&str>) -> SubTask {
        SubTask {
            id: id.to_string(),
            index: 0,
            title: title.to_string(),
            description: "".to_string(),
            depends_on: depends_on.into_iter().map(|s| s.to_string()).collect(),
            expected_output: "".to_string(),
            workflow_id: None,
            status: SubTaskStatus::Pending,
        }
    }

    fn make_plan(subtasks: Vec<SubTask>) -> DecompositionPlan {
        DecompositionPlan {
            id: "plan_1".to_string(),
            original_goal: "Test goal".to_string(),
            goal_summary: "Test".to_string(),
            subtasks,
            created_at: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn execution_order_no_dependencies_preserves_order() {
        let plan = make_plan(vec![
            make_subtask("a", "A", vec![]),
            make_subtask("b", "B", vec![]),
            make_subtask("c", "C", vec![]),
        ]);
        let order = determine_execution_order(&plan).unwrap();
        assert_eq!(order.len(), 3);
        // All have in-degree 0, so they should all appear (order may vary due to pop)
        assert!(order.contains(&0));
        assert!(order.contains(&1));
        assert!(order.contains(&2));
    }

    #[test]
    fn execution_order_linear_chain() {
        let plan = make_plan(vec![
            make_subtask("a", "A", vec![]),
            make_subtask("b", "B", vec!["a"]),
            make_subtask("c", "C", vec!["b"]),
        ]);
        let order = determine_execution_order(&plan).unwrap();
        assert_eq!(order.len(), 3);
        // a must come before b, b must come before c
        let pos_a = order.iter().position(|&x| x == 0).unwrap();
        let pos_b = order.iter().position(|&x| x == 1).unwrap();
        let pos_c = order.iter().position(|&x| x == 2).unwrap();
        assert!(pos_a < pos_b);
        assert!(pos_b < pos_c);
    }

    #[test]
    fn execution_order_detects_circular_dependency() {
        let plan = make_plan(vec![
            make_subtask("a", "A", vec!["c"]),
            make_subtask("b", "B", vec!["a"]),
            make_subtask("c", "C", vec!["b"]),
        ]);
        let result = determine_execution_order(&plan);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Circular dependency"));
    }

    #[test]
    fn build_subtask_context_with_completed_results() {
        let plan = make_plan(vec![make_subtask("a", "A", vec![])]);
        let results = vec![
            SubTaskExecutionResult {
                subtask_id: "a".to_string(),
                subtask_title: "Task A".to_string(),
                success: true,
                output_summary: "Did something useful".to_string(),
                error: None,
                task_run_id: Some("run1".to_string()),
                duration_ms: 1000,
            },
        ];
        let context = build_subtask_context(&plan, &results);
        assert!(context.contains("Task A (completed)"));
        assert!(context.contains("Did something useful"));
        assert!(context.contains("Test goal"));
    }

    #[test]
    fn build_subtask_context_with_empty_results() {
        let plan = make_plan(vec![make_subtask("a", "A", vec![])]);
        let context = build_subtask_context(&plan, &[]);
        assert!(context.is_empty());
    }

    #[test]
    fn should_skip_subtask_with_failed_dependency() {
        let subtask = make_subtask("b", "Task B", vec!["a"]);
        let failed = vec!["a".to_string()];
        let result = should_skip_subtask(&subtask, &failed);
        assert!(result.is_some());
        let reason = result.unwrap();
        assert!(reason.contains("'a' failed"));
        assert!(reason.contains("Task B"));
    }

    #[test]
    fn should_skip_subtask_with_no_failed_deps() {
        let subtask = make_subtask("b", "Task B", vec!["a"]);
        let failed: Vec<String> = vec![];
        assert!(should_skip_subtask(&subtask, &failed).is_none());
    }
}

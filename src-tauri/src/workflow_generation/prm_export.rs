//! PRM Training Data Export
//!
//! Exports step-level training data from historical workflow runs for
//! Process Reward Model training. Uses the runner's own execution results
//! (pass/fail, fixer diffs, iteration counts) as ground-truth labels —
//! the FoVer insight that the runner IS a formal verifier.

use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use tracing::{debug, info};

// ============================================================================
// Types
// ============================================================================

/// A single step definition as it appeared in the workflow.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepDefinition {
    pub step_type: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub check_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected: Option<String>,
    #[serde(default)]
    pub parameters: serde_json::Value,
}

/// Execution result for a single step.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionResult {
    Passed,
    Failed { error: String },
    Skipped,
    Timeout,
}

/// Final outcome of the workflow containing this step.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinalOutcome {
    WorkflowPassed,
    WorkflowFailed,
    StepFixedLater,
}

/// One training example for PRM training.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrmTrainingExample {
    pub run_id: String,
    pub workflow_id: Option<String>,
    pub step_index: usize,
    pub step_definition: StepDefinition,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub criterion: Option<CriterionContext>,
    pub workflow_context: String,
    pub execution_result: ExecutionResult,
    pub iteration: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fixer_diff: Option<String>,
    pub final_outcome: FinalOutcome,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
}

/// Context about the acceptance criterion this step verifies.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CriterionContext {
    pub id: String,
    pub description: String,
    pub method: String,
    pub priority: String,
}

/// Statistics from a PRM data export.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrmExportStats {
    pub total_examples: usize,
    pub passed_count: usize,
    pub failed_count: usize,
    pub fixed_count: usize,
    pub runs_processed: usize,
    pub domains: Vec<String>,
}

// ============================================================================
// Export Logic
// ============================================================================

/// Export PRM training data from historical workflow runs.
///
/// Joins task_runs with generation_pipeline_artifacts to extract:
/// - Step definitions from the final workflow JSON
/// - Pass/fail labels from verification_iterations snapshots
/// - Fixer diffs from fixer_snapshots
/// - Acceptance criteria from specification_criteria
pub fn export_prm_training_data(
    conn: &Connection,
    min_runs: usize,
) -> Result<(Vec<PrmTrainingExample>, PrmExportStats), String> {
    info!("Exporting PRM training data (min_runs={})", min_runs);

    // Query completed task runs that have pipeline artifacts with verification data
    let mut stmt = conn
        .prepare(
            r#"
            SELECT
                tr.id AS run_id,
                tr.workflow_id,
                tr.task_name,
                tr.status AS run_status,
                tr.verification_passed,
                gpa.final_json,
                gpa.verification_iterations,
                gpa.fixer_snapshots,
                gpa.specification_criteria,
                gpa.category
            FROM task_runs tr
            INNER JOIN generation_pipeline_artifacts gpa
                ON gpa.task_run_id = tr.id
                OR (gpa.task_run_id IS NULL AND gpa.workflow_id = tr.workflow_id)
            WHERE tr.status IN ('complete', 'failed')
              AND gpa.final_json IS NOT NULL
            ORDER BY tr.created_at DESC
            "#,
        )
        .map_err(|e| format!("Failed to prepare PRM export query: {e}"))?;

    let rows = stmt
        .query_map([], |row| {
            Ok(RawExportRow {
                run_id: row.get(0)?,
                workflow_id: row.get(1)?,
                task_name: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                run_status: row.get(3)?,
                verification_passed: row.get::<_, Option<i32>>(4)?.unwrap_or(0),
                final_json: row.get(5)?,
                verification_iterations: row.get(6)?,
                fixer_snapshots: row.get(7)?,
                specification_criteria: row.get(8)?,
                category: row.get(9)?,
            })
        })
        .map_err(|e| format!("Failed to query PRM export data: {e}"))?;

    let mut all_examples = Vec::new();
    let mut runs_processed = 0;
    let mut domains_set = std::collections::HashSet::new();

    for row_result in rows {
        let row = row_result.map_err(|e| format!("Row error: {e}"))?;
        runs_processed += 1;

        let domain = row.category.clone();
        if let Some(ref d) = domain {
            domains_set.insert(d.clone());
        }

        let examples = extract_examples_from_row(&row);
        all_examples.extend(examples);
    }

    if runs_processed < min_runs {
        return Err(format!(
            "Only {runs_processed} runs found, need at least {min_runs}"
        ));
    }

    let passed_count = all_examples
        .iter()
        .filter(|e| matches!(e.execution_result, ExecutionResult::Passed))
        .count();
    let failed_count = all_examples
        .iter()
        .filter(|e| matches!(e.execution_result, ExecutionResult::Failed { .. }))
        .count();
    let fixed_count = all_examples
        .iter()
        .filter(|e| matches!(e.final_outcome, FinalOutcome::StepFixedLater))
        .count();

    let stats = PrmExportStats {
        total_examples: all_examples.len(),
        passed_count,
        failed_count,
        fixed_count,
        runs_processed,
        domains: domains_set.into_iter().collect(),
    };

    info!(
        "PRM export: {} examples from {} runs ({} passed, {} failed, {} fixed)",
        stats.total_examples, stats.runs_processed, stats.passed_count, stats.failed_count, stats.fixed_count
    );

    Ok((all_examples, stats))
}

/// Lightweight stats query — counts runs and domains without materializing examples.
pub fn export_prm_stats_only(conn: &Connection) -> Result<PrmExportStats, String> {
    let mut stmt = conn
        .prepare(
            r#"
            SELECT
                COUNT(DISTINCT tr.id) AS run_count,
                COUNT(*) AS artifact_count,
                SUM(CASE WHEN tr.verification_passed = 1 AND tr.status = 'complete' THEN 1 ELSE 0 END) AS passed_runs,
                SUM(CASE WHEN tr.status = 'failed' THEN 1 ELSE 0 END) AS failed_runs,
                GROUP_CONCAT(DISTINCT gpa.category) AS domains
            FROM task_runs tr
            INNER JOIN generation_pipeline_artifacts gpa
                ON gpa.task_run_id = tr.id
                OR (gpa.task_run_id IS NULL AND gpa.workflow_id = tr.workflow_id)
            WHERE tr.status IN ('complete', 'failed')
              AND gpa.final_json IS NOT NULL
            "#,
        )
        .map_err(|e| format!("Failed to prepare PRM stats query: {e}"))?;

    let result = stmt
        .query_row([], |row| {
            let run_count: i64 = row.get(0)?;
            let artifact_count: i64 = row.get(1)?;
            let passed_runs: i64 = row.get(2)?;
            let failed_runs: i64 = row.get(3)?;
            let domains_csv: Option<String> = row.get(4)?;

            Ok(PrmExportStats {
                total_examples: artifact_count as usize, // Approximate (1 artifact ≈ N steps)
                passed_count: passed_runs as usize,
                failed_count: failed_runs as usize,
                fixed_count: 0, // Not available without full parse
                runs_processed: run_count as usize,
                domains: domains_csv
                    .map(|csv| csv.split(',').map(String::from).collect())
                    .unwrap_or_default(),
            })
        })
        .map_err(|e| format!("PRM stats query failed: {e}"))?;

    Ok(result)
}

// ============================================================================
// Internal helpers
// ============================================================================

struct RawExportRow {
    run_id: String,
    workflow_id: Option<String>,
    task_name: String,
    run_status: String,
    verification_passed: i32,
    final_json: Option<String>,
    verification_iterations: Option<String>,
    fixer_snapshots: Option<String>,
    specification_criteria: Option<String>,
    category: Option<String>,
}

/// Extract PRM training examples from a single task_run + pipeline_artifact row.
fn extract_examples_from_row(row: &RawExportRow) -> Vec<PrmTrainingExample> {
    let mut examples = Vec::new();

    // Parse the final workflow JSON to get step definitions
    let steps = match parse_steps_from_workflow(&row.final_json) {
        Some(s) => s,
        None => return examples,
    };

    // Parse verification iterations (JSON array of per-iteration results)
    let iterations = parse_verification_iterations(&row.verification_iterations);

    // Parse fixer snapshots to get diffs
    let fixer_diffs = parse_fixer_diffs(&row.fixer_snapshots);

    // Parse acceptance criteria
    let criteria = parse_criteria(&row.specification_criteria);

    let workflow_passed = row.run_status == "complete" && row.verification_passed == 1;

    for (step_idx, step_def) in steps.into_iter().enumerate() {
        // Find the iteration history for this step
        let step_iterations = iterations
            .iter()
            .filter(|it| it.step_index == step_idx)
            .collect::<Vec<_>>();

        // If no iteration data, create a single example from the final outcome
        if step_iterations.is_empty() {
            let execution_result = if workflow_passed {
                ExecutionResult::Passed
            } else {
                ExecutionResult::Failed {
                    error: "workflow failed".to_string(),
                }
            };

            let final_outcome = if workflow_passed {
                FinalOutcome::WorkflowPassed
            } else {
                FinalOutcome::WorkflowFailed
            };

            // Try to match a criterion by step name
            let criterion = find_matching_criterion(&step_def, &criteria);

            examples.push(PrmTrainingExample {
                run_id: row.run_id.clone(),
                workflow_id: row.workflow_id.clone(),
                step_index: step_idx,
                step_definition: step_def,
                criterion,
                workflow_context: row.task_name.clone(),
                execution_result,
                iteration: 0,
                fixer_diff: None,
                final_outcome,
                domain: row.category.clone(),
            });
            continue;
        }

        // Create one example per iteration for this step
        for iter_data in &step_iterations {
            let execution_result = if iter_data.passed {
                ExecutionResult::Passed
            } else {
                ExecutionResult::Failed {
                    error: iter_data
                        .error
                        .clone()
                        .unwrap_or_else(|| "verification failed".to_string()),
                }
            };

            let final_outcome = if iter_data.passed && workflow_passed {
                FinalOutcome::WorkflowPassed
            } else if !iter_data.passed && step_iterations.iter().any(|s| s.passed) {
                FinalOutcome::StepFixedLater
            } else if workflow_passed {
                FinalOutcome::WorkflowPassed
            } else {
                FinalOutcome::WorkflowFailed
            };

            let fixer_diff = if !iter_data.passed {
                fixer_diffs.get(iter_data.iteration as usize).cloned()
            } else {
                None
            };

            let criterion = find_matching_criterion(&step_def, &criteria);

            examples.push(PrmTrainingExample {
                run_id: row.run_id.clone(),
                workflow_id: row.workflow_id.clone(),
                step_index: step_idx,
                step_definition: step_def.clone(),
                criterion,
                workflow_context: row.task_name.clone(),
                execution_result,
                iteration: iter_data.iteration,
                fixer_diff,
                final_outcome,
                domain: row.category.clone(),
            });
        }
    }

    examples
}

/// Parse step definitions from the final workflow JSON.
fn parse_steps_from_workflow(json: &Option<String>) -> Option<Vec<StepDefinition>> {
    let json_str = json.as_ref()?;
    let value: serde_json::Value = serde_json::from_str(json_str).ok()?;

    // Steps can be at .steps or .verification_steps
    let steps_array = value
        .get("steps")
        .or_else(|| value.get("verification_steps"))
        .and_then(|v| v.as_array())?;

    let mut steps = Vec::new();
    for step_val in steps_array {
        let step_type = step_val
            .get("step_type")
            .or_else(|| step_val.get("type"))
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();

        let name = step_val
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("unnamed")
            .to_string();

        steps.push(StepDefinition {
            step_type,
            name,
            command: step_val.get("command").and_then(|v| v.as_str()).map(String::from),
            prompt: step_val.get("prompt").and_then(|v| v.as_str()).map(String::from),
            check_type: step_val.get("check_type").and_then(|v| v.as_str()).map(String::from),
            expected: step_val.get("expected").and_then(|v| v.as_str()).map(String::from),
            parameters: {
                // Filter out keys already captured in dedicated fields
                let mut extra = step_val.clone();
                if let Some(obj) = extra.as_object_mut() {
                    for key in &["step_type", "type", "name", "command", "prompt", "check_type", "expected"] {
                        obj.remove(*key);
                    }
                }
                extra
            },
        });
    }

    Some(steps)
}

#[derive(Debug)]
struct IterationResult {
    step_index: usize,
    iteration: u32,
    passed: bool,
    error: Option<String>,
}

/// Parse verification_iterations JSON into structured data.
fn parse_verification_iterations(json: &Option<String>) -> Vec<IterationResult> {
    let json_str = match json.as_ref() {
        Some(s) if !s.is_empty() => s,
        _ => return Vec::new(),
    };

    let value: serde_json::Value = match serde_json::from_str(json_str) {
        Ok(v) => v,
        Err(e) => {
            debug!("Failed to parse verification_iterations: {e}");
            return Vec::new();
        }
    };

    let mut results = Vec::new();

    // Format: array of iteration objects, each containing step results
    if let Some(iterations) = value.as_array() {
        for (iter_idx, iteration) in iterations.iter().enumerate() {
            // Each iteration may have a "results" array or be a flat array of step results
            let step_results = iteration
                .get("results")
                .or_else(|| iteration.get("steps"))
                .and_then(|v| v.as_array())
                .or_else(|| iteration.as_array());

            if let Some(step_results) = step_results {
                for (step_idx, step_result) in step_results.iter().enumerate() {
                    let passed = step_result
                        .get("passed")
                        .or_else(|| step_result.get("success"))
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);

                    let error = step_result
                        .get("error")
                        .or_else(|| step_result.get("message"))
                        .and_then(|v| v.as_str())
                        .map(String::from);

                    results.push(IterationResult {
                        step_index: step_idx,
                        iteration: iter_idx as u32,
                        passed,
                        error,
                    });
                }
            }
        }
    }

    results
}

/// Parse fixer snapshots to extract diffs.
fn parse_fixer_diffs(json: &Option<String>) -> Vec<String> {
    let json_str = match json.as_ref() {
        Some(s) if !s.is_empty() => s,
        _ => return Vec::new(),
    };

    let value: serde_json::Value = match serde_json::from_str(json_str) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };

    let mut diffs = Vec::new();
    if let Some(snapshots) = value.as_array() {
        for snapshot in snapshots {
            let diff = snapshot
                .get("diff")
                .or_else(|| snapshot.get("changes"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            diffs.push(diff);
        }
    }

    diffs
}

/// Parse specification criteria JSON.
fn parse_criteria(json: &Option<String>) -> Vec<CriterionContext> {
    let json_str = match json.as_ref() {
        Some(s) if !s.is_empty() => s,
        _ => return Vec::new(),
    };

    let value: serde_json::Value = match serde_json::from_str(json_str) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };

    let criteria_array = value
        .get("criteria")
        .and_then(|v| v.as_array())
        .or_else(|| value.as_array());

    match criteria_array {
        Some(arr) => arr
            .iter()
            .filter_map(|c| {
                Some(CriterionContext {
                    id: c.get("id")?.as_str()?.to_string(),
                    description: c.get("description")?.as_str()?.to_string(),
                    method: c
                        .get("method")
                        .and_then(|v| v.as_str())
                        .unwrap_or("command")
                        .to_string(),
                    priority: c
                        .get("priority")
                        .and_then(|v| v.as_str())
                        .unwrap_or("important")
                        .to_string(),
                })
            })
            .collect(),
        None => Vec::new(),
    }
}

/// Try to match a step definition to a criterion by name/category heuristics.
fn find_matching_criterion(
    step: &StepDefinition,
    criteria: &[CriterionContext],
) -> Option<CriterionContext> {
    if criteria.is_empty() {
        return None;
    }

    let step_name_lower = step.name.to_lowercase();

    // Try exact ID match
    for c in criteria {
        if step_name_lower.contains(&c.id.to_lowercase()) {
            return Some(c.clone());
        }
    }

    // Try description keyword overlap
    for c in criteria {
        let desc_lower = c.description.to_lowercase();
        let desc_words: Vec<&str> = desc_lower.split_whitespace().collect();
        let matches = desc_words
            .iter()
            .filter(|w| w.len() > 3 && step_name_lower.contains(**w))
            .count();
        if matches >= 2 {
            return Some(c.clone());
        }
    }

    None
}

/// Serialize training examples to JSONL format.
pub fn examples_to_jsonl(examples: &[PrmTrainingExample]) -> String {
    examples
        .iter()
        .filter_map(|e| serde_json::to_string(e).ok())
        .collect::<Vec<_>>()
        .join("\n")
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_steps_from_workflow() {
        let json = Some(
            r#"{"steps": [
                {"step_type": "command", "name": "check build", "command": "npm run build"},
                {"step_type": "prompt", "name": "verify output", "prompt": "check logs"}
            ]}"#
            .to_string(),
        );
        let steps = parse_steps_from_workflow(&json).unwrap();
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].step_type, "command");
        assert_eq!(steps[0].name, "check build");
        assert_eq!(steps[0].command.as_deref(), Some("npm run build"));
        assert_eq!(steps[1].step_type, "prompt");
    }

    #[test]
    fn test_parse_steps_none() {
        assert!(parse_steps_from_workflow(&None).is_none());
        assert!(parse_steps_from_workflow(&Some("{}".to_string())).is_none());
    }

    #[test]
    fn test_parse_verification_iterations() {
        let json = Some(
            r#"[
                {"results": [{"passed": true}, {"passed": false, "error": "timeout"}]},
                {"results": [{"passed": true}, {"passed": true}]}
            ]"#
            .to_string(),
        );
        let results = parse_verification_iterations(&json);
        assert_eq!(results.len(), 4);
        assert!(results[0].passed);
        assert!(!results[1].passed);
        assert_eq!(results[1].error.as_deref(), Some("timeout"));
        assert_eq!(results[2].iteration, 1);
    }

    #[test]
    fn test_parse_fixer_diffs() {
        let json = Some(r#"[{"diff": "- old\n+ new"}, {"diff": ""}]"#.to_string());
        let diffs = parse_fixer_diffs(&json);
        assert_eq!(diffs.len(), 2);
        assert_eq!(diffs[0], "- old\\n+ new");
    }

    #[test]
    fn test_parse_criteria() {
        let json = Some(
            r#"{"criteria": [
                {"id": "build-passes", "description": "Build succeeds", "method": "command", "priority": "critical"}
            ]}"#
            .to_string(),
        );
        let criteria = parse_criteria(&json);
        assert_eq!(criteria.len(), 1);
        assert_eq!(criteria[0].id, "build-passes");
    }

    #[test]
    fn test_find_matching_criterion() {
        let criteria = vec![CriterionContext {
            id: "build-passes".to_string(),
            description: "TypeScript build succeeds with no errors".to_string(),
            method: "command".to_string(),
            priority: "critical".to_string(),
        }];

        let step = StepDefinition {
            step_type: "command".to_string(),
            name: "Verify build-passes".to_string(),
            command: Some("npm run build".to_string()),
            prompt: None,
            check_type: None,
            expected: None,
            parameters: serde_json::Value::Null,
        };

        let matched = find_matching_criterion(&step, &criteria);
        assert!(matched.is_some());
        assert_eq!(matched.unwrap().id, "build-passes");
    }

    #[test]
    fn test_examples_to_jsonl() {
        let examples = vec![PrmTrainingExample {
            run_id: "run-1".to_string(),
            workflow_id: None,
            step_index: 0,
            step_definition: StepDefinition {
                step_type: "command".to_string(),
                name: "test".to_string(),
                command: Some("echo hi".to_string()),
                prompt: None,
                check_type: None,
                expected: None,
                parameters: serde_json::Value::Null,
            },
            criterion: None,
            workflow_context: "test workflow".to_string(),
            execution_result: ExecutionResult::Passed,
            iteration: 0,
            fixer_diff: None,
            final_outcome: FinalOutcome::WorkflowPassed,
            domain: None,
        }];

        let jsonl = examples_to_jsonl(&examples);
        assert!(!jsonl.is_empty());
        assert!(jsonl.contains("run-1"));
        assert!(!jsonl.contains('\n')); // Single example, no newline
    }
}

//! Stage builder for the Recap module.
//!
//! Functions for building recap stages (setup, verification, agentic, completion) from steps.

use super::types::{RecapStep, StageOccurrence, StageRecap, StageTransition};
use super::utils::{calculate_duration_ms, get_stage_display_name, map_state_to_stage};
use crate::database::{StoredVerificationResult, TaskRun};

/// Check if a step is a setup-related step based on name patterns.
fn is_setup_step(step: &RecapStep) -> bool {
    let name_lower = step.name.to_lowercase();
    name_lower.contains("setup")
        || name_lower.contains("init")
        || name_lower.contains("plan")
        || name_lower.contains("configure")
}

/// Compute overall status for a stage based on its steps.
fn compute_stage_status(steps: &[RecapStep], task_run: &TaskRun) -> String {
    if steps.is_empty() {
        return if task_run.status == "running" {
            "pending".to_string()
        } else {
            "skipped".to_string()
        };
    }

    if steps.iter().any(|s| s.status == "running") {
        return "running".to_string();
    }

    if steps.iter().any(|s| s.status == "failed") {
        return "failed".to_string();
    }

    "success".to_string()
}

/// Build stages from persisted transition history.
fn build_stages_from_history(
    transitions: &[StageTransition],
    steps: &[RecapStep],
    task_run: &TaskRun,
) -> Vec<StageRecap> {
    let mut stage_occurrences: Vec<StageOccurrence> = Vec::new();

    for (i, transition) in transitions.iter().enumerate() {
        if let Some(ui_stage) = map_state_to_stage(&transition.to) {
            let ended_at = transitions.get(i + 1).map(|t| t.timestamp.clone());

            let is_new = stage_occurrences.last().map_or(true, |last| {
                last.stage != ui_stage || last.iteration != transition.iteration
            });

            if is_new {
                stage_occurrences.push(StageOccurrence {
                    stage: ui_stage.to_string(),
                    iteration: transition.iteration,
                    started_at: transition.timestamp.clone(),
                    ended_at,
                });
            } else if let Some(last) = stage_occurrences.last_mut() {
                last.ended_at = ended_at;
            }
        }
    }

    if stage_occurrences.is_empty() && !steps.is_empty() {
        return build_stages_heuristic(task_run, steps);
    }

    let mut stages: Vec<StageRecap> = Vec::new();
    let mut remaining_steps: Vec<RecapStep> = steps.to_vec();

    for occurrence in &stage_occurrences {
        let (stage_steps, leftover): (Vec<RecapStep>, Vec<RecapStep>) = remaining_steps
            .into_iter()
            .partition(|s| s.phase.as_deref() == Some(&occurrence.stage));
        remaining_steps = leftover;

        let status = compute_stage_status(&stage_steps, task_run);

        let duration_ms = occurrence
            .ended_at
            .as_ref()
            .and_then(|end| calculate_duration_ms(&occurrence.started_at, end));

        let iteration_num = if occurrence.stage == "agentic" || occurrence.stage == "verification" {
            Some(occurrence.iteration)
        } else {
            None
        };

        stages.push(StageRecap {
            stage: occurrence.stage.clone(),
            display_name: get_stage_display_name(&occurrence.stage),
            status,
            started_at: Some(occurrence.started_at.clone()),
            ended_at: occurrence.ended_at.clone(),
            duration_ms,
            steps: stage_steps,
            iteration: iteration_num,
        });
    }

    // Handle any remaining steps
    if !remaining_steps.is_empty() {
        for step in remaining_steps {
            let phase = step.phase.as_deref().unwrap_or("agentic");
            if let Some(stage) = stages.iter_mut().rev().find(|s| s.stage == phase) {
                stage.steps.push(step);
            } else {
                stages.push(StageRecap {
                    stage: phase.to_string(),
                    display_name: get_stage_display_name(phase),
                    status: compute_stage_status(&[step.clone()], task_run),
                    started_at: None,
                    ended_at: None,
                    duration_ms: None,
                    steps: vec![step],
                    iteration: None,
                });
            }
        }
    }

    stages
}

/// Build stages heuristically from steps when no transition history is available.
fn build_stages_heuristic(task_run: &TaskRun, steps: &[RecapStep]) -> Vec<StageRecap> {
    let mut stages: Vec<StageRecap> = Vec::new();

    let mut setup_steps: Vec<RecapStep> = Vec::new();
    let mut verification_steps: Vec<RecapStep> = Vec::new();
    let mut agentic_steps: Vec<RecapStep> = Vec::new();
    let mut completion_steps: Vec<RecapStep> = Vec::new();

    for step in steps {
        if let Some(ref phase) = step.phase {
            match phase.as_str() {
                "setup" => setup_steps.push(step.clone()),
                "verification" => verification_steps.push(step.clone()),
                "agentic" => agentic_steps.push(step.clone()),
                "completion" => completion_steps.push(step.clone()),
                _ => agentic_steps.push(step.clone()),
            }
            continue;
        }

        // Fall back to inferring phase from step_type and name
        match step.step_type.as_str() {
            "check" | "test" => verification_steps.push(step.clone()),
            "ai_session" => agentic_steps.push(step.clone()),
            "workflow" => {
                let name_lower = step.name.to_lowercase();
                if name_lower.contains("setup")
                    || name_lower.contains("init")
                    || name_lower.contains("plan")
                {
                    setup_steps.push(step.clone());
                } else if name_lower.contains("summary")
                    || name_lower.contains("completion")
                    || name_lower.contains("cleanup")
                {
                    completion_steps.push(step.clone());
                } else {
                    agentic_steps.push(step.clone());
                }
            }
            "action" => agentic_steps.push(step.clone()),
            _ => agentic_steps.push(step.clone()),
        }
    }

    // Create stages for non-empty groups
    // Order: setup -> verification -> agentic -> completion
    // (verification comes before agentic because the typical flow is:
    // setup -> verification (check initial state) -> agentic (fix issues) -> verification (re-check) -> ...)
    if !setup_steps.is_empty() {
        let status = compute_stage_status(&setup_steps, task_run);
        stages.push(StageRecap {
            stage: "setup".to_string(),
            display_name: "Setup".to_string(),
            status,
            started_at: Some(task_run.created_at.clone()),
            ended_at: None,
            duration_ms: None,
            steps: setup_steps,
            iteration: None,
        });
    }

    if !verification_steps.is_empty() {
        let status = compute_stage_status(&verification_steps, task_run);
        stages.push(StageRecap {
            stage: "verification".to_string(),
            display_name: "Verification".to_string(),
            status,
            started_at: None,
            ended_at: None,
            duration_ms: None,
            steps: verification_steps,
            iteration: None,
        });
    }

    if !agentic_steps.is_empty() {
        let status = compute_stage_status(&agentic_steps, task_run);
        stages.push(StageRecap {
            stage: "agentic".to_string(),
            display_name: "Agentic".to_string(),
            status,
            started_at: None,
            ended_at: None,
            duration_ms: None,
            steps: agentic_steps,
            iteration: None,
        });
    }

    // Add completion stage for completed/failed tasks
    if task_run.status == "complete" || task_run.status == "failed" || !completion_steps.is_empty()
    {
        let completion_status = if !completion_steps.is_empty() {
            compute_stage_status(&completion_steps, task_run)
        } else if task_run.status == "complete" {
            "success".to_string()
        } else {
            "failed".to_string()
        };

        stages.push(StageRecap {
            stage: "completion".to_string(),
            display_name: "Completion".to_string(),
            status: completion_status,
            started_at: None,
            ended_at: task_run.completed_at.clone(),
            duration_ms: None,
            steps: completion_steps,
            iteration: None,
        });
    }

    stages
}

/// Build stages from transition history or heuristically from steps.
pub fn build_stages(
    task_run: &TaskRun,
    steps: &[RecapStep],
    _verification_results: &[StoredVerificationResult],
) -> Vec<StageRecap> {
    if let Some(ref history_json) = task_run.transition_history_json {
        if let Ok(transitions) = serde_json::from_str::<Vec<StageTransition>>(history_json) {
            if !transitions.is_empty() {
                return build_stages_from_history(&transitions, steps, task_run);
            }
        }
    }

    build_stages_heuristic(task_run, steps)
}

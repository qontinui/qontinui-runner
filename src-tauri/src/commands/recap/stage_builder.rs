//! Stage builder for the Recap module.
//!
//! Functions for building recap stages (setup, verification, agentic, completion) from steps.
//!
//! Phase determination follows this priority:
//! 1. Explicit phase from step config (highest priority)
//! 2. Phase from checkpoint data
//! 3. Heuristic-based phase inference from step name (legacy fallback)

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

/// Sort steps by their started_at timestamp.
/// Steps without timestamps are placed at the end.
fn sort_steps_by_timestamp(steps: &mut [RecapStep]) {
    steps.sort_by(|a, b| {
        match (&a.started_at, &b.started_at) {
            (Some(a_time), Some(b_time)) => a_time.cmp(b_time),
            (Some(_), None) => std::cmp::Ordering::Less, // Steps with timestamps come first
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        }
    });
}

/// Get the earliest started_at timestamp from a list of steps.
fn get_earliest_timestamp(steps: &[RecapStep]) -> Option<String> {
    steps
        .iter()
        .filter_map(|s| s.started_at.as_ref())
        .min()
        .cloned()
}

/// Get the latest ended_at timestamp from a list of steps.
fn get_latest_timestamp(steps: &[RecapStep]) -> Option<String> {
    steps
        .iter()
        .filter_map(|s| s.ended_at.as_ref())
        .max()
        .cloned()
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

/// Get iteration from step, defaulting to 1 if not set.
fn get_step_iteration(step: &RecapStep) -> u32 {
    step.iteration.unwrap_or(1)
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

            let is_new = stage_occurrences.last().is_none_or(|last| {
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

    // Filter out stale steps from previous interrupted runs
    // Only include steps whose timestamps are within the transition history's timeframe
    // This prevents old checkpoints from polluting the current run's display
    let first_transition_time = transitions.first().map(|t| &t.timestamp);
    let filtered_steps: Vec<RecapStep> = if let Some(start_time) = first_transition_time {
        steps
            .iter()
            .filter(|step| {
                // Steps without timestamps are included (they'll be assigned by phase/iteration)
                let Some(ref step_time) = step.started_at else {
                    return true;
                };
                // Include steps that started at or after the first transition
                // Allow a small buffer (setup steps may start slightly before first verification)
                step_time >= start_time || {
                    // Also include setup steps that are close to the start (within 30 minutes before)
                    // as setup runs before the first recorded transition
                    if step.phase.as_deref() == Some("setup") {
                        if let (Ok(step_dt), Ok(start_dt)) = (
                            chrono::DateTime::parse_from_rfc3339(step_time),
                            chrono::DateTime::parse_from_rfc3339(start_time),
                        ) {
                            let diff = start_dt.signed_duration_since(step_dt);
                            return diff.num_minutes() <= 30;
                        }
                    }
                    false
                }
            })
            .cloned()
            .collect()
    } else {
        steps.to_vec()
    };

    let steps = &filtered_steps;

    let mut stages: Vec<StageRecap> = Vec::new();
    let mut remaining_steps: Vec<RecapStep> = steps.to_vec();

    for occurrence in &stage_occurrences {
        // Partition steps by both phase AND iteration
        // For verification/agentic phases, steps have "(iteration N)" in their name
        let (mut stage_steps, leftover): (Vec<RecapStep>, Vec<RecapStep>) =
            remaining_steps.into_iter().partition(|s| {
                let phase_matches = s.phase.as_deref() == Some(&occurrence.stage);
                if !phase_matches {
                    return false;
                }

                // For verification and agentic phases, also match iteration
                if occurrence.stage == "verification" || occurrence.stage == "agentic" {
                    let step_iteration = get_step_iteration(s);
                    step_iteration == occurrence.iteration
                } else {
                    // For setup/completion, just match phase
                    true
                }
            });
        remaining_steps = leftover;

        // Sort steps by timestamp for correct chronological order
        sort_steps_by_timestamp(&mut stage_steps);

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

    // Handle any remaining steps - assign to most recent matching stage by phase and iteration
    if !remaining_steps.is_empty() {
        for step in remaining_steps {
            let phase = step.phase.as_deref().unwrap_or("agentic");
            let step_iteration = step.iteration;

            // Try to find a matching stage by phase and iteration
            let matching_stage = stages.iter_mut().rev().find(|s| {
                if s.stage != phase {
                    return false;
                }
                // For verification/agentic, prefer matching iteration
                if let Some(iter) = step_iteration {
                    s.iteration == Some(iter)
                } else {
                    true // No iteration in step, just match phase
                }
            });

            if let Some(stage) = matching_stage {
                stage.steps.push(step);
                // Re-sort the stage steps after adding
                sort_steps_by_timestamp(&mut stage.steps);
            } else {
                // Create a new stage for this step
                let started_at = step.started_at.clone();
                let ended_at = step.ended_at.clone();
                let duration_ms = step.duration_ms;

                stages.push(StageRecap {
                    stage: phase.to_string(),
                    display_name: get_stage_display_name(phase),
                    status: compute_stage_status(std::slice::from_ref(&step), task_run),
                    started_at,
                    ended_at,
                    duration_ms,
                    steps: vec![step],
                    iteration: step_iteration,
                });
            }
        }
    }

    // Final sort of all stages by their started_at timestamp for overall chronological order
    // But use phase order as primary sort when timestamps aren't reliably comparable
    stages.sort_by(|a, b| {
        // Phase order helper: setup(0) -> verification(1) -> agentic(2) -> completion(3)
        let phase_order = |s: &str| match s {
            "setup" => 0,
            "verification" => 1,
            "agentic" => 2,
            "completion" => 3,
            _ => 2,
        };

        // First, compare by iteration (if both have one)
        // Within the same iteration, verification comes before agentic
        let a_iter = a.iteration.unwrap_or(0);
        let b_iter = b.iteration.unwrap_or(0);

        // Setup and completion don't have iterations, handle them separately
        let a_phase = phase_order(&a.stage);
        let b_phase = phase_order(&b.stage);

        // Setup always comes first, completion always comes last
        if a_phase == 0 || b_phase == 0 || a_phase == 3 || b_phase == 3 {
            return a_phase.cmp(&b_phase);
        }

        // For verification and agentic (phases 1 and 2):
        // Sort by iteration first, then by phase within iteration
        if a_iter != b_iter {
            return a_iter.cmp(&b_iter);
        }

        // Same iteration: verification before agentic
        // Only use timestamps if both are the same phase (to order steps within a phase)
        if a_phase != b_phase {
            return a_phase.cmp(&b_phase);
        }

        // Same phase and iteration - use timestamps if both available
        match (&a.started_at, &b.started_at) {
            (Some(a_time), Some(b_time)) => a_time.cmp(b_time),
            _ => std::cmp::Ordering::Equal,
        }
    });

    stages
}

/// Build stages heuristically from steps when no transition history is available.
/// Groups steps by phase AND iteration to create interleaved stages.
///
/// Phase determination priority:
/// 1. Explicit phase from step.phase (set during workflow loading)
/// 2. Heuristic inference from step_type and name (legacy fallback)
fn build_stages_heuristic(task_run: &TaskRun, steps: &[RecapStep]) -> Vec<StageRecap> {
    use std::collections::BTreeMap;

    let mut stages: Vec<StageRecap> = Vec::new();

    // Group steps by phase
    let mut setup_steps: Vec<RecapStep> = Vec::new();
    let mut completion_steps: Vec<RecapStep> = Vec::new();
    // For verification and agentic, group by iteration
    let mut verification_by_iter: BTreeMap<u32, Vec<RecapStep>> = BTreeMap::new();
    let mut agentic_by_iter: BTreeMap<u32, Vec<RecapStep>> = BTreeMap::new();

    for step in steps {
        // Use explicit phase if set, otherwise fall back to heuristic inference
        let phase = step.phase.as_deref().unwrap_or_else(|| {
            // Heuristic fallback: infer phase from step_type and name (for legacy steps)
            match step.step_type.as_str() {
                "check" | "test" => "verification",
                "ai_session" => "agentic",
                "workflow" => {
                    let name_lower = step.name.to_lowercase();
                    if name_lower.contains("setup")
                        || name_lower.contains("init")
                        || name_lower.contains("plan")
                    {
                        "setup"
                    } else if name_lower.contains("summary")
                        || name_lower.contains("completion")
                        || name_lower.contains("cleanup")
                    {
                        "completion"
                    } else {
                        "agentic"
                    }
                }
                _ => "agentic",
            }
        });

        match phase {
            "setup" => setup_steps.push(step.clone()),
            "verification" => {
                let iter = step.iteration.unwrap_or(1);
                verification_by_iter
                    .entry(iter)
                    .or_default()
                    .push(step.clone());
            }
            "agentic" => {
                let iter = step.iteration.unwrap_or(1);
                agentic_by_iter.entry(iter).or_default().push(step.clone());
            }
            "completion" => completion_steps.push(step.clone()),
            _ => {
                let iter = step.iteration.unwrap_or(1);
                agentic_by_iter.entry(iter).or_default().push(step.clone());
            }
        }
    }

    // Create setup stage
    if !setup_steps.is_empty() {
        // Sort steps by timestamp
        sort_steps_by_timestamp(&mut setup_steps);

        let status = compute_stage_status(&setup_steps, task_run);

        // Derive stage timestamps from steps, falling back to task_run.created_at
        let started_at =
            get_earliest_timestamp(&setup_steps).or_else(|| Some(task_run.created_at.clone()));
        let ended_at = get_latest_timestamp(&setup_steps);
        let duration_ms = match (&started_at, &ended_at) {
            (Some(start), Some(end)) => calculate_duration_ms(start, end),
            _ => None,
        };

        stages.push(StageRecap {
            stage: "setup".to_string(),
            display_name: "Setup".to_string(),
            status,
            started_at,
            ended_at,
            duration_ms,
            steps: setup_steps,
            iteration: None,
        });
    }

    // Find max iteration across both verification and agentic
    let max_iter = verification_by_iter
        .keys()
        .chain(agentic_by_iter.keys())
        .copied()
        .max()
        .unwrap_or(0);

    // Create interleaved verification and agentic stages by iteration
    // Order: verification(1) -> agentic(1) -> verification(2) -> agentic(2) -> ...
    for iter in 1..=max_iter {
        // Verification for this iteration
        if let Some(mut ver_steps) = verification_by_iter.remove(&iter) {
            // Sort steps by timestamp
            sort_steps_by_timestamp(&mut ver_steps);

            let status = compute_stage_status(&ver_steps, task_run);

            // Derive stage timestamps from steps
            let started_at = get_earliest_timestamp(&ver_steps);
            let ended_at = get_latest_timestamp(&ver_steps);
            let duration_ms = match (&started_at, &ended_at) {
                (Some(start), Some(end)) => calculate_duration_ms(start, end),
                _ => None,
            };

            stages.push(StageRecap {
                stage: "verification".to_string(),
                display_name: "Verification".to_string(),
                status,
                started_at,
                ended_at,
                duration_ms,
                steps: ver_steps,
                iteration: Some(iter),
            });
        }

        // Agentic for this iteration
        if let Some(mut ag_steps) = agentic_by_iter.remove(&iter) {
            // Sort steps by timestamp
            sort_steps_by_timestamp(&mut ag_steps);

            let status = compute_stage_status(&ag_steps, task_run);

            // Derive stage timestamps from steps
            let started_at = get_earliest_timestamp(&ag_steps);
            let ended_at = get_latest_timestamp(&ag_steps);
            let duration_ms = match (&started_at, &ended_at) {
                (Some(start), Some(end)) => calculate_duration_ms(start, end),
                _ => None,
            };

            stages.push(StageRecap {
                stage: "agentic".to_string(),
                display_name: "Agentic".to_string(),
                status,
                started_at,
                ended_at,
                duration_ms,
                steps: ag_steps,
                iteration: Some(iter),
            });
        }
    }

    // Add any remaining verification/agentic steps with iteration 0 or unknown
    for (iter, mut ver_steps) in verification_by_iter {
        // Sort steps by timestamp
        sort_steps_by_timestamp(&mut ver_steps);

        let status = compute_stage_status(&ver_steps, task_run);

        // Derive stage timestamps from steps
        let started_at = get_earliest_timestamp(&ver_steps);
        let ended_at = get_latest_timestamp(&ver_steps);
        let duration_ms = match (&started_at, &ended_at) {
            (Some(start), Some(end)) => calculate_duration_ms(start, end),
            _ => None,
        };

        stages.push(StageRecap {
            stage: "verification".to_string(),
            display_name: "Verification".to_string(),
            status,
            started_at,
            ended_at,
            duration_ms,
            steps: ver_steps,
            iteration: Some(iter),
        });
    }
    for (iter, mut ag_steps) in agentic_by_iter {
        // Sort steps by timestamp
        sort_steps_by_timestamp(&mut ag_steps);

        let status = compute_stage_status(&ag_steps, task_run);

        // Derive stage timestamps from steps
        let started_at = get_earliest_timestamp(&ag_steps);
        let ended_at = get_latest_timestamp(&ag_steps);
        let duration_ms = match (&started_at, &ended_at) {
            (Some(start), Some(end)) => calculate_duration_ms(start, end),
            _ => None,
        };

        stages.push(StageRecap {
            stage: "agentic".to_string(),
            display_name: "Agentic".to_string(),
            status,
            started_at,
            ended_at,
            duration_ms,
            steps: ag_steps,
            iteration: Some(iter),
        });
    }

    // Add completion stage for completed/failed tasks
    if task_run.status == "complete" || task_run.status == "failed" || !completion_steps.is_empty()
    {
        // Sort completion steps by timestamp
        sort_steps_by_timestamp(&mut completion_steps);

        let completion_status = if !completion_steps.is_empty() {
            compute_stage_status(&completion_steps, task_run)
        } else if task_run.status == "complete" {
            "success".to_string()
        } else {
            "failed".to_string()
        };

        // Derive stage timestamps from steps, falling back to task_run.completed_at
        let started_at = get_earliest_timestamp(&completion_steps);
        let ended_at =
            get_latest_timestamp(&completion_steps).or_else(|| task_run.completed_at.clone());
        let duration_ms = match (&started_at, &ended_at) {
            (Some(start), Some(end)) => calculate_duration_ms(start, end),
            _ => None,
        };

        stages.push(StageRecap {
            stage: "completion".to_string(),
            display_name: "Completion".to_string(),
            status: completion_status,
            started_at,
            ended_at,
            duration_ms,
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

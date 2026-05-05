//! Pure builder functions for canvas panel data.
//!
//! Each function produces a `serde_json::Value` payload ready for a specific
//! panel component type. No side effects — easily testable.

use serde_json::{json, Value};

use super::IterationSnapshot;
use crate::orchestrator::types::Finding;
use crate::str_utils::truncate_str;
use crate::unified_workflow_executor::types::LoopConfig;
use crate::unified_workflow_executor::types::LoopResult;
use std::collections::HashMap;

/// Build the Mission Brief panel (structured data for MissionBrief component).
pub fn build_mission_brief(config: &LoopConfig) -> Value {
    let model = config
        .model_override
        .as_deref()
        .unwrap_or("default")
        .to_string();

    let stages: Vec<Value> = if config.stages.is_empty() {
        vec![
            json!({ "name": config.workflow_name, "index": 0, "max_iterations": config.max_iterations }),
        ]
    } else {
        config
            .stages
            .iter()
            .map(|s| {
                json!({
                    "name": s.name,
                    "index": s.index,
                    "max_iterations": s.max_iterations,
                })
            })
            .collect()
    };

    json!({
        "workflow_name": config.workflow_name,
        "prompt": config.base_prompt,
        "model": model,
        "reflection": config.reflection_mode,
        "approval_gate": config.approval_gate,
        "stages": stages,
        "total_stages": stages.len(),
        "max_iterations": config.max_iterations,
        "progress": {
            "current_stage_index": null,
            "current_iteration": 0,
            "phase": null,
            "activity": null,
        }
    })
}

/// Build an updated progress payload for the Mission Brief panel.
pub fn build_mission_brief_progress(
    workflow_name: &str,
    prompt: &str,
    model: &str,
    reflection: bool,
    approval_gate: bool,
    stages: &[Value],
    max_iterations: u32,
    current_stage_index: Option<u32>,
    current_iteration: u32,
    phase: Option<&str>,
    activity: Option<&str>,
) -> Value {
    json!({
        "workflow_name": workflow_name,
        "prompt": prompt,
        "model": model,
        "reflection": reflection,
        "approval_gate": approval_gate,
        "stages": stages,
        "total_stages": stages.len(),
        "max_iterations": max_iterations,
        "progress": {
            "current_stage_index": current_stage_index,
            "current_iteration": current_iteration,
            "phase": phase,
            "activity": activity,
        }
    })
}

/// Build the Setup Report panel (KeyValue).
pub fn build_setup_report(
    success: bool,
    results: &[crate::step_executor::StepExecutionResult],
    duration_ms: u64,
) -> Value {
    let total = results.len();
    let passed = results.iter().filter(|r| r.success).count();
    let status_style = if success { "success" } else { "error" };
    let status_label = if success { "Passed" } else { "Failed" };

    json!({
        "pairs": [
            { "key": "Status", "value": status_label, "style": status_style },
            { "key": "Steps Completed", "value": format!("{} / {}", passed, total) },
            { "key": "Duration", "value": format_duration_ms(duration_ms) },
        ]
    })
}

/// Build the Verification Matrix panel (Table).
/// Shows per-step pass/fail across iterations.
pub fn build_verification_matrix(
    snapshots: &[IterationSnapshot],
    verification_history: &HashMap<String, Vec<bool>>,
) -> Value {
    if snapshots.is_empty() {
        return json!({ "columns": ["Step"], "rows": [], "sortable": true });
    }

    let num_iterations = snapshots.len();
    let mut columns: Vec<Value> = vec![json!("Step")];
    for i in 1..=num_iterations {
        columns.push(json!(format!("Iter {}", i)));
    }

    // Build rows from verification_history
    let mut rows: Vec<Value> = Vec::new();
    let mut step_names: Vec<&String> = verification_history.keys().collect();
    step_names.sort();

    for name in step_names {
        let history = &verification_history[name];
        let mut row: Vec<Value> = vec![json!(name)];
        for i in 0..num_iterations {
            if i < history.len() {
                row.push(json!(history[i]));
            } else {
                row.push(Value::Null);
            }
        }
        rows.push(json!(row));
    }

    json!({
        "columns": columns,
        "rows": rows,
        "sortable": true
    })
}

/// Build the Convergence Tracker panel (ProgressChart).
pub fn build_convergence_tracker(snapshots: &[IterationSnapshot]) -> Value {
    let latest = match snapshots.last() {
        Some(s) => s,
        None => {
            return json!({
                "segments": [],
                "total": 0
            })
        }
    };

    let skipped = latest.skipped;
    json!({
        "segments": [
            { "label": "Passing", "value": latest.passed, "color": "#22c55e" },
            { "label": "Failing", "value": latest.failed, "color": "#ef4444" },
            { "label": "Skipped", "value": skipped, "color": "#6b7280" },
        ],
        "total": latest.total
    })
}

/// Build the Iteration Digest panel (Markdown) — after verification only.
pub fn build_iteration_digest_verification(
    snapshot: &IterationSnapshot,
    prev: Option<&IterationSnapshot>,
) -> Value {
    let mut content = format!(
        "### Iteration {} — Verification\n\n\
         **Result:** {} of {} passed · {} failures\n",
        snapshot.iteration, snapshot.passed, snapshot.total, snapshot.failed,
    );

    // Show failures
    if !snapshot.failed_step_errors.is_empty() {
        content.push_str("\n**Failures:**\n");
        for (name, error) in &snapshot.failed_step_errors {
            let truncated = if error.len() > 120 {
                format!("{}…", truncate_str(error, 120))
            } else {
                error.clone()
            };
            content.push_str(&format!("- **{}** — {}\n", name, truncated));
        }
    }

    // Comparison with previous
    if let Some(prev) = prev {
        let prev_failed: std::collections::HashSet<&str> =
            prev.failed_step_names.iter().map(|s| s.as_str()).collect();
        let curr_failed: std::collections::HashSet<&str> = snapshot
            .failed_step_names
            .iter()
            .map(|s| s.as_str())
            .collect();

        let new_failures = curr_failed.difference(&prev_failed).count();
        let fixed = prev_failed.difference(&curr_failed).count();
        let persistent = curr_failed.intersection(&prev_failed).count();

        content.push_str(&format!(
            "\n**vs Previous:** {} new · {} fixed · {} persistent\n",
            new_failures, fixed, persistent,
        ));
    }

    json!({ "content": content })
}

/// Build the Iteration Digest panel (Markdown) — updated after agentic phase.
pub fn build_iteration_digest_full(snapshot: &IterationSnapshot) -> Value {
    let mut content = format!(
        "### Iteration {}\n\n\
         **Verification:** {} of {} passed · {} failures\n",
        snapshot.iteration, snapshot.passed, snapshot.total, snapshot.failed,
    );

    if !snapshot.failed_step_errors.is_empty() {
        for (name, error) in &snapshot.failed_step_errors {
            let truncated = if error.len() > 120 {
                format!("{}…", truncate_str(error, 120))
            } else {
                error.clone()
            };
            content.push_str(&format!("> - **{}** — {}\n", name, truncated));
        }
    }

    let agentic_status = snapshot.agentic_outcome.as_deref().unwrap_or("Skipped");
    content.push_str(&format!(
        "\n**AI Phase:** {} · {} findings · {} injected steps\n",
        agentic_status, snapshot.findings_count, snapshot.injected_steps_count,
    ));

    let verify_dur = format_duration_ms(snapshot.verify_duration_ms);
    let agentic_dur = snapshot
        .agentic_duration_ms
        .map(format_duration_ms)
        .unwrap_or_else(|| "—".to_string());
    content.push_str(&format!(
        "**Duration:** Verify {} · AI {}\n",
        verify_dur, agentic_dur,
    ));

    json!({ "content": content })
}

/// Build the Findings Board panel (FindingList).
pub fn build_findings_board(findings: &[Finding]) -> Value {
    let items: Vec<Value> = findings
        .iter()
        .map(|f| {
            json!({
                "title": f.finding_type,
                "severity": "info",
                "description": f.description,
                "location": f.evidence.clone().unwrap_or_default(),
            })
        })
        .collect();

    json!({ "findings": items })
}

/// Build the Resource Dashboard panel (KeyValue).
pub fn build_resource_dashboard(
    iterations: u32,
    max_iterations: u32,
    sessions: u32,
    max_sessions: Option<u32>,
    elapsed: std::time::Duration,
) -> Value {
    let sessions_str = if let Some(max) = max_sessions {
        format!("{} / {}", sessions, max)
    } else {
        format!("{}", sessions)
    };

    json!({
        "pairs": [
            { "key": "Iterations", "value": format!("{} / {}", iterations, max_iterations) },
            { "key": "Sessions", "value": sessions_str },
            { "key": "Elapsed", "value": format_duration(elapsed) },
        ]
    })
}

/// Build the Outcome Alert panel (Alert).
pub fn build_outcome_alert(
    result: &LoopResult,
    duration: std::time::Duration,
    sessions: u32,
) -> Value {
    let (severity, message, details) = if result.verification_passed {
        let last_iter = result.iteration_results.last();
        let total_checks = last_iter
            .map(|r| r.passed_checks + r.failed_checks)
            .unwrap_or(0);
        (
            "success",
            format!(
                "Verification Passed — All {} checks passing after {} iteration(s)",
                total_checks, result.iterations_run,
            ),
            format!(
                "Duration: {} · {} iteration(s) · {} AI session(s)",
                format_duration(duration),
                result.iterations_run,
                sessions,
            ),
        )
    } else if result.was_stopped {
        (
            "warning",
            format!(
                "Stopped by user after {} iteration(s)",
                result.iterations_run
            ),
            format!(
                "Duration: {} · {} AI session(s)",
                format_duration(duration),
                sessions
            ),
        )
    } else if result.unfixable_errors {
        (
            "warning",
            format!(
                "AI determined errors unfixable after {} iteration(s)",
                result.iterations_run,
            ),
            format!(
                "Duration: {} · {} AI session(s)",
                format_duration(duration),
                sessions
            ),
        )
    } else if result.critical_failure {
        (
            "error",
            format!(
                "Critical failure after {} iteration(s)",
                result.iterations_run,
            ),
            format!(
                "Duration: {} · {} AI session(s)",
                format_duration(duration),
                sessions
            ),
        )
    } else {
        let last_iter = result.iteration_results.last();
        let failed = last_iter.map(|r| r.failed_checks).unwrap_or(0);
        (
            "error",
            format!(
                "Verification Failed — {} check(s) still failing after {} iteration(s)",
                failed, result.iterations_run,
            ),
            format!(
                "Duration: {} · {} AI session(s)",
                format_duration(duration),
                sessions
            ),
        )
    };

    json!({
        "severity": severity,
        "message": message,
        "details": details,
    })
}

/// Build the Summary Stats panel (SummaryStats).
/// Shows a compact pass/fail/skip ratio bar.
pub fn build_summary_stats(snapshots: &[IterationSnapshot]) -> Value {
    let latest = match snapshots.last() {
        Some(s) => s,
        None => {
            return json!({
                "total": 0,
                "passed": 0,
                "failed": 0,
                "skipped": 0,
                "label": "Verification Checks"
            })
        }
    };

    json!({
        "total": latest.total,
        "passed": latest.passed,
        "failed": latest.failed,
        "skipped": latest.skipped,
        "label": "Verification Checks"
    })
}

/// Build the State Timeline panel (StateTimeline).
/// Shows per-step pass/fail across iterations as horizontal colored bars.
pub fn build_state_timeline(verification_history: &HashMap<String, Vec<bool>>) -> Value {
    let mut step_names: Vec<&String> = verification_history.keys().collect();
    step_names.sort();

    let steps: Vec<Value> = step_names
        .iter()
        .map(|name| {
            let history = &verification_history[*name];
            let iterations: Vec<Value> = history
                .iter()
                .enumerate()
                .map(|(i, &passed)| {
                    json!({
                        "iteration": i + 1,
                        "status": if passed { "pass" } else { "fail" }
                    })
                })
                .collect();
            json!({
                "name": name,
                "iterations": iterations
            })
        })
        .collect();

    json!({ "steps": steps })
}

/// Build the Waterfall panel (Waterfall).
/// Shows time-based execution of steps as horizontal bars.
pub fn build_waterfall(
    step_timings: &[(String, u64, u64, &str, Option<&str>)], // (name, start_ms, duration_ms, status, phase)
    total_duration_ms: u64,
) -> Value {
    let entries: Vec<Value> = step_timings
        .iter()
        .map(|(name, start_ms, duration_ms, status, phase)| {
            let mut entry = json!({
                "name": name,
                "start_ms": start_ms,
                "duration_ms": duration_ms,
                "status": status,
            });
            if let Some(p) = phase {
                entry["phase"] = json!(p);
            }
            entry
        })
        .collect();

    json!({
        "entries": entries,
        "total_duration_ms": total_duration_ms
    })
}

/// Build the Sparkline panel (Sparkline).
/// Shows win-loss sparklines for each step across iterations.
pub fn build_sparkline(verification_history: &HashMap<String, Vec<bool>>) -> Value {
    let mut step_names: Vec<&String> = verification_history.keys().collect();
    step_names.sort();

    let series: Vec<Value> = step_names
        .iter()
        .map(|name| {
            let history = &verification_history[*name];
            let values: Vec<Value> = history
                .iter()
                .enumerate()
                .map(|(i, &passed)| {
                    json!({
                        "iteration": i + 1,
                        "outcome": if passed { "pass" } else { "fail" }
                    })
                })
                .collect();
            json!({
                "name": name,
                "values": values
            })
        })
        .collect();

    json!({ "series": series })
}

/// Build the Waffle Chart panel (WaffleChart).
/// Shows a grid of colored cells representing check status.
pub fn build_waffle_chart(snapshots: &[IterationSnapshot]) -> Value {
    let latest = match snapshots.last() {
        Some(s) => s,
        None => {
            return json!({
                "cells": [],
                "columns": 10
            })
        }
    };

    // Build cells from the latest snapshot
    let mut cells: Vec<Value> = Vec::new();

    // Add passed cells
    for i in 0..latest.passed {
        cells.push(json!({
            "label": format!("Check {} (passed)", i + 1),
            "status": "pass"
        }));
    }

    // Add failed cells with actual step names
    for name in &latest.failed_step_names {
        cells.push(json!({
            "label": format!("{} (failed)", name),
            "status": "fail"
        }));
    }

    // Add skipped cells
    for i in 0..latest.skipped {
        cells.push(json!({
            "label": format!("Check {} (skipped)", i + 1),
            "status": "skip"
        }));
    }

    // Choose columns based on total count
    let columns = if latest.total <= 10 {
        latest.total.max(1)
    } else if latest.total <= 25 {
        5
    } else {
        10
    };

    json!({
        "cells": cells,
        "columns": columns
    })
}

/// Build the Phase Timeline panel (PhaseTimeline).
/// Shows a horizontal segmented bar of workflow phase progression.
pub fn build_phase_timeline(
    snapshots: &[IterationSnapshot],
    setup_duration_ms: u64,
    setup_steps: usize,
    total_elapsed: std::time::Duration,
) -> Value {
    let total_ms = total_elapsed.as_millis() as u64;
    let mut phases: Vec<Value> = Vec::new();

    // Setup phase
    phases.push(json!({
        "name": "Setup",
        "duration_ms": setup_duration_ms,
        "status": "completed",
        "step_count": setup_steps,
    }));

    // Aggregate verification + agentic durations
    let mut verify_total_ms: u64 = 0;
    let mut verify_steps: usize = 0;
    let mut agentic_total_ms: u64 = 0;
    let mut agentic_count: usize = 0;
    let mut last_verify_failed = false;
    let mut last_agentic_failed = false;

    for snapshot in snapshots {
        verify_total_ms += snapshot.verify_duration_ms;
        verify_steps += snapshot.total;
        last_verify_failed = snapshot.failed > 0;

        if let Some(agentic_ms) = snapshot.agentic_duration_ms {
            agentic_total_ms += agentic_ms;
            agentic_count += 1;
            last_agentic_failed = snapshot.agentic_outcome.as_deref() == Some("Failed")
                || snapshot.agentic_outcome.as_deref() == Some("Error");
        }
    }

    // Verification phase
    let verify_status = if last_verify_failed {
        "failed"
    } else {
        "completed"
    };
    phases.push(json!({
        "name": "Verification",
        "duration_ms": verify_total_ms,
        "status": verify_status,
        "step_count": verify_steps,
    }));

    // Agentic phase
    if agentic_count > 0 {
        let agentic_status = if last_agentic_failed {
            "failed"
        } else {
            "completed"
        };
        phases.push(json!({
            "name": "Agentic",
            "duration_ms": agentic_total_ms,
            "status": agentic_status,
            "step_count": agentic_count,
        }));
    }

    json!({
        "phases": phases,
        "total_duration_ms": total_ms
    })
}

/// Build the Iteration Comparison panel (IterationComparison).
/// Shows pass/fail counts per iteration as grouped bars.
pub fn build_iteration_comparison(snapshots: &[IterationSnapshot]) -> Value {
    let iterations: Vec<Value> = snapshots
        .iter()
        .map(|s| {
            json!({
                "iteration": s.iteration,
                "passed": s.passed,
                "failed": s.failed,
                "total": s.total,
            })
        })
        .collect();

    json!({ "iterations": iterations })
}

/// Build the Step Duration Chart panel (StepDurationChart).
/// Shows the longest-running steps sorted by duration.
pub fn build_step_duration_chart(
    step_timings: &[(String, u64, &str, Option<&str>)], // (name, duration_ms, status, phase)
    max_steps: usize,
) -> Value {
    // Sort by duration descending, take top N
    let mut sorted = step_timings.to_vec();
    sorted.sort_by_key(|s| std::cmp::Reverse(s.1));
    sorted.truncate(max_steps);

    let max_duration = sorted.first().map(|s| s.1).unwrap_or(0);

    let steps: Vec<Value> = sorted
        .iter()
        .map(|(name, duration_ms, status, phase)| {
            let mut entry = json!({
                "name": name,
                "duration_ms": duration_ms,
                "status": status,
            });
            if let Some(p) = phase {
                entry["phase"] = json!(p);
            }
            entry
        })
        .collect();

    json!({
        "steps": steps,
        "max_duration_ms": max_duration
    })
}

/// Build the Phase Distribution panel (PhaseDistribution).
/// Shows a donut chart of time allocation across phases.
pub fn build_phase_distribution(
    snapshots: &[IterationSnapshot],
    setup_duration_ms: u64,
    total_elapsed: std::time::Duration,
) -> Value {
    let total_ms = total_elapsed.as_millis() as u64;
    if total_ms == 0 {
        return json!({
            "segments": [],
            "total_duration_ms": 0
        });
    }

    let mut segments: Vec<Value> = Vec::new();

    // Setup
    if setup_duration_ms > 0 {
        segments.push(json!({
            "phase": "Setup",
            "duration_ms": setup_duration_ms,
            "percentage": (setup_duration_ms as f64 / total_ms as f64) * 100.0,
            "color": "#60a5fa"
        }));
    }

    // Verification aggregate
    let verify_total: u64 = snapshots.iter().map(|s| s.verify_duration_ms).sum();
    if verify_total > 0 {
        segments.push(json!({
            "phase": "Verification",
            "duration_ms": verify_total,
            "percentage": (verify_total as f64 / total_ms as f64) * 100.0,
            "color": "#a78bfa"
        }));
    }

    // Agentic aggregate
    let agentic_total: u64 = snapshots.iter().filter_map(|s| s.agentic_duration_ms).sum();
    if agentic_total > 0 {
        segments.push(json!({
            "phase": "Agentic",
            "duration_ms": agentic_total,
            "percentage": (agentic_total as f64 / total_ms as f64) * 100.0,
            "color": "#4ade80"
        }));
    }

    // Overhead (unaccounted time)
    let accounted = setup_duration_ms + verify_total + agentic_total;
    if accounted < total_ms {
        let overhead = total_ms - accounted;
        segments.push(json!({
            "phase": "Overhead",
            "duration_ms": overhead,
            "percentage": (overhead as f64 / total_ms as f64) * 100.0,
            "color": "#6b7280"
        }));
    }

    json!({
        "segments": segments,
        "total_duration_ms": total_ms
    })
}

/// Build the Acceptance Criteria panel (AcceptanceCriteria).
/// Shows specification-derived criteria with live status tracking.
pub fn build_acceptance_criteria(
    acceptance_criteria: &Value,
    criterion_statuses: &HashMap<String, (String, Option<String>)>,
) -> Value {
    // Clone the source criteria and overlay current statuses
    let mut result = acceptance_criteria.clone();

    // Update each criterion's status from the live tracking map
    if let Some(criteria_array) = result.get_mut("criteria").and_then(|c| c.as_array_mut()) {
        for criterion in criteria_array.iter_mut() {
            if let Some(id) = criterion.get("id").and_then(|v| v.as_str()) {
                if let Some((status, last_error)) = criterion_statuses.get(id) {
                    criterion["status"] = json!(status);
                    if let Some(err) = last_error {
                        criterion["last_error"] = json!(err);
                    }
                } else {
                    // Default to pending if not tracked
                    criterion["status"] = json!("pending");
                }
            }
        }
    }

    result
}

// ─── Helpers ───

fn format_duration_ms(ms: u64) -> String {
    if ms < 1000 {
        format!("{}ms", ms)
    } else if ms < 60_000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else {
        let mins = ms / 60_000;
        let secs = (ms % 60_000) / 1000;
        format!("{}m {}s", mins, secs)
    }
}

fn format_duration(d: std::time::Duration) -> String {
    format_duration_ms(d.as_millis() as u64)
}

/// Public wrapper for format_duration (used by mod.rs).
pub fn format_duration_pub(d: std::time::Duration) -> String {
    format_duration(d)
}

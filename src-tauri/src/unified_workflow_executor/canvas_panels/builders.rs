//! Pure builder functions for canvas panel data.
//!
//! Each function produces a `serde_json::Value` payload ready for a specific
//! panel component type. No side effects — easily testable.

use serde_json::{json, Value};

use super::IterationSnapshot;
use crate::orchestrator::types::Finding;
use crate::unified_workflow_executor::types::LoopConfig;
use crate::unified_workflow_executor::types::LoopResult;
use std::collections::HashMap;

/// Build the Mission Brief panel (Markdown).
pub fn build_mission_brief(config: &LoopConfig) -> Value {
    let stages_count = if config.stages.is_empty() {
        1
    } else {
        config.stages.len()
    };
    let reflection = if config.reflection_mode { "On" } else { "Off" };
    let approval = if config.approval_gate { "On" } else { "Off" };
    let model = config
        .model_override
        .as_deref()
        .unwrap_or("default")
        .to_string();

    let content = format!(
        "## {}\n\n\
         | | |\n\
         |---|---|\n\
         | **Max Iterations** | {} |\n\
         | **Stages** | {} |\n\
         | **Reflection** | {} |\n\
         | **Approval Gate** | {} |\n\
         | **Model** | {} |",
        config.workflow_name, config.max_iterations, stages_count, reflection, approval, model,
    );

    json!({ "content": content })
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
                format!("{}…", &error[..120])
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
                format!("{}…", &error[..120])
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

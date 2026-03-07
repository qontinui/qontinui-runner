//! Storage operations for the Tiered Information Model.
//!
//! Provides CRUD operations for task_run_automation and config_statistics tables.

use crate::str_utils::truncate_str;
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use tracing::{debug, info};

use super::types::{
    ConfigStatistics, DebuggingContext, FlakyItem, RunDetails, RunFailureSummary, RunStatus,
};

// ==============================================================================
// Task Run Automation Operations (Unified TaskRun Architecture)
// ==============================================================================

/// Get recent automation runs from task_run_automation table for a config.
/// Joins with task_runs to get config_id.
pub fn get_recent_automation_runs(
    conn: &Connection,
    config_id: &str,
    limit: u32,
) -> Result<Vec<RunDetails>, String> {
    let mut stmt = conn
        .prepare(
            r#"
            SELECT
                tra.id, tr.config_id, tra.workflow_name, tra.started_at, tra.ended_at, tra.duration_ms,
                tra.automation_status, tra.success, tra.error_type, tra.error_message,
                tra.actions_summary, tra.states_visited, tra.transitions_executed,
                tra.template_matches, tra.anomalies
            FROM task_run_automation tra
            INNER JOIN task_runs tr ON tra.task_run_id = tr.id
            WHERE tr.config_id = ?1
            ORDER BY tra.started_at DESC
            LIMIT ?2
            "#,
        )
        .map_err(|e| format!("Failed to prepare query: {}", e))?;

    let runs = stmt
        .query_map(
            params![config_id, limit],
            row_to_run_details_from_automation,
        )
        .map_err(|e| format!("Failed to query automation runs: {}", e))?
        .filter_map(|r| r.ok())
        .collect();

    Ok(runs)
}

/// Get failed automation runs from task_run_automation table for a config.
pub fn get_failed_automation_runs(
    conn: &Connection,
    config_id: &str,
    limit: u32,
) -> Result<Vec<RunDetails>, String> {
    let mut stmt = conn
        .prepare(
            r#"
            SELECT
                tra.id, tr.config_id, tra.workflow_name, tra.started_at, tra.ended_at, tra.duration_ms,
                tra.automation_status, tra.success, tra.error_type, tra.error_message,
                tra.actions_summary, tra.states_visited, tra.transitions_executed,
                tra.template_matches, tra.anomalies
            FROM task_run_automation tra
            INNER JOIN task_runs tr ON tra.task_run_id = tr.id
            WHERE tr.config_id = ?1 AND (tra.automation_status = 'failed' OR tra.automation_status = 'timeout')
            ORDER BY tra.started_at DESC
            LIMIT ?2
            "#,
        )
        .map_err(|e| format!("Failed to prepare query: {}", e))?;

    let runs = stmt
        .query_map(
            params![config_id, limit],
            row_to_run_details_from_automation,
        )
        .map_err(|e| format!("Failed to query failed automation runs: {}", e))?
        .filter_map(|r| r.ok())
        .collect();

    Ok(runs)
}

/// Get all recent runs from task_run_automation.
pub fn get_all_recent_runs(
    conn: &Connection,
    config_id: &str,
    limit: u32,
) -> Result<Vec<RunDetails>, String> {
    get_recent_automation_runs(conn, config_id, limit)
}

/// Get all failed runs from task_run_automation.
pub fn get_all_failed_runs(
    conn: &Connection,
    config_id: &str,
    limit: u32,
) -> Result<Vec<RunDetails>, String> {
    get_failed_automation_runs(conn, config_id, limit)
}

// ==============================================================================
// Config Statistics Operations (Tier 4)
// ==============================================================================

/// Get or create config statistics.
pub fn get_or_create_config_statistics(
    conn: &Connection,
    config_id: &str,
) -> Result<ConfigStatistics, String> {
    if let Some(stats) = get_config_statistics(conn, config_id)? {
        return Ok(stats);
    }

    // Create new statistics
    let stats = ConfigStatistics::new(config_id.to_string());
    insert_config_statistics(conn, &stats)?;
    Ok(stats)
}

/// Get config statistics by config_id.
pub fn get_config_statistics(
    conn: &Connection,
    config_id: &str,
) -> Result<Option<ConfigStatistics>, String> {
    let result = conn
        .query_row(
            r#"
            SELECT id, config_id, config_hash, total_runs, successful_runs, failed_runs, timeout_runs,
                   avg_duration_ms, recent_success_rate, recent_avg_duration_ms,
                   transition_stats, template_stats, state_stats, error_patterns,
                   flaky_transitions, flaky_templates, first_run_at, last_run_at, last_updated_at
            FROM config_statistics
            WHERE config_id = ?1
            "#,
            params![config_id],
            row_to_config_statistics,
        )
        .optional()
        .map_err(|e| format!("Failed to get config_statistics: {}", e))?;

    Ok(result)
}

/// Insert new config statistics.
pub fn insert_config_statistics(conn: &Connection, stats: &ConfigStatistics) -> Result<(), String> {
    let transition_json =
        serde_json::to_string(&stats.transition_stats).unwrap_or_else(|_| "{}".into());
    let template_json =
        serde_json::to_string(&stats.template_stats).unwrap_or_else(|_| "{}".into());
    let state_json = serde_json::to_string(&stats.state_stats).unwrap_or_else(|_| "{}".into());
    let error_json = serde_json::to_string(&stats.error_patterns).unwrap_or_else(|_| "{}".into());
    let flaky_trans_json =
        serde_json::to_string(&stats.flaky_transitions).unwrap_or_else(|_| "[]".into());
    let flaky_templ_json =
        serde_json::to_string(&stats.flaky_templates).unwrap_or_else(|_| "[]".into());

    conn.execute(
        r#"
        INSERT INTO config_statistics (
            id, config_id, config_hash, total_runs, successful_runs, failed_runs, timeout_runs,
            avg_duration_ms, recent_success_rate, recent_avg_duration_ms,
            transition_stats, template_stats, state_stats, error_patterns,
            flaky_transitions, flaky_templates, first_run_at, last_run_at, last_updated_at
        ) VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6, ?7,
            ?8, ?9, ?10,
            ?11, ?12, ?13, ?14,
            ?15, ?16, ?17, ?18, ?19
        )
        "#,
        params![
            stats.id,
            stats.config_id,
            stats.config_hash,
            stats.total_runs,
            stats.successful_runs,
            stats.failed_runs,
            stats.timeout_runs,
            stats.avg_duration_ms,
            stats.recent_success_rate,
            stats.recent_avg_duration_ms,
            transition_json,
            template_json,
            state_json,
            error_json,
            flaky_trans_json,
            flaky_templ_json,
            stats.first_run_at,
            stats.last_run_at,
            stats.last_updated_at,
        ],
    )
    .map_err(|e| format!("Failed to insert config_statistics: {}", e))?;

    debug!("Inserted config_statistics for config {}", stats.config_id);
    Ok(())
}

/// Update config statistics.
pub fn update_config_statistics(conn: &Connection, stats: &ConfigStatistics) -> Result<(), String> {
    let transition_json =
        serde_json::to_string(&stats.transition_stats).unwrap_or_else(|_| "{}".into());
    let template_json =
        serde_json::to_string(&stats.template_stats).unwrap_or_else(|_| "{}".into());
    let state_json = serde_json::to_string(&stats.state_stats).unwrap_or_else(|_| "{}".into());
    let error_json = serde_json::to_string(&stats.error_patterns).unwrap_or_else(|_| "{}".into());
    let flaky_trans_json =
        serde_json::to_string(&stats.flaky_transitions).unwrap_or_else(|_| "[]".into());
    let flaky_templ_json =
        serde_json::to_string(&stats.flaky_templates).unwrap_or_else(|_| "[]".into());

    conn.execute(
        r#"
        UPDATE config_statistics SET
            config_hash = ?2,
            total_runs = ?3,
            successful_runs = ?4,
            failed_runs = ?5,
            timeout_runs = ?6,
            avg_duration_ms = ?7,
            recent_success_rate = ?8,
            recent_avg_duration_ms = ?9,
            transition_stats = ?10,
            template_stats = ?11,
            state_stats = ?12,
            error_patterns = ?13,
            flaky_transitions = ?14,
            flaky_templates = ?15,
            last_run_at = ?16,
            last_updated_at = ?17
        WHERE config_id = ?1
        "#,
        params![
            stats.config_id,
            stats.config_hash,
            stats.total_runs,
            stats.successful_runs,
            stats.failed_runs,
            stats.timeout_runs,
            stats.avg_duration_ms,
            stats.recent_success_rate,
            stats.recent_avg_duration_ms,
            transition_json,
            template_json,
            state_json,
            error_json,
            flaky_trans_json,
            flaky_templ_json,
            stats.last_run_at,
            stats.last_updated_at,
        ],
    )
    .map_err(|e| format!("Failed to update config_statistics: {}", e))?;

    debug!("Updated config_statistics for config {}", stats.config_id);
    Ok(())
}

/// Update statistics after a run completes.
/// This is the main entry point for updating Tier 4 data.
pub fn update_statistics_after_run(conn: &Connection, run: &RunDetails) -> Result<(), String> {
    let mut stats = get_or_create_config_statistics(conn, &run.config_id)?;
    let now = Utc::now().to_rfc3339();

    // Update run counts
    stats.total_runs += 1;
    match run.status {
        RunStatus::Completed => {
            if run.success.unwrap_or(true) {
                stats.successful_runs += 1;
            } else {
                stats.failed_runs += 1;
            }
        }
        RunStatus::Failed => stats.failed_runs += 1,
        RunStatus::Timeout => stats.timeout_runs += 1,
        RunStatus::Cancelled => {} // Don't count cancelled runs
        RunStatus::Running => {}   // Shouldn't happen
    }

    // Update timestamps
    if stats.first_run_at.is_none() {
        stats.first_run_at = Some(run.started_at.clone());
    }
    stats.last_run_at = Some(run.started_at.clone());
    stats.last_updated_at = Some(now);

    // Update average duration
    if let Some(duration) = run.duration_ms {
        if let Some(avg) = stats.avg_duration_ms {
            // Running average
            stats.avg_duration_ms =
                Some((avg * (stats.total_runs - 1) as u64 + duration) / stats.total_runs as u64);
        } else {
            stats.avg_duration_ms = Some(duration);
        }
    }

    // Update transition stats
    for tr in &run.transitions_executed {
        let key = format!("{}|{}", tr.from_state, tr.to_state);
        let entry = stats.transition_stats.entry(key).or_default();
        entry.total += 1;
        if tr.success {
            entry.success += 1;
        } else {
            entry.failure += 1;
            entry.last_failure = Some(run.started_at.clone());
        }
        // Update average duration
        if entry.avg_duration_ms == 0 {
            entry.avg_duration_ms = tr.duration_ms;
        } else {
            entry.avg_duration_ms = (entry.avg_duration_ms * (entry.total - 1) as u64
                + tr.duration_ms)
                / entry.total as u64;
        }
    }

    // Update template stats
    for tm in &run.template_matches {
        let entry = stats.template_stats.entry(tm.template.clone()).or_default();
        entry.total += tm.count;
        entry.matches += tm.count - tm.failures;
        entry.failures += tm.failures;
        // Update average confidence
        if entry.avg_confidence == 0.0 {
            entry.avg_confidence = tm.avg_confidence;
        } else {
            let total_samples = entry.total;
            entry.avg_confidence = (entry.avg_confidence * (total_samples - tm.count) as f32
                + tm.avg_confidence * tm.count as f32)
                / total_samples as f32;
        }
    }

    // Update state stats
    for state in &run.states_visited {
        let entry = stats.state_stats.entry(state.clone()).or_default();
        entry.visits += 1;
    }

    // Update error patterns
    if let Some(error_type) = &run.error_type {
        let entry = stats.error_patterns.entry(error_type.clone()).or_default();
        entry.count += 1;
        entry.last_seen = Some(run.started_at.clone());

        // Keep only last 5 error contexts
        if let Some(msg) = &run.error_message {
            if entry.contexts.len() >= 5 {
                entry.contexts.remove(0);
            }
            entry.contexts.push(msg.clone());
        }
    }

    // Recalculate flaky items (20% threshold - items with 20-80% success rate)
    stats.recalculate_flaky_transitions(0.2);
    stats.recalculate_flaky_templates(0.2);

    // Calculate recent success rate (last 10 runs from both tables)
    let recent_runs = get_all_recent_runs(conn, &run.config_id, 10)?;
    if !recent_runs.is_empty() {
        let recent_success = recent_runs
            .iter()
            .filter(|r| r.success.unwrap_or(false))
            .count();
        stats.recent_success_rate = Some(recent_success as f64 / recent_runs.len() as f64);

        // Recent average duration
        let durations: Vec<u64> = recent_runs.iter().filter_map(|r| r.duration_ms).collect();
        if !durations.is_empty() {
            stats.recent_avg_duration_ms =
                Some(durations.iter().sum::<u64>() / durations.len() as u64);
        }
    }

    // Save updated statistics
    update_config_statistics(conn, &stats)?;

    info!(
        "Updated statistics for config {}: {} runs, {:.1}% success rate",
        stats.config_id,
        stats.total_runs,
        stats.success_rate() * 100.0
    );

    Ok(())
}

// ==============================================================================
// Debugging Context (for AI prompts)
// ==============================================================================

/// Get flaky transitions with details.
pub fn get_flaky_transitions(
    conn: &Connection,
    config_id: &str,
    threshold: f64,
) -> Result<Vec<FlakyItem>, String> {
    let stats = get_config_statistics(conn, config_id)?;

    match stats {
        Some(s) => {
            let items = s
                .transition_stats
                .iter()
                .filter(|(_, ts)| ts.is_flaky(threshold))
                .map(|(key, ts)| FlakyItem {
                    name: key.clone(),
                    item_type: "transition".to_string(),
                    success_rate: ts.success_rate(),
                    total_attempts: ts.total,
                    last_failure: ts.last_failure.clone(),
                    details: serde_json::to_value(ts).ok(),
                })
                .collect();
            Ok(items)
        }
        None => Ok(Vec::new()),
    }
}

/// Get flaky templates with details.
pub fn get_flaky_templates(
    conn: &Connection,
    config_id: &str,
    threshold: f64,
) -> Result<Vec<FlakyItem>, String> {
    let stats = get_config_statistics(conn, config_id)?;

    match stats {
        Some(s) => {
            let items = s
                .template_stats
                .iter()
                .filter(|(_, ts)| ts.is_flaky(threshold))
                .map(|(key, ts)| FlakyItem {
                    name: key.clone(),
                    item_type: "template".to_string(),
                    success_rate: ts.match_rate(),
                    total_attempts: ts.total,
                    last_failure: None,
                    details: serde_json::to_value(ts).ok(),
                })
                .collect();
            Ok(items)
        }
        None => Ok(Vec::new()),
    }
}

/// Build debugging context for AI prompts.
/// This formats Tier 4 data in a way that's useful for AI analysis.
/// Uses data from task_run_automation table.
pub fn get_debugging_context(
    conn: &Connection,
    config_id: &str,
    config_name: Option<String>,
) -> Result<DebuggingContext, String> {
    let stats = get_config_statistics(conn, config_id)?;
    // Get failed runs from task_run_automation
    let failed_runs = get_all_failed_runs(conn, config_id, 5)?;

    let (
        total_runs,
        success_rate,
        recent_success_rate,
        avg_duration_ms,
        flaky_trans,
        flaky_templ,
        common_errors,
    ) = match stats {
        Some(s) => {
            let trans = get_flaky_transitions(conn, config_id, 0.2)?;
            let templ = get_flaky_templates(conn, config_id, 0.2)?;

            // Get top 5 most common errors
            let mut errors: Vec<(String, u32)> = s
                .error_patterns
                .iter()
                .map(|(k, v)| (k.clone(), v.count))
                .collect();
            errors.sort_by(|a, b| b.1.cmp(&a.1));
            errors.truncate(5);

            (
                s.total_runs,
                s.success_rate(),
                s.recent_success_rate,
                s.avg_duration_ms,
                trans,
                templ,
                errors,
            )
        }
        None => (0, 0.0, None, None, Vec::new(), Vec::new(), Vec::new()),
    };

    // Convert failed runs to summaries
    let recent_failures: Vec<RunFailureSummary> = failed_runs
        .into_iter()
        .map(|r| {
            // Find the failed transition/template if any
            let failed_transition = r
                .transitions_executed
                .iter()
                .find(|t| !t.success)
                .map(|t| format!("{}|{}", t.from_state, t.to_state));

            let failed_template = r
                .template_matches
                .iter()
                .find(|t| t.failures > 0)
                .map(|t| t.template.clone());

            RunFailureSummary {
                run_id: r.id,
                timestamp: r.started_at,
                error_type: r.error_type,
                error_message: r.error_message,
                failed_transition,
                failed_template,
            }
        })
        .collect();

    Ok(DebuggingContext {
        config_id: config_id.to_string(),
        config_name,
        total_runs,
        success_rate,
        recent_success_rate,
        avg_duration_ms,
        flaky_transitions: flaky_trans,
        flaky_templates: flaky_templ,
        common_errors,
        recent_failures,
    })
}

/// Format debugging context as a markdown string for AI prompts.
pub fn format_debugging_context_for_prompt(context: &DebuggingContext) -> String {
    let mut sections = Vec::new();

    sections.push("## Automation Run Statistics\n".to_string());

    // Header
    let config_display = context.config_name.as_ref().unwrap_or(&context.config_id);
    sections.push(format!("**Config:** {}\n", config_display));
    sections.push(format!(
        "**Total Runs:** {} | **Success Rate:** {:.1}%\n",
        context.total_runs,
        context.success_rate * 100.0
    ));

    if let Some(recent_rate) = context.recent_success_rate {
        sections.push(format!(
            "**Recent Success Rate (last 10):** {:.1}%\n",
            recent_rate * 100.0
        ));
    }

    if let Some(avg_ms) = context.avg_duration_ms {
        sections.push(format!("**Avg Duration:** {}ms\n", avg_ms));
    }

    sections.push(String::new());

    // Flaky transitions
    if !context.flaky_transitions.is_empty() {
        sections.push("### Flaky Transitions (unreliable - 20-80% success rate)\n".to_string());
        for item in &context.flaky_transitions {
            sections.push(format!(
                "- **{}**: {:.1}% success ({} attempts)\n",
                item.name,
                item.success_rate * 100.0,
                item.total_attempts
            ));
        }
        sections.push(String::new());
    }

    // Flaky templates
    if !context.flaky_templates.is_empty() {
        sections.push("### Flaky Templates (unreliable matching)\n".to_string());
        for item in &context.flaky_templates {
            sections.push(format!(
                "- **{}**: {:.1}% match rate ({} attempts)\n",
                item.name,
                item.success_rate * 100.0,
                item.total_attempts
            ));
        }
        sections.push(String::new());
    }

    // Common errors
    if !context.common_errors.is_empty() {
        sections.push("### Common Errors\n".to_string());
        for (error, count) in &context.common_errors {
            sections.push(format!("- **{}**: {} occurrences\n", error, count));
        }
        sections.push(String::new());
    }

    // Recent failures
    if !context.recent_failures.is_empty() {
        sections.push("### Recent Failures\n".to_string());
        for failure in &context.recent_failures {
            sections.push(format!(
                "- **{}** ({})\n",
                failure.run_id, failure.timestamp
            ));
            if let Some(err_type) = &failure.error_type {
                sections.push(format!("  Type: {}\n", err_type));
            }
            if let Some(msg) = &failure.error_message {
                let truncated = if msg.len() > 200 {
                    format!("{}...", truncate_str(msg, 200))
                } else {
                    msg.clone()
                };
                sections.push(format!("  Message: {}\n", truncated));
            }
            if let Some(trans) = &failure.failed_transition {
                sections.push(format!("  Failed Transition: {}\n", trans));
            }
            if let Some(templ) = &failure.failed_template {
                sections.push(format!("  Failed Template: {}\n", templ));
            }
        }
    }

    sections.join("")
}

/// Get a single run by ID from run_details table (used by tests).
/// This function works with the legacy run_details table schema.
pub fn get_run_details(conn: &Connection, run_id: &str) -> Result<Option<RunDetails>, String> {
    let result = conn
        .query_row(
            r#"
            SELECT
                id, config_id, workflow_name, started_at, ended_at, duration_ms,
                status, success, error_type, error_message,
                actions_summary, states_visited, transitions_executed,
                template_matches, anomalies
            FROM run_details
            WHERE id = ?1
            "#,
            params![run_id],
            |row| {
                let status_str: String = row.get(6)?;
                let actions_json: Option<String> = row.get(10)?;
                let states_json: Option<String> = row.get(11)?;
                let transitions_json: Option<String> = row.get(12)?;
                let templates_json: Option<String> = row.get(13)?;
                let anomalies_json: Option<String> = row.get(14)?;

                Ok(RunDetails {
                    id: row.get(0)?,
                    config_id: row.get(1)?,
                    workflow_name: row.get(2)?,
                    started_at: row.get(3)?,
                    ended_at: row.get(4)?,
                    duration_ms: row.get(5)?,
                    status: RunStatus::from_str(&status_str).unwrap_or(RunStatus::Running),
                    success: row.get(7)?,
                    error_type: row.get(8)?,
                    error_message: row.get(9)?,
                    actions_summary: actions_json.and_then(|j| serde_json::from_str(&j).ok()),
                    states_visited: states_json
                        .and_then(|j| serde_json::from_str(&j).ok())
                        .unwrap_or_default(),
                    transitions_executed: transitions_json
                        .and_then(|j| serde_json::from_str(&j).ok())
                        .unwrap_or_default(),
                    template_matches: templates_json
                        .and_then(|j| serde_json::from_str(&j).ok())
                        .unwrap_or_default(),
                    anomalies: anomalies_json
                        .and_then(|j| serde_json::from_str(&j).ok())
                        .unwrap_or_default(),
                    screenshots: Vec::new(),
                })
            },
        )
        .optional()
        .map_err(|e| format!("Failed to get run details: {}", e))?;

    Ok(result)
}

// ==============================================================================
// Helper Functions
// ==============================================================================

/// Convert a row from task_run_automation (joined with task_runs) to RunDetails.
/// Used for unified statistics aggregation.
fn row_to_run_details_from_automation(row: &rusqlite::Row) -> rusqlite::Result<RunDetails> {
    let status_str: String = row.get(6)?;
    let actions_json: Option<String> = row.get(10)?;
    let states_json: Option<String> = row.get(11)?;
    let transitions_json: Option<String> = row.get(12)?;
    let templates_json: Option<String> = row.get(13)?;
    let anomalies_json: Option<String> = row.get(14)?;

    Ok(RunDetails {
        id: row.get(0)?,
        config_id: row.get(1)?,
        workflow_name: row.get(2)?,
        started_at: row.get(3)?,
        ended_at: row.get(4)?,
        duration_ms: row.get(5)?,
        // automation_status maps to status (same values)
        status: RunStatus::from_str(&status_str).unwrap_or(RunStatus::Running),
        success: row.get(7)?,
        error_type: row.get(8)?,
        error_message: row.get(9)?,
        actions_summary: actions_json.and_then(|j| serde_json::from_str(&j).ok()),
        states_visited: states_json
            .and_then(|j| serde_json::from_str(&j).ok())
            .unwrap_or_default(),
        transitions_executed: transitions_json
            .and_then(|j| serde_json::from_str(&j).ok())
            .unwrap_or_default(),
        template_matches: templates_json
            .and_then(|j| serde_json::from_str(&j).ok())
            .unwrap_or_default(),
        anomalies: anomalies_json
            .and_then(|j| serde_json::from_str(&j).ok())
            .unwrap_or_default(),
        // Screenshots are tracked in-memory during runs; legacy DB records don't have them
        screenshots: Vec::new(),
    })
}

fn row_to_config_statistics(row: &rusqlite::Row) -> rusqlite::Result<ConfigStatistics> {
    let transition_json: Option<String> = row.get(10)?;
    let template_json: Option<String> = row.get(11)?;
    let state_json: Option<String> = row.get(12)?;
    let error_json: Option<String> = row.get(13)?;
    let flaky_trans_json: Option<String> = row.get(14)?;
    let flaky_templ_json: Option<String> = row.get(15)?;

    Ok(ConfigStatistics {
        id: row.get(0)?,
        config_id: row.get(1)?,
        config_hash: row.get(2)?,
        total_runs: row.get(3)?,
        successful_runs: row.get(4)?,
        failed_runs: row.get(5)?,
        timeout_runs: row.get(6)?,
        avg_duration_ms: row.get(7)?,
        recent_success_rate: row.get(8)?,
        recent_avg_duration_ms: row.get(9)?,
        transition_stats: transition_json
            .and_then(|j| serde_json::from_str(&j).ok())
            .unwrap_or_default(),
        template_stats: template_json
            .and_then(|j| serde_json::from_str(&j).ok())
            .unwrap_or_default(),
        state_stats: state_json
            .and_then(|j| serde_json::from_str(&j).ok())
            .unwrap_or_default(),
        error_patterns: error_json
            .and_then(|j| serde_json::from_str(&j).ok())
            .unwrap_or_default(),
        flaky_transitions: flaky_trans_json
            .and_then(|j| serde_json::from_str(&j).ok())
            .unwrap_or_default(),
        flaky_templates: flaky_templ_json
            .and_then(|j| serde_json::from_str(&j).ok())
            .unwrap_or_default(),
        first_run_at: row.get(16)?,
        last_run_at: row.get(17)?,
        last_updated_at: row.get(18)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tiered_info::{TemplateMatchRecord, TransitionRecord, TransitionStats};
    use rusqlite::Connection;

    fn create_test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS task_runs (
                id TEXT PRIMARY KEY,
                task_name TEXT NOT NULL,
                prompt TEXT,
                task_type TEXT NOT NULL DEFAULT 'task',
                status TEXT NOT NULL DEFAULT 'running',
                sessions_count INTEGER NOT NULL DEFAULT 0,
                max_sessions INTEGER,
                auto_continue BOOLEAN NOT NULL DEFAULT 1,
                output_log TEXT DEFAULT '',
                error_message TEXT,
                execution_steps_json TEXT,
                log_sources_json TEXT,
                config_id TEXT,
                workflow_name TEXT,
                summary TEXT,
                goal_achieved BOOLEAN,
                remaining_work TEXT,
                summary_generated_at TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                completed_at TEXT
            );

            CREATE TABLE IF NOT EXISTS task_run_automation (
                id TEXT PRIMARY KEY,
                task_run_id TEXT NOT NULL,
                workflow_name TEXT,
                started_at TEXT NOT NULL,
                ended_at TEXT,
                duration_ms INTEGER,
                automation_status TEXT NOT NULL DEFAULT 'running',
                success BOOLEAN,
                error_type TEXT,
                error_message TEXT,
                actions_summary TEXT,
                states_visited TEXT,
                transitions_executed TEXT,
                template_matches TEXT,
                anomalies TEXT,
                iteration_number INTEGER NOT NULL DEFAULT 1,
                FOREIGN KEY (task_run_id) REFERENCES task_runs(id) ON DELETE CASCADE
            );

            CREATE TABLE IF NOT EXISTS config_statistics (
                id TEXT PRIMARY KEY,
                config_id TEXT NOT NULL UNIQUE,
                config_hash TEXT,
                total_runs INTEGER DEFAULT 0,
                successful_runs INTEGER DEFAULT 0,
                failed_runs INTEGER DEFAULT 0,
                timeout_runs INTEGER DEFAULT 0,
                avg_duration_ms INTEGER,
                recent_success_rate REAL,
                recent_avg_duration_ms INTEGER,
                transition_stats TEXT,
                template_stats TEXT,
                state_stats TEXT,
                error_patterns TEXT,
                flaky_transitions TEXT,
                flaky_templates TEXT,
                first_run_at TEXT,
                last_run_at TEXT,
                last_updated_at TEXT
            );
            "#,
        )
        .unwrap();
        conn
    }

    #[test]
    fn test_config_statistics() {
        let conn = create_test_db();

        // Get or create
        let stats = get_or_create_config_statistics(&conn, "config-1").unwrap();
        assert_eq!(stats.config_id, "config-1");
        assert_eq!(stats.total_runs, 0);

        // Should return same stats
        let stats2 = get_or_create_config_statistics(&conn, "config-1").unwrap();
        assert_eq!(stats.id, stats2.id);
    }

    #[test]
    fn test_update_statistics_after_run() {
        let conn = create_test_db();

        // Create a successful run (in-memory, statistics work without DB insert)
        let mut run = RunDetails::new("run-1".into(), "config-1".into(), None);
        run.status = RunStatus::Completed;
        run.success = Some(true);
        run.duration_ms = Some(1000);
        run.transitions_executed = vec![TransitionRecord {
            from_state: "StateA".into(),
            to_state: "StateB".into(),
            action: "click".into(),
            success: true,
            duration_ms: 200,
            error: None,
        }];
        run.template_matches = vec![TemplateMatchRecord {
            template: "button.png".into(),
            count: 5,
            avg_confidence: 0.95,
            failures: 0,
        }];
        run.states_visited = vec!["StateA".into(), "StateB".into()];

        // update_statistics_after_run works with in-memory RunDetails
        update_statistics_after_run(&conn, &run).unwrap();

        let stats = get_config_statistics(&conn, "config-1").unwrap().unwrap();
        assert_eq!(stats.total_runs, 1);
        assert_eq!(stats.successful_runs, 1);
        assert_eq!(stats.avg_duration_ms, Some(1000));
        assert!(stats.transition_stats.contains_key("StateA|StateB"));
        assert!(stats.template_stats.contains_key("button.png"));

        // Add a failed run
        let mut run2 = RunDetails::new("run-2".into(), "config-1".into(), None);
        run2.status = RunStatus::Failed;
        run2.success = Some(false);
        run2.duration_ms = Some(500);
        run2.error_type = Some("TemplateNotFound".into());
        run2.error_message = Some("Could not find button.png".into());
        run2.transitions_executed = vec![TransitionRecord {
            from_state: "StateA".into(),
            to_state: "StateB".into(),
            action: "click".into(),
            success: false,
            duration_ms: 500,
            error: Some("Template not found".into()),
        }];

        update_statistics_after_run(&conn, &run2).unwrap();

        let stats2 = get_config_statistics(&conn, "config-1").unwrap().unwrap();
        assert_eq!(stats2.total_runs, 2);
        assert_eq!(stats2.successful_runs, 1);
        assert_eq!(stats2.failed_runs, 1);
        assert_eq!(stats2.success_rate(), 0.5);

        // Check transition stats
        let trans_stats = stats2.transition_stats.get("StateA|StateB").unwrap();
        assert_eq!(trans_stats.total, 2);
        assert_eq!(trans_stats.success, 1);
        assert_eq!(trans_stats.failure, 1);

        // Check error patterns
        assert!(stats2.error_patterns.contains_key("TemplateNotFound"));
    }

    #[test]
    fn test_flaky_detection() {
        let mut stats = TransitionStats::default();
        stats.total = 10;
        stats.success = 5;
        stats.failure = 5;

        // 50% success rate should be flaky
        assert!(stats.is_flaky(0.2));

        stats.success = 9;
        stats.failure = 1;
        // 90% success rate should not be flaky
        assert!(!stats.is_flaky(0.2));

        stats.success = 1;
        stats.failure = 9;
        // 10% success rate should not be flaky (consistently failing)
        assert!(!stats.is_flaky(0.2));
    }

    #[test]
    fn test_debugging_context() {
        let conn = create_test_db();

        // Create statistics by directly calling update_statistics_after_run
        for i in 0..5 {
            let mut run = RunDetails::new(format!("run-{}", i), "config-1".into(), None);
            run.status = if i % 2 == 0 {
                RunStatus::Completed
            } else {
                RunStatus::Failed
            };
            run.success = Some(i % 2 == 0);
            run.duration_ms = Some(1000 + i as u64 * 100);
            if i % 2 == 1 {
                run.error_type = Some("TestError".into());
                run.error_message = Some(format!("Error in run {}", i));
            }
            update_statistics_after_run(&conn, &run).unwrap();
        }

        let context = get_debugging_context(&conn, "config-1", Some("Test Config".into())).unwrap();

        assert_eq!(context.total_runs, 5);
        assert_eq!(context.config_name, Some("Test Config".into()));

        // Format for prompt
        let prompt = format_debugging_context_for_prompt(&context);
        assert!(prompt.contains("## Automation Run Statistics"));
        assert!(prompt.contains("Test Config"));
    }
}

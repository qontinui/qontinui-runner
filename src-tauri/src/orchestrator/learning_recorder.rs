//! Learning outcome and pattern recorder.
//!
//! Records workflow execution outcomes to the `learning_outcomes` and
//! `learning_patterns` tables for use by the self-improvement analyzer.
//! These tables were previously dormant — this module activates them.

use chrono::Utc;
use rusqlite::{params, Connection};
use tracing::{debug, info};
use uuid::Uuid;

/// Outcome of a workflow execution for learning purposes.
pub struct WorkflowOutcome {
    pub task_run_id: String,
    pub workflow_name: String,
    pub category: String,
    pub status: String,
    pub duration_secs: f64,
    pub iterations: u32,
    pub verification_passed: bool,
    pub max_iterations_reached: bool,
    pub was_stopped: bool,
    pub tools_used: Vec<String>,
    pub files_modified: Vec<String>,
    pub error_type: Option<String>,
    pub error_message: Option<String>,
    pub workflow_architecture: Option<String>,
    pub step_count: Option<i64>,
    pub verification_step_count: Option<i64>,
    pub agentic_step_count: Option<i64>,
    pub has_ui_bridge: bool,
    pub total_tokens: Option<u64>,
    pub total_cost_usd: Option<f64>,
}

/// Categorize an error message into a standard error type.
///
/// Matches patterns from meta_optimizer_api.rs top failure patterns query.
pub fn categorize_error(msg: &str) -> String {
    let lower = msg.to_lowercase();
    if lower.contains("max iterations") {
        "max_iterations_reached"
    } else if lower.contains("max sessions") {
        "max_sessions_reached"
    } else if lower.contains("generation") || lower.contains("schema") {
        "generation_error"
    } else if lower.contains("timeout") {
        "timeout"
    } else {
        "runtime_error"
    }
    .to_string()
}

/// Record a learning outcome from a completed workflow execution.
///
/// Writes to the `learning_outcomes` table with execution metrics and
/// optionally computes a context embedding for semantic retrieval.
/// Automatically enriches error_type from error_message if not already set.
pub fn record_learning_outcome(
    conn: &Connection,
    outcome: &WorkflowOutcome,
) -> Result<String, String> {
    let id = Uuid::new_v4().to_string();
    let now = Utc::now().to_rfc3339();

    let status = if outcome.verification_passed {
        "success"
    } else if outcome.was_stopped {
        "partial"
    } else {
        "failure"
    };

    let tools_json = serde_json::to_string(&outcome.tools_used).unwrap_or_else(|_| "[]".into());
    let files_json = serde_json::to_string(&outcome.files_modified).unwrap_or_else(|_| "[]".into());

    // Build a strategy description from the execution parameters
    let strategy = format!(
        "{}:{} (max_iter={}, cat={})",
        outcome.workflow_name,
        if outcome.verification_passed {
            "pass"
        } else {
            "fail"
        },
        outcome.iterations,
        outcome.category,
    );

    // Auto-enrich error_type from error_message if not already set
    let error_type = outcome.error_type.clone().or_else(|| {
        outcome
            .error_message
            .as_ref()
            .filter(|msg| !msg.is_empty())
            .map(|msg| categorize_error(msg))
    });

    // Ensure workflow_architecture is never NULL — default to "traditional"
    let architecture = outcome
        .workflow_architecture
        .as_deref()
        .unwrap_or("traditional");

    conn.execute(
        r#"INSERT INTO learning_outcomes
            (id, task_id, status, duration_secs, iterations, strategy,
             tools_used, files_modified, error_type, error_message, feedback,
             workflow_architecture, step_count, verification_step_count,
             agentic_step_count, has_ui_bridge, total_tokens, total_cost_usd, created_at)
           VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)"#,
        params![
            id,
            outcome.task_run_id,
            status,
            outcome.duration_secs,
            outcome.iterations as i64,
            strategy,
            tools_json,
            files_json,
            error_type,
            outcome.error_message,
            "[]", // feedback starts empty, populated later via feedback.rs
            architecture,
            outcome.step_count,
            outcome.verification_step_count,
            outcome.agentic_step_count,
            outcome.has_ui_bridge as i32,
            outcome.total_tokens.map(|t| t as i64),
            outcome.total_cost_usd,
            now,
        ],
    )
    .map_err(|e| format!("Failed to record learning outcome: {}", e))?;

    info!(
        "Recorded learning outcome {} for task {} (status={})",
        id, outcome.task_run_id, status
    );

    Ok(id)
}

/// A pattern extracted from workflow execution analysis.
pub struct PatternInput {
    pub pattern_type: String,
    pub description: String,
    pub confidence: f32,
    pub context: Option<serde_json::Value>,
}

/// Record or update a learning pattern.
///
/// If a pattern with the same type and description already exists,
/// increments its occurrence count. Otherwise creates a new pattern.
pub fn record_learning_pattern(
    conn: &Connection,
    pattern: &PatternInput,
) -> Result<String, String> {
    let now = Utc::now().to_rfc3339();

    // Try to find an existing pattern with same type and description
    let existing_id: Option<String> = conn
        .query_row(
            "SELECT id FROM learning_patterns WHERE pattern_type = ?1 AND description = ?2",
            params![pattern.pattern_type, pattern.description],
            |row| row.get(0),
        )
        .ok();

    if let Some(id) = existing_id {
        // Update occurrence count and confidence
        conn.execute(
            r#"UPDATE learning_patterns
               SET occurrences = occurrences + 1,
                   confidence = MAX(confidence, ?1),
                   updated_at = ?2
               WHERE id = ?3"#,
            params![pattern.confidence, now, id],
        )
        .map_err(|e| format!("Failed to update learning pattern: {}", e))?;

        debug!("Updated learning pattern {} (incremented occurrences)", id);
        return Ok(id);
    }

    // Create new pattern
    let id = Uuid::new_v4().to_string();
    let context_json = pattern
        .context
        .as_ref()
        .map(|c| serde_json::to_string(c).unwrap_or_else(|_| "{}".into()));

    conn.execute(
        r#"INSERT INTO learning_patterns
            (id, pattern_type, description, confidence, occurrences, context, created_at, updated_at)
           VALUES (?1, ?2, ?3, ?4, 1, ?5, ?6, ?7)"#,
        params![
            id,
            pattern.pattern_type,
            pattern.description,
            pattern.confidence,
            context_json,
            now,
            now,
        ],
    )
    .map_err(|e| format!("Failed to record learning pattern: {}", e))?;

    info!(
        "Recorded new learning pattern {} (type={})",
        id, pattern.pattern_type
    );

    Ok(id)
}

/// Extract and record patterns from a workflow outcome.
///
/// Analyzes the outcome to identify common patterns:
/// - Success/failure by category
/// - Iteration count patterns
/// - Common error types
pub fn extract_and_record_patterns(
    conn: &Connection,
    outcome: &WorkflowOutcome,
) -> Result<Vec<String>, String> {
    let mut pattern_ids = Vec::new();

    // Pattern: category success/failure rate
    let category_pattern = PatternInput {
        pattern_type: if outcome.verification_passed {
            "success".to_string()
        } else {
            "failure".to_string()
        },
        description: format!(
            "Workflow category '{}' {}",
            outcome.category,
            if outcome.verification_passed {
                "succeeded"
            } else {
                "failed"
            }
        ),
        confidence: 0.5, // Low confidence for single observation
        context: Some(serde_json::json!({
            "category": outcome.category,
            "iterations": outcome.iterations,
            "duration_secs": outcome.duration_secs,
        })),
    };
    pattern_ids.push(record_learning_pattern(conn, &category_pattern)?);

    // Pattern: max iterations exhausted (indicates problem areas)
    if outcome.max_iterations_reached {
        let exhaustion_pattern = PatternInput {
            pattern_type: "iteration_exhaustion".to_string(),
            description: format!(
                "Category '{}' exhausted max iterations ({})",
                outcome.category, outcome.iterations,
            ),
            confidence: 0.7,
            context: Some(serde_json::json!({
                "category": outcome.category,
                "workflow_name": outcome.workflow_name,
            })),
        };
        pattern_ids.push(record_learning_pattern(conn, &exhaustion_pattern)?);
    }

    // Pattern: error type tracking
    if let Some(ref error_type) = outcome.error_type {
        let error_pattern = PatternInput {
            pattern_type: "error_type".to_string(),
            description: format!(
                "Error type '{}' in category '{}'",
                error_type, outcome.category
            ),
            confidence: 0.6,
            context: Some(serde_json::json!({
                "error_type": error_type,
                "error_message": outcome.error_message,
                "category": outcome.category,
            })),
        };
        pattern_ids.push(record_learning_pattern(conn, &error_pattern)?);
    }

    Ok(pattern_ids)
}

/// Record a complete learning observation from a workflow run.
///
/// This is the main entry point called from `loop_controller.rs` after
/// a workflow completes. It records both the outcome, extracted patterns,
/// and deterministic agentic metric scores.
pub fn record_workflow_learning(
    conn: &Connection,
    outcome: &WorkflowOutcome,
) -> Result<(), String> {
    let outcome_id = record_learning_outcome(conn, outcome)?;
    let pattern_ids = extract_and_record_patterns(conn, outcome)?;

    // Compute and persist deterministic agentic metrics (zero LLM cost)
    if let Err(e) = score_and_persist_agentic_metrics(conn, outcome) {
        // Non-fatal: metrics are supplementary to the core learning outcome
        tracing::warn!(
            "Failed to compute agentic metrics for task {}: {}",
            outcome.task_run_id,
            e
        );
    }

    debug!(
        "Recorded learning for task {}: outcome={}, patterns={:?}",
        outcome.task_run_id, outcome_id, pattern_ids
    );

    Ok(())
}

/// Compute deterministic agentic metrics and persist them to the database.
///
/// Runs synchronously after recording the learning outcome. Cost: zero LLM
/// calls, milliseconds of wall time.
fn score_and_persist_agentic_metrics(
    conn: &Connection,
    outcome: &WorkflowOutcome,
) -> Result<(), String> {
    use crate::meta_optimizer::agentic_metrics::{self, DeterministicInput};

    let input = DeterministicInput {
        task_run_id: outcome.task_run_id.clone(),
        status: outcome.status.clone(),
        verification_passed: outcome.verification_passed,
        was_stopped: outcome.was_stopped,
        iterations: outcome.iterations,
        step_count: outcome.step_count,
        verification_step_count: outcome.verification_step_count,
        agentic_step_count: outcome.agentic_step_count,
        max_iterations_reached: outcome.max_iterations_reached,
        duration_secs: outcome.duration_secs,
        error_type: outcome.error_type.clone(),
    };

    let scores = agentic_metrics::compute_deterministic(&input);
    let composite = agentic_metrics::composite_score(&scores);
    let now = chrono::Utc::now().to_rfc3339();

    // Insert each metric score
    for score in &scores {
        let id = format!("ams-{}", Uuid::new_v4());
        conn.execute(
            r#"INSERT OR REPLACE INTO agentic_metric_scores
                (id, task_run_id, metric_type, score, confidence,
                 rationale, is_llm_judged, model_used, created_at)
               VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)"#,
            params![
                id,
                outcome.task_run_id,
                score.metric.as_str(),
                score.score,
                score.confidence,
                score.rationale,
                score.is_llm_judged as i32,
                score.model_used,
                now,
            ],
        )
        .map_err(|e| format!("Failed to insert agentic metric score: {}", e))?;
    }

    // Update the cached composite score on learning_outcomes
    conn.execute(
        "UPDATE learning_outcomes SET composite_agentic_score = ?1 WHERE task_id = ?2",
        params![composite, outcome.task_run_id],
    )
    .map_err(|e| format!("Failed to update composite agentic score: {}", e))?;

    info!(
        "Scored agentic metrics for task {}: composite={:.3}, metrics={}",
        outcome.task_run_id,
        composite,
        scores
            .iter()
            .map(|s| format!("{}={:.2}", s.metric, s.score))
            .collect::<Vec<_>>()
            .join(", ")
    );

    Ok(())
}

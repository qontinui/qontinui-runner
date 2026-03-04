//! Per-agent scoring for workflow generation training data.
//!
//! Combines data from pipeline artifacts, reflection fixes, user feedback,
//! and supervisor eval benchmarks to compute a quality score per generation agent.
//! This enables prompt optimization by identifying which agents produce the
//! most issues and what patterns correlate with high/low quality.

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

// ============================================================================
// Types
// ============================================================================

/// Quality score for a single generation agent on a single pipeline run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentScore {
    pub agent: String,
    pub artifact_id: String,
    pub score: f64,
    pub factors: ScoreFactors,
}

/// Breakdown of factors that contribute to an agent's score.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoreFactors {
    // From pipeline artifacts
    pub fixer_iterations: u32,
    pub benchmark_score: Option<f64>,

    // From reflection
    pub attributed_fix_count: u32,
    pub effective_fix_count: u32,

    // From feedback
    pub user_edit_count: u32,
    pub user_deleted: bool,
    pub user_rating: Option<f64>,

    // From supervisor eval (if available)
    pub eval_overall_score: Option<f64>,
    pub eval_dimension_scores: Option<serde_json::Value>,
}

/// Training example: prompt + completion + score for one agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrainingExample {
    pub agent: String,
    pub prompt: String,
    pub completion: String,
    pub score: f64,
    pub artifact_id: String,
    pub workflow_id: Option<String>,
    pub created_at: String,
}

// ============================================================================
// Per-Agent Scoring Functions
// ============================================================================

/// Score the specification agent's output quality.
pub fn score_specification(factors: &ScoreFactors) -> f64 {
    let mut score = 1.0;
    // Penalize if reflection found spec-attributed issues
    score -= 0.15 * factors.attributed_fix_count as f64;
    // Reward if benchmark scored well (criteria led to good verification)
    if let Some(bench) = factors.benchmark_score {
        score *= 0.5 + 0.5 * bench;
    }
    // Penalize if user heavily edited verification steps
    if factors.user_edit_count > 3 {
        score -= 0.2;
    }
    // User deleted the whole workflow
    if factors.user_deleted {
        score -= 0.3;
    }
    score.clamp(0.0, 1.0)
}

/// Score the builder agent's output quality.
pub fn score_builder(factors: &ScoreFactors) -> f64 {
    let mut score = 1.0;
    // Penalize for fixer iterations (0 = builder got it right first time)
    score -= 0.1 * factors.fixer_iterations as f64;
    // Penalize for reflection-attributed issues
    score -= 0.15 * factors.attributed_fix_count as f64;
    // User feedback
    if let Some(rating) = factors.user_rating {
        score = score * 0.6 + (rating / 5.0) * 0.4;
    }
    if factors.user_deleted {
        score -= 0.4;
    }
    // Benchmark score (structural + content)
    if let Some(bench) = factors.benchmark_score {
        score = score * 0.7 + bench * 0.3;
    }
    score.clamp(0.0, 1.0)
}

/// Score the verification agent's output quality.
pub fn score_verification(factors: &ScoreFactors) -> f64 {
    let mut score = 1.0;
    // Penalize for reflection-attributed issues (verification missed real problems)
    score -= 0.2 * factors.attributed_fix_count as f64;
    // Reward effective fixes (verification found real issues)
    if factors.effective_fix_count > 0 {
        score += 0.05 * factors.effective_fix_count as f64;
    }
    // Eval dimensions if available
    if let Some(eval_score) = factors.eval_overall_score {
        score = score * 0.6 + eval_score * 0.4;
    }
    score.clamp(0.0, 1.0)
}

/// Score the hardener agent's output quality.
pub fn score_hardener(factors: &ScoreFactors) -> f64 {
    let mut score = 1.0;
    // Penalize for reflection-attributed issues (hardener broke something)
    score -= 0.2 * factors.attributed_fix_count as f64;
    // User edits after hardening suggest bad conversions
    if factors.user_edit_count > 2 {
        score -= 0.15;
    }
    if factors.user_deleted {
        score -= 0.3;
    }
    score.clamp(0.0, 1.0)
}

/// Score any agent by name.
pub fn score_agent(agent: &str, factors: &ScoreFactors) -> f64 {
    match agent {
        "specification" => score_specification(factors),
        "builder" => score_builder(factors),
        "verification" => score_verification(factors),
        "hardener" => score_hardener(factors),
        _ => {
            warn!("Unknown agent for scoring: {}", agent);
            0.5
        }
    }
}

// ============================================================================
// Data Collection
// ============================================================================

/// Collect score factors for a specific agent and artifact.
pub fn collect_score_factors(
    conn: &Connection,
    artifact_id: &str,
    workflow_id: Option<&str>,
    agent: &str,
) -> Result<ScoreFactors, String> {
    // Fixer iterations from pipeline artifact
    let fixer_iterations: u32 = conn
        .query_row(
            "SELECT fixer_snapshots FROM generation_pipeline_artifacts WHERE id = ?1",
            params![artifact_id],
            |row| {
                let json: Option<String> = row.get(0)?;
                Ok(json
                    .and_then(|j| serde_json::from_str::<Vec<serde_json::Value>>(&j).ok())
                    .map(|v| v.len() as u32)
                    .unwrap_or(0))
            },
        )
        .unwrap_or(0);

    // Reflection fixes attributed to this agent
    let (attributed_fix_count, effective_fix_count) = if let Some(wf_id) = workflow_id {
        count_attributed_fixes(conn, wf_id, agent)?
    } else {
        (0, 0)
    };

    // User feedback
    let (user_edit_count, user_deleted, user_rating) = if let Some(wf_id) = workflow_id {
        get_user_feedback(conn, wf_id)?
    } else {
        (0, false, None)
    };

    Ok(ScoreFactors {
        fixer_iterations,
        benchmark_score: None, // Populated separately if benchmark was run
        attributed_fix_count,
        effective_fix_count,
        user_edit_count,
        user_deleted,
        user_rating,
        eval_overall_score: None,
        eval_dimension_scores: None,
    })
}

/// Count reflection fixes attributed to a specific agent for a workflow.
fn count_attributed_fixes(
    conn: &Connection,
    workflow_id: &str,
    agent: &str,
) -> Result<(u32, u32), String> {
    let mut stmt = conn
        .prepare(
            r#"SELECT COUNT(*), SUM(CASE WHEN rf.effectiveness = 'effective' THEN 1 ELSE 0 END)
               FROM reflection_fixes rf
               INNER JOIN task_runs tr ON rf.source_task_run_id = tr.id
               INNER JOIN unified_workflows uw ON tr.workflow_name = uw.name
               WHERE uw.id = ?1 AND rf.source_agent = ?2"#,
        )
        .map_err(|e| format!("Failed to prepare attributed fixes query: {}", e))?;

    let result = stmt
        .query_row(params![workflow_id, agent], |row| {
            Ok((
                row.get::<_, i64>(0).unwrap_or(0) as u32,
                row.get::<_, i64>(1).unwrap_or(0) as u32,
            ))
        })
        .unwrap_or((0, 0));

    Ok(result)
}

/// Get user feedback for a workflow.
fn get_user_feedback(
    conn: &Connection,
    workflow_id: &str,
) -> Result<(u32, bool, Option<f64>), String> {
    let edit_count: u32 = conn
        .query_row(
            "SELECT COUNT(*) FROM workflow_generation_feedback WHERE workflow_id = ?1 AND feedback_type = 'edit'",
            params![workflow_id],
            |row| row.get::<_, i64>(0).map(|v| v as u32),
        )
        .unwrap_or(0);

    let deleted: bool = conn
        .query_row(
            "SELECT COUNT(*) FROM workflow_generation_feedback WHERE workflow_id = ?1 AND feedback_type = 'delete'",
            params![workflow_id],
            |row| row.get::<_, i64>(0).map(|v| v > 0),
        )
        .unwrap_or(false);

    let rating: Option<f64> = conn
        .query_row(
            "SELECT AVG(CAST(feedback_data AS REAL)) FROM workflow_generation_feedback WHERE workflow_id = ?1 AND feedback_type = 'rating'",
            params![workflow_id],
            |row| row.get(0),
        )
        .unwrap_or(None);

    Ok((edit_count, deleted, rating))
}

// ============================================================================
// Training Data Export
// ============================================================================

/// Export scored training examples for a specific agent.
pub fn export_training_data(
    conn: &Connection,
    agent: &str,
    min_score: f64,
    limit: u32,
) -> Result<Vec<TrainingExample>, String> {
    // Get artifacts that have the requested agent's prompt captured
    let prompt_column = match agent {
        "specification" => "specification_prompt",
        "builder" => "builder_prompt",
        "hardener" => "hardener_prompt",
        _ => {
            return Err(format!(
                "Agent '{}' does not have a dedicated prompt column",
                agent
            ))
        }
    };

    let sql = format!(
        r#"SELECT id, workflow_id, {prompt_col}, builder_raw_output, created_at
           FROM generation_pipeline_artifacts
           WHERE {prompt_col} IS NOT NULL
           ORDER BY created_at DESC
           LIMIT ?1"#,
        prompt_col = prompt_column
    );

    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| format!("Failed to prepare training data query: {}", e))?;

    let rows = stmt
        .query_map(params![limit], |row| {
            Ok((
                row.get::<_, String>(0)?,         // artifact_id
                row.get::<_, Option<String>>(1)?, // workflow_id
                row.get::<_, String>(2)?,         // prompt
                row.get::<_, Option<String>>(3)?, // completion (builder_raw_output)
                row.get::<_, String>(4)?,         // created_at
            ))
        })
        .map_err(|e| format!("Failed to query training data: {}", e))?;

    let mut examples = Vec::new();
    for row in rows {
        let (artifact_id, workflow_id, prompt, completion, created_at) =
            row.map_err(|e| format!("Row error: {}", e))?;

        let completion = completion.unwrap_or_default();
        if completion.is_empty() {
            continue;
        }

        let factors = collect_score_factors(conn, &artifact_id, workflow_id.as_deref(), agent)?;
        let score = score_agent(agent, &factors);

        if score >= min_score {
            examples.push(TrainingExample {
                agent: agent.to_string(),
                prompt,
                completion,
                score,
                artifact_id,
                workflow_id,
                created_at,
            });
        }
    }

    debug!(
        "Exported {} training examples for agent '{}' (min_score: {})",
        examples.len(),
        agent,
        min_score
    );

    Ok(examples)
}

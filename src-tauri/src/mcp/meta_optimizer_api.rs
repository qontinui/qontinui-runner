//! Meta-Optimizer HTTP endpoints.
//!
//! Read-only API for agent trace aggregates, prompt variants,
//! optimizer recommendations, and optimizer run history.
//! These endpoints are consumed by the meta-optimizer workflow
//! setup steps via curl.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::Json,
};
use serde::{Deserialize, Serialize};
use std::fmt::Write;
use std::sync::Arc;

use crate::database::pipeline_traces;
use crate::mcp::types::{api_error, ApiResponse, ApiState};
use crate::meta_optimizer::prompt_registry;
use crate::meta_optimizer::recommendations;
use crate::meta_optimizer::types::{
    ContextTier, GenerationFeedbackDetailL1, GenerationFeedbackSummaryL0, LearningOutcomeDetailL1,
    LearningOutcomeSummaryL0, MetaOptimizerRun, PromptVariant, Recommendation,
    ReflectionFixDetailL1, ReflectionFixSummaryL0,
};

// ---------------------------------------------------------------------------
// Query parameter structs
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct TraceAggregateQuery {
    pub limit: Option<u32>,
    pub tier: Option<ContextTier>,
    pub agent_type: Option<String>,
    pub trace_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct PromptVariantQuery {
    pub agent_type: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct RecommendationQuery {
    pub optimizer_type: Option<String>,
    pub status: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct OptimizerContextQuery {
    #[serde(rename = "type")]
    pub optimizer_type: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct LearningOutcomesQuery {
    pub limit: Option<u32>,
    pub status: Option<String>,
    pub workflow_architecture: Option<String>,
    pub tier: Option<ContextTier>,
}

#[derive(Debug, Deserialize)]
pub struct GenerationFeedbackQuery {
    pub limit: Option<u32>,
    pub feedback_type: Option<String>,
    pub tier: Option<ContextTier>,
}

#[derive(Debug, Deserialize)]
pub struct GenericLimitQuery {
    pub limit: Option<u32>,
}

#[derive(Debug, Serialize)]
pub struct OptimizerContext {
    pub context: String,
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// GET /meta-optimizer/agent-trace-aggregates?limit=N&tier=l0|l1|l2
///
/// Returns agent trace data at the requested tier:
/// - L0 (default): one-line summaries (agent_type + count + success_pct)
/// - L1: core fields per trace (id, agent_type, duration_ms, downstream_success, created_at)
/// - L2: full aggregated statistics (existing behavior)
pub async fn get_agent_trace_aggregates_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<TraceAggregateQuery>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let limit = query.limit.unwrap_or(50);
    let tier = query.tier.unwrap_or_default();

    let make_err = |e: String| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!(
                "Failed to get agent trace aggregates: {}",
                e
            ))),
        )
    };

    match tier {
        ContextTier::L0 => {
            let summaries = pipeline_traces::get_trace_summaries_l0(limit).map_err(make_err)?;
            Ok(Json(ApiResponse::success(
                serde_json::to_value(summaries).unwrap_or_default(),
            )))
        }
        ContextTier::L1 => {
            let details = pipeline_traces::get_trace_details_l1(query.agent_type.as_deref(), limit)
                .map_err(make_err)?;
            Ok(Json(ApiResponse::success(
                serde_json::to_value(details).unwrap_or_default(),
            )))
        }
        ContextTier::L2 => {
            // If trace_id is provided, return a single full trace record
            if let Some(ref trace_id) = query.trace_id {
                let trace = pipeline_traces::get_trace_full_l2(trace_id).map_err(make_err)?;
                Ok(Json(ApiResponse::success(
                    serde_json::to_value(trace).unwrap_or_default(),
                )))
            } else {
                let aggregates =
                    pipeline_traces::get_agent_trace_aggregates(limit).map_err(make_err)?;
                Ok(Json(ApiResponse::success(
                    serde_json::to_value(aggregates).unwrap_or_default(),
                )))
            }
        }
    }
}

/// GET /meta-optimizer/prompt-variants?agent_type=X
///
/// Lists prompt variants, optionally filtered by agent type.
pub async fn get_prompt_variants_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<PromptVariantQuery>,
) -> Result<Json<ApiResponse<Vec<PromptVariant>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let variants =
        prompt_registry::list_variants(&state.app_state.pg_db, query.agent_type.as_deref())
            .map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(api_error(format!("Failed to list prompt variants: {}", e))),
                )
            })?;

    Ok(Json(ApiResponse::success(variants)))
}

/// GET /meta-optimizer/recommendations?optimizer_type=X&status=Y
///
/// Lists optimizer recommendations with optional filters.
pub async fn get_recommendations_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<RecommendationQuery>,
) -> Result<Json<ApiResponse<Vec<Recommendation>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let recs = recommendations::list_recommendations(
        &state.app_state.pg_db,
        query.optimizer_type.as_deref(),
        query.status.as_deref(),
    )
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to list recommendations: {}", e))),
        )
    })?;

    Ok(Json(ApiResponse::success(recs)))
}

/// GET /meta-optimizer/runs
///
/// Lists recent meta-optimizer runs.
pub async fn get_optimizer_runs_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<Vec<MetaOptimizerRun>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let runs = state
        .app_state
        .pg_db
        .get_recent_optimizer_runs(50)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to list optimizer runs: {}", e))),
            )
        })?;

    Ok(Json(ApiResponse::success(runs)))
}

/// GET /meta-optimizer/optimizer-context?type=pipeline_prompt
///
/// Returns a pre-formatted markdown text summary of optimizer history and system
/// state, designed for AI consumption as rich context.
pub async fn get_optimizer_context_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<OptimizerContextQuery>,
) -> Result<Json<ApiResponse<OptimizerContext>>, (StatusCode, Json<ApiResponse<()>>)> {
    let optimizer_type = query.optimizer_type.clone().unwrap_or_default();
    let is_pipeline = optimizer_type == "pipeline_prompt";

    let context = state
        .app_state
        .pg_db
        .get_optimizer_context(is_pipeline)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!(
                    "Failed to assemble optimizer context: {}",
                    e
                ))),
            )
        })?;

    Ok(Json(ApiResponse::success(OptimizerContext { context })))
}

// NOTE: The original ~450-line SQLite optimizer context builder was removed.
// PgDb::get_optimizer_context provides equivalent functionality.

/* BEGIN_REMOVED -- original SQLite optimizer context (~430 lines) removed during PG migration */
/*
            let since = (chrono::Utc::now() - chrono::Duration::days(30)).to_rfc3339();
            let mut out = String::new();

            // ── 1. Performance Summary ──────────────────────────────────
            {
                let result = conn.query_row(
                    r#"SELECT
                        COUNT(*) as total_runs,
                        SUM(CASE WHEN status='success' THEN 1 ELSE 0 END) as successful,
                        SUM(CASE WHEN status='partial' THEN 1 ELSE 0 END) as partial,
                        SUM(CASE WHEN status='failure' THEN 1 ELSE 0 END) as failed,
                        ROUND(AVG(duration_secs), 1) as avg_duration,
                        ROUND(AVG(iterations), 1) as avg_iterations
                    FROM learning_outcomes
                    WHERE created_at > ?1
                      AND (iterations IS NULL OR iterations > 0)"#,
                    params![since],
                    |row| {
                        Ok((
                            row.get::<_, i64>(0)?,
                            row.get::<_, i64>(1)?,
                            row.get::<_, i64>(2)?,
                            row.get::<_, i64>(3)?,
                            row.get::<_, f64>(4).unwrap_or(0.0),
                            row.get::<_, f64>(5).unwrap_or(0.0),
                        ))
                    },
                );

                if let Ok((total, success, partial, failed, avg_dur, avg_iter)) = result {
                    if total > 0 {
                        let success_pct = 100.0 * success as f64 / total as f64;
                        let partial_pct = 100.0 * partial as f64 / total as f64;
                        let failed_pct = 100.0 * failed as f64 / total as f64;

                        let _ = writeln!(out, "## Performance Summary (last 30 days)");
                        let _ = writeln!(
                            out,
                            "- {} total runs: {} successful ({:.1}%), {} partial ({:.1}%), {} failed ({:.1}%)",
                            total, success, success_pct, partial, partial_pct, failed, failed_pct
                        );
                        let _ = writeln!(
                            out,
                            "- Avg duration: {:.1}s | Avg iterations: {:.1}",
                            avg_dur, avg_iter
                        );
                        let _ = writeln!(out);
                    }
                }
            }

            // ── 2. Architecture Breakdown ───────────────────────────────
            {
                let mut stmt = conn
                    .prepare(
                        r#"SELECT
                            workflow_architecture,
                            COUNT(*) as runs,
                            ROUND(100.0 * SUM(CASE WHEN status='success' THEN 1 ELSE 0 END) / COUNT(*), 1) as success_rate,
                            ROUND(AVG(duration_secs), 1) as avg_duration
                        FROM learning_outcomes
                        WHERE created_at > ?1 AND workflow_architecture IS NOT NULL
                          AND (iterations IS NULL OR iterations > 0)
                        GROUP BY workflow_architecture"#,
                    )
                    .map_err(|e| format!("Failed to prepare arch breakdown: {}", e))?;

                let rows: Vec<(String, i64, f64, f64)> = stmt
                    .query_map(params![since], |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, i64>(1)?,
                            row.get::<_, f64>(2).unwrap_or(0.0),
                            row.get::<_, f64>(3).unwrap_or(0.0),
                        ))
                    })
                    .map_err(|e| format!("Failed to query arch breakdown: {}", e))?
                    .filter_map(|r| r.ok())
                    .collect();

                if !rows.is_empty() {
                    let _ = writeln!(out, "## Architecture Breakdown");
                    let _ = writeln!(
                        out,
                        "| Architecture | Runs | Success Rate | Avg Duration |"
                    );
                    let _ = writeln!(out, "|---|---|---|---|");
                    for (arch, runs, sr, dur) in &rows {
                        let _ = writeln!(out, "| {} | {} | {:.1}% | {:.1}s |", arch, runs, sr, dur);
                    }
                    let _ = writeln!(out);
                }
            }

            // ── 3. Previous Recommendations ─────────────────────────────
            {
                let mut stmt = conn
                    .prepare(
                        r#"SELECT
                            r.id, r.title, r.recommendation_type, r.target_agent,
                            r.confidence, r.status, r.applied_at, r.outcome_after_apply,
                            r.created_at
                        FROM meta_optimizer_recommendations r
                        WHERE r.optimizer_type = ?1
                        ORDER BY r.created_at DESC
                        LIMIT 20"#,
                    )
                    .map_err(|e| format!("Failed to prepare recommendations query: {}", e))?;

                struct RecRow {
                    _id: String,
                    title: String,
                    rec_type: String,
                    target_agent: Option<String>,
                    confidence: f64,
                    status: String,
                    applied_at: Option<String>,
                    outcome_json: Option<String>,
                }

                let rows: Vec<RecRow> = stmt
                    .query_map(params![optimizer_type], |row| {
                        Ok(RecRow {
                            _id: row.get(0)?,
                            title: row.get(1)?,
                            rec_type: row.get(2)?,
                            target_agent: row.get(3)?,
                            confidence: row.get::<_, f64>(4).unwrap_or(0.0),
                            status: row.get(5)?,
                            applied_at: row.get(6)?,
                            outcome_json: row.get(7)?,
                        })
                    })
                    .map_err(|e| format!("Failed to query recommendations: {}", e))?
                    .filter_map(|r| r.ok())
                    .collect();

                if !rows.is_empty() {
                    let _ = writeln!(out, "## Your Previous Recommendations");
                    for (i, rec) in rows.iter().enumerate() {
                        let status_label = rec.status.to_uppercase();

                        // Parse outcome verdict if available
                        let verdict_str = rec
                            .outcome_json
                            .as_ref()
                            .and_then(|json_str| serde_json::from_str::<serde_json::Value>(json_str).ok())
                            .and_then(|v| {
                                let verdict = v.get("verdict")?.as_str()?;
                                let delta = v
                                    .get("success_rate_delta")
                                    .and_then(|d| d.as_f64());
                                let arrow = match verdict {
                                    "improved" => "Improved",
                                    "regressed" => "Regressed",
                                    "neutral" => "Neutral",
                                    "insufficient_data" => "Insufficient data",
                                    _ => verdict,
                                };
                                if let Some(d) = delta {
                                    Some(format!("{} ({:+.1}pp)", arrow, d))
                                } else {
                                    Some(arrow.to_string())
                                }
                            });

                        let target_str = rec
                            .target_agent
                            .as_deref()
                            .map(|a| format!(" for {}", a))
                            .unwrap_or_default();

                        match (status_label.as_str(), &verdict_str) {
                            ("APPLIED", Some(v)) => {
                                let symbol = if v.starts_with("Improved") || v.starts_with("Neutral") {
                                    "\\u2713"
                                } else {
                                    "\\u2717"
                                };
                                let _ = write!(
                                    out,
                                    "{}. [APPLIED {} -> {}] ",
                                    i + 1,
                                    symbol,
                                    v
                                );
                            }
                            _ => {
                                let _ = write!(out, "{}. [{}] ", i + 1, status_label);
                            }
                        }

                        let _ = write!(
                            out,
                            "\"{}\" ({}{}, confidence: {:.2})",
                            rec.title, rec.rec_type, target_str, rec.confidence
                        );

                        if let Some(applied) = &rec.applied_at {
                            // Show date only (trim to first 10 chars)
                            let date = if applied.len() >= 10 {
                                &applied[..10]
                            } else {
                                applied
                            };
                            let _ = write!(out, "\n   Applied: {}", date);
                        }
                        let _ = writeln!(out);
                    }
                    let _ = writeln!(out);
                }
            }

            // ── 4. Progress vs Baseline ─────────────────────────────────
            {
                let baseline: Option<(f64, f64, String)> = conn
                    .query_row(
                        r#"SELECT metrics_json, created_at
                        FROM meta_optimizer_snapshots
                        WHERE snapshot_type = 'baseline'
                        ORDER BY created_at DESC LIMIT 1"#,
                        [],
                        |row| {
                            let json_str: String = row.get(0)?;
                            let created: String = row.get(1)?;
                            Ok((json_str, created))
                        },
                    )
                    .ok()
                    .and_then(|(json_str, created)| {
                        let v: serde_json::Value = serde_json::from_str(&json_str).ok()?;
                        let sr = v.get("success_rate")?.as_f64()?;
                        let dur = v.get("avg_duration_secs")?.as_f64()?;
                        Some((sr, dur, created))
                    });

                let current: Option<(f64, f64)> = conn
                    .query_row(
                        r#"SELECT metrics_json
                        FROM meta_optimizer_snapshots
                        WHERE snapshot_type = 'periodic'
                        ORDER BY created_at DESC LIMIT 1"#,
                        [],
                        |row| row.get::<_, String>(0),
                    )
                    .ok()
                    .and_then(|json_str| {
                        let v: serde_json::Value = serde_json::from_str(&json_str).ok()?;
                        let sr = v.get("success_rate")?.as_f64()?;
                        let dur = v.get("avg_duration_secs")?.as_f64()?;
                        Some((sr, dur))
                    });

                if let Some((b_sr, b_dur, b_date)) = &baseline {
                    let _ = writeln!(out, "## Progress vs Baseline");
                    let date = if b_date.len() >= 10 {
                        &b_date[..10]
                    } else {
                        b_date
                    };
                    let _ = writeln!(
                        out,
                        "- Baseline (captured {}): {:.1}% success, {:.1}s avg duration",
                        date,
                        b_sr * 100.0,
                        b_dur
                    );

                    if let Some((c_sr, c_dur)) = current {
                        let sr_delta = (c_sr - b_sr) * 100.0;
                        let dur_delta = c_dur - b_dur;
                        let _ = writeln!(
                            out,
                            "- Current: {:.1}% success ({:+.1}pp), {:.1}s avg duration ({:+.1}s)",
                            c_sr * 100.0,
                            sr_delta,
                            c_dur,
                            dur_delta
                        );
                    }
                    let _ = writeln!(out);
                }
            }

            // ── 5. Top Failure Patterns ─────────────────────────────────
            {
                let mut stmt = conn
                    .prepare(
                        r#"SELECT
                            CASE
                                WHEN error_message LIKE '%max iterations%' OR error_message LIKE '%max_iterations%' OR error_message LIKE '%iteration budget%' OR error_message LIKE '%iterations exhausted%' THEN 'max_iterations_reached'
                                WHEN error_message LIKE '%Max sessions%' OR error_message LIKE '%sessions exhausted%' THEN 'max_sessions_reached'
                                WHEN error_message LIKE '%setup failed%' THEN 'setup_failure'
                                WHEN error_message LIKE '%Unfixable errors%' THEN 'unfixable_errors'
                                WHEN error_message LIKE '%stopped%' OR status = 'stopped' THEN 'stopped_by_user'
                                WHEN error_message LIKE '%Critical failure%' OR error_message LIKE '%critical%' THEN 'critical_failure'
                                WHEN error_message LIKE '%no iterations ran%' THEN 'zero_iterations'
                                WHEN error_message LIKE '%interrupted%' OR error_message LIKE '%restart%' THEN 'interrupted'
                                WHEN error_message IS NOT NULL THEN 'runtime_error'
                                WHEN goal_achieved = 0 THEN 'goal_not_achieved'
                                ELSE 'unknown'
                            END as reason,
                            COUNT(*) as count
                        FROM task_runs
                        WHERE status IN ('failed', 'stopped')
                            AND created_at > ?1
                            AND COALESCE(is_fixer, 0) = 0 AND COALESCE(is_reflection, 0) = 0
                            AND COALESCE(is_follow_up, 0) = 0 AND COALESCE(is_meta_optimizer, 0) = 0
                        GROUP BY reason ORDER BY count DESC LIMIT 5"#,
                    )
                    .map_err(|e| format!("Failed to prepare failure patterns query: {}", e))?;

                let rows: Vec<(String, i64)> = stmt
                    .query_map(params![since], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
                    })
                    .map_err(|e| format!("Failed to query failure patterns: {}", e))?
                    .filter_map(|r| r.ok())
                    .collect();

                if !rows.is_empty() {
                    let _ = writeln!(out, "## Top Failure Patterns");
                    for (i, (reason, count)) in rows.iter().enumerate() {
                        let label = if *count == 1 { "run" } else { "runs" };
                        let _ = writeln!(out, "{}. {}: {} {}", i + 1, reason, count, label);
                    }
                    let _ = writeln!(out);
                }
            }

            // ── 6. Pipeline Agent Failure Rates (pipeline_prompt only) ──
            if is_pipeline {
                let mut stmt = conn
                    .prepare(
                        r#"SELECT agent_type,
                            COUNT(*) as total,
                            SUM(CASE WHEN downstream_success = 0 THEN 1 ELSE 0 END) as failures,
                            ROUND(100.0 * SUM(CASE WHEN downstream_success = 0 THEN 1 ELSE 0 END) / NULLIF(COUNT(*), 0), 1) as failure_rate
                        FROM pipeline_agent_traces
                        WHERE created_at > ?1 AND downstream_success IS NOT NULL
                        GROUP BY agent_type"#,
                    )
                    .map_err(|e| format!("Failed to prepare agent failure query: {}", e))?;

                let rows: Vec<(String, i64, i64, f64)> = stmt
                    .query_map(params![since], |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, i64>(1)?,
                            row.get::<_, i64>(2).unwrap_or(0),
                            row.get::<_, f64>(3).unwrap_or(0.0),
                        ))
                    })
                    .map_err(|e| format!("Failed to query agent failures: {}", e))?
                    .filter_map(|r| r.ok())
                    .collect();

                if !rows.is_empty() {
                    let _ = writeln!(out, "## Pipeline Agent Failure Rates");
                    let _ = writeln!(out, "| Agent | Total | Failures | Rate |");
                    let _ = writeln!(out, "|---|---|---|---|");
                    for (agent, total, failures, rate) in &rows {
                        let _ = writeln!(
                            out,
                            "| {} | {} | {} | {:.1}% |",
                            agent, total, failures, rate
                        );
                    }
                    let _ = writeln!(out);
                }

                // ── 7. Token Efficiency per Agent ──────────────────────────
                let mut token_stmt = conn
                    .prepare(
                        r#"SELECT agent_type,
                            COUNT(*) as run_count,
                            ROUND(AVG(tokens_in), 0) as avg_tokens_in,
                            ROUND(AVG(tokens_out), 0) as avg_tokens_out,
                            ROUND(AVG(cost_usd), 4) as avg_cost_usd,
                            SUM(tokens_in) as total_tokens_in,
                            SUM(tokens_out) as total_tokens_out,
                            ROUND(SUM(cost_usd), 2) as total_cost_usd
                        FROM pipeline_agent_traces
                        WHERE created_at > ?1
                            AND (tokens_in > 0 OR tokens_out > 0)
                        GROUP BY agent_type
                        ORDER BY total_cost_usd DESC"#,
                    )
                    .map_err(|e| format!("Failed to prepare token efficiency query: {}", e))?;

                let token_rows: Vec<(String, i64, f64, f64, f64, i64, i64, f64)> = token_stmt
                    .query_map(params![since], |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, i64>(1)?,
                            row.get::<_, f64>(2).unwrap_or(0.0),
                            row.get::<_, f64>(3).unwrap_or(0.0),
                            row.get::<_, f64>(4).unwrap_or(0.0),
                            row.get::<_, i64>(5).unwrap_or(0),
                            row.get::<_, i64>(6).unwrap_or(0),
                            row.get::<_, f64>(7).unwrap_or(0.0),
                        ))
                    })
                    .map_err(|e| format!("Failed to query token efficiency: {}", e))?
                    .filter_map(|r| r.ok())
                    .collect();

                if !token_rows.is_empty() {
                    let _ = writeln!(out, "## Token Efficiency");
                    let _ = writeln!(
                        out,
                        "| Agent | Runs | Avg In | Avg Out | Avg Cost | Total Cost |"
                    );
                    let _ = writeln!(out, "|---|---|---|---|---|---|");
                    for (agent, runs, avg_in, avg_out, avg_cost, _total_in, _total_out, total_cost) in &token_rows {
                        let _ = writeln!(
                            out,
                            "| {} | {} | {:.0} | {:.0} | ${:.4} | ${:.2} |",
                            agent, runs, avg_in, avg_out, avg_cost, total_cost
                        );
                    }
                    let _ = writeln!(out);
                }
            }

            if out.is_empty() {
                let _ = writeln!(out, "No optimizer history data available yet. This is likely the first run.");
            }

*/
/* END_REMOVED */

/// GET /meta-optimizer/cost-analysis
///
/// Returns a cost-efficiency summary: per-agent cost breakdown, total pipeline
/// cost, cost trend (increasing/decreasing/stable), and active cost recommendations.
pub async fn get_cost_analysis_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<
    Json<ApiResponse<crate::meta_optimizer::cost_optimizer::CostAnalysisSummary>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    let summary =
        crate::meta_optimizer::cost_optimizer::build_cost_analysis(&state.app_state.pg_db)
            .map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(api_error(format!("Failed to build cost analysis: {}", e))),
                )
            })?;

    Ok(Json(ApiResponse::success(summary)))
}

/// GET /learning/outcomes?limit=N&status=X&workflow_architecture=Y&tier=l0|l1|l2
///
/// Returns learning outcomes at the requested tier:
/// - L0 (default): counts and averages grouped by status
/// - L1: core fields (id, task_id, status, duration, iterations, architecture, error_type, created_at)
/// - L2: full records with all fields (existing behavior)
pub async fn get_learning_outcomes_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<LearningOutcomesQuery>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let limit = query.limit.unwrap_or(50);
    let status = query.status.clone();
    let workflow_architecture = query.workflow_architecture.clone();
    let tier = query.tier.unwrap_or_default();

    let make_err = |e: String| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to get learning outcomes: {}", e))),
        )
    };

    match tier {
        ContextTier::L0 => {
            let summaries = state
                .app_state
                .pg_db
                .get_learning_outcomes_l0(status.as_deref(), workflow_architecture.as_deref())
                .await
                .map_err(make_err)?;
            Ok(Json(ApiResponse::success(
                serde_json::to_value(summaries).unwrap_or_default(),
            )))
        }
        ContextTier::L1 => {
            let details = state
                .app_state
                .pg_db
                .get_learning_outcomes_l1(
                    status.as_deref(),
                    workflow_architecture.as_deref(),
                    limit,
                )
                .await
                .map_err(make_err)?;
            Ok(Json(ApiResponse::success(
                serde_json::to_value(details).unwrap_or_default(),
            )))
        }
        ContextTier::L2 => {
            let outcomes = state
                .app_state
                .pg_db
                .get_learning_outcomes_l2(
                    status.as_deref(),
                    workflow_architecture.as_deref(),
                    limit,
                )
                .await
                .unwrap_or_default();
            Ok(Json(ApiResponse::success(
                serde_json::to_value(outcomes).unwrap_or_default(),
            )))
        }
    }
}

/// GET /workflow-generation/feedback?limit=N&feedback_type=X&tier=l0|l1|l2
///
/// Returns workflow generation feedback at the requested tier:
/// - L0 (default): counts grouped by feedback_type with avg rating
/// - L1: core fields (id, feedback_type, edited_field, rating, workflow_category, created_at)
/// - L2: full records with all fields (existing behavior)
pub async fn get_generation_feedback_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<GenerationFeedbackQuery>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let limit = query.limit.unwrap_or(50);
    let feedback_type = query.feedback_type.clone();
    let tier = query.tier.unwrap_or_default();

    let make_err = |e: String| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!(
                "Failed to get generation feedback: {}",
                e
            ))),
        )
    };

    match tier {
        ContextTier::L0 => {
            let summaries = state
                .app_state
                .pg_db
                .get_generation_feedback_l0(feedback_type.as_deref())
                .await
                .map_err(make_err)?;
            Ok(Json(ApiResponse::success(
                serde_json::to_value(summaries).unwrap_or_default(),
            )))
        }
        ContextTier::L1 => {
            let details = state
                .app_state
                .pg_db
                .get_generation_feedback_l1(feedback_type.as_deref(), limit)
                .await
                .map_err(make_err)?;
            Ok(Json(ApiResponse::success(
                serde_json::to_value(details).unwrap_or_default(),
            )))
        }
        ContextTier::L2 => {
            let feedback = state
                .app_state
                .pg_db
                .get_generation_feedback_l2(feedback_type.as_deref(), limit)
                .await
                .unwrap_or_default();
            Ok(Json(ApiResponse::success(
                serde_json::to_value(feedback).unwrap_or_default(),
            )))
        }
    }
}

/// GET /prompt-analysis?limit=N
///
/// Stub endpoint — the prompt_analysis table does not exist yet.
/// Returns an empty array so the setup step does not fail.
pub async fn get_prompt_analysis_handler(
    Query(_query): Query<GenericLimitQuery>,
) -> Result<Json<ApiResponse<Vec<serde_json::Value>>>, (StatusCode, Json<ApiResponse<()>>)> {
    Ok(Json(ApiResponse::success(vec![])))
}

// ---------------------------------------------------------------------------
// Reflection fixes (cross-workflow, no workflow_name filter required)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct ReflectionFixesQuery {
    limit: Option<u32>,
    source_agent: Option<String>,
    tier: Option<ContextTier>,
}

/// GET /meta-optimizer/reflection-fixes?tier=l0|l1|l2 — returns reflection fixes at requested tier.
///
/// - L0 (default): counts grouped by fix_type and effectiveness
/// - L1: core fields (fix_type, fix_description, confidence, effectiveness, source_agent, created_at)
/// - L2: full records including old_value, new_value, reasoning, etc.
pub async fn get_reflection_fixes_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<ReflectionFixesQuery>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let limit = query.limit.unwrap_or(50) as i64;
    let source_agent = query.source_agent.clone();
    let tier = query.tier.unwrap_or_default();

    let make_err = |e: String| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to get reflection fixes: {}", e))),
        )
    };

    match tier {
        ContextTier::L0 => {
            let summaries = state
                .app_state
                .pg_db
                .get_reflection_fixes_l0(source_agent.as_deref())
                .await
                .map_err(make_err)?;
            Ok(Json(ApiResponse::success(
                serde_json::to_value(summaries).unwrap_or_default(),
            )))
        }
        ContextTier::L1 => {
            let details = state
                .app_state
                .pg_db
                .get_reflection_fixes_l1(source_agent.as_deref(), limit)
                .await
                .map_err(make_err)?;
            Ok(Json(ApiResponse::success(
                serde_json::to_value(details).unwrap_or_default(),
            )))
        }
        ContextTier::L2 => {
            let fixes = state
                .app_state
                .pg_db
                .get_reflection_fixes_l2(source_agent.as_deref(), limit)
                .await
                .map_err(make_err)?;
            Ok(Json(ApiResponse::success(
                serde_json::to_value(fixes).unwrap_or_default(),
            )))
        }
    }
}

// ---------------------------------------------------------------------------
// Apply / Reject handlers
// ---------------------------------------------------------------------------

/// POST /meta-optimizer/recommendations/:id/apply
///
/// Applies a pending recommendation, executing side-effects (rule creation, config change, etc.).
pub async fn apply_recommendation_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<()>>, (StatusCode, Json<ApiResponse<()>>)> {
    recommendations::apply_recommendation_with_side_effects(&state.app_state.pg_db, &id).map_err(
        |e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to apply recommendation: {}", e))),
            )
        },
    )?;
    Ok(Json(ApiResponse::success(())))
}

/// POST /meta-optimizer/recommendations/:id/reject
///
/// Rejects a pending recommendation.
pub async fn reject_recommendation_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<()>>, (StatusCode, Json<ApiResponse<()>>)> {
    recommendations::reject_recommendation(&state.app_state.pg_db, &id).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to reject recommendation: {}", e))),
        )
    })?;
    Ok(Json(ApiResponse::success(())))
}

// ---------------------------------------------------------------------------
// Iteration history (approach pattern analysis for agentic verification loops)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct IterationHistoryQuery {
    limit: Option<u32>,
    status: Option<String>,
    tier: Option<ContextTier>,
}

/// GET /meta-optimizer/iteration-history?tier=l0|l1|l2&status=X&limit=N
///
/// Exposes compressed iteration history from agentic verification loops
/// for meta-optimizer analysis of approach patterns.
///
/// - L0 (default): aggregate stats grouped by run status
/// - L1: per-run iteration sequences with approaches and confidence trajectory
/// - L2: full raw iteration_history JSON from each run
pub async fn get_iteration_history_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<IterationHistoryQuery>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let limit = query.limit.unwrap_or(50) as i64;
    let status_filter = query.status.clone();
    let tier = query.tier.unwrap_or_default();

    let make_err = |e: String| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to get iteration history: {}", e))),
        )
    };

    let pg = &state.app_state.pg_db;
    match tier {
        ContextTier::L0 => {
            let summaries = pg
                .get_iteration_history_l0(limit, status_filter.as_deref())
                .await
                .map_err(make_err)?;
            Ok(Json(ApiResponse::success(
                serde_json::to_value(summaries).unwrap_or_default(),
            )))
        }
        ContextTier::L1 => {
            let details = pg
                .get_iteration_history_l1(limit, status_filter.as_deref())
                .await
                .map_err(make_err)?;
            Ok(Json(ApiResponse::success(
                serde_json::to_value(details).unwrap_or_default(),
            )))
        }
        ContextTier::L2 => {
            let full = pg
                .get_iteration_history_l2(limit, status_filter.as_deref())
                .await
                .map_err(make_err)?;
            Ok(Json(ApiResponse::success(
                serde_json::to_value(full).unwrap_or_default(),
            )))
        }
    }
}

// NOTE: ~200 lines of SQLite iteration history code removed during PG migration.
// PgDb::get_iteration_history_l0/l1/l2 provides equivalent functionality.

// Removed SQLite iteration history code — now served from PG.
// The original dead code block has been deleted.
// (Removed dead SQLite iteration_history code block)

// ---------------------------------------------------------------------------
// Eval spec handlers
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct EvalSpecQuery {
    pub target_agent: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct EvalResultQuery {
    pub spec_id: Option<String>,
    pub recommendation_id: Option<String>,
}

async fn get_eval_specs_handler(
    State(state): State<Arc<ApiState>>,
    Query(q): Query<EvalSpecQuery>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let make_err = |e: String| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to get eval specs: {}", e))),
        )
    };

    let specs = crate::meta_optimizer::eval_spec::list_eval_specs(
        &state.app_state.pg_db,
        q.target_agent.as_deref(),
    )
    .map_err(make_err)?;

    Ok(Json(ApiResponse::success(
        serde_json::to_value(specs).unwrap_or_default(),
    )))
}

async fn create_eval_spec_handler(
    State(state): State<Arc<ApiState>>,
    Json(spec): Json<crate::meta_optimizer::eval_spec::EvalSpec>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let make_err = |e: String| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to create eval spec: {}", e))),
        )
    };

    crate::meta_optimizer::eval_spec::save_eval_spec(&state.app_state.pg_db, &spec)
        .map_err(make_err)?;

    Ok(Json(ApiResponse::success(serde_json::json!({
        "id": spec.id,
        "status": "saved"
    }))))
}

async fn get_eval_results_handler(
    State(state): State<Arc<ApiState>>,
    Query(q): Query<EvalResultQuery>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let make_err = |e: String| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to get eval results: {}", e))),
        )
    };

    let results = crate::meta_optimizer::eval_spec::list_eval_results(
        &state.app_state.pg_db,
        q.spec_id.as_deref(),
        q.recommendation_id.as_deref(),
    )
    .map_err(make_err)?;

    Ok(Json(ApiResponse::success(
        serde_json::to_value(results).unwrap_or_default(),
    )))
}

async fn evaluate_recommendation_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let make_err = |e: String| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!(
                "Failed to evaluate recommendation: {}",
                e
            ))),
        )
    };

    let result =
        crate::meta_optimizer::eval_runner::validate_recommendation(&state.app_state.pg_db, &id)
            .map_err(make_err)?;

    Ok(Json(ApiResponse::success(
        serde_json::to_value(result).unwrap_or_default(),
    )))
}

// ---------------------------------------------------------------------------
// Live evaluation with I/O (LLM-as-judge)
// ---------------------------------------------------------------------------

/// Request body for the evaluate-with-io endpoint.
#[derive(Debug, Deserialize)]
struct EvaluateWithIoRequest {
    /// The eval spec (inline JSON or a spec_id to look up).
    eval_spec: serde_json::Value,
    /// The input that was provided to the agent/workflow.
    input: String,
    /// The output the agent/workflow produced.
    output: String,
    /// Optional context (e.g., reference documents, ground truth).
    context: Option<String>,
}

/// POST /meta-optimizer/evaluate-with-io
///
/// Evaluate an eval spec against live input/output data using LLM-as-judge assertions.
/// Accepts either an inline EvalSpec object or a `{ "spec_id": "..." }` reference.
async fn evaluate_with_io_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<EvaluateWithIoRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let make_err = |e: String| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to evaluate with I/O: {}", e))),
        )
    };

    // Resolve the eval spec: either inline or by spec_id lookup.
    let spec: crate::meta_optimizer::eval_spec::EvalSpec = if let Some(spec_id) =
        body.eval_spec.get("spec_id").and_then(|v| v.as_str())
    {
        // Look up by ID
        let specs = crate::meta_optimizer::eval_spec::list_eval_specs(&state.app_state.pg_db, None)
            .map_err(make_err)?;
        specs
            .into_iter()
            .find(|s| s.id == spec_id)
            .ok_or_else(|| make_err(format!("Eval spec not found: {}", spec_id)))?
    } else {
        serde_json::from_value(body.eval_spec)
            .map_err(|e| make_err(format!("Invalid eval spec: {}", e)))?
    };

    // Build aggregate metrics for non-judge assertions
    let metrics = if let Some(ref agent) = spec.target_agent {
        let period_start = (chrono::Utc::now() - chrono::Duration::days(30)).to_rfc3339();
        let period_end = chrono::Utc::now().to_rfc3339();

        crate::database::pipeline_traces::get_agent_aggregates_for_period(
            agent,
            &period_start,
            &period_end,
        )
        .ok()
        .flatten()
        .map(
            |agg| crate::meta_optimizer::eval_spec::EvalAggregateMetrics {
                success_rate: if agg.run_count > 0 {
                    agg.success_count as f64 / agg.run_count as f64
                } else {
                    0.0
                },
                mean_duration_ms: agg.avg_duration_ms,
                mean_iterations: 0.0,
                mean_cost_cents: agg.avg_cost_usd * 100.0,
                trial_count: agg.run_count as u32,
            },
        )
        .unwrap_or_else(|| crate::meta_optimizer::eval_spec::EvalAggregateMetrics {
            success_rate: 0.0,
            mean_duration_ms: 0.0,
            mean_iterations: 0.0,
            mean_cost_cents: 0.0,
            trial_count: 0,
        })
    } else {
        crate::meta_optimizer::eval_spec::EvalAggregateMetrics {
            success_rate: 0.0,
            mean_duration_ms: 0.0,
            mean_iterations: 0.0,
            mean_cost_cents: 0.0,
            trial_count: 0,
        }
    };

    // Evaluate all test cases
    let mut all_results = Vec::new();
    let mut test_case_results = Vec::new();
    for tc in &spec.test_cases {
        let results = crate::meta_optimizer::eval_runner::evaluate_test_case_with_io(
            tc,
            &body.input,
            &body.output,
            body.context.as_deref(),
            &metrics,
        );
        let passed = results.iter().all(|r| r.passed);
        test_case_results.push(serde_json::json!({
            "test_case_id": tc.id,
            "passed": passed,
            "assertion_results": results,
        }));
        all_results.extend(results);
    }

    let all_passed = all_results.iter().all(|r| r.passed);

    Ok(Json(ApiResponse::success(serde_json::json!({
        "status": if all_passed { "passed" } else { "failed" },
        "test_case_results": test_case_results,
        "assertion_results": all_results,
        "total_assertions": all_results.len(),
        "passed_assertions": all_results.iter().filter(|r| r.passed).count(),
    }))))
}

// ---------------------------------------------------------------------------
// Canary rollout endpoints
// ---------------------------------------------------------------------------

/// GET /meta-optimizer/canaries
///
/// Returns all active canary rollouts with recommendation metadata.
pub async fn get_canaries_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let canaries = crate::meta_optimizer::canary::get_active_canaries(&state.app_state.pg_db)
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to get canaries: {}", e))),
            )
        })?;

    // Enrich with recommendation metadata
    let mut enriched: Vec<serde_json::Value> = Vec::new();
    for c in &canaries {
        let rec_info = state
            .app_state
            .pg_db
            .get_recommendation_metadata(&c.recommendation_id)
            .await
            .ok()
            .flatten();

        let (title, agent, rec_type) = rec_info.unwrap_or((None, None, None));

        enriched.push(serde_json::json!({
            "id": c.id,
            "recommendation_id": c.recommendation_id,
            "percentage": c.percentage,
            "status": c.status,
            "start_date": c.start_date,
            "end_date": c.end_date,
            "baseline_run_count": c.baseline_run_count,
            "canary_run_count": c.canary_run_count,
            "baseline_metrics_json": c.baseline_metrics_json,
            "canary_metrics_json": c.canary_metrics_json,
            "created_at": c.created_at,
            "recommendation_title": title,
            "target_agent": agent,
            "recommendation_type": rec_type,
        }));
    }

    Ok(Json(ApiResponse::success(
        serde_json::to_value(enriched).unwrap_or_default(),
    )))
}

/// GET /meta-optimizer/canaries/history?limit=N
///
/// Returns completed canary rollouts (promoted or rolled back).
pub async fn get_canary_history_handler(
    State(state): State<Arc<ApiState>>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let limit = params
        .get("limit")
        .and_then(|v| v.parse().ok())
        .unwrap_or(20u32);

    let history = crate::meta_optimizer::canary::get_canary_history(&state.app_state.pg_db, limit)
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to get canary history: {}", e))),
            )
        })?;

    Ok(Json(ApiResponse::success(
        serde_json::to_value(history).unwrap_or_default(),
    )))
}

/// POST /meta-optimizer/canaries/{id}/promote
pub async fn promote_canary_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<()>>, (StatusCode, Json<ApiResponse<()>>)> {
    crate::meta_optimizer::canary::promote_canary(&state.app_state.pg_db, &id).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to promote canary: {}", e))),
        )
    })?;
    Ok(Json(ApiResponse::success(())))
}

/// POST /meta-optimizer/canaries/{id}/rollback
pub async fn rollback_canary_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<()>>, (StatusCode, Json<ApiResponse<()>>)> {
    crate::meta_optimizer::canary::rollback_canary(&state.app_state.pg_db, &id).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to rollback canary: {}", e))),
        )
    })?;
    Ok(Json(ApiResponse::success(())))
}

/// GET /meta-optimizer/canaries/{id}/evaluation
pub async fn evaluate_canary_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let eval = crate::meta_optimizer::canary::evaluate_canary(&state.app_state.pg_db, &id)
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to evaluate canary: {}", e))),
            )
        })?;

    Ok(Json(ApiResponse::success(
        serde_json::to_value(eval).unwrap_or_default(),
    )))
}

// ---------------------------------------------------------------------------
// Prompt Optimization handlers (meta-prompt optimizer)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct PromptEvolutionQuery {
    pub agent_type: Option<String>,
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub struct PromptEvidenceQuery {
    pub phase: Option<String>,
    pub max_failures: Option<usize>,
    pub max_successes: Option<usize>,
}

async fn get_prompt_optimization_status_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let pg_db = &state.app_state.pg_db;

    let samples = crate::meta_optimizer::prompt_extractor::extract_prompt_samples_pg(pg_db, 500)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))?;
    let groups =
        crate::meta_optimizer::prompt_extractor::compute_group_metrics_with_db_pg(&samples, pg_db);
    let evolution = crate::meta_optimizer::prompt_evolution::get_evolution_history(pg_db, None, 50)
        .unwrap_or_default();

    let active_canaries: Vec<_> = evolution
        .iter()
        .filter(|e| e.canary_verdict.is_none())
        .cloned()
        .collect();

    Ok(Json(ApiResponse {
        success: true,
        data: Some(serde_json::json!({
            "prompt_groups": groups,
            "active_canaries": active_canaries,
            "evolution_history": evolution,
        })),
        error: None,
        error_detail: None,
        hint: None,
    }))
}

async fn get_prompt_group_metrics_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let pg_db = &state.app_state.pg_db;

    let samples = crate::meta_optimizer::prompt_extractor::extract_prompt_samples_pg(pg_db, 500)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))?;
    let groups =
        crate::meta_optimizer::prompt_extractor::compute_group_metrics_with_db_pg(&samples, pg_db);

    Ok(Json(ApiResponse {
        success: true,
        data: Some(serde_json::json!(groups)),
        error: None,
        error_detail: None,
        hint: None,
    }))
}

async fn get_prompt_optimization_evidence_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<PromptEvidenceQuery>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let pg_db = &state.app_state.pg_db;
    let phase = query.phase.as_deref().unwrap_or("generation");
    let max_failures = query.max_failures.unwrap_or(5);
    let max_successes = query.max_successes.unwrap_or(2);

    let (failures, successes) =
        crate::meta_optimizer::prompt_extractor::collect_evidence_samples_pg(
            pg_db,
            phase,
            "",
            max_failures,
            max_successes,
        )
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))?;

    Ok(Json(ApiResponse {
        success: true,
        data: Some(serde_json::json!({
            "failures": failures,
            "successes": successes,
        })),
        error: None,
        error_detail: None,
        hint: None,
    }))
}

async fn get_prompt_evolution_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<PromptEvolutionQuery>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let pg_db = &state.app_state.pg_db;
    let limit = query.limit.unwrap_or(50) as usize;

    let history = crate::meta_optimizer::prompt_evolution::get_evolution_history(
        pg_db,
        query.agent_type.as_deref(),
        limit,
    )
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))?;

    Ok(Json(ApiResponse {
        success: true,
        data: Some(serde_json::json!(history)),
        error: None,
        error_detail: None,
        hint: None,
    }))
}

// ---------------------------------------------------------------------------
// Duel pools / beam search / span events (read-only)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct DuelPoolsQuery {
    pub agent_type: Option<String>,
    pub status: Option<String>,
    pub limit: Option<i64>,
}

/// GET /meta-optimizer/duel-pools
pub async fn list_duel_pools_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<DuelPoolsQuery>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let pools = state
        .app_state
        .pg_db
        .list_duel_pools(
            query.agent_type.as_deref(),
            query.status.as_deref(),
            query.limit.unwrap_or(50),
        )
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to list duel pools: {}", e))),
            )
        })?;
    Ok(Json(ApiResponse::success(serde_json::json!(pools))))
}

/// GET /meta-optimizer/duel-pools/{id}
/// Returns pool info plus all candidates.
pub async fn get_duel_pool_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let make_err = |e: String| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to get duel pool: {}", e))),
        )
    };
    let pool = state
        .app_state
        .pg_db
        .get_duel_pool(&id)
        .await
        .map_err(make_err)?;
    let pool = match pool {
        Some(p) => p,
        None => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(api_error(format!("duel pool {} not found", id))),
            ))
        }
    };
    let candidates = state
        .app_state
        .pg_db
        .list_pool_candidates_full(&id)
        .await
        .map_err(make_err)?;
    Ok(Json(ApiResponse::success(serde_json::json!({
        "pool": pool,
        "candidates": candidates,
    }))))
}

#[derive(Debug, Deserialize)]
pub struct DuelResultsQuery {
    pub limit: Option<i64>,
}

/// GET /meta-optimizer/duel-pools/{id}/results
pub async fn list_duel_results_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Query(query): Query<DuelResultsQuery>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let results = state
        .app_state
        .pg_db
        .list_duel_results(&id, query.limit.unwrap_or(200))
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to list duel results: {}", e))),
            )
        })?;
    Ok(Json(ApiResponse::success(serde_json::json!(results))))
}

#[derive(Debug, Deserialize)]
pub struct BeamRunsQuery {
    pub agent_type: Option<String>,
    pub limit: Option<i64>,
}

/// GET /meta-optimizer/beam-runs
pub async fn list_beam_runs_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<BeamRunsQuery>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let runs = state
        .app_state
        .pg_db
        .list_beam_search_runs(query.agent_type.as_deref(), query.limit.unwrap_or(50))
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to list beam runs: {}", e))),
            )
        })?;
    Ok(Json(ApiResponse::success(serde_json::json!(runs))))
}

/// GET /meta-optimizer/beam-runs/{id}
/// Returns run candidates.
pub async fn get_beam_run_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let candidates = state
        .app_state
        .pg_db
        .list_beam_candidates(&id)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to list beam candidates: {}", e))),
            )
        })?;
    Ok(Json(ApiResponse::success(serde_json::json!({
        "beam_run_id": id,
        "candidates": candidates,
    }))))
}

#[derive(Debug, Deserialize)]
pub struct SpanEventsQuery {
    pub execution_id: Option<String>,
    pub trace_id: Option<String>,
    pub limit: Option<i64>,
}

/// GET /meta-optimizer/span-events?execution_id=X
pub async fn list_span_events_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<SpanEventsQuery>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let events = state
        .app_state
        .pg_db
        .list_span_events(
            query.execution_id.as_deref(),
            query.trace_id.as_deref(),
            query.limit.unwrap_or(500),
        )
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to list span events: {}", e))),
            )
        })?;
    Ok(Json(ApiResponse::success(serde_json::json!(events))))
}

// ---------------------------------------------------------------------------
// Route registration
// ---------------------------------------------------------------------------

/// Register meta-optimizer API routes.
pub fn routes() -> axum::Router<Arc<ApiState>> {
    use axum::routing::{get, post};

    axum::Router::new()
        .route(
            "/meta-optimizer/recommendations/{id}/apply",
            post(apply_recommendation_handler),
        )
        .route(
            "/meta-optimizer/recommendations/{id}/reject",
            post(reject_recommendation_handler),
        )
        .route(
            "/meta-optimizer/agent-trace-aggregates",
            get(get_agent_trace_aggregates_handler),
        )
        .route(
            "/meta-optimizer/prompt-variants",
            get(get_prompt_variants_handler),
        )
        .route(
            "/meta-optimizer/recommendations",
            get(get_recommendations_handler),
        )
        .route("/meta-optimizer/runs", get(get_optimizer_runs_handler))
        .route(
            "/meta-optimizer/cost-analysis",
            get(get_cost_analysis_handler),
        )
        .route(
            "/meta-optimizer/optimizer-context",
            get(get_optimizer_context_handler),
        )
        .route("/learning/outcomes", get(get_learning_outcomes_handler))
        .route(
            "/workflow-generation/feedback",
            get(get_generation_feedback_handler),
        )
        .route("/prompt-analysis", get(get_prompt_analysis_handler))
        .route(
            "/meta-optimizer/reflection-fixes",
            get(get_reflection_fixes_handler),
        )
        .route(
            "/meta-optimizer/iteration-history",
            get(get_iteration_history_handler),
        )
        // Eval spec routes (promptfoo-inspired declarative evaluation)
        .route("/meta-optimizer/eval-specs", get(get_eval_specs_handler))
        .route("/meta-optimizer/eval-specs", post(create_eval_spec_handler))
        .route(
            "/meta-optimizer/eval-results",
            get(get_eval_results_handler),
        )
        .route(
            "/meta-optimizer/recommendations/{id}/evaluate",
            post(evaluate_recommendation_handler),
        )
        // Live evaluation with I/O (LLM-as-judge)
        .route(
            "/meta-optimizer/evaluate-with-io",
            post(evaluate_with_io_handler),
        )
        // Canary rollout routes
        .route("/meta-optimizer/canaries", get(get_canaries_handler))
        .route(
            "/meta-optimizer/canaries/history",
            get(get_canary_history_handler),
        )
        .route(
            "/meta-optimizer/canaries/{id}/promote",
            post(promote_canary_handler),
        )
        .route(
            "/meta-optimizer/canaries/{id}/rollback",
            post(rollback_canary_handler),
        )
        .route(
            "/meta-optimizer/canaries/{id}/evaluation",
            get(evaluate_canary_handler),
        )
        // Prompt optimization routes (meta-prompt optimizer)
        .route(
            "/meta-optimizer/prompt-optimization/status",
            get(get_prompt_optimization_status_handler),
        )
        .route(
            "/meta-optimizer/prompt-optimization/group-metrics",
            get(get_prompt_group_metrics_handler),
        )
        .route(
            "/meta-optimizer/prompt-optimization/evidence",
            get(get_prompt_optimization_evidence_handler),
        )
        .route(
            "/meta-optimizer/prompt-evolution",
            get(get_prompt_evolution_handler),
        )
        // Duel pools / beam search / span events (read-only)
        .route("/meta-optimizer/duel-pools", get(list_duel_pools_handler))
        .route(
            "/meta-optimizer/duel-pools/{id}",
            get(get_duel_pool_handler),
        )
        .route(
            "/meta-optimizer/duel-pools/{id}/results",
            get(list_duel_results_handler),
        )
        .route("/meta-optimizer/beam-runs", get(list_beam_runs_handler))
        .route("/meta-optimizer/beam-runs/{id}", get(get_beam_run_handler))
        .route("/meta-optimizer/span-events", get(list_span_events_handler))
    // Note: POST /prompt-optimization/trigger is handled via Tauri command
    // `trigger_meta_optimizer` with optimizer_type="meta_prompt", not HTTP API,
    // because launching workflows requires AppHandle and ConfigStorage.
}

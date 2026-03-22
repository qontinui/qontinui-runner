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
use rusqlite::params;
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

#[derive(Debug, Deserialize)]
pub struct AutoresearchQuery {
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
            let summaries =
                pipeline_traces::get_trace_summaries_l0(&state.app_state.checkpoint_db, limit)
                    .map_err(make_err)?;
            Ok(Json(ApiResponse::success(
                serde_json::to_value(summaries).unwrap_or_default(),
            )))
        }
        ContextTier::L1 => {
            let details = pipeline_traces::get_trace_details_l1(
                &state.app_state.checkpoint_db,
                query.agent_type.as_deref(),
                limit,
            )
            .map_err(make_err)?;
            Ok(Json(ApiResponse::success(
                serde_json::to_value(details).unwrap_or_default(),
            )))
        }
        ContextTier::L2 => {
            // If trace_id is provided, return a single full trace record
            if let Some(ref trace_id) = query.trace_id {
                let trace =
                    pipeline_traces::get_trace_full_l2(&state.app_state.checkpoint_db, trace_id)
                        .map_err(make_err)?;
                Ok(Json(ApiResponse::success(
                    serde_json::to_value(trace).unwrap_or_default(),
                )))
            } else {
                let aggregates = pipeline_traces::get_agent_trace_aggregates(
                    &state.app_state.checkpoint_db,
                    limit,
                )
                .map_err(make_err)?;
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
        prompt_registry::list_variants(&state.app_state.checkpoint_db, query.agent_type.as_deref())
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
        &state.app_state.checkpoint_db,
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
    let runs =
        recommendations::list_optimizer_runs(&state.app_state.checkpoint_db).map_err(|e| {
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
        .checkpoint_db
        .with_conn(move |conn| {
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
                    WHERE created_at > ?1"#,
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

            Ok(out)
        })
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
        crate::meta_optimizer::cost_optimizer::build_cost_analysis(&state.app_state.checkpoint_db)
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
            let status_c = status.clone();
            let wa_c = workflow_architecture.clone();
            let summaries = state
                .app_state
                .checkpoint_db
                .with_conn(move |conn| {
                    let mut sql = String::from(
                        r#"SELECT status,
                                  COUNT(*) as run_count,
                                  COALESCE(AVG(duration_secs), 0.0) as avg_duration_secs,
                                  COALESCE(AVG(iterations), 0.0) as avg_iterations
                           FROM learning_outcomes
                           WHERE 1=1"#,
                    );
                    let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
                    let mut idx = 1;

                    if let Some(ref s) = status_c {
                        sql.push_str(&format!(" AND status = ?{}", idx));
                        param_values.push(Box::new(s.clone()));
                        idx += 1;
                    }
                    if let Some(ref wa) = wa_c {
                        sql.push_str(&format!(" AND workflow_architecture = ?{}", idx));
                        param_values.push(Box::new(wa.clone()));
                        idx += 1;
                    }
                    let _ = idx;

                    sql.push_str(" GROUP BY status ORDER BY run_count DESC");

                    let mut stmt = conn
                        .prepare(&sql)
                        .map_err(|e| format!("Query error: {}", e))?;
                    let rows: Vec<LearningOutcomeSummaryL0> = stmt
                        .query_map(rusqlite::params_from_iter(param_values.iter()), |row| {
                            Ok(LearningOutcomeSummaryL0 {
                                status: row.get(0)?,
                                run_count: row.get(1)?,
                                avg_duration_secs: row.get(2)?,
                                avg_iterations: row.get(3)?,
                            })
                        })
                        .map_err(|e| format!("Query error: {}", e))?
                        .filter_map(|r| r.ok())
                        .collect();
                    Ok(rows)
                })
                .map_err(make_err)?;
            Ok(Json(ApiResponse::success(
                serde_json::to_value(summaries).unwrap_or_default(),
            )))
        }
        ContextTier::L1 => {
            let status_c = status.clone();
            let wa_c = workflow_architecture.clone();
            let details = state
                .app_state
                .checkpoint_db
                .with_conn(move |conn| {
                    let mut sql = String::from(
                        r#"SELECT id, task_id, status, duration_secs, iterations,
                                  workflow_architecture, error_type, created_at
                           FROM learning_outcomes
                           WHERE 1=1"#,
                    );
                    let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
                    let mut idx = 1;

                    if let Some(ref s) = status_c {
                        sql.push_str(&format!(" AND status = ?{}", idx));
                        param_values.push(Box::new(s.clone()));
                        idx += 1;
                    }
                    if let Some(ref wa) = wa_c {
                        sql.push_str(&format!(" AND workflow_architecture = ?{}", idx));
                        param_values.push(Box::new(wa.clone()));
                        idx += 1;
                    }
                    let _ = idx;

                    sql.push_str(&format!(" ORDER BY created_at DESC LIMIT {}", limit));

                    let mut stmt = conn
                        .prepare(&sql)
                        .map_err(|e| format!("Query error: {}", e))?;
                    let rows: Vec<LearningOutcomeDetailL1> = stmt
                        .query_map(rusqlite::params_from_iter(param_values.iter()), |row| {
                            Ok(LearningOutcomeDetailL1 {
                                id: row.get(0)?,
                                task_id: row.get(1)?,
                                status: row.get(2)?,
                                duration_secs: row.get(3)?,
                                iterations: row.get(4)?,
                                workflow_architecture: row.get(5)?,
                                error_type: row.get(6)?,
                                created_at: row.get(7)?,
                            })
                        })
                        .map_err(|e| format!("Query error: {}", e))?
                        .filter_map(|r| r.ok())
                        .collect();
                    Ok(rows)
                })
                .map_err(make_err)?;
            Ok(Json(ApiResponse::success(
                serde_json::to_value(details).unwrap_or_default(),
            )))
        }
        ContextTier::L2 => {
            // Full records (existing behavior)
            let outcomes = state
                .app_state
                .checkpoint_db
                .with_conn(move |conn| {
                    let mut sql = String::from(
                        r#"SELECT id, task_id, status, duration_secs, iterations, strategy,
                                  tools_used, files_modified, error_type, error_message,
                                  workflow_architecture, created_at, step_count,
                                  verification_step_count, agentic_step_count, has_ui_bridge,
                                  technology_tags, domain_tags, complexity_tier
                           FROM learning_outcomes
                           WHERE 1=1"#,
                    );
                    let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
                    let mut param_idx = 1u32;

                    if let Some(ref s) = status {
                        param_idx += 1;
                        sql.push_str(&format!(" AND status = ?{}", param_idx));
                        param_values.push(Box::new(s.clone()));
                    }
                    if let Some(ref wa) = workflow_architecture {
                        param_idx += 1;
                        sql.push_str(&format!(" AND workflow_architecture = ?{}", param_idx));
                        param_values.push(Box::new(wa.clone()));
                    }

                    sql.push_str(" ORDER BY created_at DESC LIMIT ?1");

                    let mut stmt = conn
                        .prepare(&sql)
                        .map_err(|e| format!("Failed to prepare learning_outcomes query: {}", e))?;

                    let mut all_params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
                    all_params.push(Box::new(limit as i64));
                    all_params.extend(param_values);

                    let param_refs: Vec<&dyn rusqlite::types::ToSql> =
                        all_params.iter().map(|p| p.as_ref()).collect();

                    let results: Vec<serde_json::Value> = stmt
                        .query_map(param_refs.as_slice(), |row| {
                            Ok(serde_json::json!({
                                "id": row.get::<_, String>(0).unwrap_or_default(),
                                "task_id": row.get::<_, String>(1).unwrap_or_default(),
                                "status": row.get::<_, String>(2).unwrap_or_default(),
                                "duration_secs": row.get::<_, Option<f64>>(3).unwrap_or(None),
                                "iterations": row.get::<_, Option<i64>>(4).unwrap_or(None),
                                "strategy": row.get::<_, Option<String>>(5).unwrap_or(None),
                                "tools_used": row.get::<_, Option<String>>(6).unwrap_or(None),
                                "files_modified": row.get::<_, Option<String>>(7).unwrap_or(None),
                                "error_type": row.get::<_, Option<String>>(8).unwrap_or(None),
                                "error_message": row.get::<_, Option<String>>(9).unwrap_or(None),
                                "workflow_architecture": row.get::<_, Option<String>>(10).unwrap_or(None),
                                "created_at": row.get::<_, String>(11).unwrap_or_default(),
                                "step_count": row.get::<_, Option<i64>>(12).unwrap_or(None),
                                "verification_step_count": row.get::<_, Option<i64>>(13).unwrap_or(None),
                                "agentic_step_count": row.get::<_, Option<i64>>(14).unwrap_or(None),
                                "has_ui_bridge": row.get::<_, Option<i32>>(15).unwrap_or(None).map(|v| v != 0),
                                "technology_tags": row.get::<_, Option<String>>(16).unwrap_or(None).and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                                "domain_tags": row.get::<_, Option<String>>(17).unwrap_or(None).and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                                "complexity_tier": row.get::<_, Option<String>>(18).unwrap_or(None),
                            }))
                        })
                        .map_err(|e| format!("Failed to query learning_outcomes: {}", e))?
                        .filter_map(|r| r.ok())
                        .collect();

                    Ok(results)
                })
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
            let ft_c = feedback_type.clone();
            let summaries = state
                .app_state
                .checkpoint_db
                .with_conn(move |conn| {
                    let mut sql = String::from(
                        r#"SELECT feedback_type,
                                  COUNT(*) as total_count,
                                  AVG(rating) as avg_rating
                           FROM workflow_generation_feedback
                           WHERE 1=1"#,
                    );
                    let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
                    let mut idx = 1;

                    if let Some(ref ft) = ft_c {
                        sql.push_str(&format!(" AND feedback_type = ?{}", idx));
                        param_values.push(Box::new(ft.clone()));
                        idx += 1;
                    }
                    let _ = idx;

                    sql.push_str(" GROUP BY feedback_type ORDER BY total_count DESC");

                    let mut stmt = conn
                        .prepare(&sql)
                        .map_err(|e| format!("Query error: {}", e))?;
                    let rows: Vec<GenerationFeedbackSummaryL0> = stmt
                        .query_map(rusqlite::params_from_iter(param_values.iter()), |row| {
                            Ok(GenerationFeedbackSummaryL0 {
                                feedback_type: row.get(0)?,
                                total_count: row.get(1)?,
                                avg_rating: row.get(2)?,
                            })
                        })
                        .map_err(|e| format!("Query error: {}", e))?
                        .filter_map(|r| r.ok())
                        .collect();
                    Ok(rows)
                })
                .map_err(make_err)?;
            Ok(Json(ApiResponse::success(
                serde_json::to_value(summaries).unwrap_or_default(),
            )))
        }
        ContextTier::L1 => {
            let ft_c = feedback_type.clone();
            let details = state
                .app_state
                .checkpoint_db
                .with_conn(move |conn| {
                    let mut sql = String::from(
                        r#"SELECT id, feedback_type, edited_field, rating,
                                  workflow_category, created_at
                           FROM workflow_generation_feedback
                           WHERE 1=1"#,
                    );
                    let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
                    let mut idx = 1;

                    if let Some(ref ft) = ft_c {
                        sql.push_str(&format!(" AND feedback_type = ?{}", idx));
                        param_values.push(Box::new(ft.clone()));
                        idx += 1;
                    }
                    let _ = idx;

                    sql.push_str(&format!(" ORDER BY created_at DESC LIMIT {}", limit));

                    let mut stmt = conn
                        .prepare(&sql)
                        .map_err(|e| format!("Query error: {}", e))?;
                    let rows: Vec<GenerationFeedbackDetailL1> = stmt
                        .query_map(rusqlite::params_from_iter(param_values.iter()), |row| {
                            Ok(GenerationFeedbackDetailL1 {
                                id: row.get(0)?,
                                feedback_type: row.get(1)?,
                                edited_field: row.get(2)?,
                                rating: row.get(3)?,
                                workflow_category: row.get(4)?,
                                created_at: row.get(5)?,
                            })
                        })
                        .map_err(|e| format!("Query error: {}", e))?
                        .filter_map(|r| r.ok())
                        .collect();
                    Ok(rows)
                })
                .map_err(make_err)?;
            Ok(Json(ApiResponse::success(
                serde_json::to_value(details).unwrap_or_default(),
            )))
        }
        ContextTier::L2 => {
            // Full records (existing behavior)
            let feedback = state
                .app_state
                .checkpoint_db
                .with_conn(move |conn| {
                    let mut sql = String::from(
                        r#"SELECT id, workflow_id, task_run_id, feedback_type, edited_field,
                                  old_value, new_value, delete_reason, rating, rating_comment,
                                  workflow_category, workflow_description, created_at
                           FROM workflow_generation_feedback
                           WHERE 1=1"#,
                    );
                    let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

                    if let Some(ref ft) = feedback_type {
                        sql.push_str(" AND feedback_type = ?2");
                        param_values.push(Box::new(ft.clone()));
                    }

                    sql.push_str(" ORDER BY created_at DESC LIMIT ?1");

                    let mut stmt = conn
                        .prepare(&sql)
                        .map_err(|e| format!("Failed to prepare generation_feedback query: {}", e))?;

                    let mut all_params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
                    all_params.push(Box::new(limit as i64));
                    all_params.extend(param_values);

                    let param_refs: Vec<&dyn rusqlite::types::ToSql> =
                        all_params.iter().map(|p| p.as_ref()).collect();

                    let results: Vec<serde_json::Value> = stmt
                        .query_map(param_refs.as_slice(), |row| {
                            Ok(serde_json::json!({
                                "id": row.get::<_, String>(0).unwrap_or_default(),
                                "workflow_id": row.get::<_, String>(1).unwrap_or_default(),
                                "task_run_id": row.get::<_, Option<String>>(2).unwrap_or(None),
                                "feedback_type": row.get::<_, String>(3).unwrap_or_default(),
                                "edited_field": row.get::<_, Option<String>>(4).unwrap_or(None),
                                "old_value": row.get::<_, Option<String>>(5).unwrap_or(None),
                                "new_value": row.get::<_, Option<String>>(6).unwrap_or(None),
                                "delete_reason": row.get::<_, Option<String>>(7).unwrap_or(None),
                                "rating": row.get::<_, Option<i64>>(8).unwrap_or(None),
                                "rating_comment": row.get::<_, Option<String>>(9).unwrap_or(None),
                                "workflow_category": row.get::<_, Option<String>>(10).unwrap_or(None),
                                "workflow_description": row.get::<_, Option<String>>(11).unwrap_or(None),
                                "created_at": row.get::<_, String>(12).unwrap_or_default(),
                            }))
                        })
                        .map_err(|e| format!("Failed to query generation_feedback: {}", e))?
                        .filter_map(|r| r.ok())
                        .collect();

                    Ok(results)
                })
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

/// GET /autoresearch/campaigns?limit=N
///
/// Returns autoresearch campaign data with their experiments.
pub async fn get_autoresearch_campaigns_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<AutoresearchQuery>,
) -> Result<Json<ApiResponse<Vec<serde_json::Value>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let limit = query.limit.unwrap_or(50);

    let campaigns = state
        .app_state
        .checkpoint_db
        .with_conn(move |conn| {
            let mut stmt = conn
                .prepare(
                    r#"SELECT id, name, config_json, current_control_json, status,
                              experiment_count, accepted_count, created_at
                       FROM autoresearch_campaigns
                       ORDER BY created_at DESC
                       LIMIT ?1"#,
                )
                .map_err(|e| format!("Failed to prepare autoresearch_campaigns query: {}", e))?;

            let campaigns: Vec<serde_json::Value> = stmt
                .query_map(params![limit as i64], |row| {
                    Ok((
                        row.get::<_, String>(0).unwrap_or_default(),
                        row.get::<_, String>(1).unwrap_or_default(),
                        row.get::<_, String>(2).unwrap_or_default(),
                        row.get::<_, String>(3).unwrap_or_default(),
                        row.get::<_, String>(4).unwrap_or_default(),
                        row.get::<_, i64>(5).unwrap_or(0),
                        row.get::<_, i64>(6).unwrap_or(0),
                        row.get::<_, String>(7).unwrap_or_default(),
                    ))
                })
                .map_err(|e| format!("Failed to query autoresearch_campaigns: {}", e))?
                .filter_map(|r| r.ok())
                .map(|(id, name, config_json, current_control_json, status, experiment_count, accepted_count, created_at)| {
                    // Fetch experiments for this campaign
                    let experiments: Vec<serde_json::Value> = conn
                        .prepare(
                            r#"SELECT id, experiment_number, config_json, aggregate_json,
                                      accepted, reason, p_value, created_at
                               FROM autoresearch_experiments
                               WHERE campaign_id = ?1
                               ORDER BY experiment_number DESC
                               LIMIT 10"#,
                        )
                        .and_then(|mut exp_stmt| {
                            let exps = exp_stmt
                                .query_map(params![id], |row| {
                                    Ok(serde_json::json!({
                                        "id": row.get::<_, String>(0).unwrap_or_default(),
                                        "experiment_number": row.get::<_, i64>(1).unwrap_or(0),
                                        "config_json": row.get::<_, String>(2).unwrap_or_default(),
                                        "aggregate_json": row.get::<_, String>(3).unwrap_or_default(),
                                        "accepted": row.get::<_, i64>(4).unwrap_or(0) != 0,
                                        "reason": row.get::<_, Option<String>>(5).unwrap_or(None),
                                        "p_value": row.get::<_, Option<f64>>(6).unwrap_or(None),
                                        "created_at": row.get::<_, String>(7).unwrap_or_default(),
                                    }))
                                })?
                                .filter_map(|r| r.ok())
                                .collect();
                            Ok(exps)
                        })
                        .unwrap_or_default();

                    serde_json::json!({
                        "id": id,
                        "name": name,
                        "config_json": config_json,
                        "current_control_json": current_control_json,
                        "status": status,
                        "experiment_count": experiment_count,
                        "accepted_count": accepted_count,
                        "created_at": created_at,
                        "experiments": experiments,
                    })
                })
                .collect();

            Ok(campaigns)
        })
        .unwrap_or_default();

    Ok(Json(ApiResponse::success(campaigns)))
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
                .checkpoint_db
                .with_conn(move |conn| {
                    let mut sql = String::from(
                        r#"SELECT fix_type,
                                  COUNT(*) as total_count,
                                  SUM(CASE WHEN effectiveness = 'effective' THEN 1 ELSE 0 END) as effective_count
                           FROM reflection_fixes WHERE 1=1"#,
                    );
                    let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
                    let mut idx = 1;

                    if let Some(ref agent) = source_agent {
                        sql.push_str(&format!(" AND source_agent = ?{}", idx));
                        param_values.push(Box::new(agent.clone()));
                        idx += 1;
                    }
                    let _ = idx;

                    sql.push_str(" GROUP BY fix_type ORDER BY total_count DESC");

                    let mut stmt = conn
                        .prepare(&sql)
                        .map_err(|e| format!("Query error: {}", e))?;
                    let rows: Vec<ReflectionFixSummaryL0> = stmt
                        .query_map(rusqlite::params_from_iter(param_values.iter()), |row| {
                            Ok(ReflectionFixSummaryL0 {
                                fix_type: row.get(0)?,
                                total_count: row.get(1)?,
                                effective_count: row.get(2)?,
                            })
                        })
                        .map_err(|e| format!("Query error: {}", e))?
                        .filter_map(|r| r.ok())
                        .collect();
                    Ok(rows)
                })
                .map_err(make_err)?;
            Ok(Json(ApiResponse::success(
                serde_json::to_value(summaries).unwrap_or_default(),
            )))
        }
        ContextTier::L1 => {
            let details = state
                .app_state
                .checkpoint_db
                .with_conn(move |conn| {
                    let mut sql = String::from(
                        r#"SELECT id, fix_type, fix_description, confidence,
                                  effectiveness, source_agent, created_at
                           FROM reflection_fixes WHERE 1=1"#,
                    );
                    let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
                    let mut idx = 1;

                    if let Some(ref agent) = source_agent {
                        sql.push_str(&format!(" AND source_agent = ?{}", idx));
                        param_values.push(Box::new(agent.clone()));
                        idx += 1;
                    }
                    let _ = idx;

                    sql.push_str(&format!(" ORDER BY created_at DESC LIMIT {}", limit));

                    let mut stmt = conn
                        .prepare(&sql)
                        .map_err(|e| format!("Query error: {}", e))?;
                    let rows: Vec<ReflectionFixDetailL1> = stmt
                        .query_map(rusqlite::params_from_iter(param_values.iter()), |row| {
                            Ok(ReflectionFixDetailL1 {
                                id: row.get(0)?,
                                fix_type: row.get(1)?,
                                fix_description: row.get(2)?,
                                confidence: row.get(3)?,
                                effectiveness: row.get(4)?,
                                source_agent: row.get(5)?,
                                created_at: row.get(6)?,
                            })
                        })
                        .map_err(|e| format!("Query error: {}", e))?
                        .filter_map(|r| r.ok())
                        .collect();
                    Ok(rows)
                })
                .map_err(make_err)?;
            Ok(Json(ApiResponse::success(
                serde_json::to_value(details).unwrap_or_default(),
            )))
        }
        ContextTier::L2 => {
            // Full records (existing behavior)
            let fixes = state
                .app_state
                .checkpoint_db
                .with_conn(move |conn| {
                    let mut sql = String::from(
                        r#"SELECT id, source_task_run_id, reflection_task_run_id,
                                  source_finding_id, source_knowledge_id,
                                  fix_type, fix_description, file_changed,
                                  old_value, new_value, confidence, status,
                                  effectiveness, effectiveness_evidence,
                                  applied_at, evaluated_at, created_at,
                                  content_hash, source_agent, reflection_scope,
                                  project_path, target_component, reuse_count,
                                  reasoning, alternatives_considered,
                                  applicability_context
                           FROM reflection_fixes WHERE 1=1"#,
                    );
                    let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
                    let mut idx = 1;

                    if let Some(ref agent) = source_agent {
                        sql.push_str(&format!(" AND source_agent = ?{}", idx));
                        param_values.push(Box::new(agent.clone()));
                        idx += 1;
                    }
                    let _ = idx;

                    sql.push_str(&format!(" ORDER BY created_at DESC LIMIT {}", limit));

                    let mut stmt = conn
                        .prepare(&sql)
                        .map_err(|e| format!("Query error: {}", e))?;
                    let rows: Vec<serde_json::Value> = stmt
                        .query_map(rusqlite::params_from_iter(param_values.iter()), |row| {
                            Ok(serde_json::json!({
                                "id": row.get::<_, String>(0)?,
                                "source_task_run_id": row.get::<_, String>(1)?,
                                "reflection_task_run_id": row.get::<_, String>(2)?,
                                "source_finding_id": row.get::<_, Option<String>>(3)?,
                                "source_knowledge_id": row.get::<_, Option<String>>(4)?,
                                "fix_type": row.get::<_, String>(5)?,
                                "fix_description": row.get::<_, String>(6)?,
                                "file_changed": row.get::<_, Option<String>>(7)?,
                                "old_value": row.get::<_, Option<String>>(8)?,
                                "new_value": row.get::<_, Option<String>>(9)?,
                                "confidence": row.get::<_, String>(10)?,
                                "status": row.get::<_, String>(11)?,
                                "effectiveness": row.get::<_, Option<String>>(12)?,
                                "effectiveness_evidence": row.get::<_, Option<String>>(13)?,
                                "applied_at": row.get::<_, String>(14)?,
                                "evaluated_at": row.get::<_, Option<String>>(15)?,
                                "created_at": row.get::<_, String>(16)?,
                                "content_hash": row.get::<_, Option<String>>(17)?,
                                "source_agent": row.get::<_, Option<String>>(18)?,
                                "reflection_scope": row.get::<_, Option<String>>(19)?,
                                "project_path": row.get::<_, Option<String>>(20)?,
                                "target_component": row.get::<_, Option<String>>(21)?,
                                "reuse_count": row.get::<_, Option<i64>>(22)?,
                                "reasoning": row.get::<_, Option<String>>(23)?,
                                "alternatives_considered": row.get::<_, Option<String>>(24)?,
                                "applicability_context": row.get::<_, Option<String>>(25)?,
                            }))
                        })
                        .map_err(|e| format!("Query error: {}", e))?
                        .filter_map(|r| r.ok())
                        .collect();
                    Ok(rows)
                })
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
    recommendations::apply_recommendation_with_side_effects(&state.app_state.checkpoint_db, &id)
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to apply recommendation: {}", e))),
            )
        })?;
    Ok(Json(ApiResponse::success(())))
}

/// POST /meta-optimizer/recommendations/:id/reject
///
/// Rejects a pending recommendation.
pub async fn reject_recommendation_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<()>>, (StatusCode, Json<ApiResponse<()>>)> {
    recommendations::reject_recommendation(&state.app_state.checkpoint_db, &id).map_err(|e| {
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

    match tier {
        ContextTier::L0 => {
            // Aggregate: group by status, compute avg iterations, confidence delta, top approaches
            let summaries = state
                .app_state
                .checkpoint_db
                .with_conn(move |conn| {
                    let mut sql = String::from(
                        r#"SELECT status, iteration_history
                           FROM task_runs
                           WHERE iteration_history IS NOT NULL
                             AND iteration_history != '[]'"#,
                    );
                    let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
                    if let Some(ref s) = status_filter {
                        sql.push_str(" AND status = ?1");
                        param_values.push(Box::new(s.clone()));
                    }
                    sql.push_str(&format!(" ORDER BY created_at DESC LIMIT {}", limit));

                    let mut stmt = conn
                        .prepare(&sql)
                        .map_err(|e| format!("Query error: {}", e))?;

                    let rows: Vec<(String, String)> = stmt
                        .query_map(rusqlite::params_from_iter(param_values.iter()), |row| {
                            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                        })
                        .map_err(|e| format!("Query error: {}", e))?
                        .filter_map(|r| r.ok())
                        .collect();

                    // Group by status and compute aggregates
                    let mut groups: std::collections::HashMap<String, Vec<Vec<serde_json::Value>>> =
                        std::collections::HashMap::new();
                    for (status, history_json) in &rows {
                        if let Ok(entries) =
                            serde_json::from_str::<Vec<serde_json::Value>>(history_json)
                        {
                            groups.entry(status.clone()).or_default().push(entries);
                        }
                    }

                    let mut summaries = Vec::new();
                    for (status, runs) in &groups {
                        let run_count = runs.len() as i64;
                        let mut total_iterations: usize = 0;
                        let mut total_confidence_delta: f64 = 0.0;
                        let mut approach_counts: std::collections::HashMap<String, usize> =
                            std::collections::HashMap::new();

                        for entries in runs {
                            total_iterations += entries.len();
                            for entry in entries {
                                if let Some(delta) =
                                    entry.get("confidence_delta").and_then(|d| d.as_f64())
                                {
                                    total_confidence_delta += delta;
                                }
                                if let Some(approach) =
                                    entry.get("approach").and_then(|a| a.as_str())
                                {
                                    let key: String = approach.chars().take(50).collect();
                                    *approach_counts.entry(key).or_default() += 1;
                                }
                            }
                        }

                        let mut top_approaches: Vec<(String, usize)> =
                            approach_counts.into_iter().collect();
                        top_approaches.sort_by(|a, b| b.1.cmp(&a.1));
                        let top_5: Vec<String> = top_approaches
                            .into_iter()
                            .take(5)
                            .map(|(approach, count)| format!("{} (x{})", approach, count))
                            .collect();

                        summaries.push(crate::meta_optimizer::types::IterationHistorySummaryL0 {
                            status: status.clone(),
                            run_count,
                            avg_iterations: total_iterations as f64 / run_count as f64,
                            avg_final_confidence_delta: total_confidence_delta / run_count as f64,
                            most_common_approaches: top_5,
                        });
                    }

                    Ok(summaries)
                })
                .map_err(make_err)?;

            Ok(Json(ApiResponse::success(
                serde_json::to_value(summaries).unwrap_or_default(),
            )))
        }
        ContextTier::L1 => {
            // Per-run: iteration count, approaches, confidence trajectory
            let details = state
                .app_state
                .checkpoint_db
                .with_conn(move |conn| {
                    let mut sql = String::from(
                        r#"SELECT id, task_name, status, iteration_history, created_at
                           FROM task_runs
                           WHERE iteration_history IS NOT NULL
                             AND iteration_history != '[]'"#,
                    );
                    let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
                    if let Some(ref s) = status_filter {
                        sql.push_str(" AND status = ?1");
                        param_values.push(Box::new(s.clone()));
                    }
                    sql.push_str(&format!(" ORDER BY created_at DESC LIMIT {}", limit));

                    let mut stmt = conn
                        .prepare(&sql)
                        .map_err(|e| format!("Query error: {}", e))?;

                    let rows: Vec<crate::meta_optimizer::types::IterationHistoryDetailL1> = stmt
                        .query_map(rusqlite::params_from_iter(param_values.iter()), |row| {
                            let id: String = row.get(0)?;
                            let task_name: String = row.get(1)?;
                            let status: String = row.get(2)?;
                            let history_json: String = row.get(3)?;
                            let created_at: String = row.get(4)?;

                            let entries: Vec<serde_json::Value> =
                                serde_json::from_str(&history_json).unwrap_or_default();

                            let iteration_count = entries.len();
                            let total_confidence_delta: f64 = entries
                                .iter()
                                .filter_map(|e| e.get("confidence_delta").and_then(|d| d.as_f64()))
                                .sum();
                            let approaches: Vec<String> = entries
                                .iter()
                                .filter_map(|e| {
                                    e.get("approach").and_then(|a| a.as_str()).map(String::from)
                                })
                                .collect();

                            Ok(crate::meta_optimizer::types::IterationHistoryDetailL1 {
                                task_run_id: id,
                                task_name,
                                status,
                                iteration_count,
                                total_confidence_delta,
                                approaches,
                                created_at,
                            })
                        })
                        .map_err(|e| format!("Query error: {}", e))?
                        .filter_map(|r| r.ok())
                        .collect();

                    Ok(rows)
                })
                .map_err(make_err)?;

            Ok(Json(ApiResponse::success(
                serde_json::to_value(details).unwrap_or_default(),
            )))
        }
        ContextTier::L2 => {
            // Full raw iteration_history JSON per run
            let full = state
                .app_state
                .checkpoint_db
                .with_conn(move |conn| {
                    let mut sql = String::from(
                        r#"SELECT id, task_name, status, iteration_history, created_at
                           FROM task_runs
                           WHERE iteration_history IS NOT NULL
                             AND iteration_history != '[]'"#,
                    );
                    let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
                    if let Some(ref s) = status_filter {
                        sql.push_str(" AND status = ?1");
                        param_values.push(Box::new(s.clone()));
                    }
                    sql.push_str(&format!(" ORDER BY created_at DESC LIMIT {}", limit));

                    let mut stmt = conn
                        .prepare(&sql)
                        .map_err(|e| format!("Query error: {}", e))?;

                    let rows: Vec<serde_json::Value> = stmt
                        .query_map(rusqlite::params_from_iter(param_values.iter()), |row| {
                            let id: String = row.get(0)?;
                            let task_name: String = row.get(1)?;
                            let status: String = row.get(2)?;
                            let history_json: String = row.get(3)?;
                            let created_at: String = row.get(4)?;

                            let entries: serde_json::Value =
                                serde_json::from_str(&history_json).unwrap_or_default();

                            Ok(serde_json::json!({
                                "task_run_id": id,
                                "task_name": task_name,
                                "status": status,
                                "iteration_history": entries,
                                "created_at": created_at,
                            }))
                        })
                        .map_err(|e| format!("Query error: {}", e))?
                        .filter_map(|r| r.ok())
                        .collect();

                    Ok(rows)
                })
                .map_err(make_err)?;

            Ok(Json(ApiResponse::success(
                serde_json::to_value(full).unwrap_or_default(),
            )))
        }
    }
}

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
        &state.app_state.checkpoint_db,
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

    crate::meta_optimizer::eval_spec::save_eval_spec(&state.app_state.checkpoint_db, &spec)
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
        &state.app_state.checkpoint_db,
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

    let result = crate::meta_optimizer::eval_runner::validate_recommendation(
        &state.app_state.checkpoint_db,
        &id,
    )
    .map_err(make_err)?;

    Ok(Json(ApiResponse::success(
        serde_json::to_value(result).unwrap_or_default(),
    )))
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
            "/autoresearch/campaigns",
            get(get_autoresearch_campaigns_handler),
        )
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
}

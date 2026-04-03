//! Step Quality Evaluation HTTP API handlers
//!
//! Provides endpoints for evaluating verification step quality:
//! - Evaluate entire workflow's verification steps
//! - Evaluate a single step
//! - Get semantic coverage matrix
//! - Get repair guidance for weak steps

use axum::{
    extract::State,
    http::StatusCode,
    response::Json,
    routing::{get, post},
    Router,
};
use serde::Deserialize;
use std::sync::Arc;
use tracing::info;

use crate::mcp::types::{api_error, ApiResponse, ApiState};
use crate::workflow_generation::evaluation;

// ============================================================================
// Request Types
// ============================================================================

#[derive(Debug, Deserialize)]
pub struct EvaluateWorkflowRequest {
    /// The workflow to evaluate (full UnifiedWorkflow JSON)
    pub workflow: crate::unified_workflows::UnifiedWorkflow,
    /// Acceptance criteria to evaluate against
    #[serde(default)]
    pub acceptance_criteria: Option<crate::workflow_generation::specification::AcceptanceCriteria>,
    /// Scoring strategy: "fast_only", "standard", "full"
    #[serde(default = "default_strategy")]
    pub strategy: String,
    /// PRM service base URL (only used with "full" strategy)
    #[serde(default)]
    pub prm_base_url: Option<String>,
}

fn default_strategy() -> String {
    "fast_only".to_string()
}

#[derive(Debug, Deserialize)]
pub struct EvaluateStepRequest {
    /// The step to evaluate (JSON object)
    pub step: serde_json::Value,
    /// Acceptance criteria to evaluate against
    #[serde(default)]
    pub acceptance_criteria: Option<crate::workflow_generation::specification::AcceptanceCriteria>,
}

#[derive(Debug, Deserialize)]
pub struct RepairGuidanceRequest {
    /// The workflow to generate repair guidance for
    pub workflow: crate::unified_workflows::UnifiedWorkflow,
    /// Acceptance criteria
    #[serde(default)]
    pub acceptance_criteria: Option<crate::workflow_generation::specification::AcceptanceCriteria>,
}

// ============================================================================
// Handlers
// ============================================================================

async fn evaluate_workflow_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<EvaluateWorkflowRequest>,
) -> Result<Json<ApiResponse<evaluation::WorkflowEvaluation>>, (StatusCode, Json<ApiResponse<()>>)>
{
    let doctor_handle = state.doctor_handle.clone();

    let strategy = match body.strategy.as_str() {
        "standard" => evaluation::ScoringStrategy::Standard,
        "full" => evaluation::ScoringStrategy::Full,
        _ => evaluation::ScoringStrategy::FastOnly,
    };

    // Default PRM service URL when strategy is "full" and no URL was provided
    let prm_url = body.prm_base_url.or_else(|| {
        if matches!(strategy, evaluation::ScoringStrategy::Full) {
            std::env::var("QONTINUI_PRM_URL")
                .ok()
                .or_else(|| Some("http://localhost:8400".to_string()))
        } else {
            None
        }
    });

    let result = tokio::task::spawn_blocking(move || {
        evaluation::evaluate_workflow(
            &body.workflow,
            body.acceptance_criteria.as_ref(),
            strategy,
            doctor_handle.as_ref(),
            None, // model
            None, // provider
            prm_url.as_deref(),
        )
    })
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(e.to_string())),
        )
    })?;

    info!(
        "Evaluated workflow: overall={:.3}, gate={}",
        result.overall_score,
        if result.quality_gate.passed {
            "PASS"
        } else {
            "FAIL"
        }
    );

    Ok(Json(ApiResponse::success(result)))
}

async fn evaluate_step_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<EvaluateStepRequest>,
) -> Result<Json<ApiResponse<evaluation::StepEvaluation>>, (StatusCode, Json<ApiResponse<()>>)> {
    let doctor_handle = state.doctor_handle.clone();

    let result = tokio::task::spawn_blocking(move || {
        evaluation::evaluate_step(
            &body.step,
            body.acceptance_criteria.as_ref(),
            doctor_handle.as_ref(),
            None,
            None,
        )
    })
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(e.to_string())),
        )
    })?;

    Ok(Json(ApiResponse::success(result)))
}

async fn repair_guidance_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<RepairGuidanceRequest>,
) -> Result<
    Json<ApiResponse<Vec<evaluation::RepairInstruction>>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    let doctor_handle = state.doctor_handle.clone();

    let result = tokio::task::spawn_blocking(move || {
        let eval = evaluation::evaluate_workflow(
            &body.workflow,
            body.acceptance_criteria.as_ref(),
            evaluation::ScoringStrategy::FastOnly,
            doctor_handle.as_ref(),
            None,
            None,
            None,
        );
        evaluation::generate_repair_guidance(&eval)
    })
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(e.to_string())),
        )
    })?;

    info!("Generated {} repair instructions", result.len());

    Ok(Json(ApiResponse::success(result)))
}

async fn cache_stats_handler(
    State(_state): State<Arc<ApiState>>,
) -> Json<ApiResponse<crate::workflow_generation::evaluation::cache::CacheStats>> {
    let stats = crate::workflow_generation::evaluation::judges::get_cache_stats();
    Json(ApiResponse::success(stats))
}

// ============================================================================
// Routes
// ============================================================================

pub fn routes() -> Router<Arc<ApiState>> {
    Router::new()
        .route("/evaluation/workflow", post(evaluate_workflow_handler))
        .route("/evaluation/step", post(evaluate_step_handler))
        .route("/evaluation/repair-guidance", post(repair_guidance_handler))
        .route("/evaluation/cache-stats", get(cache_stats_handler))
}

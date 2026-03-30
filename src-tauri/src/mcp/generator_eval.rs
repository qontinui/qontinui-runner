//! Generator Evaluation HTTP API handlers
//!
//! Provides endpoints for the Generator Evaluation page:
//! - Dashboard metrics and trends
//! - Pipeline artifact inspection
//! - Edit analysis from feedback data
//! - Benchmark management and execution
//! - Example library (ground truth) management
//!
//! NOTE: All spawn_blocking calls in this module wrap complex SQLite operations
//! (pipeline artifacts, benchmarks, edit analysis, example workflows) that have
//! no PG equivalents yet. They use `checkpoint_db` directly.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::Json,
    routing::{delete, get, post, put},
    Router,
};
use chrono::Utc;
use serde::Deserialize;
use std::sync::Arc;
use tracing::{info, warn};
use uuid::Uuid;

use crate::database::{BenchmarkUpdate, GeneratorBenchmark, GeneratorBenchmarkResult};
use crate::mcp::types::{api_error, ApiResponse, ApiState};

// ============================================================================
// Query Parameters
// ============================================================================

#[derive(Debug, Deserialize)]
pub struct PaginationParams {
    #[serde(default = "default_limit")]
    pub limit: u32,
    #[serde(default)]
    pub offset: u32,
}

fn default_limit() -> u32 {
    20
}

#[derive(Debug, Deserialize)]
pub struct TrendsParams {
    #[serde(default = "default_days")]
    pub days: u32,
}

fn default_days() -> u32 {
    30
}

#[derive(Debug, Deserialize)]
pub struct BenchmarkResultsParams {
    #[serde(default = "default_results_limit")]
    pub limit: u32,
}

fn default_results_limit() -> u32 {
    20
}

// ============================================================================
// Request Types
// ============================================================================

#[derive(Debug, Deserialize)]
pub struct CreateBenchmarkRequest {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    pub expected_structure: crate::workflow_generation::benchmark::ExpectedStructure,
}

#[derive(Debug, Deserialize)]
pub struct UpdateExampleStatusRequest {
    pub example_status: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct FeedbackRequest {
    pub workflow_id: String,
    #[serde(default)]
    pub workflow_name: Option<String>,
    pub feedback_type: String,
    #[serde(default)]
    pub edited_field: Option<String>,
    #[serde(default)]
    pub old_value: Option<String>,
    #[serde(default)]
    pub new_value: Option<String>,
    #[serde(default)]
    pub rating: Option<i32>,
}

#[derive(Debug, Deserialize)]
pub struct CleanupRequest {
    #[serde(default = "default_cleanup_days")]
    pub older_than_days: u32,
}

fn default_cleanup_days() -> u32 {
    30
}

#[derive(Debug, Deserialize)]
pub struct ImportBenchmarksRequest {
    pub benchmarks: Vec<ImportBenchmarkEntry>,
}

#[derive(Debug, Deserialize)]
pub struct ImportBenchmarkEntry {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    pub expected_structure: crate::workflow_generation::benchmark::ExpectedStructure,
    #[serde(default)]
    pub enabled: Option<bool>,
}

// ============================================================================
// Dashboard Endpoints
// ============================================================================

async fn dashboard_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let db = state.app_state.checkpoint_db.clone();

    let metrics = tokio::task::spawn_blocking(move || db.get_generation_dashboard_metrics())
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(e.to_string())),
            )
        })?
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))?;

    Ok(Json(ApiResponse::success(
        serde_json::to_value(metrics).unwrap_or_default(),
    )))
}

async fn trends_handler(
    State(state): State<Arc<ApiState>>,
    Query(params): Query<TrendsParams>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let db = state.app_state.checkpoint_db.clone();

    let trends =
        tokio::task::spawn_blocking(move || db.get_generation_metrics_over_time(params.days))
            .await
            .map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(api_error(e.to_string())),
                )
            })?
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))?;

    Ok(Json(ApiResponse::success(
        serde_json::to_value(trends).unwrap_or_default(),
    )))
}

// ============================================================================
// Pipeline Inspector Endpoints
// ============================================================================

async fn list_artifacts_handler(
    State(state): State<Arc<ApiState>>,
    Query(params): Query<PaginationParams>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let db = state.app_state.checkpoint_db.clone();

    let artifacts = tokio::task::spawn_blocking(move || {
        db.list_pipeline_artifacts(params.limit, params.offset)
    })
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(e.to_string())),
        )
    })?
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))?;

    Ok(Json(ApiResponse::success(
        serde_json::to_value(artifacts).unwrap_or_default(),
    )))
}

async fn get_artifact_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let db = state.app_state.checkpoint_db.clone();

    let artifact = tokio::task::spawn_blocking(move || db.get_pipeline_artifact(&id))
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(e.to_string())),
            )
        })?
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))?;

    match artifact {
        Some(a) => Ok(Json(ApiResponse::success(
            serde_json::to_value(a).unwrap_or_default(),
        ))),
        None => Err((StatusCode::NOT_FOUND, Json(api_error("Artifact not found")))),
    }
}

async fn get_artifact_by_workflow_handler(
    State(state): State<Arc<ApiState>>,
    Path(workflow_id): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let db = state.app_state.checkpoint_db.clone();

    let artifact =
        tokio::task::spawn_blocking(move || db.get_pipeline_artifact_for_workflow(&workflow_id))
            .await
            .map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(api_error(e.to_string())),
                )
            })?
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))?;

    match artifact {
        Some(a) => Ok(Json(ApiResponse::success(
            serde_json::to_value(a).unwrap_or_default(),
        ))),
        None => Err((
            StatusCode::NOT_FOUND,
            Json(api_error("No artifact found for this workflow")),
        )),
    }
}

// ============================================================================
// Edit Analysis Endpoints
// ============================================================================

async fn edit_analysis_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let db = state.app_state.checkpoint_db.clone();

    let analysis = tokio::task::spawn_blocking(move || db.get_edit_analysis())
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(e.to_string())),
            )
        })?
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))?;

    Ok(Json(ApiResponse::success(
        serde_json::to_value(analysis).unwrap_or_default(),
    )))
}

// ============================================================================
// Benchmark Endpoints
// ============================================================================

async fn list_benchmarks_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let db = state.app_state.checkpoint_db.clone();

    let benchmarks = tokio::task::spawn_blocking(move || db.list_benchmarks())
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(e.to_string())),
            )
        })?
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))?;

    Ok(Json(ApiResponse::success(
        serde_json::to_value(benchmarks).unwrap_or_default(),
    )))
}

async fn create_benchmark_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<CreateBenchmarkRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let db = state.app_state.checkpoint_db.clone();
    let now = Utc::now().to_rfc3339();

    let benchmark = GeneratorBenchmark {
        id: Uuid::new_v4().to_string(),
        name: body.name,
        description: body.description,
        category: body.category,
        tags: body.tags,
        expected_structure: body.expected_structure,
        created_at: now.clone(),
        updated_at: now,
        enabled: true,
    };

    let bm_clone = benchmark.clone();
    tokio::task::spawn_blocking(move || db.save_benchmark(&bm_clone))
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(e.to_string())),
            )
        })?
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))?;

    info!("Created benchmark: {} ({})", benchmark.name, benchmark.id);

    Ok(Json(ApiResponse::success(
        serde_json::to_value(benchmark).unwrap_or_default(),
    )))
}

async fn get_benchmark_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let db = state.app_state.checkpoint_db.clone();

    let benchmark = tokio::task::spawn_blocking(move || db.get_benchmark(&id))
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(e.to_string())),
            )
        })?
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))?;

    match benchmark {
        Some(b) => Ok(Json(ApiResponse::success(
            serde_json::to_value(b).unwrap_or_default(),
        ))),
        None => Err((
            StatusCode::NOT_FOUND,
            Json(api_error("Benchmark not found")),
        )),
    }
}

async fn update_benchmark_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Json(body): Json<BenchmarkUpdate>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let db = state.app_state.checkpoint_db.clone();

    tokio::task::spawn_blocking(move || db.update_benchmark(&id, &body))
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(e.to_string())),
            )
        })?
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))?;

    Ok(Json(ApiResponse::success(serde_json::json!({
        "updated": true
    }))))
}

async fn delete_benchmark_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let db = state.app_state.checkpoint_db.clone();

    tokio::task::spawn_blocking(move || db.delete_benchmark(&id))
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(e.to_string())),
            )
        })?
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))?;

    Ok(Json(ApiResponse::success(serde_json::json!({
        "deleted": true
    }))))
}

async fn run_benchmark_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let db = state.app_state.checkpoint_db.clone();
    let pg_db = state.app_state.pg_db.clone();
    let pg_clone = pg_db.clone();
    let doctor_handle = state.doctor_handle.clone();

    let result = tokio::task::spawn_blocking(move || {
        // Load benchmark
        let benchmark = db.get_benchmark(&id)?;
        let benchmark = benchmark.ok_or_else(|| "Benchmark not found".to_string())?;

        // Run generation with the benchmark description
        let request = crate::workflow_generation::GenerateWorkflowRequest {
            description: benchmark.description.clone(),
            category: benchmark.category.clone(),
            tags: benchmark.tags.clone(),
            max_fix_iterations: Some(3),
            ..Default::default()
        };

        let start = std::time::Instant::now();
        let (gen_response, artifact) = crate::workflow_generation::generate_workflow(
            request,
            doctor_handle.as_ref(),
            Some(&pg_clone),
            None,
        );
        let duration_ms = start.elapsed().as_millis() as u64;

        // Score the result
        let (score, passed) = if let Some(ref workflow) = gen_response.workflow {
            let score = crate::workflow_generation::benchmark::score_workflow(
                workflow,
                &benchmark.expected_structure,
            );
            let passed = score.overall_score >= 0.7;
            (Some(score), passed)
        } else {
            (None, false)
        };

        let result = GeneratorBenchmarkResult {
            id: Uuid::new_v4().to_string(),
            benchmark_id: benchmark.id.clone(),
            artifact_id: Some(artifact.id.clone()),
            run_at: Utc::now().to_rfc3339(),
            model_used: gen_response.model_used.clone(),
            structure_score: score.as_ref().map(|s| s.structure_score),
            content_score: score.as_ref().map(|s| s.content_score),
            step_type_score: score.as_ref().map(|s| s.step_type_score),
            overall_score: score.as_ref().map(|s| s.overall_score),
            score_breakdown: score
                .as_ref()
                .and_then(|s| serde_json::to_value(&s.breakdown).ok()),
            generated_json: gen_response
                .workflow
                .as_ref()
                .and_then(|w| serde_json::to_value(w).ok()),
            duration_ms: Some(duration_ms),
            passed,
            notes: if gen_response.success {
                None
            } else {
                gen_response.error.clone()
            },
        };

        db.save_benchmark_result(&result)?;

        Ok::<_, String>((result, artifact))
    })
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(e.to_string())),
        )
    })?
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))?;

    let (result, artifact) = result;

    // Save pipeline artifact to PG (async, non-blocking)
    if let Err(e) = pg_db.save_generation_artifact(&artifact).await {
        warn!("Failed to save benchmark pipeline artifact to PG: {}", e);
    }

    Ok(Json(ApiResponse::success(
        serde_json::to_value(result).unwrap_or_default(),
    )))
}

async fn list_benchmark_results_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Query(params): Query<BenchmarkResultsParams>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let db = state.app_state.checkpoint_db.clone();

    let results = tokio::task::spawn_blocking(move || db.list_benchmark_results(&id, params.limit))
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(e.to_string())),
            )
        })?
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))?;

    Ok(Json(ApiResponse::success(
        serde_json::to_value(results).unwrap_or_default(),
    )))
}

// ============================================================================
// Feedback Endpoint
// ============================================================================

async fn save_feedback_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<FeedbackRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let db = state.app_state.checkpoint_db.clone();
    let id = Uuid::new_v4().to_string();
    let now = Utc::now().to_rfc3339();

    let feedback_id = id.clone();
    let workflow_id = body.workflow_id.clone();
    let workflow_name = body.workflow_name.clone();
    let feedback_type = body.feedback_type.clone();
    let edited_field = body.edited_field.clone();
    let old_value = body.old_value.clone();
    let new_value = body.new_value.clone();
    let rating = body.rating;

    tokio::task::spawn_blocking(move || {
        db.save_generator_feedback(
            &feedback_id,
            &workflow_id,
            workflow_name.as_deref(),
            &feedback_type,
            edited_field.as_deref(),
            old_value.as_deref(),
            new_value.as_deref(),
            rating,
            &now,
        )
    })
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(e.to_string())),
        )
    })?
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))?;

    info!(
        "Saved generator feedback: {} (type={}, workflow={})",
        id, body.feedback_type, body.workflow_id
    );

    Ok(Json(ApiResponse::success(serde_json::json!({
        "id": id,
        "saved": true
    }))))
}

// ============================================================================
// Artifact Cleanup Endpoint
// ============================================================================

async fn cleanup_artifacts_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<CleanupRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let db = state.app_state.checkpoint_db.clone();
    let days = body.older_than_days;

    let deleted =
        tokio::task::spawn_blocking(move || db.delete_pipeline_artifacts_older_than(days))
            .await
            .map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(api_error(e.to_string())),
                )
            })?
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))?;

    info!(
        "Cleaned up {} pipeline artifacts older than {} days",
        deleted, days
    );

    Ok(Json(ApiResponse::success(serde_json::json!({
        "deleted_count": deleted,
        "older_than_days": days
    }))))
}

// ============================================================================
// Benchmark Import/Export Endpoints
// ============================================================================

async fn export_benchmarks_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let db = state.app_state.checkpoint_db.clone();

    let benchmarks = tokio::task::spawn_blocking(move || db.list_benchmarks())
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(e.to_string())),
            )
        })?
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))?;

    Ok(Json(ApiResponse::success(
        serde_json::to_value(benchmarks).unwrap_or_default(),
    )))
}

async fn import_benchmarks_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<ImportBenchmarksRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let db = state.app_state.checkpoint_db.clone();

    let result = tokio::task::spawn_blocking(move || {
        let mut imported = 0u32;
        let mut skipped = 0u32;

        for entry in &body.benchmarks {
            // Skip duplicates by name
            match db.benchmark_exists_by_name(&entry.name) {
                Ok(true) => {
                    skipped += 1;
                    continue;
                }
                Ok(false) => {}
                Err(e) => return Err(e),
            }

            let now = Utc::now().to_rfc3339();
            let benchmark = GeneratorBenchmark {
                id: Uuid::new_v4().to_string(),
                name: entry.name.clone(),
                description: entry.description.clone(),
                category: entry.category.clone(),
                tags: entry.tags.clone(),
                expected_structure: entry.expected_structure.clone(),
                created_at: now.clone(),
                updated_at: now,
                enabled: entry.enabled.unwrap_or(true),
            };

            db.save_benchmark(&benchmark)?;
            imported += 1;
        }

        Ok::<_, String>((imported, skipped))
    })
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(e.to_string())),
        )
    })?
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))?;

    let (imported, skipped) = result;
    info!(
        "Imported {} benchmarks ({} skipped as duplicates)",
        imported, skipped
    );

    Ok(Json(ApiResponse::success(serde_json::json!({
        "imported": imported,
        "skipped": skipped
    }))))
}

// ============================================================================
// Example Library Endpoints
// ============================================================================

async fn list_examples_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let db = state.app_state.checkpoint_db.clone();

    let examples = tokio::task::spawn_blocking(move || db.list_example_workflows())
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(e.to_string())),
            )
        })?
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))?;

    Ok(Json(ApiResponse::success(
        serde_json::to_value(examples).unwrap_or_default(),
    )))
}

async fn update_example_status_handler(
    State(state): State<Arc<ApiState>>,
    Path(workflow_id): Path<String>,
    Json(body): Json<UpdateExampleStatusRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let db = state.app_state.checkpoint_db.clone();
    let status = body.example_status.clone();

    tokio::task::spawn_blocking(move || db.update_example_status(&workflow_id, status.as_deref()))
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(e.to_string())),
            )
        })?
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))?;

    Ok(Json(ApiResponse::success(serde_json::json!({
        "updated": true
    }))))
}

// ============================================================================
// Routes
// ============================================================================

pub fn routes() -> Router<Arc<ApiState>> {
    Router::new()
        // Dashboard
        .route("/generator-eval/dashboard", get(dashboard_handler))
        .route("/generator-eval/trends", get(trends_handler))
        // Pipeline Inspector
        .route("/generator-eval/artifacts", get(list_artifacts_handler))
        // Artifact Cleanup (static path before {id} param)
        .route(
            "/generator-eval/artifacts/cleanup",
            delete(cleanup_artifacts_handler),
        )
        .route(
            "/generator-eval/artifacts/by-workflow/{workflow_id}",
            get(get_artifact_by_workflow_handler),
        )
        .route("/generator-eval/artifacts/{id}", get(get_artifact_handler))
        // Edit Analysis + Feedback
        .route("/generator-eval/edit-analysis", get(edit_analysis_handler))
        .route("/generator-eval/feedback", post(save_feedback_handler))
        // Benchmarks
        .route(
            "/generator-eval/benchmarks",
            get(list_benchmarks_handler).post(create_benchmark_handler),
        )
        // Benchmark Import/Export (static paths before {id} param)
        .route(
            "/generator-eval/benchmarks/export",
            get(export_benchmarks_handler),
        )
        .route(
            "/generator-eval/benchmarks/import",
            post(import_benchmarks_handler),
        )
        .route(
            "/generator-eval/benchmarks/{id}",
            get(get_benchmark_handler)
                .put(update_benchmark_handler)
                .delete(delete_benchmark_handler),
        )
        .route(
            "/generator-eval/benchmarks/{id}/run",
            post(run_benchmark_handler),
        )
        .route(
            "/generator-eval/benchmarks/{id}/results",
            get(list_benchmark_results_handler),
        )
        // Example Library
        .route("/generator-eval/examples", get(list_examples_handler))
        .route(
            "/generator-eval/examples/{workflow_id}",
            put(update_example_status_handler),
        )
        // Training Data Export
        .route("/generator-eval/training-data", get(training_data_handler))
        // Prompt Analysis Insights
        .route("/generator-eval/insights", get(prompt_insights_handler))
}

// ============================================================================
// Training Data Export
// ============================================================================

#[derive(Deserialize)]
struct TrainingDataQuery {
    agent: String,
    #[serde(default = "default_min_score")]
    min_score: f64,
    #[serde(default = "default_training_limit")]
    limit: u32,
}

fn default_min_score() -> f64 {
    0.0
}
fn default_training_limit() -> u32 {
    1000
}

async fn training_data_handler(
    State(state): State<Arc<ApiState>>,
    Query(params): Query<TrainingDataQuery>,
) -> Result<
    Json<ApiResponse<Vec<crate::workflow_generation::training_data::TrainingExample>>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    let db = state.app_state.checkpoint_db.clone();
    let agent = params.agent;
    let min_score = params.min_score;
    let limit = params.limit;

    let examples = tokio::task::spawn_blocking(move || {
        let conn = db.get_conn_string()?;
        crate::workflow_generation::training_data::export_training_data(
            &conn, &agent, min_score, limit,
        )
    })
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(e.to_string())),
        )
    })?
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))?;

    Ok(Json(ApiResponse::success(examples)))
}

// ============================================================================
// Prompt Analysis Insights
// ============================================================================

async fn prompt_insights_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<
    Json<ApiResponse<Vec<crate::workflow_generation::prompt_analysis::PromptInsight>>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    let db = state.app_state.checkpoint_db.clone();

    let insights = tokio::task::spawn_blocking(move || {
        let conn = db.get_conn_string()?;
        crate::workflow_generation::prompt_analysis::analyze_all(&conn)
    })
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(e.to_string())),
        )
    })?
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))?;

    Ok(Json(ApiResponse::success(insights)))
}

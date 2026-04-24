//! Analytics endpoints — selector performance, cross-run quality, health score.
//!
//! All handlers here read from `state.app_state.pg_db` and don't touch the
//! IPC transport. They're effectively typed wrappers over database queries
//! in `crate::database::ui_bridge_ops`.

use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::Json,
};
use serde::Deserialize;
use std::sync::Arc;

use crate::mcp::types::{api_error, ApiResponse, ApiState};

#[derive(Debug, Deserialize)]
pub struct AnalyticsDaysQuery {
    #[serde(default = "default_analytics_days")]
    pub days: u32,
    #[serde(default = "default_analytics_limit")]
    pub limit: i64,
}
fn default_analytics_days() -> u32 {
    7
}
fn default_analytics_limit() -> i64 {
    20
}

#[derive(Debug, Deserialize)]
pub struct DecayCurveQuery {
    pub element_id: String,
    #[serde(default = "default_window_ms")]
    pub window_ms: i64,
    #[serde(default = "default_num_windows")]
    pub windows: i64,
}
fn default_window_ms() -> i64 {
    86_400_000
} // 1 day
fn default_num_windows() -> i64 {
    7
}

fn days_to_epoch_ms(days: u32) -> i64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64;
    now - (days as i64 * 86_400_000)
}

/// GET /ui-bridge/analytics/decay-curve
pub async fn analytics_decay_curve_handler(
    State(state): State<Arc<ApiState>>,
    Query(q): Query<DecayCurveQuery>,
) -> Result<
    Json<ApiResponse<Vec<crate::database::ui_bridge_ops::DecayCurveBucket>>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    match state
        .app_state
        .pg_db
        .get_element_decay_curve(&q.element_id, q.window_ms, q.windows)
        .await
    {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// GET /ui-bridge/analytics/action-baselines
pub async fn analytics_action_baselines_handler(
    State(state): State<Arc<ApiState>>,
    Query(q): Query<AnalyticsDaysQuery>,
) -> Result<
    Json<ApiResponse<Vec<crate::database::ui_bridge_ops::ActionBaseline>>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    let since = days_to_epoch_ms(q.days);
    match state
        .app_state
        .pg_db
        .get_action_latency_baselines(since)
        .await
    {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// GET /ui-bridge/analytics/failure-taxonomy
pub async fn analytics_failure_taxonomy_handler(
    State(state): State<Arc<ApiState>>,
    Query(q): Query<AnalyticsDaysQuery>,
) -> Result<
    Json<ApiResponse<Vec<crate::database::ui_bridge_ops::FailureCluster>>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    let since = days_to_epoch_ms(q.days);
    match state
        .app_state
        .pg_db
        .get_failure_taxonomy(since, q.limit)
        .await
    {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// GET /ui-bridge/analytics/fragility-heatmap
pub async fn analytics_fragility_heatmap_handler(
    State(state): State<Arc<ApiState>>,
    Query(q): Query<AnalyticsDaysQuery>,
) -> Result<
    Json<ApiResponse<Vec<crate::database::ui_bridge_ops::ElementFragility>>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    let since = days_to_epoch_ms(q.days);
    match state
        .app_state
        .pg_db
        .get_element_fragility_by_region(since)
        .await
    {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// GET /ui-bridge/analytics/regressions
pub async fn analytics_regressions_handler(
    State(state): State<Arc<ApiState>>,
    Query(q): Query<AnalyticsDaysQuery>,
) -> Result<
    Json<ApiResponse<Vec<crate::database::ui_bridge_ops::AutomationRegression>>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    let since = days_to_epoch_ms(q.days);
    match state
        .app_state
        .pg_db
        .get_automation_regressions(since, q.limit)
        .await
    {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// GET /ui-bridge/analytics/stall-frequency
pub async fn analytics_stall_frequency_handler(
    State(state): State<Arc<ApiState>>,
    Query(q): Query<AnalyticsDaysQuery>,
) -> Result<
    Json<ApiResponse<Vec<crate::database::ui_bridge_ops::StallFrequency>>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    let since = (chrono::Utc::now() - chrono::Duration::days(q.days as i64)).to_rfc3339();
    match state.app_state.pg_db.get_stall_frequency(&since).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// GET /ui-bridge/analytics/intervention-effectiveness
pub async fn analytics_intervention_handler(
    State(state): State<Arc<ApiState>>,
    Query(q): Query<AnalyticsDaysQuery>,
) -> Result<
    Json<ApiResponse<Vec<crate::database::ui_bridge_ops::InterventionStats>>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    let since = (chrono::Utc::now() - chrono::Duration::days(q.days as i64)).to_rfc3339();
    match state
        .app_state
        .pg_db
        .get_intervention_effectiveness(&since)
        .await
    {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

#[derive(Debug, Deserialize)]
pub struct StateCoverageQuery {
    pub task_run_id: i64,
}

/// GET /ui-bridge/analytics/state-coverage
pub async fn analytics_state_coverage_handler(
    State(state): State<Arc<ApiState>>,
    Query(q): Query<StateCoverageQuery>,
) -> Result<Json<ApiResponse<Vec<String>>>, (StatusCode, Json<ApiResponse<()>>)> {
    match state
        .app_state
        .pg_db
        .get_state_coverage(q.task_run_id)
        .await
    {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

#[derive(Debug, Deserialize)]
pub struct AnnotationGapQuery {
    #[serde(default = "default_annotation_min")]
    pub min_interactions: i64,
}
fn default_annotation_min() -> i64 {
    10
}

/// GET /ui-bridge/analytics/annotation-gaps
pub async fn analytics_annotation_gaps_handler(
    State(state): State<Arc<ApiState>>,
    Query(q): Query<AnnotationGapQuery>,
) -> Result<
    Json<ApiResponse<Vec<crate::database::ui_bridge_ops::AnnotationGap>>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    match state
        .app_state
        .pg_db
        .get_unannotated_high_interaction_elements(q.min_interactions)
        .await
    {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// GET /ui-bridge/analytics/health-score
pub async fn analytics_health_score_handler(
    State(state): State<Arc<ApiState>>,
    Query(q): Query<AnalyticsDaysQuery>,
) -> Result<
    Json<ApiResponse<crate::database::ui_bridge_ops::AutomationHealthScore>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    let since = days_to_epoch_ms(q.days);
    let data = state
        .app_state
        .pg_db
        .compute_automation_health_score(since)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))?;
    // Deserialize from serde_json::Value to the expected type
    let typed: crate::database::ui_bridge_ops::AutomationHealthScore = serde_json::from_value(data)
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Deserialization error: {}", e))),
            )
        })?;
    Ok(Json(ApiResponse::success(typed)))
}

/// GET /ui-bridge/analytics/recommendations
pub async fn analytics_recommendations_handler(
    State(state): State<Arc<ApiState>>,
    Query(q): Query<AnalyticsDaysQuery>,
) -> Result<
    Json<ApiResponse<Vec<crate::database::ui_bridge_ops::Recommendation>>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    let since = days_to_epoch_ms(q.days);
    let data = state
        .app_state
        .pg_db
        .generate_recommendations(since)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))?;
    // Deserialize from serde_json::Value to the expected type
    let typed: Vec<crate::database::ui_bridge_ops::Recommendation> = data
        .into_iter()
        .filter_map(|v| serde_json::from_value(v).ok())
        .collect();
    Ok(Json(ApiResponse::success(typed)))
}

pub fn routes() -> axum::Router<Arc<ApiState>> {
    use axum::routing::get;
    axum::Router::new()
        .route(
            "/ui-bridge/analytics/decay-curve",
            get(analytics_decay_curve_handler),
        )
        .route(
            "/ui-bridge/analytics/action-baselines",
            get(analytics_action_baselines_handler),
        )
        .route(
            "/ui-bridge/analytics/failure-taxonomy",
            get(analytics_failure_taxonomy_handler),
        )
        .route(
            "/ui-bridge/analytics/fragility-heatmap",
            get(analytics_fragility_heatmap_handler),
        )
        .route(
            "/ui-bridge/analytics/regressions",
            get(analytics_regressions_handler),
        )
        .route(
            "/ui-bridge/analytics/stall-frequency",
            get(analytics_stall_frequency_handler),
        )
        .route(
            "/ui-bridge/analytics/intervention-effectiveness",
            get(analytics_intervention_handler),
        )
        .route(
            "/ui-bridge/analytics/state-coverage",
            get(analytics_state_coverage_handler),
        )
        .route(
            "/ui-bridge/analytics/annotation-gaps",
            get(analytics_annotation_gaps_handler),
        )
        .route(
            "/ui-bridge/analytics/health-score",
            get(analytics_health_score_handler),
        )
        .route(
            "/ui-bridge/analytics/recommendations",
            get(analytics_recommendations_handler),
        )
}

pub fn route_entries() -> &'static [(&'static str, &'static str)] {
    &[
        ("GET", "/ui-bridge/analytics/decay-curve"),
        ("GET", "/ui-bridge/analytics/action-baselines"),
        ("GET", "/ui-bridge/analytics/failure-taxonomy"),
        ("GET", "/ui-bridge/analytics/fragility-heatmap"),
        ("GET", "/ui-bridge/analytics/regressions"),
        ("GET", "/ui-bridge/analytics/stall-frequency"),
        ("GET", "/ui-bridge/analytics/intervention-effectiveness"),
        ("GET", "/ui-bridge/analytics/state-coverage"),
        ("GET", "/ui-bridge/analytics/annotation-gaps"),
        ("GET", "/ui-bridge/analytics/health-score"),
        ("GET", "/ui-bridge/analytics/recommendations"),
    ]
}

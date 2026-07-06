//! HTTP handlers for the multi-tenant app registry — spec-multi-app Stream B.
//!
//! Five routes mounted at `/apps` + `/apps/:app_id` (see `spec_api/mod.rs`):
//!
//! | Method | Path              | Handler              |
//! |--------|-------------------|----------------------|
//! | GET    | `/apps`           | `handle_list_apps`   |
//! | POST   | `/apps`           | `handle_register_app`|
//! | GET    | `/apps/:app_id`   | `handle_get_app`     |
//! | PATCH  | `/apps/:app_id`   | `handle_update_app`  |
//! | DELETE | `/apps/:app_id`   | `handle_delete_app`  |
//!
//! Storage primitive calls go through `PgDb::{list_apps, get_app, insert_app,
//! update_app, delete_app}` (see `database/pg/apps.rs`). Validation lives on
//! the schema side — `qontinui_types::apps::validate_app_id` is invoked from
//! `insert_app`; this module just wires HTTP → DB → JSON.
//!
//! Error responses follow the `spec_api/responses.rs::SpecError` shape:
//! `{ ok: false, reason: <kebab-case>, detail: <message> }`. The
//! `IntoResponse for AppError` impl below maps each `AppError` variant to a
//! status code per PLAN.md §C.3 (Stream C will share the impl via
//! `spec_api/handlers.rs` — for now it lives here).

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;
use tracing::instrument;

use qontinui_types::apps::{AppError, AppListResponse, RegisterAppRequest, UpdateAppRequest};

use super::responses::SpecError;
use crate::mcp::types::ApiState;

// ---------------------------------------------------------------------------
// AppError → HTTP response
// ---------------------------------------------------------------------------

/// Map a schema-side `AppError` onto the `SpecError` envelope + a status code.
/// Status mapping per PLAN.md §C.3 of spec-multi-app:
/// - `NotRegistered` → 404
/// - `InvalidAppId` → 400
/// - `InvalidRepoRoot` → 500
/// - `AlreadyRegistered` → 409 (only reachable from `POST /apps`)
pub fn app_error_into_response(err: AppError) -> Response {
    let (status, reason, detail) = match &err {
        AppError::NotRegistered { app_id } => (
            StatusCode::NOT_FOUND,
            "app-not-found",
            json!({ "appId": app_id }),
        ),
        AppError::InvalidAppId { app_id } => (
            StatusCode::BAD_REQUEST,
            "invalid-app-id",
            json!({ "appId": app_id }),
        ),
        AppError::InvalidRepoRoot { repo_root } => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "invalid-repo-root",
            json!({ "repoRoot": repo_root }),
        ),
        AppError::AlreadyRegistered { app_id } => (
            StatusCode::CONFLICT,
            "already-registered",
            json!({ "appId": app_id }),
        ),
        AppError::InvalidUpdateStrategy { update_strategy } => (
            StatusCode::BAD_REQUEST,
            "invalid-update-strategy",
            json!({ "updateStrategy": update_strategy }),
        ),
    };
    (status, Json(SpecError::with_detail(reason, detail))).into_response()
}

/// Extension trait so consuming modules can write
/// `err.into_app_response()` once they `use AppErrorExt;`. We cannot
/// `impl IntoResponse for AppError` directly because `AppError` is defined
/// in `qontinui-types` (orphan rules forbid). The method is named
/// `into_app_response` to avoid colliding with `IntoResponse::into_response`.
pub trait AppErrorExt {
    fn into_app_response(self) -> Response;
}

impl AppErrorExt for AppError {
    fn into_app_response(self) -> Response {
        app_error_into_response(self)
    }
}

// ---------------------------------------------------------------------------
// GET /apps
// ---------------------------------------------------------------------------

#[instrument(skip(state))]
pub async fn handle_list_apps(State(state): State<Arc<ApiState>>) -> Response {
    let pg = state.app_state.pg_db.clone();
    match pg.list_apps().await {
        Ok(apps) => Json(AppListResponse { ok: true, apps }).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(SpecError::with_detail(
                "list-apps-failed",
                json!({ "error": e }),
            )),
        )
            .into_response(),
    }
}

// ---------------------------------------------------------------------------
// POST /apps
// ---------------------------------------------------------------------------

#[instrument(skip(state, body), fields(app_id = %body.app_id))]
pub async fn handle_register_app(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<RegisterAppRequest>,
) -> Response {
    let pg = state.app_state.pg_db.clone();
    match pg.insert_app(&body).await {
        Ok(app) => (StatusCode::CREATED, Json(app)).into_response(),
        Err(e) => app_error_into_response(e),
    }
}

// ---------------------------------------------------------------------------
// GET /apps/:app_id
// ---------------------------------------------------------------------------

#[instrument(skip(state), fields(app_id = %app_id))]
pub async fn handle_get_app(
    Path(app_id): Path<String>,
    State(state): State<Arc<ApiState>>,
) -> Response {
    let pg = state.app_state.pg_db.clone();
    match pg.get_app(&app_id).await {
        Ok(Some(app)) => Json(app).into_response(),
        Ok(None) => app_error_into_response(AppError::NotRegistered { app_id }),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(SpecError::with_detail(
                "get-app-failed",
                json!({ "appId": app_id, "error": e }),
            )),
        )
            .into_response(),
    }
}

// ---------------------------------------------------------------------------
// PATCH /apps/:app_id
// ---------------------------------------------------------------------------

#[instrument(skip(state, body), fields(app_id = %app_id))]
pub async fn handle_update_app(
    Path(app_id): Path<String>,
    State(state): State<Arc<ApiState>>,
    Json(body): Json<UpdateAppRequest>,
) -> Response {
    let pg = state.app_state.pg_db.clone();
    match pg.update_app(&app_id, &body).await {
        Ok(app) => Json(app).into_response(),
        Err(e) => app_error_into_response(e),
    }
}

// ---------------------------------------------------------------------------
// DELETE /apps/:app_id
// ---------------------------------------------------------------------------

#[instrument(skip(state), fields(app_id = %app_id))]
pub async fn handle_delete_app(
    Path(app_id): Path<String>,
    State(state): State<Arc<ApiState>>,
) -> Response {
    let pg = state.app_state.pg_db.clone();
    match pg.delete_app(&app_id).await {
        Ok(true) => Json(json!({ "ok": true })).into_response(),
        Ok(false) => app_error_into_response(AppError::NotRegistered { app_id }),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(SpecError::with_detail(
                "delete-app-failed",
                json!({ "appId": app_id, "error": e }),
            )),
        )
            .into_response(),
    }
}

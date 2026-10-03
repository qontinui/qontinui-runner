//! `/fanout` — the HTTP door to the fan-out dispatcher (plan
//! `2026-09-20-terminal-page-review-notes-become-prompts-and-prompt-matrix-fan-out`,
//! Phase 6). The dispatcher itself is `crate::fanout`.
//!
//! `POST /fanout` takes the member list EXACTLY as previewed — the server
//! never re-expands a matrix, so what the operator saw is what runs. Every
//! route is a credential door on the origin guard's roster: creating a run
//! spawns processes, and a run's members carry the operator's prompts.
//!
//! Wire shapes (camelCase; every response is the runner's `ApiResponse`
//! envelope, `{success, data | error}`):
//!
//! * `POST /fanout` ← `{tenantId?, templateSlug?, templateVersion?,
//!   maxConcurrent?, configDirPolicy, workingDir, members: [{title, prompt}]}`
//!   → `CapOutcome {run, fanoutBound, clampedFrom}`
//! * `GET /fanout` → `[RunView]`, newest first
//! * `GET /fanout/{id}` → `RunView`
//! * `POST /fanout/{id}/cancel` → `RunView` (queued members only)
//! * `PATCH /fanout/{id}` ← `{maxConcurrent}` → `CapOutcome`
//! * `POST /fanout/{id}/members/{index}/release` → `RunView`
//!
//! Until the dispatcher has loaded its ledger from PostgreSQL (the boot settle,
//! or while PG is unreachable) EVERY route answers `503` with `code:
//! "FANOUT_LEDGER_NOT_LOADED"` and the reason — never `200 []`, which would
//! read as "no runs" while the ledger may hold active ones. `POST /fanout` is
//! refused too: see `FanoutDispatcher::create` for the double-spawn it closes.
//! A member's `index` is its position in the posted list; an optional
//! `previewIndex` on each posted member is stored and echoed so the strip can
//! show the preview's own row number.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use tauri::Manager;
use tracing::warn;
use uuid::Uuid;

use crate::fanout::dispatcher::{CapOutcome, FanoutDispatcher, NewRun, OpError};
use crate::fanout::model::{build_members, ConfigDirPolicy, MemberInput, RunView};
use crate::mcp::types::{api_error, ApiResponse, ApiState};

/// The cap a create carries when the caller names none — the preview's default.
const DEFAULT_MAX_CONCURRENT: u32 = 3;

type Refusal = (StatusCode, Json<ApiResponse<()>>);
type Reply<T> = Result<Json<ApiResponse<T>>, Refusal>;

fn refuse(status: StatusCode, message: impl Into<String>) -> Refusal {
    let message = message.into();
    warn!(status = status.as_u16(), "HTTP /fanout: {message}");
    (status, Json(api_error(message)))
}

/// The machine-readable code on a refusal served because the ledger is not
/// loaded — the UI renders it as UNKNOWN, never as "no runs".
pub const LEDGER_NOT_LOADED_CODE: &str = "FANOUT_LEDGER_NOT_LOADED";

fn not_loaded(message: String) -> Refusal {
    warn!("HTTP /fanout: {message}");
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(ApiResponse::<()>::error_with_code(
            message,
            LEDGER_NOT_LOADED_CODE,
        )),
    )
}

fn op_refusal(e: OpError) -> Refusal {
    match e {
        OpError::NotLoaded(m) => not_loaded(m),
        OpError::NotFound(m) => refuse(StatusCode::NOT_FOUND, m),
        OpError::Conflict(m) => refuse(StatusCode::CONFLICT, m),
        OpError::Store(m) => refuse(
            StatusCode::SERVICE_UNAVAILABLE,
            format!("fan-out ledger write failed, nothing changed: {m}"),
        ),
    }
}

/// The managed dispatcher, or `None` when this runner never started one.
fn dispatcher(state: &ApiState) -> Option<Arc<FanoutDispatcher>> {
    state
        .app_handle
        .try_state::<Arc<FanoutDispatcher>>()
        .map(|s| s.inner().clone())
}

fn not_running() -> Refusal {
    refuse(
        StatusCode::SERVICE_UNAVAILABLE,
        "the fan-out dispatcher is not running on this runner",
    )
}

/// Parse a path run id; the refusal text on failure.
fn run_id(raw: &str) -> Result<Uuid, String> {
    Uuid::parse_str(raw).map_err(|e| format!("{raw} is not a run id: {e}"))
}

fn bad_request(m: String) -> Refusal {
    refuse(StatusCode::BAD_REQUEST, m)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateFanoutRequest {
    #[serde(default)]
    pub tenant_id: Option<String>,
    #[serde(default)]
    pub template_slug: Option<String>,
    #[serde(default)]
    pub template_version: Option<i32>,
    #[serde(default)]
    pub max_concurrent: Option<u32>,
    pub config_dir_policy: ConfigDirPolicy,
    pub working_dir: String,
    pub members: Vec<MemberInput>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PatchFanoutRequest {
    pub max_concurrent: u32,
}

/// Validate a create request into a [`NewRun`]. The tenant is admitted HERE,
/// once, and the run carries it to every member spawn.
fn new_run(req: CreateFanoutRequest) -> Result<NewRun, String> {
    let tenant_id = crate::commands::terminal::admit_spawn_tenant(req.tenant_id.as_deref())?;
    let working_dir = req.working_dir.trim().to_string();
    let path = std::path::Path::new(&working_dir);
    if !path.is_absolute() {
        return Err(format!(
            "workingDir: {working_dir:?} is not an absolute path"
        ));
    }
    if !path.is_dir() {
        return Err(format!("workingDir: {working_dir} is not a directory"));
    }
    if let ConfigDirPolicy::Fixed { config_dir } = &req.config_dir_policy {
        if !std::path::Path::new(config_dir).is_dir() {
            return Err(format!(
                "configDirPolicy.configDir: {config_dir} is not a directory"
            ));
        }
    }
    Ok(NewRun {
        tenant_id,
        template_slug: req.template_slug,
        template_version: req.template_version,
        requested_max_concurrent: req.max_concurrent.unwrap_or(DEFAULT_MAX_CONCURRENT),
        config_dir_policy: req.config_dir_policy,
        working_dir,
        members: build_members(&req.members)?,
    })
}

async fn create_handler(
    State(state): State<Arc<ApiState>>,
    Json(req): Json<CreateFanoutRequest>,
) -> Reply<CapOutcome> {
    let d = dispatcher(&state).ok_or_else(not_running)?;
    let new = new_run(req).map_err(|m| refuse(StatusCode::BAD_REQUEST, m))?;
    let out = d.create(new).await.map_err(op_refusal)?;
    Ok(Json(ApiResponse::success(out)))
}

async fn list_handler(State(state): State<Arc<ApiState>>) -> Reply<Vec<RunView>> {
    let d = dispatcher(&state).ok_or_else(not_running)?;
    let runs = d.list().map_err(not_loaded)?;
    Ok(Json(ApiResponse::success(runs.as_ref().clone())))
}

async fn get_handler(State(state): State<Arc<ApiState>>, Path(id): Path<String>) -> Reply<RunView> {
    let d = dispatcher(&state).ok_or_else(not_running)?;
    let id = run_id(&id).map_err(bad_request)?;
    d.get(id)
        .map_err(not_loaded)?
        .map(|v| Json(ApiResponse::success(v)))
        .ok_or_else(|| refuse(StatusCode::NOT_FOUND, format!("no fan-out run {id}")))
}

async fn cancel_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Reply<RunView> {
    let d = dispatcher(&state).ok_or_else(not_running)?;
    let id = run_id(&id).map_err(bad_request)?;
    let v = d.cancel(id).await.map_err(op_refusal)?;
    Ok(Json(ApiResponse::success(v)))
}

async fn patch_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Json(req): Json<PatchFanoutRequest>,
) -> Reply<CapOutcome> {
    let d = dispatcher(&state).ok_or_else(not_running)?;
    let id = run_id(&id).map_err(bad_request)?;
    let out = d
        .set_max_concurrent(id, req.max_concurrent)
        .await
        .map_err(op_refusal)?;
    Ok(Json(ApiResponse::success(out)))
}

async fn release_handler(
    State(state): State<Arc<ApiState>>,
    Path((id, index)): Path<(String, u32)>,
) -> Reply<RunView> {
    let d = dispatcher(&state).ok_or_else(not_running)?;
    let id = run_id(&id).map_err(bad_request)?;
    let v = d.release(id, index).await.map_err(op_refusal)?;
    Ok(Json(ApiResponse::success(v)))
}

pub fn routes() -> Router<Arc<ApiState>> {
    Router::new()
        .without_v07_checks()
        .route("/fanout", get(list_handler).post(create_handler))
        .route("/fanout/{id}", get(get_handler).patch(patch_handler))
        .route("/fanout/{id}/cancel", post(cancel_handler))
        .route(
            "/fanout/{id}/members/{index}/release",
            post(release_handler),
        )
}

/// Every `(METHOD, path)` [`routes`] registers. Pinned against the source by
/// the test below, and against the origin guard's door roster.
pub fn route_entries() -> &'static [(&'static str, &'static str)] {
    &[
        ("GET", "/fanout"),
        ("POST", "/fanout"),
        ("GET", "/fanout/{id}"),
        ("PATCH", "/fanout/{id}"),
        ("POST", "/fanout/{id}/cancel"),
        ("POST", "/fanout/{id}/members/{index}/release"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drift guard: every `.route(` path in [`routes`] has [`route_entries`]
    /// rows, and every entry names a registered path.
    #[test]
    fn fanout_route_entries_match_the_registered_routes() {
        let source = include_str!("fanout.rs");
        // Only the `routes()` body, and tolerant of rustfmt wrapping a long
        // `.route(` call so its path lands on the next line.
        let body = source
            .split("pub fn routes()")
            .nth(1)
            .and_then(|rest| rest.split("pub fn route_entries()").next())
            .expect("routes() is in this file");
        let registered: std::collections::BTreeSet<String> = body
            .split(".route(")
            .skip(1)
            .filter_map(|rest| rest.trim_start().strip_prefix('"'))
            .filter_map(|rest| rest.split('"').next())
            .map(str::to_string)
            .collect();
        let declared: std::collections::BTreeSet<String> = route_entries()
            .iter()
            .map(|(_, path)| (*path).to_string())
            .collect();
        assert_eq!(
            registered, declared,
            "routes() and route_entries() drifted apart"
        );
        let scanned = crate::mcp::relay_path_policy::tests::registered_routes();
        for (method, path) in route_entries() {
            assert!(
                scanned.contains(&(method.to_string(), path.to_string())),
                "{method} {path} is declared but not registered"
            );
        }
    }

    /// Creating a run spawns processes and every run carries the operator's
    /// prompts, so no browser origin but the runner's own may reach any route.
    #[test]
    fn every_fanout_route_is_a_credential_door() {
        for (method, path) in route_entries() {
            assert!(
                crate::mcp::origin_guard::is_credential_door(method, path),
                "{method} {path} is not on the origin guard's door roster"
            );
        }
    }

    fn temp_dir_json() -> String {
        serde_json::to_string(&std::env::temp_dir().to_string_lossy()).unwrap()
    }

    #[test]
    fn fanout_create_request_wire_shape() {
        let req: CreateFanoutRequest = serde_json::from_str(&format!(
            r#"{{"tenantId":null,"templateSlug":"seed","templateVersion":3,"maxConcurrent":2,
                "configDirPolicy":{{"kind":"bestHeadroom"}},"workingDir":{},
                "members":[{{"title":"a","prompt":"p","previewIndex":4}}]}}"#,
            temp_dir_json()
        ))
        .unwrap();
        assert_eq!(req.max_concurrent, Some(2));
        assert_eq!(req.members.len(), 1);
        let run = new_run(req).unwrap();
        assert_eq!(run.requested_max_concurrent, 2);
        assert_eq!(run.tenant_id, None);
        assert_eq!(
            (run.members[0].index, run.members[0].preview_index),
            (0, Some(4))
        );
    }

    #[test]
    fn fanout_not_loaded_is_a_typed_503() {
        let (status, Json(body)) = op_refusal(OpError::NotLoaded("not loaded yet".to_string()));
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        let wire = serde_json::to_value(&body).unwrap();
        assert_eq!(wire["success"], false);
        assert_eq!(wire["code"], LEDGER_NOT_LOADED_CODE);
        assert_eq!(wire["error"], "not loaded yet");
        assert!(wire.get("data").is_none(), "never an empty list: {wire}");
    }

    #[test]
    fn fanout_create_refuses_a_bad_tenant_and_a_relative_working_dir() {
        let base =
            r#""configDirPolicy":{"kind":"bestHeadroom"},"members":[{"title":"a","prompt":"p"}]"#;
        let req: CreateFanoutRequest =
            serde_json::from_str(&format!(r#"{{"workingDir":"rel/dir",{base}}}"#)).unwrap();
        assert!(new_run(req).unwrap_err().contains("not an absolute path"));
        let req: CreateFanoutRequest = serde_json::from_str(&format!(
            r#"{{"tenantId":"not-a-uuid","workingDir":{},{base}}}"#,
            temp_dir_json()
        ))
        .unwrap();
        assert!(new_run(req)
            .unwrap_err()
            .starts_with("terminal:tenant_invalid:"));
    }
}

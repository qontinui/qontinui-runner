//! Axum router for the `/wrappers/...` HTTP surface.
//!
//! Wires every Phase 1.2-1.6 route in one place so `mcp_api::create_router`
//! only needs to call `wrappers::routes::router()` like every other
//! sub-module.
//!
//! Route surface (full prefixes):
//!
//! | Method | Path                                            | Handler            |
//! | ------ | ----------------------------------------------- | ------------------ |
//! | GET    | `/wrappers`                                     | list_wrappers      |
//! | GET    | `/wrappers/:id`                                 | get_wrapper        |
//! | POST   | `/wrappers/install`                             | install_wrapper    |
//! | POST   | `/wrappers/:id/update`                          | update_wrapper     |
//! | DELETE | `/wrappers/:id`                                 | uninstall_wrapper  |
//! | POST   | `/wrappers/:id/start`                           | start_wrapper      |
//! | POST   | `/wrappers/:id/stop`                            | stop_wrapper       |
//! | GET    | `/wrappers/:id/status`                          | wrapper_status     |
//! | POST   | `/wrappers/:id/dispatch`                        | dispatch_handler   |
//! | GET    | `/wrappers/:id/credentials`                     | list_credentials   |
//! | PUT    | `/wrappers/:id/credentials/:name`               | set_credential     |
//! | DELETE | `/wrappers/:id/credentials/:name`               | delete_credential  |

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::{
    extract::{Path as AxumPath, State},
    http::StatusCode,
    response::{
        sse::{Event, KeepAlive, Sse},
        Json,
    },
    routing::{get, post, put},
    Router,
};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use tokio_stream::wrappers::BroadcastStream;

use super::clients::{self, AiClientId, ClientError, ClientInfo};
use super::credentials::CredentialStore;
use super::dispatch::dispatch_handler;
use super::events;
use super::install::{install, uninstall, update, InstallError, InstallRequest, InstallResponse};
use super::manager::WrapperStatus;
use super::primary_proxy::{self, into_http as proxy_error_to_http};
use super::registry::Wrapper;
use super::WrapperState;
use crate::mcp::types::{api_error, runner_api_port, ApiResponse, ApiState};

/// Pull the wrapper subsystem off `ApiState` or 503 if not yet
/// initialized. Centralized so every handler returns the same envelope.
fn require_wrapper_state(
    state: &Arc<ApiState>,
) -> Result<Arc<WrapperState>, (StatusCode, Json<ApiResponse<()>>)> {
    state.app_state.wrapper_state.get().cloned().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(api_error("wrappers subsystem is not initialized")),
        )
    })
}

pub fn router() -> Router<Arc<ApiState>> {
    Router::new()
        // Registry
        .route("/wrappers", get(list_wrappers))
        // GET + DELETE share `/wrappers/{id}` — chain on a single MethodRouter
        // to satisfy axum 0.8's "one MethodRouter per path" rule.
        .route("/wrappers/{id}", get(get_wrapper).delete(uninstall_wrapper))
        // Install pipeline
        .route("/wrappers/install", post(install_wrapper))
        .route("/wrappers/{id}/update", post(update_wrapper))
        // Lifecycle
        .route("/wrappers/{id}/start", post(start_wrapper))
        .route("/wrappers/{id}/stop", post(stop_wrapper))
        .route("/wrappers/{id}/status", get(wrapper_status))
        // Dispatch
        .route("/wrappers/{id}/dispatch", post(dispatch_handler))
        // Credentials
        .route("/wrappers/{id}/credentials", get(list_credentials))
        .route(
            "/wrappers/{id}/credentials/{name}",
            put(set_credential).delete(delete_credential),
        )
        // AI client connect/disconnect
        .route("/wrappers/clients", get(list_clients))
        .route("/wrappers/clients/{id}/connect", post(connect_client))
        .route("/wrappers/clients/{id}/disconnect", post(disconnect_client))
        // Change-event SSE stream — consumed by `bin/wrappers_mcp.rs` so
        // newly-installed wrappers surface as MCP tools without an AI
        // session restart.
        .route("/wrappers/events", get(events_stream))
}

// ---------------------------------------------------------------------------
// Change-event stream
// ---------------------------------------------------------------------------

/// `GET /wrappers/events` — SSE stream of `wrapper.changed` events.
///
/// Each registry mutation (`rescan`, `rescan_all`, `remove`) emits one
/// event. Subscribers refetch `GET /wrappers` after a wakeup; the event
/// payload itself only carries an `atMs` timestamp.
///
/// Primary-only. On a secondary the wrapper subsystem isn't initialized
/// (see `mcp_api.rs` — `WrapperState::new_default()` is gated on
/// `!is_secondary()`), and our broadcaster is a per-process `OnceLock`
/// so a secondary's channel has no producer. Returning 503 here makes
/// the bin's reconnect-with-backoff surface the misroute in stderr;
/// silently serving an empty stream (older behavior) looked like a
/// healthy connection but received no events. The bin should always
/// point at the primary's port via `QONTINUI_PRIMARY_PORT`.
async fn events_stream(
    State(_state): State<Arc<ApiState>>,
) -> Result<
    Sse<impl futures_util::Stream<Item = Result<Event, Infallible>>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    if primary_proxy::is_secondary() {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(api_error(
                "wrapper change-events are primary-only — point QONTINUI_PRIMARY_PORT at the primary",
            )),
        ));
    }
    let rx = events::subscribe();
    let stream = BroadcastStream::new(rx).filter_map(|res| async move {
        match res {
            Ok(ev) => match Event::default().event("wrapper.changed").json_data(&ev) {
                Ok(e) => Some(Ok::<Event, Infallible>(e)),
                Err(_) => None,
            },
            // Lag means a slow subscriber fell behind. Drop the lag
            // marker silently — the next genuine event still triggers a
            // refresh, which catches up.
            Err(_) => None,
        }
    });
    Ok(Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(30))))
}

// ---------------------------------------------------------------------------
// Registry handlers
// ---------------------------------------------------------------------------

async fn list_wrappers(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<Vec<Wrapper>>>, (StatusCode, Json<ApiResponse<()>>)> {
    if primary_proxy::is_secondary() {
        let wrappers = primary_proxy::list_wrappers()
            .await
            .map_err(proxy_error_to_http)?;
        return Ok(Json(ApiResponse::success(wrappers)));
    }
    let ws = require_wrapper_state(&state)?;
    Ok(Json(ApiResponse::success(ws.registry.get_all().await)))
}

async fn get_wrapper(
    State(state): State<Arc<ApiState>>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<ApiResponse<Wrapper>>, (StatusCode, Json<ApiResponse<()>>)> {
    if primary_proxy::is_secondary() {
        let wrapper = primary_proxy::get_wrapper(&id)
            .await
            .map_err(proxy_error_to_http)?;
        return Ok(Json(ApiResponse::success(wrapper)));
    }
    let ws = require_wrapper_state(&state)?;
    match ws.registry.get(&id).await {
        Some(w) => Ok(Json(ApiResponse::success(w))),
        None => Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!("wrapper '{}' is not installed", id))),
        )),
    }
}

// ---------------------------------------------------------------------------
// Install handlers
// ---------------------------------------------------------------------------

async fn install_wrapper(
    State(state): State<Arc<ApiState>>,
    Json(req): Json<InstallRequest>,
) -> Result<Json<ApiResponse<InstallResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    if primary_proxy::is_secondary() {
        let resp = primary_proxy::install_wrapper(&req)
            .await
            .map_err(proxy_error_to_http)?;
        return Ok(Json(ApiResponse::success(resp)));
    }
    let ws = require_wrapper_state(&state)?;
    match install(&ws.registry, &req.package, req.version.as_deref()).await {
        Ok(resp) => Ok(Json(ApiResponse::success(resp))),
        Err(e) => Err(install_error_to_http(e)),
    }
}

async fn update_wrapper(
    State(state): State<Arc<ApiState>>,
    AxumPath(id): AxumPath<String>,
    body: Option<Json<UpdateRequest>>,
) -> Result<Json<ApiResponse<InstallResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    let version = body.and_then(|Json(b)| b.version);
    if primary_proxy::is_secondary() {
        let resp = primary_proxy::update_wrapper(&id, version.as_deref())
            .await
            .map_err(proxy_error_to_http)?;
        return Ok(Json(ApiResponse::success(resp)));
    }
    let ws = require_wrapper_state(&state)?;
    match update(&ws.registry, &ws.manager, &id, version.as_deref()).await {
        Ok(resp) => Ok(Json(ApiResponse::success(resp))),
        Err(e) => Err(install_error_to_http(e)),
    }
}

async fn uninstall_wrapper(
    State(state): State<Arc<ApiState>>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<ApiResponse<&'static str>>, (StatusCode, Json<ApiResponse<()>>)> {
    if primary_proxy::is_secondary() {
        let _ = primary_proxy::uninstall_wrapper(&id)
            .await
            .map_err(proxy_error_to_http)?;
        return Ok(Json(ApiResponse::success("uninstalled")));
    }
    let ws = require_wrapper_state(&state)?;
    match uninstall(&ws.registry, &ws.manager, &id).await {
        Ok(()) => Ok(Json(ApiResponse::success("uninstalled"))),
        Err(e) => Err(install_error_to_http(e)),
    }
}

#[derive(Debug, Deserialize)]
struct UpdateRequest {
    #[serde(default)]
    version: Option<String>,
}

fn install_error_to_http(e: InstallError) -> (StatusCode, Json<ApiResponse<()>>) {
    let status = match &e {
        InstallError::NoPackageManager => StatusCode::SERVICE_UNAVAILABLE,
        InstallError::InvalidSpec(_) => StatusCode::BAD_REQUEST,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (status, Json(api_error(e.to_string())))
}

// ---------------------------------------------------------------------------
// Lifecycle handlers
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct StartedResponse {
    port: u16,
}

async fn start_wrapper(
    State(state): State<Arc<ApiState>>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<ApiResponse<StartedResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    if primary_proxy::is_secondary() {
        let port = primary_proxy::start_wrapper(&id)
            .await
            .map_err(proxy_error_to_http)?;
        return Ok(Json(ApiResponse::success(StartedResponse { port })));
    }
    let ws = require_wrapper_state(&state)?;
    match ws.manager.spawn(&id).await {
        Ok(port) => Ok(Json(ApiResponse::success(StartedResponse { port }))),
        Err(e) => Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(api_error(format!("failed to start '{}': {}", id, e))),
        )),
    }
}

async fn stop_wrapper(
    State(state): State<Arc<ApiState>>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<ApiResponse<&'static str>>, (StatusCode, Json<ApiResponse<()>>)> {
    if primary_proxy::is_secondary() {
        let _ = primary_proxy::stop_wrapper(&id)
            .await
            .map_err(proxy_error_to_http)?;
        return Ok(Json(ApiResponse::success("stopped")));
    }
    let ws = require_wrapper_state(&state)?;
    match ws.manager.stop(&id).await {
        Ok(()) => Ok(Json(ApiResponse::success("stopped"))),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("failed to stop '{}': {}", id, e))),
        )),
    }
}

async fn wrapper_status(
    State(state): State<Arc<ApiState>>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<ApiResponse<WrapperStatus>>, (StatusCode, Json<ApiResponse<()>>)> {
    if primary_proxy::is_secondary() {
        let status = primary_proxy::wrapper_status(&id)
            .await
            .map_err(proxy_error_to_http)?;
        return Ok(Json(ApiResponse::success(status)));
    }
    let ws = require_wrapper_state(&state)?;
    Ok(Json(ApiResponse::success(ws.manager.status(&id).await)))
}

// ---------------------------------------------------------------------------
// Credential handlers
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct CredentialDescriptor {
    name: String,
    /// `true` iff a value is currently stored. Plaintext is NEVER returned
    /// by this endpoint — see `credentials.rs` for the security
    /// invariants.
    #[serde(rename = "hasValue")]
    has_value: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    description: String,
    required: bool,
    secret: bool,
}

async fn list_credentials(
    State(state): State<Arc<ApiState>>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    if primary_proxy::is_secondary() {
        let descriptors = primary_proxy::list_credentials(&id)
            .await
            .map_err(proxy_error_to_http)?;
        return Ok(Json(ApiResponse::success(serde_json::Value::Array(
            descriptors,
        ))));
    }
    let ws = require_wrapper_state(&state)?;
    let wrapper = ws.registry.get(&id).await.ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            Json(api_error(format!("wrapper '{}' is not installed", id))),
        )
    })?;
    let descriptors: Vec<CredentialDescriptor> = wrapper
        .manifest
        .env_vars
        .iter()
        .map(|spec| CredentialDescriptor {
            name: spec.name.clone(),
            has_value: CredentialStore::has(&id, &spec.name),
            description: spec.description.clone(),
            required: spec.required,
            secret: spec.secret,
        })
        .collect();
    let value = serde_json::to_value(descriptors).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!(
                "failed to serialize credential descriptors: {}",
                e
            ))),
        )
    })?;
    Ok(Json(ApiResponse::success(value)))
}

#[derive(Debug, Deserialize)]
struct SetCredentialRequest {
    value: String,
}

async fn set_credential(
    State(state): State<Arc<ApiState>>,
    AxumPath((id, name)): AxumPath<(String, String)>,
    Json(req): Json<SetCredentialRequest>,
) -> Result<Json<ApiResponse<&'static str>>, (StatusCode, Json<ApiResponse<()>>)> {
    if primary_proxy::is_secondary() {
        let _ = primary_proxy::set_credential(&id, &name, &req.value)
            .await
            .map_err(proxy_error_to_http)?;
        return Ok(Json(ApiResponse::success("set")));
    }
    let ws = require_wrapper_state(&state)?;
    if ws.registry.get(&id).await.is_none() {
        return Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!("wrapper '{}' is not installed", id))),
        ));
    }
    if req.value.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(api_error(
                "credential value must not be empty (use DELETE to clear)",
            )),
        ));
    }
    match CredentialStore::set(&id, &name, &req.value) {
        Ok(()) => Ok(Json(ApiResponse::success("set"))),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("failed to set credential: {}", e))),
        )),
    }
}

// ---------------------------------------------------------------------------
// AI client handlers
// ---------------------------------------------------------------------------

fn parse_client_id(s: &str) -> Result<AiClientId, (StatusCode, Json<ApiResponse<()>>)> {
    serde_json::from_value::<AiClientId>(serde_json::Value::String(s.to_string())).map_err(|_| {
        (
            StatusCode::BAD_REQUEST,
            Json(api_error(format!("unknown AI client id: {}", s))),
        )
    })
}

fn client_error_to_http(e: ClientError) -> (StatusCode, Json<ApiResponse<()>>) {
    let (status, msg) = match e {
        ClientError::NotInstalled => (StatusCode::NOT_FOUND, "client is not installed".to_string()),
        ClientError::ConfigParseFailed { ref path } => (
            StatusCode::UNPROCESSABLE_ENTITY,
            format!(
                "Your config at {} has invalid JSON. Open it and fix it, or delete it to start fresh.",
                path.display()
            ),
        ),
        ClientError::LauncherMissing => (StatusCode::SERVICE_UNAVAILABLE, e.to_string()),
        ClientError::IoError { ref path, ref msg } => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("I/O error for {}: {}", path.display(), msg),
        ),
    };
    (status, Json(api_error(msg)))
}

async fn list_clients(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<Vec<ClientInfo>>>, (StatusCode, Json<ApiResponse<()>>)> {
    if primary_proxy::is_secondary() {
        let infos = primary_proxy::list_clients()
            .await
            .map_err(proxy_error_to_http)?;
        return Ok(Json(ApiResponse::success(infos)));
    }
    let port = runner_api_port(&state.app_state);
    Ok(Json(ApiResponse::success(clients::detect_all(port))))
}

async fn connect_client(
    State(state): State<Arc<ApiState>>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<ApiResponse<&'static str>>, (StatusCode, Json<ApiResponse<()>>)> {
    let id = parse_client_id(&id)?;
    if primary_proxy::is_secondary() {
        primary_proxy::connect_client(id)
            .await
            .map_err(proxy_error_to_http)?;
        return Ok(Json(ApiResponse::success("connected")));
    }
    let port = runner_api_port(&state.app_state);
    clients::connect(id, port).map_err(client_error_to_http)?;
    Ok(Json(ApiResponse::success("connected")))
}

async fn disconnect_client(
    State(_state): State<Arc<ApiState>>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<ApiResponse<&'static str>>, (StatusCode, Json<ApiResponse<()>>)> {
    let id = parse_client_id(&id)?;
    if primary_proxy::is_secondary() {
        primary_proxy::disconnect_client(id)
            .await
            .map_err(proxy_error_to_http)?;
        return Ok(Json(ApiResponse::success("disconnected")));
    }
    clients::disconnect(id).map_err(client_error_to_http)?;
    Ok(Json(ApiResponse::success("disconnected")))
}

async fn delete_credential(
    State(state): State<Arc<ApiState>>,
    AxumPath((id, name)): AxumPath<(String, String)>,
) -> Result<Json<ApiResponse<&'static str>>, (StatusCode, Json<ApiResponse<()>>)> {
    if primary_proxy::is_secondary() {
        let _ = primary_proxy::delete_credential(&id, &name)
            .await
            .map_err(proxy_error_to_http)?;
        return Ok(Json(ApiResponse::success("deleted")));
    }
    let ws = require_wrapper_state(&state)?;
    if ws.registry.get(&id).await.is_none() {
        return Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!("wrapper '{}' is not installed", id))),
        ));
    }
    match CredentialStore::delete(&id, &name) {
        Ok(()) => Ok(Json(ApiResponse::success("deleted"))),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("failed to delete credential: {}", e))),
        )),
    }
}

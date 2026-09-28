//! Auto-continue settings handlers for MCP API
//!
//! Manages the auto-continue AI workflow setting at both global
//! and per-active-workflow levels, plus supervisor availability checks.

use axum::response::Json;
use serde::{Deserialize, Serialize};
use tracing::info;

use crate::mcp::types::ApiResponse;
use crate::settings;

/// Response for auto-continue setting
#[derive(Debug, Serialize)]
pub struct AutoContinueSettingResponse {
    enabled: bool,
}

/// Request body for setting auto-continue
#[derive(Debug, Deserialize)]
pub struct SetAutoContinueRequest {
    enabled: bool,
}

/// Response for per-workflow auto-continue setting
#[derive(Debug, Serialize)]
pub struct WorkflowAutoContinueResponse {
    enabled: bool,
    workflow_name: Option<String>,
}

/// Get the auto-continue AI workflow setting
pub async fn get_auto_continue_setting() -> Json<ApiResponse<AutoContinueSettingResponse>> {
    let enabled = settings::get_auto_continue_ai_workflow();
    Json(ApiResponse::success(AutoContinueSettingResponse {
        enabled,
    }))
}

/// Set the auto-continue AI workflow setting
pub async fn set_auto_continue_setting(
    Json(body): Json<SetAutoContinueRequest>,
) -> Json<ApiResponse<AutoContinueSettingResponse>> {
    match settings::save_auto_continue_ai_workflow(body.enabled) {
        Ok(_) => {
            info!(
                "Auto-continue AI workflow setting updated to: {}",
                body.enabled
            );
            Json(ApiResponse::success(AutoContinueSettingResponse {
                enabled: body.enabled,
            }))
        }
        Err(e) => Json(ApiResponse {
            success: false,
            data: None,
            error: Some(format!("Failed to save setting: {}", e)),
            error_detail: None,
            hint: None,
            code: None,
            suggestions: None,
        }),
    }
}

/// Get the auto-continue setting for the active workflow.
/// Uses global setting and checks for running tasks in database.
pub async fn get_workflow_auto_continue() -> Json<ApiResponse<WorkflowAutoContinueResponse>> {
    let enabled = settings::get_auto_continue_ai_workflow();

    // Check if there are any running tasks (PG)
    let workflow_name = if let Some(pg) = crate::database::pg::PgDb::try_global() {
        pg.get_running_task_runs(None)
            .await
            .ok()
            .and_then(|tasks| tasks.first().map(|t| t.task_name.clone()))
    } else {
        None
    };

    Json(ApiResponse::success(WorkflowAutoContinueResponse {
        enabled,
        workflow_name,
    }))
}

/// Set the auto-continue setting for the active workflow.
/// Updates the global setting.
pub async fn set_workflow_auto_continue(
    Json(body): Json<SetAutoContinueRequest>,
) -> Json<ApiResponse<WorkflowAutoContinueResponse>> {
    // Update the global setting
    match settings::save_auto_continue_ai_workflow(body.enabled) {
        Ok(_) => {
            info!("Auto-continue setting updated to: {}", body.enabled);

            // Get the active workflow name if any (PG)
            let workflow_name = if let Some(pg) = crate::database::pg::PgDb::try_global() {
                pg.get_running_task_runs(None)
                    .await
                    .ok()
                    .and_then(|tasks| tasks.first().map(|t| t.task_name.clone()))
            } else {
                None
            };

            Json(ApiResponse::success(WorkflowAutoContinueResponse {
                enabled: body.enabled,
                workflow_name,
            }))
        }
        Err(e) => Json(ApiResponse {
            success: false,
            data: None,
            error: Some(format!("Failed to update auto-continue setting: {}", e)),
            error_detail: None,
            hint: None,
            code: None,
            suggestions: None,
        }),
    }
}

/// How long the supervisor probe waits for a TCP connect before calling the
/// supervisor absent.
const SUPERVISOR_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(500);

/// Probe one `host:port` for a TCP listener. PURE apart from the connect.
///
/// Tri-state, and the third state is the point: `None` means the address
/// could not be parsed, so NOTHING was probed. The previous `bool` collapsed
/// that into `false` ("no supervisor"), which is a verdict derived from a
/// failure to ask — e.g. `QONTINUI_SUPERVISOR_URL=http://localhost:<port>`
/// yields `localhost:<port>`, which `SocketAddr` does not parse.
pub fn probe_supervisor_at(addr: &str, timeout: std::time::Duration) -> Option<bool> {
    let socket_addr: std::net::SocketAddr = addr.parse().ok()?;
    Some(std::net::TcpStream::connect_timeout(&socket_addr, timeout).is_ok())
}

/// Whether a supervisor listens at this runner's configured supervisor
/// address. BLOCKING (up to 500 ms): call it from a blocking thread.
///
/// `Some(true)` a listener answered, `Some(false)` none did, `None` the
/// configured address does not parse — see [`probe_supervisor_at`]. Used to
/// pick the restart instructions an AI session gets, and served as a read by
/// `GET /supervisor/observation`.
pub fn check_supervisor_available() -> Option<bool> {
    probe_supervisor_at(
        &crate::api_config::get_supervisor_socket_addr(),
        SUPERVISOR_PROBE_TIMEOUT,
    )
}

/// `GET /supervisor/observation` — the ONE supervisor probe, exposed as a read.
///
/// The published runner has no supervisor; the dev-only settings panels that
/// talk to one (CI runner, "Test My Change") render only when this reports
/// `observed: true`, and take the supervisor's address from here rather than
/// from a port literal. Plan
/// `2026-09-20-the-published-product-works-without-knowing-a-development-environment-exists` B2.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SupervisorObservation {
    /// `true` a listener answered, `false` none did, `null` the configured
    /// address does not parse, so nothing was probed (UNKNOWN, not absent).
    pub observed: Option<bool>,
    /// RFC 3339 time of THIS probe — every read probes afresh.
    pub probed_at: String,
    /// The probed port; `null` when the address does not parse.
    pub port: Option<u16>,
    /// The supervisor's HTTP base URL; `null` when the address does not parse.
    pub base_url: Option<String>,
}

/// Build the observation for one address. PURE apart from the connect.
pub fn observe_supervisor_at(
    addr: &str,
    base_url: &str,
    timeout: std::time::Duration,
) -> SupervisorObservation {
    let parsed: Option<std::net::SocketAddr> = addr.parse().ok();
    SupervisorObservation {
        observed: probe_supervisor_at(addr, timeout),
        probed_at: chrono::Utc::now().to_rfc3339(),
        port: parsed.map(|a| a.port()),
        base_url: parsed.map(|_| base_url.to_string()),
    }
}

/// [`observe_supervisor_at`] against this runner's configured supervisor.
/// BLOCKING (up to 500 ms).
pub fn observe_supervisor() -> SupervisorObservation {
    observe_supervisor_at(
        &crate::api_config::get_supervisor_socket_addr(),
        &crate::api_config::get_supervisor_url(),
        SUPERVISOR_PROBE_TIMEOUT,
    )
}

/// `GET /supervisor/observation`. The probe is a blocking TCP connect, so it
/// runs off the async runtime.
async fn get_supervisor_observation() -> Json<ApiResponse<SupervisorObservation>> {
    match qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked(observe_supervisor).await {
        Ok(obs) => Json(ApiResponse::success(obs)),
        Err(e) => Json(ApiResponse {
            success: false,
            data: None,
            error: Some(format!(
                "supervisor observation probe did not complete: {e}"
            )),
            error_detail: None,
            hint: None,
            code: None,
            suggestions: None,
        }),
    }
}

/// Create routes for auto-continue settings.
pub fn routes() -> axum::Router<std::sync::Arc<crate::mcp::types::ApiState>> {
    use axum::routing::get;
    axum::Router::new()
        .route(
            "/workflow/auto-continue",
            get(get_auto_continue_setting).post(set_auto_continue_setting),
        )
        .route(
            "/workflow/active/auto-continue",
            get(get_workflow_auto_continue).post(set_workflow_auto_continue),
        )
        .route("/supervisor/observation", get(get_supervisor_observation))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::time::Duration;

    const T: Duration = Duration::from_millis(500);

    /// The third state: an address that does not parse probed NOTHING, so it
    /// is `None` — never collapsed into "no supervisor".
    #[test]
    fn an_unparseable_address_is_unknown_not_absent() {
        // What `get_supervisor_socket_addr` yields for
        // `QONTINUI_SUPERVISOR_URL=http://localhost:<port>`: a hostname, which
        // `SocketAddr` does not parse.
        assert_eq!(probe_supervisor_at("localhost:1", T), None);
        assert_eq!(probe_supervisor_at("not an address", T), None);
        assert_eq!(probe_supervisor_at("", T), None);
    }

    #[test]
    fn a_listening_address_is_observed() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr").to_string();
        assert_eq!(probe_supervisor_at(&addr, T), Some(true));
    }

    #[test]
    fn a_silent_address_is_not_observed() {
        // Bind to learn a free port, then drop the listener so nothing answers.
        let addr = {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
            listener.local_addr().expect("addr").to_string()
        };
        assert_eq!(probe_supervisor_at(&addr, T), Some(false));
    }

    #[test]
    fn the_observation_carries_port_and_base_url_only_when_the_address_parses() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let addr = format!("127.0.0.1:{port}");
        let base = format!("http://127.0.0.1:{port}");
        let obs = observe_supervisor_at(&addr, &base, T);
        assert_eq!(obs.observed, Some(true));
        assert_eq!(obs.port, Some(port));
        assert_eq!(obs.base_url.as_deref(), Some(base.as_str()));
        assert!(chrono::DateTime::parse_from_rfc3339(&obs.probed_at).is_ok());

        let unknown = observe_supervisor_at("localhost:1", "http://localhost:1", T);
        assert_eq!(unknown.observed, None);
        assert_eq!(unknown.port, None);
        assert_eq!(unknown.base_url, None);
    }

    /// The wire shape the settings panels read: `observed` serialises as a
    /// JSON `null` (not an absent key) when unknown.
    #[test]
    fn unknown_serialises_as_explicit_null() {
        let obs = observe_supervisor_at("localhost:1", "http://localhost:1", T);
        let v = serde_json::to_value(&obs).expect("serialise");
        assert!(v.get("observed").expect("observed key present").is_null());
        assert!(v.get("port").expect("port key present").is_null());
        assert!(v.get("probed_at").expect("probed_at").is_string());
    }
}

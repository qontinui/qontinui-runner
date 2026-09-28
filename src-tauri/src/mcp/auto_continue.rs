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

/// Total budget for the TCP half of a supervisor probe, across every address
/// the configured host resolves to.
const SUPERVISOR_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(500);

/// Budget for the identity half (`GET <base>/health`) of an observation.
const SUPERVISOR_IDENTITY_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(1500);

/// Probe one `host:port` for a TCP listener. BLOCKING, bounded by `budget`.
///
/// The host is RESOLVED (`ToSocketAddrs`), so `localhost:<port>` works, and
/// every resolved address is tried in turn (typically `::1` then
/// `127.0.0.1`) — each attempt gets an even share of what is left of the
/// budget, so one address that hangs cannot starve the rest.
///
/// Tri-state: `None` means the address did not resolve (or resolved to
/// nothing), so NOTHING was probed. The previous `bool` collapsed that into
/// `false` ("no supervisor") — a verdict derived from a failure to ask.
pub fn probe_supervisor_at(addr: &str, budget: std::time::Duration) -> Option<bool> {
    use std::net::ToSocketAddrs;
    let addrs: Vec<std::net::SocketAddr> = addr.to_socket_addrs().ok()?.collect();
    if addrs.is_empty() {
        return None;
    }
    let deadline = std::time::Instant::now() + budget;
    let n = addrs.len() as u32;
    for (i, a) in addrs.iter().enumerate() {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        let share = remaining / (n - i as u32);
        if std::net::TcpStream::connect_timeout(a, share.max(std::time::Duration::from_millis(1)))
            .is_ok()
        {
            return Some(true);
        }
    }
    Some(false)
}

/// Whether a supervisor listens at this runner's configured supervisor
/// address. BLOCKING (up to 500 ms): call it from a blocking thread.
///
/// `Some(true)` a listener answered, `Some(false)` none did, `None` the
/// configured URL names no port or its host does not resolve — see
/// [`probe_supervisor_at`]. TCP only: it picks the restart instructions an AI
/// session gets. `GET /supervisor/observation` additionally confirms identity
/// ([`observe_supervisor`]).
pub fn check_supervisor_available() -> Option<bool> {
    probe_supervisor_at(
        &crate::api_config::get_supervisor_socket_addr()?,
        SUPERVISOR_PROBE_TIMEOUT,
    )
}

/// `GET /supervisor/observation` — the supervisor probe, exposed as a read.
///
/// The published runner has no supervisor; the dev-only settings panels that
/// talk to one (CI runner, "Test My Change") render only when this reports
/// `observed: true`, and take the supervisor's address from here rather than
/// from a port literal. Plan
/// `2026-09-20-the-published-product-works-without-knowing-a-development-environment-exists` B2.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SupervisorObservation {
    /// `true` a supervisor answered and identified itself; `false` nothing
    /// listens, or what listens is not a supervisor; `null` nothing could be
    /// probed (the configured URL names no port, or its host does not
    /// resolve) — UNKNOWN, not absent.
    pub observed: Option<bool>,
    /// RFC 3339 time of THIS probe — every read probes afresh.
    pub probed_at: String,
    /// The port that was probed; `null` when nothing was.
    pub port: Option<u16>,
    /// `<scheme>://<host:port>` — exactly the authority that was probed, with
    /// any configured path dropped; `null` when nothing was.
    pub base_url: Option<String>,
    /// Why `observed` is not `true`; `null` when it is.
    pub reason: Option<String>,
}

/// PURE: does a `/health` body identify a qontinui-supervisor?
///
/// Keyed on `supervisor.project_dir` (a string): the supervisor's
/// `HealthResponse.supervisor: SupervisorInfo` always carries it, it is the
/// field the "Test My Change" panel consumes, and the runner's own `/health`
/// has no `supervisor` object — so another runner, or any other local
/// service, on that port does not pass.
pub fn is_supervisor_health(body: &serde_json::Value) -> bool {
    body.get("supervisor")
        .and_then(|s| s.get("project_dir"))
        .is_some_and(|v| v.is_string())
}

/// `GET <base_url>/health` and check [`is_supervisor_health`]. BLOCKING.
fn confirm_supervisor_identity(base_url: &str, timeout: std::time::Duration) -> Result<(), String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(timeout)
        .build()
        .map_err(|e| format!("could not build HTTP client: {e}"))?;
    let resp = client
        .get(format!("{base_url}/health"))
        .send()
        .map_err(|e| format!("GET {base_url}/health failed: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("GET {base_url}/health answered {}", resp.status()));
    }
    let body: serde_json::Value = resp
        .json()
        .map_err(|e| format!("GET {base_url}/health is not JSON: {e}"))?;
    if is_supervisor_health(&body) {
        Ok(())
    } else {
        Err(format!(
            "the listener at {base_url} is not a supervisor (its /health has no supervisor.project_dir)"
        ))
    }
}

/// Build the observation for a configured supervisor URL. BLOCKING, bounded
/// by `tcp_budget + identity_budget` (plus name resolution).
pub fn observe_supervisor_at(
    supervisor_url: &str,
    tcp_budget: std::time::Duration,
    identity_budget: std::time::Duration,
) -> SupervisorObservation {
    let probed_at = chrono::Utc::now().to_rfc3339();
    let unknown = |reason: String| SupervisorObservation {
        observed: None,
        probed_at: probed_at.clone(),
        port: None,
        base_url: None,
        reason: Some(reason),
    };
    let Some(host_port) = crate::api_config::supervisor_host_port(supervisor_url) else {
        return unknown(format!(
            "the configured supervisor URL {supervisor_url:?} names no port, so nothing was probed"
        ));
    };
    let port = host_port
        .rsplit_once(':')
        .and_then(|(_, p)| p.parse::<u16>().ok());
    let scheme = supervisor_url.split_once("://").map_or("http", |x| x.0);
    let base_url = format!("{scheme}://{host_port}");
    let known = |observed: bool, reason: Option<String>| SupervisorObservation {
        observed: Some(observed),
        probed_at: probed_at.clone(),
        port,
        base_url: Some(base_url.clone()),
        reason,
    };
    match probe_supervisor_at(&host_port, tcp_budget) {
        None => unknown(format!(
            "{host_port} does not resolve, so nothing was probed"
        )),
        Some(false) => known(false, Some(format!("nothing listens at {host_port}"))),
        Some(true) => match confirm_supervisor_identity(&base_url, identity_budget) {
            Ok(()) => known(true, None),
            Err(reason) => known(false, Some(reason)),
        },
    }
}

/// [`observe_supervisor_at`] against this runner's configured supervisor.
/// BLOCKING (TCP ≤ 500 ms, then identity ≤ 1.5 s only if something listens).
pub fn observe_supervisor() -> SupervisorObservation {
    observe_supervisor_at(
        &crate::api_config::get_supervisor_url(),
        SUPERVISOR_PROBE_TIMEOUT,
        SUPERVISOR_IDENTITY_TIMEOUT,
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
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::time::Duration;

    const T: Duration = Duration::from_millis(500);

    /// A tiny HTTP server answering every request with `body`. Connections
    /// that send nothing (the bare TCP probe) are dropped.
    fn serve(body: &'static str) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        std::thread::spawn(move || {
            for stream in listener.incoming().take(8) {
                let Ok(mut s) = stream else { continue };
                let _ = s.set_read_timeout(Some(Duration::from_millis(500)));
                let mut buf = [0u8; 2048];
                if !matches!(s.read(&mut buf), Ok(n) if n > 0) {
                    continue;
                }
                let _ = write!(
                    s,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
            }
        });
        port
    }

    /// The third state: an address that does not resolve probed NOTHING, so
    /// it is `None` — never collapsed into "no supervisor".
    #[test]
    fn an_unresolvable_address_is_unknown_not_absent() {
        assert_eq!(probe_supervisor_at("not an address", T), None);
        assert_eq!(probe_supervisor_at("no-port-here", T), None);
        assert_eq!(probe_supervisor_at("", T), None);
    }

    #[test]
    fn a_listening_address_is_observed() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr").to_string();
        assert_eq!(probe_supervisor_at(&addr, T), Some(true));
    }

    /// `localhost` is RESOLVED, and every address tried: on a dual-stack box
    /// `::1` is refused first and `127.0.0.1` answers.
    #[test]
    fn a_host_name_is_resolved_and_every_address_tried() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        assert_eq!(
            probe_supervisor_at(&format!("localhost:{port}"), T),
            Some(true)
        );
    }

    /// TEST-NET-1 (RFC 5737) is reserved and unroutable: nothing can answer,
    /// whether the connect is refused, unreachable or times out.
    #[test]
    fn a_silent_address_is_not_observed() {
        assert_eq!(
            probe_supervisor_at("192.0.2.1:9", Duration::from_millis(200)),
            Some(false)
        );
    }

    #[test]
    fn a_url_with_no_port_is_unknown_and_reports_nothing_probed() {
        let obs = observe_supervisor_at("http://sup.example", T, T);
        assert_eq!(obs.observed, None);
        assert_eq!(obs.port, None);
        assert_eq!(obs.base_url, None);
        assert!(obs.reason.is_some());
    }

    #[test]
    fn a_supervisor_health_body_is_observed_at_the_probed_address() {
        let port = serve(r#"{"status":"ok","supervisor":{"version":"x","project_dir":"/p"}}"#);
        // The configured path is dropped: base_url is exactly what was probed.
        let obs = observe_supervisor_at(&format!("http://127.0.0.1:{port}/ignored"), T, T);
        assert_eq!(obs.observed, Some(true), "{:?}", obs.reason);
        assert_eq!(obs.port, Some(port));
        assert_eq!(
            obs.base_url.as_deref(),
            Some(format!("http://127.0.0.1:{port}").as_str())
        );
        assert_eq!(obs.reason, None);
        assert!(chrono::DateTime::parse_from_rfc3339(&obs.probed_at).is_ok());
    }

    /// Something else on the port — e.g. a runner, whose `/health` has no
    /// `supervisor` object — is NOT a supervisor.
    #[test]
    fn a_non_supervisor_listener_is_not_observed_and_says_why() {
        let port = serve(r#"{"status":"ok","uiBridge":{"appId":"runner"}}"#);
        let obs = observe_supervisor_at(&format!("http://127.0.0.1:{port}"), T, T);
        assert_eq!(obs.observed, Some(false));
        assert_eq!(obs.port, Some(port));
        assert!(obs
            .reason
            .as_deref()
            .unwrap_or("")
            .contains("not a supervisor"));
    }

    #[test]
    fn a_bare_tcp_listener_is_not_observed() {
        // Accepts and never speaks HTTP.
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let obs = observe_supervisor_at(&format!("http://127.0.0.1:{port}"), T, T);
        assert_eq!(obs.observed, Some(false));
        assert!(obs.reason.is_some());
    }

    #[test]
    fn identity_is_keyed_on_supervisor_project_dir() {
        assert!(is_supervisor_health(
            &serde_json::json!({"supervisor": {"project_dir": "/x"}})
        ));
        assert!(!is_supervisor_health(
            &serde_json::json!({"project_dir": "/x"})
        ));
        assert!(!is_supervisor_health(
            &serde_json::json!({"supervisor": {"project_dir": 1}})
        ));
        assert!(!is_supervisor_health(&serde_json::json!({})));
    }

    /// The wire shape the settings panels read: `observed` serialises as a
    /// JSON `null` (not an absent key) when unknown.
    #[test]
    fn unknown_serialises_as_explicit_null() {
        let obs = observe_supervisor_at("http://sup.example", T, T);
        let v = serde_json::to_value(&obs).expect("serialise");
        assert!(v.get("observed").expect("observed key present").is_null());
        assert!(v.get("port").expect("port key present").is_null());
        assert!(v.get("probed_at").expect("probed_at").is_string());
        assert!(v.get("reason").expect("reason").is_string());
    }
}

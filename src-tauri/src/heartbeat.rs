//! Heartbeat service for fleet registration.
//!
//! Periodically sends heartbeat to the web backend so the dev dashboard
//! can track all runner instances across machines.

use std::sync::atomic::Ordering;
use std::sync::Arc;
use tokio::time::{interval, Duration};
use tracing::{debug, info};

use serde::Serialize;

use crate::commands::AppState;
use crate::ui_error::UiError;

#[derive(Debug, Serialize)]
struct HeartbeatPayload {
    hostname: String,
    ip: String,
    port: u16,
    /// Reachability honesty (plan
    /// 2026-06-12-mobile-account-usage-error-recovery, runner P1): whether
    /// the advertised `ip:port` is actually served on a LAN-reachable
    /// (non-loopback) interface. Derived at bind time from the API
    /// listener's REAL local address (`AppState.api_lan_bound`, set in
    /// `mcp_api::start_server`) — currently always `false` because the
    /// runner API intentionally binds `127.0.0.1` only
    /// (`mcp_api::try_bind_port`) while `ip` reports the machine's LAN IP.
    /// Consumers (backend fleet registry → mobile) should treat `false` as
    /// "do not dial `ip:port` directly; use a loopback path (adb reverse)
    /// or a proxy". Additive field — backends that don't know it ignore it.
    lan_reachable: bool,
    instance_name: Option<String>,
    os: String,
    os_version: Option<String>,
    running_task_count: u32,
    running_task_ids: Vec<String>,
    /// Phase 3J.2 — `"healthy"` if no UI error is tracked, `"errored"`
    /// when the React error boundary has reported an unhandled error that
    /// has not yet been cleared OR a fresh Rust crash dump is present
    /// (post-3J follow-up).
    derived_status: String,
    /// Phase 3J.2 — latest UI error snapshot. Serialized as `null` when no
    /// error is tracked (always present as a key so consumers can assume
    /// the shape).
    ui_error: Option<UiError>,
    /// Post-3J follow-up — most recent Rust crash dump surfaced by the
    /// startup scanner. `null` when no fresh dump is present. Independent
    /// of `ui_error`: non-unwinding panics abort the process before the
    /// React boundary can report them.
    recent_crash: Option<HeartbeatRecentCrash>,
}

/// Snake-case projection of [`crate::crash_dumps::RecentCrash`] for the
/// heartbeat wire format. The `/health` endpoint emits the source struct as
/// camelCase to stay in sync with that endpoint's casing; qontinui-web's
/// heartbeat schema uses snake_case (matching `UiErrorPayload`) so we emit
/// a dedicated shape here.
#[derive(Debug, Clone, Serialize)]
struct HeartbeatRecentCrash {
    file_path: String,
    reported_at: chrono::DateTime<chrono::Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    panic_location: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    panic_message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    thread: Option<String>,
}

impl From<crate::crash_dumps::RecentCrash> for HeartbeatRecentCrash {
    fn from(c: crate::crash_dumps::RecentCrash) -> Self {
        Self {
            file_path: c.file_path,
            reported_at: c.reported_at,
            panic_location: c.panic_location,
            panic_message: c.panic_message,
            thread: c.thread,
        }
    }
}

/// Start the heartbeat background task.
///
/// Spawns a tokio task that sends a heartbeat POST to the web backend
/// every 30 seconds. If this is a secondary instance (`QONTINUI_PRIMARY_PORT`
/// is set), also sends a heartbeat to the primary runner every 15 seconds.
pub fn start_heartbeat(app_state: Arc<AppState>) {
    // `get_api_base_url()` is the single resolver: it already honors
    // `QONTINUI_WEB_BACKEND_URL` → `QONTINUI_API_URL` → default, so heartbeat
    // and workflow-sync can no longer resolve to different hosts.
    let backend_url = crate::api_config::get_api_base_url();

    // Backend renamed the route from `/api/v1/dev-dashboard/heartbeat` to
    // `/api/v1/operations/heartbeat` as part of the operations-rename refactor
    // (see `qontinui-web/backend/app/api/v1/endpoints/operations.py` — the
    // file was renamed from `dev_dashboard.py` and re-mounted at the new
    // prefix in `api.py`). The `/heartbeat` endpoint itself stays unauth +
    // LAN-only as the cross-machine beacon path.
    let heartbeat_url = format!("{}/api/v1/operations/heartbeat", backend_url);

    // If this is a secondary, also heartbeat to the primary runner
    let primary_port = crate::instance::primary_port();
    let registration_id: Arc<tokio::sync::Mutex<Option<String>>> =
        Arc::new(tokio::sync::Mutex::new(None));

    if let Some(pp) = primary_port {
        info!(
            "Starting heartbeat service -> {} + primary runner on port {}",
            heartbeat_url, pp
        );
    } else {
        info!("Starting heartbeat service -> {}", heartbeat_url);
    }

    tauri::async_runtime::spawn(async move {
        // Wait 5 seconds for the API to be ready before the first heartbeat
        tokio::time::sleep(Duration::from_secs(5)).await;

        let client = match reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
        {
            Ok(c) => c,
            Err(e) => {
                debug!("Failed to create heartbeat HTTP client: {}", e);
                return;
            }
        };

        let hostname = crate::fleet::canonical_hostname().unwrap_or_else(|| "unknown".to_string());

        let instance_name = crate::instance::instance_name();

        let os = if cfg!(target_os = "windows") {
            "windows"
        } else if cfg!(target_os = "macos") {
            "macos"
        } else {
            "linux"
        }
        .to_string();

        // Backend heartbeat: every 30s. Primary heartbeat: every 15s.
        // We tick every 15s and send the backend heartbeat every other tick.
        let mut ticker = interval(Duration::from_secs(15));
        let mut tick_count: u64 = 0;

        loop {
            ticker.tick().await;
            tick_count += 1;

            let port = app_state.api_port.load(Ordering::Relaxed);
            if port == 0 {
                debug!("API port not yet assigned, skipping heartbeat");
                continue;
            }

            // Get running task count from PostgreSQL
            let (task_count, task_ids) = match app_state.pg_db.get_running_task_runs(None).await {
                Ok(runs) => {
                    let count = runs.len() as u32;
                    let ids = runs.into_iter().map(|r| r.id).collect();
                    (count, ids)
                }
                Err(_) => get_running_tasks_fallback(),
            };

            // Phase 3J.2 — snapshot the latest UI error (if any) once per
            // tick; used by both the backend and primary-runner heartbeats.
            // Post-3J follow-up also surfaces the most recent Rust crash
            // dump so a runner that just restarted after a non-unwinding
            // panic still reports errored until the dump is dismissed.
            let ui_error_snapshot = app_state.ui_error.get().await;
            let recent_crash_snapshot = app_state
                .crash_dumps
                .get()
                .await
                .map(HeartbeatRecentCrash::from);
            // Read the /health-driven embedding probe cache. `None` (probe
            // hasn't run yet) collapses to "healthy" so a pre-probe heartbeat
            // doesn't flap to "degraded" on boot.
            let base_status = crate::ui_error::compute_derived_status(
                ui_error_snapshot.is_some(),
                recent_crash_snapshot.is_some(),
                crate::mcp_api::embedding_reachable_cached(),
            );

            // Phase 5 — provisioning completeness gate. A runner is
            // provisioning-complete only once it holds a live coord device JWT
            // (doctor check 5). ADVISORY by default: surface + log loudly but
            // never withhold ready (a hard block could brick a runner
            // mid-provision). `QONTINUI_PROVISIONING_GATE_ENFORCE` flips it to
            // ENFORCING, where readiness is withheld until check 5 passes. The
            // device-JWT predicate is REUSED from `coord_doctor` (not
            // re-implemented); the credential-gap is ALSO surfaced to the fleet
            // by `device_jwt_refresher`'s Phase-1b coord_credential status, so
            // the same `runner_coord_credentials_missing` alert lights up — we
            // do not invent a second signal here.
            let check5 = qontinui_runner_lib::coord_doctor::device_jwt_live_check();
            let readiness = qontinui_runner_lib::coord_doctor::provisioning_readiness(
                check5.ok,
                qontinui_runner_lib::coord_doctor::provisioning_gate_enforce_enabled(),
            );
            let derived_status =
                provisioning_gated_status(base_status, readiness, &check5.detail).to_string();

            // Send to web backend every 30s (every other tick)
            if tick_count.is_multiple_of(2) {
                let ip = get_local_ip().unwrap_or_else(|| "127.0.0.1".to_string());
                let payload = HeartbeatPayload {
                    hostname: hostname.clone(),
                    ip,
                    port,
                    lan_reachable: app_state.api_lan_bound.load(Ordering::Relaxed),
                    instance_name: instance_name.clone(),
                    os: os.clone(),
                    os_version: None,
                    running_task_count: task_count,
                    running_task_ids: task_ids.clone(),
                    derived_status: derived_status.clone(),
                    ui_error: ui_error_snapshot.clone(),
                    recent_crash: recent_crash_snapshot.clone(),
                };

                match client.post(&heartbeat_url).json(&payload).send().await {
                    Ok(resp) if resp.status().is_success() => {
                        debug!("Heartbeat sent to backend");
                    }
                    Ok(resp) => {
                        debug!("Backend heartbeat response: {}", resp.status());
                    }
                    Err(e) => {
                        debug!("Backend heartbeat failed: {}", e);
                    }
                }
            }

            // Send to primary runner every 15s (every tick) if we're a secondary
            if let Some(pp) = primary_port {
                // Resolve our registration ID (cached after first successful registration)
                let reg_id = {
                    let guard = registration_id.lock().await;
                    guard.clone()
                };

                let id = match reg_id {
                    Some(id) => id,
                    None => {
                        // Try to register first
                        if let Some(id) = crate::instance::register_with_primary().await {
                            let mut guard = registration_id.lock().await;
                            *guard = Some(id.clone());
                            id
                        } else {
                            continue;
                        }
                    }
                };

                let url = format!("http://127.0.0.1:{}/instances/{}/heartbeat", pp, id);
                let body = serde_json::json!({
                    "running_task_count": task_count,
                    "running_task_ids": task_ids,
                    // Phase 3J.2 — include UI error status for supervisor
                    // aggregation. `ui_error` is null when healthy.
                    // Post-3J follow-up also includes `recent_crash` so a
                    // runner restarted after a Rust panic is visible to the
                    // primary's aggregation view.
                    "derived_status": derived_status,
                    "ui_error": ui_error_snapshot,
                    "recent_crash": recent_crash_snapshot,
                });

                match client.post(&url).json(&body).send().await {
                    Ok(resp) if resp.status().is_success() => {
                        debug!("Heartbeat sent to primary runner");
                    }
                    Ok(resp) if resp.status() == reqwest::StatusCode::NOT_FOUND => {
                        // Primary doesn't know us — clear cached ID so we re-register
                        debug!("Primary returned 404 for heartbeat — will re-register");
                        let mut guard = registration_id.lock().await;
                        *guard = None;
                    }
                    Ok(_) | Err(_) => {
                        debug!("Primary runner heartbeat failed (primary may be down)");
                    }
                }
            }
        }
    });
}

/// The heartbeat `derived_status` value a runner reports when it is
/// provisioning-incomplete under the ENFORCING gate: it has not yet met the
/// completeness bar (a live coord device JWT) so it withholds a "ready"
/// (healthy) status. Distinct from `errored`/`degraded` so consumers can tell
/// "still provisioning" apart from "broke at runtime".
pub(crate) const PROVISIONING_INCOMPLETE_STATUS: &str = "provisioning_incomplete";

/// Apply the Phase-5 provisioning gate to the base derived status.
///
/// Pure over its inputs (the side-effecting log is the one exception, and is
/// the loud-surface the plan asks for) so the status decision is unit-testable.
///
/// - [`ProvisioningReadiness::Ready`] ⇒ pass the base status through unchanged.
/// - [`ProvisioningReadiness::IncompleteAdvisory`] ⇒ log loudly, but DO NOT
///   change the status (advisory never withholds ready / never bricks a runner
///   mid-provision).
/// - [`ProvisioningReadiness::IncompleteEnforced`] ⇒ withhold ready by
///   overriding to [`PROVISIONING_INCOMPLETE_STATUS`] — UNLESS the base status
///   is already a worse signal (`errored`/`degraded`), which must win so a real
///   runtime fault is never masked by the provisioning state.
fn provisioning_gated_status(
    base_status: &'static str,
    readiness: qontinui_runner_lib::coord_doctor::ProvisioningReadiness,
    check_detail: &str,
) -> &'static str {
    use qontinui_runner_lib::coord_doctor::ProvisioningReadiness;
    match readiness {
        ProvisioningReadiness::Ready => base_status,
        ProvisioningReadiness::IncompleteAdvisory => {
            tracing::warn!(
                "provisioning gate (advisory): runner has NO live coord device JWT \
                 (doctor check device_jwt_live: {check_detail}). Reporting ready anyway; \
                 set {} to withhold ready until a device JWT is live. \
                 Run `coord doctor` for the failing link + fix.",
                qontinui_runner_lib::coord_doctor::PROVISIONING_GATE_ENFORCE_ENV,
            );
            base_status
        }
        ProvisioningReadiness::IncompleteEnforced => {
            // A real runtime fault must still win over "still provisioning".
            if base_status == "errored" || base_status == "degraded" {
                tracing::warn!(
                    "provisioning gate (enforcing): runner has NO live coord device JWT \
                     ({check_detail}), but a worse status ({base_status}) wins."
                );
                base_status
            } else {
                tracing::warn!(
                    "provisioning gate (enforcing): WITHHOLDING ready — runner has no live \
                     coord device JWT ({check_detail}). Reporting {PROVISIONING_INCOMPLETE_STATUS}. \
                     Run `coord doctor` for the fix."
                );
                PROVISIONING_INCOMPLETE_STATUS
            }
        }
    }
}

/// Get the local IP address (non-loopback).
fn get_local_ip() -> Option<String> {
    // Use a UDP socket trick to find the local IP without actually sending data
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("8.8.8.8:80").ok()?;
    let addr = socket.local_addr().ok()?;
    Some(addr.ip().to_string())
}

/// Fallback when PG query fails — returns empty.
fn get_running_tasks_fallback() -> (u32, Vec<String>) {
    (0, vec![])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Loopback bind ⇒ heartbeat advertises `lan_reachable: false`, with the
    /// flag DERIVED from a listener's real bound address through the same
    /// seam `mcp_api::start_server` uses (`lan_reachable_for_bound_addr`),
    /// not hardcoded. Also asserts the payload stays backward compatible:
    /// the pre-existing keys are still emitted alongside the new field.
    #[test]
    fn payload_serializes_lan_reachable_false_when_bound_to_loopback() {
        // Bind exactly like the runner API does today: loopback only.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().expect("local_addr");
        let lan_reachable = crate::mcp_api::lan_reachable_for_bound_addr(&addr);

        let payload = HeartbeatPayload {
            hostname: "test-host".to_string(),
            // The advertised LAN IP — the half that made the old payload
            // dishonest when paired with a loopback-only bind.
            ip: "192.168.8.192".to_string(),
            port: addr.port(),
            lan_reachable,
            instance_name: None,
            os: "windows".to_string(),
            os_version: None,
            running_task_count: 0,
            running_task_ids: vec![],
            derived_status: "healthy".to_string(),
            ui_error: None,
            recent_crash: None,
        };

        let json = serde_json::to_value(&payload).expect("payload serializes");
        assert_eq!(json["lan_reachable"], serde_json::Value::Bool(false));
        // Backward compatibility: existing wire keys unchanged.
        assert_eq!(json["ip"], "192.168.8.192");
        assert_eq!(json["port"], addr.port());
        assert_eq!(json["hostname"], "test-host");
        assert_eq!(json["derived_status"], "healthy");
        assert!(json.as_object().unwrap().contains_key("ui_error"));
    }

    /// The derivation must flip to `true` for a non-loopback bind (all-
    /// interfaces or a concrete LAN address) so a future bind-strategy
    /// change is advertised honestly without touching the heartbeat.
    #[test]
    fn lan_reachable_derivation_flips_true_for_non_loopback_bind() {
        let unspecified: std::net::SocketAddr = "0.0.0.0:9876".parse().unwrap();
        assert!(crate::mcp_api::lan_reachable_for_bound_addr(&unspecified));

        let lan: std::net::SocketAddr = "192.168.8.192:9876".parse().unwrap();
        assert!(crate::mcp_api::lan_reachable_for_bound_addr(&lan));

        let loopback: std::net::SocketAddr = "127.0.0.1:9876".parse().unwrap();
        assert!(!crate::mcp_api::lan_reachable_for_bound_addr(&loopback));

        let v6_loopback: std::net::SocketAddr = "[::1]:9876".parse().unwrap();
        assert!(!crate::mcp_api::lan_reachable_for_bound_addr(&v6_loopback));
    }

    // ---- Phase 5 — provisioning gate applied to derived_status ----

    use qontinui_runner_lib::coord_doctor::ProvisioningReadiness;

    #[test]
    fn provisioning_gate_ready_passes_base_status_through() {
        // A live device JWT ⇒ no change to whatever the base status was.
        assert_eq!(
            provisioning_gated_status("healthy", ProvisioningReadiness::Ready, "ok"),
            "healthy"
        );
        assert_eq!(
            provisioning_gated_status("degraded", ProvisioningReadiness::Ready, "ok"),
            "degraded"
        );
    }

    #[test]
    fn provisioning_gate_advisory_does_not_change_status() {
        // Advisory surfaces (logs) but never withholds ready — base passes through.
        assert_eq!(
            provisioning_gated_status(
                "healthy",
                ProvisioningReadiness::IncompleteAdvisory,
                "no device JWT in the access-token slot",
            ),
            "healthy",
            "advisory must NOT withhold ready"
        );
    }

    #[test]
    fn provisioning_gate_enforced_withholds_ready() {
        // Enforcing + incomplete + otherwise-healthy ⇒ provisioning_incomplete.
        assert_eq!(
            provisioning_gated_status(
                "healthy",
                ProvisioningReadiness::IncompleteEnforced,
                "no device JWT in the access-token slot",
            ),
            PROVISIONING_INCOMPLETE_STATUS
        );
    }

    #[test]
    fn provisioning_gate_enforced_lets_worse_status_win() {
        // A real runtime fault (errored/degraded) must NOT be masked by the
        // provisioning-incomplete override.
        assert_eq!(
            provisioning_gated_status("errored", ProvisioningReadiness::IncompleteEnforced, "x"),
            "errored"
        );
        assert_eq!(
            provisioning_gated_status("degraded", ProvisioningReadiness::IncompleteEnforced, "x"),
            "degraded"
        );
    }
}

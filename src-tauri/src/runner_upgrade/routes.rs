//! The two HTTP doors (plan
//! `2026-11-24-runner-upgrades-itself-daily-keeping-sessions-alive-with-no-outage-and-automatic-rollback`,
//! Phase 5):
//!
//! - `GET /runner/upgrade/preflight?candidate=<abs dir>[&rollback=true]` —
//!   the verdict, every check with its detail. Always `200` with the verdict
//!   in the body (`ok`, `verdict`, `blocking`); `400` only for a request that
//!   names no candidate. It RUNS the candidate's binaries (`--provenance`,
//!   `--protocol-versions`), so it is a credential door.
//! - `POST /runner/upgrade {"candidate", "rollback"?, "guardUnit"?,
//!   "guardDelaySecs"?}` — re-runs the preflight and refuses with `409` and the
//!   verdict unless it passes; then the executor (record, marker, guard, swap,
//!   handoff exit) and `202` with the record. The process exits shortly after
//!   answering. Credential door; a JSON body is required (`415` otherwise), so
//!   a cross-site form post cannot reach it.
//!
//! Both are classified in `mcp::origin_guard::CREDENTIAL_DOORS`. Who may CALL
//! the upgrade verb is served policy (`production-and-cost`
//! `runner-self-upgrade`), not this module.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use tracing::{error, info, warn};

use super::executor::{self, ExecuteParams, UpgradeEffects};
use super::preflight::{self, Layout, PreflightInputs, PreflightReport};
use super::rehome::{self, RehomeOutcome};
use super::sys::{self, CommandRunner, RealCommandRunner};
use crate::mcp::types::ApiState;

/// Routes contributed by this module; merged in `mcp_api.rs`.
pub fn routes() -> Router<Arc<ApiState>> {
    Router::new()
        .route("/runner/upgrade/preflight", get(get_preflight))
        .route("/runner/upgrade", post(post_upgrade))
}

/// One upgrade at a time per process.
static UPGRADE_RUNNING: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Deserialize)]
struct PreflightQuery {
    candidate: Option<String>,
    #[serde(default)]
    rollback: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UpgradeBody {
    candidate: String,
    #[serde(default)]
    rollback: Option<bool>,
    /// The transient guard unit's name (a scratch upgrade names its own).
    #[serde(default)]
    guard_unit: Option<String>,
    #[serde(default)]
    guard_delay_secs: Option<u64>,
}

/// The bounds on a requested guard delay: long enough for a boot and its
/// probation, short enough that a dead successor is not left serving nothing.
const GUARD_DELAY_MIN: u64 = 30;
const GUARD_DELAY_MAX: u64 = 1800;

fn refusal(
    status: StatusCode,
    msg: impl Into<String>,
    data: Option<Value>,
) -> (StatusCode, Json<Value>) {
    let mut body = json!({"success": false, "error": msg.into()});
    if let Some(d) = data {
        body["data"] = d;
    }
    (status, Json(body))
}

async fn get_preflight(
    State(state): State<Arc<ApiState>>,
    Query(q): Query<PreflightQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let candidate = q
        .candidate
        .filter(|c| !c.trim().is_empty())
        .ok_or_else(|| {
            refusal(
                StatusCode::BAD_REQUEST,
                "?candidate=<absolute promoted dir> is required",
                None,
            )
        })?;
    let report = gather(
        &state,
        PathBuf::from(candidate),
        q.rollback.unwrap_or(false),
    )
    .await
    .map_err(|e| refusal(StatusCode::SERVICE_UNAVAILABLE, e, None))?;
    Ok(Json(json!({"success": true, "data": report})))
}

async fn post_upgrade(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> Result<(StatusCode, Json<Value>), (StatusCode, Json<Value>)> {
    let json_ct = headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|v| {
            v.split(';')
                .next()
                .unwrap_or("")
                .trim()
                .eq_ignore_ascii_case("application/json")
        })
        .unwrap_or(false);
    if !json_ct {
        return Err(refusal(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "Content-Type must be application/json — nothing was done",
            None,
        ));
    }
    let req: UpgradeBody = serde_json::from_slice(&body).map_err(|e| {
        refusal(
            StatusCode::BAD_REQUEST,
            format!("body must be {{candidate, rollback?, guardUnit?, guardDelaySecs?}}: {e}"),
            None,
        )
    })?;
    let delay = req
        .guard_delay_secs
        .unwrap_or(executor::DEFAULT_GUARD_DELAY.as_secs());
    if !(GUARD_DELAY_MIN..=GUARD_DELAY_MAX).contains(&delay) {
        return Err(refusal(
            StatusCode::BAD_REQUEST,
            format!("guardDelaySecs must be {GUARD_DELAY_MIN}..={GUARD_DELAY_MAX}"),
            None,
        ));
    }
    if let Some(u) = &req.guard_unit {
        if !executor::valid_unit_name(u) {
            return Err(refusal(
                StatusCode::BAD_REQUEST,
                format!("{u:?} is not a valid unit name"),
                None,
            ));
        }
    }
    if UPGRADE_RUNNING.swap(true, Ordering::SeqCst) {
        return Err(refusal(
            StatusCode::CONFLICT,
            "an upgrade is already running in this process",
            None,
        ));
    }
    let result = run_upgrade(&state, req, Duration::from_secs(delay)).await;
    if result.is_err() {
        UPGRADE_RUNNING.store(false, Ordering::SeqCst);
    }
    result
}

async fn run_upgrade(
    state: &Arc<ApiState>,
    req: UpgradeBody,
    delay: Duration,
) -> Result<(StatusCode, Json<Value>), (StatusCode, Json<Value>)> {
    let report = gather(
        state,
        PathBuf::from(&req.candidate),
        req.rollback.unwrap_or(false),
    )
    .await
    .map_err(|e| refusal(StatusCode::SERVICE_UNAVAILABLE, e, None))?;
    if !report.ok {
        warn!(blocking = ?report.blocking, "runner upgrade refused: the preflight did not pass");
        return Err(refusal(
            StatusCode::CONFLICT,
            format!("the preflight did not pass: {}", report.blocking.join(", ")),
            serde_json::to_value(&report).ok(),
        ));
    }
    let port = state.app_state.api_port.load(Ordering::Relaxed);
    let fx = LiveUpgradeEffects {
        runner: RealCommandRunner,
        app: state.app_handle.clone(),
        app_state: state.app_state.clone(),
    };
    let dir = sys::upgrades_dir();
    let listener_fds = super::listener_fds::registered();
    let guard_unit = req.guard_unit.clone();
    let executed = tokio::task::spawn_blocking(move || {
        executor::execute(
            ExecuteParams {
                report: &report,
                upgrades_dir: &dir,
                api_port: port,
                from_pid: std::process::id(),
                guard_unit,
                guard_delay: delay,
                listener_fds,
                now: chrono::Utc::now(),
            },
            &fx,
        )
    })
    .await
    .map_err(|e| {
        refusal(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("the executor task failed: {e}"),
            None,
        )
    })?;
    match executed {
        Ok(done) => {
            info!(
                id = %done.record.id,
                from = %done.record.from_sha,
                to = %done.record.to_sha,
                "runner upgrade: handoff exit staged"
            );
            Ok((
                StatusCode::ACCEPTED,
                Json(json!({
                    "success": true,
                    "data": {
                        "record": done.record_path,
                        "upgrade": done.record,
                        "message": "the guard is armed and current points at the candidate; this runner exits \
                                    now and its successor proves itself within the guard window",
                    }
                })),
            ))
        }
        Err(e) => {
            error!(error = %e.message, "runner upgrade aborted before the exit");
            Err(refusal(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!(
                    "aborted before the exit, nothing changed hands: {}",
                    e.message
                ),
                Some(json!({"record": e.record_path})),
            ))
        }
    }
}

/// Gather every input and run the preflight.
pub async fn gather(
    state: &Arc<ApiState>,
    candidate: PathBuf,
    rollback: bool,
) -> Result<PreflightReport, String> {
    use tauri::Manager;
    let layout = Layout::resolve()?;
    let store = state
        .app_handle
        .try_state::<Arc<crate::session::session_lifecycle_store::SessionLifecycleStore>>()
        .map(|s| s.inner().clone());
    let config_dir_of = move |id: &str| {
        store
            .as_ref()
            .and_then(|s| s.get(id))
            .and_then(|r| r.config_dir)
    };

    let readiness = crate::mcp::restart_readiness::compute_restart_readiness(state).await;
    let mut census = preflight::census_facts(&readiness, &config_dir_of);
    let rehome = match &census {
        Ok(c) => rehome::rehome(rehome::active_rehomer().as_ref(), &c.dies_with_runner).await,
        Err(_) => RehomeOutcome::NothingToRehome,
    };
    // A rehome changed where sessions run: the census is re-read so the
    // verdict describes the box as it now is.
    if matches!(&rehome, RehomeOutcome::Report(r) if !r.rehomed.is_empty()) {
        let fresh = crate::mcp::restart_readiness::compute_restart_readiness(state).await;
        census = preflight::census_facts(&fresh, &config_dir_of);
    }

    let port = state.app_state.api_port.load(Ordering::Relaxed);
    let listener_api = read_own_health(port).await.map(|body| {
        body.get("listenerInheritance")
            .or_else(|| body.get("data").and_then(|d| d.get("listenerInheritance")))
            .and_then(|l| l.get("api"))
            .and_then(Value::as_str)
            .map(str::to_string)
    });
    let pg_schema = super::pg_schema::fingerprint().await;
    let serving_sha = env!("QONTINUI_GIT_SHA").to_string();
    let systemd_unit = sys::own_systemd_user_unit();
    let self_managed_fds = super::listener_fds::registered().len();
    let checked_at = super::record::rfc3339(chrono::Utc::now());
    tokio::task::spawn_blocking(move || {
        preflight::evaluate(
            PreflightInputs {
                candidate: &candidate,
                rollback,
                serving_sha: &serving_sha,
                layout: &layout,
                census,
                rehome,
                listener_api,
                systemd_unit,
                self_managed_fds,
                pg_schema,
                checked_at,
            },
            &RealCommandRunner,
        )
    })
    .await
    .map_err(|e| format!("the preflight task failed: {e}"))
}

/// This runner's own `/health` body, over loopback.
pub async fn read_own_health(port: u16) -> Result<Value, String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .no_proxy()
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client
        .get(format!("http://127.0.0.1:{port}/health"))
        .send()
        .await
        .map_err(|e| format!("GET /health: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("GET /health: {}", resp.status()));
    }
    resp.json::<Value>()
        .await
        .map_err(|e| format!("GET /health body: {e}"))
}

/// The real executor effects of this process.
struct LiveUpgradeEffects {
    runner: RealCommandRunner,
    app: tauri::AppHandle,
    app_state: Arc<crate::commands::AppState>,
}

impl UpgradeEffects for LiveUpgradeEffects {
    fn commands(&self) -> &dyn CommandRunner {
        &self.runner
    }
    fn spawn_detached(&self, exe: &Path, args: &[String]) -> Result<u32, String> {
        super::spawn::spawn_detached(exe, args)
    }
    fn spawn_successor(
        &self,
        exe: &Path,
        args: &[String],
        fds: &[(String, i32)],
    ) -> Result<u32, String> {
        super::spawn::spawn_successor(exe, args, fds)
    }
    fn swap_link(&self, link: &Path, target: &Path) -> Result<(), String> {
        sys::swap_link_atomically(link, target).map_err(|e| e.to_string())
    }
    /// Stage `CloseMode::Handoff` in `AppState::pending_close_mode` and close
    /// the main window — the same in-process sequence the close-request door
    /// runs, without calling that door. Scheduled a moment later so the
    /// `202` reaches the caller first. With no main window (headless), or a
    /// close that fails, the same handoff teardown runs through the
    /// force-close path, which takes the mode directly.
    fn stage_handoff_exit(&self) -> Result<(), String> {
        let app = self.app.clone();
        let app_state = self.app_state.clone();
        std::thread::Builder::new()
            .name("runner-upgrade-exit".into())
            .spawn(move || {
                use tauri::Manager;
                std::thread::sleep(Duration::from_millis(750));
                let mode = crate::terminal::CloseMode::Handoff;
                let reason = "runner upgrade: handoff exit to the successor".to_string();
                match app.get_webview_window(qontinui_runner_lib::get_main_window_label()) {
                    Some(window) => {
                        let armed = app_state.pending_close_mode.arm(mode);
                        match window.close() {
                            Ok(()) => armed.commit(),
                            Err(e) => {
                                drop(armed);
                                warn!(error = %e, "runner upgrade: window close failed; force-closing with the handoff");
                                crate::mcp::ai_session::emergency_quit::request_force_close(app, app_state, reason, mode);
                            }
                        }
                    }
                    None => {
                        crate::mcp::ai_session::emergency_quit::request_force_close(app, app_state, reason, mode);
                    }
                }
            })
            .map(|_| ())
            .map_err(|e| format!("could not start the exit thread: {e}"))
    }
}

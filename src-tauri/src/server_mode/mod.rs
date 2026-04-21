//! Web-backend integration (Phase 3G: settings-driven).
//!
//! When [`WebIntegrationSettings::enabled`] is true and `backend_url` +
//! `runner_token` are both non-empty, this module:
//!
//! 1. Registers the runner with `POST /api/v1/runners/register` (with
//!    exponential backoff until success).
//! 2. Captures the assigned `runner_id` in `ServerModeState` so later
//!    phase-result posts and heartbeats can reference it.
//! 3. Spawns a heartbeat loop that sends `POST /api/v1/runners/{id}/heartbeat`
//!    every 30 seconds.
//! 4. Exposes `post_phase_result` for fire-and-forget posting of
//!    `PhaseResult` payloads from the workflow executor.
//!
//! All HTTP failures are non-fatal. The runner continues to execute locally
//! even when the web backend is unreachable — events are still persisted to
//! PG and can be backfilled later.
//!
//! # Env var compatibility
//!
//! `QONTINUI_WEB_BACKEND_URL` + `QONTINUI_RUNNER_TOKEN` still work: the
//! settings loader applies them as an in-memory overlay at boot, defaulting
//! `enabled` to true when both are present. Desktop UIs can also persist
//! this state via [`WebIntegrationSettings`] without env vars.
//!
//! # Hot reload
//!
//! [`ServerModeState::shutdown`] signals background tasks to exit via an
//! `Arc<AtomicBool>`. Call it from the settings-save command before swapping
//! in a freshly-constructed state; the old heartbeat/registration tasks
//! observe the flag and return cleanly.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::settings::WebIntegrationSettings;
use crate::unified_workflow_executor::types::PhaseResult;

pub mod token_flow;

pub use token_flow::{PendingTokenFlow, TokenFlowStore};

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Resolved configuration for web-backend integration.
///
/// Produced from [`WebIntegrationSettings`] via [`ServerModeConfig::from_settings`].
/// Callers that see `None` should skip all web-side reporting (phase events,
/// registration, heartbeats).
#[derive(Debug, Clone)]
pub struct ServerModeConfig {
    /// Base URL of the qontinui-web API, e.g. `"https://api.qontinui.io"`.
    /// No trailing slash — callers append `/api/v1/...`.
    pub web_backend_url: String,
    /// Plaintext runner bearer token (`qontinui_runner_<64hex>`).
    /// Never logged.
    pub runner_token: String,
}

impl ServerModeConfig {
    /// Build from the persisted [`WebIntegrationSettings`].
    ///
    /// Returns `None` unless `enabled=true` AND `backend_url` is non-empty
    /// AND `runner_token` is non-empty. Trims a trailing slash from
    /// `backend_url` so callers can safely append `/api/v1/...` paths.
    pub fn from_settings(settings: &WebIntegrationSettings) -> Option<Self> {
        if !settings.enabled {
            return None;
        }
        let backend_url = settings.backend_url.trim();
        let runner_token = settings.runner_token.trim();
        if backend_url.is_empty() || runner_token.is_empty() {
            return None;
        }
        Some(Self {
            web_backend_url: backend_url.trim_end_matches('/').to_string(),
            runner_token: runner_token.to_string(),
        })
    }
}

/// Shared state for the server-mode integration.
///
/// Holds the assigned `runner_id` (populated after a successful registration),
/// the `dispatch_secret` (a per-runner bearer value returned by web at
/// registration time), and the `ServerModeConfig` itself. Clones cheaply
/// (`Arc` under the hood).
///
/// `dispatch_secret` is a machine-to-machine credential web uses when it
/// POSTs to `/api/workflows/run` on this runner — distinct from
/// `config.runner_token` (which the runner uses to authenticate *to* web).
/// The runner accepts either value as a bearer on `/api/workflows/run`.
#[derive(Debug, Clone)]
pub struct ServerModeState {
    pub config: ServerModeConfig,
    runner_id: Arc<RwLock<Option<Uuid>>>,
    dispatch_secret: Arc<RwLock<Option<String>>>,
    /// ISO-8601 timestamp of the last successful heartbeat. Populated by the
    /// heartbeat loop; read by the `get_web_integration_status` command.
    last_heartbeat_at: Arc<RwLock<Option<String>>>,
    /// Last registration error message (from `register_with_retry`). Cleared
    /// to `None` on successful registration.
    registration_error: Arc<RwLock<Option<String>>>,
    /// Shutdown flag for hot-reload: when settings change, the settings-save
    /// command calls [`ServerModeState::shutdown`] to flip this to `true`;
    /// the registration retry and heartbeat loops check between iterations
    /// and return cleanly.
    shutdown: Arc<AtomicBool>,
}

impl ServerModeState {
    pub fn new(config: ServerModeConfig) -> Self {
        Self {
            config,
            runner_id: Arc::new(RwLock::new(None)),
            dispatch_secret: Arc::new(RwLock::new(None)),
            last_heartbeat_at: Arc::new(RwLock::new(None)),
            registration_error: Arc::new(RwLock::new(None)),
            shutdown: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Current runner_id (if registration has succeeded at least once).
    pub async fn runner_id(&self) -> Option<Uuid> {
        *self.runner_id.read().await
    }

    /// Set the runner_id. Called exactly once from the registration task.
    async fn set_runner_id(&self, id: Uuid) {
        let mut guard = self.runner_id.write().await;
        *guard = Some(id);
    }

    /// Current dispatch_secret (if registration has succeeded at least once
    /// *and* the web backend returned one). Returns `None` until the first
    /// successful registration response has been parsed.
    pub async fn dispatch_secret(&self) -> Option<String> {
        self.dispatch_secret.read().await.clone()
    }

    /// Set the dispatch_secret. Called once from the registration task on the
    /// first successful registration response.
    async fn set_dispatch_secret(&self, secret: String) {
        let mut guard = self.dispatch_secret.write().await;
        *guard = Some(secret);
    }

    /// Timestamp (ISO-8601) of the last successful heartbeat. `None` when the
    /// heartbeat loop has not yet recorded a success.
    pub async fn last_heartbeat_at(&self) -> Option<String> {
        self.last_heartbeat_at.read().await.clone()
    }

    async fn set_last_heartbeat_at(&self, ts: String) {
        let mut guard = self.last_heartbeat_at.write().await;
        *guard = Some(ts);
    }

    /// Most recent registration error, or `None` if the last attempt
    /// succeeded (or none has been made yet).
    pub async fn registration_error(&self) -> Option<String> {
        self.registration_error.read().await.clone()
    }

    async fn set_registration_error(&self, err: Option<String>) {
        let mut guard = self.registration_error.write().await;
        *guard = err;
    }

    /// Signal the background tasks to exit cleanly. After calling this the
    /// state can be dropped; the registration retry loop and heartbeat loop
    /// will observe the flag on their next iteration and return.
    pub fn shutdown(&self) {
        self.shutdown.store(true, Ordering::Relaxed);
    }

    fn is_shutting_down(&self) -> bool {
        self.shutdown.load(Ordering::Relaxed)
    }
}

// ---------------------------------------------------------------------------
// Startup: registration + heartbeat
// ---------------------------------------------------------------------------

/// Registration payload. Mirrors `app.schemas.runner_fleet.RunnerRegistrationRequest`.
#[derive(Debug, Serialize)]
struct RegistrationRequest {
    name: String,
    hostname: String,
    port: u16,
    capabilities: Vec<String>,
    server_mode: bool,
    restate_enabled: bool,
    restate_healthy: bool,
}

#[derive(Debug, Deserialize)]
struct RegistrationResponse {
    runner_id: Uuid,
    #[serde(default)]
    #[allow(dead_code)]
    registered_at: Option<String>,
    /// Per-runner machine-to-machine secret the web backend signs dispatch
    /// requests with. Optional for forward-compatibility: if a future web
    /// version omits it the runner simply falls back to verifying
    /// `QONTINUI_RUNNER_TOKEN` on dispatch calls.
    #[serde(default)]
    dispatch_secret: Option<String>,
}

#[derive(Debug, Serialize)]
struct HeartbeatRequest {
    restate_healthy: bool,
    status: String,
    /// Phase 3J.2 — `"healthy"` if no UI error is tracked, `"errored"`
    /// when the React error boundary has reported an unhandled error.
    /// Distinct from `status` (which tracks Restate health) so the
    /// qontinui-web fleet page can surface both independently.
    derived_status: String,
    /// Phase 3J.2 — latest UI error snapshot from the runner's React error
    /// boundary. Serialized as `null` when healthy (the key is always
    /// present so consumers can assume the shape).
    ui_error: Option<crate::ui_error::UiError>,
}

fn build_http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

fn get_hostname() -> String {
    hostname::get()
        .map(|h| h.to_string_lossy().to_string())
        .unwrap_or_else(|_| "unknown".to_string())
}

fn get_runner_name() -> String {
    std::env::var("QONTINUI_INSTANCE_NAME").unwrap_or_else(|_| "primary".to_string())
}

fn get_runner_port() -> u16 {
    std::env::var("QONTINUI_PORT")
        .ok()
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(9876)
}

/// Spawn the web-integration registration + heartbeat tasks on the tauri
/// async runtime.
///
/// Call this from `.setup()` (i.e. after the tauri runtime is up) or from
/// the web-integration settings-save command during hot-reload. The state
/// itself is constructed via [`ServerModeState::new`] and stored on
/// `AppState`; `spawn_background_tasks` takes a clone and drives the HTTP
/// work in the background. The registration task fills in `runner_id`
/// asynchronously, then chains into the heartbeat loop so the heartbeat
/// never runs without a registered id. Both loops observe
/// [`ServerModeState::shutdown`] and exit cleanly when it is set.
///
/// `restate_enabled` is captured up front from the live `RestateSettings`;
/// registration reports the static value (the dynamic health is reported on
/// each heartbeat instead).
pub fn spawn_background_tasks(
    state: ServerModeState,
    restate_enabled: bool,
    ui_error_state: Arc<crate::ui_error::UiErrorState>,
) {
    let state_for_register = state.clone();
    tauri::async_runtime::spawn(async move {
        register_with_retry(&state_for_register, restate_enabled).await;
        // Abort the chain if shutdown happened during registration.
        if state_for_register.is_shutting_down() {
            return;
        }
        // Once registered, start heartbeats. We chain them so the heartbeat
        // loop never runs without a runner_id (which would 404 every tick).
        run_heartbeat_loop(state_for_register, restate_enabled, ui_error_state).await;
    });
}

/// Register with exponential backoff. Retries until success or shutdown —
/// the runner runs locally even when registration keeps failing, so blocking
/// workflow execution here is wrong.
///
/// Records the latest error via [`ServerModeState::set_registration_error`]
/// on each non-2xx/network failure; clears it on success.
async fn register_with_retry(state: &ServerModeState, restate_enabled: bool) {
    let client = build_http_client();
    let url = format!("{}/api/v1/runners/register", state.config.web_backend_url);
    let payload = RegistrationRequest {
        name: get_runner_name(),
        hostname: get_hostname(),
        port: get_runner_port(),
        capabilities: vec![
            "gui_automation".to_string(),
            "accessibility".to_string(),
            "restate".to_string(),
        ],
        server_mode: true,
        restate_enabled,
        // restate_healthy=false at registration; the first heartbeat updates it.
        restate_healthy: false,
    };

    let mut delay = Duration::from_secs(2);
    let max_delay = Duration::from_secs(300); // 5 min cap

    loop {
        if state.is_shutting_down() {
            debug!("Server-mode registration: shutdown requested, exiting loop");
            return;
        }

        match client
            .post(&url)
            .bearer_auth(&state.config.runner_token)
            .json(&payload)
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => {
                match resp.json::<RegistrationResponse>().await {
                    Ok(body) => {
                        state.set_runner_id(body.runner_id).await;
                        let has_secret = body.dispatch_secret.is_some();
                        if let Some(secret) = body.dispatch_secret {
                            state.set_dispatch_secret(secret).await;
                        }
                        state.set_registration_error(None).await;
                        info!(
                            runner_id = %body.runner_id,
                            name = %payload.name,
                            dispatch_secret_received = has_secret,
                            "Web-integration runner registered with web backend"
                        );
                        return;
                    }
                    Err(e) => {
                        let msg = format!("2xx body did not decode as RegistrationResponse: {}", e);
                        warn!("Server-mode registration: {}", msg);
                        state.set_registration_error(Some(msg)).await;
                    }
                }
            }
            Ok(resp) => {
                let status = resp.status();
                let body_preview = resp
                    .text()
                    .await
                    .unwrap_or_else(|_| "<no body>".to_string());
                let msg = format!("HTTP {}: {}", status, truncate(&body_preview, 200));
                warn!("Server-mode registration failed: {}", msg);
                state.set_registration_error(Some(msg)).await;
            }
            Err(e) => {
                let msg = format!("network error: {}", e);
                warn!("Server-mode registration {}", msg);
                state.set_registration_error(Some(msg)).await;
            }
        }

        // Sleep with shutdown-responsive polling so hot-reload isn't blocked
        // by the next retry delay (can reach 5 minutes).
        let mut slept = Duration::from_secs(0);
        let poll = Duration::from_millis(250);
        while slept < delay {
            if state.is_shutting_down() {
                return;
            }
            tokio::time::sleep(poll).await;
            slept += poll;
        }
        // Exponential backoff (2s -> 4s -> 8s -> ... -> 300s cap).
        delay = std::cmp::min(delay.saturating_mul(2), max_delay);
    }
}

/// Heartbeat loop: every 30s while `runner_id` is set. Network errors warn
/// but never back off — a missed heartbeat is fine. Exits when
/// [`ServerModeState::shutdown`] has been called.
async fn run_heartbeat_loop(
    state: ServerModeState,
    restate_enabled: bool,
    ui_error_state: Arc<crate::ui_error::UiErrorState>,
) {
    let client = build_http_client();
    let mut ticker = tokio::time::interval(Duration::from_secs(30));
    // Skip the immediate tick — we just registered.
    ticker.tick().await;

    loop {
        ticker.tick().await;

        if state.is_shutting_down() {
            debug!("Server-mode heartbeat: shutdown requested, exiting loop");
            return;
        }

        let runner_id = match state.runner_id().await {
            Some(id) => id,
            None => {
                // Registration task hasn't landed yet; skip this tick.
                debug!("Server-mode heartbeat: runner_id not yet set, skipping tick");
                continue;
            }
        };

        let restate_healthy = if restate_enabled {
            // Cheap probe of the locally-configured admin plane.
            let settings = crate::settings::load_settings();
            crate::restate::lifecycle::is_restate_healthy_for(&settings.restate).await
        } else {
            false
        };

        let status = if restate_enabled && !restate_healthy {
            "unhealthy"
        } else {
            "healthy"
        };

        // Phase 3J.2 — snapshot the UI error state for this tick. The
        // `derived_status` reported here is specifically about UI health,
        // independent of Restate's `status` field above.
        let ui_error_snapshot = ui_error_state.get().await;
        let derived_status = if ui_error_snapshot.is_some() {
            "errored".to_string()
        } else {
            "healthy".to_string()
        };

        let url = format!(
            "{}/api/v1/runners/{}/heartbeat",
            state.config.web_backend_url, runner_id
        );
        let body = HeartbeatRequest {
            restate_healthy,
            status: status.to_string(),
            derived_status,
            ui_error: ui_error_snapshot,
        };

        match client
            .post(&url)
            .bearer_auth(&state.config.runner_token)
            .json(&body)
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => {
                debug!("Server-mode heartbeat sent (status={})", status);
                let ts = chrono::Utc::now().to_rfc3339();
                state.set_last_heartbeat_at(ts).await;
            }
            Ok(resp) => {
                let s = resp.status();
                let t = resp
                    .text()
                    .await
                    .unwrap_or_else(|_| "<no body>".to_string());
                warn!(
                    "Server-mode heartbeat failed ({}): {}",
                    s,
                    truncate(&t, 200)
                );
            }
            Err(e) => {
                warn!("Server-mode heartbeat network error: {}", e);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Phase-result POSTs
// ---------------------------------------------------------------------------

/// Payload for `POST /api/v1/events/phase-completed`.
///
/// Mirrors `app.schemas.phase_result.PhaseResultIngestRequest`. Flattens the
/// `PhaseResult` fields directly rather than boxing under a `"result"` key
/// so Pydantic's validator sees them at the top level.
#[derive(Debug, Serialize)]
struct PhaseResultIngest<'a> {
    /// Optional: when set, the web backend attributes the row to this
    /// specific runner instead of the "most-recently-heartbeated" fallback.
    #[serde(skip_serializing_if = "Option::is_none")]
    runner_id: Option<Uuid>,
    execution_id: &'a str,
    phase: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    iteration: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stage_index: Option<u32>,
    success: bool,
    all_passed: bool,
    duration_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    failure_context: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    commit_hash: Option<&'a str>,
    step_results: &'a [crate::unified_workflow_executor::types::StepResultRecord],
    #[serde(skip_serializing_if = "Option::is_none")]
    variables_set: Option<Value>,
}

/// Fire-and-forget POST of a `PhaseResult` to the web backend.
///
/// Spawns a tokio task internally so the caller — `emit_and_persist_phase_result`
/// — never blocks on web latency. On non-2xx or network error the call just
/// logs a warning; local persistence already happened upstream.
pub fn post_phase_result(state: ServerModeState, execution_id: String, result: PhaseResult) {
    tokio::spawn(async move {
        let runner_id = state.runner_id().await;
        if runner_id.is_none() {
            debug!(
                "Server-mode phase-result post skipped (runner_id not yet registered): execution_id={}",
                execution_id
            );
            return;
        }

        let url = format!(
            "{}/api/v1/events/phase-completed",
            state.config.web_backend_url
        );

        // Serialize `variables_set` as JSON. The web schema accepts either
        // list-of-pairs or dict; the Rust shape is `Vec<(String, String)>`
        // which serializes to list-of-pairs by default.
        let variables_set = result
            .variables_set
            .as_ref()
            .and_then(|v| serde_json::to_value(v).ok());

        let payload = PhaseResultIngest {
            runner_id,
            execution_id: &execution_id,
            phase: &result.phase,
            iteration: result.iteration,
            stage_index: result.stage_index,
            success: result.success,
            all_passed: result.all_passed,
            duration_ms: result.duration_ms,
            failure_context: result.failure_context.as_deref(),
            commit_hash: result.commit_hash.as_deref(),
            step_results: &result.step_results,
            variables_set,
        };

        let client = build_http_client();
        match client
            .post(&url)
            .bearer_auth(&state.config.runner_token)
            .json(&payload)
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => {
                debug!(
                    "Server-mode phase-result posted: execution_id={} phase={}",
                    execution_id, result.phase
                );
            }
            Ok(resp) => {
                let status = resp.status();
                let body = resp
                    .text()
                    .await
                    .unwrap_or_else(|_| "<no body>".to_string());
                warn!(
                    "Server-mode phase-result post failed ({}) for execution_id={} phase={}: {}",
                    status,
                    execution_id,
                    result.phase,
                    truncate(&body, 200)
                );
            }
            Err(e) => {
                warn!(
                    "Server-mode phase-result post network error for execution_id={} phase={}: {}",
                    execution_id, result.phase, e
                );
            }
        }
    });
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Truncate a string at a char-boundary-safe byte limit.
fn truncate(s: &str, max_len: usize) -> &str {
    if s.len() <= max_len {
        return s;
    }
    let mut end = max_len;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

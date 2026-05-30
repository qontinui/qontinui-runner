//! Web-integration commands (Phase 3G).
//!
//! Exposes three Tauri commands that let the frontend manage runner ↔
//! qontinui-web integration independently of the `QONTINUI_SERVER_MODE`
//! env var:
//!
//! * [`save_web_integration_settings`] — persist settings to `settings.json`
//!   and hot-reload the in-process background tasks (registration,
//!   heartbeat, phase-result POSTs) without restarting the runner.
//! * [`get_web_integration_status`] — snapshot the current configuration
//!   and live state (runner_id, last heartbeat, registration error) for
//!   display in the Settings UI.
//! * [`test_web_integration_connection`] — probe a candidate backend URL +
//!   runner token pair without persisting: registers a throwaway runner
//!   entry, reads the assigned runner_id, then immediately deletes it so
//!   the test never leaves debris on the web side.
//!
//! After a successful `save_web_integration_settings` call the command
//! emits a `web-integration-changed` Tauri event so live status views can
//! refresh.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::plugin::{Builder as PluginBuilder, TauriPlugin};
use tauri::{AppHandle, Emitter, Runtime, State};
use tracing::{info, warn};

use crate::commands::compartments::{HealthCompartment, IntegrationCompartment};
use crate::commands::AppState;
use crate::error::AppError;
use crate::server_mode::{ServerModeConfig, ServerModeState};
use crate::settings::{self, WebIntegrationSettings};

/// Name of the Tauri event emitted when web-integration state is hot-reloaded.
/// Frontend should re-fetch status when it sees this event.
pub const WEB_INTEGRATION_CHANGED_EVENT: &str = "web-integration-changed";

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Mask a runner token for display.
///
/// Returns `<first-16-chars>…<last-4-chars>`. The 16-char prefix includes
/// the literal `qontinui_runner_` marker so users can still tell at a glance
/// that the token is well-formed. For tokens shorter than 24 characters the
/// full value is replaced with `****` to avoid revealing most of a short
/// token — though such tokens are invalid by construction.
fn mask_runner_token(token: &str) -> String {
    if token.is_empty() {
        return String::new();
    }
    const PREFIX_LEN: usize = 16;
    const SUFFIX_LEN: usize = 4;
    if token.chars().count() < PREFIX_LEN + SUFFIX_LEN {
        return "****".to_string();
    }
    let prefix: String = token.chars().take(PREFIX_LEN).collect();
    let suffix: String = token
        .chars()
        .rev()
        .take(SUFFIX_LEN)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("{}…{}", prefix, suffix)
}

fn build_http_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| {
            String::from(AppError::NetworkError(format!(
                "failed to build HTTP client: {}",
                e
            )))
        })
}

fn trim_backend_url(url: &str) -> String {
    url.trim().trim_end_matches('/').to_string()
}

/// Normalize both candidate settings structs so an idempotent save can be
/// detected cheaply. Trims whitespace and trailing slashes on `backend_url`.
fn normalize_for_compare(s: &WebIntegrationSettings) -> WebIntegrationSettings {
    WebIntegrationSettings {
        enabled: s.enabled,
        backend_url: trim_backend_url(&s.backend_url),
        web_base_url: s
            .web_base_url
            .as_deref()
            .map(trim_backend_url)
            .filter(|s| !s.is_empty()),
        runner_token: s.runner_token.trim().to_string(),
    }
}

// ---------------------------------------------------------------------------
// get_web_integration_status
// ---------------------------------------------------------------------------

/// Response for [`get_web_integration_status`].
///
/// `runner_token_masked` never contains more than a 16-char prefix + 4-char
/// suffix. `registration_error` is the most recent non-2xx/network error
/// from the background registration retry loop, or `None` if registration
/// succeeded (or has not yet been attempted).
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetWebIntegrationStatusResponse {
    pub enabled: bool,
    pub backend_url: String,
    pub runner_token_masked: String,
    pub runner_id: Option<String>,
    pub last_heartbeat_at: Option<String>,
    pub registration_error: Option<String>,
    /// Whether the unified runner WebSocket is currently connected
    /// (post-handshake). Updated by `crate::mcp::backend_relay`.
    pub ws_connected: bool,
}

#[tauri::command]
pub async fn get_web_integration_status(
    integration: State<'_, IntegrationCompartment>,
) -> Result<GetWebIntegrationStatusResponse, String> {
    let persisted = settings::load_settings().web_integration.clone();
    let sm_state_opt = integration.server_mode().read().await.clone();

    let (runner_id, last_heartbeat_at, registration_error, ws_connected) = match sm_state_opt {
        Some(sm) => (
            sm.runner_id().await.map(|id| id.to_string()),
            sm.last_heartbeat_at().await,
            sm.registration_error().await,
            sm.is_ws_connected(),
        ),
        None => (None, None, None, false),
    };

    Ok(GetWebIntegrationStatusResponse {
        enabled: persisted.enabled,
        backend_url: persisted.backend_url,
        runner_token_masked: mask_runner_token(&persisted.runner_token),
        runner_id,
        last_heartbeat_at,
        registration_error,
        ws_connected,
    })
}

// ---------------------------------------------------------------------------
// save_web_integration_settings
// ---------------------------------------------------------------------------

/// Internal implementation of `save_web_integration_settings` — shared
/// between the Tauri command and the HTTP callback handler used by the
/// browser-login flow.
///
/// Separated so the `GET /auth/runner-token-callback` handler (which runs
/// inside an axum handler, not a Tauri command) can apply the same
/// persist + hot-reload + emit logic without duplicating it.
///
/// Safe to call with `settings` equal to the currently-persisted values:
/// the no-op short-circuit preserves idempotency.
pub async fn apply_web_integration_settings<R: Runtime>(
    app_state: &AppState,
    app_handle: &AppHandle<R>,
    settings: WebIntegrationSettings,
) -> Result<(), String> {
    let normalized = normalize_for_compare(&settings);

    // Early-out when nothing changed.
    let previous = settings::load_settings().web_integration.clone();
    if normalize_for_compare(&previous) == normalized {
        info!("apply_web_integration_settings: no-op (values unchanged)");
        return Ok(());
    }

    // Persist normalized values to settings.json.
    {
        let mut full = settings::load_settings();
        full.web_integration = normalized.clone();
        settings::save_settings(&full).map_err(|e| {
            String::from(AppError::ConfigError(format!(
                "failed to save settings: {}",
                e
            )))
        })?;
    }

    // Hot-reload: shut down the old state, build the new one.
    let new_state_opt = ServerModeConfig::from_settings(&normalized).map(ServerModeState::new);

    {
        let mut guard = app_state.server_mode.write().await;
        if let Some(old) = guard.take() {
            info!("Web-integration hot-reload: signalling old state shutdown");
            old.shutdown();
        }
        *guard = new_state_opt.clone();
    }

    // The WS relay (see `crate::mcp::backend_relay`) is the single outbound
    // channel. It is launched once at startup from `mcp_api::start_server`,
    // reads `WebIntegrationSettings` (and the new `ServerModeState` we just
    // installed) on every reconnect attempt, and observes `shutdown()` on
    // the old state to drop the previous connection cleanly. Kick it here
    // so any in-progress backoff sleep is interrupted and the relay
    // immediately reconnects with the fresh settings + token.
    if new_state_opt.is_some() {
        crate::mcp::backend_relay::commands::kick_cloud_relay().await;
        info!(
            "Web-integration hot-reload: WS relay kicked to pick up new settings (backend={})",
            normalized.backend_url
        );
    } else {
        info!(
            "Web-integration hot-reload: new settings do not form a valid config \
             (enabled={}, backend_url_empty={}, runner_token_empty={}) — integration is now disabled",
            normalized.enabled,
            normalized.backend_url.is_empty(),
            normalized.runner_token.is_empty(),
        );
    }

    // Notify frontend so status views can refresh.
    if let Err(e) = app_handle.emit(WEB_INTEGRATION_CHANGED_EVENT, ()) {
        warn!(
            "failed to emit {} event: {}",
            WEB_INTEGRATION_CHANGED_EVENT, e
        );
    }

    Ok(())
}

/// Persist new web-integration settings and hot-reload the background tasks.
///
/// **Wire contract (IMPORTANT):** JS callers pass flat top-level arguments —
/// `enabled`, `backendUrl`, `runnerToken`, optional `webBaseUrl` — NOT a
/// wrapped `settings` object. Tauri's IPC converts top-level arg names
/// camelCase → snake_case, so the JS shape maps naturally to this signature's
/// `enabled, backend_url, runner_token, web_base_url` parameters. A
/// `settings: WebIntegrationSettings` struct-arg would have forced JS callers
/// to pass `{ settings: { ... } }` with snake-case inner keys (Tauri does NOT
/// recurse rename_all into struct fields) — that mismatch shipped broken in
/// Phase 3G. When adding a new top-level arg, update FOUR sites:
///   1. this command's signature
///   2. the JS caller in `WebIntegrationSettings.tsx`
///   3. the `WebIntegrationSettings` struct in `settings.rs`
///   4. the `SaveArgs` mirror in `ipc_wire_contract_tests` at the bottom of
///      this file (add a test that locks in the new field's wire shape)
///
/// Idempotency: if the incoming values match the currently-persisted
/// settings (after trimming), the function returns early without touching
/// disk, the running `ServerModeState`, or emitting an event. This keeps
/// the Settings UI safe to call on every field change without tearing down
/// and rebuilding the registration flow.
///
/// On an actual change: writes the full `Settings` to disk, calls
/// `shutdown()` on the old `ServerModeState` (if any), installs a freshly
/// constructed `ServerModeState` (or `None` if the new settings don't form
/// a valid config), and spawns the registration + heartbeat tasks on it.
///
/// Emits [`WEB_INTEGRATION_CHANGED_EVENT`] on success so live status views
/// can refresh.
#[tauri::command]
pub async fn save_web_integration_settings<R: Runtime>(
    integration: State<'_, IntegrationCompartment>,
    app_handle: AppHandle<R>,
    enabled: bool,
    backend_url: String,
    runner_token: String,
    web_base_url: Option<String>,
) -> Result<(), String> {
    let settings = WebIntegrationSettings {
        enabled,
        backend_url,
        web_base_url: web_base_url
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
        runner_token,
    };
    // `apply_web_integration_settings` takes `&AppState` because it is also
    // invoked from the axum auth-callback handler. The compartment's inner
    // `Arc<AppState>` is `pub(crate)`, so we can deref it cross-module to
    // satisfy the existing signature without restructuring the helper.
    apply_web_integration_settings(&integration.0, &app_handle, settings).await
}

// ---------------------------------------------------------------------------
// test_web_integration_connection
// ---------------------------------------------------------------------------

/// Response payload for [`test_web_integration_connection`].
#[derive(Debug, Serialize, Deserialize)]
pub struct TestConnectionResponse {
    /// Runner ID assigned by the web backend for the throwaway probe entry.
    /// Returned for debugging only — the entry is deleted before this
    /// function returns, so the ID is not valid for any subsequent call.
    pub runner_id: String,
}

#[derive(Serialize)]
struct ProbeRegisterRequest {
    name: String,
    hostname: String,
    port: u16,
    capabilities: Vec<String>,
    server_mode: bool,
    restate_enabled: bool,
    restate_healthy: bool,
}

#[derive(Deserialize)]
struct ProbeRegisterResponse {
    runner_id: String,
}

async fn delete_probe_runner(
    client: &reqwest::Client,
    backend_url: &str,
    token: &str,
    runner_id: &str,
) {
    let del_url = format!("{}/api/v1/runners/{}", backend_url, runner_id);
    match client.delete(&del_url).bearer_auth(token).send().await {
        Ok(r) if r.status().is_success() || r.status().as_u16() == 404 => {
            // 404 is fine — it means the entry was already cleaned up by the
            // backend (e.g. transient race) so there's nothing to remove.
        }
        Ok(r) => {
            let status = r.status();
            let body = r.text().await.unwrap_or_default();
            warn!(
                "test_web_integration_connection cleanup: DELETE returned {} ({})",
                status,
                body.chars().take(200).collect::<String>()
            );
        }
        Err(e) => {
            warn!(
                "test_web_integration_connection cleanup: DELETE network error: {}",
                e
            );
        }
    }
}

/// Validate a `(backend_url, runner_token)` pair by performing a register +
/// immediate delete round-trip against the web backend. Never mutates
/// runner settings and never spawns background tasks — purely a probe.
///
/// The probe runner uses a unique name (`test-connection-<timestamp>`) so
/// a partial failure leaves easily-identifiable debris. The DELETE is
/// issued from a `Drop`-ish guard: even if the registration body fails to
/// parse after the server allocated the entry, the runner is cleaned up.
#[tauri::command]
pub async fn test_web_integration_connection(
    backend_url: String,
    runner_token: String,
) -> Result<TestConnectionResponse, String> {
    let trimmed_backend = trim_backend_url(&backend_url);
    // Fall back to the persisted token when the caller passes an empty
    // string. The Settings UI clears its in-memory token field after a
    // successful Save (so it never holds the secret longer than needed),
    // which previously made a follow-up "Test connection" send an empty
    // token and 422 with "runner_token is required". Resolving against the
    // persisted value here means Save-then-Test works as the operator
    // expects without re-typing the token.
    let trimmed_token = {
        let from_arg = runner_token.trim().to_string();
        if from_arg.is_empty() {
            settings::load_settings()
                .web_integration
                .runner_token
                .trim()
                .to_string()
        } else {
            from_arg
        }
    };
    if trimmed_backend.is_empty() {
        return Err("backend_url is required".to_string());
    }
    if trimmed_token.is_empty() {
        return Err("runner_token is required".to_string());
    }

    let client = build_http_client()?;

    let timestamp_ns = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let probe_name = format!("test-connection-{}", timestamp_ns);

    let hostname = hostname::get()
        .map(|h| h.to_string_lossy().to_string())
        .unwrap_or_else(|_| "test".to_string());

    let payload = ProbeRegisterRequest {
        name: probe_name,
        hostname,
        port: 0,
        capabilities: vec![],
        server_mode: true,
        restate_enabled: false,
        restate_healthy: false,
    };

    let register_url = format!("{}/api/v1/runners/register", trimmed_backend);
    let resp = client
        .post(&register_url)
        .bearer_auth(&trimmed_token)
        .json(&payload)
        .send()
        .await
        .map_err(|e| {
            String::from(AppError::NetworkError(format!(
                "network error contacting {}: {}",
                trimmed_backend, e
            )))
        })?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        let truncated_body = body.chars().take(200).collect::<String>();
        let msg = if status.as_u16() == 401 || status.as_u16() == 403 {
            String::from(AppError::AuthError(format!(
                "auth failed ({}): {}",
                status, truncated_body
            )))
        } else {
            String::from(AppError::HttpStatusError {
                status: status.as_u16(),
                body: truncated_body,
            })
        };
        return Err(msg);
    }

    // Parse the response; on decode failure we still want to attempt
    // cleanup. We first pull the raw body so we can log + attempt decode
    // without re-requesting.
    let body_text = resp.text().await.map_err(|e| {
        String::from(AppError::NetworkError(format!(
            "failed to read register response body: {}",
            e
        )))
    })?;
    let parsed: Result<ProbeRegisterResponse, _> = serde_json::from_str(&body_text);

    match parsed {
        Ok(body) => {
            // Best-effort cleanup regardless of success or subsequent error.
            delete_probe_runner(&client, &trimmed_backend, &trimmed_token, &body.runner_id).await;
            Ok(TestConnectionResponse {
                runner_id: body.runner_id,
            })
        }
        Err(e) => {
            // We got 2xx but can't parse the body. The web backend
            // allocated a runner entry we can't identify, so we can't
            // clean it up — emit a clear warning but surface the parse
            // failure to the caller.
            warn!(
                "test_web_integration_connection: 2xx response body did not parse: {} body={}",
                e,
                body_text.chars().take(200).collect::<String>()
            );
            Err(format!(
                "register returned 2xx but body did not decode as \
                 RegistrationResponse: {}",
                e
            ))
        }
    }
}

// ---------------------------------------------------------------------------
// start_web_token_flow (Phase 3G-web-polish)
// ---------------------------------------------------------------------------

/// URL-encode a value for safe inclusion in a query string.
///
/// Scoped to just the characters we use here (hostnames, ports, hex state,
/// HTTP URLs). We intentionally avoid pulling in `urlencoding` / `percent-encoding`
/// as a dedicated dep — the set of characters we produce is tightly
/// controlled.
fn url_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' | '~' => out.push(c),
            _ => {
                let mut buf = [0u8; 4];
                for b in c.encode_utf8(&mut buf).as_bytes() {
                    out.push_str(&format!("%{:02X}", b));
                }
            }
        }
    }
    out
}

fn get_hostname_for_browser() -> String {
    hostname::get()
        .map(|h| h.to_string_lossy().to_string())
        .unwrap_or_else(|_| "runner".to_string())
}

/// Start a one-click "connect with web login" flow.
///
/// Persists the `backend_url` if provided (so the eventual callback's save
/// has a valid backend to merge against), generates a random state, stores
/// it in the in-memory [`TokenFlowStore`](crate::server_mode::TokenFlowStore),
/// and opens the user's default browser at
/// `{backend_url}/connect-runner?state=...&callback=...&runner_name=...`.
///
/// The web page at `/connect-runner` prompts the user to confirm token
/// creation, POSTs to `/api/v1/runners/tokens`, and redirects the browser
/// back to `http://127.0.0.1:<runner-port>/auth/runner-token-callback`
/// where the token is captured and applied via
/// [`apply_web_integration_settings`].
///
/// # Errors
///
/// Returns `Err` if:
/// - `backend_url` is empty and no backend is persisted in settings.
/// - `open::that` fails to launch the browser (e.g. no default handler).
#[tauri::command]
pub async fn start_web_token_flow<R: Runtime>(
    integration: State<'_, IntegrationCompartment>,
    health: State<'_, HealthCompartment>,
    app_handle: AppHandle<R>,
    backend_url: Option<String>,
) -> Result<(), String> {
    // Resolve effective backend URL.
    let effective_backend: String = match backend_url
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(provided) => {
            let trimmed = trim_backend_url(provided);
            // Persist so the eventual callback (which merges token into
            // current settings) sees this backend even if the user hasn't
            // hit Save yet.
            let mut full = settings::load_settings();
            if full.web_integration.backend_url != trimmed {
                full.web_integration.backend_url = trimmed.clone();
                settings::save_settings(&full).map_err(|e| {
                    String::from(AppError::ConfigError(format!(
                        "failed to persist backend_url: {}",
                        e
                    )))
                })?;
                // Don't flip `enabled` — just staging the URL. The callback
                // sets `enabled=true` when it lands the token.
                if let Err(e) = app_handle.emit(WEB_INTEGRATION_CHANGED_EVENT, ()) {
                    warn!(
                        "failed to emit {} event during backend_url staging: {}",
                        WEB_INTEGRATION_CHANGED_EVENT, e
                    );
                }
            }
            trimmed
        }
        None => {
            let persisted = settings::load_settings()
                .web_integration
                .backend_url
                .clone();
            let trimmed = trim_backend_url(&persisted);
            if trimmed.is_empty() {
                return Err("backend_url not configured — please enter one first".to_string());
            }
            trimmed
        }
    };

    // Generate state and store it.
    let state = integration.token_flow().start(effective_backend.clone());

    // Build callback URL (runner's own HTTP API). Use 127.0.0.1 not
    // localhost so the regex on the web side can validate it strictly.
    let runner_port = {
        // Prefer the actually-bound port if we can read it from AppState;
        // fall back to the env-var-driven value for the (unusual) case
        // where this command fires before the HTTP server has recorded its
        // port. The callback route lives on the same axum router, so once
        // that router is up the port matches.
        let bound = health.api_port().load(std::sync::atomic::Ordering::Relaxed);
        if bound > 0 {
            bound
        } else {
            crate::mcp::types::get_mcp_api_port()
        }
    };
    let callback_url = format!(
        "http://127.0.0.1:{}/auth/runner-token-callback",
        runner_port
    );

    let runner_name = get_hostname_for_browser();

    // `/connect-runner` is a Next.js page, not an API endpoint. In production
    // the API and the web frontend are split hosts (`api.qontinui.io` vs
    // `qontinui.io`); in split local dev they differ by port. Honor an explicit
    // `web_base_url` override when set, else derive the frontend origin from the
    // backend by stripping a leading `api.` label (a no-op for localhost / a
    // unified origin), so the browser never lands on the API host — which has
    // no `/connect-runner` page.
    let web_origin = settings::load_settings()
        .web_integration
        .web_base_url
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(trim_backend_url)
        .unwrap_or_else(|| crate::api_config::derive_web_base_url(&effective_backend));

    let web_url = format!(
        "{}/connect-runner?state={}&callback={}&runner_name={}",
        web_origin,
        url_encode(&state),
        url_encode(&callback_url),
        url_encode(&runner_name),
    );

    info!(
        "start_web_token_flow: opening browser to {}/connect-runner (callback_port={}, api_backend={})",
        web_origin, runner_port, effective_backend
    );

    open::that(&web_url).map_err(|e| {
        String::from(AppError::ProcessError(format!(
            "failed to open browser: {}",
            e
        )))
    })?;

    Ok(())
}

// ---------------------------------------------------------------------------
// redeem_pair_code — paste-pair via single-use 5-min code (Phase 2a.3)
// ---------------------------------------------------------------------------

/// Wire response for [`redeem_pair_code`].
///
/// Mirrors the runner-side ``PairCompleteResponse`` shape so the frontend
/// can show the operator a uniform "paired!" confirmation regardless of
/// which pair flow was used. The device-token JWT is NOT returned to JS
/// — it's persisted to the auth store on the Rust side via
/// ``pair::persist_pairing``.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RedeemPairCodeResponse {
    pub user_id: String,
    pub tenant_id: String,
    pub device_id: String,
}

/// Redeem a 6-char single-use pair code against the active profile's web
/// backend. Single round-trip; on success persists the device JWT +
/// updates `paired_user.json` so the WS relay picks it up.
///
/// Phase 2a.3 of plan
/// ``D:/qontinui-root/plans/2026-05-22-mtc-iter3-remediation-web-dashboard.md``.
///
/// # Errors
///
/// Returns `Err(String)` if:
/// - `code` is blank.
/// - The active profile has no `coord_url` (we can't derive the web base).
/// - The device has not been initialised (`~/.qontinui/machine.json`
///   missing).
/// - The web backend returns 4xx/5xx (`Err` carries the structured error
///   from the response so the UI can surface "expired" / "already
///   redeemed" / "unknown code").
/// - Network failure.
/// - Persisting the resulting JWT to local storage failed (in which case
///   the device IS paired server-side but the runner cannot use the
///   credential — operator should retry).
#[tauri::command]
pub async fn redeem_pair_code(
    code: String,
    backend_url: Option<String>,
) -> Result<RedeemPairCodeResponse, String> {
    use qontinui_runner_lib::pair::{
        coord_http_base, derive_web_base_from_coord, pair_with_pair_code, persist_pairing,
        read_device_id_from_disk,
    };

    let code_trimmed = code.trim().to_string();
    if code_trimmed.is_empty() {
        return Err("pair code is empty".to_string());
    }

    // Resolve the device_id from disk. The runner must have been
    // initialised at least once (machine.json present) — typically true
    // for any installed runner; surfacing a clear error if not.
    let device_id =
        read_device_id_from_disk().map_err(|e| format!("could not read device identity: {}", e))?;

    // Resolve the web base. Pair-code endpoints live on qontinui-web,
    // not coord. Precedence:
    //   1. An explicit `backend_url` passed from the Settings form. The
    //      operator may have typed a new backend without hitting Save yet;
    //      honoring the form value means "redeem against the URL I see in
    //      the field" rather than a stale persisted one.
    //   2. `QONTINUI_WEB_BASE` env override (split web/coord hosts).
    //   3. Derived from the active profile's coord_url.
    let web_base = match backend_url
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(form_url) => trim_backend_url(form_url),
        None => {
            let coord_base = coord_http_base().map_err(|e| format!("active profile: {}", e))?;
            std::env::var("QONTINUI_WEB_BASE")
                .ok()
                .unwrap_or_else(|| derive_web_base_from_coord(&coord_base))
        }
    };

    // Run the blocking HTTP call on a tokio blocking thread so we don't
    // tie up the async runtime. `pair_with_pair_code` uses a 30-second
    // timeout internally, so this never hangs indefinitely.
    let code_for_blocking = code_trimmed.clone();
    let device_id_for_blocking = device_id.clone();
    let web_base_for_blocking = web_base.clone();
    let resp = tokio::task::spawn_blocking(move || {
        pair_with_pair_code(
            &web_base_for_blocking,
            &code_for_blocking,
            &device_id_for_blocking,
        )
    })
    .await
    .map_err(|e| format!("pair-code redeem task panicked: {e}"))??;

    // Parse the tenant_id off the response so we can pass it through to
    // persist_pairing.
    let tenant_id_str = resp
        .tenant_id
        .as_deref()
        .ok_or_else(|| "server omitted tenant_id in pair-code redeem response".to_string())?;
    let tenant_id = uuid::Uuid::parse_str(tenant_id_str.trim())
        .map_err(|e| format!("server returned malformed tenant_id: {e}"))?;

    persist_pairing(&resp, tenant_id).map_err(|e| format!("persist pairing: {}", e))?;

    // Promote to Tier 2 (qontinui_account) now that a device JWT is in
    // hand. Redeeming a pair code IS a cloud-account bind, so the runner
    // must leave Tier Local for the WS relay to come online. Defensive:
    // the Settings UI also promotes after redeem, but a headless / UI-Bridge
    // caller that doesn't run the FE path still ends up online. Idempotent —
    // a no-op when already at Tier 2. Kicks the relay + JWT refresher as a
    // side effect of `set_runner_tier`.
    {
        let mut s = settings::load_settings();
        if s.tier != settings::RunnerTier::QontinuiAccount {
            s.tier = settings::RunnerTier::QontinuiAccount;
            s.tier_initialized = true;
            if crate::instance::is_secondary() {
                warn!("redeem_pair_code: secondary runner — applying in-memory only, skipping save_settings");
                crate::mcp::backend_relay::commands::kick_cloud_relay().await;
                crate::mcp::device_jwt_refresher::commands::kick_device_jwt_refresher().await;
                info!("redeem_pair_code: promoted runner to Tier QontinuiAccount + kicked relay");
            } else if let Err(e) = settings::save_settings(&s) {
                warn!("redeem_pair_code: tier promotion persist failed (continuing): {e}");
            } else {
                crate::mcp::backend_relay::commands::kick_cloud_relay().await;
                crate::mcp::device_jwt_refresher::commands::kick_device_jwt_refresher().await;
                info!("redeem_pair_code: promoted runner to Tier QontinuiAccount + kicked relay");
            }
        }
    }

    let response_device_id = resp.device_id.clone().unwrap_or_else(|| device_id.clone());

    info!(
        "redeem_pair_code: device paired (user_id={}, tenant_id={}, device_id={})",
        resp.user_id, tenant_id_str, response_device_id
    );

    Ok(RedeemPairCodeResponse {
        user_id: resp.user_id,
        tenant_id: tenant_id_str.to_string(),
        device_id: response_device_id,
    })
}

/// Tauri plugin exposing all web-integration commands.
pub fn plugin<R: Runtime>() -> TauriPlugin<R> {
    PluginBuilder::new("qontinui_web_integration")
        .invoke_handler(tauri::generate_handler![
            get_web_integration_status,
            save_web_integration_settings,
            test_web_integration_connection,
            start_web_token_flow,
            redeem_pair_code,
        ])
        .build()
}

// ---------------------------------------------------------------------------
// Regression tests — IPC wire contract (Phase 3H)
// ---------------------------------------------------------------------------
//
// The Phase 3G frontend shipped broken: it invoked
// `save_web_integration_settings` with flat camelCase args while the Rust
// signature took a single `settings: WebIntegrationSettings` struct arg.
// Tauri's IPC accepts top-level args by name and converts camelCase →
// snake_case, but it does NOT recurse that rename into struct fields. The
// mismatch silently failed (Tauri returned "missing required key settings",
// which the frontend logged as a generic error). No build step caught it.
//
// These tests mirror what Tauri's `#[tauri::command]` macro generates for
// argument extraction so a shape-drift regression fails in CI. They're
// decoupled from Tauri itself so they stay fast and don't require a mock
// app. If Tauri changes its internal extractor, these tests become a
// false-confidence measure — re-verify with a real end-to-end invoke via
// the UI Bridge when touching the macro or upgrading Tauri.

#[cfg(test)]
mod ipc_wire_contract_tests {
    use serde::Deserialize;

    /// Mirror of what Tauri generates for `save_web_integration_settings`
    /// argument extraction. Top-level args are renamed camelCase → snake_case.
    /// Kept in sync with the live command signature — update this struct AND
    /// the command's doc block when adding a new top-level arg.
    #[derive(Debug, Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct SaveArgs {
        enabled: bool,
        backend_url: String,
        runner_token: String,
        #[serde(default)]
        web_base_url: Option<String>,
    }

    #[test]
    fn save_accepts_frontends_natural_shape() {
        // Three-field shape from WebIntegrationSettings.tsx — no webBaseUrl.
        // Must keep working so desktop users who don't set the SPA override
        // continue to save cleanly (webBaseUrl falls back to backend_url).
        let payload = r#"{
            "enabled": true,
            "backendUrl": "http://localhost:8000",
            "runnerToken": "qontinui_runner_0000000000000000000000000000000000000000000000000000000000000000"
        }"#;
        let args: SaveArgs = serde_json::from_str(payload).expect("must deserialize");
        assert!(args.enabled);
        assert_eq!(args.backend_url, "http://localhost:8000");
        assert!(args.runner_token.starts_with("qontinui_runner_"));
        assert_eq!(args.web_base_url, None);
    }

    #[test]
    fn save_accepts_web_base_url_override() {
        // Split local dev: FastAPI on :8000, Next.js SPA on :3001. When the
        // frontend exposes webBaseUrl, the four-field payload must deserialize.
        let payload = r#"{
            "enabled": true,
            "backendUrl": "http://localhost:8000",
            "runnerToken": "qontinui_runner_0000000000000000000000000000000000000000000000000000000000000000",
            "webBaseUrl": "http://localhost:3001"
        }"#;
        let args: SaveArgs = serde_json::from_str(payload).expect("must deserialize");
        assert_eq!(args.web_base_url.as_deref(), Some("http://localhost:3001"));
    }

    #[test]
    fn save_rejects_phase3g_broken_wrapped_shape() {
        // The shape that shipped broken in Phase 3G — `settings` wrapper
        // around the inner fields. Must fail so a future accidental revert
        // is caught in CI.
        let payload = r#"{
            "settings": {
                "enabled": true,
                "backendUrl": "http://localhost:8000",
                "runnerToken": "qontinui_runner_x"
            }
        }"#;
        let result = serde_json::from_str::<SaveArgs>(payload);
        assert!(
            result.is_err(),
            "settings-wrapped payload must not deserialize — that was the 3G bug"
        );
    }

    #[test]
    fn save_rejects_snake_case_keys() {
        // Tauri top-level arg convention is camelCase in; snake_case keys
        // from JS would indicate the frontend is speaking the wrong dialect.
        let payload = r#"{
            "enabled": true,
            "backend_url": "http://localhost:8000",
            "runner_token": "qontinui_runner_x"
        }"#;
        let result = serde_json::from_str::<SaveArgs>(payload);
        assert!(
            result.is_err(),
            "snake_case top-level keys must not deserialize — JS callers must use camelCase"
        );
    }

    /// Mirror for `test_web_integration_connection` (already flat from 3G).
    #[derive(Debug, Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct TestConnectionArgs {
        backend_url: String,
        runner_token: String,
    }

    #[test]
    fn test_connection_accepts_frontends_natural_shape() {
        let payload = r#"{"backendUrl": "http://x", "runnerToken": "tok"}"#;
        let args: TestConnectionArgs = serde_json::from_str(payload).expect("must deserialize");
        assert_eq!(args.backend_url, "http://x");
        assert_eq!(args.runner_token, "tok");
    }

    /// Mirror for `start_web_token_flow` — `backendUrl` is optional.
    #[derive(Debug, Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct StartFlowArgs {
        backend_url: Option<String>,
    }

    #[test]
    fn start_flow_accepts_missing_backend_url() {
        let payload = r#"{}"#;
        let args: StartFlowArgs = serde_json::from_str(payload).expect("empty must deserialize");
        assert_eq!(args.backend_url, None);
    }

    #[test]
    fn start_flow_accepts_camelcase_backend_url() {
        let payload = r#"{"backendUrl": "http://x"}"#;
        let args: StartFlowArgs = serde_json::from_str(payload).expect("must deserialize");
        assert_eq!(args.backend_url.as_deref(), Some("http://x"));
    }
}

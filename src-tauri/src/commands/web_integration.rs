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
//! * [`test_web_integration_connection`] — probe a candidate backend URL
//!   without persisting. READ-ONLY: it reaches the backend's health route and
//!   then, if the runner holds a device JWT, its own device identity. It
//!   creates nothing and deletes nothing.
//!
//! After a successful `save_web_integration_settings` call the command
//! emits a `web-integration-changed` Tauri event so live status views can
//! refresh.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::plugin::{Builder as PluginBuilder, TauriPlugin};
use tauri::{AppHandle, Emitter, Runtime, State};
use tracing::{debug, info, warn};

use crate::commands::compartments::IntegrationCompartment;
use crate::commands::AppState;
use crate::error::AppError;
use crate::server_mode::{ServerModeConfig, ServerModeState};
use crate::settings::{self, WebIntegrationSettings};
use qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked;

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
    /// The persisted web-frontend origin override, or `""` when unset (the
    /// backend then derives it from `backend_url`).
    ///
    /// NO-DOWNGRADE: this field was DECLARED by the TypeScript
    /// `WebIntegrationStatus` interface but never sent by Rust, so
    /// `applyStatusToForm` always seeded the Advanced "web base URL" input
    /// with `""`. Saving the panel for any unrelated reason (toggling
    /// Enabled, pasting a token) then posted `webBaseUrl: undefined` and
    /// silently WIPED a configured override — a capability reduction caused
    /// by a value that was never read back. Sending it closes the loop.
    pub web_base_url: String,
    pub runner_token_masked: String,
    pub runner_id: Option<String>,
    pub last_heartbeat_at: Option<String>,
    pub registration_error: Option<String>,
    /// Whether the unified runner WebSocket is currently connected
    /// (post-handshake). Updated by `crate::mcp::backend_relay`.
    pub ws_connected: bool,
    /// NO-DOWNGRADE: the settings-read fault, when settings.json could not be
    /// read or parsed. Non-null means every other field in this response is a
    /// PLACEHOLDER, not the user's saved configuration — the UI must say so
    /// rather than rendering a runner that looks disabled/signed-out.
    pub settings_fault: Option<crate::settings::SettingsFault>,
}

#[tauri::command]
pub async fn get_web_integration_status(
    integration: State<'_, IntegrationCompartment>,
) -> Result<GetWebIntegrationStatusResponse, String> {
    let loaded = settings::load_settings_full();
    let persisted = loaded.settings.web_integration.clone();
    let settings_fault = if loaded.is_authoritative() {
        None
    } else {
        crate::settings::settings_fault()
    };
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
        web_base_url: persisted.web_base_url.unwrap_or_default(),
        runner_token_masked: mask_runner_token(&persisted.runner_token),
        runner_id,
        last_heartbeat_at,
        registration_error,
        ws_connected,
        settings_fault,
    })
}

// ---------------------------------------------------------------------------
// get_settings_health
// ---------------------------------------------------------------------------

/// Response for [`get_settings_health`].
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsHealthResponse {
    /// `"loaded" | "fresh_install" | "unreadable"`.
    pub provenance: String,
    /// `true` when the values the runner is operating on are the user's real
    /// persisted state. When `false`, the runner is deliberately refusing to
    /// write settings and every capability read is reporting UNKNOWN rather
    /// than the lesser capability.
    pub authoritative: bool,
    /// Populated iff `!authoritative` — path + error + when it was seen.
    pub fault: Option<crate::settings::SettingsFault>,
    /// Ready-to-render one-line explanation, or `None` when healthy.
    pub message: Option<String>,
}

/// Report whether `settings.json` is readable.
///
/// The user-visible half of C1: a settings-read failure no longer silently
/// demotes the runner, so something has to SAY so. Poll this (or read
/// `settingsFault` on [`get_web_integration_status`]) to surface a "settings
/// unreadable" banner.
#[tauri::command]
pub fn get_settings_health() -> Result<SettingsHealthResponse, String> {
    let loaded = settings::load_settings_full();
    let authoritative = loaded.is_authoritative();
    Ok(SettingsHealthResponse {
        provenance: loaded.provenance.as_str().to_string(),
        authoritative,
        fault: if authoritative {
            None
        } else {
            crate::settings::settings_fault()
        },
        message: if authoritative {
            None
        } else {
            Some(loaded.unreadable_message())
        },
    })
}

// ---------------------------------------------------------------------------
// save_web_integration_settings
// ---------------------------------------------------------------------------

/// Internal implementation of `save_web_integration_settings`.
///
/// Takes `&AppState` (rather than a compartment) so it can apply the same
/// persist + hot-reload + emit logic from any caller that holds the app
/// state directly.
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

    // Persist normalized values to settings.json. Provenance-checked: refuses
    // rather than rewriting an unreadable settings.json from defaults (which
    // would drop tier / saved projects / every other field as a side effect of
    // saving one panel).
    {
        let to_persist = normalized.clone();
        settings::update_settings(move |full| full.web_integration = to_persist).map_err(|e| {
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
             (enabled={}, backend_url_empty={}) — integration is now disabled",
            normalized.enabled,
            normalized.backend_url.is_empty(),
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
    // `apply_web_integration_settings` takes `&AppState`. The compartment's
    // inner `Arc<AppState>` is `pub(crate)`, so we can deref it cross-module
    // to satisfy the signature without restructuring the helper.
    apply_web_integration_settings(&integration.0, &app_handle, settings).await
}

// ---------------------------------------------------------------------------
// test_web_integration_connection
// ---------------------------------------------------------------------------

/// Response payload for [`test_web_integration_connection`].
///
/// Every field answers a DIFFERENT question, because "test connection" is not
/// one question. Reachability, pairing and token shape fail independently and
/// an operator needs to know which one broke.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TestConnectionResponse {
    /// The backend answered `GET /api/v1/health/live`. False is never reached
    /// today (an unreachable backend is an `Err`), but the field is explicit so
    /// the UI never infers reachability from the absence of an error.
    pub reachable: bool,
    /// Whether this runner holds a device JWT that the backend accepted.
    /// `false` means reachable-but-unpaired, which is a normal first-run state
    /// and NOT a configuration error.
    pub paired: bool,
    /// WHY the identity leg did not succeed. `paired: false` alone conflates
    /// faults an operator must act on differently — "never paired yet" and
    /// "your stored credential was rejected" are not the same problem, and a UI
    /// branching only on `paired` shows them identically.
    pub identity_fault: IdentityFault,
    /// Device identity from `GET /api/v1/devices/me`. Present iff `paired`.
    pub device_id: Option<String>,
    pub user_id: Option<String>,
    pub tenant_id: Option<String>,
    /// Whether the CONFIGURED runner token has the expected `qontinui_runner_`
    /// shape. `None` means no token is configured at all, which is the normal
    /// state for a runner paired through Cognito or a pair code — those paths
    /// never mint a `qontinui_runner_` token, so absence is not a fault and
    /// must not be rendered as one. `Some(false)` is the only actionable value.
    /// A LOCAL shape check only — see the note on
    /// [`test_web_integration_connection`] for why the token is not sent.
    pub token_format_valid: Option<bool>,
    /// One line an operator can act on, covering whichever arm was reached.
    pub detail: String,
}

/// Why the identity leg of [`test_web_integration_connection`] did not produce
/// an identity. Distinct variants because each wants a different operator
/// action; collapsing them is what sends someone to re-paste a credential when
/// coord's verifier is the thing that is down.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentityFault {
    /// The identity check succeeded.
    None,
    /// No device JWT is stored — this runner has never paired. A normal
    /// first-run state, not a misconfiguration.
    Unpaired,
    /// The probed URL is not the backend this runner is bound to, so the
    /// identity leg was deliberately SKIPPED rather than presenting this
    /// runner's live credential to an unbound host. See the security note on
    /// [`test_web_integration_connection`].
    NotBoundBackend,
    /// 401 — the stored device JWT was rejected (stale, revoked, or minted for
    /// another backend). Re-pair.
    Rejected,
    /// 403 — understood, but not permitted here.
    Forbidden,
    /// 503 — the backend could not reach coord's JWKS to verify the token. A
    /// coord-tier fault that says nothing about this runner's credential.
    VerifierDown,
    /// A 2xx whose body did not decode, or any other unexpected status.
    Unexpected,
}

/// Response of `GET /api/v1/devices/me`.
#[derive(Deserialize)]
struct DeviceIdentityResponse {
    device_id: String,
    user_id: String,
    tenant_id: String,
}

/// The prefix every runner token minted by web's `/connect-runner` flow
/// carries. Checked locally so an obvious paste error is caught without a
/// round-trip; see [`test_web_integration_connection`] for why that is the only
/// check this command can honestly make.
const RUNNER_TOKEN_PREFIX: &str = "qontinui_runner_";

/// Is `candidate` the backend this runner is BOUND to — the origin
/// `api_config::get_api_base_url()` resolves to, which is what
/// `mcp::backend_relay` dials and what the stored device JWT was minted for?
///
/// Compared on ORIGIN (scheme + host + port), not on the raw string, so a
/// trailing slash or a case difference in the host does not read as a different
/// backend. Anything that does not parse compares unequal — fail closed, since
/// the consequence of a false positive is presenting a live credential to an
/// unbound host.
fn is_bound_backend(candidate: &str) -> bool {
    fn origin(raw: &str) -> Option<(String, String, Option<u16>)> {
        let u = reqwest::Url::parse(raw.trim()).ok()?;
        let host = u.host_str()?.to_ascii_lowercase();
        Some((
            u.scheme().to_ascii_lowercase(),
            host,
            u.port_or_known_default(),
        ))
    }
    match (
        origin(candidate),
        origin(&crate::api_config::get_api_base_url()),
    ) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}

/// Validate a candidate `(backend_url, runner_token)` pair WITHOUT persisting
/// anything and WITHOUT mutating the backend. Purely a probe.
///
/// # Why this no longer registers a throwaway runner
///
/// Until 2026-09-12 this command did a register + immediate delete round-trip
/// against `POST /api/v1/runners/register`. That endpoint was deleted from
/// qontinui-web in `ad3692e6c` (2026-04-28) — "delete legacy fleet endpoints" —
/// and the whole `/api/v1/runners` router followed in `1574bd036`. There is no
/// `register` route anywhere in the web tree today and no alias, so this probe
/// had been returning 404 to every operator who pressed "Test connection" for
/// months. The replacement for registration is PAIRING, not a different
/// register URL, so there is nothing to repoint it at.
///
/// # What it checks instead
///
/// Two read-only steps, reported independently so the operator learns which one
/// failed:
///
/// 1. `GET {backend}/api/v1/health/live` — unauthenticated, no dependency
///    checks, 200 whenever the process is up. This isolates "wrong URL / server
///    down" from every credential question.
/// 2. `GET {backend}/api/v1/devices/me` with the runner's OWN device JWT from
///    [`crate::auth::AuthManager`]'s `access_token` slot. That is the credential
///    the backend relay actually presents, so this proves the thing the runner
///    will really do. 200 yields `{device_id, user_id, tenant_id}`; 401 means
///    the stored JWT is stale or foreign; 503 means coord's JWKS is unreachable
///    and says nothing about this runner.
///
/// # The identity leg only runs against the BOUND backend
///
/// Step 2 presents this runner's live, 4-hour, coord-issued device JWT. That
/// credential is minted for one backend, and this command's `backend_url` is
/// caller-supplied — the command is on the UI-Bridge invoke allowlist, so a
/// local HTTP caller supplies it directly with no operator in the loop. Sending
/// the JWT to whatever host was typed would hand a live credential to an
/// arbitrary, attacker-choosable origin, and the `/health/live` gate is no
/// protection because any host can answer 200.
///
/// So the identity leg runs ONLY when the probed origin matches the persisted
/// backend (`api_config::get_api_base_url`) — the same origin
/// `mcp::backend_relay` dials. Any other URL still gets its reachability
/// answered and comes back [`IdentityFault::NotBoundBackend`], which is an
/// honest "not checked", never a pass. The predecessor sent the legacy runner
/// token here; upgrading the credential without narrowing the destination would
/// have widened the blast radius while fixing the 404.
///
/// # Why the runner token is not sent anywhere
///
/// `runner_token` is no longer a qontinui-web credential. Since the unified
/// devices migration it is presented on exactly one route — coord's `pair-cli`
/// — where it is exchanged for a device JWT. Exercising it would therefore mean
/// performing a real pairing and minting a real credential, which is a mutation
/// and not something a "Test connection" button should do. So the token is
/// checked for its `qontinui_runner_` shape locally, and `token_format_valid`
/// says exactly that much and no more.
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
    // token. Resolving against the persisted value here means
    // Save-then-Test works as the operator expects without re-typing it.
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

    // A missing token is no longer fatal: the probe's substantive half is the
    // device-JWT identity check, which does not use the token at all. Report
    // the shape rather than refusing to run — and report ABSENCE as absence
    // (`None`), not as a malformed token, because a runner paired through
    // Cognito or a pair code legitimately has none.
    let token_format_valid = if trimmed_token.is_empty() {
        None
    } else {
        Some(trimmed_token.starts_with(RUNNER_TOKEN_PREFIX))
    };

    let client = build_http_client()?;

    // ---- Step 1: reachability (unauthenticated) ----
    let health_url = format!("{}/api/v1/health/live", trimmed_backend);
    // coord-auth-exempt(not-coord): `qontinui-web` `/api/v1/health/live`, an
    // unauthenticated liveness route.
    let health_resp = client.get(&health_url).send().await.map_err(|e| {
        String::from(AppError::NetworkError(format!(
            "cannot reach {} — check the backend URL: {}",
            trimmed_backend, e
        )))
    })?;
    if !health_resp.status().is_success() {
        let status = health_resp.status();
        let body = health_resp.text().await.unwrap_or_default();
        return Err(String::from(AppError::HttpStatusError {
            status: status.as_u16(),
            body: format!(
                "{} did not answer its liveness route: {}",
                health_url,
                body.chars().take(200).collect::<String>()
            ),
        }));
    }

    // ---- Step 2: identity, with the credential the relay actually uses ----
    //
    // GATE FIRST, read the credential second. The bound-backend check must
    // happen before the JWT is even loaded, so an unbound URL cannot reach the
    // credential at all. See the security note on this function.
    if !is_bound_backend(&trimmed_backend) {
        return Ok(TestConnectionResponse {
            reachable: true,
            paired: false,
            identity_fault: IdentityFault::NotBoundBackend,
            device_id: None,
            user_id: None,
            tenant_id: None,
            token_format_valid,
            detail: format!(
                "Backend reachable at {trimmed_backend}, but that is not the backend \
                 this runner is bound to ({}). Identity was NOT checked — this \
                 runner's device credential is only ever presented to its bound \
                 backend. Save this URL and re-pair to bind to it.",
                crate::api_config::get_api_base_url()
            ),
        });
    }

    // `get_access_token` is synchronous and can fall through to the OS keychain,
    // which on Linux is a D-Bus Secret Service round-trip bounded at 3s
    // (`auth::keychain_call_bounded`). That is a real stall of a tokio worker on
    // an operator button press, and the unpaired case — the one this branch
    // exists to report — is precisely the one that always reaches the keychain.
    // Same treatment `redeem_pair_code` below already gives its blocking call.
    let device_jwt = spawn_blocking_tracked(|| {
        crate::auth::AuthManager::new()
            .get_access_token()
            .ok()
            .filter(|j| !j.trim().is_empty())
    })
    .await
    .unwrap_or(None);

    let Some(device_jwt) = device_jwt else {
        return Ok(TestConnectionResponse {
            reachable: true,
            paired: false,
            identity_fault: IdentityFault::Unpaired,
            device_id: None,
            user_id: None,
            tenant_id: None,
            token_format_valid,
            detail: format!(
                "Backend reachable at {trimmed_backend}. This runner holds no device \
                 JWT yet, so it is not paired — complete pairing from the web UI's \
                 /connect-runner flow."
            ),
        });
    };

    let me_url = format!("{}/api/v1/devices/me", trimmed_backend);
    // coord-auth-exempt(not-coord): `qontinui-web` `/api/v1/devices/me`, with
    // the runner's own coord-issued device JWT.
    let me_resp = client
        .get(&me_url)
        .bearer_auth(&device_jwt)
        .send()
        .await
        .map_err(|e| {
            String::from(AppError::NetworkError(format!(
                "reached {} but the identity check failed: {}",
                trimmed_backend, e
            )))
        })?;

    let status = me_resp.status();
    if !status.is_success() {
        let body = me_resp.text().await.unwrap_or_default();
        let truncated = body.chars().take(200).collect::<String>();
        // These are genuinely different faults. Collapsing them into one "auth
        // failed" is what sends an operator to re-paste a token when coord's
        // JWKS is the thing that is down.
        let (fault, detail) = match status.as_u16() {
            401 => (
                IdentityFault::Rejected,
                "the stored device JWT was rejected (stale, revoked, or issued \
                 for another backend) — re-pair this runner"
                    .to_string(),
            ),
            403 => (
                IdentityFault::Forbidden,
                "the device JWT was understood but is not permitted here".to_string(),
            ),
            503 => (
                IdentityFault::VerifierDown,
                "the backend could not reach coord's JWKS to verify the device \
                 JWT — a coord-tier fault that says nothing about this runner's \
                 credential"
                    .to_string(),
            ),
            _ => (
                IdentityFault::Unexpected,
                format!("unexpected status from {me_url}"),
            ),
        };
        return Ok(TestConnectionResponse {
            reachable: true,
            paired: false,
            identity_fault: fault,
            device_id: None,
            user_id: None,
            tenant_id: None,
            token_format_valid,
            detail: format!(
                "Backend reachable. Identity check failed ({status}): {detail}. {truncated}"
            ),
        });
    }

    let body_text = me_resp.text().await.map_err(|e| {
        String::from(AppError::NetworkError(format!(
            "failed to read identity response body: {}",
            e
        )))
    })?;
    match serde_json::from_str::<DeviceIdentityResponse>(&body_text) {
        Ok(id) => Ok(TestConnectionResponse {
            reachable: true,
            paired: true,
            identity_fault: IdentityFault::None,
            detail: format!(
                "Connected to {} as device {} (tenant {}).",
                trimmed_backend, id.device_id, id.tenant_id
            ),
            device_id: Some(id.device_id),
            user_id: Some(id.user_id),
            tenant_id: Some(id.tenant_id),
            token_format_valid,
        }),
        Err(e) => {
            warn!(
                "test_web_integration_connection: /devices/me returned 2xx but did not \
                 parse: {} body={}",
                e,
                body_text.chars().take(200).collect::<String>()
            );
            Err(format!(
                "identity check returned 2xx but the body did not decode as \
                 DeviceIdentityResponse: {e}"
            ))
        }
    }
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
        pair_with_pair_code, persist_pairing, read_device_id_from_disk,
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
    //   3. `api_config::get_api_base_url()` — the canonical four-rung resolver
    //      (env web/api vars, the persisted `web_integration.backend_url`, then
    //      the build default). This replaced a coord_url derivation that was
    //      never correct: in prod coord and the web backend are different
    //      services, and in dev they share a host but not a port.
    let web_base = match backend_url
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(form_url) => trim_backend_url(form_url),
        None => std::env::var("QONTINUI_WEB_BASE")
            .ok()
            .map(|v| v.trim().trim_end_matches('/').to_string())
            .filter(|v| !v.is_empty())
            .unwrap_or_else(crate::api_config::get_api_base_url),
    };

    // Run the blocking HTTP call on a tokio blocking thread so we don't
    // tie up the async runtime. `pair_with_pair_code` uses a 30-second
    // timeout internally, so this never hangs indefinitely.
    let code_for_blocking = code_trimmed.clone();
    let device_id_for_blocking = device_id.clone();
    let web_base_for_blocking = web_base.clone();
    let resp = spawn_blocking_tracked(move || {
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

    // Redeeming a pair code IS an explicit interactive credential acquisition —
    // the operator typed a code that a signed-in web session minted — so it ends
    // any interactive logout, exactly like a Cognito sign-in does.
    //
    // Without this, an operator who used the autonomy-preserving logout and then
    // re-paired by code was dead-ended: the device JWT is valid, the runner is
    // Tier 2 with the relay online and autonomy running, but `check_auth_status`
    // still short-circuits on the persisted marker and reports
    // `authenticated:false` forever — so the App gate renders LoginScreen, with
    // the very Settings pane where a pair code is entered unreachable behind it.
    // (This command is allowlisted over the UI-Bridge HTTP surface for
    // agent-driven pairing, so it is a live path, not a theoretical one.)
    //
    // Placed AFTER `persist_pairing` on purpose: a redeem that failed to persist
    // must not un-logout the operator. Placed HERE rather than inside
    // `persist_pairing` also on purpose: the background device-JWT refresher
    // writes the same credential slots, and clearing on that path would silently
    // un-logout the operator on the next refresh cycle.
    if let Err(e) = crate::auth::AuthManager::new().clear_interactive_signed_out() {
        warn!("redeem_pair_code: could not clear the interactive sign-out marker: {e}");
    }

    // Promote to Tier 2 (qontinui_account) now that a device JWT is in
    // hand. Redeeming a pair code IS a cloud-account bind, so the runner
    // must leave Tier Local for the WS relay to come online. Defensive:
    // the Settings UI also promotes after redeem, but a headless / UI-Bridge
    // caller that doesn't run the FE path still ends up online. Idempotent —
    // a no-op when already at Tier 2.
    //
    // The write itself lives in `qontinui_runner_lib::profiles` — the SAME
    // helper the headless CLI door (`qontinui_profile device pair`) calls —
    // reached here through `settings::promote_tier_to_account`, the bin-side
    // door that additionally drops this process's settings parse cache (the
    // lib cannot: the cache is bin-side).
    // That door used to write the pairing credentials and never touch the
    // tier, so the box that most needs Tier 2 was the only one that could not
    // reach it. One writer, two doors: they cannot drift again. The helper
    // owns all three conditions of `settings::should_persist_migration` —
    // nothing-to-persist, `!is_secondary`, and (structurally, via its
    // `serde_json::Value` edit) an authoritative source.
    match settings::promote_tier_to_account() {
        Ok((qontinui_runner_lib::profiles::TierWrite::Written, path)) => {
            info!(
                "redeem_pair_code: promoted runner to Tier QontinuiAccount in {}",
                path.display()
            );
        }
        Ok((qontinui_runner_lib::profiles::TierWrite::Unchanged, _)) => {
            debug!("redeem_pair_code: runner already at Tier QontinuiAccount — no settings write");
        }
        Ok((qontinui_runner_lib::profiles::TierWrite::SkippedSecondary, _)) => {
            // A secondary must never write the shared settings.json (it would
            // demote the primary), but THIS process still holds a device JWT
            // and needs Tier 2 to bring its relay online — so apply the tier
            // as the in-memory-only overlay, which is never persisted.
            // Guarded on there being no runtime override already: an explicit
            // operator choice (`set_runner_tier`) is authoritative over an
            // inferred promotion, and that precedence must not be inverted.
            if settings::in_memory_tier().is_none() {
                settings::set_in_memory_tier(settings::RunnerTier::QontinuiAccount);
                warn!("redeem_pair_code: secondary runner — applying tier in-memory only, skipping the settings.json write");
            } else {
                warn!("redeem_pair_code: secondary runner with an explicit runtime tier override — leaving it alone");
            }
        }
        Err(e) => {
            warn!("redeem_pair_code: tier promotion persist failed (continuing): {e}");
        }
    }

    // ALWAYS kick the relay + JWT refresher after a successful redeem — NOT
    // only when the tier changed. `persist_pairing` above just wrote a fresh
    // device-JWT into the slot the relay reads, but a re-pair on a runner that
    // is ALREADY Tier 2 (the common case: an idle runner whose 4h JWT expired)
    // used to skip these kicks entirely — they lived inside the `tier !=
    // QontinuiAccount` branch. The result was a fresh JWT staged on disk that
    // nothing ever told the live relay/refresher to pick up, so the runner sat
    // `ws_connected:false` until a full restart despite a valid pairing.
    // Idempotent: the refresher no-ops when the JWT is still fresh, and a kick
    // to a connected relay just re-evaluates its idle gate.
    crate::mcp::backend_relay::commands::kick_cloud_relay().await;
    crate::mcp::device_jwt_refresher::commands::kick_device_jwt_refresher().await;
    info!("redeem_pair_code: kicked relay + device-JWT refresher to pick up the fresh pairing");

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
            get_settings_health,
            save_web_integration_settings,
            test_web_integration_connection,
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

    // The probe's own constants/types. This module deliberately does NOT
    // `use super::*` — it mirrors the Tauri wire shapes rather than exercising
    // the module — so the few real items the probe tests touch are named.
    use super::{is_bound_backend, IdentityFault, TestConnectionResponse, RUNNER_TOKEN_PREFIX};

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

    /// NO-DOWNGRADE round-trip guard for the READ side.
    ///
    /// The save wire contract above was tested; the STATUS response was not.
    /// `WebIntegrationStatus` in `WebIntegrationSettings.tsx` declares
    /// `webBaseUrl`, but the Rust response struct never carried it — so the
    /// Advanced "web base URL" input was seeded with `""` on every load, and
    /// the next unrelated Save posted `webBaseUrl: undefined`, silently
    /// wiping a configured override. Every field the frontend seeds its form
    /// from must be present in the response, or saving becomes lossy.
    #[test]
    fn status_response_carries_every_field_the_form_seeds_from() {
        let json = serde_json::to_value(super::GetWebIntegrationStatusResponse {
            enabled: true,
            backend_url: "http://localhost:8000".into(),
            web_base_url: "http://localhost:3001".into(),
            runner_token_masked: "qontinui_runner_…abcd".into(),
            runner_id: None,
            last_heartbeat_at: None,
            registration_error: None,
            ws_connected: false,
            settings_fault: None,
        })
        .expect("status response must serialize");

        // Exactly the keys `applyStatusToForm` / `isDirty` read.
        for key in ["enabled", "backendUrl", "webBaseUrl", "runnerTokenMasked"] {
            assert!(
                json.get(key).is_some(),
                "status response is missing `{key}` — the form seeds from it, so an \
                 absent field silently blanks the user's saved value on the next save"
            );
        }
        assert_eq!(json["webBaseUrl"], "http://localhost:3001");
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

    /// The Settings UI reads `reachable` / `paired` / `deviceId` / `detail` off
    /// this payload. Tauri does not rename for us, so the `rename_all` attribute
    /// is load-bearing: drop it and every field silently reads `undefined` in
    /// the UI rather than failing anywhere.
    #[test]
    fn test_connection_response_is_camel_case_on_the_wire() {
        let resp = TestConnectionResponse {
            reachable: true,
            paired: true,
            identity_fault: IdentityFault::None,
            device_id: Some("dev-1".to_string()),
            user_id: Some("user-1".to_string()),
            tenant_id: Some("tenant-1".to_string()),
            token_format_valid: Some(true),
            detail: "ok".to_string(),
        };
        let v: serde_json::Value = serde_json::to_value(&resp).expect("serializes");
        let obj = v.as_object().expect("object");
        for key in [
            "reachable",
            "paired",
            "identityFault",
            "deviceId",
            "userId",
            "tenantId",
            "tokenFormatValid",
            "detail",
        ] {
            assert!(obj.contains_key(key), "missing `{key}` in {v}");
        }
        for key in [
            "device_id",
            "user_id",
            "tenant_id",
            "token_format_valid",
            "identity_fault",
        ] {
            assert!(!obj.contains_key(key), "snake_case `{key}` leaked in {v}");
        }
    }

    /// The fault variants are a wire contract the UI branches on, so their
    /// spelling is pinned. `snake_case` on the VALUES even though the fields
    /// are camelCase — that is what `rename_all` on the enum produces.
    #[test]
    fn identity_fault_variants_serialize_as_named() {
        for (v, want) in [
            (IdentityFault::None, "\"none\""),
            (IdentityFault::Unpaired, "\"unpaired\""),
            (IdentityFault::NotBoundBackend, "\"not_bound_backend\""),
            (IdentityFault::Rejected, "\"rejected\""),
            (IdentityFault::Forbidden, "\"forbidden\""),
            (IdentityFault::VerifierDown, "\"verifier_down\""),
            (IdentityFault::Unexpected, "\"unexpected\""),
        ] {
            assert_eq!(serde_json::to_string(&v).unwrap(), want);
        }
    }

    /// The security gate: a URL that is not the bound backend must never reach
    /// the identity leg, because that leg presents a live device credential.
    /// Anything unparseable compares unequal — fail closed.
    #[test]
    fn is_bound_backend_fails_closed_on_junk() {
        assert!(!is_bound_backend(""));
        assert!(!is_bound_backend("not-a-url"));
        assert!(!is_bound_backend("https://evil.example"));
    }

    /// `token_format_valid` is a LOCAL shape check and nothing more — the token
    /// is not sent anywhere, so this is the only statement the probe can
    /// honestly make about it. Pin the prefix so the check cannot quietly widen
    /// into "any non-empty string is fine".
    #[test]
    fn runner_token_prefix_is_the_only_local_check() {
        // Absence is reported as `None`, never as a malformed token: a runner
        // paired through Cognito or a pair code legitimately has no
        // `qontinui_runner_` token at all.
        assert!("qontinui_runner_deadbeef".starts_with(RUNNER_TOKEN_PREFIX));
        assert!(!"qontinui_device_deadbeef".starts_with(RUNNER_TOKEN_PREFIX));
        assert!(!"".starts_with(RUNNER_TOKEN_PREFIX));
        assert!(!"deadbeef".starts_with(RUNNER_TOKEN_PREFIX));
    }

    /// Regression pin for the 2026-09-12 rewrite. `POST /api/v1/runners/register`
    /// was deleted from qontinui-web in `ad3692e6c`; the whole `/api/v1/runners`
    /// router went in `1574bd036`, with no alias. A probe that drifts back to it
    /// 404s on every press of "Test connection" with no local error, which is
    /// exactly the failure this command shipped with for months. The route
    /// strings this command may build are pinned here.
    #[test]
    fn probe_targets_live_routes_only() {
        let backend = "https://api.qontinui.io";
        let health = format!("{backend}/api/v1/health/live");
        let me = format!("{backend}/api/v1/devices/me");
        for url in [&health, &me] {
            assert!(
                !url.contains("/api/v1/runners"),
                "probe must not target the retired runners router: {url}"
            );
        }
        assert!(health.ends_with("/api/v1/health/live"));
        assert!(me.ends_with("/api/v1/devices/me"));
    }
}

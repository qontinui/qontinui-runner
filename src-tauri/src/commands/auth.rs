//! Authentication command handlers for Tauri.
//!
//! This module provides Tauri commands for user authentication, including:
//! - Cognito Hosted-UI sign-in (RFC 8252 PKCE) — the runner's ONLY login path
//! - Logout / sign-out
//! - Authentication status checking
//! - Device information retrieval
//!
//! ## Web-backend authentication (Cognito-only)
//!
//! The web backend (`qontinui-web`) no longer exposes a local FastAPI-Users
//! login. `POST /api/v1/auth/jwt/login`, `POST /api/v1/auth/jwt/refresh`, and
//! the local HS256 token verification are gone server-side; every request is
//! authenticated via the runner's **Cognito** identity. Accordingly, this
//! module:
//!
//! - Has NO email/password login command, no `/jwt/login` POST, and no
//!   `/jwt/refresh` (all deleted — the legacy local-auth path).
//! - Authenticates its web-backend calls (`/api/v1/auth/users/me`,
//!   `/api/v1/projects`) with the runner's Cognito **access token**, refreshed
//!   first via [`device_jwt_refresher::ensure_fresh_cognito_bearer`] when stale.
//! - Drives all sign-in through [`cognito_sign_in`] (system browser + PKCE),
//!   which is wired as the runner's primary login command (see
//!   `AccountSettings.tsx`).

use crate::auth::AuthManager;
use crate::commands::compartments::HealthCompartment;
use crate::error::AppError;
use crate::mcp::device_jwt_refresher::ensure_fresh_cognito_bearer;
use serde::{Deserialize, Serialize};
use std::sync::atomic::Ordering;
use tauri::plugin::{Builder as PluginBuilder, TauriPlugin};
use tauri::Runtime;
use tracing::{error, info, warn};

use crate::api_config::get_api_base_url;
use crate::settings;

/// Resolve the user bearer the runner presents to the web backend for
/// Cognito-authenticated calls (`/api/v1/auth/users/me`, `/api/v1/projects`).
///
/// Returns the runner's Cognito **access token**, refreshed first via the
/// shared [`ensure_fresh_cognito_bearer`] helper (Cognito `refresh_token`
/// grant) when the stored token is within the refresh threshold. The web
/// backend's Cognito arm verifies this token and resolves the user — there is
/// no local-login token anymore.
///
/// `Err(AuthError)` when no Cognito session is present (the runner must sign in
/// via [`cognito_sign_in`] first).
async fn web_backend_user_bearer(auth_manager: &AuthManager) -> Result<String, AppError> {
    match ensure_fresh_cognito_bearer(auth_manager).await {
        Some(t) if !t.trim().is_empty() => Ok(t.trim().to_string()),
        _ => Err(AppError::AuthError(
            "Not signed in to Qontinui. Sign in via Settings → Account.".to_string(),
        )),
    }
}

/// Pure-input variant of [`require_tier_2`] used by tests. Reject any tier
/// other than `QontinuiAccount` with the canonical error message.
///
/// Extracted so the tier-matrix calibration suite (Phase 9 of the runner
/// tier-decoupling plan) can exercise the gate without going through disk
/// I/O or `load_settings`.
pub(crate) fn require_tier_2_for(tier: settings::RunnerTier) -> Result<(), AppError> {
    if tier != settings::RunnerTier::QontinuiAccount {
        return Err(AppError::AuthError(
            "Tier 0/1 (Local / LocalProvider) — Qontinui account commands are unavailable. \
             Sign in via Settings → Account to enable."
                .to_string(),
        ));
    }
    Ok(())
}

/// Returns Err with a structured "Tier 0/1 — no auth" message when the
/// runner is definitively not in Tier 2. All cloud-reaching auth commands
/// gate on this.
///
/// NO-DOWNGRADE (C4): a settings-read failure used to fail closed here with
/// the Tier 0/1 message, telling a Tier 2 user to "Sign in via Settings →
/// Account" — the wrong remediation for a corrupt settings.json, and it also
/// blocked `logout` / `sign_out_full`, so the user could not even get out.
/// An unresolvable tier is now a distinct [`AppError::ConfigError`]: it names
/// the real fault and, unlike `AuthError`, does not render a sign-in CTA.
fn require_tier_2() -> Result<(), AppError> {
    match settings::resolve_tier() {
        settings::TierResolution::Known(tier) => require_tier_2_for(tier),
        settings::TierResolution::Unknown { reason } => {
            error!("require_tier_2: runner tier is UNKNOWN — {reason}");
            Err(AppError::ConfigError(format!(
                "Runner tier could not be determined, so this command was not run \
                 (your account state is unchanged). {reason}"
            )))
        }
    }
}

/// User information returned after login
#[derive(Debug, Serialize, Deserialize)]
pub struct UserInfo {
    pub id: String,
    pub email: String,
    pub name: Option<String>,
}

/// Authentication status information
#[derive(Debug, Serialize, Deserialize)]
pub struct AuthStatus {
    pub authenticated: bool,
    pub user: Option<UserInfo>,
    pub device_id: Option<String>,
    /// `true` when the credential store file EXISTS but cannot be
    /// decrypted/parsed — the "corrupt / wrong-machine key" case (a machine
    /// rename / disk move / re-image invalidates the hostname+username-derived
    /// AES key). The read side fails closed to the LoginScreen; this flag lets
    /// the frontend explain WHY and offer a reset (`reset_credential_store`)
    /// rather than showing a bare, unexplained sign-in prompt. Always `false`
    /// for Tier 0/1 and for a readable/absent store.
    #[serde(default)]
    pub store_unreadable: bool,
}

/// Device information for registration
#[derive(Debug, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub device_id: String,
    pub device_name: String,
    pub platform: String,
}

/// User info from API (/api/v1/auth/users/me)
/// Maps to UserRead schema from backend
#[derive(Debug, Serialize, Deserialize)]
struct ApiUserInfo {
    id: String,
    email: String,
    username: String,
    full_name: Option<String>,
    is_verified: bool,
    is_active: bool,
    #[serde(default)]
    tenant_id: Option<String>,
}

/// Gets the current platform name
fn get_platform() -> String {
    std::env::consts::OS.to_string()
}

/// Generates a device name based on hostname and platform
fn get_device_name() -> String {
    let hostname = hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .unwrap_or_else(|| "Unknown".to_string());

    let platform = match std::env::consts::OS {
        "macos" => "Mac",
        "windows" => "Windows PC",
        "linux" => "Linux PC",
        other => other,
    };

    format!("{} ({})", hostname, platform)
}

/// Logs out the current user WITHOUT stopping autonomous terminal sessions.
///
/// This is the DEFAULT logout: it clears only the interactive device-JWT
/// session and PRESERVES the Cognito (`oauth_*`) session, then kicks the
/// device-JWT refresher so a fresh device JWT is re-minted immediately. The
/// runner's background daemons keep driving autonomous terminal AI sessions
/// across the logout — there is no multi-minute autonomy gap.
///
/// Use [`sign_out_full`] to fully sign out and STOP autonomous sessions.
///
/// # Errors
///
/// Returns an error string if clearing fails.
#[tauri::command]
pub async fn logout() -> Result<(), String> {
    logout_impl().await.map_err(String::from)
}

async fn logout_impl() -> Result<(), AppError> {
    require_tier_2()?;
    info!("Logout requested (interactive session only — autonomous sessions preserved)");

    let auth_manager = AuthManager::new();

    // Phase 3: there's no longer a separate `/api/v1/runner-devices/{id}`
    // backend record to deactivate. The runner's presence is governed by
    // the unified WebSocket relay; closing the WS (which happens on token
    // revocation) is the equivalent of "logging the device out".
    //
    // Clear ONLY the interactive device-JWT session; preserve the Cognito
    // session so autonomy survives the logout.
    auth_manager.clear_interactive_session()?;

    // Re-mint the device JWT right away from the preserved Cognito session so
    // the autonomous daemons don't see even a transient missing-token window.
    crate::mcp::device_jwt_refresher::commands::kick_device_jwt_refresher().await;

    info!("Logout successful — autonomous terminal sessions preserved (device JWT re-minting from the kept Cognito session)");
    Ok(())
}

/// Fully signs out and STOPS the runner's autonomous terminal sessions.
///
/// Unlike [`logout`], this clears ALL credentials — the device-JWT pair AND
/// the Cognito (`oauth_*`) session, including the long-lived
/// `oauth_refresh_token`. With the Cognito session gone the device-JWT
/// refresher can no longer self-recover, so the background daemons stop
/// driving autonomous sessions until an interactive re-login. This is the
/// explicit "stop autonomy" sign-out.
///
/// # Errors
///
/// Returns an error string if clearing fails.
#[tauri::command]
pub async fn sign_out_full() -> Result<(), String> {
    sign_out_full_impl().await.map_err(String::from)
}

async fn sign_out_full_impl() -> Result<(), AppError> {
    require_tier_2()?;
    info!("Full sign-out requested — clearing ALL credentials (autonomous sessions will stop)");

    let auth_manager = AuthManager::new();
    auth_manager.clear_all_credentials()?;

    info!(
        "Full sign-out successful — autonomous terminal sessions stopped (Cognito session wiped)"
    );
    Ok(())
}

/// Deletes a corrupt/undecryptable credential store so the operator can sign in
/// again. The reset affordance behind the LoginScreen "your credential store is
/// corrupt" banner (`AuthStatus::store_unreadable`).
///
/// When the `.enc` is present-but-unreadable (a machine rename / disk move /
/// re-image invalidates the hostname+username-derived AES key), the read side
/// fails closed and the operator lands on the LoginScreen. Deleting the file
/// turns the next launch into a clean first-run: absent store ⇒ no
/// interactive-sign-out marker, and the next sign-in writes succeed. If a valid
/// device token still lives in the OS keychain (which is keyed per OS-user, not
/// by the dead `.enc` key), `get_access_token`'s migration path re-seeds a fresh
/// `.enc` with the correct current-machine key — a transparent self-heal.
///
/// # Errors
///
/// Returns an error string if the file exists but cannot be removed.
#[tauri::command]
pub async fn reset_credential_store() -> Result<(), String> {
    info!("reset_credential_store: deleting the credential store to allow a fresh sign-in");
    let storage = crate::secure_storage::SecureStorage::new().map_err(|e| {
        let msg = format!("could not open secure storage: {e}");
        error!("reset_credential_store: {msg}");
        msg
    })?;
    storage.delete_storage().map_err(|e| {
        let msg = format!("could not delete credential store: {e}");
        error!("reset_credential_store: {msg}");
        msg
    })?;
    info!(
        "reset_credential_store: credential store deleted — LoginScreen sign-in will start fresh"
    );
    Ok(())
}

/// Checks the current authentication status.
///
/// This command:
/// 1. Resolves the runner's Cognito access token (signed-in check)
/// 2. Validates it by calling the /api/v1/auth/users/me endpoint (Cognito arm)
/// 3. Returns authentication status with user information if authenticated
///
/// # Errors
///
/// Returns an error string if the validation request fails on a transient error.
#[tauri::command]
pub async fn check_auth_status() -> Result<AuthStatus, String> {
    check_auth_status_impl().await.map_err(String::from)
}

/// Wall-clock cap on the best-effort profile enrichment inside
/// [`check_auth_status_impl`]. The authenticated/unauthenticated verdict is
/// decided from local credentials alone, so exceeding this budget costs only
/// the `user` field — never the verdict.
const ENRICHMENT_BUDGET: std::time::Duration = std::time::Duration::from_secs(3);

/// Sub-budget for the FIRST of enrichment's two network legs — the bearer
/// resolution, which performs a Cognito `refresh_token` grant when the stored
/// token is stale.
///
/// Without a sub-budget the two legs share one clock, so a slow grant starves
/// the `users/me` GET behind it and enrichment times out as a whole — the
/// profile is lost on precisely the calls where a refresh happened to fall
/// due. Capping the first leg separately bounds that: on overrun we fall back
/// to the STORED access token (the same REPLACE-not-REVOKE posture
/// `refresh_cognito_bearer` takes on a failed grant) and still spend the
/// remaining budget on the profile fetch.
const BEARER_BUDGET: std::time::Duration = std::time::Duration::from_secs(1);

async fn check_auth_status_impl() -> Result<AuthStatus, AppError> {
    info!("Checking authentication status");

    // Tier 0/1 — never reach the backend, never touch the keychain.
    // Defense in depth: Phase 1 frontend doesn't call this in Tier 0/1, but
    // any caller that does must get an unambiguous "not authenticated".
    //
    // NO-DOWNGRADE (C5): an UNKNOWN tier must NOT be answered with
    // `Ok(authenticated: false)`. A successful command carrying a downgraded
    // verdict is settled as far as the frontend is concerned — the hardened
    // AuthProvider retry chain will not retry it — so a transient
    // settings-read failure became an automatic logout that survived every
    // frontend safeguard. Returning `Err` keeps it in the retryable class and
    // renders the real reason instead of the LoginScreen.
    match settings::resolve_tier() {
        settings::TierResolution::Known(settings::RunnerTier::QontinuiAccount) => {}
        settings::TierResolution::Known(_) => {
            return Ok(AuthStatus {
                authenticated: false,
                user: None,
                device_id: None,
                store_unreadable: false,
            });
        }
        settings::TierResolution::Unknown { reason } => {
            error!("check_auth_status: runner tier is UNKNOWN — {reason}");
            return Err(AppError::ConfigError(format!(
                "Could not determine the runner tier, so the sign-in state is unknown \
                 (you have NOT been signed out). {reason}"
            )));
        }
    }

    let auth_manager = AuthManager::new();
    let device_id = auth_manager.get_device_id().ok();

    // Surface a present-but-undecryptable store so the LoginScreen can offer a
    // reset instead of a bare sign-in prompt. This is the same condition the
    // read side fails closed on (`is_interactively_signed_in` → false below), so
    // the operator would otherwise land on an unexplained LoginScreen.
    let store_unreadable = auth_manager.is_store_present_but_unreadable();

    // Authoritative signal: `AuthManager::is_interactively_signed_in` — the
    // operator has not explicitly logged out AND the runner holds local
    // credentials. True immediately after a successful `cognito_sign_in` (device
    // paired + Cognito tokens stored) AND on a fresh boot that restored those
    // credentials from disk. Anything else means the runner has genuinely never
    // signed in, or was explicitly logged out — report unauthenticated so the
    // frontend shows LoginScreen. (Both cases log their reason inside the
    // helper.)
    //
    // `device_id: None` here matches the tier short-circuit above; the device
    // may well still be paired and running autonomy after an
    // autonomy-preserving logout, but an unauthenticated status reports no
    // device by convention.
    if !auth_manager.is_interactively_signed_in() {
        return Ok(AuthStatus {
            authenticated: false,
            user: None,
            device_id: None,
            store_unreadable,
        });
    }

    // We ARE signed in. Best-effort: enrich the status with the user's
    // profile via `/api/v1/auth/users/me`. A failure here (network error, or
    // even a 401/403 — the federated-identity `users/me` gap) does NOT
    // downgrade `authenticated`: a valid local device pairing is proof the
    // runner is signed in. We only use the response to populate `user`.
    //
    // The enrichment is two NETWORK legs — `ensure_fresh_cognito_bearer` (a
    // Cognito `refresh_token` grant when the stored token is stale) and the
    // `users/me` GET (its own 5s client timeout) — so left unbounded it can
    // hold this command open well past any caller's patience. That matters
    // because the verdict above is already decided from the LOCAL keychain:
    // a caller that gives up waiting has no way to tell "slow enrichment"
    // from "not signed in" and renders the sign-in screen at a runner that
    // is fully authenticated (observed on pop-out terminal windows, whose
    // fresh webview probes auth while the app is still booting).
    // Cap the whole enrichment so the verdict is never held hostage to it, and
    // cap the bearer leg WITHIN that so it cannot starve the profile fetch —
    // see [`BEARER_BUDGET`].
    let user = match tokio::time::timeout(ENRICHMENT_BUDGET, async {
        let bearer =
            match tokio::time::timeout(BEARER_BUDGET, web_backend_user_bearer(&auth_manager)).await
            {
                Ok(Ok(access_token)) => Some(access_token),
                // Not signed in to Cognito at all — nothing to enrich with.
                Ok(Err(_)) => None,
                Err(_) => {
                    // The grant outran its sub-budget. Use the stored token: it is
                    // what `refresh_cognito_bearer` itself would have returned had
                    // the grant failed outright, and a token that is merely near
                    // expiry still authenticates a `users/me` GET.
                    warn!(
                        "check_auth_status: bearer resolution exceeded {BEARER_BUDGET:?} — \
                         falling back to the stored access token for profile enrichment"
                    );
                    auth_manager
                        .get_oauth_access_token()
                        .ok()
                        .filter(|t| !t.trim().is_empty())
                }
            };
        match bearer {
            Some(access_token) => fetch_user_info(&access_token).await,
            None => None,
        }
    })
    .await
    {
        Ok(user) => user,
        Err(_) => {
            warn!(
                "check_auth_status: user enrichment exceeded {:?} — returning authenticated \
                 status without profile",
                ENRICHMENT_BUDGET
            );
            None
        }
    };

    info!(
        "Runner authenticated via local session (user enrichment: {})",
        if user.is_some() { "ok" } else { "unavailable" }
    );

    Ok(AuthStatus {
        authenticated: true,
        user,
        device_id,
        // A signed-in status means the store read succeeded, so it is readable
        // by construction; carry the computed value for completeness.
        store_unreadable,
    })
}

/// Best-effort fetch of the signed-in user's profile from the web backend.
/// Returns `None` on any failure (network error, 4xx/5xx, malformed body) —
/// callers treat a missing profile as "authenticated but unenriched", never as
/// a sign-out signal.
async fn fetch_user_info(access_token: &str) -> Option<UserInfo> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .ok()?;
    let response = client
        .get(format!("{}/api/v1/auth/users/me", get_api_base_url()))
        .bearer_auth(access_token)
        .send()
        .await
        .ok()?;
    if !response.status().is_success() {
        warn!(
            "users/me returned {} — keeping local-session authentication, skipping user enrichment",
            response.status()
        );
        return None;
    }
    let user_info: ApiUserInfo = response.json().await.ok()?;
    Some(UserInfo {
        id: user_info.id,
        email: user_info.email,
        name: user_info.full_name,
    })
}

/// Gets information about the current device.
///
/// Returns the device ID, device name, and platform.
///
/// # Errors
///
/// Returns an error string if device ID retrieval fails.
#[tauri::command]
pub async fn get_device_info() -> Result<DeviceInfo, String> {
    get_device_info_impl().await.map_err(String::from)
}

async fn get_device_info_impl() -> Result<DeviceInfo, AppError> {
    info!("Getting device info");

    let auth_manager = AuthManager::new();
    let device_id = auth_manager.get_device_id()?;

    let device_name = get_device_name();
    let platform = get_platform();

    Ok(DeviceInfo {
        device_id,
        device_name,
        platform,
    })
}

/// Project information
#[derive(Debug, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// Gets all projects accessible to the current user.
///
/// This command:
/// 1. Retrieves the access token from keychain
/// 2. Calls the backend API to get all accessible projects
/// 3. Returns the list of projects
///
/// # Errors
///
/// Returns an error string if:
/// - Not authenticated (no tokens)
/// - Backend API call fails
#[tauri::command]
pub async fn get_user_projects() -> Result<Vec<Project>, String> {
    get_user_projects_impl().await.map_err(String::from)
}

async fn get_user_projects_impl() -> Result<Vec<Project>, AppError> {
    require_tier_2()?;
    info!("Getting user projects");

    let auth_manager = AuthManager::new();

    // Authenticate with the runner's Cognito access token (refresh-first).
    let access_token = web_backend_user_bearer(&auth_manager).await?;

    // Call backend API
    let client = reqwest::Client::new();
    let response = client
        .get(format!("{}/api/v1/projects", get_api_base_url()))
        .bearer_auth(&access_token)
        .send()
        .await?;

    if !response.status().is_success() {
        let status = response.status().as_u16();
        let body = response.text().await.unwrap_or_default();
        error!("Get projects failed with status {}: {}", status, body);
        return Err(AppError::HttpStatusError { status, body });
    }

    let projects: Vec<Project> = response.json().await?;

    info!("Retrieved {} projects", projects.len());
    Ok(projects)
}

/// Get access token for WebSocket connection
///
/// Returns the current access token from the keychain.
/// This is used by the frontend to configure WebSocket connections.
///
/// # Errors
///
/// Returns an error string if:
/// - Not authenticated (no token found)
/// - Keychain access fails
#[tauri::command]
pub async fn get_access_token_for_websocket() -> Result<String, String> {
    get_access_token_for_websocket_impl()
        .await
        .map_err(String::from)
}

async fn get_access_token_for_websocket_impl() -> Result<String, AppError> {
    require_tier_2()?;
    info!("Getting access token for WebSocket");

    let auth_manager = AuthManager::new();

    // Check authentication
    if !auth_manager.has_tokens() {
        return Err(AppError::AuthError(
            "Not authenticated. Please log in first.".to_string(),
        ));
    }

    // Get access token
    let access_token = auth_manager.get_access_token()?;

    info!("Access token retrieved for WebSocket");
    Ok(access_token)
}

/// Check if the HTTP API server is ready to accept requests.
///
/// The frontend calls this on mount to detect if the API server started
/// before the event listener was set up (e.g., after a page refresh).
#[tauri::command]
pub fn is_api_ready(health: tauri::State<'_, HealthCompartment>) -> bool {
    health.api_ready().load(Ordering::Relaxed)
}

/// Get the actual port the HTTP API server is listening on.
///
/// Returns the port the server bound to (may differ from default 9876 if
/// `QONTINUI_PORT` env var was set or if the primary port was occupied).
#[tauri::command]
pub fn get_api_port(health: tauri::State<'_, HealthCompartment>) -> u16 {
    health.api_port().load(Ordering::Relaxed)
}

/// Returns the current runner tier. Frontend gates its auth-touching effects
/// on this — see `AuthProvider.tsx`.
///
/// NO-DOWNGRADE: when settings.json cannot be read the tier is UNKNOWN, and
/// this returns `Err` rather than the string `"local"`. Answering `"local"`
/// made cloud features silently vanish on a transient read error; an `Err`
/// keeps `useRunnerTier` in its retryable/unknown state instead.
#[tauri::command]
pub fn get_runner_tier() -> Result<String, String> {
    let resolved = settings::resolve_tier();
    match &resolved {
        // String form so the React side doesn't need a TS enum mirror.
        settings::TierResolution::Known(_) => Ok(resolved.as_str().to_string()),
        settings::TierResolution::Unknown { reason } => {
            error!("get_runner_tier: tier is UNKNOWN — {reason}");
            Err(reason.clone())
        }
    }
}

/// Wire response for [`set_runner_tier`].
///
/// The command used to return a bare `Ok(())`, which made "the tier was saved"
/// and "the tier was silently discarded" indistinguishable to the frontend —
/// on a secondary runner the whole command was a no-op. The two facts are now
/// reported separately so a caller can tell them apart:
///
/// - `applied` — the tier is in effect for this process (`get_runner_tier` will
///   answer with it).
/// - `persisted` — the tier survives a restart, i.e. it was written to this
///   instance's `settings.json`.
/// - `reason` — why `persisted` is false, when it is.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SetRunnerTierResult {
    /// The tier is in effect for the remainder of this process's lifetime.
    pub applied: bool,
    /// The tier was written to disk and survives a restart.
    pub persisted: bool,
    /// Why `persisted` is false. `None` when it is true.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Sets the current runner tier and persists. Used by the SetupWizard's
/// TierStep and the AccountSettings sign-in completion handler.
///
/// On a **secondary** runner (any supervisor-launched temp/named instance) the
/// tier is applied IN MEMORY ONLY and `persisted: false` is reported. Writing
/// would target the primary's shared `settings.json` and silently demote a
/// Tier-2 primary to whatever tier the temp runner picked — see the FOOTGUN
/// GUARD in `settings::load_settings_full`. Applying in memory is what lets a
/// temp runner exercise the same `isTier2` transition a primary does; without
/// it every wizard test on a temp runner validated a path that changed neither
/// disk nor memory.
///
/// `async fn`, not `pub fn`, on purpose: `tauri-macros` maps a NON-async
/// command to its `ExecutionContext::Blocking` arm (`command/wrapper.rs:50`,
/// `:116`), which emits `body_blocking` — the command body runs INLINE on the
/// thread that dispatched the IPC message. That thread is the webview's UI
/// thread (wry calls the custom-protocol handler straight from the
/// `WebResourceRequested` event handler, `wry-0.55.0/src/webview2/mod.rs:1027`,
/// with no thread hop), so the settings read/write below would block the whole
/// UI for the duration of any OS stall on the config dir. `async` + an explicit
/// `spawn_blocking` keeps the file I/O off it.
#[tauri::command]
pub async fn set_runner_tier(tier: String) -> Result<SetRunnerTierResult, String> {
    let parsed = match tier.as_str() {
        "local" => settings::RunnerTier::Local,
        "local_provider" => settings::RunnerTier::LocalProvider,
        "qontinui_account" => settings::RunnerTier::QontinuiAccount,
        other => return Err(format!("invalid tier: {}", other)),
    };
    let is_secondary = crate::instance::is_secondary();
    // Blocking file I/O (`fs::read_to_string`, `create_dir_all`, and the
    // atomic write's `File::create`/`write_all`/`sync_all`/`rename`) off the
    // UI thread. The in-memory branch is cheap but goes through the same hop
    // so both arms have one shape.
    let result = tauri::async_runtime::spawn_blocking(move || {
        if is_secondary {
            // NOT a no-op any more: actually apply the tier for this process.
            settings::set_in_memory_tier(parsed);
            warn!(
                "set_runner_tier: secondary runner — applied in memory only (tier={:?}), \
                 skipping save_settings so the primary's shared settings.json is not clobbered",
                parsed
            );
            Ok(SetRunnerTierResult {
                applied: true,
                persisted: false,
                reason: Some("secondary_runner".to_string()),
            })
        } else {
            // Provenance-checked write: refuses (loudly) rather than clobbering an
            // unreadable settings.json with an all-defaults file that happens to
            // carry the new tier.
            settings::update_settings(|s| {
                s.tier = parsed;
                s.tier_initialized = true;
            })
            .map(|()| SetRunnerTierResult {
                applied: true,
                persisted: true,
                reason: None,
            })
        }
    })
    .await
    .map_err(|e| format!("set_runner_tier: settings task failed: {e}"))??;

    // Kick both the relay (so it picks up the new tier without a runner
    // restart) and the device-JWT refresher (Phase 2 of unified-devices
    // — promotion into Tier 2 should trigger a JWT refresh check, and
    // demotion out of it should let the refresher idle). The refresher's
    // `next_action` predicate already handles the tier branching; we
    // just need to wake it.
    //
    // Use `tauri::async_runtime::spawn`, NOT `tokio::spawn`: the Tauri async
    // runtime handle is always available and routes to the same Tokio runtime
    // the relay/refresher live on, whereas a bare `tokio::spawn` from a thread
    // with no entered runtime context panics with "there is no reactor
    // running" (a non-unwinding panic that aborts the whole process).
    tauri::async_runtime::spawn(async {
        crate::mcp::backend_relay::commands::kick_cloud_relay().await;
        crate::mcp::device_jwt_refresher::commands::kick_device_jwt_refresher().await;
    });
    Ok(result)
}

/// Wire response for [`cognito_sign_in`]. Mirrors the credentials/pair-code
/// confirmation shape so the FE shows a uniform "signed in" panel.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CognitoSignInResponse {
    /// Cognito `sub` (the user id used to bind the device).
    pub user_id: String,
    /// `email` claim from the Cognito id token, if present.
    pub email: Option<String>,
    /// Tenant the coord device JWT was minted under (decoded from the minted
    /// device JWT — coord stamps it; the Cognito token does not carry it).
    pub tenant_id: Option<String>,
    /// The paired device id.
    pub device_id: String,
}

/// Sign in to the runner with a **Cognito** account via RFC 8252 (system
/// browser + PKCE + loopback redirect), then bind the coord device-token JWT
/// to that user.
///
/// Phase 5 of the unified-Cognito-identity plan. Flow:
///   1. RFC-8252 PKCE login against the Cognito Hosted UI
///      (`cognito::pkce_login` — system browser + fixed-port loopback). Yields
///      `{access_token, id_token, refresh_token, expires_at, sub, email}`.
///   2. Persist the Cognito tokens in the distinct oauth slots
///      (`AuthManager::store_oauth_tokens`) — kept separate from the coord
///      device-JWT slot so the WS relay keeps using the device JWT while
///      user-facing calls use the Cognito token.
///   3. Bind device→user: reuse the EXISTING web-backend `pair-cli` flow
///      (`pair::pair_with_auth_token_with_ids`) with the Cognito **access
///      token** as the user bearer and the Cognito `sub` as
///      `X-Qontinui-User-Id`, so the minted device JWT is user-bound. (Swaps
///      the token *source* — Cognito instead of local-login JWT — not the
///      endpoint.) The web backend resolves `tenant_id` server-side.
///   4. Persist the device JWT, promote to Tier 2, kick the relay + refresher.
///
/// `backend_url` is the web-backend base (e.g. `https://api.qontinui.io`). It
/// is required and explicit (NOT `get_api_base_url()`) so a debug build can
/// target prod — symmetric with [`pair_with_credentials`].
///
/// `identity_provider` optionally selects a federated IdP to jump straight into
/// (Cognito provider name — `Google`, `MicrosoftEntra`, `GitHub`). When `None`
/// the Hosted UI shows its native email/password + chooser screen (unchanged
/// "Sign in with Qontinui" behaviour).
#[tauri::command]
pub async fn cognito_sign_in(
    backend_url: String,
    identity_provider: Option<String>,
) -> Result<CognitoSignInResponse, String> {
    cognito_sign_in_impl(backend_url, identity_provider)
        .await
        .map_err(String::from)
}

async fn cognito_sign_in_impl(
    backend_url: String,
    identity_provider: Option<String>,
) -> Result<CognitoSignInResponse, AppError> {
    let base = backend_url.trim().trim_end_matches('/').to_string();
    if base.is_empty() {
        return Err(AppError::Raw("backend_url is required".to_string()));
    }

    // 1. RFC-8252 PKCE login (blocking — browser + loopback). Run on a
    //    worker thread so it doesn't block the tokio runtime.
    info!("cognito_sign_in: starting RFC-8252 PKCE login");
    let login = tokio::task::spawn_blocking(move || {
        qontinui_runner_lib::cognito::pkce_login(identity_provider.as_deref())
    })
    .await
    .map_err(|e| {
        error!("cognito_sign_in: PKCE login task panicked: {e}");
        AppError::Raw(format!("PKCE login task panicked: {e}"))
    })?
    .map_err(|e| {
        // Most common cause: a prior sign-in leaked the fixed loopback port
        // (BUG 1, now fixed) so the bind fails with os error 10048.
        error!("cognito_sign_in: PKCE login failed at step 1 (pkce_login): {e}");
        AppError::Raw(e)
    })?;

    info!(
        "cognito_sign_in: PKCE login succeeded (sub={}) — binding device",
        login.sub
    );

    // Run the shared post-auth chain (store tokens → pair → persist → promote).
    finalize_signed_in(login, base).await
}

/// Shared post-authentication chain used by BOTH the Hosted-UI PKCE sign-in
/// ([`cognito_sign_in`]) and the direct credential sign-in
/// ([`cognito_sign_in_password`]). Given a freshly obtained
/// [`CognitoLoginResult`] (from either flow), this:
///
///   2. Persists the Cognito user tokens in the distinct oauth slots.
///   3. Reads the device identity from disk.
///   4. Binds device→user via the existing web-backend `pair-cli` flow,
///      presenting the Cognito access token + `sub`.
///   5. Persists the minted coord device JWT + paired-user file.
///   6. Promotes the runner to Tier 2, stages the backend URL, and kicks the
///      cloud relay + device-JWT refresher.
///
/// Factored out so the two sign-in entry points cannot diverge. The
/// `runner-tier-changed` window event is dispatched by the FRONTEND on success
/// (both call sites do this) — identical to the prior single-callsite behavior.
async fn finalize_signed_in(
    login: qontinui_runner_lib::cognito::CognitoLoginResult,
    base: String,
) -> Result<CognitoSignInResponse, AppError> {
    use qontinui_runner_lib::pair::{
        ensure_device_initialized, pair_with_auth_token_with_ids, persist_pairing,
        read_device_id_from_disk, tenant_id_from_oauth_claim,
    };

    // 2. Persist the Cognito user tokens in the distinct oauth slots.
    //
    // `store_oauth_tokens_FRESH`, not the plain writer: this is the FIRST write
    // of an EXPLICIT, operator-initiated sign-in, so it is allowed to overwrite
    // a present-but-unreadable `.enc` (the old encrypted bytes are already
    // cryptographically dead on this machine — the AES key derives from
    // hostname+username, so a rename / disk move produces exactly that). The
    // plain (refuse-on-unreadable) writer dead-ended re-auth: every sign-in
    // write refused and the operator could never leave the LoginScreen. The
    // background refresher keeps using the refusing writer. Healing here also
    // makes the subsequent pairing writes (step 5) see a readable store.
    let auth_manager = AuthManager::new();
    auth_manager
        .store_oauth_tokens_fresh(
            &login.access_token,
            &login.id_token,
            &login.refresh_token,
            login.expires_at,
        )
        .map_err(|e| {
            error!("finalize_signed_in: step 2 (store_oauth_tokens_fresh) failed: {e}");
            AppError::Raw(format!("persist Cognito tokens: {e}"))
        })?;

    // 3. Device identity from disk. Mint it first if a fresh install never
    //    ran `device init` (startup already does this, but sign-in self-heals
    //    the edge case rather than dead-ending the user with a CLI hint).
    ensure_device_initialized();
    let device_id = read_device_id_from_disk().map_err(|e| {
        error!("finalize_signed_in: step 3 (read_device_id_from_disk) failed: {e}");
        AppError::Raw(format!("could not read device identity: {e}"))
    })?;

    // 4. Bind device→user via the existing pair-cli flow. The web backend
    //    resolves tenant_id server-side from the authenticated user, so the
    //    body tenant_id is a placeholder (nil) — same as the headless path.
    //    The user bearer is the Cognito ACCESS token; the user id is the
    //    Cognito `sub`.
    let base_b = base.clone();
    let cognito_access = login.access_token.clone();
    let device_b = device_id.clone();
    let sub_b = login.sub.clone();
    let pair_resp = tokio::task::spawn_blocking(move || {
        pair_with_auth_token_with_ids(
            &base_b,
            &cognito_access,
            &device_b,
            &sub_b,
            uuid::Uuid::nil(),
        )
    })
    .await
    .map_err(|e| {
        error!("finalize_signed_in: device-pair task panicked: {e}");
        AppError::Raw(format!("device-pair task panicked: {e}"))
    })?
    .map_err(|e| {
        error!("finalize_signed_in: step 4 (pair_with_auth_token_with_ids) failed: {e}");
        AppError::Raw(e)
    })?;

    // Coord stamps the real tenant_id on the minted device JWT; decode it for
    // persistence + display (the Cognito token does not carry tenant_id).
    let tenant_id = tenant_id_from_oauth_claim(&pair_resp.token)
        .and_then(|s| uuid::Uuid::parse_str(s.trim()).ok())
        .unwrap_or(uuid::Uuid::nil());

    // 5. Persist the device JWT + paired-user file.
    persist_pairing(&pair_resp, tenant_id).map_err(|e| {
        error!("finalize_signed_in: step 5 (persist_pairing) failed AFTER a successful pair: {e}");
        AppError::Raw(format!("persist pairing: {e}"))
    })?;

    // 5b. The sign-in has now GENUINELY succeeded (Cognito tokens stored, device
    //     bound, device JWT persisted), so end any interactive logout. Deliberate
    //     ordering: doing this right after step 2 (`store_oauth_tokens`) meant a
    //     sign-in that then failed at step 3/4/5 left the marker cleared with an
    //     `oauth_refresh_token` on disk — so the next `check_auth_status`
    //     reported `authenticated:true` and the app entered the shell on a FAILED
    //     sign-in. Everything after this point (tier promotion, coord_url, relay
    //     kicks) is either non-fatal or purely local, so clearing here cannot
    //     resurrect that class of bug.
    //
    //     Equally deliberate: this is NOT done inside `store_oauth_tokens` /
    //     `store_tokens` / `persist_pairing`. The background device-JWT refresher
    //     writes those same slots on every cycle, so clearing there would
    //     silently un-logout the operator.
    if let Err(e) = auth_manager.clear_interactive_signed_out() {
        warn!("finalize_signed_in: could not clear the interactive sign-out marker: {e}");
    }

    let tenant_id_str = if tenant_id.is_nil() {
        None
    } else {
        Some(tenant_id.to_string())
    };

    // 6. Promote to Tier 2 + stage backend, then kick the relay/refresher.
    {
        if crate::instance::is_secondary() {
            warn!("finalize_signed_in: secondary runner — applying in-memory only, skipping save_settings");
        } else {
            let base_for_write = base.clone();
            let sub = login.sub.clone();
            settings::update_settings(move |s| {
                if s.web_integration.backend_url.trim() != base_for_write {
                    s.web_integration.backend_url = base_for_write;
                }
                s.web_integration.enabled = true;
                s.qontinui_user_id = Some(sub);
                s.tier = settings::RunnerTier::QontinuiAccount;
                s.tier_initialized = true;
            }).map_err(|e| {
                error!("finalize_signed_in: step 6 (update_settings tier promotion) failed AFTER a successful pair: {e}");
                AppError::Raw(format!("persist tier promotion: {e}"))
            })?;
        }
    }
    // 6b. Persist the hosted coordinator endpoint into the active profile's
    //     `coord_url` — create-if-absent ONLY (never clobbers an operator
    //     value), so the effective coord base is inspectable on disk and the
    //     WS consumers that read `coord_url` directly are healed too (plan
    //     2026-07-16-runner-prod-coord-base-default-and-502-self-diagnosis,
    //     D2). Non-fatal: sign-in must never fail on this, and the tier
    //     default (D1) covers the HTTP side regardless.
    if let Err(e) = qontinui_runner_lib::profiles::ensure_coord_url(
        qontinui_runner_lib::profiles::PROD_COORD_WS_URL,
    ) {
        warn!(
            "finalize_signed_in: could not persist coord_url into profiles.json (non-fatal): {e}"
        );
    }

    tokio::spawn(async {
        crate::mcp::backend_relay::commands::kick_cloud_relay().await;
        crate::mcp::device_jwt_refresher::commands::kick_device_jwt_refresher().await;
    });

    let resp_device_id = pair_resp.device_id.clone().unwrap_or(device_id);
    info!(
        "finalize_signed_in: signed in + device bound (sub={}, device={resp_device_id}) — Tier 2 active",
        login.sub
    );

    Ok(CognitoSignInResponse {
        user_id: login.sub,
        email: login.email,
        tenant_id: tenant_id_str,
        device_id: resp_device_id,
    })
}

/// Sign in to the runner with an email + password **directly** via Cognito
/// `InitiateAuth` USER_PASSWORD_AUTH — **no system browser**. This is the
/// fully-headless / UI-Bridge-driveable counterpart to [`cognito_sign_in`]
/// (Hosted-UI PKCE): the operator can complete the entire sign-in through the
/// runner's own UI without a browser hop.
///
/// Additive — the Hosted-UI "Sign in with Qontinui" button
/// ([`cognito_sign_in`]) is unchanged. Both flows converge on the SAME
/// post-auth chain ([`finalize_signed_in`]): store Cognito tokens → bind the
/// coord device JWT via `pair-cli` → persist → promote to Tier 2 → kick the
/// relay/refresher. On a Cognito error (bad credentials, unconfirmed user,
/// etc.) a clean `Err(message)` is returned.
///
/// The password is NEVER logged. Requires the runner Cognito app-client to
/// have `ALLOW_USER_PASSWORD_AUTH` enabled.
///
/// `backend_url` is the web-backend base (e.g. `https://api.qontinui.io`),
/// explicit + required — symmetric with [`cognito_sign_in`].
#[tauri::command]
pub async fn cognito_sign_in_password(
    email: String,
    password: String,
    backend_url: String,
) -> Result<CognitoSignInResponse, String> {
    cognito_sign_in_password_impl(email, password, backend_url)
        .await
        .map_err(String::from)
}

async fn cognito_sign_in_password_impl(
    email: String,
    password: String,
    backend_url: String,
) -> Result<CognitoSignInResponse, AppError> {
    let base = backend_url.trim().trim_end_matches('/').to_string();
    if base.is_empty() {
        return Err(AppError::Raw("backend_url is required".to_string()));
    }
    let email = email.trim().to_string();
    if email.is_empty() {
        return Err(AppError::Raw("email is required".to_string()));
    }
    if password.is_empty() {
        return Err(AppError::Raw("password is required".to_string()));
    }

    // NOTE: never log `password`.
    info!("cognito_sign_in_password: starting InitiateAuth USER_PASSWORD_AUTH");

    // Blocking reqwest call — run on a worker thread off the tokio runtime.
    // `password` is moved into the closure and dropped there; it is never logged.
    let login = tokio::task::spawn_blocking(move || {
        qontinui_runner_lib::cognito::password_login(&email, &password)
    })
    .await
    .map_err(|e| {
        error!("cognito_sign_in_password: login task panicked: {e}");
        AppError::Raw(format!("password login task panicked: {e}"))
    })?
    .map_err(|e| {
        // `e` is already a humanized Cognito message (no secrets).
        error!("cognito_sign_in_password: InitiateAuth failed: {e}");
        AppError::Raw(e)
    })?;

    info!(
        "cognito_sign_in_password: InitiateAuth succeeded (sub={}) — binding device",
        login.sub
    );

    // Shared post-auth chain — identical to the Hosted-UI path.
    finalize_signed_in(login, base).await
}

/// Clears the runner token but KEEPS the runner at Tier 2
/// (`QontinuiAccount`) in an *unauthenticated* state, and kicks the relay
/// so the WS connection closes cleanly.
///
/// Used by `AccountSettings.tsx`'s "Sign out" button. An explicit "Sign
/// out" is a deliberate "I want to sign in again / switch accounts"
/// action, so the runner must land on the `LoginScreen` rather than
/// silently continue as a local guest. The App gate
/// (`isTier2 && !authStatus.authenticated → LoginScreen`) renders the
/// login screen precisely for this Tier-2-unauthenticated state, so we
/// clear the token (→ `check_auth_status` reports `authenticated: false`)
/// while leaving `tier = QontinuiAccount`.
///
/// This intentionally does NOT drop to `RunnerTier::Local`: the
/// local-guest tier is a *separate, deliberate* entry point chosen at
/// first-run via the SetupWizard's tier selector, not a side effect of
/// signing out of an account.
///
/// The FE is expected to dispatch a `runner-tier-changed` window event
/// after this call returns Ok so `useRunnerTier` consumers (notably
/// `AuthProvider`) re-read the tier and re-run `check_auth_status`, which
/// now returns unauthenticated → the gate shows `LoginScreen`.
///
/// # Errors
///
/// Returns `Err` only if persisting the cleared settings fails. Keychain
/// clear failures and the relay-kick are best-effort and logged but not
/// surfaced to the caller.
#[tauri::command]
pub async fn qontinui_sign_out() -> Result<(), String> {
    info!("qontinui_sign_out: clearing token + staying Tier 2 (unauthenticated) so the LoginScreen shows");

    // Best-effort: a stale keychain entry is annoying but not fatal —
    // the gating checks all run off the `tier` field + cleared token below.
    //
    // This is the explicit "switch accounts → LoginScreen" path, so it must be
    // the FULL wipe: `AuthManager::has_local_signed_in_session` treats a
    // preserved Cognito session as still-authenticated, so an
    // interactive-only clear would keep
    // the App gate out of the LoginScreen. Clearing the Cognito session also
    // (correctly) stops the old account's autonomous sessions before a new
    // account signs in.
    let auth_manager = AuthManager::new();
    if let Err(e) = auth_manager.clear_all_credentials() {
        warn!(
            "qontinui_sign_out: clear_all_credentials failed (continuing): {}",
            e
        );
    }

    if crate::instance::is_secondary() {
        warn!(
            "qontinui_sign_out: secondary runner — applying in-memory only, skipping save_settings"
        );
    } else {
        settings::update_settings(|s| {
            s.web_integration.runner_token = String::new();
            s.qontinui_user_id = None;
            // Keep tier == QontinuiAccount so the App gate renders LoginScreen
            // for this Tier-2-unauthenticated state instead of falling through
            // to the synthesized local-guest app shell.
            s.tier = settings::RunnerTier::QontinuiAccount;
            s.tier_initialized = true;
        })
        .map_err(|e| {
            let msg = format!("failed to persist sign-out: {}", e);
            error!("qontinui_sign_out: {}", msg);
            msg
        })?;
    }

    // Kick the relay — now that tier != QontinuiAccount, the cloud relay
    // task enters its idle-await-kick state and drops the WS.
    tokio::spawn(async {
        crate::mcp::backend_relay::commands::kick_cloud_relay().await;
    });

    Ok(())
}

/// Returns true iff `AuthManager`'s access_token slot looks like a JWT.
/// The frontend uses this from `WebIntegrationAuthBanner` to decide
/// whether to surface a "re-pair this runner" CTA after upgrade
/// (Phase 4 of the unified-devices migration).
///
/// NO-DOWNGRADE (M3): a credential-store READ ERROR is not "unpaired". It used
/// to be flattened by `unwrap_or_default()`, so a locked/corrupt store rendered
/// a "pair this runner first" CTA at a runner that IS paired. A read error is
/// now `Err` (unknown), leaving `false` to mean "definitively no JWT".
#[tauri::command]
pub fn device_jwt_present() -> Result<bool, String> {
    use crate::secure_storage::StoredTokenRead;
    match AuthManager::new().probe_access_token() {
        StoredTokenRead::Present(token) => Ok(crate::auth::looks_like_jwt(&token)),
        StoredTokenRead::Absent => Ok(false),
        StoredTokenRead::Unreadable(e) => {
            error!("device_jwt_present: credential store unreadable — pairing state is UNKNOWN, not unpaired: {e}");
            Err(format!(
                "Could not read the credential store, so this runner's pairing state is \
                 unknown (it has NOT been unpaired): {e}"
            ))
        }
    }
}

/// Returns the runner's coord **device-JWT** (the token stored in
/// `AuthManager`'s `access_token` slot), or `None` when the device is unpaired
/// — i.e. the slot is empty or does not hold a JWT-shaped value.
///
/// The CI-runner settings panel attaches this as `Authorization: Bearer <jwt>`
/// on its loopback calls to the supervisor (`:9875` enable/disable), which now
/// require + forward the credential so coord can enforce `FleetPrincipal` on
/// the registration-token mint. A `None` return is the FE's cue to surface a
/// "pair this runner first" CTA rather than calling the supervisor anonymously.
///
/// Unlike [`get_access_token_for_websocket`], this neither requires tier-2 nor
/// errors when unpaired: it is a credential *probe*, so a missing token is a
/// normal `Ok(None)`, not an error.
///
/// NO-DOWNGRADE (M3): a store READ ERROR is distinct from "unpaired". It used
/// to be flattened by `unwrap_or_default()` into `Ok(None)`, which the FE
/// renders as "pair this runner first" — wrong remediation, and it disabled
/// the CI-runner controls on a paired runner. A read error is now `Err`, so
/// `Ok(None)` keeps its single meaning: definitively no device JWT.
#[tauri::command]
pub fn get_coord_device_token() -> Result<Option<String>, String> {
    use crate::secure_storage::StoredTokenRead;
    match AuthManager::new().probe_access_token() {
        StoredTokenRead::Present(token) if crate::auth::looks_like_jwt(&token) => Ok(Some(token)),
        StoredTokenRead::Present(_) | StoredTokenRead::Absent => Ok(None),
        StoredTokenRead::Unreadable(e) => {
            error!("get_coord_device_token: credential store unreadable — pairing state is UNKNOWN, not unpaired: {e}");
            Err(format!(
                "Could not read the credential store, so this runner's device token is \
                 unknown (it has NOT been unpaired): {e}"
            ))
        }
    }
}

/// Tauri-command wrapper around the device-JWT refresher's `kick` API.
///
/// Used by `WebIntegrationAuthBanner`'s "retry manually" CTA when the
/// post-upgrade migration banner has been visible >5min and the operator
/// asks the refresher to try again immediately. Idempotent — if no
/// refresher is registered yet, this is a no-op.
#[tauri::command]
pub async fn kick_device_jwt_refresher_cmd() -> Result<(), String> {
    crate::mcp::device_jwt_refresher::commands::kick_device_jwt_refresher().await;
    Ok(())
}

/// Build the Tauri plugin that registers this module's command handlers.
///
/// See `commands/mod.rs` for the migration guide explaining the plugin pattern.
pub fn plugin<R: Runtime>() -> TauriPlugin<R> {
    PluginBuilder::new("qontinui_auth")
        .invoke_handler(tauri::generate_handler![
            logout,
            check_auth_status,
            get_device_info,
            get_user_projects,
            get_access_token_for_websocket,
            is_api_ready,
            get_api_port,
            get_runner_tier,
            set_runner_tier,
            cognito_sign_in,
            cognito_sign_in_password,
            qontinui_sign_out,
            sign_out_full,
            reset_credential_store,
            device_jwt_present,
            get_coord_device_token,
            kick_device_jwt_refresher_cmd,
        ])
        .build()
}

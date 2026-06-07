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
/// runner is not in Tier 2. All cloud-reaching auth commands gate on this.
fn require_tier_2() -> Result<(), AppError> {
    let s = settings::load_settings();
    require_tier_2_for(s.tier)
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

/// Logs out the current user.
///
/// This command:
/// 1. Retrieves the device ID
/// 2. Calls the backend to deactivate the device (optional)
/// 3. Clears all tokens from the keychain
///
/// # Errors
///
/// Returns an error string if keychain operations fail.
/// Network errors during device deactivation are logged but don't fail the operation.
#[tauri::command]
pub async fn logout() -> Result<(), String> {
    logout_impl().await.map_err(String::from)
}

async fn logout_impl() -> Result<(), AppError> {
    require_tier_2()?;
    info!("Logout requested");

    let auth_manager = AuthManager::new();

    // Phase 3: there's no longer a separate `/api/v1/runner-devices/{id}`
    // backend record to deactivate. The runner's presence is governed by
    // the unified WebSocket relay; closing the WS (which happens on token
    // revocation) is the equivalent of "logging the device out".
    //
    // Clear tokens from the keychain.
    auth_manager.clear_tokens()?;

    info!("Logout successful");
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

/// `true` iff the runner holds a valid *local* signed-in session: a paired
/// coord device-token JWT (the credential the WS relay presents) and/or a
/// stored Cognito session. This is the authoritative "is the runner signed
/// in?" signal — it is what a successful [`cognito_sign_in`] produces
/// (`store_oauth_tokens` + `persist_pairing` → device JWT in the access_token
/// slot) and what survives a process restart on disk.
///
/// Why this is the source of truth (and NOT a `/api/v1/auth/users/me`
/// round-trip): the web backend's `users/me` endpoint can return 401/403 for a
/// federated Cognito identity even though the runner is fully signed in and
/// device-paired (the pair-cli bind returned 201 with the SAME access token).
/// Gating "authenticated" on that call made a completed sign-in render the
/// LoginScreen forever. The runner is signed in when it has local credentials,
/// regardless of whether `users/me` happens to accept the Cognito access token.
fn has_local_signed_in_session(auth_manager: &AuthManager) -> bool {
    // A paired device has a (non-expired) coord device-token JWT in the
    // access_token slot — written by `persist_pairing` after a successful
    // pair-cli bind. `device_jwt_needs_refresh() == Ok(false)` means a real,
    // unexpired JWT is present.
    let has_valid_device_jwt = matches!(auth_manager.device_jwt_needs_refresh(), Ok(false));

    // A stored Cognito session (refresh token present) also means the user
    // signed in — even if the device JWT is momentarily stale and awaiting the
    // refresher's next pair cycle.
    let has_cognito_session = auth_manager
        .get_oauth_refresh_token()
        .map(|t| !t.trim().is_empty())
        .unwrap_or(false);

    has_valid_device_jwt || has_cognito_session
}

async fn check_auth_status_impl() -> Result<AuthStatus, AppError> {
    info!("Checking authentication status");

    // Tier 0/1 — never reach the backend, never touch the keychain.
    // Defense in depth: Phase 1 frontend doesn't call this in Tier 0/1, but
    // any caller that does must get an unambiguous "not authenticated".
    if settings::load_settings().tier != settings::RunnerTier::QontinuiAccount {
        return Ok(AuthStatus {
            authenticated: false,
            user: None,
            device_id: None,
        });
    }

    let auth_manager = AuthManager::new();
    let device_id = auth_manager.get_device_id().ok();

    // Authoritative signal: does the runner hold a valid local signed-in
    // session? This is true immediately after a successful `cognito_sign_in`
    // (device paired + Cognito tokens stored) AND on a fresh boot that restored
    // those credentials from disk. If there's no local session, the runner has
    // genuinely never signed in (or was logged out) — report unauthenticated so
    // the frontend shows LoginScreen.
    if !has_local_signed_in_session(&auth_manager) {
        info!("No local signed-in session — user not authenticated");
        return Ok(AuthStatus {
            authenticated: false,
            user: None,
            device_id: None,
        });
    }

    // We ARE signed in. Best-effort: enrich the status with the user's
    // profile via `/api/v1/auth/users/me`. A failure here (network error, or
    // even a 401/403 — the federated-identity `users/me` gap) does NOT
    // downgrade `authenticated`: a valid local device pairing is proof the
    // runner is signed in. We only use the response to populate `user`.
    let user = match web_backend_user_bearer(&auth_manager).await {
        Ok(access_token) => fetch_user_info(&access_token).await,
        Err(_) => None,
    };

    info!(
        "Runner authenticated via local session (user enrichment: {})",
        if user.is_some() { "ok" } else { "unavailable" }
    );

    Ok(AuthStatus {
        authenticated: true,
        user,
        device_id,
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
#[tauri::command]
pub fn get_runner_tier() -> Result<String, String> {
    let s = settings::load_settings();
    // String form so the React side doesn't need a TS enum mirror.
    Ok(match s.tier {
        settings::RunnerTier::Local => "local",
        settings::RunnerTier::LocalProvider => "local_provider",
        settings::RunnerTier::QontinuiAccount => "qontinui_account",
    }
    .to_string())
}

/// Sets the current runner tier and persists. Used by the SetupWizard's
/// TierStep and the AccountSettings sign-in completion handler.
#[tauri::command]
pub fn set_runner_tier(tier: String) -> Result<(), String> {
    let parsed = match tier.as_str() {
        "local" => settings::RunnerTier::Local,
        "local_provider" => settings::RunnerTier::LocalProvider,
        "qontinui_account" => settings::RunnerTier::QontinuiAccount,
        other => return Err(format!("invalid tier: {}", other)),
    };
    let mut s = settings::load_settings();
    s.tier = parsed;
    s.tier_initialized = true;
    if crate::instance::is_secondary() {
        warn!(
            "set_runner_tier: secondary runner — applying in-memory only, skipping save_settings"
        );
    } else {
        settings::save_settings(&s).map_err(|e| e.to_string())?;
    }
    // Kick both the relay (so it picks up the new tier without a runner
    // restart) and the device-JWT refresher (Phase 2 of unified-devices
    // — promotion into Tier 2 should trigger a JWT refresh check, and
    // demotion out of it should let the refresher idle). The refresher's
    // `next_action` predicate already handles the tier branching; we
    // just need to wake it.
    //
    // Use `tauri::async_runtime::spawn`, NOT `tokio::spawn`: this command is
    // a *synchronous* `#[tauri::command]`, which Tauri invokes on a worker
    // thread that does NOT carry an entered Tokio runtime context. A bare
    // `tokio::spawn` there panics with "there is no reactor running" (a
    // non-unwinding panic that aborts the whole process). The Tauri async
    // runtime handle is always available and routes to the same Tokio
    // runtime the relay/refresher live on.
    tauri::async_runtime::spawn(async {
        crate::mcp::backend_relay::commands::kick_cloud_relay().await;
        crate::mcp::device_jwt_refresher::commands::kick_device_jwt_refresher().await;
    });
    Ok(())
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
        pair_with_auth_token_with_ids, persist_pairing, read_device_id_from_disk,
        tenant_id_from_oauth_claim,
    };

    // 2. Persist the Cognito user tokens in the distinct oauth slots.
    let auth_manager = AuthManager::new();
    auth_manager
        .store_oauth_tokens(
            &login.access_token,
            &login.id_token,
            &login.refresh_token,
            login.expires_at,
        )
        .map_err(|e| {
            error!("finalize_signed_in: step 2 (store_oauth_tokens) failed: {e}");
            AppError::Raw(format!("persist Cognito tokens: {e}"))
        })?;

    // 3. Device identity from disk.
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

    let tenant_id_str = if tenant_id.is_nil() {
        None
    } else {
        Some(tenant_id.to_string())
    };

    // 6. Promote to Tier 2 + stage backend, then kick the relay/refresher.
    {
        let mut s = settings::load_settings();
        if s.web_integration.backend_url.trim() != base {
            s.web_integration.backend_url = base.clone();
        }
        s.web_integration.enabled = true;
        s.qontinui_user_id = Some(login.sub.clone());
        s.tier = settings::RunnerTier::QontinuiAccount;
        s.tier_initialized = true;
        if crate::instance::is_secondary() {
            warn!("finalize_signed_in: secondary runner — applying in-memory only, skipping save_settings");
        } else {
            settings::save_settings(&s).map_err(|e| {
                error!("finalize_signed_in: step 6 (save_settings tier promotion) failed AFTER a successful pair: {e}");
                AppError::Raw(format!("persist tier promotion: {e}"))
            })?;
        }
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
    let auth_manager = AuthManager::new();
    if let Err(e) = auth_manager.clear_tokens() {
        warn!("qontinui_sign_out: clear_tokens failed (continuing): {}", e);
    }

    let mut s = settings::load_settings();
    s.web_integration.runner_token = String::new();
    s.qontinui_user_id = None;
    // Keep tier == QontinuiAccount so the App gate renders LoginScreen for
    // this Tier-2-unauthenticated state instead of falling through to the
    // synthesized local-guest app shell.
    s.tier = settings::RunnerTier::QontinuiAccount;
    s.tier_initialized = true;
    if crate::instance::is_secondary() {
        warn!(
            "qontinui_sign_out: secondary runner — applying in-memory only, skipping save_settings"
        );
    } else {
        settings::save_settings(&s).map_err(|e| {
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
#[tauri::command]
pub fn device_jwt_present() -> Result<bool, String> {
    let token = AuthManager::new().get_access_token().unwrap_or_default();
    Ok(crate::auth::looks_like_jwt(&token))
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
#[tauri::command]
pub fn get_coord_device_token() -> Result<Option<String>, String> {
    let token = AuthManager::new().get_access_token().unwrap_or_default();
    if crate::auth::looks_like_jwt(&token) {
        Ok(Some(token))
    } else {
        Ok(None)
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
            device_jwt_present,
            get_coord_device_token,
            kick_device_jwt_refresher_cmd,
        ])
        .build()
}

//! Authentication command handlers for Tauri.
//!
//! This module provides Tauri commands for user authentication, including:
//! - Login with email/password
//! - Logout
//! - Authentication status checking
//! - Device information retrieval

use crate::auth::AuthManager;
use crate::commands::compartments::HealthCompartment;
use crate::error::AppError;
use serde::{Deserialize, Serialize};
use std::sync::atomic::Ordering;
use tauri::plugin::{Builder as PluginBuilder, TauriPlugin};
use tauri::Runtime;
use tracing::{error, info, warn};

use crate::api_config::get_api_base_url;
use crate::settings;

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

/// Response from the login endpoint
#[derive(Debug, Serialize, Deserialize)]
pub struct LoginResponse {
    pub access_token: String,
    pub refresh_token: String,
    pub user: UserInfo,
    pub device_info: DeviceInfo,
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

/// Login API response (from qontinui-web)
/// Matches the TokenResponse model from the backend
#[derive(Debug, Serialize, Deserialize)]
struct ApiLoginResponse {
    access_token: String,
    refresh_token: String,
    token_type: String,
    expires_in: i32,
    refresh_expires_in: i32,
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

/// Authenticates a user with email and password.
///
/// This command:
/// 1. Calls the qontinui-web login endpoint
/// 2. Stores the received tokens in the OS keychain
/// 3. Retrieves or generates a device ID
/// 4. Registers the device with the backend
/// 5. Returns the login response with user information
///
/// # Arguments
///
/// * `email` - User's email address
/// * `password` - User's password
///
/// # Errors
///
/// Returns an error string if:
/// - Login credentials are invalid
/// - Network request fails
/// - Token storage fails
/// - Device registration fails
#[tauri::command]
pub async fn login(email: String, password: String) -> Result<LoginResponse, String> {
    login_impl(email, password).await.map_err(String::from)
}

async fn login_impl(email: String, password: String) -> Result<LoginResponse, AppError> {
    require_tier_2()?;
    login_impl_inner(email, password).await
}

/// Shared login body — performs the OAuth2 form login + `/users/me` fetch +
/// token storage against [`get_api_base_url`].
///
/// Tier-gating is applied by the public `login` command (via `login_impl`).
/// The bootstrap credentials-pairing path ([`pair_with_credentials`]) does
/// NOT route through here — it logs in against an explicit `backend_url`
/// (so a debug build can target prod) and is tier-ungated by design, closing
/// the chicken-and-egg where `login` requires Tier 2 but reaching Tier 2
/// requires logging in first.
async fn login_impl_inner(email: String, password: String) -> Result<LoginResponse, AppError> {
    info!("Login attempt for email: {}", email);

    let auth_manager = AuthManager::new();

    // 1. Call login endpoint
    // Note: The backend expects OAuth2 form data (username/password fields), not JSON
    let client = reqwest::Client::new();
    let form_data = [
        ("username", email.as_str()),
        ("password", password.as_str()),
    ];

    let response = client
        .post(format!("{}/api/v1/auth/jwt/login", get_api_base_url()))
        .form(&form_data)
        .send()
        .await?;

    if !response.status().is_success() {
        let status = response.status();
        let error_text = response
            .text()
            .await
            .unwrap_or_else(|_| "Unknown error".to_string());
        error!("Login failed with status {}: {}", status, error_text);

        // Provide user-friendly error messages
        let user_message = match status.as_u16() {
            401 => "Invalid email or password".to_string(),
            404 => "Login service not available. Please check your connection.".to_string(),
            500..=599 => "Server error. Please try again later.".to_string(),
            _ => {
                // Try to extract message from JSON error response
                if let Ok(json_error) = serde_json::from_str::<serde_json::Value>(&error_text) {
                    if let Some(message) = json_error.get("message").and_then(|m| m.as_str()) {
                        message.to_string()
                    } else {
                        "Login failed. Please try again.".to_string()
                    }
                } else {
                    "Login failed. Please try again.".to_string()
                }
            }
        };

        return Err(AppError::Raw(user_message));
    }

    let api_response: ApiLoginResponse = response.json().await?;

    info!("Login successful, fetching user info...");

    // Fetch user info using the access token
    let user_response = client
        .get(format!("{}/api/v1/auth/users/me", get_api_base_url()))
        .bearer_auth(&api_response.access_token)
        .send()
        .await?;

    if !user_response.status().is_success() {
        let status = user_response.status();
        let error_text = user_response
            .text()
            .await
            .unwrap_or_else(|_| "Unknown error".to_string());
        error!(
            "Failed to fetch user info with status {}: {}",
            status, error_text
        );
        return Err(AppError::Raw(format!(
            "Failed to fetch user information: {}",
            status
        )));
    }

    let user_info: ApiUserInfo = user_response.json().await?;

    info!("Login successful for user: {}", user_info.id);

    // 2. Store tokens in keychain
    auth_manager.store_tokens(&api_response.access_token, &api_response.refresh_token)?;

    // 3. Get or generate device ID for local identification only.
    //
    // Phase 3: the legacy `/api/v1/runner-devices/register` HTTP flow is
    // deleted server-side. The runner is now identified to qontinui-web
    // via the unified WebSocket relay (`mcp::backend_relay`), keyed by
    // `(user_id, runner_name)` upserted into the `runners` table on
    // handshake. The `device_id` here is retained only as a stable local
    // identifier (used by some UI elements).
    let device_id = auth_manager.get_device_id()?;
    let device_name = get_device_name();
    let platform = get_platform();

    // 4. Return success response
    Ok(LoginResponse {
        access_token: api_response.access_token,
        refresh_token: api_response.refresh_token,
        user: UserInfo {
            id: user_info.id,
            email: user_info.email,
            name: user_info.full_name,
        },
        device_info: DeviceInfo {
            device_id,
            device_name,
            platform,
        },
    })
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
/// 1. Checks if tokens exist in the keychain
/// 2. If tokens exist, validates them by calling the /api/v1/auth/users/me endpoint
/// 3. Returns authentication status with user information if authenticated
///
/// # Errors
///
/// Returns an error string if the validation request fails or tokens are invalid.
#[tauri::command]
pub async fn check_auth_status() -> Result<AuthStatus, String> {
    check_auth_status_impl().await.map_err(String::from)
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

    // Check if tokens exist
    if !auth_manager.has_tokens() {
        info!("No tokens found - user not authenticated");
        return Ok(AuthStatus {
            authenticated: false,
            user: None,
            device_id: None,
        });
    }

    // Get access token
    let access_token = auth_manager.get_access_token()?;

    // Validate token by calling /users/me endpoint — reqwest::Error From impl
    // wraps build failures into AppError::NetworkError, but we keep the
    // historical wire string via AppError::Raw to preserve frontend parsing.
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| AppError::Raw(format!("Failed to create HTTP client: {}", e)))?;
    let response = match client
        .get(format!("{}/api/v1/auth/users/me", get_api_base_url()))
        .bearer_auth(&access_token)
        .send()
        .await
    {
        Ok(resp) => resp,
        Err(e) => {
            // Backend unreachable — tokens exist, assume authenticated.
            // The runner should work offline; token validity will be checked
            // when the backend becomes available again.
            warn!(
                "Backend unreachable during auth check ({}), assuming authenticated with stored tokens",
                e
            );
            let device_id = auth_manager.get_device_id().ok();
            return Ok(AuthStatus {
                authenticated: true,
                user: None,
                device_id,
            });
        }
    };

    if !response.status().is_success() {
        let status = response.status();
        warn!("Token validation failed: {}", status);

        // Only clear tokens on definitive auth failures (401/403).
        // For transient errors (5xx, timeouts, etc.), return Err so the
        // frontend catch block fires without changing auth state.
        if status.as_u16() == 401 || status.as_u16() == 403 {
            let _ = auth_manager.clear_tokens();
            return Ok(AuthStatus {
                authenticated: false,
                user: None,
                device_id: None,
            });
        } else {
            return Err(AppError::Raw(format!(
                "Auth status check failed with transient error: {}",
                status
            )));
        }
    }

    let user_info: ApiUserInfo = response.json().await?;

    let device_id = auth_manager.get_device_id().ok();

    info!("User authenticated: {}", user_info.id);

    Ok(AuthStatus {
        authenticated: true,
        user: Some(UserInfo {
            id: user_info.id,
            email: user_info.email,
            name: user_info.full_name,
        }),
        device_id,
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

    // Check authentication
    if !auth_manager.has_tokens() {
        return Err(AppError::AuthError(
            "Not authenticated. Please log in first.".to_string(),
        ));
    }

    // Get access token
    let access_token = auth_manager.get_access_token()?;

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

/// Refresh token command
///
/// Refreshes the access token using the refresh token.
/// This should be called periodically to keep the user authenticated.
///
/// # Errors
///
/// Returns an error string if:
/// - No refresh token found
/// - Token refresh fails
#[tauri::command]
pub async fn refresh_token() -> Result<(), String> {
    refresh_token_impl().await.map_err(String::from)
}

async fn refresh_token_impl() -> Result<(), AppError> {
    require_tier_2()?;
    info!("Refreshing access token");

    let auth_manager = AuthManager::new();

    // Get refresh token
    let refresh_token = auth_manager.get_refresh_token()?;

    // Call refresh endpoint
    let client = reqwest::Client::new();

    #[derive(Serialize)]
    struct RefreshRequest {
        refresh_token: String,
    }

    #[derive(Deserialize)]
    struct RefreshResponse {
        access_token: String,
        refresh_token: String,
    }

    let refresh_request = RefreshRequest { refresh_token };

    let response = client
        .post(format!("{}/api/v1/auth/jwt/refresh", get_api_base_url()))
        .json(&refresh_request)
        .send()
        .await?;

    if !response.status().is_success() {
        let status = response.status();
        let error_text = response
            .text()
            .await
            .unwrap_or_else(|_| "Unknown error".to_string());
        error!(
            "Token refresh failed with status {}: {}",
            status, error_text
        );

        // Only clear tokens on definitive auth failures (401/403 = token truly invalid).
        // For transient errors (5xx, network issues), keep the tokens so the next
        // refresh cycle can retry without forcing a full re-login.
        if status.as_u16() == 401 || status.as_u16() == 403 {
            let _ = auth_manager.clear_tokens();
        }

        return Err(AppError::Raw(format!(
            "Token refresh failed: {}",
            error_text
        )));
    }

    let refresh_response: RefreshResponse = response.json().await?;

    // Store new tokens
    auth_manager.store_tokens(
        &refresh_response.access_token,
        &refresh_response.refresh_token,
    )?;

    info!("Token refreshed successfully");
    Ok(())
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

/// Credentials for temp-runner auto-login, sourced from process env.
#[derive(Debug, Serialize, Deserialize)]
pub struct TestAutoLoginCreds {
    pub email: String,
    pub password: String,
}

/// Returns test auto-login credentials if this process was launched with
/// `QONTINUI_TEST_AUTO_LOGIN_EMAIL` + `QONTINUI_TEST_AUTO_LOGIN_PASSWORD` set.
///
/// The supervisor forwards these env vars to spawned non-primary test runners
/// (when the supervisor itself is in dev_mode and has `QONTINUI_TEST_LOGIN_EMAIL`
/// / `QONTINUI_TEST_LOGIN_PASSWORD` in its environment). The React AuthProvider
/// invokes this command to auto-authenticate temp test runners so UI Bridge
/// inspection can reach authenticated pages (Process Manager, settings, etc.).
///
/// Returns `None` for normal primary runners where the env vars are absent.
/// This is safe to expose: it only reveals credentials the process already has
/// in its own environment — no privilege escalation.
///
/// # Logging contract (phase-8 manual-test remediation)
///
/// When this returns `None`, the call site logs at `INFO` with target
/// `test_auto_login_skipped` and a `reason=<AUTO_LOGIN_SKIP_*>` field so
/// an operator staring at a LoginScreen can `grep test_auto_login_skipped`
/// `runner-tauri.log` to determine why auto-login didn't fire. The log is
/// emitted on every invocation rather than deduped via a once-flag — the
/// AuthProvider effect invokes this exactly once per mount, HMR reloads
/// are dev-only noise, and the cost of an extra info line is negligible
/// compared to the cost of silent failure.
#[tauri::command]
pub fn get_test_auto_login(
    launch_env: tauri::State<'_, crate::launch_env::SharedLaunchEnv>,
) -> Option<TestAutoLoginCreds> {
    match launch_env.auto_login.as_ref() {
        Some(c) => Some(TestAutoLoginCreds {
            email: c.email.clone(),
            password: c.password.clone(),
        }),
        None => {
            // `auto_login_skip_reason` is `Some` whenever `auto_login` is
            // `None` — populated from `classify_test_auto_login` in
            // `RunnerLaunchEnv::read`. The `unwrap_or` guards against a
            // future code path that sets `auto_login = None` without
            // populating the reason; preserves the log contract.
            let reason = launch_env.auto_login_skip_reason.unwrap_or("unknown");
            info!(reason = %reason, "test_auto_login_skipped");
            None
        }
    }
}

/// Headless/back-end auto-login for supervisor-spawned test runners.
///
/// The webview-driven auto-login (`get_test_auto_login` → React `AuthProvider`)
/// only fires once the frontend mounts. A supervisor-spawned temp runner that
/// stays headless (no webview, or a slow/cold WebView2 init) therefore never
/// authenticates, so the device-JWT is never minted and backend heartbeats /
/// the WS relay stay disabled (`runner_token_empty=true`, 401 on workflow
/// sync — the staging device-registration gap). This routine performs the same
/// login from the Rust side at startup, independent of the webview, whenever
/// `QONTINUI_TEST_AUTO_LOGIN_EMAIL` / `_PASSWORD` are present.
///
/// Fire-and-forget; safe no-op (with a grep-able `headless_auto_login_skipped`
/// reason) when creds are absent, the runner isn't tier-2, or tokens already
/// exist (e.g. the webview path or a prior run already authenticated).
pub fn spawn_headless_auto_login(launch_env: crate::launch_env::SharedLaunchEnv) {
    let creds = match launch_env.auto_login.as_ref() {
        Some(c) => (c.email.clone(), c.password.clone()),
        None => {
            let reason = launch_env.auto_login_skip_reason.unwrap_or("unknown");
            info!(reason = %reason, "headless_auto_login_skipped");
            return;
        }
    };

    tauri::async_runtime::spawn(async move {
        // Tier gate: login requires tier 2 (QontinuiAccount). Skip quietly
        // otherwise rather than emitting a misleading login-failed warning.
        if settings::load_settings().tier != settings::RunnerTier::QontinuiAccount {
            info!(reason = "not_tier_2", "headless_auto_login_skipped");
            return;
        }

        use qontinui_runner_lib::pair::{
            pair_with_auth_token_with_ids, persist_pairing, read_device_id_from_disk,
            tenant_id_from_oauth_claim,
        };

        // When the backend URL is explicitly overridden (e.g. a staging temp
        // runner via extra_env), stored tokens from the shared settings.json
        // likely target a different backend (the primary's localhost default).
        // Force a fresh login+pair against the intended target.
        let backend_override = std::env::var("QONTINUI_WEB_BACKEND_URL").is_ok();

        let auth_manager = AuthManager::new();
        let already_paired = dirs::data_local_dir()
            .map(|d| d.join("com.qontinui.runner").join("paired_user.json").exists())
            .unwrap_or(false);

        // Phase 1: Login (skip if tokens already exist from a prior run,
        // unless the backend was explicitly overridden).
        let access_token;
        let user_id;
        if !backend_override && auth_manager.has_tokens() {
            if already_paired {
                info!("headless_auto_login: tokens + paired_user present — fully skipping");
                return;
            }
            info!("headless_auto_login: tokens present but not paired — skipping login, will pair");
            access_token = match auth_manager.get_access_token() {
                Ok(t) => t,
                Err(e) => {
                    warn!(error = %e, "headless_auto_login: could not retrieve stored token");
                    return;
                }
            };
            user_id = String::new();
        } else {
            let (email, password) = creds;
            if backend_override {
                info!(
                    email = %email,
                    "headless_auto_login: QONTINUI_WEB_BACKEND_URL set — forcing fresh login (stale tokens ignored)"
                );
            }
            info!(email = %email, "headless_auto_login: attempting backend login");
            let resp = match login_impl(email, password).await {
                Ok(r) => r,
                Err(e) => {
                    warn!(error = %String::from(e), "headless_auto_login: login failed");
                    return;
                }
            };
            info!(
                user = %resp.user.id,
                "headless_auto_login: login succeeded — attempting device pairing"
            );
            access_token = resp.access_token.clone();
            user_id = resp.user.id.clone();
        }

        // Phase 2: Auto-pair — mint a device-JWT so the backend relay
        // can register. The web backend's POST /api/v1/devices/pair-cli
        // resolves tenant_id SERVER-SIDE; the runner only sends
        // (device_id, hostname, name) + Bearer auth.

        let device_id = match read_device_id_from_disk() {
            Ok(d) => d,
            Err(e) => {
                warn!(
                    error = %e,
                    "headless_auto_login: could not read device identity — skipping pairing"
                );
                return;
            }
        };

        // POST pair-cli. tenant_id in the body is ignored by the web
        // backend (PairCliRequest only has device_id/hostname/name);
        // pass nil as a placeholder.
        let base = get_api_base_url();
        let base_c = base.clone();
        let token_c = access_token.clone();
        let device_c = device_id.clone();
        let user_c = user_id.clone();
        let pair_result = match tokio::task::spawn_blocking(move || {
            pair_with_auth_token_with_ids(
                &base_c,
                &token_c,
                &device_c,
                &user_c,
                uuid::Uuid::nil(),
            )
        })
        .await
        {
            Ok(inner) => inner,
            Err(join_err) => {
                warn!(
                    error = %join_err,
                    "headless_auto_login: pair task join failed — skipping pairing"
                );
                return;
            }
        };

        let pair_resp = match pair_result {
            Ok(r) => r,
            Err(e) => {
                warn!(
                    error = %e,
                    "headless_auto_login: device-pair POST failed — skipping pairing"
                );
                return;
            }
        };

        // Extract the real tenant_id from the coord-minted device-JWT
        // (the response token carries it, unlike the user OAuth token).
        let tenant_id = tenant_id_from_oauth_claim(&pair_resp.token)
            .and_then(|s| uuid::Uuid::parse_str(s.trim()).ok())
            .unwrap_or(uuid::Uuid::nil());

        if let Err(e) = persist_pairing(&pair_resp, tenant_id) {
            warn!(
                error = %e,
                "headless_auto_login: persist pairing failed — device-JWT not stored"
            );
            return;
        }

        let resp_device_id = pair_resp
            .device_id
            .as_deref()
            .unwrap_or(device_id.as_str());
        info!(
            user = %user_id,
            device = %resp_device_id,
            tenant = %tenant_id,
            "headless_auto_login: device paired + JWT persisted"
        );

        // Kick the relay + JWT refresher so they pick up the fresh
        // device-JWT without waiting for the next poll cycle.
        crate::mcp::backend_relay::commands::kick_cloud_relay().await;
        crate::mcp::device_jwt_refresher::commands::kick_device_jwt_refresher().await;

        info!("headless_auto_login: relay + refresher kicked — auto-pair complete");
    });
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
    settings::save_settings(&s).map_err(|e| e.to_string())?;
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

/// Opens the system browser to `/connect-runner` with the runner's loopback
/// callback URL.
///
/// This is the Settings → Account "Sign in to Qontinui" entry point used by
/// `AccountSettings.tsx`. It is a thin wrapper over the existing
/// `start_web_token_flow` machinery (same `TokenFlowStore` + same
/// `/auth/runner-token-callback` handler) — kept as a separate command so
/// the FE call site reads as a tier-promotion intent rather than a
/// generic "open token flow" action.
///
/// The actual tier promotion happens in `mcp::auth_callback` when the
/// user clicks Confirm in the browser; this command returns as soon as
/// the browser has been launched.
///
/// # Errors
///
/// Returns `Err` if no `backend_url` is configured in settings or if
/// `open::that` fails to launch a browser.
#[tauri::command]
pub async fn start_qontinui_sign_in<R: Runtime>(
    integration: tauri::State<'_, crate::commands::compartments::IntegrationCompartment>,
    health: tauri::State<'_, HealthCompartment>,
    app_handle: tauri::AppHandle<R>,
) -> Result<(), String> {
    // Delegate to the existing token-flow command — same pending-flow store,
    // same callback handler, same browser-launch logic. `backend_url = None`
    // re-uses whatever is already persisted (or errors if nothing is).
    crate::commands::web_integration::start_web_token_flow(integration, health, app_handle, None)
        .await
}

/// Wire response for [`pair_with_credentials`]. Mirrors the redeem/pair
/// confirmation shape so the FE can show a uniform "paired!" panel.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairWithCredentialsResponse {
    pub user_id: String,
    pub tenant_id: String,
    pub device_id: String,
}

/// Headless, in-app email/password pairing — the non-browser, UI-Bridge-
/// drivable cloud-pair path.
///
/// The only other cloud-pair entry points are the external-browser SSO
/// (`start_qontinui_sign_in`) and a dashboard-minted pair code
/// (`redeem_pair_code`). Neither is fully self-service from inside the app:
/// SSO needs a human to click Confirm in a browser; the pair code needs a
/// human to mint it on the dashboard first. This command closes that gap —
/// given an email, password, and backend URL it logs in, mints a device
/// JWT, persists it, and promotes the runner to Tier 2, all without leaving
/// the app.
///
/// Flow:
///   1. OAuth2 form login against `{backend_url}/api/v1/auth/jwt/login`
///      (tier-ungated bootstrap — see [`login_bootstrap_impl`]).
///   2. `GET /api/v1/auth/users/me` for the `user_id`.
///   3. Resolve `tenant_id` from the login JWT's `tenant_id` claim.
///   4. Device-pair via the existing `pair_with_auth_token_with_ids`
///      (`POST {backend_url}/api/v1/devices/pair-cli`), which returns a
///      coord-minted device JWT.
///   5. Persist the device JWT + paired-user file, then promote the runner
///      to Tier QontinuiAccount and kick the relay.
///
/// `backend_url` is required and explicit (NOT `get_api_base_url()`) so the
/// caller can target prod from a debug build, where the default resolves to
/// `http://127.0.0.1:8000`.
#[tauri::command]
pub async fn pair_with_credentials(
    email: String,
    password: String,
    backend_url: String,
) -> Result<PairWithCredentialsResponse, String> {
    pair_with_credentials_impl(email, password, backend_url)
        .await
        .map_err(String::from)
}

async fn pair_with_credentials_impl(
    email: String,
    password: String,
    backend_url: String,
) -> Result<PairWithCredentialsResponse, AppError> {
    use qontinui_runner_lib::pair::{
        pair_with_auth_token_with_ids, persist_pairing, read_device_id_from_disk,
        tenant_id_from_oauth_claim,
    };

    let base = backend_url.trim().trim_end_matches('/').to_string();
    if base.is_empty() {
        return Err(AppError::Raw("backend_url is required".to_string()));
    }
    info!(
        "pair_with_credentials: logging in {} against {}",
        email, base
    );

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| AppError::Raw(format!("Failed to create HTTP client: {e}")))?;

    // 1. OAuth2 form login.
    let form_data = [
        ("username", email.as_str()),
        ("password", password.as_str()),
    ];
    let login_resp = client
        .post(format!("{base}/api/v1/auth/jwt/login"))
        .form(&form_data)
        .send()
        .await?;
    if !login_resp.status().is_success() {
        let status = login_resp.status();
        let body = login_resp.text().await.unwrap_or_default();
        let msg = match status.as_u16() {
            401 => "Invalid email or password".to_string(),
            _ => format!(
                "login failed ({status}): {}",
                body.chars().take(200).collect::<String>()
            ),
        };
        return Err(AppError::Raw(msg));
    }
    let api_login: ApiLoginResponse = login_resp.json().await?;
    let access_token = api_login.access_token.clone();

    // 2. Fetch user info for the user_id.
    let me_resp = client
        .get(format!("{base}/api/v1/auth/users/me"))
        .bearer_auth(&access_token)
        .send()
        .await?;
    if !me_resp.status().is_success() {
        let status = me_resp.status();
        return Err(AppError::Raw(format!("fetch user info failed: {status}")));
    }
    let user_info: ApiUserInfo = me_resp.json().await?;
    let user_id = user_info.id.clone();

    // Store the user tokens so subsequent tier-2 commands (refresh, ws
    // token) can use them.
    let auth_manager = AuthManager::new();
    auth_manager.store_tokens(&access_token, &api_login.refresh_token)?;

    // 3. Resolve tenant_id from the login JWT's claim. The pair-cli proxy
    //    requires it in the body. If the backend's user JWT doesn't carry a
    //    `tenant_id` claim, there's no runner-side way to recover it without
    //    a prior browser pair — surface a clear, actionable error.
    let tenant_id_str = tenant_id_from_oauth_claim(&access_token).ok_or_else(|| {
        AppError::Raw(
            "login succeeded but the user token carries no tenant_id claim — \
             cannot device-pair via credentials. Use the browser sign-in or a \
             dashboard pair code for the first pair."
                .to_string(),
        )
    })?;
    let tenant_id = uuid::Uuid::parse_str(tenant_id_str.trim())
        .map_err(|e| AppError::Raw(format!("malformed tenant_id claim: {e}")))?;

    // 4. Device identity from disk.
    let device_id = read_device_id_from_disk()
        .map_err(|e| AppError::Raw(format!("could not read device identity: {e}")))?;

    // 5. Device-pair (blocking HTTP on a worker thread).
    let base_b = base.clone();
    let token_b = access_token.clone();
    let device_b = device_id.clone();
    let user_b = user_id.clone();
    let pair_resp = tokio::task::spawn_blocking(move || {
        pair_with_auth_token_with_ids(&base_b, &token_b, &device_b, &user_b, tenant_id)
    })
    .await
    .map_err(|e| AppError::Raw(format!("device-pair task panicked: {e}")))?
    .map_err(AppError::Raw)?;

    // 6. Persist the device JWT + paired-user file.
    persist_pairing(&pair_resp, tenant_id)
        .map_err(|e| AppError::Raw(format!("persist pairing: {e}")))?;

    // 7. Promote to Tier 2 + kick the relay (idempotent).
    {
        let mut s = settings::load_settings();
        // Stage the backend so the relay + later Save see the right host.
        if s.web_integration.backend_url.trim() != base {
            s.web_integration.backend_url = base.clone();
        }
        s.web_integration.enabled = true;
        s.qontinui_user_id = Some(user_id.clone());
        s.tier = settings::RunnerTier::QontinuiAccount;
        s.tier_initialized = true;
        settings::save_settings(&s)
            .map_err(|e| AppError::Raw(format!("persist tier promotion: {e}")))?;
    }
    tokio::spawn(async {
        crate::mcp::backend_relay::commands::kick_cloud_relay().await;
        crate::mcp::device_jwt_refresher::commands::kick_device_jwt_refresher().await;
    });

    let resp_device_id = pair_resp.device_id.clone().unwrap_or(device_id);
    info!(
        "pair_with_credentials: paired (user_id={user_id}, tenant_id={tenant_id_str}, device_id={resp_device_id}) — promoted to Tier 2"
    );

    Ok(PairWithCredentialsResponse {
        user_id,
        tenant_id: tenant_id_str,
        device_id: resp_device_id,
    })
}

/// Clears the runner token, drops the runner back to Tier 0/1, and kicks
/// the relay so the WS connection closes cleanly.
///
/// Used by `AccountSettings.tsx`'s "Sign out" button. The FE is expected
/// to dispatch a `runner-tier-changed` window event after this call
/// returns Ok so `useRunnerTier` consumers (notably `AuthProvider`)
/// re-read and switch back to the synthesized local-guest auth.
///
/// # Errors
///
/// Returns `Err` only if persisting the cleared settings fails. Keychain
/// clear failures and the relay-kick are best-effort and logged but not
/// surfaced to the caller.
#[tauri::command]
pub async fn qontinui_sign_out() -> Result<(), String> {
    info!("qontinui_sign_out: clearing token + dropping to Tier Local");

    // Best-effort: a stale keychain entry is annoying but not fatal —
    // the gating checks all run off the `tier` field below.
    let auth_manager = AuthManager::new();
    if let Err(e) = auth_manager.clear_tokens() {
        warn!("qontinui_sign_out: clear_tokens failed (continuing): {}", e);
    }

    let mut s = settings::load_settings();
    s.web_integration.runner_token = String::new();
    s.qontinui_user_id = None;
    s.tier = settings::RunnerTier::Local;
    s.tier_initialized = true;
    settings::save_settings(&s).map_err(|e| {
        let msg = format!("failed to persist sign-out: {}", e);
        error!("qontinui_sign_out: {}", msg);
        msg
    })?;

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
            login,
            logout,
            check_auth_status,
            get_device_info,
            get_user_projects,
            refresh_token,
            get_access_token_for_websocket,
            is_api_ready,
            get_api_port,
            get_test_auto_login,
            get_runner_tier,
            set_runner_tier,
            start_qontinui_sign_in,
            pair_with_credentials,
            qontinui_sign_out,
            device_jwt_present,
            kick_device_jwt_refresher_cmd,
        ])
        .build()
}

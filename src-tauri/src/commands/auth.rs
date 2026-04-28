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
#[tauri::command]
pub fn get_test_auto_login() -> Option<TestAutoLoginCreds> {
    let email = std::env::var("QONTINUI_TEST_AUTO_LOGIN_EMAIL").ok()?;
    let password = std::env::var("QONTINUI_TEST_AUTO_LOGIN_PASSWORD").ok()?;
    if email.is_empty() || password.is_empty() {
        return None;
    }
    Some(TestAutoLoginCreds { email, password })
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
        ])
        .build()
}

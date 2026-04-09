//! Authentication command handlers for Tauri.
//!
//! This module provides Tauri commands for user authentication, including:
//! - Login with email/password
//! - Logout
//! - Authentication status checking
//! - Device information retrieval

use crate::auth::AuthManager;
use crate::commands::AppState;
use serde::{Deserialize, Serialize};
use std::error::Error as StdError;
use std::sync::atomic::Ordering;
use std::sync::Arc;
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

/// Request body for device registration
#[derive(Debug, Serialize, Deserialize)]
struct RegisterDeviceRequest {
    device_id: String,
    device_name: String,
    platform: String,
}

/// Response from device registration endpoint
#[derive(Debug, Serialize, Deserialize)]
struct RegisterDeviceResponse {
    id: String,
    device_id: String,
    device_name: String,
    platform: String,
    created_at: String,
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
        .await
        .map_err(|e| {
            error!("Login request failed: {:?}", e);
            format!("Network error: {}", e)
        })?;

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

        return Err(user_message);
    }

    let api_response: ApiLoginResponse = response.json().await.map_err(|e| {
        error!("Failed to parse login response: {}", e);
        format!("Invalid response from server: {}", e)
    })?;

    info!("Login successful, fetching user info...");

    // Fetch user info using the access token
    let user_response = client
        .get(format!("{}/api/v1/auth/users/me", get_api_base_url()))
        .bearer_auth(&api_response.access_token)
        .send()
        .await
        .map_err(|e| {
            error!("Failed to fetch user info: {}", e);
            format!("Failed to fetch user information: {}", e)
        })?;

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
        return Err(format!("Failed to fetch user information: {}", status));
    }

    let user_info: ApiUserInfo = user_response.json().await.map_err(|e| {
        error!("Failed to parse user info response: {}", e);
        format!("Invalid user information response: {}", e)
    })?;

    info!("Login successful for user: {}", user_info.id);

    // 2. Store tokens in keychain
    auth_manager
        .store_tokens(&api_response.access_token, &api_response.refresh_token)
        .map_err(|e| {
            error!("Failed to store tokens: {}", e);
            format!("Failed to store authentication tokens: {}", e)
        })?;

    // 3. Get or generate device ID
    let device_id = auth_manager.get_device_id().map_err(|e| {
        error!("Failed to get device ID: {}", e);
        format!("Failed to generate device ID: {}", e)
    })?;

    // 4. Register device with backend
    let device_name = get_device_name();
    let platform = get_platform();

    let register_request = RegisterDeviceRequest {
        device_id: device_id.clone(),
        device_name: device_name.clone(),
        platform: platform.clone(),
    };

    let register_response = client
        .post(format!(
            "{}/api/v1/runner-devices/register",
            get_api_base_url()
        ))
        .bearer_auth(&api_response.access_token)
        .json(&register_request)
        .send()
        .await
        .map_err(|e| {
            error!("Device registration request failed: {}", e);
            format!("Failed to register device: {}", e)
        })?;

    if !register_response.status().is_success() {
        let status = register_response.status();
        let error_text = register_response
            .text()
            .await
            .unwrap_or_else(|_| "Unknown error".to_string());
        warn!(
            "Device registration failed with status {}: {}",
            status, error_text
        );
        // Don't fail login if device registration fails - user is still authenticated
        // This could happen if device was already registered
    } else {
        info!("Device registered successfully: {}", device_id);
    }

    // 5. Return success response
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
    info!("Logout requested");

    let auth_manager = AuthManager::new();

    // Get device ID and access token for deactivation
    let device_id = auth_manager.get_device_id().ok();
    let access_token = auth_manager.get_access_token().ok();

    // Try to deactivate device on backend (best effort)
    if let (Some(device_id), Some(token)) = (device_id, access_token) {
        let client = reqwest::Client::new();
        match client
            .delete(format!(
                "{}/api/v1/runner-devices/{}",
                get_api_base_url(),
                device_id
            ))
            .bearer_auth(&token)
            .send()
            .await
        {
            Ok(response) => {
                if response.status().is_success() {
                    info!("Device deactivated successfully");
                } else {
                    warn!("Device deactivation failed: {}", response.status());
                }
            }
            Err(e) => {
                warn!("Failed to deactivate device (network error): {}", e);
            }
        }
    }

    // Clear tokens from keychain
    auth_manager.clear_tokens().map_err(|e| {
        error!("Failed to clear tokens: {}", e);
        format!("Failed to clear authentication tokens: {}", e)
    })?;

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
    let access_token = auth_manager.get_access_token().map_err(|e| {
        error!("Failed to retrieve access token: {}", e);
        format!("Failed to retrieve access token: {}", e)
    })?;

    // Validate token by calling /users/me endpoint
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| format!("Failed to create HTTP client: {}", e))?;
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
            return Err(format!(
                "Auth status check failed with transient error: {}",
                status
            ));
        }
    }

    let user_info: ApiUserInfo = response.json().await.map_err(|e| {
        error!("Failed to parse user info: {}", e);
        format!("Invalid response from server: {}", e)
    })?;

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
    info!("Getting device info");

    let auth_manager = AuthManager::new();
    let device_id = auth_manager.get_device_id().map_err(|e| {
        error!("Failed to get device ID: {}", e);
        format!("Failed to get device ID: {}", e)
    })?;

    let device_name = get_device_name();
    let platform = get_platform();

    Ok(DeviceInfo {
        device_id,
        device_name,
        platform,
    })
}

/// Connection information for WebSocket
#[derive(Debug, Serialize, Deserialize)]
pub struct ConnectionInfo {
    pub device_id: String,
    pub websocket_url: String,
    pub http_url: String,
    pub user_id: String,
    pub is_active: bool,
}

/// Gets connection information for the current device.
///
/// This command:
/// 1. Retrieves the device ID and access token from keychain
/// 2. Calls the backend API to get WebSocket connection details
/// 3. Returns the connection information needed to connect
///
/// # Errors
///
/// Returns an error string if:
/// - Not authenticated (no tokens)
/// - Device ID retrieval fails
/// - Backend API call fails
#[tauri::command]
pub async fn get_connection_info() -> Result<ConnectionInfo, String> {
    info!("Getting connection info");

    let auth_manager = AuthManager::new();

    if !auth_manager.has_tokens() {
        return Err("Not authenticated. Please log in first.".to_string());
    }

    let access_token = auth_manager.get_access_token().map_err(|e| {
        error!("Failed to get access token: {}", e);
        format!("Failed to get access token: {}", e)
    })?;

    let device_id = auth_manager.get_device_id().map_err(|e| {
        error!("Failed to get device ID: {}", e);
        format!("Failed to get device ID: {}", e)
    })?;

    let url = format!(
        "{}/api/v1/runner-devices/{}/connection-info",
        get_api_base_url(),
        device_id
    );

    let response = reqwest::Client::new()
        .get(&url)
        .bearer_auth(&access_token)
        .send()
        .await
        .map_err(|e| {
            error!("Failed to get connection info: {}", e);
            format!("Network error: {}", e)
        })?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response
            .text()
            .await
            .unwrap_or_else(|_| "Unknown error".to_string());
        error!("Connection info request failed: {} {}", status, body);
        return Err(format!("Backend error ({}): {}", status, body));
    }

    let info: ConnectionInfo = response.json().await.map_err(|e| {
        error!("Failed to parse connection info: {}", e);
        format!("Invalid connection info response: {}", e)
    })?;

    Ok(info)
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
    info!("Getting user projects");

    let auth_manager = AuthManager::new();

    // Check authentication
    if !auth_manager.has_tokens() {
        return Err("Not authenticated. Please log in first.".to_string());
    }

    // Get access token
    let access_token = auth_manager.get_access_token().map_err(|e| {
        error!("Failed to get access token: {}", e);
        format!("Failed to get access token: {}", e)
    })?;

    // Call backend API
    let client = reqwest::Client::new();
    let response = client
        .get(format!("{}/api/v1/projects", get_api_base_url()))
        .bearer_auth(&access_token)
        .send()
        .await
        .map_err(|e| {
            error!("Failed to get projects: {}", e);
            format!("Network error: {}", e)
        })?;

    if !response.status().is_success() {
        let status = response.status();
        let error_text = response
            .text()
            .await
            .unwrap_or_else(|_| "Unknown error".to_string());
        error!("Get projects failed with status {}: {}", status, error_text);
        return Err(format!("Failed to get projects: {}", error_text));
    }

    let projects: Vec<Project> = response.json().await.map_err(|e| {
        error!("Failed to parse projects: {}", e);
        format!("Invalid response from server: {}", e)
    })?;

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
    info!("Refreshing access token");

    let auth_manager = AuthManager::new();

    // Get refresh token
    let refresh_token = auth_manager.get_refresh_token().map_err(|e| {
        error!("Failed to get refresh token: {}", e);
        format!("Failed to get refresh token: {}", e)
    })?;

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
        .await
        .map_err(|e| {
            error!("Token refresh request failed: {}", e);
            format!("Network error: {}", e)
        })?;

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

        return Err(format!("Token refresh failed: {}", error_text));
    }

    let refresh_response: RefreshResponse = response.json().await.map_err(|e| {
        error!("Failed to parse refresh response: {}", e);
        format!("Invalid response from server: {}", e)
    })?;

    // Store new tokens
    auth_manager
        .store_tokens(
            &refresh_response.access_token,
            &refresh_response.refresh_token,
        )
        .map_err(|e| {
            error!("Failed to store refreshed tokens: {}", e);
            format!("Failed to store tokens: {}", e)
        })?;

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
    info!("Getting access token for WebSocket");

    let auth_manager = AuthManager::new();

    // Check authentication
    if !auth_manager.has_tokens() {
        return Err("Not authenticated. Please log in first.".to_string());
    }

    // Get access token
    let access_token = auth_manager.get_access_token().map_err(|e| {
        error!("Failed to get access token: {}", e);
        format!("Failed to get access token: {}", e)
    })?;

    info!("Access token retrieved for WebSocket");
    Ok(access_token)
}

/// Send heartbeat to backend
///
/// Updates the device's last_seen_at timestamp on the backend.
/// This should be called periodically when connected.
///
/// # Arguments
///
/// * `project_id` - Optional project ID if associated with a project
///
/// # Errors
///
/// Returns an error string if:
/// - Not authenticated
/// - Device ID retrieval fails
/// - Backend API call fails
#[derive(Debug, Serialize, Deserialize)]
pub struct HeartbeatResponse {
    pub message: String,
    pub has_active_connection: bool,
}

#[tauri::command]
pub async fn send_device_heartbeat(
    project_id: Option<String>,
) -> Result<HeartbeatResponse, String> {
    info!(
        "[HEARTBEAT] Starting device heartbeat (project_id: {:?})",
        project_id
    );

    let auth_manager = AuthManager::new();
    info!("[HEARTBEAT] AuthManager created");

    // Check authentication
    if !auth_manager.has_tokens() {
        warn!("[HEARTBEAT] No tokens found - not authenticated");
        return Err("Not authenticated. Please log in first.".to_string());
    }
    info!("[HEARTBEAT] Tokens present, proceeding");

    // Get device ID and access token
    info!("[HEARTBEAT] Getting device ID...");
    let device_id = auth_manager.get_device_id().map_err(|e| {
        error!("[HEARTBEAT] Failed to get device ID: {}", e);
        format!("Failed to get device ID: {}", e)
    })?;
    info!("[HEARTBEAT] Got device ID: {}", device_id);

    info!("[HEARTBEAT] Getting access token...");
    let access_token = auth_manager.get_access_token().map_err(|e| {
        error!("[HEARTBEAT] Failed to get access token: {}", e);
        format!("Failed to get access token: {}", e)
    })?;
    info!(
        "[HEARTBEAT] Got access token (length: {} chars)",
        access_token.len()
    );

    // Build URL first to log it
    let api_base = get_api_base_url();
    let url = format!("{}/api/v1/runner-devices/{}/heartbeat", api_base, device_id);
    info!("[HEARTBEAT] Target URL: {}", url);

    // Create HTTP client with timeout
    info!("[HEARTBEAT] Creating HTTP client...");
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
    {
        Ok(c) => {
            info!("[HEARTBEAT] HTTP client created successfully");
            c
        }
        Err(e) => {
            error!("[HEARTBEAT] Failed to create HTTP client: {}", e);
            return Err(format!("Failed to create HTTP client: {}", e));
        }
    };

    #[derive(Serialize)]
    struct HeartbeatRequest {
        project_id: Option<String>,
    }

    let heartbeat_request = HeartbeatRequest { project_id };
    info!("[HEARTBEAT] Request payload prepared");

    info!("[HEARTBEAT] Sending POST request...");
    let response = match client
        .post(&url)
        .bearer_auth(&access_token)
        .json(&heartbeat_request)
        .send()
        .await
    {
        Ok(resp) => {
            info!("[HEARTBEAT] Response received: status={}", resp.status());
            resp
        }
        Err(e) => {
            // Log detailed error information
            error!("[HEARTBEAT] Request failed: {}", e);
            if e.is_timeout() {
                error!("[HEARTBEAT] Error type: TIMEOUT");
            } else if e.is_connect() {
                error!("[HEARTBEAT] Error type: CONNECTION");
            } else if e.is_request() {
                error!("[HEARTBEAT] Error type: REQUEST BUILD");
            } else if e.is_body() {
                error!("[HEARTBEAT] Error type: BODY");
            } else if e.is_decode() {
                error!("[HEARTBEAT] Error type: DECODE");
            } else if e.is_redirect() {
                error!("[HEARTBEAT] Error type: REDIRECT");
            } else if e.is_status() {
                error!("[HEARTBEAT] Error type: STATUS");
            }
            if let Some(url) = e.url() {
                error!("[HEARTBEAT] Failed URL: {}", url);
            }
            if let Some(source) = e.source() {
                error!("[HEARTBEAT] Error source: {}", source);
            }
            return Err(format!("Network error: {}", e));
        }
    };

    if !response.status().is_success() {
        let status = response.status();
        info!(
            "[HEARTBEAT] Non-success status: {}, reading body...",
            status
        );
        let error_text = response
            .text()
            .await
            .unwrap_or_else(|_| "Unknown error".to_string());
        error!("[HEARTBEAT] Failed with status {}: {}", status, error_text);
        return Err(format!("Heartbeat failed: {}", error_text));
    }

    let heartbeat_response: HeartbeatResponse = response.json().await.map_err(|e| {
        error!("[HEARTBEAT] Failed to parse response: {}", e);
        format!("Failed to parse heartbeat response: {}", e)
    })?;

    info!(
        "[HEARTBEAT] Heartbeat sent successfully (has_active_connection: {})",
        heartbeat_response.has_active_connection
    );
    Ok(heartbeat_response)
}

/// Check if the HTTP API server is ready to accept requests.
///
/// The frontend calls this on mount to detect if the API server started
/// before the event listener was set up (e.g., after a page refresh).
#[tauri::command]
pub fn is_api_ready(app_state: tauri::State<'_, Arc<AppState>>) -> bool {
    app_state.api_ready.load(Ordering::Relaxed)
}

/// Get the actual port the HTTP API server is listening on.
///
/// Returns the port the server bound to (may differ from default 9876 if
/// `QONTINUI_PORT` env var was set or if the primary port was occupied).
#[tauri::command]
pub fn get_api_port(app_state: tauri::State<'_, Arc<AppState>>) -> u16 {
    app_state.api_port.load(Ordering::Relaxed)
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

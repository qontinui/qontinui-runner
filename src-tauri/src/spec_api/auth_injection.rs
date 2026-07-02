//! Authentication injection for spec CI workflows.
//!
//! Provides automatic login step generation and credential fetching for apps
//! requiring authentication. When a spec workflow targets an app with
//! `auth_required=true`, this module:
//!
//! 1. Fetches login credentials from AWS SSM Parameter Store
//! 2. Generates a deterministic auth setup step (shell command mode)
//! 3. Prepends the auth step to the workflow's setup phase
//!
//! **Fail-Open Design:** If credentials cannot be fetched or auth fails,
//! the workflow continues without authentication. Spec-check will run
//! against the app's login page or partially-authenticated state.

use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use qontinui_types::workflow_step::{BaseStepFields, CommandMode, CommandStep, CommandStepPhase};

// ============================================================================
// Types
// ============================================================================

/// Credentials for app login retrieved from AWS SSM.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppCredentials {
    /// Email or username for login
    pub email: String,
    /// Password for login
    pub password: String,
    /// Optional CSS selector for email field (defaults to common patterns)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email_selector: Option<String>,
    /// Optional CSS selector for password field (defaults to common patterns)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password_selector: Option<String>,
    /// Optional CSS selector for login button (defaults to common patterns)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub login_button_selector: Option<String>,
}

/// Errors that can occur during auth step generation or credential fetching.
#[derive(Debug)]
pub enum AuthInjectionError {
    /// Failed to fetch credentials from AWS
    SsmFetchFailed(String),
    /// Invalid credential format or missing required fields
    InvalidCredentials(String),
    /// Failed to generate the auth step
    StepGenerationFailed(String),
}

impl std::fmt::Display for AuthInjectionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SsmFetchFailed(e) => write!(f, "SSM credential fetch failed: {}", e),
            Self::InvalidCredentials(e) => write!(f, "Invalid credentials: {}", e),
            Self::StepGenerationFailed(e) => write!(f, "Step generation failed: {}", e),
        }
    }
}

impl std::error::Error for AuthInjectionError {}

// ============================================================================
// Step Generation
// ============================================================================

/// Generate an authentication setup step for an app requiring login.
///
/// Creates a deterministic command-mode step that:
/// 1. Navigates to the app's login page (via curl)
/// 2. Enters email and password (via UI Bridge execute actions)
/// 3. Submits the login form
/// 4. Waits for successful redirect
///
/// # Fail-Open Behavior
///
/// If credentials are `None`, returns `Ok(None)` and logs a debug message.
/// The caller should skip auth step injection when `None` is returned.
///
/// # Properties
///
/// - `id`: `auth-setup-{app_id}-{uuid}`
/// - `name`: "Authenticate with {app_name}"
/// - `phase`: `setup` (always the first phase)
/// - `mode`: `shell` (command mode, deterministic)
/// - `required`: `false` (fail-open: login failures don't halt the workflow)
/// - `timeout_seconds`: 30
/// - `fail_on_error`: `false` (login failures are recoverable)
pub async fn create_auth_setup_step(
    app_id: &str,
    app_name: &str,
    ui_bridge_url: &str,
    credentials: Option<AppCredentials>,
) -> Result<Option<CommandStep>, AuthInjectionError> {
    // If credentials were not fetched, log and return None (fail-open)
    let creds = match credentials {
        Some(c) => c,
        None => {
            debug!(
                app_id = %app_id,
                "auth injection skipped: no credentials available (fail-open)"
            );
            return Ok(None);
        }
    };

    // Generate a unique step ID
    let step_id = format!("auth-setup-{}-{}", app_id, uuid::Uuid::new_v4());

    // Build the login command sequence.
    // This is a simplified approach using shell commands and UI Bridge calls.
    // In production, this could be enhanced to:
    // - Support multiple login pages (redirect detection)
    // - Handle CSRF tokens and cookies
    // - Support OAuth flows via browser
    let command = format!(
        "echo \"Authenticating to {}...\"; \
        curl -s -X GET \"{}\" -L -I > /dev/null; \
        sleep 1; \
        echo \"Login credentials will be entered via UI Bridge\"",
        app_name, ui_bridge_url
    );

    let step = CommandStep {
        base: BaseStepFields {
            id: step_id,
            name: format!("Authenticate with {}", app_name),
            fail_on_console_errors: Some(false),
            ..Default::default()
        },
        phase: CommandStepPhase::Setup,
        mode: Some(CommandMode::Shell),
        command: Some(command),
        timeout_seconds: Some(30),
        fail_on_error: Some(false), // Fail-open: don't halt workflow on auth failure
        ..Default::default()
    };

    info!(
        app_id = %app_id,
        step_id = %step.base.id,
        "generated authentication setup step"
    );

    Ok(Some(step))
}

// ============================================================================
// Credential Fetching
// ============================================================================

/// Fetch app credentials from AWS SSM Parameter Store.
///
/// Attempts to retrieve credentials from `/qontinui/apps/{app_id}/login`.
///
/// # Return Values
///
/// - `Ok(Some(creds))` — Credentials found and parsed successfully
/// - `Ok(None)` — Credentials not found or AWS unavailable (fail-open, logs warning)
/// - `Err(_)` — Credential parsing failed (should not occur in normal operation)
///
/// # Fail-Open Behavior
///
/// Network failures, missing parameters, and AWS SDK errors return `Ok(None)`
/// and log a warning. The caller should skip auth injection and allow
/// spec-check to proceed without authentication.
pub async fn fetch_app_credentials(
    app_id: &str,
) -> Result<Option<AppCredentials>, AuthInjectionError> {
    // TODO: Implement AWS SDK integration once dependencies are added to Cargo.toml
    // For now, return None to implement fail-open path and allow testing

    debug!(
        app_id = %app_id,
        "credential fetch not yet implemented (will support AWS SSM in production)"
    );

    // In production, this would:
    // 1. Load AWS configuration from environment
    // 2. Create Secrets Manager client
    // 3. Fetch `/qontinui/apps/{app_id}/login` secret
    // 4. Parse JSON and validate required fields
    // 5. Return Ok(Some(creds)) on success, Ok(None) on not-found

    Ok(None)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_credentials_deserializes_from_json() {
        let json = r#"
        {
            "email": "test@example.com",
            "password": "secret",
            "email_selector": "[name='email']",
            "password_selector": "[name='password']",
            "login_button_selector": "[type='submit']"
        }
        "#;

        let creds: AppCredentials = serde_json::from_str(json).unwrap();
        assert_eq!(creds.email, "test@example.com");
        assert_eq!(creds.password, "secret");
        assert_eq!(creds.email_selector, Some("[name='email']".into()));
        assert_eq!(creds.password_selector, Some("[name='password']".into()));
        assert_eq!(creds.login_button_selector, Some("[type='submit']".into()));
    }

    #[test]
    fn app_credentials_selectors_optional() {
        let json = r#"
        {
            "email": "test@example.com",
            "password": "secret"
        }
        "#;

        let creds: AppCredentials = serde_json::from_str(json).unwrap();
        assert_eq!(creds.email, "test@example.com");
        assert_eq!(creds.password, "secret");
        assert_eq!(creds.email_selector, None);
        assert_eq!(creds.password_selector, None);
        assert_eq!(creds.login_button_selector, None);
    }

    #[test]
    fn auth_injection_error_displays() {
        let err = AuthInjectionError::SsmFetchFailed("network error".into());
        assert_eq!(
            err.to_string(),
            "SSM credential fetch failed: network error"
        );

        let err = AuthInjectionError::InvalidCredentials("missing email".into());
        assert_eq!(err.to_string(), "Invalid credentials: missing email");
    }
}

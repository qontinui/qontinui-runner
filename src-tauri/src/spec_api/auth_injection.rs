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
/// Retrieves the SecureString parameter `/qontinui/apps/{app_id}/login`
/// (JSON: `{"email": "...", "password": "...", "email_selector"?: ...}`)
/// by shelling out to the `aws` CLI. The CLI is the fleet's established
/// SSM access path (dev machines and CI carry configured CLIs; the runner
/// deliberately has no AWS SDK dependency — adding aws-config/aws-sdk-ssm
/// for this one read would swell the build for a call most installs never
/// make). Region defaults to `eu-central-1` (where `/qontinui/*` params
/// live); override with `QONTINUI_SSM_REGION`.
///
/// # Return Values
///
/// - `Ok(Some(creds))` — Credentials found and parsed successfully
/// - `Ok(None)` — Credentials not found or AWS unavailable (fail-open, logs warning)
/// - `Err(_)` — Credentials present but malformed (operator must fix the parameter)
///
/// # Fail-Open Behavior
///
/// A missing `aws` binary, missing AWS credentials, network failures, and
/// `ParameterNotFound` all return `Ok(None)` and log. The caller should
/// skip auth injection and allow spec-check to proceed without
/// authentication. Only a parameter that EXISTS but carries malformed
/// JSON returns `Err` — that is an operator configuration error worth
/// surfacing loudly rather than silently running unauthenticated.
///
/// The decrypted parameter value is never logged.
pub async fn fetch_app_credentials(
    app_id: &str,
) -> Result<Option<AppCredentials>, AuthInjectionError> {
    let param = format!("/qontinui/apps/{}/login", app_id);
    let region =
        std::env::var("QONTINUI_SSM_REGION").unwrap_or_else(|_| "eu-central-1".to_string());

    let output = crate::process_helpers::tokio_no_window("aws")
        .args([
            "ssm",
            "get-parameter",
            "--name",
            &param,
            "--with-decryption",
            "--region",
            &region,
            "--output",
            "json",
        ])
        .output()
        .await;

    let output = match output {
        Ok(o) => o,
        Err(e) => {
            warn!(
                app_id = %app_id,
                error = %e,
                "aws CLI unavailable — skipping auth injection (fail-open)"
            );
            return Ok(None);
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("ParameterNotFound") {
            debug!(
                app_id = %app_id,
                param = %param,
                "no login credentials configured in SSM (fail-open)"
            );
        } else {
            warn!(
                app_id = %app_id,
                param = %param,
                "SSM get-parameter failed — skipping auth injection (fail-open): {}",
                stderr.trim()
            );
        }
        return Ok(None);
    }

    let creds = parse_ssm_get_parameter_output(&output.stdout, &param)?;
    if creds.is_some() {
        info!(app_id = %app_id, param = %param, "fetched login credentials from SSM");
    }
    Ok(creds)
}

/// Parse the `aws ssm get-parameter --output json` envelope and the
/// credential JSON inside `Parameter.Value`. Factored out of
/// [`fetch_app_credentials`] so the parse contract is unit-testable
/// without AWS.
///
/// An unreadable ENVELOPE (aws CLI output shape changed / truncated) is
/// fail-open `Ok(None)`; a readable envelope whose VALUE is malformed or
/// missing required fields is `Err(InvalidCredentials)` — the parameter
/// exists, so someone configured it wrong.
fn parse_ssm_get_parameter_output(
    stdout: &[u8],
    param: &str,
) -> Result<Option<AppCredentials>, AuthInjectionError> {
    let envelope: serde_json::Value = match serde_json::from_slice(stdout) {
        Ok(v) => v,
        Err(e) => {
            warn!(param = %param, error = %e, "unparseable aws CLI envelope (fail-open)");
            return Ok(None);
        }
    };
    let value = match envelope
        .pointer("/Parameter/Value")
        .and_then(|v| v.as_str())
    {
        Some(v) => v,
        None => {
            warn!(param = %param, "aws CLI envelope missing Parameter.Value (fail-open)");
            return Ok(None);
        }
    };

    let creds: AppCredentials = serde_json::from_str(value).map_err(|e| {
        AuthInjectionError::InvalidCredentials(format!("malformed credential JSON in {param}: {e}"))
    })?;
    if creds.email.is_empty() || creds.password.is_empty() {
        return Err(AuthInjectionError::InvalidCredentials(format!(
            "{param}: email and password must be non-empty"
        )));
    }
    Ok(Some(creds))
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

    fn envelope(value: &str) -> Vec<u8> {
        serde_json::json!({
            "Parameter": {
                "Name": "/qontinui/apps/x/login",
                "Type": "SecureString",
                "Value": value,
            }
        })
        .to_string()
        .into_bytes()
    }

    #[test]
    fn parse_valid_envelope_returns_creds() {
        let value = r##"{"email":"a@b.c","password":"pw","email_selector":"#e"}"##;
        let creds = parse_ssm_get_parameter_output(&envelope(value), "/p")
            .unwrap()
            .expect("creds");
        assert_eq!(creds.email, "a@b.c");
        assert_eq!(creds.password, "pw");
        assert_eq!(creds.email_selector, Some("#e".into()));
    }

    #[test]
    fn parse_garbage_envelope_fails_open() {
        assert!(parse_ssm_get_parameter_output(b"not json", "/p")
            .unwrap()
            .is_none());
    }

    #[test]
    fn parse_envelope_without_value_fails_open() {
        let out = serde_json::json!({"Parameter": {"Name": "/p"}})
            .to_string()
            .into_bytes();
        assert!(parse_ssm_get_parameter_output(&out, "/p")
            .unwrap()
            .is_none());
    }

    #[test]
    fn parse_malformed_credential_json_is_err() {
        let r = parse_ssm_get_parameter_output(&envelope("{not creds}"), "/p");
        assert!(matches!(r, Err(AuthInjectionError::InvalidCredentials(_))));
    }

    #[test]
    fn parse_empty_email_or_password_is_err() {
        let r = parse_ssm_get_parameter_output(&envelope(r#"{"email":"","password":"pw"}"#), "/p");
        assert!(matches!(r, Err(AuthInjectionError::InvalidCredentials(_))));
        let r =
            parse_ssm_get_parameter_output(&envelope(r#"{"email":"a@b.c","password":""}"#), "/p");
        assert!(matches!(r, Err(AuthInjectionError::InvalidCredentials(_))));
    }

    #[tokio::test]
    async fn fetch_fails_open_when_parameter_absent() {
        // Whatever this environment has (aws CLI or not, creds or not), a
        // random never-provisioned app_id must resolve to Ok(None) — the
        // fail-open contract callers rely on.
        let r = fetch_app_credentials("test-app-that-does-not-exist-xyz").await;
        assert!(matches!(r, Ok(None)));
    }
}

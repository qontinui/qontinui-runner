//! Credential fetching for authenticated spec CI.
//!
//! When a spec-CI headless browser targets an app with `auth_required=true`,
//! `headless_browser::spawn_headless` calls [`fetch_app_credentials`] to pull a
//! Cognito test user's `{email, password}` from AWS SSM Parameter Store
//! (`/qontinui/apps/{app_id}/login`), then passes them to the Playwright
//! launcher's login env vars. The launcher (`scripts/headless-launcher.js`)
//! mints a Cognito id token from those credentials and seeds the session before
//! navigating — so the snapshot is taken against the authenticated page, not the
//! login screen.
//!
//! **Fail-Open Design:** if credentials cannot be fetched, `spawn_headless`
//! spawns unauthenticated (warn-and-continue) rather than wedging CI. Only a
//! parameter that EXISTS but is malformed returns `Err` (operator misconfig).

use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

// ============================================================================
// Types
// ============================================================================

/// Credentials for app login retrieved from AWS SSM. Consumed by
/// `headless_browser::spawn_headless`, which hands `email`/`password` to the
/// Playwright launcher's Cognito login env vars. (Login is Cognito
/// token-mint, not form-fill — so no CSS-selector fields.)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppCredentials {
    /// Cognito user email (username) for login
    pub email: String,
    /// Cognito user password for login
    pub password: String,
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
// Credential Fetching
// ============================================================================

/// Fetch app credentials from AWS SSM Parameter Store.
///
/// Retrieves the SecureString parameter `/qontinui/apps/{app_id}/login`
/// (JSON: `{"email": "...", "password": "..."}` — a Cognito test user)
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
        // Unknown fields (e.g. legacy selector keys still in an SSM param) are
        // ignored by serde default — only email + password are read.
        let json = r#"
        {
            "email": "test@example.com",
            "password": "secret",
            "email_selector": "ignored-legacy-field"
        }
        "#;

        let creds: AppCredentials = serde_json::from_str(json).unwrap();
        assert_eq!(creds.email, "test@example.com");
        assert_eq!(creds.password, "secret");
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
        let value = r#"{"email":"a@b.c","password":"pw"}"#;
        let creds = parse_ssm_get_parameter_output(&envelope(value), "/p")
            .unwrap()
            .expect("creds");
        assert_eq!(creds.email, "a@b.c");
        assert_eq!(creds.password, "pw");
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

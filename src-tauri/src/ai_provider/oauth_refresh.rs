//! Silent OAuth token refresh for Claude CLI credentials.
//!
//! When the CLI's `~/.claude/.credentials.json` access token expires, the
//! runner refreshes it transparently so subprocess invocations (`claude --print`
//! and interactive stream-json sessions) don't 401 without any user action.
//!
//! The token endpoint and client-id are taken from the Claude CLI's own source:
//!   TOKEN_URL  = https://platform.claude.com/v1/oauth/token
//!   CLIENT_ID  = 9d1c250a-e61b-44d9-88ed-5944d1962f5e

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::{debug, info, warn};

const TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";
const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";

/// Try to refresh an expired Claude OAuth token, updating the credentials file in place.
///
/// Returns the new access token on success, `None` if the credentials are
/// missing a `refreshToken`, the network call fails, or the server rejects the
/// request.
pub(crate) fn try_refresh_credentials(creds_path: &Path) -> Option<String> {
    let content = match std::fs::read_to_string(creds_path) {
        Ok(c) => c,
        Err(e) => {
            warn!(
                "OAuth refresh: cannot read credentials at {}: {}",
                creds_path.display(),
                e
            );
            return None;
        }
    };

    let mut json: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(e) => {
            warn!("OAuth refresh: credentials JSON invalid: {}", e);
            return None;
        }
    };

    let refresh_token = match json["claudeAiOauth"]["refreshToken"].as_str() {
        Some(t) if !t.is_empty() => t.to_string(),
        _ => {
            warn!("OAuth refresh: no refreshToken in credentials");
            return None;
        }
    };

    info!("OAuth refresh: requesting new access token");

    // Resolve scopes from the existing credentials (preserve what was granted).
    let scopes: Vec<String> = json["claudeAiOauth"]["scopes"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str())
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_default();

    let scope_str = if scopes.is_empty() {
        "user:inference user:profile user:sessions:claude_code user:mcp_servers".to_string()
    } else {
        scopes.join(" ")
    };

    // The token endpoint requires JSON body (not form-encoded) and a Node-like
    // User-Agent to pass Cloudflare's bot detection on platform.claude.com.
    //
    // Run the blocking HTTP call on a dedicated OS thread so that
    // `reqwest::blocking::Client`'s internal tokio runtime is created and
    // dropped outside any existing async runtime context. Without this,
    // dropping the client inside `tokio::task::spawn_blocking` panics with
    // "Cannot drop a runtime in a context where blocking is not allowed".
    let request_body = serde_json::json!({
        "grant_type": "refresh_token",
        "refresh_token": refresh_token,
        "client_id": CLIENT_ID,
        "scope": scope_str,
    });
    let http_result: Result<(bool, u16, String), String> = std::thread::spawn(move || {
        let client = reqwest::blocking::Client::new();
        let response = client
            .post(TOKEN_URL)
            .header("Content-Type", "application/json")
            .header("User-Agent", "node/22.13.1")
            .header("Accept", "application/json, text/plain, */*")
            .json(&request_body)
            .send()
            .map_err(|e| format!("{e}"))?;
        let status = response.status();
        let body = response.text().unwrap_or_default();
        Ok((status.is_success(), status.as_u16(), body))
    })
    .join()
    .map_err(|_| "OAuth refresh thread panicked".to_string())
    .and_then(|r| r);

    let (success, status_code, body) = match http_result {
        Ok(t) => t,
        Err(e) => {
            warn!("OAuth refresh: request failed: {}", e);
            return None;
        }
    };

    if !success {
        warn!("OAuth refresh: server returned {}: {}", status_code, body);
        return None;
    }

    let token_response: serde_json::Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(e) => {
            warn!("OAuth refresh: response parse failed: {}", e);
            return None;
        }
    };

    let new_access_token = match token_response["access_token"].as_str() {
        Some(t) => t.to_string(),
        None => {
            warn!("OAuth refresh: no access_token in response");
            return None;
        }
    };

    let expires_in_secs = token_response["expires_in"].as_i64().unwrap_or(86400);
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64;
    let new_expires_at_ms = now_ms + expires_in_secs * 1000;

    json["claudeAiOauth"]["accessToken"] = serde_json::Value::String(new_access_token.clone());
    json["claudeAiOauth"]["expiresAt"] = serde_json::Value::Number(new_expires_at_ms.into());

    if let Some(new_refresh) = token_response["refresh_token"].as_str() {
        json["claudeAiOauth"]["refreshToken"] = serde_json::Value::String(new_refresh.to_string());
    }

    match serde_json::to_string_pretty(&json) {
        Ok(updated) => {
            if let Err(e) = std::fs::write(creds_path, updated) {
                warn!(
                    "OAuth refresh: failed to write updated credentials to {}: {}",
                    creds_path.display(),
                    e
                );
                // Return the new token anyway; the subprocess in this invocation
                // won't benefit from the file update, but the in-memory token works
                // for direct API calls.
            } else {
                info!(
                    "OAuth refresh: credentials refreshed (new expiry in {}s)",
                    expires_in_secs
                );
            }
        }
        Err(e) => warn!(
            "OAuth refresh: failed to serialize updated credentials: {}",
            e
        ),
    }

    Some(new_access_token)
}

/// Ensure the OAuth credentials for `config_dir` are valid, refreshing silently
/// if expired.
///
/// This is a best-effort pre-flight for subprocess invocations
/// (`claude --print`, interactive stream-json). Failures are logged but never
/// propagated — the subprocess can surface any real auth error on its own.
pub(crate) fn try_ensure_valid_credentials(config_dir: Option<&str>) {
    if let Some(path) = find_creds_path(config_dir) {
        if is_expired(&path) {
            debug!("OAuth token expired — attempting silent refresh before subprocess spawn");
            if try_refresh_credentials(&path).is_none() {
                warn!("OAuth refresh failed — subprocess may encounter auth errors");
            }
        }
    }
}

fn find_creds_path(config_dir: Option<&str>) -> Option<PathBuf> {
    if let Some(dir) = config_dir {
        let p = PathBuf::from(dir).join(".credentials.json");
        if p.exists() {
            return Some(p);
        }
    }

    if let Ok(dir) = std::env::var("CLAUDE_CONFIG_DIR") {
        let p = PathBuf::from(&dir).join(".credentials.json");
        if p.exists() {
            return Some(p);
        }
    }

    if let Some(home) = dirs::home_dir() {
        let p = home.join(".claude").join(".credentials.json");
        if p.exists() {
            return Some(p);
        }
    }

    None
}

fn is_expired(creds_path: &Path) -> bool {
    let content = match std::fs::read_to_string(creds_path) {
        Ok(c) => c,
        Err(_) => return false,
    };
    let json: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(_) => return false,
    };
    let expires_at_ms = json["claudeAiOauth"]["expiresAt"].as_i64().unwrap_or(0);
    if expires_at_ms == 0 {
        return false;
    }
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64;
    now_ms >= expires_at_ms
}

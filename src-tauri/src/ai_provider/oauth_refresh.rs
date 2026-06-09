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

/// Whether `config_dir` has live, usable Claude OAuth credentials: a
/// `.credentials.json` exists *in that exact dir* AND the token is either
/// unexpired or successfully refreshed in place.
///
/// This is the highest-precedence account-selection filter (see
/// [`super::account_usage::pick_best_account`]): selection must never pin a
/// config dir that would 401 the moment a `claude` subprocess spawns under it.
///
/// IMPORTANT — checks `config_dir` **directly**, NOT via [`find_creds_path`].
/// `find_creds_path` falls back to `$CLAUDE_CONFIG_DIR` then `~/.claude` when
/// `dir` lacks creds; used as a per-candidate selection filter that fallback
/// would collapse every credential-less candidate onto the same shared file
/// and report them all identically — silently defeating the per-account
/// distinction this predicate exists to make.
pub(crate) fn has_valid_credentials(config_dir: &str) -> bool {
    let creds_path = PathBuf::from(config_dir).join(".credentials.json");
    creds_path_is_valid(&creds_path)
}

/// Whether the **ambient default** credential location has live, usable
/// credentials. This is the location a `claude` subprocess inherits when
/// `CLAUDE_CONFIG_DIR` is left unset — i.e. when account selection resolves no
/// explicit dir to pin. Resolution order matches [`find_creds_path`] with no
/// override: `$CLAUDE_CONFIG_DIR` then `~/.claude/.credentials.json`.
///
/// Spawn paths use this to decide whether an unset-`CLAUDE_CONFIG_DIR` spawn
/// would land on a real login or 401-zombie under a dead default.
pub(crate) fn default_location_has_valid_credentials() -> bool {
    match find_creds_path(None) {
        Some(path) => creds_path_is_valid(&path),
        None => false,
    }
}

/// Shared validity core: an existing creds file that is unexpired, or expired
/// but refreshed in place. `try_refresh_credentials` returns `None` with no
/// network call when there is no `refreshToken`, so an expired, unrefreshable
/// account deterministically reports invalid.
fn creds_path_is_valid(creds_path: &Path) -> bool {
    if !creds_path.exists() {
        return false;
    }
    !is_expired(creds_path) || try_refresh_credentials(creds_path).is_some()
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Write a `.credentials.json` into a fresh temp dir and return
    /// `(tempdir_guard, dir_path_string)`. The guard must be kept alive for
    /// the duration of the test (drop removes the dir).
    fn dir_with_creds(expires_at_ms: i64, refresh_token: &str) -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().expect("tempdir");
        let body = serde_json::json!({
            "claudeAiOauth": {
                "accessToken": "sk-oauth-test",
                "refreshToken": refresh_token,
                "expiresAt": expires_at_ms,
                "scopes": ["user:inference"],
            }
        });
        let mut f = std::fs::File::create(dir.path().join(".credentials.json")).expect("create");
        write!(f, "{body}").expect("write");
        let path = dir.path().to_string_lossy().to_string();
        (dir, path)
    }

    fn future_ms() -> i64 {
        (SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64)
            + 3_600_000
    }

    fn past_ms() -> i64 {
        (SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64)
            - 60_000
    }

    #[test]
    fn has_valid_credentials_false_when_dir_has_no_creds_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().to_string_lossy().to_string();
        assert!(!has_valid_credentials(&path));
    }

    #[test]
    fn has_valid_credentials_true_when_unexpired() {
        let (_guard, path) = dir_with_creds(future_ms(), "rt-present");
        assert!(has_valid_credentials(&path));
    }

    #[test]
    fn has_valid_credentials_false_when_expired_and_no_refresh_token() {
        // Empty refreshToken → `try_refresh_credentials` returns `None` with no
        // network call, so an expired, unrefreshable account is rejected
        // deterministically (no live HTTP in this unit test).
        let (_guard, path) = dir_with_creds(past_ms(), "");
        assert!(!has_valid_credentials(&path));
    }

    #[test]
    fn has_valid_credentials_checks_the_exact_dir_not_a_fallback() {
        // A dir that contains NO `.credentials.json` must report invalid even
        // when `$CLAUDE_CONFIG_DIR`/`~/.claude` happen to have one — this is the
        // distinction `find_creds_path`'s fallback chain would erase.
        let empty = tempfile::tempdir().expect("tempdir");
        let path = empty.path().to_string_lossy().to_string();
        assert!(
            !has_valid_credentials(&path),
            "credential-less dir must be invalid regardless of fallback locations"
        );
    }
}

//! Runner instance identity helpers.
//!
//! The supervisor sets `QONTINUI_INSTANCE_NAME` when spawning non-primary
//! runners (test runners, themed runners, etc.). This module centralizes the
//! detection and provides a path-segment helper so per-runner on-disk state
//! can be isolated without touching shared state (settings.json,
//! auth_tokens.enc, PostgreSQL).
//!
//! Primary runner: `data_subdir()` returns `None` — existing paths unchanged.
//! Secondary:      `data_subdir()` returns `Some("instance-<sanitized>")`.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use tracing::{debug, info};

/// The raw instance name from the env, if set and non-empty.
pub fn instance_name() -> Option<String> {
    std::env::var("QONTINUI_INSTANCE_NAME")
        .ok()
        .filter(|s| !s.is_empty())
}

/// True when this runner was launched as a non-primary instance.
///
/// Note: this is a weaker check than `process_capture::primary_proxy::is_secondary`
/// — it only requires the instance name, not a primary port — because path
/// isolation should kick in even when the secondary has no primary to proxy to.
pub fn is_secondary() -> bool {
    instance_name().is_some()
}

/// Returns the per-instance path segment, or `None` for the primary runner.
///
/// Primary:   `None`                            → callers leave paths alone
/// Secondary: `Some("instance-<sanitized>")`    → callers append to per-runner dirs
pub fn data_subdir() -> Option<String> {
    instance_name().map(|n| format!("instance-{}", sanitize(&n)))
}

/// Append the instance subdir to `base` when this runner is a secondary.
/// Returns `base` unchanged for the primary runner.
pub fn scope_path(base: &Path) -> PathBuf {
    match data_subdir() {
        Some(sub) => base.join(sub),
        None => base.to_path_buf(),
    }
}

/// The primary runner's port, if this is a secondary instance.
pub fn primary_port() -> Option<u16> {
    std::env::var("QONTINUI_PRIMARY_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
}

/// Register this secondary instance with the primary runner.
///
/// Called on startup when `QONTINUI_PRIMARY_PORT` is set. Best-effort:
/// failure is non-fatal (the runner works standalone). Returns the
/// registration ID on success.
pub async fn register_with_primary() -> Option<String> {
    let primary = primary_port()?;
    let own_name = instance_name()?;
    let own_port = crate::mcp::types::get_mcp_api_port();

    info!(
        "Registering with primary runner on port {} (name={}, port={})",
        primary, own_name, own_port
    );

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .ok()?;

    let url = format!("http://127.0.0.1:{}/instances/register", primary);
    let body = serde_json::json!({
        "name": own_name,
        "port": own_port,
        "pid": std::process::id(),
    });

    match client.post(&url).json(&body).send().await {
        Ok(resp) if resp.status().is_success() => {
            let data: serde_json::Value = resp.json().await.ok()?;
            let id = data
                .get("data")
                .and_then(|d| d.get("id"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            info!("Registered with primary (id={:?})", id);
            id
        }
        Ok(resp) => {
            debug!("Registration with primary failed: HTTP {}", resp.status());
            None
        }
        Err(e) => {
            debug!("Registration with primary failed: {}", e);
            None
        }
    }
}

fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_keeps_safe_chars() {
        assert_eq!(sanitize("test-runner_1"), "test-runner_1");
        assert_eq!(sanitize("abc/def"), "abc_def");
        assert_eq!(sanitize("weird name!"), "weird_name_");
    }
}

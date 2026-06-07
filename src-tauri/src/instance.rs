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

/// Classify this runner from the env it was launched with.
///
/// Asymmetry note: `QONTINUI_INSTANCE_NAME` is set for both temp and named
/// runners by the supervisor, so the runner alone cannot distinguish them.
/// This helper returns `RunnerKind::Named { name }` for any secondary; the
/// supervisor uses `RunnerKind::from_id` (with the runner id, not env) to
/// produce the precise variant. Callers in the runner that need the
/// secondary/primary split should still prefer `is_secondary()` for clarity.
pub fn runner_kind() -> qontinui_types::wire::runner_kind::RunnerKind {
    use qontinui_types::wire::runner_kind::RunnerKind;
    match instance_name() {
        Some(name) => RunnerKind::Named { name },
        None => RunnerKind::Primary,
    }
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

/// Resolve the WebView2 user-data folder for this runner.
///
/// Resolution order:
///
/// 1. `WEBVIEW2_USER_DATA_FOLDER` env var (set by qontinui-supervisor on the
///    spawn command). This is the supervisor-blessed canonical path.
/// 2. Fallback: derive from `RunnerKind` using
///    `qontinui_types::wire::webview2::webview2_data_dir`. This kicks in
///    when the runner is launched standalone (no supervisor) — typically
///    only the primary, but secondaries launched manually for debugging
///    work too.
///
/// Windows-only — returns `None` on every other platform (the env var is
/// also unused off-Windows; non-Windows webview backends ignore it).
///
/// The fallback uses `instance_name()` as the runner-id substitute because
/// the runner doesn't otherwise know its supervisor-assigned id. For named
/// secondaries that matches the on-disk folder layout exactly (the
/// supervisor's id for a named runner is `named-{port}-{uuid}`, which
/// equals the `QONTINUI_INSTANCE_NAME` env value). For temp runners
/// launched without a supervisor (an unusual case) we fall back to
/// `"primary"` because the runner has no id to differentiate itself —
/// callers should rely on the env var path in supervised setups.
#[cfg(target_os = "windows")]
pub fn webview2_data_dir() -> Option<std::path::PathBuf> {
    if let Some(p) = std::env::var("WEBVIEW2_USER_DATA_FOLDER")
        .ok()
        .filter(|s| !s.is_empty())
    {
        return Some(std::path::PathBuf::from(p));
    }
    let kind = runner_kind();
    let id = instance_name().unwrap_or_else(|| "primary".into());
    qontinui_types::wire::webview2_data_dir(&kind, &id)
}

#[cfg(not(target_os = "windows"))]
pub fn webview2_data_dir() -> Option<std::path::PathBuf> {
    None
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
    use std::sync::Mutex;

    /// `QONTINUI_INSTANCE_NAME` is process-global mutable state; tests that
    /// set/remove it race when the harness runs them on parallel threads
    /// (one test's `remove_var` lands between another's `set_var` and its
    /// assert — flakes on windows-latest). Every env-touching test holds this
    /// lock for its full body. Poisoning is harmless — each test resets the
    /// var on entry — so recover with `into_inner`. Mirrors the ENV_GUARD
    /// pattern in `claude_session/coord_register.rs`.
    static ENV_GUARD: Mutex<()> = Mutex::new(());

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        ENV_GUARD.lock().unwrap_or_else(|p| p.into_inner())
    }

    #[test]
    fn sanitize_keeps_safe_chars() {
        assert_eq!(sanitize("test-runner_1"), "test-runner_1");
        assert_eq!(sanitize("abc/def"), "abc_def");
        assert_eq!(sanitize("weird name!"), "weird_name_");
    }

    /// The outbox base dir (`~/.qontinui/runner`) must scope per-instance for
    /// secondaries so spawn-test runners never race the primary on a single
    /// shared `session-outbox.jsonl`. This asserts the exact `scope_path`
    /// contract relied on by the outbox + pane-store wiring in `main.rs`.
    #[test]
    fn scope_path_isolates_outbox_dir_for_secondary() {
        let _env = env_lock();
        let base = Path::new(".qontinui").join("runner");

        // Secondary: appends `instance-<sanitized-name>`.
        std::env::set_var("QONTINUI_INSTANCE_NAME", "test-runner 7!");
        let scoped = scope_path(&base);
        assert_eq!(
            scoped,
            base.join("instance-test-runner_7_"),
            "secondary outbox dir must be instance-scoped"
        );
        let outbox = scoped.join("session-outbox.jsonl");
        assert!(outbox.to_string_lossy().contains("instance-test-runner_7_"));

        std::env::remove_var("QONTINUI_INSTANCE_NAME");
    }

    /// The PRIMARY runner (no `QONTINUI_INSTANCE_NAME`) must keep resolving to
    /// the UNSCOPED legacy path so its pre-existing pending outbox rows are
    /// never orphaned by this change.
    #[test]
    fn scope_path_is_noop_for_primary() {
        let _env = env_lock();
        std::env::remove_var("QONTINUI_INSTANCE_NAME");
        let base = Path::new(".qontinui").join("runner");
        assert_eq!(
            scope_path(&base),
            base,
            "primary must keep the legacy unscoped outbox path"
        );
    }
}

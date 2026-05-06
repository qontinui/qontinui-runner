//! Dev-only Tauri commands for seeding synthetic findings.
//!
//! Purpose: make the [`ExecutionReport`] non-empty render path exercisable
//! during manual testing without having to run a whole workflow. The seed
//! command emits synthetic findings into the frontend [`FindingsTracker`]
//! via a Tauri event so the UI can render them just like real findings.
//!
//! # Security / gating
//!
//! Every command in this module is gated on the `QONTINUI_DEV_ENDPOINTS=1`
//! environment variable. When the gate is off, commands return
//! `Err("dev endpoint disabled")` without doing any work. The
//! [`is_dev_endpoints_enabled`] command exposes the gate so the React side
//! can conditionally register its `dev:seed-finding` event listener.
//!
//! This follows the same pattern as the existing
//! [`commands::auth::get_test_auto_login`] which reads
//! `QONTINUI_TEST_AUTO_LOGIN_EMAIL` from the process env.

use serde_json::{json, Value};
use tauri::plugin::{Builder as PluginBuilder, TauriPlugin};
use tauri::{AppHandle, Emitter, Runtime};

use qontinui_runner_lib::tauri_event_payloads::DevSeedFindingPayload as SeedFindingPayload;

/// Environment variable that gates all dev-only endpoints in this module.
const DEV_ENV_VAR: &str = "QONTINUI_DEV_ENDPOINTS";

/// Returns `true` when `QONTINUI_DEV_ENDPOINTS=1` is set in the process env.
///
/// Exposed to the frontend so the React `TauriFindingsListener` can decide
/// whether to subscribe to the `dev:seed-finding` event. There is no harm
/// in exposing the flag itself — it reveals nothing beyond "is this runner
/// running in dev mode", which is already implied by other dev-mode UI.
#[tauri::command]
pub fn is_dev_endpoints_enabled() -> bool {
    is_dev_endpoints_value(std::env::var(DEV_ENV_VAR).ok().as_deref())
}

/// Pure policy: the gate is on iff the env value is exactly `"1"`. Split out
/// from `is_dev_endpoints_enabled()` so tests can exercise the predicate
/// without racing on `std::env` mutations from sibling tests in the same
/// `cargo test` process.
fn is_dev_endpoints_value(v: Option<&str>) -> bool {
    v == Some("1")
}

// `SeedFindingPayload` is the canonical wire-format struct in
// `qontinui_runner_lib::tauri_event_payloads::DevSeedFindingPayload`, which is
// registered with schemars in `schema_export.rs` so the TS bindings stay in
// sync with the Rust shape.

/// Seed one or more synthetic findings into the frontend findings tracker.
///
/// # Parameters
/// - `count`: number of synthetic findings to emit (default `1`, clamped to `1..=50`)
/// - `category`: category id (default `"code_bug"`)
/// - `status`: finding status (default `"detected"`); when `"needs_input"` the
///   action type is set to `"needs_user_input"`, otherwise `"auto_fix"`
///
/// # Errors
/// Returns `Err("dev endpoint disabled")` unless `QONTINUI_DEV_ENDPOINTS=1`.
/// Returns `Err(...)` if the Tauri event emit fails.
///
/// # Returns
/// A JSON object `{ "seeded": N, "ids": [...] }` describing the findings
/// that were emitted.
#[tauri::command]
pub async fn dev_seed_finding<R: Runtime>(
    app_handle: AppHandle<R>,
    count: Option<u32>,
    category: Option<String>,
    status: Option<String>,
) -> Result<Value, String> {
    if !is_dev_endpoints_enabled() {
        return Err("dev endpoint disabled".to_string());
    }

    let n = count.unwrap_or(1).clamp(1, 50);
    let category_id = category.unwrap_or_else(|| "code_bug".to_string());
    let status_str = status.unwrap_or_else(|| "detected".to_string());

    // Map status/actionable inference. "needs_input" status maps to the
    // needs_user_input action type so the UI routes these through the
    // user-input panel instead of auto-fix.
    let action_type = if status_str == "needs_input" {
        "needs_user_input"
    } else {
        "auto_fix"
    };
    let actionable = action_type != "informational";

    let now_ms = chrono::Utc::now().timestamp_millis();
    let mut ids = Vec::with_capacity(n as usize);

    for i in 1..=n {
        let id = format!("dev-seed-{}-{}", now_ms, i);
        let payload = SeedFindingPayload {
            id: id.clone(),
            category_id: category_id.clone(),
            severity: "medium".to_string(),
            status: status_str.clone(),
            title: format!("Synthetic finding #{}", i),
            description: "Seeded by dev_seed_finding for manual testing.".to_string(),
            detected_at: now_ms,
            action_type: action_type.to_string(),
            actionable,
            source_session_id: Some("dev-seed-session".to_string()),
        };

        app_handle
            .emit("dev:seed-finding", &payload)
            .map_err(|e| format!("failed to emit dev:seed-finding: {}", e))?;
        ids.push(id);
    }

    tracing::info!(
        count = n,
        category = %category_id,
        status = %status_str,
        "Seeded synthetic findings via dev_seed_finding"
    );

    Ok(json!({
        "seeded": n,
        "ids": ids,
    }))
}

/// Build the Tauri plugin that registers this module's command handlers.
///
/// See `commands/mod.rs` for the migration guide explaining the plugin pattern.
pub fn plugin<R: Runtime>() -> TauriPlugin<R> {
    PluginBuilder::new("qontinui_dev_findings")
        .invoke_handler(tauri::generate_handler![
            is_dev_endpoints_enabled,
            dev_seed_finding,
        ])
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Tests exercise the pure policy directly instead of mutating the process
    // env. Earlier versions of these tests called set_var/remove_var and
    // raced against each other under cargo's test-thread parallelism (saw
    // failures on Ubuntu, macOS, and locally on Windows depending on which
    // sibling test happened to interleave).

    #[test]
    fn gate_off_returns_false() {
        assert!(!is_dev_endpoints_value(None));
        assert!(!is_dev_endpoints_value(Some("")));
        assert!(!is_dev_endpoints_value(Some("0")));
        assert!(!is_dev_endpoints_value(Some("true")));
    }

    #[test]
    fn gate_on_returns_true() {
        assert!(is_dev_endpoints_value(Some("1")));
    }
}

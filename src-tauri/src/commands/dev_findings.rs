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

use serde::Serialize;
use serde_json::{json, Value};
use tauri::plugin::{Builder as PluginBuilder, TauriPlugin};
use tauri::{AppHandle, Emitter, Runtime};

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
    std::env::var(DEV_ENV_VAR).as_deref() == Ok("1")
}

/// Payload shape emitted with the `dev:seed-finding` Tauri event.
///
/// Field names use camelCase so the TS listener can spread them directly
/// into a `Finding` object without translation.
#[derive(Debug, Clone, Serialize)]
struct SeedFindingPayload {
    id: String,
    #[serde(rename = "categoryId")]
    category_id: String,
    severity: String,
    status: String,
    title: String,
    description: String,
    #[serde(rename = "detectedAt")]
    detected_at: i64,
    #[serde(rename = "actionType")]
    action_type: String,
    actionable: bool,
    #[serde(rename = "sourceSessionId", skip_serializing_if = "Option::is_none")]
    source_session_id: Option<String>,
}

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

    // These assertions all touch the same process-global env var
    // (`QONTINUI_DEV_ENDPOINTS`), so they cannot run as separate
    // `#[test]` functions — cargo's default thread pool would let them
    // race against each other and against any other env-touching test
    // in the suite. CI Windows hit exactly this race on PR #48 run
    // 25424040391: `gate_off_returns_false` saw the var as `1` because a
    // sibling test had set it concurrently. Bundling the assertions in
    // one test and clearing the var at start + finally serializes them
    // by construction.
    #[test]
    fn gate_reads_env_var() {
        std::env::remove_var(DEV_ENV_VAR);
        assert!(!is_dev_endpoints_enabled(), "gate must be off when var unset");

        std::env::set_var(DEV_ENV_VAR, "1");
        assert!(is_dev_endpoints_enabled(), "gate must be on when var=1");

        std::env::set_var(DEV_ENV_VAR, "0");
        assert!(!is_dev_endpoints_enabled(), "gate must be off when var!=1");

        std::env::remove_var(DEV_ENV_VAR);
        assert!(!is_dev_endpoints_enabled(), "gate must be off when var unset (final)");
    }
}

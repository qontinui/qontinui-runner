//! Tauri command surface for the scripted-output script emitter.
//!
//! Thin wrapper around
//! `crate::step_output::script_emitter::emit_extraction_script`. The TS
//! `TauriScriptEmitter` (Phase B) calls this via
//! `invoke("emit_extraction_script", {...})` when a handler's stdout
//! exceeds the 8 KB scripted-output threshold.

use serde::Serialize;
use tauri::plugin::{Builder as PluginBuilder, TauriPlugin};
use tauri::Runtime;

use crate::step_output::script_emitter::{self, DisabledReason, EmitError, EmitResult};

/// Structured error shape returned to the TS shim on rejection. The shim
/// maps any failure onto the truncation fallback; differentiating the
/// reason lets the base handler emit the right `scripted_output.fallback`
/// sub-event on the TS side, and lets manual-test agents see at a glance
/// which gate is closed.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EmitExtractionScriptError {
    /// Short machine token. Stable values: `cost_cap`, `token_budget`,
    /// `timeout`, `breaker_open`, `llm_error`, `invalid_response`, plus
    /// the three flavours of disabled — `disabled_env` (env kill
    /// switch), `disabled_settings` (`scripted_output.enabled = false`),
    /// `disabled_no_credentials` (Auto cascade exhausted).
    pub kind: String,
    /// Human-readable message (already safe to log).
    pub message: String,
}

impl From<EmitError> for EmitExtractionScriptError {
    fn from(err: EmitError) -> Self {
        let (kind, message) = match &err {
            EmitError::CostCap { .. } => ("cost_cap", err.to_string()),
            EmitError::TokenBudget { .. } => ("token_budget", err.to_string()),
            EmitError::Timeout { .. } => ("timeout", err.to_string()),
            EmitError::BreakerOpen => ("breaker_open", err.to_string()),
            EmitError::Disabled { reason } => (disabled_kind(reason), err.to_string()),
            EmitError::LlmError(_) => ("llm_error", err.to_string()),
            EmitError::InvalidResponse(_) => ("invalid_response", err.to_string()),
        };
        Self {
            kind: kind.to_string(),
            message,
        }
    }
}

/// Map a `DisabledReason` to the wire-level `kind` discriminator. Kept
/// separate so callers grepping for the canonical token list above find
/// the strings here too.
fn disabled_kind(reason: &DisabledReason) -> &'static str {
    match reason {
        DisabledReason::EnvKillSwitch => "disabled_env",
        DisabledReason::SettingsFlag => "disabled_settings",
        DisabledReason::NoCredentials => "disabled_no_credentials",
    }
}

/// `invoke("emit_extraction_script", {...})` handler.
///
/// Parameters are unpacked into named fields so the invoke payload is flat
/// (`{ goal, schemaHint, outputPreview, taskRunId }`) — matching the
/// convention used by every other command in the UI Bridge invoke allowlist
/// and avoiding the `{args: {args: {...}}}` double-wrap gotcha that a single
/// struct parameter would introduce.
#[tauri::command]
pub async fn emit_extraction_script(
    goal: String,
    schema_hint: serde_json::Value,
    output_preview: String,
    task_run_id: Option<String>,
) -> Result<EmitResult, EmitExtractionScriptError> {
    script_emitter::emit_extraction_script(goal, schema_hint, output_preview, task_run_id)
        .await
        .map_err(Into::into)
}

/// `invoke("emit_scripted_output_event", {...})` handler.
///
/// Called by the TS `ScriptedOutputHandler` base class (via the helper in
/// `src/lib/step-output-handlers/scripted-output-telemetry.ts`) to record
/// the three TS-originated events (`scripted_output.attempted`,
/// `scripted_output.worker_ok`, `scripted_output.bytes_avoided`) and the
/// `bad_expression` flavour of `scripted_output.fallback`. Goes through the
/// Rust emitter's FK-aware insert path — an unassigned/unknown task_run_id
/// is recorded with a NULL FK column (the raw value is preserved in
/// `metadata.task_run_id_raw`) instead of being dropped.
///
/// Parameters are unpacked into named fields so the invoke payload is flat
/// (`{ name, metadata?, taskRunId? }`) — see `emit_extraction_script` for
/// the same rationale.
#[tauri::command]
pub async fn emit_scripted_output_event(
    name: String,
    metadata: Option<serde_json::Value>,
    task_run_id: Option<String>,
) -> Result<(), String> {
    let metadata = metadata.unwrap_or_else(|| serde_json::json!({}));
    script_emitter::emit_ts_originated_event(&name, metadata, task_run_id)
}

/// Build the Tauri plugin that registers this module's command handlers.
pub fn plugin<R: Runtime>() -> TauriPlugin<R> {
    PluginBuilder::new("qontinui_script_emitter")
        .invoke_handler(tauri::generate_handler![
            emit_extraction_script,
            emit_scripted_output_event,
        ])
        .build()
}

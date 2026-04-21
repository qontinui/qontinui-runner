//! Tauri command surface for the scripted-output script emitter.
//!
//! Thin wrapper around
//! `crate::step_output::script_emitter::emit_extraction_script`. The TS
//! `TauriScriptEmitter` (Phase B) calls this via
//! `invoke("emit_extraction_script", {...})` when a handler's stdout
//! exceeds the 8 KB scripted-output threshold.

use serde::{Deserialize, Serialize};

use crate::step_output::script_emitter::{self, EmitError, EmitResult};

/// Request payload for `emit_extraction_script`.
///
/// Camel-cased because the TS caller sends camelCase. The frontend
/// invoker follows Tauri's default convention.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmitExtractionScriptArgs {
    /// Human-readable description of what the summariser wants (e.g.
    /// "find the last 10 ERROR lines").
    pub goal: String,
    /// Top-level key+type summary of the expected extracted shape. Used
    /// for cache-key stability and as an LLM hint.
    pub schema_hint: serde_json::Value,
    /// First ~2 KB of the raw stdout. Full stdout is deliberately NOT
    /// passed — the whole point of the indirection is to avoid that cost.
    pub output_preview: String,
    /// Optional task-run identifier. Drives per-run cost ceilings and
    /// telemetry correlation; may be `None` for ad-hoc / test calls.
    pub task_run_id: Option<String>,
}

/// Structured error shape returned to the TS shim on rejection. The shim
/// maps any failure onto the truncation fallback; differentiating the
/// reason lets the base handler emit the right `scripted_output.fallback`
/// sub-event on the TS side.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EmitExtractionScriptError {
    /// Short machine token: `cost_cap`, `token_budget`, `timeout`,
    /// `breaker_open`, `disabled`, `llm_error`, `invalid_response`.
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
            EmitError::Disabled => ("disabled", err.to_string()),
            EmitError::LlmError(_) => ("llm_error", err.to_string()),
            EmitError::InvalidResponse(_) => ("invalid_response", err.to_string()),
        };
        Self {
            kind: kind.to_string(),
            message,
        }
    }
}

/// `invoke("emit_extraction_script", {...})` handler.
#[tauri::command]
pub async fn emit_extraction_script(
    args: EmitExtractionScriptArgs,
) -> Result<EmitResult, EmitExtractionScriptError> {
    script_emitter::emit_extraction_script(
        args.goal,
        args.schema_hint,
        args.output_preview,
        args.task_run_id,
    )
    .await
    .map_err(Into::into)
}

/// Request payload for `emit_scripted_output_event`.
///
/// Called by the TS `ScriptedOutputHandler` base class (via the helper in
/// `src/lib/step-output-handlers/scripted-output-telemetry.ts`) to record
/// the three TS-originated events (`attempted`, `worker_ok`,
/// `bytes_avoided`) and the `bad_expression` flavour of `fallback`.
///
/// Goes through the Rust emitter's FK-aware insert path, so an
/// unassigned/unknown task_run_id is handled by recording the event with a
/// NULL FK column instead of being dropped.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmitScriptedOutputEventArgs {
    /// Event name. Must be one of `scripted_output.attempted`,
    /// `scripted_output.worker_ok`, `scripted_output.bytes_avoided`, or
    /// `scripted_output.fallback`.
    pub name: String,
    /// Arbitrary JSON metadata. Stored as-is in
    /// `activity_timeline.metadata_json`.
    #[serde(default)]
    pub metadata: serde_json::Value,
    /// Optional task run id. If set, the emitter pre-checks its existence
    /// in `task_runs` and falls back to NULL on miss (the raw value is
    /// preserved in `metadata.task_run_id_raw`).
    pub task_run_id: Option<String>,
}

/// `invoke("emit_scripted_output_event", {...})` handler.
#[tauri::command]
pub async fn emit_scripted_output_event(
    args: EmitScriptedOutputEventArgs,
) -> Result<(), String> {
    script_emitter::emit_ts_originated_event(&args.name, args.metadata, args.task_run_id)
}

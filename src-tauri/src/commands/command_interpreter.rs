//! Phase 8 — Tier-3 free-text → registry action resolver.
//!
//! The CommandBar's two synchronous tiers (slash autocomplete, declarative
//! regex patterns) cover the long tail of expected phrasings; this module
//! catches the rest by spawning the local `claude` CLI as a one-shot
//! subprocess and asking it to route a user's free-text prompt to a
//! registered action with structured-output enforcement.
//!
//! ## Architecture choice — local subprocess, not HTTP
//!
//! The plan's vet pass made this an explicit decision per
//! `feedback_no_anthropic_api`: the operator pays per Claude Code
//! subscription, not per-token. Calling `api.anthropic.com` from coord
//! or runner would be a double-bill failure mode. The mandated pattern
//! is the same one used by `agent_runtime.rs:638-670` — spawn `claude`
//! via tokio and pipe the prompt through. Each invocation re-uses the
//! operator's existing OAuth/keychain auth.
//!
//! ## CLI flags validated against `claude --help`
//!
//! - `-p / --print` — non-interactive mode
//! - `--output-format json` — wraps the response in a metadata envelope
//! - `--json-schema <schema>` — enforces structured output; the model
//!   can't return free text once this is set, eliminating the brittle
//!   "scrape JSON out of prose" parser the original plan relied on
//! - `--system-prompt <text>` — overrides the default system prompt
//! - `--disallowed-tools` — comma- or space-separated tool denylist; we
//!   block edit/exec tools entirely since this is a router, not an agent
//! - `--model haiku` — alias for the cheapest fast model; routing is
//!   pure classification + arg extraction, doesn't need Sonnet
//!
//! ## Envelope shape
//!
//! Verified empirically (`claude -p --output-format json --json-schema
//! ... "say hi"`):
//!
//! ```json
//! {
//!     "type": "result",
//!     "subtype": "success",
//!     "is_error": false,
//!     "duration_ms": 4111,
//!     "session_id": "...",
//!     "total_cost_usd": 0.0384,
//!     "structured_output": {...},  // ← our payload lives here
//!     "result": "",                // ← empty when --json-schema is set
//!     ...
//! }
//! ```
//!
//! On error (`is_error: true`), `.result` carries an error message and
//! `.structured_output` is absent.
//!
//! ## What's deferred from the plan's Phase 8 spec
//!
//! - **Long-lived subprocess + session resume** — claude `--print` is
//!   one-shot by design; reusing prompt-cache across calls requires
//!   threading the returned `session_id` back via `--resume`. The L1
//!   cache covers most repeated phrasings without it; revisit if the
//!   ~3-5s cold-start ever becomes a complaint.
//! - **Coord PG `command_interpretations` table + `coord_command_interpret`
//!   MCP tool (L2 cache)** — second repo, separate migration. Single-
//!   machine L1 cache lives in `instanceStorage` on the TS side.
//! - **Optimistic execution + Undo** — the CommandBar always shows the
//!   match as the selected suggestion; operator confirms with Enter.
//!   Undo affordance lands when destructive registry actions are added.
//! - **Telemetry to coord** — same gating as the coord cache.
//! - **Idle-shutdown + restart-on-crash** — irrelevant for the one-shot
//!   path. Revisit if we ever go long-lived.

use serde::{Deserialize, Serialize};
use tokio::process::Command;

/// Strict shape the model is constrained to emit by `--json-schema`.
///
/// `tool` is a registered action id (e.g. `"terminal.spawn-ai"`) or the
/// empty string when nothing fits. `args` is the parameter map matching
/// the action's `paramSchema`. `confidence` is the model's own [0..1]
/// estimate — the frontend gates display + execution on it.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct InterpretResult {
    pub tool: String,
    pub args: serde_json::Value,
    pub confidence: f64,
}

/// Schema enforced on the model's response. Kept narrow so the model
/// can't hallucinate extra fields and so any deserialization failure
/// surfaces as a clear runtime error.
const OUTPUT_SCHEMA: &str = r#"{
    "type": "object",
    "properties": {
        "tool": {"type": "string"},
        "args": {"type": "object"},
        "confidence": {"type": "number", "minimum": 0, "maximum": 1}
    },
    "required": ["tool", "args", "confidence"],
    "additionalProperties": false
}"#;

const SYSTEM_PROMPT: &str = "You are a slash-command router for the Qontinui Terminal page. \
The user will give you a free-text request. You will also receive a JSON catalog of available actions, \
each with: id (the stable wire id), slash (the slash-command form), label, description, and paramSchema \
(parameter shape). \
Choose the single action that best matches the user's intent and return ONLY a JSON object with: \
- tool: the action id from the catalog (or empty string \"\" when nothing fits) \
- args: an object whose keys match the chosen action's paramSchema; coerce numeric tokens to numbers; \
  omit optional fields the user did not mention \
- confidence: a real number in [0.0, 1.0]; use >= 0.95 only when the mapping is unambiguous \
Do not invent action ids that are not in the catalog. Do not include prose, code fences, or markdown. \
Return only the JSON object.";

/// Phase 8 Tier-3 entry point — invoked from the TS frontend's
/// `interpretCommand` after Tier-1/2 miss.
///
/// `text` is the operator's raw input (already stripped of any leading
/// `/` slash). `tools_json` is a JSON-serialized array of `{id, slash,
/// label, description, paramSchema}` objects projected from the live
/// registry by the caller — we don't reach into the registry from Rust
/// because the registry is a frontend-only React module.
///
/// Returns the model's structured choice or an error string. Errors:
/// spawn failure, non-zero exit, missing envelope, missing
/// `structured_output`, JSON parse failure. The frontend surfaces these
/// to the operator as "Tier-3 unavailable" and falls back to fuzzy.
#[tauri::command]
pub async fn command_interpret(
    text: String,
    tools_json: String,
) -> Result<InterpretResult, String> {
    let prompt = format!(
        "Available actions (JSON array):\n{}\n\nUser input: {}",
        tools_json, text
    );

    let output = crate::process_helpers::tokio_no_window("claude")
        .arg("-p")
        .arg("--output-format")
        .arg("json")
        .arg("--json-schema")
        .arg(OUTPUT_SCHEMA)
        .arg("--system-prompt")
        .arg(SYSTEM_PROMPT)
        // Block every edit/exec tool — this is a router, not an agent.
        // The model still has read tools available (Read, Grep, etc.)
        // but those have no useful effect on a one-shot routing call.
        .arg("--disallowed-tools")
        .arg("Edit,Write,Bash,WebFetch,WebSearch,NotebookEdit,Task,Agent")
        .arg("--model")
        .arg("haiku")
        .arg(&prompt)
        .output()
        .await
        .map_err(|e| format!("claude spawn failed: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "claude exited {} — stderr: {}",
            output.status, stderr
        ));
    }

    let envelope: serde_json::Value =
        serde_json::from_slice(&output.stdout).map_err(|e| format!("envelope JSON parse: {e}"))?;

    if envelope
        .get("is_error")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        let result_str = envelope
            .get("result")
            .and_then(|v| v.as_str())
            .unwrap_or("(no error message)");
        return Err(format!("claude returned is_error=true: {result_str}"));
    }

    // `--json-schema` mode routes the structured payload to
    // `.structured_output` and leaves `.result` empty. If a future CLI
    // version changes this convention, the parser below will surface a
    // clear error rather than silently returning garbage.
    let structured = envelope
        .get("structured_output")
        .ok_or_else(|| "envelope missing .structured_output".to_string())?;

    let parsed: InterpretResult = serde_json::from_value(structured.clone())
        .map_err(|e| format!("structured_output parse: {e} — raw: {structured}"))?;

    Ok(parsed)
}

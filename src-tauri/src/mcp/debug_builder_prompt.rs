//! Debug endpoint: assembled Builder recognition prompt (no LLM).
//!
//! Phase B1 observability: the wrapper manifest (built by
//! `workflow_generation::wrapper_manifest::build_manifest`) is unit-tested
//! and the formatter is unit-tested, but until this endpoint existed there
//! was no way to confirm the manifest actually appears in a Builder's
//! assembled recognition prompt during a real generation run without
//! actually paying for an LLM round trip.
//!
//! This module exposes one route:
//!
//! ```text
//! POST /debug/builder-prompt
//! Body: {
//!   "description": string,
//!   "spec_brief": object?,   // optional; embedded as compact JSON in the inline context
//!   "category": string?
//! }
//! Returns: {
//!   "prompt": string,                  // the full assembled Builder prompt
//!   "wrapper_manifest_present": bool,  // prompt contains "## Registered Wrapper Apps"
//!   "wrapper_manifest_length": int,    // raw manifest string length
//!   "wrapper_count": int               // number of "- **wrapperId:" bullets in the manifest
//! }
//! ```
//!
//! ## Design choice
//!
//! Two paths were considered for assembling the prompt:
//!
//! 1. **Reuse `meta_workflow::build_builder_prompt` directly** — the actual
//!    function the Builder runs through. Required making it `pub` (it was
//!    `fn`), but the blast radius is minimal: the only existing caller is
//!    `build_meta_workflow_template` in the same module. This option gives
//!    the highest-fidelity observation: byte-for-byte the prompt the Builder
//!    would see, including all surrounding sections (project context,
//!    schema, etc.).
//!
//! 2. Add a new `pub async fn debug_assemble_recognition_prompt` that
//!    composes JUST the recognition-prompt + manifest portion. Lower
//!    visibility surface but a smaller observation — would not catch
//!    formatting drift in `build_builder_prompt` itself.
//!
//! We chose (1) because the cost (one `fn` → `pub fn` line) is trivial and
//! the observation is strictly more useful. The visibility change is
//! documented at the call site too.
//!
//! The endpoint does NOT invoke any LLM and has no side effects beyond
//! reading the wrapper registry, so it is always-on rather than
//! `#[cfg(debug_assertions)]`-gated. This matches the convention of the
//! existing `/debug/app/errors` route in `mcp/misc.rs`.

use std::sync::Arc;

use axum::{extract::State, response::Json};
use serde::{Deserialize, Serialize};

use crate::mcp::types::{runner_api_port, ApiResponse, ApiState};
use crate::workflow_generation::schema_context::build_schema_context_for_description;

#[derive(Debug, Deserialize)]
pub struct DebugBuilderPromptRequest {
    /// Free-form task description fed to the Builder.
    pub description: String,
    /// Optional spec generation brief. When present, it is embedded as
    /// compact JSON inside a `<context name="Spec Generation Brief (JSON)">`
    /// block — that literal substring is what triggers the recognition
    /// prompt branch (and thus the wrapper manifest injection) in
    /// `build_builder_prompt`.
    #[serde(default)]
    pub spec_brief: Option<serde_json::Value>,
    /// Optional workflow category hint, propagated into the assembled prompt.
    #[serde(default)]
    pub category: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct DebugBuilderPromptResponse {
    /// The full assembled Builder prompt — what the Builder LLM would see.
    pub prompt: String,
    /// Whether the prompt contains the wrapper manifest section header.
    pub wrapper_manifest_present: bool,
    /// Length (in bytes / chars; ASCII so equal) of the manifest section
    /// that was injected into the prompt. 0 when no wrapper apps are live.
    pub wrapper_manifest_length: usize,
    /// Number of `- **wrapperId:` bullets in the manifest.
    pub wrapper_count: usize,
}

/// POST /debug/builder-prompt — assemble the Builder recognition prompt
/// without invoking the LLM, so callers can confirm the wrapper manifest
/// appears as expected.
pub async fn debug_builder_prompt_handler(
    State(state): State<Arc<ApiState>>,
    Json(req): Json<DebugBuilderPromptRequest>,
) -> Json<ApiResponse<DebugBuilderPromptResponse>> {
    // Build the live wrapper manifest. This goes through the same code
    // path the Builder uses (no separate fetcher), so the bytes seen here
    // are bytes-for-bytes what the Builder would see when generating a
    // workflow from the same brief at the same instant.
    let port = runner_api_port(&state.app_state);
    let manifest =
        crate::workflow_generation::wrapper_manifest::build_manifest(&state.app_state, port).await;

    let manifest_len = manifest.len();
    let wrapper_count = count_wrapper_bullets(&manifest);

    // Build the inline context — the trigger string is the literal
    // "Spec Generation Brief" substring (see `build_builder_prompt` in
    // meta_workflow.rs around the `resolved_contexts.contains(...)` check).
    // Embed the spec_brief (or an empty object) as compact JSON inside the
    // canonical wrapper block so production parsing rules stay consistent.
    let brief_json = match &req.spec_brief {
        Some(v) => serde_json::to_string(v).unwrap_or_else(|_| "{}".to_string()),
        None => "{}".to_string(),
    };
    let inline_context = format!(
        "<context name=\"Spec Generation Brief (JSON)\">\n{}\n</context>",
        brief_json
    );

    // Build a representative schema context using the production helper.
    // We use the description-filtered variant to keep the prompt size
    // reasonable; the manifest section we care about is appended later
    // and unaffected by the schema choice.
    let schema_context = build_schema_context_for_description(&req.description);

    let category = req.category.as_deref();

    // Reuse the production builder. See module docstring for why we
    // chose this path over a custom assembler.
    //
    // `build_builder_prompt` is a sync function that internally calls
    // `tokio::runtime::Handle::current().block_on(...)` to fetch the
    // wrapper manifest (mirrors the production codepaths in
    // `meta_workflow::build_meta_workflow_template` and
    // `generator::run_builder_agent`, both of which are sync). Calling
    // `block_on` from inside an async tokio task panics, so we wrap
    // the call in `spawn_blocking` exactly like the production HTTP
    // path does.
    let app_state_clone = state.app_state.clone();
    let description = req.description.clone();
    let category_owned = category.map(str::to_string);
    let prompt = match tokio::task::spawn_blocking(move || {
        crate::workflow_generation::meta_workflow::build_builder_prompt(
            &description,
            &schema_context,
            &inline_context,
            None,
            category_owned.as_deref(),
            None,
            "",
            Some(&app_state_clone),
        )
    })
    .await
    {
        Ok(p) => p,
        Err(e) => {
            return Json(ApiResponse::error(format!(
                "build_builder_prompt task failed: {e}"
            )))
        }
    };

    let wrapper_manifest_present = prompt.contains("## Registered Wrapper Apps");

    Json(ApiResponse::success(DebugBuilderPromptResponse {
        prompt,
        wrapper_manifest_present,
        wrapper_manifest_length: manifest_len,
        wrapper_count,
    }))
}

/// Count the number of wrapper bullets in the manifest. Each wrapper is
/// emitted as a line starting with `- **wrapperId:` (see
/// `wrapper_manifest::format_manifest`).
fn count_wrapper_bullets(manifest: &str) -> usize {
    manifest
        .lines()
        .filter(|l| l.trim_start().starts_with("- **wrapperId:"))
        .count()
}

pub fn routes() -> axum::Router<Arc<ApiState>> {
    use axum::routing::post;
    axum::Router::new().route("/debug/builder-prompt", post(debug_builder_prompt_handler))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn count_wrapper_bullets_zero_for_empty() {
        assert_eq!(count_wrapper_bullets(""), 0);
    }

    #[test]
    fn count_wrapper_bullets_counts_each_wrapper_line() {
        let manifest = "## Registered Wrapper Apps\n\
             \n\
             Available wrappers and actions:\n\
             \n\
             - **wrapperId: `alpha`** (\"Alpha\")\n\
               - `do-thing` — params: `{}`\n\
             - **wrapperId: `beta`** (\"Beta\")\n\
               - `do-other` — params: `{}`\n";
        assert_eq!(count_wrapper_bullets(manifest), 2);
    }

    #[test]
    fn count_wrapper_bullets_ignores_action_bullets() {
        // Action bullets start with backtick after the dash; only
        // wrapperId bullets should match.
        let manifest = "- **wrapperId: `solo`** (\"Solo\")\n\
                        - `act-1` — params: `{}`\n\
                        - `act-2` — params: `{}`\n";
        assert_eq!(count_wrapper_bullets(manifest), 1);
    }
}

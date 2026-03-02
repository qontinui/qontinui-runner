//! Pre-Generation Investigation Step
//!
//! Analyzes the codebase in context of the user's intent before workflow
//! generation. Produces an enriched, technically-grounded description that
//! replaces the original when passed to the Builder Agent.
//!
//! This step bridges the gap between a user's high-level intent (e.g.,
//! "improve the execution status widget") and the specific technical context
//! needed by the Builder (e.g., which files contain the widget, what data
//! sources exist, which event emitters are actually called).

use crate::ai_provider::AiResponse;
use crate::ai_router::TaskContext;
use crate::doctor::DoctorHandle;
use std::time::Instant;
use tracing::{debug, info, warn};

/// Result of the investigation phase.
#[derive(Debug, Clone)]
pub struct InvestigationResult {
    /// The enriched description produced by the investigation AI.
    pub enriched_description: String,
    /// Whether the investigation succeeded.
    pub success: bool,
    /// How long the investigation took in milliseconds.
    pub duration_ms: u64,
}

/// Run the AI investigation step to analyze the codebase in context of the
/// user's prompt and discovery data.
///
/// Returns an `InvestigationResult` with an enriched description on success,
/// or with `success: false` and the original description on failure.
pub fn run_investigation(
    original_description: &str,
    discovery_context: &str,
    resolved_contexts: &str,
    doctor_handle: Option<&DoctorHandle>,
    model_override: Option<&str>,
    provider_override: Option<&str>,
) -> InvestigationResult {
    let start = Instant::now();

    let prompt =
        build_investigation_prompt(original_description, discovery_context, resolved_contexts);
    let task_context = TaskContext::from_prompt(&prompt);
    let ai_result: AiResponse = crate::ai_provider::run_prompt_with_model_override(
        &prompt,
        &task_context,
        doctor_handle,
        model_override,
        provider_override,
        None,
        None,
        None,
        None,
    );

    let duration_ms = start.elapsed().as_millis() as u64;

    if !ai_result.success {
        warn!(
            "Investigation step failed: {}",
            ai_result.error.as_deref().unwrap_or("unknown error")
        );
        return InvestigationResult {
            enriched_description: original_description.to_string(),
            success: false,
            duration_ms,
        };
    }

    let output = ai_result.output.trim().to_string();

    // Sanity check: the output should be substantially non-empty and should
    // not be JSON (we want plain text, not a workflow).
    if output.is_empty() || output.len() < 20 {
        warn!(
            "Investigation produced too-short output ({} chars), falling back",
            output.len()
        );
        return InvestigationResult {
            enriched_description: original_description.to_string(),
            success: false,
            duration_ms,
        };
    }

    if output.starts_with('{') || output.starts_with('[') {
        warn!("Investigation produced JSON instead of plain text, falling back");
        return InvestigationResult {
            enriched_description: original_description.to_string(),
            success: false,
            duration_ms,
        };
    }

    info!(
        "Investigation completed in {}ms, enriched description: {} chars (original: {} chars)",
        duration_ms,
        output.len(),
        original_description.len()
    );
    debug!(
        "Enriched description preview: {}",
        &output[..output.len().min(200)]
    );

    InvestigationResult {
        enriched_description: output,
        success: true,
        duration_ms,
    }
}

/// Build the prompt for the investigation AI.
fn build_investigation_prompt(
    original_description: &str,
    discovery_context: &str,
    resolved_contexts: &str,
) -> String {
    let mut prompt = format!(
        r#"You are a codebase investigation AI. Your job is to analyze a user's task description in context of their project and produce an enriched, technically-grounded version of that description.

## User's Original Task Description

{original_description}

## Project Discovery Context

The following information was gathered by scanning the project:

{discovery_context}
"#,
        original_description = original_description,
        discovery_context = if discovery_context.is_empty() {
            "(No discovery data available)"
        } else {
            discovery_context
        },
    );

    if !resolved_contexts.is_empty() {
        prompt.push_str(&format!(
            "\n## Additional Context\n\n{}\n",
            resolved_contexts
        ));
    }

    prompt.push_str(
        r#"
## Your Task

Analyze the user's intent in context of the project structure and produce an **enriched task description** that will be used to generate an automation workflow. Your enriched description should:

1. **Preserve the original intent** — do not change what the user wants to accomplish
2. **Identify relevant components** — name specific files, directories, modules, and patterns from the discovery data that are relevant to the task
3. **Note technical context** — mention frameworks, build tools, test runners, and conventions visible in the project structure
4. **Flag potential issues** — if you notice dead code paths, missing implementations, broken data flows, or gaps that the workflow should address, mention them
5. **Specify concrete targets** — replace vague references ("the widget", "the API") with specific file paths and component names where possible
6. **Mention verification approaches** — suggest what should be checked to verify the work is done correctly (e.g., specific test commands, URLs to check, files that should change)

## Output Format

Output ONLY the enriched task description as plain text. Do NOT output:
- JSON
- Markdown code blocks
- A workflow specification
- Explanations of what you did
- Prefixes like "Enriched description:" or "Here is the enriched version:"

Just output the enriched description directly. It should read as a more detailed, technically-specific version of the user's original request that a workflow generation AI can use to produce a precise automation plan.
"#,
    );

    prompt
}

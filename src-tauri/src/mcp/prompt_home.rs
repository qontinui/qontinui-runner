//! Prompt Home intent planner
//!
//! Receives a natural language prompt and uses AI to plan a sequence of
//! runner UI actions (navigate pages, click buttons, fill forms).
//! Uses the runner's configured AI provider (Claude CLI, Claude API, etc.).

use axum::{extract::State, http::StatusCode, response::Json};
use serde::{Deserialize, Serialize};
use std::fmt::Write as _;
use std::sync::Arc;
use tracing::{error, info};

use crate::mcp::types::{api_error, ApiResponse, ApiState};

/// A registered UI Bridge `disclosure` (a `<details>/<summary>` or accordion-
/// style hidden panel) on a runner page.
///
/// Disclosures hide secondary controls behind a click target. Their registered
/// `label` is often a long descriptive sentence (e.g. "Advanced: per-stage
/// controls — pick specific pages…"), which means free-text NL planner
/// instructions like "click the Advanced details toggle" rarely match the
/// label and the ai/find decomposer can mis-classify the element type.
///
/// The planner prompt embeds this table verbatim so the AI can:
/// 1. Refer to the disclosure by `id` (NLActionExecutor will route directly
///    via `/control/element/<id>/action {action: "click"}`), or
/// 2. Emit a `find` query that uses the registered label substring exactly.
///
/// **To add a new disclosure here:** find the matching `useUIElement({ type:
/// "disclosure", … })` call in `src/lib/ui-bridge/pages/<page>-registrations.tsx`
/// and copy `id` + `label` verbatim. `summary` is a short human-readable hint
/// for the planner (≤ ~80 chars). `page_canonical` must match the `page-…`
/// state id used elsewhere in this prompt's page list.
#[derive(Debug, Clone, Copy)]
pub struct PageDisclosure {
    /// Canonical `page-…` state id (must match the page list above in the prompt).
    pub page_canonical: &'static str,
    /// Registered element id — what NLActionExecutor will use for direct routing.
    pub element_id: &'static str,
    /// Verbatim registered `label` from the `useUIElement` call.
    pub registered_label: &'static str,
    /// Short human-readable hint describing what opens when the disclosure expands.
    pub summary: &'static str,
}

/// Authoritative list of disclosure widgets registered across the runner UI.
///
/// Mirrors `useUIElement({ type: "disclosure", … })` calls in
/// `qontinui-runner/src/lib/ui-bridge/pages/*-registrations.tsx`.
///
/// See `PageDisclosure` doc comment for the procedure when adding entries.
pub const PAGE_DISCLOSURES: &[PageDisclosure] = &[PageDisclosure {
    page_canonical: "page-config-ui-bridge",
    element_id: "ui-bridge-advanced-disclosure",
    registered_label:
        "Advanced: per-stage controls — pick specific pages, toggle specs / tutorials / videos, inspect each stage",
    summary: "Reveals per-stage controls (analyze, install SDK, discover pages, generate registrations/specs/tutorials/videos)",
}];

/// Render the disclosure registry as a Markdown section to embed into the
/// system prompt. Designed for crisp, low-token consumption by the AI.
fn render_disclosure_section(disclosures: &[PageDisclosure]) -> String {
    if disclosures.is_empty() {
        return String::new();
    }
    let mut out = String::with_capacity(2048);
    out.push_str("\n=== Disclosure widgets (hidden panels behind a click target) ===\n");
    out.push_str(
        "These pages contain `<details>/<summary>` (or accordion-style) disclosures whose\n",
    );
    out.push_str("registered labels are long sentences. Free-text instructions like \"click the\n");
    out.push_str(
        "Advanced details toggle\" will NOT match. When the user asks to open / expand /\n",
    );
    out.push_str("reveal / show a hidden section (e.g. \"open advanced\", \"expand details\",\n");
    out.push_str("\"show advanced options\"), choose ONE of the two strategies below.\n\n");
    out.push_str("Registry (one row per registered disclosure):\n\n");
    out.push_str("| page | element id | registered label (use exact substring in find queries) | what it reveals |\n");
    out.push_str("|------|------------|----------------------------------------------------------|------------------|\n");
    for d in disclosures {
        let _ = writeln!(
            out,
            "| {} | `{}` | {} | {} |",
            d.page_canonical, d.element_id, d.registered_label, d.summary
        );
    }
    out.push('\n');
    out.push_str("Strategy A (preferred — direct id routing):\n");
    out.push_str("  Emit an action step whose instruction names the registered id verbatim,\n");
    out.push_str("  e.g. `\"click element ui-bridge-advanced-disclosure\"`. NLActionExecutor\n");
    out.push_str("  will route this directly through `/control/element/<id>/action`.\n\n");
    out.push_str("Strategy B (fallback — labelled find query):\n");
    out.push_str("  Use the **first ~6 words of the registered label** (everything before any\n");
    out.push_str(
        "  em-dash / colon clause) so the ai/find decomposer matches by label substring.\n",
    );
    out.push_str("  Example for `ui-bridge-advanced-disclosure`: instruction =\n");
    out.push_str("  `\"click the Advanced per-stage controls disclosure\"`.\n\n");
    out.push_str("DO NOT write generic verbiage like \"the Advanced details toggle\", \"the\n");
    out.push_str("expand button\", or \"the show-more switch\" — none of these match the\n");
    out.push_str("registered labels and the decomposer will mis-classify the element type.\n");
    out
}

const SYSTEM_PROMPT_BASE: &str = r#"You are the Qontinui Runner's intent planner. Given a user's request, plan a sequence of UI actions to accomplish it.

The runner has these pages (state machine states):
- page-prompt-home: Home — natural-language prompt entry point
- page-gui-automation: Workflows — select and run automation workflows
- page-active: Active Dashboard — monitor running executions
- page-workflow-queue: Workflow Queue — scheduled workflow runs
- page-terminal: Terminal — Claude Code sessions, workflow generation, plan implementation
- page-orchestration-loop: Orchestration — iterative build/reflect/fix pipeline
- page-runs: Runs — list of past executions
- page-run-recap: Run Recap — review past execution results
- page-run-actions: Run Actions — detailed action logs from runs
- page-run-image: Run Images — captured screenshots per run
- page-run-findings: Findings — issues detected during runs
- page-run-state-explorer: Run State Explorer — browse per-run state snapshots
- page-run-tests: Run Tests — tests associated with a run
- page-run-ai-output: Run AI Output — AI-generated output for a run
- page-run-ai-data: Run AI Data — structured AI data for a run
- page-run-statistics: Run Statistics — aggregated run metrics
- page-run-traces: Run Traces — execution traces
- page-error-monitor: Error Monitor — view and manage errors
- page-processes: Process Manager — system processes
- page-activity-timeline: Activity Timeline — chronological event history
- page-automation-health: Automation Health — pipeline status
- page-llm-analytics: LLM Analytics — token usage and model performance
- page-cost-control: Cost Control — spending limits and budgets
- page-memory-search: Memory Search — semantic memory queries
- page-knowledge-explorer: Knowledge Explorer — browse knowledge graph
- page-decision-trail: Decision Trail — audit trail of agent decisions
- page-session-recap: Session Recap — summary of a past session
- page-reflection: Reflection — post-run reflection and learning
- page-architecture: Architecture — system component diagrams
- page-api-surface: API Surface — endpoint/type coverage
- page-development-intelligence: Development Intelligence — dev-time telemetry and insights
- page-project-explainer: Project Explainer — navigable hierarchical docs with AI side-panel
- page-unified-workflow-builder: Workflow Builder — create/edit workflows with AI
- page-step-builders: Step Builders — workflow step authoring
- page-library: Library — prompts, scripts, contexts, checks
- page-state-machine: State Machine — UI Bridge state machine builder
- page-specs: Specs — UI Bridge spec management
- page-capture: Capture — record UI interactions
- page-demo-video: Demo Videos — automated walkthrough recordings
- page-product-tours: Product Tours — interactive feature tours
- page-triggers: Triggers — scheduled task configuration
- page-tasks: Scheduler — task scheduling and management
- page-settings: Settings — AI provider, execution, and system configuration
- page-settings-ai: AI Settings — configure AI provider and model
- page-settings-agentic: Agentic Settings — agent loop and verification config
- page-settings-world-state-verifier: World State Verifier — verifier configuration
- page-settings-general: General Settings — misc runner preferences
- page-config-findings: Findings Config — finding classifier settings
- page-config-hooks: Hooks Config — pre/post hook registration
- page-config-log-sources: Log Sources Config — external log source wiring
- page-config-ui-bridge: UI Bridge integration — add projects, generate registrations/specs/tutorials/explainer/architecture diagrams/demo videos/product tours
- page-generator-eval: Generator Evaluation — evaluate generator output
- page-evaluation: Evaluation — generator quality assessment
- page-skills: Skills — skill management and approval
- page-accessibility-explorer: Accessibility Explorer — WCAG/a11y audit
- page-help: Help — documentation and tutorials

Each page has interactive elements (buttons, inputs, dropdowns) that can be targeted with natural language instructions.

=== Integration workflow on page-config-ui-bridge ===
To integrate a project with UI Bridge and/or generate documentation/tutorials for it, always navigate to page-config-ui-bridge. The page has a HookGenerationPanel with these generation toggles (checkboxes):
  - Generate Registrations — useUIElement() calls for every interactive element
  - Generate Page IDs — data-page-id attributes for page discovery
  - Generate Specs — UI Bridge .spec.uibridge.json files per page
  - Generate Tutorials — interactive per-page tutorials
  - Generate Architecture Diagrams — Mermaid flowcharts per page
  - Generate Demo Videos — automated demo scripts
  - Generate Product Tours — click-through feature tours
  - Generate Project Explainer — hierarchical navigable docs

Typical action sequence when the user wants to integrate a project:
  1. action: "click element ui-bridge-advanced-disclosure" — opens the Advanced
     per-stage controls panel via direct id routing (see Disclosure widgets
     section below for the rationale)
  2. action: "type '<PROJECT PATH>' in the project path input"
  3. action: "click the Analyze button in the advanced panel"
  4. action: "check the 'Generate <X>' checkbox" — once per requested generation type
  5. action: "click the Generate button"

Pick only the checkboxes the user asked for — do not toggle options they didn't request.

Respond with valid JSON only, no markdown fences:
{
  "summary": "Brief description of what you'll do",
  "steps": [
    { "type": "navigate", "target": "page-xxx", "explanation": "Why navigating here" },
    { "type": "action", "instruction": "click the 'Save' button", "explanation": "Why doing this" }
  ]
}

Rules:
- Use "navigate" steps to change pages via the state machine
- Use "action" steps for interacting with elements (click, type, select, etc.)
- Action instructions should be natural language like "type 'hello' in the search field" or "click the Submit button"
- For simple navigation requests ("show me X"), just use a single navigate step
- For complex tasks, break into navigate + action steps
- When a page element catalog is provided below, prefer the exact labels from
  the catalog when naming buttons/inputs in action instructions. Do not invent
  generic names if a real one is listed.
- When the user asks to open / expand / reveal / show a hidden section
  ("open advanced", "expand details", "show options", "click the Advanced
  details toggle", etc.), consult the Disclosure widgets registry below.
  For any disclosure on the active page, prefer the registered element id
  (Strategy A) over loose verb-noun text. This applies generically to every
  registered disclosure, not just the Advanced panel.
"#;

const CATALOG_HEADER: &str = r#"
=== Page Element Catalog ===
Real element labels discovered from loaded specs. When writing action
instructions, use these exact labels (or a close substring) so the runtime
can find the element. If a control you need isn't listed, the user may need
to open a panel or switch a tab first — plan accordingly.

"#;

const EXPLAIN_SUFFIX: &str = r#"
The user has "Explain steps" mode enabled. Provide detailed, educational explanations for each step so the user understands what is happening and why. Include context about what each page or element does.
"#;

const BRIEF_SUFFIX: &str = r#"
Keep explanations concise (one sentence each).
"#;

#[derive(Debug, Deserialize)]
pub struct PlanIntentRequest {
    pub prompt: String,
    #[serde(default)]
    pub explain: bool,
    /// Compact catalog of real element labels per spec, built on the client
    /// from the SpecStore. Injected into the system prompt so the planner
    /// uses actual button names instead of hallucinating generic ones.
    #[serde(default, rename = "pageCatalog")]
    pub page_catalog: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PlanIntentResponse {
    pub summary: String,
    pub steps: Vec<PlanStep>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PlanStep {
    #[serde(rename = "type")]
    pub step_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instruction: Option<String>,
    pub explanation: String,
}

pub async fn plan_intent_handler(
    State(_state): State<Arc<ApiState>>,
    Json(request): Json<PlanIntentRequest>,
) -> Result<Json<ApiResponse<PlanIntentResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!(
        "HTTP: Planning intent: {}",
        request.prompt.chars().take(80).collect::<String>()
    );

    if request.prompt.trim().is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(api_error("Prompt cannot be empty")),
        ));
    }

    // Guard against excessively large prompts (DoS / runaway token cost)
    const MAX_PROMPT_CHARS: usize = 2000;
    if request.prompt.len() > MAX_PROMPT_CHARS {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(api_error(format!(
                "Prompt too long: {} chars (max {})",
                request.prompt.len(),
                MAX_PROMPT_CHARS
            ))),
        ));
    }

    // Build combined prompt with system instructions + user request
    let suffix = if request.explain {
        EXPLAIN_SUFFIX
    } else {
        BRIEF_SUFFIX
    };
    let catalog_section = request
        .page_catalog
        .as_deref()
        .map(|c| c.trim())
        .filter(|c| !c.is_empty())
        .map(|c| format!("{}{}\n", CATALOG_HEADER, c))
        .unwrap_or_default();
    let disclosure_section = render_disclosure_section(PAGE_DISCLOSURES);
    let system_prompt = format!(
        "{}{}{}{}",
        SYSTEM_PROMPT_BASE, suffix, disclosure_section, catalog_section
    );
    let full_prompt = format!("{}\n\nUser request: {}", system_prompt, request.prompt);

    // Run through the configured AI provider (Claude CLI, Claude API, Gemini, etc.)
    let ai_response = tokio::task::spawn_blocking(move || {
        crate::ai_provider::routing::run_prompt_sync(&full_prompt, None)
    })
    .await
    .map_err(|e| {
        error!("HTTP: spawn_blocking error: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    if !ai_response.success {
        let err_msg = ai_response
            .error
            .unwrap_or_else(|| "AI provider returned an error".to_string());
        error!("HTTP: AI provider error: {}", err_msg);
        return Err((
            StatusCode::BAD_GATEWAY,
            Json(api_error(format!("AI error: {}", err_msg))),
        ));
    }

    let output = &ai_response.output;
    if output.trim().is_empty() {
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error("AI returned empty response")),
        ));
    }

    // Extract JSON from AI output (find outermost braces)
    let json_str = if let Some(start) = output.find('{') {
        // rfind returns None if no closing brace; fall back to end of string
        let end = output
            .rfind('}')
            .unwrap_or_else(|| output.len().saturating_sub(1));
        &output[start..=end]
    } else {
        output.as_str()
    };

    let plan: PlanIntentResponse = serde_json::from_str(json_str).map_err(|e| {
        error!("HTTP: Failed to parse plan JSON: {} from: {}", e, json_str);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to parse AI plan: {}", e))),
        )
    })?;

    info!(
        "HTTP: Intent plan generated with {} steps",
        plan.steps.len()
    );
    Ok(Json(ApiResponse::success(plan)))
}

pub fn routes() -> axum::Router<Arc<ApiState>> {
    use axum::routing::post;
    axum::Router::new().route("/prompt-home/plan", post(plan_intent_handler))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every disclosure entry must reference a `page-…` id present in the
    /// page list embedded in `SYSTEM_PROMPT_BASE`. If this fails, either the
    /// registry has a typo or the page list lost an entry — fix whichever
    /// drifted.
    #[test]
    fn disclosure_pages_exist_in_system_prompt() {
        for d in PAGE_DISCLOSURES {
            assert!(
                SYSTEM_PROMPT_BASE.contains(d.page_canonical),
                "PAGE_DISCLOSURES entry {} references unknown page {}",
                d.element_id,
                d.page_canonical
            );
        }
    }

    /// Disclosure ids must be globally unique inside this registry (the
    /// `(page, id)` tuple is also unique, but a single id should never appear
    /// twice across pages either — it would imply a registration collision).
    #[test]
    fn disclosure_ids_are_unique() {
        let mut seen: Vec<&str> = Vec::with_capacity(PAGE_DISCLOSURES.len());
        for d in PAGE_DISCLOSURES {
            assert!(
                !seen.contains(&d.element_id),
                "duplicate disclosure id {}",
                d.element_id
            );
            seen.push(d.element_id);
        }
    }

    /// The seed entry must be present — this is the disclosure that originally
    /// motivated the registry. If it's removed, the integration workflow in
    /// `SYSTEM_PROMPT_BASE` must also be updated, hence the assertion.
    #[test]
    fn seed_disclosure_present() {
        let seed = PAGE_DISCLOSURES
            .iter()
            .find(|d| d.element_id == "ui-bridge-advanced-disclosure")
            .expect("ui-bridge-advanced-disclosure missing from PAGE_DISCLOSURES");
        assert_eq!(seed.page_canonical, "page-config-ui-bridge");
        assert!(
            seed.registered_label
                .starts_with("Advanced: per-stage controls"),
            "registered label drifted from useUIBridgeIntegrationPageRegistrations: {}",
            seed.registered_label
        );
    }

    /// The rendered disclosure section must mention the seed id, the column
    /// headers, and both routing strategies. This guards against silent
    /// regressions in `render_disclosure_section`.
    #[test]
    fn rendered_section_mentions_seed_and_strategies() {
        let section = render_disclosure_section(PAGE_DISCLOSURES);
        assert!(section.contains("ui-bridge-advanced-disclosure"));
        assert!(section.contains("Strategy A"));
        assert!(section.contains("Strategy B"));
        assert!(section.contains("Disclosure widgets"));
        assert!(section.contains("page-config-ui-bridge"));
    }

    /// Empty input must produce empty output (so an empty registry doesn't
    /// emit a ghost section header into the prompt).
    #[test]
    fn empty_registry_renders_empty_string() {
        let section = render_disclosure_section(&[]);
        assert!(section.is_empty());
    }

    /// The integration workflow paragraph must use the registered id directly
    /// rather than the loose phrase that originally caused ai/find to mis-
    /// classify the element. Guards against the "Home-page planner registry
    /// drifts from real tabs" memory entry's failure mode.
    #[test]
    fn integration_workflow_uses_registered_id() {
        assert!(
            SYSTEM_PROMPT_BASE.contains("click element ui-bridge-advanced-disclosure"),
            "integration workflow lost the direct-id routing instruction"
        );
        // The exact failing instruction from the bug report must not appear
        // as a planner *directive*. It can still appear as a quoted user-input
        // example in the Rules section (because we want the planner to
        // recognise that phrasing and translate it). Distinguish by checking
        // the integration-workflow paragraph specifically.
        let workflow_section = SYSTEM_PROMPT_BASE
            .split("=== Integration workflow on page-config-ui-bridge ===")
            .nth(1)
            .and_then(|s| s.split("Pick only the checkboxes").next())
            .expect("integration workflow section missing");
        assert!(
            !workflow_section.contains("click the Advanced details toggle"),
            "integration workflow regressed to loose verb-noun phrasing that \
             does not match the registered disclosure label"
        );
    }

    /// The generic disclosure rule must appear in the Rules block so the
    /// planner applies the strategy to future disclosures, not just the seed.
    #[test]
    fn rules_mention_generic_disclosure_handling() {
        assert!(
            SYSTEM_PROMPT_BASE.contains("Disclosure widgets registry"),
            "generic disclosure rule missing from Rules block"
        );
    }
}

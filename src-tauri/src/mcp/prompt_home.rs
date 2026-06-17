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

/// A disclosure row that can be rendered into the prompt's registry table.
///
/// Both the static [`PageDisclosure`] (runner fallback) and the caller-supplied
/// owned [`DisclosureDef`] (e.g. the web co-pilot's web pages) implement this so
/// [`render_disclosure_section`] can render either source uniformly.
trait DisclosureRow {
    fn page_canonical(&self) -> &str;
    fn element_id(&self) -> &str;
    fn registered_label(&self) -> &str;
    fn summary(&self) -> &str;
}

impl DisclosureRow for PageDisclosure {
    fn page_canonical(&self) -> &str {
        self.page_canonical
    }
    fn element_id(&self) -> &str {
        self.element_id
    }
    fn registered_label(&self) -> &str {
        self.registered_label
    }
    fn summary(&self) -> &str {
        self.summary
    }
}

impl DisclosureRow for DisclosureDef {
    fn page_canonical(&self) -> &str {
        &self.page_canonical
    }
    fn element_id(&self) -> &str {
        &self.element_id
    }
    fn registered_label(&self) -> &str {
        &self.registered_label
    }
    fn summary(&self) -> &str {
        &self.summary
    }
}

/// Render the disclosure registry as a Markdown section to embed into the
/// system prompt. Designed for crisp, low-token consumption by the AI.
///
/// Generic over [`DisclosureRow`] so it renders either the static runner
/// [`PAGE_DISCLOSURES`] (fallback) or a caller-supplied [`DisclosureDef`] list.
fn render_disclosure_section<D: DisclosureRow>(disclosures: &[D]) -> String {
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
            d.page_canonical(),
            d.element_id(),
            d.registered_label(),
            d.summary()
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

/// Role line + page-list header. Always emitted first. Caller-supplied pages or
/// the runner fallback page list are appended after this.
const SYSTEM_PROMPT_PREFIX: &str = r#"You are the Qontinui Runner's intent planner. Given a user's request, plan a sequence of UI actions to accomplish it.

The runner has these pages (state machine states):
"#;

/// Runner's own hardcoded page list. Used ONLY as the fallback when a caller
/// does not supply its own `pages`. The leading `- page-prompt-home` and the
/// trailing "Each page has interactive elements…" sentence are part of this
/// block so the fallback concatenation is byte-stable with the original prompt.
const SYSTEM_PROMPT_RUNNER_PAGES: &str = r#"- page-prompt-home: Home — natural-language prompt entry point
- page-gui-automation: Workflows — select and run automation workflows
- page-active: Active Dashboard — monitor running executions
- page-workflow-queue: Workflow Queue — scheduled workflow runs
- page-terminal: Terminal — interactive shells and AI coding sessions (Claude Code, etc.); use ONLY when the user explicitly wants a terminal or an AI coding session. For creating workflows use page-unified-workflow-builder; for running existing workflows use page-gui-automation.
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
"#;

/// Shared footer appended after the page list (runner or caller). Generic — not
/// runner-specific — so it is emitted in both paths.
const SYSTEM_PROMPT_PAGES_FOOTER: &str = r#"
Each page has interactive elements (buttons, inputs, dropdowns) that can be targeted with natural language instructions.
"#;

/// Runner-specific integration walkthrough. Emitted ONLY in the fallback path
/// (when the caller supplied no `pages`); it references runner-only element ids
/// and would mislead a non-runner planner.
const SYSTEM_PROMPT_RUNNER_INTEGRATION: &str = r#"
=== Integration workflow on page-config-ui-bridge ===
To integrate a project with UI Bridge and/or generate documentation/tutorials for it, always navigate to page-config-ui-bridge. The Advanced disclosure reveals a project-path input, an Analyze button, and a generation-options checklist with these toggles (registered ids in backticks — use them verbatim with the `element <id>` form so NLActionExecutor routes directly via `/control/element/<id>/action`):
  - `ui-bridge-generate-registrations-checkbox` — useUIElement() calls for every interactive element
  - `ui-bridge-generate-page-ids-checkbox` — data-page-id attributes for page discovery
  - `ui-bridge-generate-specs-checkbox` — UI Bridge .spec.uibridge.json files per page
  - `ui-bridge-generate-tutorials-checkbox` — interactive per-page tutorials
  - `ui-bridge-generate-architecture-diagrams-checkbox` — Mermaid flowcharts per page
  - `ui-bridge-generate-demo-videos-checkbox` — automated demo scripts
  - `ui-bridge-generate-product-tours-checkbox` — click-through feature tours
  - `ui-bridge-generate-project-explainer-checkbox` — hierarchical navigable docs

Typical action sequence when the user wants to integrate a project:
  1. action: "click element ui-bridge-advanced-disclosure" — opens the Advanced
     per-stage controls panel via direct id routing (see Disclosure widgets
     section below for the rationale)
  2. action: "type '<PROJECT PATH>' in element ui-bridge-project-path-input"
  3. action: "click element ui-bridge-analyze-button"
  4. action: "check element ui-bridge-generate-<X>-checkbox" — once per requested
     generation type, using the registered ids listed above (e.g.
     "check element ui-bridge-generate-tutorials-checkbox",
     "check element ui-bridge-generate-project-explainer-checkbox")
  5. action: "click element ui-bridge-generate-button"

Pick only the checkboxes the user asked for — do not toggle options they didn't request.
"#;

/// Runner-specific component-action catalog. Emitted ONLY in the fallback path
/// (runner-owned component ids). Component actions are programmatic affordances
/// registered via `useUIComponent` — they are NOT clickable on-screen elements,
/// so the planner must invoke them with a `component-action` step (componentId +
/// actionId), never with a `find`/`action` step that names a non-existent button.
///
/// The seed entry is the `terminal-launch-menu` component, whose
/// `create-best-account` action spawns a live Claude Code AI session using the
/// account with the lowest current utilization (no configDir needed). This is
/// the REAL "open an AI session" affordance: there is no "Claude Code" button to
/// click. Mirrors `useUIComponent({ id: "terminal-launch-menu", … })` in
/// `qontinui-runner/src/components/terminal/TerminalPage.tsx`.
const SYSTEM_PROMPT_RUNNER_COMPONENT_ACTIONS: &str = r#"
=== Component actions (programmatic affordances — NOT clickable elements) ===
Some capabilities are exposed as registered COMPONENT ACTIONS, not as buttons on
the screen. To use one, emit a "component-action" step with the exact
`componentId` + `actionId` below. Do NOT try to "click" these — there is no
visible button for them, and naming a fake button (e.g. a "Claude Code" button)
will fail.

| componentId | actionId | what it does | params |
|-------------|----------|--------------|--------|
| terminal-launch-menu | create-best-account | Spawn a live AI coding session (Claude Code) using the AI account with the lowest current utilization. This IS how you "open an AI session" / "start Claude" — there is no Claude Code button to click. | { "count": number (optional, default 1), "context": string (optional initial prompt auto-typed after claude starts) } |
| terminal-launch-menu | create-plain | Spawn N blank terminals using the user's default shell. | { "count": number (optional, default 1) } |
| terminal-launch-menu | create-with-command | Spawn N terminals and auto-type the given shell command into each. | { "count": number (optional, default 1), "command": string (required) } |

The `terminal-launch-menu` component is on page-terminal. When the user asks to
"open an AI session", "start Claude / Claude Code", or "open an ai session in the
terminal page", plan:
  1. { "type": "navigate", "target": "page-terminal", "explanation": "..." }
  2. { "type": "component-action", "componentId": "terminal-launch-menu", "actionId": "create-best-account", "params": { "count": 1 }, "explanation": "..." }
Do NOT add an extra "action" step that clicks a "Claude Code" button — that
button does not exist; create-best-account is the entire spawn affordance.
"#;

/// The "Respond with valid JSON only…" instruction + Rules block. Always
/// emitted last (before suffix/disclosure/catalog). Generic — caller-agnostic.
const SYSTEM_PROMPT_RULES: &str = r#"
Respond with valid JSON only, no markdown fences:
{
  "summary": "Brief description of what you'll do",
  "steps": [
    { "type": "navigate", "target": "page-xxx", "explanation": "Why navigating here" },
    { "type": "action", "instruction": "click the 'Save' button", "explanation": "Why doing this" },
    { "type": "component-action", "componentId": "some-component", "actionId": "some-action", "params": { "key": "value" }, "explanation": "Why invoking this component action" }
  ]
}

Rules:
- Use "navigate" steps to change pages via the state machine
- Use "action" steps for interacting with elements (click, type, select, etc.)
- Use "component-action" steps to invoke a registered COMPONENT action (a named
  programmatic affordance that is NOT a clickable on-screen element). These are
  listed in the "Component actions" section below when available. A
  component-action step MUST set "componentId" and "actionId" to ids that appear
  verbatim in that section, plus an optional "params" object. NEVER emit a
  component-action whose ids are not listed.
- Action instructions should be natural language like "type 'hello' in the search field" or "click the Submit button"
- For simple navigation requests ("show me X"), just use a single navigate step
- For complex tasks, break into navigate + action steps
- When a page element catalog is provided below, prefer the exact labels from
  the catalog when naming buttons/inputs in action instructions. Do not invent
  generic names if a real one is listed.
- CRITICAL — never invent an element label, button name, or component/action id
  that does not appear in the page lists, the Component actions section, the
  Disclosure widgets registry, or the Page Element Catalog below. If the
  affordance you want is not listed, it almost certainly does not exist as a
  clickable element — look for a "component-action" instead (e.g. spawning an
  AI / Claude Code session), or navigate to the page that owns it. Do NOT
  fabricate a "Claude Code" (or similar) button: launching an AI coding session
  is a component-action, not a clickable button (see Component actions below).
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

/// A caller-supplied page (state machine state) to enumerate in the planner's
/// system prompt. When a caller (e.g. the web co-pilot) supplies its own pages,
/// they replace the runner's hardcoded fallback list so the planner emits the
/// caller's `page-…`/route ids and plans the caller's UI.
#[derive(Debug, Clone, Deserialize)]
pub struct PageDef {
    /// A `page-…`/route id (e.g. `page-dashboard`).
    pub id: String,
    /// Short human-readable description of the page.
    pub description: String,
}

/// Caller-supplied owned-`String` mirror of [`PageDisclosure`]. When supplied,
/// these replace the runner's static [`PAGE_DISCLOSURES`] in the rendered
/// disclosure section.
#[derive(Debug, Clone, Deserialize)]
pub struct DisclosureDef {
    /// Canonical `page-…` state id (must match an id in the caller's page list).
    pub page_canonical: String,
    /// Registered element id — used by NLActionExecutor for direct routing.
    pub element_id: String,
    /// Verbatim registered `label` from the `useUIElement` call.
    pub registered_label: String,
    /// Short human-readable hint describing what the disclosure reveals.
    pub summary: String,
}

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
    /// Caller-supplied page list. When present and non-empty, the planner
    /// enumerates THESE pages (and omits the runner-only integration workflow)
    /// instead of the runner's hardcoded fallback list. Wire name: `pages`.
    #[serde(default)]
    pub pages: Option<Vec<PageDef>>,
    /// Caller-supplied disclosure registry. When present, the planner renders
    /// the disclosure section from these instead of the static runner
    /// [`PAGE_DISCLOSURES`]. Wire name: `disclosures`.
    #[serde(default)]
    pub disclosures: Option<Vec<DisclosureDef>>,
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
    /// For `component-action` steps: the registered `useUIComponent` id whose
    /// action to invoke (e.g. `terminal-launch-menu`).
    #[serde(rename = "componentId", skip_serializing_if = "Option::is_none")]
    pub component_id: Option<String>,
    /// For `component-action` steps: the action id on that component (e.g.
    /// `create-best-account`).
    #[serde(rename = "actionId", skip_serializing_if = "Option::is_none")]
    pub action_id: Option<String>,
    /// For `component-action` steps: optional params object passed verbatim to
    /// the component action handler.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
    pub explanation: String,
}

/// Assemble the full planner system prompt.
///
/// Pure (no I/O) so it is unit-testable without HTTP or `run_prompt_sync`.
///
/// - `explain`: chooses the educational vs. brief suffix.
/// - `pages`: caller-supplied page list. When `Some` and non-empty, these pages
///   are enumerated and the runner-only integration-workflow section is OMITTED.
///   When `None`/empty, the runner's hardcoded fallback page list + integration
///   workflow are used (byte-stable with the original prompt).
/// - `disclosures`: caller-supplied disclosure registry. When `Some`, rendered
///   in place of the static [`PAGE_DISCLOSURES`].
/// - `catalog`: optional pre-trimmed page element catalog text.
fn build_system_prompt(
    explain: bool,
    pages: Option<&[PageDef]>,
    disclosures: Option<&[DisclosureDef]>,
    catalog: Option<&str>,
) -> String {
    let suffix = if explain {
        EXPLAIN_SUFFIX
    } else {
        BRIEF_SUFFIX
    };

    let catalog_section = catalog
        .map(|c| c.trim())
        .filter(|c| !c.is_empty())
        .map(|c| format!("{}{}\n", CATALOG_HEADER, c))
        .unwrap_or_default();

    let disclosure_section = match disclosures {
        Some(d) => render_disclosure_section(d),
        None => render_disclosure_section(PAGE_DISCLOSURES),
    };

    let mut out = String::with_capacity(8192);
    out.push_str(SYSTEM_PROMPT_PREFIX);

    match pages.filter(|p| !p.is_empty()) {
        // Caller supplied pages: enumerate THEM, omit the runner-only
        // integration-workflow section (it references runner element ids and
        // would mislead a non-runner planner).
        Some(p) => {
            for page in p {
                let _ = writeln!(out, "- {}: {}", page.id, page.description);
            }
            out.push_str(SYSTEM_PROMPT_PAGES_FOOTER);
        }
        // Fallback: runner's hardcoded page list + integration workflow.
        None => {
            out.push_str(SYSTEM_PROMPT_RUNNER_PAGES);
            out.push_str(SYSTEM_PROMPT_PAGES_FOOTER);
            out.push_str(SYSTEM_PROMPT_RUNNER_INTEGRATION);
            out.push_str(SYSTEM_PROMPT_RUNNER_COMPONENT_ACTIONS);
        }
    }

    out.push_str(SYSTEM_PROMPT_RULES);
    out.push_str(suffix);
    out.push_str(&disclosure_section);
    out.push_str(&catalog_section);
    out
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

    // Build combined prompt with system instructions + user request. The
    // caller may supply its own `pages` + `disclosures` (e.g. the web co-pilot
    // passing web pages); otherwise the runner's hardcoded fallback is used.
    let system_prompt = build_system_prompt(
        request.explain,
        request.pages.as_deref(),
        request.disclosures.as_deref(),
        request.page_catalog.as_deref(),
    );
    let full_prompt = format!("{}\n\nUser request: {}", system_prompt, request.prompt);

    // Pin the Claude account with the most remaining quota for the duration
    // of this prompt-home submission. No-op unless multi-account least-usage
    // mode is enabled in settings.
    let ai_response = tokio::task::spawn_blocking(move || {
        crate::ai_provider::pick_best_account();
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

    /// The assembled fallback system prompt (no caller pages/disclosures).
    /// Helper so tests don't re-thread the argument list everywhere.
    fn fallback_prompt() -> String {
        build_system_prompt(false, None, None, None)
    }

    /// Every disclosure entry must reference a `page-…` id present in the
    /// page list embedded in the fallback system prompt. If this fails, either
    /// the registry has a typo or the page list lost an entry — fix whichever
    /// drifted.
    #[test]
    fn disclosure_pages_exist_in_system_prompt() {
        let prompt = fallback_prompt();
        for d in PAGE_DISCLOSURES {
            assert!(
                prompt.contains(d.page_canonical),
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
    /// motivated the registry. If it's removed, the runner integration workflow
    /// in `SYSTEM_PROMPT_RUNNER_INTEGRATION` must also be updated, hence the
    /// assertion.
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
        let section = render_disclosure_section::<PageDisclosure>(&[]);
        assert!(section.is_empty());
    }

    /// The integration workflow paragraph must use the registered id directly
    /// rather than the loose phrase that originally caused ai/find to mis-
    /// classify the element. Guards against the "Home-page planner registry
    /// drifts from real tabs" memory entry's failure mode.
    #[test]
    fn integration_workflow_uses_registered_id() {
        let prompt = fallback_prompt();
        assert!(
            prompt.contains("click element ui-bridge-advanced-disclosure"),
            "integration workflow lost the direct-id routing instruction"
        );
        // The exact failing instruction from the bug report must not appear
        // as a planner *directive*. It can still appear as a quoted user-input
        // example in the Rules section (because we want the planner to
        // recognise that phrasing and translate it). Distinguish by checking
        // the integration-workflow paragraph specifically.
        let workflow_section = prompt
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
            fallback_prompt().contains("Disclosure widgets registry"),
            "generic disclosure rule missing from Rules block"
        );
    }

    /// When the caller supplies its own pages, the assembled prompt enumerates
    /// THOSE ids, drops the runner-only page ids, and omits the runner-specific
    /// integration-workflow section (which references runner element ids).
    #[test]
    fn caller_pages_replace_runner_pages_and_omit_integration() {
        let pages = vec![
            PageDef {
                id: "page-dashboard".to_string(),
                description: "Dashboard — overview of activity".to_string(),
            },
            PageDef {
                id: "page-runs".to_string(),
                description: "Runs — past executions".to_string(),
            },
        ];
        let prompt = build_system_prompt(false, Some(&pages), None, None);

        // Caller's pages + descriptions are enumerated.
        assert!(prompt.contains("page-dashboard"));
        assert!(prompt.contains("Dashboard — overview of activity"));
        assert!(prompt.contains("page-runs"));

        // Runner-only page ids must be gone.
        assert!(
            !prompt.contains("page-gui-automation"),
            "caller-pages prompt leaked a runner-only page id"
        );

        // Runner-only integration workflow must be omitted.
        assert!(
            !prompt.contains("=== Integration workflow on page-config-ui-bridge ==="),
            "caller-pages prompt leaked the runner-only integration workflow"
        );

        // Generic scaffolding (role line, pages footer, rules) must remain.
        assert!(prompt.contains("intent planner"));
        assert!(prompt.contains("Each page has interactive elements"));
        assert!(prompt.contains("Respond with valid JSON only"));
    }

    /// When no pages are supplied, the fallback prompt is unchanged: it contains
    /// the runner ids and the integration-workflow header.
    #[test]
    fn no_pages_keeps_runner_fallback() {
        let prompt = fallback_prompt();
        assert!(prompt.contains("page-gui-automation"));
        assert!(prompt.contains("page-prompt-home"));
        assert!(prompt.contains("=== Integration workflow on page-config-ui-bridge ==="));
        assert!(prompt.contains("=== Component actions"));
    }

    /// An empty (but `Some`) page list falls back to the runner list (treated
    /// the same as `None`).
    #[test]
    fn empty_caller_pages_falls_back_to_runner() {
        let prompt = build_system_prompt(false, Some(&[]), None, None);
        assert!(prompt.contains("page-gui-automation"));
        assert!(prompt.contains("=== Integration workflow on page-config-ui-bridge ==="));
    }

    /// Caller-supplied disclosures are rendered in place of the static registry.
    #[test]
    fn caller_disclosures_render_in_section() {
        let pages = vec![PageDef {
            id: "page-dashboard".to_string(),
            description: "Dashboard".to_string(),
        }];
        let disclosures = vec![DisclosureDef {
            page_canonical: "page-dashboard".to_string(),
            element_id: "web-advanced-filters-disclosure".to_string(),
            registered_label: "Advanced filters — narrow results by date and status".to_string(),
            summary: "Reveals date/status filter controls".to_string(),
        }];
        let prompt = build_system_prompt(false, Some(&pages), Some(&disclosures), None);
        assert!(prompt.contains("web-advanced-filters-disclosure"));
        assert!(prompt.contains("Advanced filters — narrow results"));
        // The static runner registry ROW data (its registered label) must not
        // be rendered — only the caller's rows. (Note: the disclosure-section
        // scaffolding prose hardcodes `ui-bridge-advanced-disclosure` as a
        // Strategy-B example regardless of the rows, so we assert on the row
        // payload — the registered label — not that example id.)
        assert!(
            !prompt.contains("Advanced: per-stage controls"),
            "caller-disclosure prompt leaked the static runner registry row"
        );
    }

    /// The fallback prompt must teach the planner the REAL spawn affordance:
    /// the `terminal-launch-menu` / `create-best-account` component action, and
    /// must forbid inventing a "Claude Code" button. Guards the D-NL1 root-cause
    /// fix (planner invented a non-existent "Claude Code" clickable button).
    #[test]
    fn fallback_prompt_teaches_terminal_component_action() {
        let prompt = fallback_prompt();
        assert!(
            prompt.contains("terminal-launch-menu"),
            "fallback prompt missing the terminal-launch-menu component"
        );
        assert!(
            prompt.contains("create-best-account"),
            "fallback prompt missing the create-best-account action"
        );
        assert!(
            prompt.contains("component-action"),
            "fallback prompt missing the component-action step type"
        );
        // Must explicitly disabuse the planner of the non-existent button.
        assert!(
            prompt.contains("Claude Code") && prompt.contains("does not exist"),
            "fallback prompt must explicitly forbid the invented 'Claude Code' button"
        );
    }

    /// Caller-supplied pages must NOT leak the runner-only component-action
    /// catalog (it references runner component ids).
    #[test]
    fn caller_pages_omit_runner_component_actions() {
        let pages = vec![PageDef {
            id: "page-dashboard".to_string(),
            description: "Dashboard".to_string(),
        }];
        let prompt = build_system_prompt(false, Some(&pages), None, None);
        assert!(
            !prompt.contains("terminal-launch-menu"),
            "caller-pages prompt leaked the runner-only component-action catalog"
        );
    }

    /// A `component-action` plan step round-trips through serde with the
    /// camelCase wire names the planner emits (`componentId`/`actionId`).
    #[test]
    fn component_action_step_deserializes() {
        let json = r#"{
            "type": "component-action",
            "componentId": "terminal-launch-menu",
            "actionId": "create-best-account",
            "params": { "count": 1 },
            "explanation": "spawn a Claude Code session"
        }"#;
        let step: PlanStep = serde_json::from_str(json).expect("component-action step parses");
        assert_eq!(step.step_type, "component-action");
        assert_eq!(step.component_id.as_deref(), Some("terminal-launch-menu"));
        assert_eq!(step.action_id.as_deref(), Some("create-best-account"));
        assert_eq!(
            step.params.as_ref().and_then(|p| p.get("count")),
            Some(&serde_json::json!(1))
        );
    }

    /// The catalog section is appended when a non-empty catalog is supplied.
    #[test]
    fn catalog_section_appended_when_present() {
        let prompt = build_system_prompt(false, None, None, Some("page-runs: Run, Cancel"));
        assert!(prompt.contains("=== Page Element Catalog ==="));
        assert!(prompt.contains("page-runs: Run, Cancel"));
    }
}

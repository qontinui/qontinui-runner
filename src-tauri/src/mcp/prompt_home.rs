//! Prompt Home intent planner
//!
//! Receives a natural language prompt and uses AI to plan a sequence of
//! runner UI actions (navigate pages, click buttons, fill forms).
//! Uses the runner's configured AI provider (Claude CLI, Claude API, etc.).

use axum::{extract::State, http::StatusCode, response::Json};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::{error, info};

use crate::mcp::types::{api_error, ApiResponse, ApiState};

const SYSTEM_PROMPT_BASE: &str = r#"You are the Qontinui Runner's intent planner. Given a user's request, plan a sequence of UI actions to accomplish it.

The runner has these pages (state machine states):
- page-workflows: Workflows — select and run automation workflows
- page-active: Active Dashboard — monitor running executions
- page-terminal: Terminal — Claude Code sessions, workflow generation, plan implementation
- page-triggers: Triggers — scheduled task configuration
- page-tasks: Scheduler — task scheduling and management
- page-unified-workflow-builder: Workflow Builder — create/edit workflows with AI
- page-library: Library — prompts, scripts, contexts, checks
- page-settings: Settings — AI provider, execution, and system configuration
- page-settings-ai: AI Settings — configure AI provider and model
- page-orchestration-loop: Orchestration — iterative build/reflect/fix pipeline
- page-error-monitor: Error Monitor — view and manage errors
- page-run-recap: Run Recap — review past execution results
- page-run-actions: Run Actions — detailed action logs from runs
- page-run-findings: Findings — issues detected during runs
- page-specs: Specs — UI Bridge spec management
- page-state-machine: State Machine — UI Bridge state machine builder
- page-capture: Capture — record UI interactions
- page-llm-analytics: LLM Analytics — token usage and model performance
- page-cost-control: Cost Control — spending limits and budgets
- page-evaluation: Evaluation — generator quality assessment
- page-knowledge-explorer: Knowledge Explorer — browse knowledge graph
- page-activity-timeline: Activity Timeline — chronological event history
- page-processes: Process Manager — system processes
- page-help: Help — documentation and tutorials
- page-skills: Skills — skill management and approval

Each page has interactive elements (buttons, inputs, dropdowns) that can be targeted with natural language instructions.

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
    let system_prompt = format!("{}{}{}", SYSTEM_PROMPT_BASE, suffix, catalog_section);
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

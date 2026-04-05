//! Prompt Home intent planner
//!
//! Receives a natural language prompt and uses AI to plan a sequence of
//! runner UI actions (navigate pages, click buttons, fill forms).

use axum::{extract::State, http::StatusCode, response::Json};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::{error, info};

use crate::mcp::types::{api_error, ApiResponse, ApiState};

const SYSTEM_PROMPT_BASE: &str = r#"You are the Qontinui Runner's intent planner. Given a user's request, plan a sequence of UI actions to accomplish it.

The runner has these pages (state machine states):
- page-gui-automation: Workflows — select and run automation workflows
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

    // Get API key from keychain
    let api_key = crate::config_facade::ai_keychain()
        .get("claude_api")
        .map_err(|e| {
            error!("HTTP: Failed to get API key: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("API key error: {}", e))),
            )
        })?
        .ok_or_else(|| {
            (
                StatusCode::BAD_REQUEST,
                Json(api_error(
                    "No Claude API key configured. Please set up your API key in Settings > AI.",
                )),
            )
        })?;

    // Get model from settings
    let ai_settings = crate::settings::get_ai_settings();
    let model = ai_settings.claude_api.model.clone();

    // Build system prompt based on explain mode
    let system_prompt = if request.explain {
        format!("{}{}", SYSTEM_PROMPT_BASE, EXPLAIN_SUFFIX)
    } else {
        format!("{}{}", SYSTEM_PROMPT_BASE, BRIEF_SUFFIX)
    };

    // Call Claude API with a request timeout
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .unwrap_or_default();

    let response = client
        .post("https://api.anthropic.com/v1/messages")
        .header("x-api-key", &api_key)
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json")
        .json(&serde_json::json!({
            "model": model,
            "max_tokens": 2048,
            "system": system_prompt,
            "messages": [{"role": "user", "content": request.prompt}]
        }))
        .send()
        .await
        .map_err(|e| {
            error!("HTTP: Claude API request failed: {}", e);
            (
                StatusCode::BAD_GATEWAY,
                Json(api_error(format!("AI request failed: {}", e))),
            )
        })?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        error!("HTTP: Claude API error ({}): {}", status, body);
        return Err((
            StatusCode::BAD_GATEWAY,
            Json(api_error(format!("AI error ({}): {}", status, body))),
        ));
    }

    // Parse Claude's response
    let api_response: serde_json::Value = response.json().await.map_err(|e| {
        error!("HTTP: Failed to parse Claude response: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!(
                "Failed to parse AI response: {}",
                e
            ))),
        )
    })?;

    // Extract text content from Claude's response
    let text_content = api_response["content"]
        .as_array()
        .and_then(|arr| arr.iter().find(|b| b["type"] == "text"))
        .and_then(|b| b["text"].as_str())
        .ok_or_else(|| {
            error!("HTTP: No text in Claude response");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error("No text in AI response")),
            )
        })?;

    // Extract JSON from Claude's text (find outermost braces)
    let json_str = if let Some(start) = text_content.find('{') {
        let end = text_content.rfind('}').unwrap_or(text_content.len() - 1);
        &text_content[start..=end]
    } else {
        text_content
    };

    let plan: PlanIntentResponse = serde_json::from_str(json_str).map_err(|e| {
        error!("HTTP: Failed to parse plan JSON: {} from: {}", e, json_str);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to parse AI plan: {}", e))),
        )
    })?;

    info!("HTTP: Intent plan generated with {} steps", plan.steps.len());
    Ok(Json(ApiResponse::success(plan)))
}

pub fn routes() -> axum::Router<Arc<ApiState>> {
    use axum::routing::post;
    axum::Router::new().route("/prompt-home/plan", post(plan_intent_handler))
}

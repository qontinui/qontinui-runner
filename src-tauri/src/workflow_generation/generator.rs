//! Workflow Generator
//!
//! Generates UnifiedWorkflows from natural language descriptions using AI.

use crate::ai_provider::{run_prompt_with_routing, AiResponse};
use crate::ai_router::TaskContext;
use crate::unified_workflows::UnifiedWorkflow;
use crate::workflow_generation::schema_context::build_schema_context;
use crate::workflow_generation::validation::{fix_workflow, validate_workflow, ValidationError};
use serde::{Deserialize, Serialize};
use tracing::{debug, error, info, warn};

/// Request to generate a workflow from natural language
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenerateWorkflowRequest {
    /// Natural language description of what the workflow should do
    pub description: String,
    /// Optional category for the generated workflow
    pub category: Option<String>,
    /// Optional tags for the generated workflow
    pub tags: Option<Vec<String>>,

    // === Workflow Configuration Options ===

    /// Maximum iterations for agentic phase (default: 10)
    #[serde(default)]
    pub max_iterations: Option<u32>,
    /// AI provider override (claude_cli, anthropic_api, openai_api, gemini_api)
    #[serde(default)]
    pub provider: Option<String>,
    /// Model override (depends on provider)
    #[serde(default)]
    pub model: Option<String>,
    /// Skip AI summary generation at the end (default: false)
    #[serde(default)]
    pub skip_ai_summary: Option<bool>,
    /// Log source selection mode: "default", "ai", "all", or a profile_id
    #[serde(default)]
    pub log_source_selection: Option<String>,
    /// Custom developer prompt template for the workflow
    #[serde(default)]
    pub prompt_template: Option<String>,
    /// Whether to auto-include contexts based on task mentions (default: true)
    #[serde(default)]
    pub auto_include_contexts: Option<bool>,
}

/// Response from workflow generation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenerateWorkflowResponse {
    /// The generated workflow (if successful)
    pub workflow: Option<UnifiedWorkflow>,
    /// Any validation errors found in the generated workflow
    pub validation_errors: Vec<String>,
    /// Whether the generation was successful
    pub success: bool,
    /// Error message if generation failed
    pub error: Option<String>,
    /// The model that was used for generation (not available for CLI)
    pub model_used: Option<String>,
}

/// Generate a workflow from a natural language description
pub fn generate_workflow(request: GenerateWorkflowRequest) -> GenerateWorkflowResponse {
    info!(
        "Generating workflow from description: {}",
        &request.description[..request.description.len().min(100)]
    );

    // Build the full prompt with schema context
    let schema_context = build_schema_context();
    let user_prompt = format!(
        r#"## User's Request
{}

{}

Generate a complete UnifiedWorkflow JSON that accomplishes this task.
Remember: Return ONLY valid JSON, no markdown code blocks or explanations."#,
        request.description,
        if let Some(ref category) = request.category {
            format!("Use category: {}", category)
        } else {
            String::new()
        }
    );

    let full_prompt = format!("{}\n\n{}", schema_context, user_prompt);

    // Create task context for routing - workflow generation is a complex task
    // The prompt contains "generate" and "workflow" which will be analyzed for complexity
    let task_context = TaskContext::from_prompt(&full_prompt);

    // Call the AI provider (synchronous)
    let ai_result: AiResponse = run_prompt_with_routing(&full_prompt, &task_context, 120);

    if !ai_result.success {
        error!(
            "AI provider error: {}",
            ai_result.error.as_deref().unwrap_or("Unknown error")
        );
        return GenerateWorkflowResponse {
            workflow: None,
            validation_errors: vec![],
            success: false,
            error: Some(format!(
                "AI provider error: {}",
                ai_result
                    .error
                    .unwrap_or_else(|| "Unknown error".to_string())
            )),
            model_used: None,
        };
    }

    debug!("AI response received, parsing JSON...");

    // Extract JSON from response (handle potential markdown code blocks)
    let json_text = extract_json_from_response(&ai_result.output);

    // Parse the JSON into a workflow
    let mut workflow: UnifiedWorkflow = match serde_json::from_str(&json_text) {
        Ok(w) => w,
        Err(e) => {
            error!("Failed to parse workflow JSON: {}", e);
            warn!("Response text: {}", &json_text[..json_text.len().min(500)]);
            return GenerateWorkflowResponse {
                workflow: None,
                validation_errors: vec![],
                success: false,
                error: Some(format!(
                    "Failed to parse generated workflow: {}. The AI may have returned invalid JSON.",
                    e
                )),
                model_used: None,
            };
        }
    };

    // Apply request options to the generated workflow
    if let Some(category) = request.category {
        workflow.category = category;
    }
    if let Some(tags) = request.tags {
        workflow.tags = tags;
    }
    if let Some(max_iterations) = request.max_iterations {
        workflow.max_iterations = max_iterations;
    }
    if let Some(provider) = request.provider {
        workflow.provider = Some(provider);
    }
    if let Some(model) = request.model {
        workflow.model = Some(model);
    }
    if let Some(skip_ai_summary) = request.skip_ai_summary {
        workflow.skip_ai_summary = skip_ai_summary;
    }
    if let Some(ref log_source) = request.log_source_selection {
        // Parse log source selection - can be "default", "ai", "all", or a profile_id
        use crate::unified_workflows::LogSourceSelection;
        workflow.log_source_selection = if log_source == "default"
            || log_source == "ai"
            || log_source == "all"
        {
            LogSourceSelection::Mode(log_source.clone())
        } else {
            LogSourceSelection::Profile {
                profile_id: log_source.clone(),
            }
        };
    }
    if let Some(prompt_template) = request.prompt_template {
        workflow.prompt_template = Some(prompt_template);
    }
    if let Some(auto_include) = request.auto_include_contexts {
        workflow.auto_include_contexts = auto_include;
    }

    // Auto-fix common issues
    fix_workflow(&mut workflow);

    // Validate the workflow
    let validation_errors: Vec<ValidationError> = validate_workflow(&workflow);
    let validation_error_strings: Vec<String> =
        validation_errors.iter().map(|e| e.to_string()).collect();

    if !validation_errors.is_empty() {
        warn!(
            "Generated workflow has {} validation errors",
            validation_errors.len()
        );
        for err in &validation_errors {
            warn!("  - {}", err);
        }
    }

    info!(
        "Successfully generated workflow: {} ({} setup, {} verification, {} agentic, {} completion steps)",
        workflow.name,
        workflow.setup_steps.len(),
        workflow.verification_steps.len(),
        workflow.agentic_steps.len(),
        workflow.completion_steps.len()
    );

    GenerateWorkflowResponse {
        workflow: Some(workflow),
        validation_errors: validation_error_strings,
        success: true,
        error: None,
        model_used: None, // CLI doesn't return model info
    }
}

/// Extract JSON from AI response, handling markdown code blocks
fn extract_json_from_response(response: &str) -> String {
    let trimmed = response.trim();

    // Try to find JSON in markdown code block
    if let Some(start) = trimmed.find("```json") {
        if let Some(end) = trimmed[start + 7..].find("```") {
            return trimmed[start + 7..start + 7 + end].trim().to_string();
        }
    }

    // Try to find JSON in generic code block
    if let Some(start) = trimmed.find("```") {
        let after_backticks = &trimmed[start + 3..];
        // Skip language identifier if present (e.g., ```json or ```)
        let json_start = if let Some(newline_pos) = after_backticks.find('\n') {
            newline_pos + 1
        } else {
            0
        };
        if let Some(end) = after_backticks[json_start..].find("```") {
            return after_backticks[json_start..json_start + end]
                .trim()
                .to_string();
        }
    }

    // Try to find JSON object directly
    if let Some(start) = trimmed.find('{') {
        if let Some(end) = trimmed.rfind('}') {
            if end > start {
                return trimmed[start..=end].to_string();
            }
        }
    }

    // Return as-is if no JSON found
    trimmed.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_json_from_code_block() {
        let response = r#"Here's the workflow:

```json
{"name": "test"}
```

Hope this helps!"#;

        let json = extract_json_from_response(response);
        assert_eq!(json, r#"{"name": "test"}"#);
    }

    #[test]
    fn test_extract_json_direct() {
        let response = r#"{"name": "test", "id": "123"}"#;
        let json = extract_json_from_response(response);
        assert_eq!(json, r#"{"name": "test", "id": "123"}"#);
    }

    #[test]
    fn test_extract_json_with_text() {
        let response =
            r#"Sure, here is the workflow: {"name": "test"} Let me know if you need changes."#;
        let json = extract_json_from_response(response);
        assert_eq!(json, r#"{"name": "test"}"#);
    }
}

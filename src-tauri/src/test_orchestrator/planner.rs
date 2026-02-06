//! Test Orchestration Planner
//!
//! Uses AI to create execution plans from saved API requests and
//! generates verification test code from execution results.

use super::types::*;
use crate::ai_provider::run_prompt_sync;
use crate::api_request::types::VariableExtraction;
use crate::saved_api_requests::SavedApiRequest;
use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};
use uuid::Uuid;

/// Safely truncate a string at a character boundary
fn safe_truncate(s: &str, max_len: usize) -> &str {
    if s.len() <= max_len {
        return s;
    }
    // Find a safe truncation point at a char boundary
    s.char_indices()
        .take_while(|(i, _)| *i < max_len)
        .last()
        .map(|(i, c)| &s[..i + c.len_utf8()])
        .unwrap_or("")
}

/// Safely truncate from the end of a string
fn safe_truncate_end(s: &str, max_len: usize) -> &str {
    if s.len() <= max_len {
        return s;
    }
    let start = s.len().saturating_sub(max_len);
    // Find a safe start point at a char boundary
    s.char_indices()
        .find(|(i, _)| *i >= start)
        .map(|(i, _)| &s[i..])
        .unwrap_or("")
}

/// Check if content type indicates binary data
fn is_binary_content_type(content_type: &str) -> bool {
    let ct = content_type.to_lowercase();
    ct.starts_with("image/")
        || ct.starts_with("audio/")
        || ct.starts_with("video/")
        || ct.starts_with("application/octet-stream")
        || ct.starts_with("application/zip")
        || ct.starts_with("application/gzip")
        || ct.starts_with("application/x-gzip")
        || ct.starts_with("application/pdf")
        || ct.contains("binary")
}

/// Check if body content looks like binary data
fn is_binary_body(body: &serde_json::Value) -> bool {
    if let serde_json::Value::String(s) = body {
        // Check for common binary magic bytes (when serialized as escaped string)
        // Gzip: \x1f\x8b or \u001f\ufffd
        // PNG: \x89PNG
        // JPEG: \xff\xd8\xff
        if s.len() > 10 {
            let start = safe_truncate(s, 20);
            // Gzip magic bytes
            if start.starts_with("\u{1f}\u{8b}") || start.starts_with("\\u001f") {
                return true;
            }
            // PNG magic bytes
            if start.starts_with("\u{89}PNG")
                || (start.contains("PNG") && start.starts_with("\u{89}"))
            {
                return true;
            }
            // Check for high concentration of non-printable characters
            let non_printable = s
                .chars()
                .take(100)
                .filter(|c| !c.is_ascii_graphic() && !c.is_ascii_whitespace())
                .count();
            if non_printable > 30 {
                return true;
            }
        }
    }
    false
}

/// AI response structure for plan creation
#[derive(Debug, Clone, Serialize, Deserialize)]
struct AiPlanResponse {
    steps: Vec<AiStep>,
    #[serde(default)]
    variable_mappings: Vec<AiVariableMapping>,
    #[serde(default)]
    verification_suggestions: Vec<AiVerificationSuggestion>,
    #[serde(default = "default_explanation")]
    explanation: String,
}

fn default_explanation() -> String {
    "AI-generated orchestration plan".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AiStep {
    request_id: String,
    name: String,
    purpose: String,
    depends_on: Vec<usize>,
    extractions: Vec<AiExtraction>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AiExtraction {
    variable_name: String,
    json_path: String,
    /// Default value - can be any JSON type (string, number, array, null)
    #[serde(default)]
    default_value: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AiVariableMapping {
    variable_name: String,
    source_step: usize,
    json_path: String,
    used_in_steps: Vec<usize>,
    description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AiVerificationSuggestion {
    condition: String,
    description: String,
    #[serde(default)]
    json_path: Option<String>,
    step_index: usize,
}

/// Create an orchestration plan from selected API requests
///
/// This function:
/// 1. Builds a prompt with the saved requests and user's test description
/// 2. Sends it to the AI for analysis
/// 3. Parses the AI response into a structured plan
pub fn create_orchestration_plan(
    request: &TestOrchestrationRequest,
    saved_requests: &[SavedApiRequest],
) -> Result<TestOrchestrationPlan> {
    info!(
        "Creating orchestration plan for {} requests: {:?}",
        request.request_ids.len(),
        request.test_description
    );

    // Filter to only requested IDs
    let selected_requests: Vec<&SavedApiRequest> = saved_requests
        .iter()
        .filter(|r| request.request_ids.contains(&r.id))
        .collect();

    if selected_requests.is_empty() {
        return Err(anyhow!(
            "No matching saved requests found for IDs: {:?}",
            request.request_ids
        ));
    }

    // Build the prompt
    let prompt = build_planning_prompt(request, &selected_requests);

    // Run AI
    info!(
        "Sending orchestration planning prompt ({} chars)",
        prompt.len()
    );
    let ai_response = run_prompt_sync(&prompt, 120);

    info!(
        "AI response: success={}, output_len={}, error={:?}",
        ai_response.success,
        ai_response.output.len(),
        ai_response.error
    );

    // Log the raw response for debugging truncation issues
    if ai_response.output.len() < 5000 {
        info!("AI raw output: {}", ai_response.output);
    } else {
        info!(
            "AI raw output (first 2000 chars): {}...",
            safe_truncate(&ai_response.output, 2000)
        );
        info!(
            "AI raw output (last 500 chars): ...{}",
            safe_truncate_end(&ai_response.output, 500)
        );
    }

    if !ai_response.success {
        return Err(anyhow!(
            "AI planning failed: {}",
            ai_response.error.unwrap_or_else(|| "Unknown error".into())
        ));
    }

    // Parse response
    let plan = parse_plan_response(&ai_response.output, request, &selected_requests)?;

    info!("Created orchestration plan with {} steps", plan.steps.len());

    Ok(plan)
}

/// Build the AI prompt for plan creation
fn build_planning_prompt(
    request: &TestOrchestrationRequest,
    saved_requests: &[&SavedApiRequest],
) -> String {
    let mut prompt = String::new();

    prompt.push_str("You are an API test orchestration planner. Analyze the available API requests and create an execution plan.\n\n");
    prompt.push_str("## Available API Requests\n\n");

    for (idx, req) in saved_requests.iter().enumerate() {
        prompt.push_str(&format!("### Request {} (ID: {})\n", idx + 1, req.id));
        prompt.push_str(&format!("**Name:** {}\n", req.name));
        prompt.push_str(&format!("**Description:** {}\n", req.description));
        prompt.push_str(&format!("**Method:** {}\n", req.method));
        prompt.push_str(&format!("**URL:** {}\n", req.url));

        if !req.headers.is_empty() {
            prompt.push_str("**Headers:**\n```json\n");
            prompt.push_str(
                &serde_json::to_string_pretty(&req.headers).unwrap_or_else(|_| "{}".into()),
            );
            prompt.push_str("\n```\n");
        }

        if let Some(body) = &req.body {
            prompt.push_str("**Body:**\n```json\n");
            // Try to pretty-print if it's JSON
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(body) {
                prompt.push_str(
                    &serde_json::to_string_pretty(&json).unwrap_or_else(|_| body.clone()),
                );
            } else {
                prompt.push_str(body);
            }
            prompt.push_str("\n```\n");
        }

        if !req.variable_extractions.is_empty() {
            prompt.push_str("**Existing Extractions:**\n");
            for ext in &req.variable_extractions {
                prompt.push_str(&format!(
                    "- `{}` from `{}`\n",
                    ext.variable_name, ext.json_path
                ));
            }
        }

        prompt.push('\n');
    }

    prompt.push_str("## User's Test Goal\n\n");
    prompt.push_str(&format!("\"{}\"\n\n", request.test_description));

    if let Some(ctx) = &request.context {
        prompt.push_str("## Additional Context\n\n");
        prompt.push_str(&format!("{}\n\n", ctx));
    }

    prompt.push_str(r#"## Your Task

Analyze these API requests and create an execution plan that achieves the user's test goal.

**Instructions:**
1. Determine which requests are needed and in what order
2. Identify variables to extract from responses (look for IDs, tokens, data that subsequent requests need)
3. Look for URL/body patterns like `{{variable}}` or `{id}` that need values from previous steps
4. For each step, specify what to extract and why
5. Suggest what to verify in the final response to confirm the test goal

**Output Format:**
Return ONLY a JSON object (no markdown code blocks) with this structure:

{
  "steps": [
    {
      "request_id": "the-request-uuid",
      "name": "Step 1: Start the process",
      "purpose": "Initiates the extraction process and returns an ID",
      "depends_on": [],
      "extractions": [
        {
          "variable_name": "extraction_id",
          "json_path": "$.data.id",
          "default_value": null
        }
      ]
    },
    {
      "request_id": "another-uuid",
      "name": "Step 2: Get results",
      "purpose": "Fetches the results using the extraction ID",
      "depends_on": [0],
      "extractions": []
    }
  ],
  "variable_mappings": [
    {
      "variable_name": "extraction_id",
      "source_step": 0,
      "json_path": "$.data.id",
      "used_in_steps": [1],
      "description": "The unique identifier for the extraction session"
    }
  ],
  "verification_suggestions": [
    {
      "condition": "response.data.states.length > 3",
      "description": "Verify that there are more than 3 states in the results",
      "json_path": "$.data.states",
      "step_index": 1
    }
  ],
  "explanation": "Brief explanation of the plan and why this order/approach was chosen"
}

Important:
- Use the exact request IDs provided
- Step indices are 0-based
- depends_on contains step indices that must complete first
- JSON paths should be valid JSONPath expressions (starting with $.)
"#);

    prompt
}

/// Parse the AI response into a structured plan
fn parse_plan_response(
    ai_output: &str,
    request: &TestOrchestrationRequest,
    saved_requests: &[&SavedApiRequest],
) -> Result<TestOrchestrationPlan> {
    // Try to extract JSON from the response
    let json_str = extract_json_from_response(ai_output)?;

    let ai_plan: AiPlanResponse = serde_json::from_str(&json_str)
        .with_context(|| format!("Failed to parse AI response as JSON: {}", json_str))?;

    // Convert AI response to our types
    let steps: Vec<OrchestrationStep> = ai_plan
        .steps
        .iter()
        .enumerate()
        .map(|(idx, ai_step)| {
            // Find the saved request to get URL/body templates
            let saved_req = saved_requests.iter().find(|r| r.id == ai_step.request_id);

            OrchestrationStep {
                step_index: idx,
                name: ai_step.name.clone(),
                request_id: ai_step.request_id.clone(),
                extractions: ai_step
                    .extractions
                    .iter()
                    .map(|e| VariableExtraction {
                        variable_name: e.variable_name.clone(),
                        json_path: e.json_path.clone(),
                        default_value: e.default_value.as_ref().map(|v| {
                            // Convert JSON value to string representation
                            match v {
                                serde_json::Value::String(s) => s.clone(),
                                serde_json::Value::Null => "null".to_string(),
                                other => other.to_string(),
                            }
                        }),
                    })
                    .collect(),
                depends_on: ai_step.depends_on.clone(),
                purpose: ai_step.purpose.clone(),
                url_template: saved_req.map(|r| r.url.clone()).unwrap_or_default(),
                body_template: saved_req.and_then(|r| r.body.clone()),
            }
        })
        .collect();

    let variable_mappings: Vec<VariableMappingInfo> = ai_plan
        .variable_mappings
        .iter()
        .map(|vm| VariableMappingInfo {
            variable_name: vm.variable_name.clone(),
            source_step: vm.source_step,
            json_path: vm.json_path.clone(),
            used_in_steps: vm.used_in_steps.clone(),
            description: vm.description.clone(),
        })
        .collect();

    let verification_suggestions: Vec<VerificationSuggestion> = ai_plan
        .verification_suggestions
        .iter()
        .map(|vs| VerificationSuggestion {
            condition: vs.condition.clone(),
            description: vs.description.clone(),
            json_path: vs.json_path.clone(),
            step_index: vs.step_index,
        })
        .collect();

    Ok(TestOrchestrationPlan {
        id: Uuid::new_v4().to_string(),
        request: request.clone(),
        steps,
        variable_mappings,
        verification_suggestions,
        explanation: ai_plan.explanation,
        created_at: Utc::now().to_rfc3339(),
    })
}

/// Extract JSON from AI response (handles markdown code blocks and truncation)
fn extract_json_from_response(response: &str) -> Result<String> {
    let trimmed = response.trim();

    // Try to find the JSON content
    let json_content = if trimmed.starts_with('{') {
        trimmed.to_string()
    } else if let Some(start) = trimmed.find("```json") {
        let after_marker = &trimmed[start + 7..];
        if let Some(end) = after_marker.find("```") {
            after_marker[..end].trim().to_string()
        } else {
            // No closing ```, might be truncated
            after_marker.trim().to_string()
        }
    } else if let Some(start) = trimmed.find("```") {
        let after_marker = &trimmed[start + 3..];
        let after_newline = after_marker
            .find('\n')
            .map(|i| &after_marker[i + 1..])
            .unwrap_or(after_marker);
        if let Some(end) = after_newline.find("```") {
            after_newline[..end].trim().to_string()
        } else {
            after_newline.trim().to_string()
        }
    } else if let Some(start) = trimmed.find('{') {
        // Try to find the matching closing brace by parsing progressively
        let remainder = &trimmed[start..];
        let mut best_json = String::new();
        // Try parsing increasingly larger substrings until we find valid JSON
        for (i, c) in remainder.char_indices() {
            if c == '}' {
                let candidate = &remainder[..=i];
                if serde_json::from_str::<serde_json::Value>(candidate).is_ok() {
                    best_json = candidate.to_string();
                    // Continue to find potentially larger valid JSON
                }
            }
        }
        if !best_json.is_empty() {
            best_json
        } else {
            // Fall back to taking everything from { to end
            remainder.to_string()
        }
    } else {
        return Err(anyhow!("Could not find JSON in AI response"));
    };

    // If it parses successfully, return it
    if serde_json::from_str::<serde_json::Value>(&json_content).is_ok() {
        return Ok(json_content);
    }

    // Try to repair truncated JSON
    let repaired = repair_truncated_json(&json_content);
    if serde_json::from_str::<serde_json::Value>(&repaired).is_ok() {
        warn!("Repaired truncated JSON response");
        return Ok(repaired);
    }

    // Return the original and let caller handle the parse error with context
    Err(anyhow!(
        "Failed to parse AI response as JSON: {}",
        if json_content.len() > 500 {
            format!("{}...", safe_truncate(&json_content, 500))
        } else {
            json_content
        }
    ))
}

/// Attempt to repair truncated JSON by closing open structures
fn repair_truncated_json(json: &str) -> String {
    let mut result = json.trim_end().to_string();

    // Remove any trailing partial string
    if let Some(last_quote) = result.rfind('"') {
        let after_quote = &result[last_quote + 1..];
        // If there's content after the last quote that isn't valid JSON syntax, truncate
        if !after_quote.is_empty()
            && !after_quote.starts_with([',', ']', '}', ':', ' ', '\n', '\r'])
        {
            // Truncated in middle of a string value, close the string
            result = format!("{}\"", &result[..last_quote + 1]);
        }
    }

    // Count brackets and add closing ones
    let mut open_braces = 0i32;
    let mut open_brackets = 0i32;
    let mut in_string = false;
    let mut prev_char = ' ';

    for c in result.chars() {
        if c == '"' && prev_char != '\\' {
            in_string = !in_string;
        } else if !in_string {
            match c {
                '{' => open_braces += 1,
                '}' => open_braces -= 1,
                '[' => open_brackets += 1,
                ']' => open_brackets -= 1,
                _ => {}
            }
        }
        prev_char = c;
    }

    // Ensure we're not in a string
    if in_string {
        result.push('"');
    }

    // Remove trailing comma if present
    let trimmed = result.trim_end();
    if let Some(stripped) = trimmed.strip_suffix(',') {
        result = stripped.to_string();
    }

    // Close open brackets and braces (in reverse order of likely nesting)
    for _ in 0..open_brackets {
        result.push(']');
    }
    for _ in 0..open_braces {
        result.push('}');
    }

    result
}

/// Generate verification test code from orchestration execution results
pub fn generate_test_from_execution(request: &GenerateTestRequest) -> Result<GeneratedTest> {
    info!(
        "Generating {} test from orchestration execution",
        request.test_type
    );

    let prompt = build_test_generation_prompt(request);

    let ai_response = run_prompt_sync(&prompt, 120);

    if !ai_response.success {
        return Err(anyhow!(
            "AI test generation failed: {}",
            ai_response.error.unwrap_or_else(|| "Unknown error".into())
        ));
    }

    parse_test_generation_response(&ai_response.output, &request.test_type)
}

/// Build the prompt for test code generation
fn build_test_generation_prompt(request: &GenerateTestRequest) -> String {
    let mut prompt = String::new();

    prompt.push_str("You are a test code generator. Generate verification test code based on API execution results.\n\n");

    // Include the original plan
    prompt.push_str("## Original Test Plan\n\n");
    prompt.push_str(&format!(
        "**Goal:** {}\n\n",
        request.plan.request.test_description
    ));
    prompt.push_str(&format!(
        "**Explanation:** {}\n\n",
        request.plan.explanation
    ));

    // Include execution results
    prompt.push_str("## Execution Results\n\n");
    prompt.push_str(&format!(
        "**Success:** {}\n\n",
        request.execution_result.success
    ));

    for step_result in &request.execution_result.step_results {
        prompt.push_str(&format!(
            "### Step {}: {}\n\n",
            step_result.step_index + 1,
            step_result.step_name
        ));
        prompt.push_str(&format!(
            "**Request:** {} {}\n",
            step_result.request.method, step_result.request.url
        ));
        prompt.push_str(&format!(
            "**Status:** {}\n",
            step_result.response.status_code
        ));
        prompt.push_str(&format!("**Duration:** {}ms\n", step_result.duration_ms));

        if !step_result.extracted_variables.is_empty() {
            prompt.push_str("**Extracted Variables:**\n");
            for (name, value) in &step_result.extracted_variables {
                let value_str = serde_json::to_string(value).unwrap_or_else(|_| "?".into());
                // Truncate long values safely
                let truncated = if value_str.len() > 200 {
                    format!("{}...", safe_truncate(&value_str, 200))
                } else {
                    value_str
                };
                prompt.push_str(&format!("- `{}` = {}\n", name, truncated));
            }
        }

        // Check if response is binary (image, compressed, etc.)
        let content_type = step_result.response.content_type.as_deref().unwrap_or("");
        let is_binary =
            is_binary_content_type(content_type) || is_binary_body(&step_result.response.body);

        if is_binary {
            prompt.push_str("**Response Body:** [Binary data]\n");
            prompt.push_str(&format!(
                "- Content-Type: {}\n- Size: {} bytes\n",
                content_type, step_result.response.size_bytes
            ));
            if content_type.starts_with("image/") {
                prompt.push_str("- Note: Response is an image file\n");
            }
            prompt.push('\n');
        } else {
            prompt.push_str("**Response Body (truncated):**\n```json\n");
            let body_str = serde_json::to_string_pretty(&step_result.response.body)
                .unwrap_or_else(|_| "null".into());
            // Truncate very long responses safely (respecting char boundaries)
            let truncated_body = if body_str.len() > 2000 {
                format!("{}...[truncated]", safe_truncate(&body_str, 2000))
            } else {
                body_str
            };
            prompt.push_str(&truncated_body);
            prompt.push_str("\n```\n\n");
        }
    }

    // Include verification suggestions
    prompt.push_str("## Verification Suggestions from Plan\n\n");
    for suggestion in &request.plan.verification_suggestions {
        prompt.push_str(&format!(
            "- {} (step {}, path: {})\n",
            suggestion.description,
            suggestion.step_index + 1,
            suggestion.json_path.as_deref().unwrap_or("N/A")
        ));
    }

    // Test type specific instructions
    prompt.push_str(&format!("\n## Test Type: {}\n\n", request.test_type));

    match request.test_type.as_str() {
        "python_script" => {
            prompt.push_str(
                r#"Generate a Python verification test that:
1. Makes the same API requests with proper variable chaining
2. Verifies the suggested conditions
3. Uses the `requests` library for HTTP calls
4. Returns verification results via print statements

The test should follow this structure:
```python
import requests
import json

def run_verification():
    """Run the verification test."""
    results = {"passed": True, "checks": []}

    # Step 1: First API call
    # ... make request, extract variables ...

    # Step 2: Second API call using extracted variables
    # ... make request with substituted variables ...

    # Verifications
    # ... check conditions, append to results["checks"] ...

    return results

if __name__ == "__main__":
    result = run_verification()
    print(json.dumps(result, indent=2))
```
"#,
            );
        }
        "playwright_cdp" => {
            prompt.push_str(
                r#"Generate a Playwright test using CDP (Chrome DevTools Protocol) that:
1. Makes the API requests using page.evaluate or fetch
2. Chains variables between requests
3. Verifies the suggested conditions using expect()

Use the Playwright test structure with async/await.
"#,
            );
        }
        _ => {
            prompt.push_str("Generate appropriate verification test code for this test type.\n");
        }
    }

    if let Some(instructions) = &request.additional_instructions {
        prompt.push_str(&format!(
            "\n## Additional Instructions\n\n{}\n",
            instructions
        ));
    }

    prompt.push_str(
        r#"
## Output Format

Return ONLY a JSON object (no markdown code blocks) with this structure:

{
  "name": "Test name based on the goal",
  "description": "Description of what the test verifies",
  "code": "The complete test code as a string (use \\n for newlines)",
  "explanation": "Brief explanation of the test and what each part does",
  "steps": [
    {
      "step_number": 1,
      "action": "Setup: Initialize HTTP client and variables",
      "expected": "Client ready for requests",
      "step_type": "setup"
    },
    {
      "step_number": 2,
      "action": "Request: POST to /extraction/start to create extraction",
      "expected": "200 OK with extraction_id in response",
      "step_type": "request"
    },
    {
      "step_number": 3,
      "action": "Assertion: Verify states.length > 3",
      "expected": "True - extraction returned more than 3 states",
      "step_type": "assertion"
    }
  ]
}

The "steps" array must contain a structured preview of what the test will do:
- step_type must be one of: "setup", "request", "assertion", "cleanup"
- Include ALL steps: setup, each API request, each assertion, any cleanup
- Be specific about what each step does and what result is expected
"#,
    );

    prompt
}

/// Parse the test generation response
fn parse_test_generation_response(ai_output: &str, test_type: &str) -> Result<GeneratedTest> {
    let json_str = extract_json_from_response(ai_output)?;

    #[derive(Deserialize)]
    struct AiTestStep {
        step_number: usize,
        action: String,
        expected: String,
        step_type: String,
    }

    #[derive(Deserialize)]
    struct AiTestResponse {
        name: String,
        description: String,
        code: String,
        explanation: String,
        #[serde(default)]
        steps: Vec<AiTestStep>,
    }

    let ai_test: AiTestResponse = serde_json::from_str(&json_str)
        .with_context(|| format!("Failed to parse test generation response: {}", json_str))?;

    let steps = ai_test
        .steps
        .into_iter()
        .map(|s| TestStep {
            step_number: s.step_number,
            action: s.action,
            expected: s.expected,
            step_type: s.step_type,
        })
        .collect();

    Ok(GeneratedTest {
        name: ai_test.name,
        description: ai_test.description,
        code: ai_test.code,
        test_type: test_type.to_string(),
        explanation: ai_test.explanation,
        steps,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_json_from_plain() {
        let input = r#"{"key": "value"}"#;
        let result = extract_json_from_response(input).unwrap();
        assert_eq!(result, r#"{"key": "value"}"#);
    }

    #[test]
    fn test_extract_json_from_markdown() {
        let input = r#"Here's the plan:

```json
{"steps": []}
```

That's it!"#;
        let result = extract_json_from_response(input).unwrap();
        assert_eq!(result, r#"{"steps": []}"#);
    }

    #[test]
    fn test_extract_json_fallback() {
        let input = r#"The response is {"data": 123} as shown."#;
        let result = extract_json_from_response(input).unwrap();
        assert_eq!(result, r#"{"data": 123}"#);
    }
}

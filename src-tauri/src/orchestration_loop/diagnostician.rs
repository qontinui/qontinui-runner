//! Diagnostic triage for the orchestration loop.
//!
//! Captures UI state after workflow execution, classifies failure root causes,
//! and builds context for prompt rewriting in the next iteration.

use tracing::{info, warn};

use super::remote_client::RunnerClient;
use super::types::{DiagnosePhaseConfig, DiagnosticResult, RootCauseCategory};
use crate::ai_provider::routing::run_prompt_sync;

/// Run the full diagnostic evaluation: capture state, run assertions, triage if failed.
pub async fn run_diagnostic(
    runner: &RunnerClient,
    config: &DiagnosePhaseConfig,
    diagnostic_history: &[DiagnosticResult],
    iteration: u32,
) -> Result<DiagnosticResult, String> {
    // 1. Capture page health
    let page_health = runner.get_page_health().await.unwrap_or_else(|e| {
        warn!("Failed to get page health: {}", e);
        serde_json::json!({ "status": "error", "error": e })
    });

    // 2. Run assertions
    let assertion_results = if !config.assertions.is_empty() {
        runner
            .run_assertions(&config.assertions)
            .await
            .unwrap_or_else(|e| {
                warn!("Failed to run assertions: {}", e);
                vec![serde_json::json!({ "error": e })]
            })
    } else {
        vec![]
    };

    // 3. Check if everything passed
    let health_ok = page_health
        .get("status")
        .and_then(|v| v.as_str())
        .map(|s| s == "healthy" || s == "ok")
        .unwrap_or(false);

    let all_assertions_passed = assertion_results
        .iter()
        .all(|r| r.get("passed").and_then(|v| v.as_bool()).unwrap_or(false));

    let passed = health_ok && (config.assertions.is_empty() || all_assertions_passed);

    if passed {
        info!("Diagnostic evaluation PASSED at iteration {}", iteration);
        return Ok(DiagnosticResult {
            passed: true,
            page_health,
            assertion_results,
            root_cause: None,
            diagnosis: None,
            prompt_rewrite_suggestion: None,
        });
    }

    // 4. Capture snapshot for AI triage context (if configured)
    let snapshot_context = if config.capture_snapshot {
        match runner.get_ui_snapshot().await {
            Ok(snap) => {
                let snap_str = serde_json::to_string_pretty(&snap).unwrap_or_default();
                if snap_str.len() > config.snapshot_max_chars {
                    // Truncate at a safe boundary
                    let mut end = config.snapshot_max_chars;
                    while end > 0 && !snap_str.is_char_boundary(end) {
                        end -= 1;
                    }
                    Some(snap_str[..end].to_string())
                } else {
                    Some(snap_str)
                }
            }
            Err(e) => {
                warn!("Failed to capture snapshot for triage: {}", e);
                None
            }
        }
    } else {
        None
    };

    // 5. AI triage call
    let triage_prompt = build_triage_prompt(
        &page_health,
        &assertion_results,
        snapshot_context.as_deref(),
        diagnostic_history,
        iteration,
    );

    let triage_result = tokio::task::spawn_blocking(move || run_prompt_sync(&triage_prompt, None))
        .await;

    let (root_cause, diagnosis, prompt_suggestion) = match triage_result {
        Ok(response) if response.success => parse_triage_response(&response.output),
        Ok(response) => {
            warn!(
                "Triage AI call failed: {}",
                response.error.unwrap_or_default()
            );
            (
                RootCauseCategory::Unknown,
                "AI triage call failed".to_string(),
                None,
            )
        }
        Err(e) => {
            warn!("Triage task panicked: {}", e);
            (
                RootCauseCategory::Unknown,
                format!("Triage task panicked: {}", e),
                None,
            )
        }
    };

    info!(
        "Diagnostic evaluation FAILED at iteration {}: root_cause={:?}",
        iteration, root_cause
    );

    Ok(DiagnosticResult {
        passed: false,
        page_health,
        assertion_results,
        root_cause: Some(root_cause),
        diagnosis: Some(diagnosis),
        prompt_rewrite_suggestion: prompt_suggestion,
    })
}

/// Build the AI triage prompt from diagnostic evidence.
fn build_triage_prompt(
    page_health: &serde_json::Value,
    assertion_results: &[serde_json::Value],
    snapshot: Option<&str>,
    history: &[DiagnosticResult],
    iteration: u32,
) -> String {
    let mut prompt = String::from(
        "# Diagnostic Triage — Workflow Failure Analysis\n\n\
         A workflow was executed and the result does not match expectations.\n\
         Analyze the evidence below and classify the root cause.\n\n",
    );

    prompt.push_str(&format!("## Iteration: {}\n\n", iteration));

    prompt.push_str("## Page Health\n```json\n");
    prompt.push_str(&serde_json::to_string_pretty(page_health).unwrap_or_default());
    prompt.push_str("\n```\n\n");

    prompt.push_str("## Assertion Results\n```json\n");
    prompt.push_str(&serde_json::to_string_pretty(assertion_results).unwrap_or_default());
    prompt.push_str("\n```\n\n");

    if let Some(snap) = snapshot {
        prompt.push_str("## DOM Snapshot (truncated)\n```json\n");
        prompt.push_str(snap);
        prompt.push_str("\n```\n\n");
    }

    if !history.is_empty() {
        prompt.push_str("## Previous Diagnostic History\n\n");
        for (i, prev) in history.iter().enumerate() {
            prompt.push_str(&format!(
                "### Attempt {}\n- Passed: {}\n- Root cause: {:?}\n- Diagnosis: {}\n- Suggestion: {}\n\n",
                i + 1,
                prev.passed,
                prev.root_cause
                    .as_ref()
                    .unwrap_or(&RootCauseCategory::Unknown),
                prev.diagnosis.as_deref().unwrap_or("N/A"),
                prev.prompt_rewrite_suggestion.as_deref().unwrap_or("N/A"),
            ));
        }
    }

    prompt.push_str(
        "## Instructions\n\n\
         Classify the root cause into exactly ONE of these categories:\n\
         - `bad_ui_rendering` — The UI itself rendered incorrectly (wrong components, broken layout)\n\
         - `bad_ui_bridge_evaluation` — UI Bridge misread the page (element not found, wrong selector)\n\
         - `bad_verification_steps` — The assertions/verification are wrong (testing the wrong thing)\n\
         - `bad_generation_prompt` — The workflow prompt was unclear or incomplete\n\
         - `bad_state_machine_logic` — The generated state machine has logical errors (wrong transitions, missing states)\n\
         - `infrastructure_issue` — Network error, timeout, crashed app, or other infrastructure failure\n\
         - `unknown` — Cannot determine\n\n\
         Respond with JSON only:\n\
         ```json\n\
         {\n\
           \"root_cause\": \"<category>\",\n\
           \"diagnosis\": \"<1-3 sentence explanation>\",\n\
           \"prompt_rewrite_suggestion\": \"<specific changes to make to the workflow generation prompt for the next attempt, or null if N/A>\"\n\
         }\n\
         ```",
    );

    prompt
}

/// Parse the AI triage response into structured fields.
fn parse_triage_response(response: &str) -> (RootCauseCategory, String, Option<String>) {
    let json_str = extract_json_block(response);

    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&json_str) {
        let root_cause = parsed
            .get("root_cause")
            .and_then(|v| v.as_str())
            .map(|s| match s {
                "bad_ui_rendering" => RootCauseCategory::BadUiRendering,
                "bad_ui_bridge_evaluation" => RootCauseCategory::BadUiBridgeEvaluation,
                "bad_verification_steps" => RootCauseCategory::BadVerificationSteps,
                "bad_generation_prompt" => RootCauseCategory::BadGenerationPrompt,
                "bad_state_machine_logic" => RootCauseCategory::BadStateMachineLogic,
                "infrastructure_issue" => RootCauseCategory::InfrastructureIssue,
                _ => RootCauseCategory::Unknown,
            })
            .unwrap_or(RootCauseCategory::Unknown);

        let diagnosis = parsed
            .get("diagnosis")
            .and_then(|v| v.as_str())
            .unwrap_or("No diagnosis provided")
            .to_string();

        let suggestion = parsed
            .get("prompt_rewrite_suggestion")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        (root_cause, diagnosis, suggestion)
    } else {
        // Fallback: use raw response as diagnosis
        (RootCauseCategory::Unknown, response.to_string(), None)
    }
}

/// Extract a JSON block from a response that may contain markdown code fences.
fn extract_json_block(text: &str) -> String {
    if let Some(start) = text.find("```json") {
        let after_fence = &text[start + 7..];
        if let Some(end) = after_fence.find("```") {
            return after_fence[..end].trim().to_string();
        }
    }
    // Try finding raw JSON object
    if let Some(start) = text.find('{') {
        if let Some(end) = text.rfind('}') {
            return text[start..=end].to_string();
        }
    }
    text.to_string()
}

/// Build a context string from diagnostic history for injection into
/// the next `generate_workflow` call's context parameter.
pub fn build_diagnostic_context(
    diagnostic_history: &[DiagnosticResult],
    original_context: Option<&str>,
) -> String {
    let mut ctx = String::new();

    if let Some(orig) = original_context {
        ctx.push_str(orig);
        ctx.push_str("\n\n");
    }

    if diagnostic_history.is_empty() {
        return ctx;
    }

    ctx.push_str("## Diagnostic History from Previous Attempts\n\n");
    ctx.push_str(
        "The following attempts were made and failed. \
         Use this information to avoid repeating the same mistakes.\n\n",
    );

    for (i, result) in diagnostic_history.iter().enumerate() {
        ctx.push_str(&format!(
            "### Attempt {} — {}\n",
            i + 1,
            if result.passed { "PASSED" } else { "FAILED" }
        ));

        if let Some(ref cause) = result.root_cause {
            ctx.push_str(&format!("- **Root cause:** {:?}\n", cause));
        }
        if let Some(ref diag) = result.diagnosis {
            ctx.push_str(&format!("- **Diagnosis:** {}\n", diag));
        }
        if let Some(ref suggestion) = result.prompt_rewrite_suggestion {
            ctx.push_str(&format!("- **Suggestion for next attempt:** {}\n", suggestion));
        }
        ctx.push('\n');
    }

    // Emphasize the latest suggestion
    if let Some(last) = diagnostic_history.last() {
        if let Some(ref suggestion) = last.prompt_rewrite_suggestion {
            ctx.push_str(&format!(
                "**IMPORTANT — Apply this change in the generated workflow:** {}\n\n",
                suggestion
            ));
        }
    }

    ctx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_triage_response_json_block() {
        let response = r#"Here is my analysis:
```json
{
  "root_cause": "bad_generation_prompt",
  "diagnosis": "The workflow prompt did not specify required transitions.",
  "prompt_rewrite_suggestion": "Add explicit transition list to the prompt."
}
```"#;

        let (cause, diag, suggestion) = parse_triage_response(response);
        assert_eq!(cause, RootCauseCategory::BadGenerationPrompt);
        assert!(diag.contains("transitions"));
        assert_eq!(
            suggestion.unwrap(),
            "Add explicit transition list to the prompt."
        );
    }

    #[test]
    fn test_parse_triage_response_raw_json() {
        let response =
            r#"{"root_cause": "infrastructure_issue", "diagnosis": "App crashed.", "prompt_rewrite_suggestion": null}"#;

        let (cause, diag, suggestion) = parse_triage_response(response);
        assert_eq!(cause, RootCauseCategory::InfrastructureIssue);
        assert_eq!(diag, "App crashed.");
        assert!(suggestion.is_none());
    }

    #[test]
    fn test_parse_triage_response_garbage() {
        let response = "I couldn't analyze the failure.";
        let (cause, diag, _) = parse_triage_response(response);
        assert_eq!(cause, RootCauseCategory::Unknown);
        assert_eq!(diag, response);
    }

    #[test]
    fn test_build_diagnostic_context_empty_history() {
        let ctx = build_diagnostic_context(&[], Some("Original context"));
        assert_eq!(ctx, "Original context\n\n");
    }

    #[test]
    fn test_build_diagnostic_context_with_history() {
        let history = vec![DiagnosticResult {
            passed: false,
            page_health: serde_json::json!({}),
            assertion_results: vec![],
            root_cause: Some(RootCauseCategory::BadGenerationPrompt),
            diagnosis: Some("Missing transitions".to_string()),
            prompt_rewrite_suggestion: Some("Add transitions".to_string()),
        }];

        let ctx = build_diagnostic_context(&history, None);
        assert!(ctx.contains("Diagnostic History"));
        assert!(ctx.contains("Missing transitions"));
        assert!(ctx.contains("IMPORTANT"));
        assert!(ctx.contains("Add transitions"));
    }

    #[test]
    fn test_extract_json_block_with_fences() {
        let text = "blah\n```json\n{\"a\": 1}\n```\nmore";
        assert_eq!(extract_json_block(text), "{\"a\": 1}");
    }

    #[test]
    fn test_extract_json_block_raw() {
        let text = "prefix {\"a\": 1} suffix";
        assert_eq!(extract_json_block(text), "{\"a\": 1}");
    }
}

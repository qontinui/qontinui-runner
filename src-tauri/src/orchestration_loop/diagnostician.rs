//! Diagnostic triage for the orchestration loop.
//!
//! Captures UI state after workflow execution, classifies failure root causes,
//! and builds context for prompt rewriting in the next iteration.

use tracing::{info, warn};

use super::remote_client::RunnerClient;
use super::types::{DiagnosePhaseConfig, DiagnosticResult, RootCauseCategory};
use crate::ai_provider::routing::run_prompt_with_model_override;
use crate::ai_router::TaskContext;
use crate::mcp::ui_bridge::screenshots::PageHealthSummary;
use crate::mcp::ui_bridge::structured_assert::{
    build_structured_assert_expression, StructuredAssertRequest,
};
use qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked;

/// Run the full diagnostic evaluation: capture state, run assertions, triage if failed.
#[expect(
    clippy::string_slice,
    reason = "legacy str byte slice — migrate to str::get / char_indices / str_utils::truncate_str; plan 2026-09-14-runner-str-byte-slice-class-has-no-lint-gate"
)]
pub async fn run_diagnostic(
    runner: &RunnerClient,
    config: &DiagnosePhaseConfig,
    diagnostic_history: &[DiagnosticResult],
    iteration: u32,
) -> Result<DiagnosticResult, String> {
    // 1. Capture page health. A read that produced no report is recorded as
    // such (no `summary`), so the pass predicate cannot read it as healthy.
    let page_health = runner.get_page_health().await.unwrap_or_else(|e| {
        warn!("Failed to get page health: {}", e);
        serde_json::json!({ "error": e })
    });
    if page_health_summary(&page_health).is_none() {
        warn!(
            "page health has no recognised `summary` (OK|WARNING|CRITICAL): {}",
            page_health
        );
    }

    // 2. Run assertions — one entry per configured element, each a verdict
    // or a no-verdict entry (`evaluated: false`, no `passed`).
    let assertion_results = if !config.assertions.is_empty() {
        runner.run_assertions(&config.assertions).await
    } else {
        vec![]
    };
    for (i, entry) in assertion_results.iter().enumerate() {
        if is_no_verdict(entry) {
            warn!(
                "diagnose assertion {} produced no verdict (status {}): {}",
                i,
                entry
                    .get("status")
                    .filter(|s| !s.is_null())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| "none".to_string()),
                entry
                    .get("error")
                    .and_then(|e| e.as_str())
                    .unwrap_or("no error message")
            );
        }
    }

    // 3. Check if everything passed
    let passed = diagnostic_passed(&page_health, &assertion_results, config.assertions.len());

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

    let model_override = config.model_override.clone();
    let triage_result = spawn_blocking_tracked(move || {
        let context = TaskContext::from_prompt(&triage_prompt);
        run_prompt_with_model_override(
            &triage_prompt,
            &context,
            None,
            model_override.as_deref(),
            None,
            None,
            None,
            None,
            None,
        )
    })
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

/// The page-health report's `summary`, when it is one the producer writes.
fn page_health_summary(page_health: &serde_json::Value) -> Option<PageHealthSummary> {
    page_health
        .get("summary")
        .and_then(|v| v.as_str())
        .and_then(PageHealthSummary::parse)
}

/// An assertion entry the route answered without a verdict: `evaluated:
/// false`, or any entry with no boolean `passed`.
fn is_no_verdict(entry: &serde_json::Value) -> bool {
    entry.get("evaluated").and_then(|v| v.as_bool()) == Some(false)
        || !entry.get("passed").is_some_and(|p| p.is_boolean())
}

/// The diagnose phase's pass predicate: the page-health `summary` is `OK`,
/// and — when assertions are configured — every configured assertion has an
/// entry carrying `passed: true`. A missing or unrecognised `summary`, a
/// no-verdict entry and a missing entry are all NOT a pass: an unknown is
/// never rendered as a verdict either way.
pub(crate) fn diagnostic_passed(
    page_health: &serde_json::Value,
    assertion_results: &[serde_json::Value],
    configured: usize,
) -> bool {
    let health_ok = page_health_summary(page_health) == Some(PageHealthSummary::Ok);
    let assertions_ok = assertion_results.len() == configured
        && assertion_results
            .iter()
            .all(|r| r.get("passed").and_then(|v| v.as_bool()).unwrap_or(false));
    health_ok && assertions_ok
}

/// Refuse a diagnose config whose assertions the route would reject: each
/// element must deserialize as the route's `StructuredAssertRequest` and
/// build into a script. The message names the element's index and the error.
/// Called by `start_loop` and `start_multi_loop` before anything runs, so a
/// malformed config costs no iteration, rebuild or AI triage call.
pub(crate) fn validate_diagnose_assertions(assertions: &[serde_json::Value]) -> Result<(), String> {
    for (i, element) in assertions.iter().enumerate() {
        let request: StructuredAssertRequest =
            serde_json::from_value(element.clone()).map_err(|e| {
                format!(
                    "pipeline.diagnose.assertions[{i}] is not an assertion the route accepts: {e}"
                )
            })?;
        build_structured_assert_expression(&request).map_err(|e| {
            format!("pipeline.diagnose.assertions[{i}] is not an assertion the route accepts: {e}")
        })?;
    }
    Ok(())
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

    // What the instruments could not establish is stated, not left for the
    // model to read as a UI failure.
    if page_health_summary(page_health).is_none() {
        prompt.push_str(
            "Page health produced no recognised `summary` (OK|WARNING|CRITICAL): the \
             health instrument did not report, so the page's health is UNKNOWN, not bad.\n\n",
        );
    }
    let no_verdict = assertion_results
        .iter()
        .filter(|r| is_no_verdict(r))
        .count();
    if no_verdict > 0 {
        prompt.push_str(&format!(
            "{} of {} assertions produced no verdict (the assertion was refused or \
             could not be evaluated); those entries say nothing about the UI and point \
             at the verification steps or the infrastructure, not the rendering.\n\n",
            no_verdict,
            assertion_results.len()
        ));
    }

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
#[expect(
    clippy::string_slice,
    reason = "legacy str byte slice — migrate to str::get / char_indices / str_utils::truncate_str; plan 2026-09-14-runner-str-byte-slice-class-has-no-lint-gate"
)]
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
            ctx.push_str(&format!(
                "- **Suggestion for next attempt:** {}\n",
                suggestion
            ));
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
        let response = r#"{"root_cause": "infrastructure_issue", "diagnosis": "App crashed.", "prompt_rewrite_suggestion": null}"#;

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

#[cfg(test)]
mod diagnose_verdict_tests {
    //! The diagnose phase's pass predicate, its triage disclosure and its
    //! start-time refusal (plan
    //! 2026-11-12-structured-assert-arms-never-pass-and-diagnose-assertions-never-reach-the-route,
    //! Phase 2). A no-verdict entry and an unrecognised health `summary` are
    //! UNKNOWN: never a pass, and named to triage as unknown rather than bad.

    use super::{build_triage_prompt, diagnostic_passed, validate_diagnose_assertions};
    use serde_json::json;

    /// The example the panel's placeholder and the tutorial both render.
    const PANEL_EXAMPLE: &str =
        include_str!("../../../src/components/orchestration-loop/diagnose-assertions-example.json");

    #[test]
    fn diagnostic_passed_reads_the_health_summary_and_every_verdict() {
        let ok = json!({ "summary": "OK" });
        let pass = json!({ "passed": true });
        let fail = json!({ "passed": false });
        let no_verdict = json!({ "evaluated": false, "error": "refused", "status": 400 });
        let rows: Vec<(&str, serde_json::Value, Vec<serde_json::Value>, usize, bool)> = vec![
            ("OK + passing", ok.clone(), vec![pass.clone()], 1, true),
            ("OK + no assertions", ok.clone(), vec![], 0, true),
            (
                "WARNING",
                json!({ "summary": "WARNING" }),
                vec![pass.clone()],
                1,
                false,
            ),
            (
                "CRITICAL",
                json!({ "summary": "CRITICAL" }),
                vec![],
                0,
                false,
            ),
            (
                "no summary",
                json!({ "error": "page health unavailable" }),
                vec![],
                0,
                false,
            ),
            (
                "legacy status key",
                json!({ "status": "healthy" }),
                vec![],
                0,
                false,
            ),
            ("lowercase ok", json!({ "summary": "ok" }), vec![], 0, false),
            (
                "OK + failing",
                ok.clone(),
                vec![pass.clone(), fail],
                2,
                false,
            ),
            (
                "OK + verdict beside no-verdict",
                ok.clone(),
                vec![pass.clone(), no_verdict],
                2,
                false,
            ),
            (
                "OK + missing entry",
                ok.clone(),
                vec![pass.clone()],
                2,
                false,
            ),
        ];
        let mut failures = Vec::new();
        for (name, health, results, configured, want) in rows {
            let got = diagnostic_passed(&health, &results, configured);
            if got != want {
                failures.push(format!("{name}: want {want}, got {got}"));
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    #[test]
    fn the_triage_prompt_names_assertions_with_no_verdict() {
        let results = vec![
            json!({ "passed": true }),
            json!({ "evaluated": false, "error": "refused", "status": 422 }),
            json!({ "evaluated": false, "error": "unreachable", "status": null }),
        ];
        let prompt = build_triage_prompt(&json!({ "summary": "OK" }), &results, None, &[], 1);
        assert!(
            prompt.contains("2 of 3 assertions produced no verdict"),
            "{prompt}"
        );
        let clean = build_triage_prompt(&json!({ "summary": "OK" }), &results[..1], None, &[], 1);
        assert!(!clean.contains("produced no verdict"), "{clean}");
    }

    #[test]
    fn the_triage_prompt_names_an_unrecognised_health_summary() {
        let prompt = build_triage_prompt(&json!({ "error": "HTTP 503" }), &[], None, &[], 1);
        assert!(
            prompt.contains("Page health produced no recognised `summary`"),
            "{prompt}"
        );
        let known = build_triage_prompt(&json!({ "summary": "WARNING" }), &[], None, &[], 1);
        assert!(!known.contains("no recognised `summary`"), "{known}");
    }

    /// T7: the example the authoring surfaces teach is one the route accepts.
    #[test]
    fn the_panels_example_is_accepted_by_the_route() {
        let example: Vec<serde_json::Value> =
            serde_json::from_str(PANEL_EXAMPLE).expect("the panel example is a JSON array");
        assert!(
            !example.is_empty(),
            "the example teaches at least one assertion"
        );
        validate_diagnose_assertions(&example).expect("every example element is accepted");
    }

    #[test]
    fn validate_diagnose_assertions_names_the_element_and_the_field() {
        let err = validate_diagnose_assertions(&[json!({ "type": "visible", "role": "tab" })])
            .expect_err("the old placeholder shape is refused");
        assert!(err.contains("assertions[0]"), "{err}");
        assert!(
            err.contains("unknown field `role`"),
            "names the field: {err}"
        );
        let err = validate_diagnose_assertions(&[
            json!({ "query": "p", "assertion": "exists" }),
            json!({ "query": "p", "assertion": "hasText", "expected": "" }),
        ])
        .expect_err("an empty hasText expected is refused");
        assert!(err.contains("assertions[1]"), "{err}");
        assert!(err.contains("`expected`"), "{err}");
    }
}

//! Verification Hardener Sub-Agent
//!
//! Post-processes generated workflows to strengthen verification steps:
//!
//! 1. Converts `prompt` verification steps to deterministic equivalents
//!    (`api_request`, `spec`, `check`) where possible.
//! 2. Converts Playwright/test-based UI checks to UI Bridge SDK `api_request`
//!    steps when the SDK is connected — SDK checks are faster, more reliable,
//!    and don't require Playwright browser installation.
//! 3. Upgrades weak assertions (e.g., status_code-only on SDK endpoints) to
//!    include `body_contains` for meaningful content verification.
//!
//! ## Pipeline placement
//!
//! ```text
//! Builder Agent → fix_workflow() → [Verification → Fixer] × N → Hardener → validate_workflow()
//! ```
//!
//! The hardener runs once after the verification/fixer loop, best-effort.
//! On error it falls back to the original workflow.

use crate::ai_provider::{run_prompt_with_routing, AiResponse};
use crate::ai_router::TaskContext;
use crate::doctor::DoctorHandle;
use crate::unified_workflows::UnifiedWorkflow;
use crate::workflow_generation::generator::extract_json_from_response;
use crate::workflow_generation::rules;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::{debug, info, warn};

// ============================================================================
// Public types
// ============================================================================

/// Summary of hardening conversions applied to a workflow.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HardeningSummary {
    /// Number of steps converted to better alternatives
    pub converted_count: usize,
    /// Number of prompt steps kept (genuinely subjective)
    pub kept_as_prompt_count: usize,
    /// Details of each conversion
    pub conversions: Vec<HardeningConversion>,
}

/// A single hardening conversion.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HardeningConversion {
    /// Step ID that was converted
    pub step_id: String,
    /// Original step name
    pub original_name: String,
    /// Original step type
    pub original_type: String,
    /// New type after conversion
    pub new_type: String,
    /// Brief explanation of the conversion
    pub explanation: String,
}

// ============================================================================
// Context extraction
// ============================================================================

/// Application context extracted from the workflow to guide hardening.
#[derive(Debug)]
struct AppContext {
    /// Whether the workflow targets a web app (localhost:3001 or localhost:1420)
    targets_web_app: bool,
    /// Whether setup includes a UI Bridge SDK connect step
    has_sdk_connect: bool,
    /// Target URL from setup page navigation (if any)
    setup_navigate_url: Option<String>,
    /// Working directories referenced in the workflow
    working_directories: Vec<String>,
    /// Language hints from check_type values
    language_hints: Vec<String>,
}

impl AppContext {
    /// Extract application context from a workflow and its description.
    fn from_workflow(workflow: &UnifiedWorkflow, _description: &str) -> Self {
        let workflow_json = serde_json::to_string(workflow).unwrap_or_default();

        let targets_web_app = workflow_json.contains("localhost:3001")
            || workflow_json.contains("localhost:1420");

        let has_sdk_connect = workflow_json.contains("ui-bridge/sdk/connect");

        // Extract navigation URL from setup steps
        let setup_navigate_url = workflow.setup_steps.iter().find_map(|step| {
            let url = step.get("url").and_then(|v| v.as_str()).unwrap_or("");
            if url.contains("ui-bridge/sdk/page/navigate") {
                step.get("body")
                    .and_then(|v| v.as_str())
                    .and_then(|body| serde_json::from_str::<Value>(body).ok())
                    .and_then(|parsed| parsed.get("url").and_then(|u| u.as_str()).map(String::from))
            } else {
                None
            }
        });

        let mut working_directories = Vec::new();
        let mut language_hints = Vec::new();

        let all_steps: Vec<&Value> = workflow
            .setup_steps
            .iter()
            .chain(workflow.verification_steps.iter())
            .chain(workflow.agentic_steps.iter())
            .chain(workflow.completion_steps.iter())
            .collect();

        for step in &all_steps {
            if let Some(wd) = step
                .get("working_directory")
                .and_then(|v| v.as_str())
            {
                if !working_directories.contains(&wd.to_string()) {
                    working_directories.push(wd.to_string());
                }
            }
            if let Some(ct) = step.get("check_type").and_then(|v| v.as_str()) {
                let hint = match ct {
                    "typecheck" => {
                        if let Some(cmd) = step.get("command").and_then(|v| v.as_str()) {
                            if cmd.contains("tsc") || cmd.contains("typescript") {
                                "typescript"
                            } else if cmd.contains("mypy") || cmd.contains("pyright") {
                                "python"
                            } else if cmd.contains("cargo") {
                                "rust"
                            } else {
                                "unknown"
                            }
                        } else {
                            "unknown"
                        }
                    }
                    "lint" | "format" | "analyze" | "security" => ct,
                    _ => continue,
                };
                if !language_hints.contains(&hint.to_string()) {
                    language_hints.push(hint.to_string());
                }
            }
        }

        Self {
            targets_web_app,
            has_sdk_connect,
            setup_navigate_url,
            working_directories,
            language_hints,
        }
    }
}

// ============================================================================
// Public API
// ============================================================================

/// Returns true if any verification steps could benefit from hardening.
///
/// Checks for:
/// - `prompt` steps (should be deterministic where possible)
/// - `test` steps with `playwright` test_type when SDK is connected (should use SDK)
/// - `api_request` steps hitting SDK endpoints with weak/missing assertions
/// - Agentic steps that lack corresponding verification coverage
pub fn should_harden_verification(workflow: &UnifiedWorkflow) -> bool {
    let has_sdk_connect = {
        let json = serde_json::to_string(workflow).unwrap_or_default();
        json.contains("ui-bridge/sdk/connect")
    };

    let has_hardenable_steps = workflow.verification_steps.iter().any(|step| {
        let step_type = step
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        match step_type {
            // Prompt steps are always candidates
            "prompt" => true,

            // Playwright test steps are candidates when SDK is connected
            "test" => {
                if !has_sdk_connect {
                    return false;
                }
                let test_type = step
                    .get("test_type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                // Playwright UI tests can be replaced by SDK checks
                test_type == "playwright" || is_ui_verification_test(step)
            }

            // API requests to SDK with weak assertions are candidates
            "api_request" if has_sdk_connect => {
                let url = step.get("url").and_then(|v| v.as_str()).unwrap_or("");
                if !url.contains("ui-bridge/sdk") {
                    return false;
                }
                // Check if assertions are weak (status_code only, no body_contains)
                let assertions = step
                    .get("assertions")
                    .and_then(|v| v.as_array())
                    .map(|a| a.len())
                    .unwrap_or(0);
                let has_body_check = step
                    .get("assertions")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter().any(|a| {
                            a.get("type")
                                .and_then(|t| t.as_str())
                                .map(|t| t == "body_contains")
                                .unwrap_or(false)
                        })
                    })
                    .unwrap_or(false);
                // Only status_code assertion, no content validation
                assertions > 0 && !has_body_check
            }

            _ => false,
        }
    });

    if has_hardenable_steps {
        return true;
    }

    // Check for agentic-verification coverage gaps:
    // If there are more agentic steps than deterministic verification steps,
    // there's likely a gap where some agentic work has no verification coverage
    if !workflow.agentic_steps.is_empty() {
        let deterministic_verification_count = workflow
            .verification_steps
            .iter()
            .filter(|s| {
                let t = s.get("type").and_then(|v| v.as_str()).unwrap_or("");
                t != "gate" && t != "prompt"
            })
            .count();

        // Heuristic: each agentic step should have at least one dedicated check
        if workflow.agentic_steps.len() > deterministic_verification_count {
            return true;
        }
    }

    false
}

/// Check if a test step is doing UI verification (vs. unit/integration tests).
fn is_ui_verification_test(step: &Value) -> bool {
    let command = step.get("command").and_then(|v| v.as_str()).unwrap_or("");
    let description = step.get("description").and_then(|v| v.as_str()).unwrap_or("");
    let name = step.get("name").and_then(|v| v.as_str()).unwrap_or("");

    let combined = format!("{} {} {}", command, description, name).to_lowercase();

    // UI-related keywords suggest this test verifies UI state
    combined.contains("ui") || combined.contains("element")
        || combined.contains("render") || combined.contains("visual")
        || combined.contains("page") || combined.contains("tab")
        || combined.contains("button") || combined.contains("component")
        || combined.contains("graph editor") || combined.contains("thumbnail")
}

/// Run the hardener agent to strengthen verification steps.
///
/// Converts prompt steps to deterministic equivalents, replaces Playwright UI
/// tests with SDK checks when the SDK is available, and upgrades weak assertions.
///
/// Non-fatal: returns the original workflow on any error.
pub fn run_hardener_agent(
    workflow: &UnifiedWorkflow,
    description: &str,
    doctor_handle: Option<&DoctorHandle>,
    conn: Option<&Connection>,
) -> (UnifiedWorkflow, Option<HardeningSummary>) {
    if !should_harden_verification(workflow) {
        debug!("No hardenable verification steps found, skipping");
        return (workflow.clone(), None);
    }

    let candidate_count = count_candidates(workflow);
    info!(
        "Running hardener agent on {} candidate verification steps",
        candidate_count
    );

    let app_context = AppContext::from_workflow(workflow, description);
    let workflow_json = match serde_json::to_string_pretty(workflow) {
        Ok(j) => j,
        Err(e) => {
            warn!("Failed to serialize workflow for hardening: {}", e);
            return (workflow.clone(), None);
        }
    };

    let prompt = build_hardener_prompt(&workflow_json, description, &app_context, conn);
    let task_context = TaskContext::from_prompt(&prompt);
    let ai_result: AiResponse = run_prompt_with_routing(&prompt, &task_context, doctor_handle);

    if !ai_result.success {
        warn!(
            "Hardener agent failed: {}",
            ai_result.error.as_deref().unwrap_or("unknown")
        );
        return (workflow.clone(), None);
    }

    // Parse the response
    let json_text = extract_json_from_response(&ai_result.output);
    let hardened: UnifiedWorkflow = match serde_json::from_str(&json_text) {
        Ok(w) => w,
        Err(e) => {
            warn!("Hardener produced invalid JSON: {}", e);
            return (workflow.clone(), None);
        }
    };

    // Safety checks
    if let Some(error) = validate_hardened_output(workflow, &hardened) {
        warn!("Hardener safety check failed: {}", error);
        return (workflow.clone(), None);
    }

    // Build summary by comparing original and hardened verification steps
    let summary = build_summary(workflow, &hardened);

    info!(
        "Hardener converted {} steps, kept {} as prompt",
        summary.converted_count, summary.kept_as_prompt_count
    );

    (hardened, Some(summary))
}

/// Count verification steps that are candidates for hardening.
fn count_candidates(workflow: &UnifiedWorkflow) -> usize {
    let has_sdk = {
        let json = serde_json::to_string(workflow).unwrap_or_default();
        json.contains("ui-bridge/sdk/connect")
    };

    workflow.verification_steps.iter().filter(|step| {
        let t = step.get("type").and_then(|v| v.as_str()).unwrap_or("");
        match t {
            "prompt" => true,
            "test" if has_sdk => {
                let tt = step.get("test_type").and_then(|v| v.as_str()).unwrap_or("");
                tt == "playwright" || is_ui_verification_test(step)
            }
            "api_request" if has_sdk => {
                let url = step.get("url").and_then(|v| v.as_str()).unwrap_or("");
                url.contains("ui-bridge/sdk")
            }
            _ => false,
        }
    }).count()
}

// ============================================================================
// Prompt builder
// ============================================================================

fn build_hardener_prompt(workflow_json: &str, description: &str, app_context: &AppContext, conn: Option<&Connection>) -> String {
    let mut prompt = format!(
        r#"You are a verification hardener agent for Qontinui Runner.

Your job is to analyze ALL verification steps and convert them to the best available deterministic
approach. The workflow below was generated for this task: "{description}"

## Conversion Rules
"#,
        description = description,
    );

    // Load conversion rules from DB or use fallback
    let conversion_rules = if let Some(conn) = conn {
        let all = rules::load_rules(conn, "hardener", "conversion_rules");
        if !all.is_empty() {
            // Filter by condition
            let filtered: Vec<_> = all
                .into_iter()
                .filter(|r| match r.condition.as_deref() {
                    None => true,
                    Some("has_sdk_connect") => app_context.has_sdk_connect,
                    Some("targets_web_app") => app_context.targets_web_app,
                    _ => true,
                })
                .collect();
            Some(filtered)
        } else {
            None
        }
    } else {
        None
    };

    if let Some(ref db_rules) = conversion_rules {
        for rule in db_rules {
            prompt.push_str(&format!("\n### Rule {}: {}\n\n{}\n", rule.rule_number, rule.title, rule.content));
        }
    } else {
        // Fallback: hardcoded conversion rules
        prompt.push_str(r#"
### Rule 1: Convert `prompt` steps to deterministic equivalents

| Prompt check type | Convert to | Method |
|---|---|---|
| UI element presence/structure | `api_request` | UI Bridge SDK endpoint with assertions |
| Content/text on page | `api_request` | UI Bridge SDK `/ai/search` with `body_contains` assertion |
| File existence | `check` | `custom_command` with `test -f <path>` |
| File content | `check` | `custom_command` with `grep -q <pattern> <file>` |
| Code quality (lint) | `check` | `check_type: "lint"` with appropriate command |
| Code quality (typecheck) | `check` | `check_type: "typecheck"` with appropriate command |
| API health/response | `api_request` | Direct HTTP with `status_code` assertion |
| Subjective/qualitative | Keep as `prompt` | Cannot be made deterministic |
"#);

        if app_context.has_sdk_connect {
            prompt.push_str(r#"
### Rule 2: Replace Playwright UI tests with UI Bridge SDK checks

When the UI Bridge SDK is connected (it IS connected in this workflow's setup), Playwright-based UI
verification tests should be converted to `api_request` steps using SDK endpoints. The SDK provides
direct programmatic access to registered UI elements without requiring a Playwright browser instance.

**Why:** Playwright tests require browser binaries (often missing), are slow, flaky, and test through
the browser rendering layer. The UI Bridge SDK communicates directly with the app's element registry,
making checks faster, more reliable, and always available.

**How to convert a Playwright test step to SDK-based api_request steps:**

If a single Playwright test checks multiple things (e.g., "verify tabs exist AND thumbnails render AND
buttons work"), split it into multiple `api_request` steps — one per distinct verification concern.
Each step should check one thing well rather than trying to replicate the entire test.

Common conversions:
| Playwright test checks... | SDK `api_request` equivalent |
|---|---|
| Element exists on page | `GET /ui-bridge/sdk/elements?contentOnly=true` with `body_contains: "elementId"` |
| Text content visible | `POST /ui-bridge/sdk/ai/search` with body `{"text":"..."}` and `body_contains` assertion |
| Tab/section present | `GET /ui-bridge/sdk/elements?contentOnly=true` with `body_contains: "tab-name"` |
| Page loads without errors | `GET /ui-bridge/sdk/snapshot` with `status_code: 200` and `body_contains: "elements"` |
| Click interaction works | `POST /ui-bridge/sdk/ai/execute` with body `{"action":"click","elementId":"..."}` |
| Element count/state | `GET /ui-bridge/sdk/elements` with `body_contains` for expected elements |

**URL base for all SDK endpoints:** `http://localhost:9876/ui-bridge/sdk/`

Keep the original `test` step's `id` on ONE of the replacement `api_request` steps, and generate new
UUIDs for additional steps. If you add steps, update the step count accordingly.

**IMPORTANT — SDK capability limits.** Do NOT convert Playwright tests that involve any of these
interactions, because the UI Bridge SDK cannot perform them:
- **Keyboard shortcuts** (Ctrl+Z, Delete, Ctrl+S, etc.)
- **File upload dialogs**
- **Form submit/reset** (not implemented in SDK)
- **Multi-tab or multi-window** interactions
- **Screenshot pixel comparisons**

These tests MUST remain as Playwright `test` steps.

Note: **Drag-and-drop CAN be converted** to SDK. Use `POST /ui-bridge/sdk/ai/execute` with
`{"action":"drag","elementId":"<source>","params":{"target":{"elementId":"<target>"}}}` or
with `{"action":"drag","elementId":"<source>","params":{"targetPosition":{"x":N,"y":N}}}`.
"#);
        }

        if app_context.has_sdk_connect {
            prompt.push_str(r#"
### Rule 3: Strengthen weak SDK api_request assertions

If an existing `api_request` step targets a UI Bridge SDK endpoint but only has a `status_code` assertion,
add a `body_contains` assertion to verify meaningful content. A 200 response from the SDK just means the
endpoint is reachable — it doesn't verify the UI state. SDK endpoints return 200 even for EMPTY results.

**Specific endpoint guidance:**

- **`POST /ui-bridge/sdk/ai/search`**: This is the most common weak assertion. The response is `{"results": [...], "total": N}`.
  When `total` is 0, the search found NOTHING — but status is still 200!
  **FIX:** Look at the step's `body` field to find the search text (e.g., `{"text": "Settings tab"}`).
  Extract the key term and add `body_contains` with that term (e.g., `"expected": "Settings"`).
  If the search text references a specific UI element, use a distinctive keyword from the element's expected text content.

- **`GET /ui-bridge/sdk/elements`** or **`GET /ui-bridge/sdk/elements?contentOnly=true`**:
  Add `body_contains: "id"` at minimum (verifies elements exist). Better: add the expected element ID or text.

- **`GET /ui-bridge/sdk/snapshot`**: Add `body_contains: "elements"` (verifies snapshot has data).
  Better: add the expected page title or a known element's text content.

- **`POST /ui-bridge/sdk/discover`**: Add `body_contains` with expected element attributes or text.

- **`GET /ui-bridge/sdk/ai/summary`** or **`GET /ui-bridge/sdk/ai/snapshot`**:
  Add `body_contains` with a keyword from the expected page content.
"#);
        }

        if app_context.has_sdk_connect {
            prompt.push_str(r#"
### Rule 4: Inject page navigation before SDK verification checks

If the workflow's setup_steps include a page navigation step (`POST /ui-bridge/sdk/page/navigate` with a target URL),
the verification phase MUST also navigate to that same URL before any SDK element checks. This is because the
agentic phase may navigate the browser away from the target page (e.g., to documentation, error pages, or other
app routes), and verification needs to be on the correct page to check UI state.

**How to apply:**
1. Look in `setup_steps` for any `api_request` step with URL containing `/ui-bridge/sdk/page/navigate`
2. Extract the target URL from that step's `body` field (e.g., `{"url": "http://localhost:3001/management"}`)
3. If the FIRST SDK-related step in `verification_steps` is NOT a navigation step, INSERT a new `api_request`
   step at the beginning of verification_steps:
   ```json
   {
     "id": "<new-uuid>",
     "type": "api_request",
     "phase": "verification",
     "name": "Navigate to target page",
     "method": "POST",
     "url": "http://localhost:9876/ui-bridge/sdk/page/navigate",
     "body": "{\"url\": \"<extracted-url>\"}",
     "content_type": "application/json",
     "assertions": [{"type": "status_code", "expected": 200}]
   }
   ```
4. If verification already starts with a navigate step to the same URL, skip this rule.
"#);
        }

        prompt.push_str(r#"
### Rule 5: Ensure every agentic step has corresponding verification coverage

Examine EACH prompt step in `agentic_steps` and identify the distinct goals/features it describes.
Then check whether `verification_steps` has at least one deterministic step (check, test, api_request, spec)
that would FAIL if that specific goal was NOT implemented.

**Common gaps:**
- Agentic step says "implement drag-and-drop" but verification only checks tab existence
- Agentic step says "add thumbnails to state nodes" but no verification checks for thumbnails
- Agentic step says "add keyboard shortcuts" but no verification for keyboard functionality

**How to fix gaps:**
1. For each uncovered agentic goal, ADD a new `api_request` verification step that checks for the
   feature's artifacts (e.g., search for specific component names, element types, or content text)
2. Generate new UUIDs for added steps
3. Add the new step IDs to the gate's `required_steps` array
4. If an agentic step has multiple distinct goals (e.g., "implement thumbnails AND drag-and-drop"),
   add one verification step per goal

**Tab/section existence is NOT adequate coverage.** Checking that a tab named "State View" exists does NOT
verify that the tab has spatial visualization content. Add content-specific checks.
"#);
    }

    // Add context-specific guidance
    if app_context.targets_web_app {
        prompt.push_str("\n### Web App Context\n");
        prompt.push_str("This workflow targets a web application. ");
        if app_context.has_sdk_connect {
            prompt.push_str("UI Bridge SDK is connected in setup. Use SDK endpoints:\n");
            prompt.push_str("- Element list: `GET http://localhost:9876/ui-bridge/sdk/elements?contentOnly=true`\n");
            prompt.push_str("- AI search: `POST http://localhost:9876/ui-bridge/sdk/ai/search` with body `{\"text\":\"query\"}`\n");
            prompt.push_str("- Page snapshot: `GET http://localhost:9876/ui-bridge/sdk/snapshot`\n");
            prompt.push_str("- Execute action: `POST http://localhost:9876/ui-bridge/sdk/ai/execute` with body `{\"action\":\"click\",\"elementId\":\"id\"}`\n");
            if let Some(ref nav_url) = app_context.setup_navigate_url {
                prompt.push_str(&format!(
                    "- Setup navigates to: `{}` — verification should navigate here before SDK checks (Rule 4)\n",
                    nav_url
                ));
            }
        } else {
            prompt.push_str("No SDK connect step found — prefer file-based or API health checks.\n");
        }
    }

    if !app_context.working_directories.is_empty() {
        prompt.push_str(&format!(
            "\n### Working Directories\nKnown directories: {}\n",
            app_context.working_directories.join(", ")
        ));
    }

    if !app_context.language_hints.is_empty() {
        prompt.push_str(&format!(
            "\n### Languages Detected\n{}\n",
            app_context.language_hints.join(", ")
        ));
    }

    // Load critical rules from DB or use fallback
    let critical_section = if let Some(conn) = conn {
        let critical = rules::load_rules(conn, "hardener", "critical_rules");
        if !critical.is_empty() {
            let mut s = String::from("\n## Critical Rules\n\n");
            s.push_str(&rules::format_rules_as_markdown(&critical));
            Some(s)
        } else {
            None
        }
    } else {
        None
    };

    if let Some(critical) = critical_section {
        prompt.push_str(&critical);
    } else {
        prompt.push_str(r#"
## Critical Rules

1. **Only modify verification_steps**: Do NOT change setup_steps, agentic_steps, or completion_steps
2. **Preserve step IDs**: Every step must keep its original `id` field (unless splitting a step, in which case keep the original ID on one and generate new UUIDs for additions)
3. **Preserve step order**: Steps must remain in the same relative position
4. **Adding steps is allowed**: If a Playwright test step checks multiple things, you MAY replace it with multiple `api_request` steps. You MAY also add NEW verification steps to cover uncovered agentic goals. Keep original `id`s on existing steps and generate new UUIDs for additions. Update `gate` required_steps if you add new step IDs.
5. **Keep subjective prompts**: If a prompt step is genuinely subjective (e.g., "Is the UX intuitive?"), keep it as `prompt`
6. **Complete required fields**: Every converted step must have all required fields for its new type
7. **API request assertions required**: For `api_request` steps, ALWAYS include `assertions` with at least `status_code` AND `body_contains`
8. **Check conversion fields**: For `check` conversions, include `check_type`, `command`, and `working_directory`
9. **Do not convert check steps**: Do NOT convert `check` steps (lint, typecheck, etc.) — they are already deterministic
10. **Do not convert gate steps**: Do NOT convert `gate` steps — they are structural"#);
    }

    prompt.push_str(&format!(
        r#"

## Output

Return ONLY the complete, valid UnifiedWorkflow JSON with the hardened verification steps.
No markdown, no code fences, no explanations.

## Current Workflow JSON

{workflow_json}"#,
        workflow_json = workflow_json,
    ));

    prompt
}

// ============================================================================
// Safety validation
// ============================================================================

/// Validate that the hardened output preserves critical invariants.
/// Returns None if valid, Some(error_message) if invalid.
fn validate_hardened_output(
    original: &UnifiedWorkflow,
    hardened: &UnifiedWorkflow,
) -> Option<String> {
    // Step count may increase (splitting is allowed) but never decrease
    if hardened.verification_steps.len() < original.verification_steps.len() {
        return Some(format!(
            "Step count decreased: {} -> {} (splitting adds steps, never removes)",
            original.verification_steps.len(),
            hardened.verification_steps.len()
        ));
    }

    // All original step IDs must still be present (in any order since splits insert after)
    for orig_step in &original.verification_steps {
        let orig_id = orig_step.get("id").and_then(|v| v.as_str()).unwrap_or("");
        if orig_id.is_empty() {
            continue;
        }
        let found = hardened.verification_steps.iter().any(|h| {
            h.get("id")
                .and_then(|v| v.as_str())
                .map(|id| id == orig_id)
                .unwrap_or(false)
        });
        if !found {
            return Some(format!(
                "Original step ID '{}' is missing from hardened output",
                orig_id
            ));
        }
    }

    // Non-verification phases must be unchanged
    if original.setup_steps != hardened.setup_steps {
        return Some("setup_steps were modified".to_string());
    }
    if original.agentic_steps != hardened.agentic_steps {
        return Some("agentic_steps were modified".to_string());
    }
    if original.completion_steps != hardened.completion_steps {
        return Some("completion_steps were modified".to_string());
    }

    None
}

// ============================================================================
// Summary builder
// ============================================================================

fn build_summary(original: &UnifiedWorkflow, hardened: &UnifiedWorkflow) -> HardeningSummary {
    let mut conversions = Vec::new();
    let mut converted_count = 0;
    let mut kept_as_prompt_count = 0;

    // Build a map of original step IDs to their types/names for comparison
    let orig_map: std::collections::HashMap<&str, (&str, &str)> = original
        .verification_steps
        .iter()
        .filter_map(|s| {
            let id = s.get("id").and_then(|v| v.as_str())?;
            let t = s.get("type").and_then(|v| v.as_str()).unwrap_or("unknown");
            let n = s.get("name").and_then(|v| v.as_str()).unwrap_or("");
            Some((id, (t, n)))
        })
        .collect();

    // Check each hardened step against the original
    for hard_step in &hardened.verification_steps {
        let hard_id = hard_step.get("id").and_then(|v| v.as_str()).unwrap_or("");
        let hard_type = hard_step
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");

        if let Some(&(orig_type, orig_name)) = orig_map.get(hard_id) {
            // This step existed in the original — check if type changed
            if orig_type != hard_type {
                converted_count += 1;
                conversions.push(HardeningConversion {
                    step_id: hard_id.to_string(),
                    original_name: orig_name.to_string(),
                    original_type: orig_type.to_string(),
                    new_type: hard_type.to_string(),
                    explanation: format!("Converted from {} to {}", orig_type, hard_type),
                });
            } else if orig_type == "prompt" {
                kept_as_prompt_count += 1;
            }
        } else {
            // New step (from splitting) — count as a conversion
            let hard_name = hard_step
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            converted_count += 1;
            conversions.push(HardeningConversion {
                step_id: hard_id.to_string(),
                original_name: format!("(split from parent) {}", hard_name),
                original_type: "split".to_string(),
                new_type: hard_type.to_string(),
                explanation: format!("New {} step from splitting a multi-concern step", hard_type),
            });
        }
    }

    HardeningSummary {
        converted_count,
        kept_as_prompt_count,
        conversions,
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_test_workflow(verification_steps: Vec<Value>) -> UnifiedWorkflow {
        UnifiedWorkflow {
            id: "test-id".to_string(),
            name: "Test Workflow".to_string(),
            description: "Test".to_string(),
            category: "testing".to_string(),
            tags: vec![],
            setup_steps: vec![],
            verification_steps,
            agentic_steps: vec![],
            completion_steps: vec![],
            max_iterations: 3,
            timeout_seconds: None,
            provider: None,
            model: None,
            skip_ai_summary: false,
            targeted_error_ids: vec![],
            log_source_selection: Default::default(),
            context_ids: vec![],
            disabled_context_ids: vec![],
            auto_include_contexts: true,
            prompt_template: None,
            log_watch_enabled: false,
            health_check_enabled: false,
            health_check_urls: vec![],
            preflight_check_enabled: false,
            generated_by_task_run_id: None,
            created_at: "2025-01-01T00:00:00Z".to_string(),
            updated_at: "2025-01-01T00:00:00Z".to_string(),
        }
    }

    fn make_sdk_workflow(verification_steps: Vec<Value>) -> UnifiedWorkflow {
        let mut w = make_test_workflow(verification_steps);
        w.setup_steps = vec![json!({
            "id": "setup-sdk",
            "type": "api_request",
            "method": "POST",
            "url": "http://localhost:9876/ui-bridge/sdk/connect",
            "body": "{\"url\": \"http://localhost:3001\"}"
        })];
        w
    }

    // === should_harden_verification tests ===

    #[test]
    fn test_should_harden_returns_false_for_all_deterministic() {
        let workflow = make_test_workflow(vec![
            json!({"id": "s1", "type": "check", "check_type": "typecheck", "command": "npx tsc --noEmit"}),
            json!({"id": "s2", "type": "api_request", "method": "GET", "url": "http://localhost:3001/health"}),
        ]);
        assert!(!should_harden_verification(&workflow));
    }

    #[test]
    fn test_should_harden_returns_true_for_prompt_verification() {
        let workflow = make_test_workflow(vec![
            json!({"id": "s1", "type": "check", "check_type": "typecheck", "command": "npx tsc --noEmit"}),
            json!({"id": "s2", "type": "prompt", "phase": "verification", "content": "Check if the button exists"}),
        ]);
        assert!(should_harden_verification(&workflow));
    }

    #[test]
    fn test_should_harden_returns_false_for_empty_verification() {
        let workflow = make_test_workflow(vec![]);
        assert!(!should_harden_verification(&workflow));
    }

    #[test]
    fn test_should_harden_detects_playwright_with_sdk() {
        let workflow = make_sdk_workflow(vec![
            json!({"id": "s1", "type": "test", "test_type": "playwright", "command": "npx playwright test ui-check"}),
        ]);
        assert!(should_harden_verification(&workflow));
    }

    #[test]
    fn test_should_harden_ignores_playwright_without_sdk() {
        let workflow = make_test_workflow(vec![
            json!({"id": "s1", "type": "test", "test_type": "playwright", "command": "npx playwright test"}),
        ]);
        assert!(!should_harden_verification(&workflow));
    }

    #[test]
    fn test_should_harden_detects_ui_test_with_sdk() {
        let workflow = make_sdk_workflow(vec![
            json!({"id": "s1", "type": "test", "test_type": "custom_command", "name": "Verify page renders correctly", "command": "check-ui"}),
        ]);
        assert!(should_harden_verification(&workflow));
    }

    #[test]
    fn test_should_harden_detects_weak_sdk_assertions() {
        let workflow = make_sdk_workflow(vec![
            json!({
                "id": "s1", "type": "api_request",
                "url": "http://localhost:9876/ui-bridge/sdk/elements",
                "assertions": [{"type": "status_code", "expected": 200}]
            }),
        ]);
        assert!(should_harden_verification(&workflow));
    }

    #[test]
    fn test_should_harden_ignores_strong_sdk_assertions() {
        let workflow = make_sdk_workflow(vec![
            json!({
                "id": "s1", "type": "api_request",
                "url": "http://localhost:9876/ui-bridge/sdk/elements",
                "assertions": [
                    {"type": "status_code", "expected": 200},
                    {"type": "body_contains", "expected": "button-submit"}
                ]
            }),
        ]);
        assert!(!should_harden_verification(&workflow));
    }

    // === is_ui_verification_test tests ===

    #[test]
    fn test_is_ui_test_detects_ui_keywords() {
        assert!(is_ui_verification_test(&json!({
            "name": "Verify page renders with element thumbnails",
            "command": "test"
        })));
        assert!(is_ui_verification_test(&json!({
            "name": "Check UI components",
            "command": "test"
        })));
    }

    #[test]
    fn test_is_ui_test_ignores_non_ui() {
        assert!(!is_ui_verification_test(&json!({
            "name": "Run unit tests",
            "command": "pytest tests/"
        })));
    }

    // === validate_hardened_output tests ===

    #[test]
    fn test_validate_rejects_step_count_decrease() {
        let original = make_test_workflow(vec![
            json!({"id": "a", "type": "prompt"}),
            json!({"id": "b", "type": "prompt"}),
        ]);
        let hardened = make_test_workflow(vec![json!({"id": "a", "type": "check"})]);

        let error = validate_hardened_output(&original, &hardened);
        assert!(error.is_some());
        assert!(error.unwrap().contains("Step count decreased"));
    }

    #[test]
    fn test_validate_allows_step_count_increase_from_splitting() {
        let original = make_test_workflow(vec![
            json!({"id": "a", "type": "test", "test_type": "playwright"}),
        ]);
        let hardened = make_test_workflow(vec![
            json!({"id": "a", "type": "api_request", "url": "http://localhost:9876/ui-bridge/sdk/elements"}),
            json!({"id": "new-1", "type": "api_request", "url": "http://localhost:9876/ui-bridge/sdk/ai/search"}),
        ]);

        let error = validate_hardened_output(&original, &hardened);
        assert!(error.is_none());
    }

    #[test]
    fn test_validate_rejects_missing_original_id() {
        let original = make_test_workflow(vec![
            json!({"id": "step-1", "type": "prompt"}),
        ]);
        let hardened = make_test_workflow(vec![
            json!({"id": "step-2", "type": "check"}),
        ]);

        let error = validate_hardened_output(&original, &hardened);
        assert!(error.is_some());
        assert!(error.unwrap().contains("missing"));
    }

    #[test]
    fn test_validate_rejects_setup_modification() {
        let mut original = make_test_workflow(vec![json!({"id": "a", "type": "prompt"})]);
        original.setup_steps = vec![json!({"id": "setup-1", "type": "shell_command"})];

        let mut hardened = make_test_workflow(vec![json!({"id": "a", "type": "check"})]);
        hardened.setup_steps = vec![json!({"id": "setup-1", "type": "api_request"})];

        let error = validate_hardened_output(&original, &hardened);
        assert!(error.is_some());
        assert!(error.unwrap().contains("setup_steps were modified"));
    }

    #[test]
    fn test_validate_accepts_valid_conversion() {
        let original = make_test_workflow(vec![
            json!({"id": "step-1", "type": "prompt", "content": "Check button"}),
        ]);
        let hardened = make_test_workflow(vec![
            json!({"id": "step-1", "type": "api_request", "method": "GET", "url": "http://localhost:9876/ui-bridge/sdk/elements"}),
        ]);

        let error = validate_hardened_output(&original, &hardened);
        assert!(error.is_none());
    }

    // === build_summary tests ===

    #[test]
    fn test_summary_tracks_prompt_conversions() {
        let original = make_test_workflow(vec![
            json!({"id": "s1", "name": "Check code", "type": "prompt"}),
            json!({"id": "s2", "name": "Check UI", "type": "prompt"}),
            json!({"id": "s3", "name": "Lint check", "type": "check"}),
        ]);
        let hardened = make_test_workflow(vec![
            json!({"id": "s1", "name": "Check code", "type": "check"}),
            json!({"id": "s2", "name": "Check UI", "type": "prompt"}),
            json!({"id": "s3", "name": "Lint check", "type": "check"}),
        ]);

        let summary = build_summary(&original, &hardened);
        assert_eq!(summary.converted_count, 1);
        assert_eq!(summary.kept_as_prompt_count, 1);
        assert_eq!(summary.conversions.len(), 1);
        assert_eq!(summary.conversions[0].step_id, "s1");
        assert_eq!(summary.conversions[0].original_type, "prompt");
        assert_eq!(summary.conversions[0].new_type, "check");
    }

    #[test]
    fn test_summary_tracks_test_to_api_request_conversion() {
        let original = make_test_workflow(vec![
            json!({"id": "s1", "name": "Playwright UI test", "type": "test", "test_type": "playwright"}),
        ]);
        let hardened = make_test_workflow(vec![
            json!({"id": "s1", "name": "Verify elements via SDK", "type": "api_request"}),
        ]);

        let summary = build_summary(&original, &hardened);
        assert_eq!(summary.converted_count, 1);
        assert_eq!(summary.conversions[0].original_type, "test");
        assert_eq!(summary.conversions[0].new_type, "api_request");
    }

    #[test]
    fn test_summary_tracks_split_steps() {
        let original = make_test_workflow(vec![
            json!({"id": "s1", "name": "Big Playwright test", "type": "test"}),
        ]);
        let hardened = make_test_workflow(vec![
            json!({"id": "s1", "name": "Check elements", "type": "api_request"}),
            json!({"id": "new-1", "name": "Check tabs", "type": "api_request"}),
            json!({"id": "new-2", "name": "Check content", "type": "api_request"}),
        ]);

        let summary = build_summary(&original, &hardened);
        assert_eq!(summary.converted_count, 3); // 1 type change + 2 new splits
        assert_eq!(summary.conversions.len(), 3);
    }

    // === AppContext tests ===

    #[test]
    fn test_app_context_detects_web_app() {
        let mut workflow = make_test_workflow(vec![]);
        workflow.setup_steps = vec![json!({
            "id": "s1", "type": "api_request", "url": "http://localhost:3001/api/test"
        })];
        let ctx = AppContext::from_workflow(&workflow, "test");
        assert!(ctx.targets_web_app);
    }

    #[test]
    fn test_app_context_detects_sdk_connect() {
        let workflow = make_sdk_workflow(vec![]);
        let ctx = AppContext::from_workflow(&workflow, "test");
        assert!(ctx.has_sdk_connect);
    }

    #[test]
    fn test_app_context_extracts_navigate_url() {
        let mut workflow = make_sdk_workflow(vec![]);
        workflow.setup_steps.push(json!({
            "id": "nav-1",
            "type": "api_request",
            "method": "POST",
            "url": "http://localhost:9876/ui-bridge/sdk/page/navigate",
            "body": "{\"url\": \"http://localhost:3001/management\"}",
            "content_type": "application/json"
        }));
        let ctx = AppContext::from_workflow(&workflow, "test");
        assert_eq!(ctx.setup_navigate_url.as_deref(), Some("http://localhost:3001/management"));
    }

    #[test]
    fn test_app_context_no_navigate_url_without_nav_step() {
        let workflow = make_sdk_workflow(vec![]);
        let ctx = AppContext::from_workflow(&workflow, "test");
        assert!(ctx.setup_navigate_url.is_none());
    }

    #[test]
    fn test_hardener_prompt_includes_rule_4_when_sdk_connected() {
        let workflow = make_sdk_workflow(vec![
            json!({"id": "s1", "type": "prompt", "content": "Check page"}),
        ]);
        let ctx = AppContext::from_workflow(&workflow, "test");
        let prompt = build_hardener_prompt("{}", "test", &ctx, None);
        assert!(prompt.contains("Rule 4"));
        assert!(prompt.contains("page navigation"));
    }

    #[test]
    fn test_hardener_prompt_includes_sdk_response_guidance() {
        let workflow = make_sdk_workflow(vec![
            json!({"id": "s1", "type": "api_request", "url": "http://localhost:9876/ui-bridge/sdk/ai/search", "assertions": [{"type": "status_code", "expected": 200}]}),
        ]);
        let ctx = AppContext::from_workflow(&workflow, "test");
        let prompt = build_hardener_prompt("{}", "test", &ctx, None);
        assert!(prompt.contains("ai/search"));
        assert!(prompt.contains("total"));
        assert!(prompt.contains("body_contains"));
    }

    #[test]
    fn test_should_harden_detects_agentic_verification_gap() {
        // 5 agentic steps but only 2 deterministic verification steps → gap
        let mut workflow = make_test_workflow(vec![
            json!({"id": "s1", "type": "check", "check_type": "typecheck", "command": "npx tsc --noEmit"}),
            json!({"id": "s2", "type": "check", "check_type": "lint", "command": "npx eslint ."}),
            json!({"id": "s3", "type": "gate", "required_steps": ["s1", "s2"]}),
        ]);
        workflow.agentic_steps = vec![
            json!({"id": "a1", "type": "prompt", "content": "Implement feature A"}),
            json!({"id": "a2", "type": "prompt", "content": "Implement feature B"}),
            json!({"id": "a3", "type": "prompt", "content": "Implement feature C"}),
        ];
        assert!(should_harden_verification(&workflow));
    }

    #[test]
    fn test_should_harden_no_gap_when_sufficient_coverage() {
        // 2 agentic steps with 3 deterministic verification steps → sufficient
        let mut workflow = make_test_workflow(vec![
            json!({"id": "s1", "type": "check", "check_type": "typecheck", "command": "npx tsc --noEmit"}),
            json!({"id": "s2", "type": "api_request", "url": "http://localhost:3001/health"}),
            json!({"id": "s3", "type": "api_request", "url": "http://localhost:3001/api/test"}),
            json!({"id": "s4", "type": "gate", "required_steps": ["s1", "s2", "s3"]}),
        ]);
        workflow.agentic_steps = vec![
            json!({"id": "a1", "type": "prompt", "content": "Fix feature A"}),
            json!({"id": "a2", "type": "prompt", "content": "Fix feature B"}),
        ];
        assert!(!should_harden_verification(&workflow));
    }

    #[test]
    fn test_hardener_prompt_includes_rule_5() {
        let mut workflow = make_sdk_workflow(vec![
            json!({"id": "s1", "type": "check", "check_type": "typecheck", "command": "npx tsc --noEmit"}),
        ]);
        workflow.agentic_steps = vec![
            json!({"id": "a1", "type": "prompt", "content": "Implement thumbnails"}),
        ];
        let ctx = AppContext::from_workflow(&workflow, "test");
        let prompt = build_hardener_prompt("{}", "test", &ctx, None);
        assert!(prompt.contains("Rule 5"));
        assert!(prompt.contains("agentic step"));
        assert!(prompt.contains("verification coverage"));
    }
}

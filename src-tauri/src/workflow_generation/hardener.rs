//! Verification Hardener Sub-Agent
//!
//! Post-processes generated workflows to strengthen verification steps:
//!
//! 1. Converts `prompt` verification steps to deterministic equivalents
//!    (`command` with check_type/curl, `ui_bridge` with assert) where possible.
//! 2. Converts Playwright-based UI checks to `command` steps using curl to
//!    UI Bridge SDK endpoints when the SDK is connected — SDK checks are
//!    faster, more reliable, and don't require Playwright browser installation.
//! 3. Strengthens weak SDK verification commands (e.g., curl without grep)
//!    to include content checks for meaningful verification.
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

        let targets_web_app =
            workflow_json.contains("localhost:3001") || workflow_json.contains("localhost:1420");

        let has_sdk_connect = workflow_json.contains("ui-bridge/sdk/connect");

        // Extract navigation URL from setup steps
        let setup_navigate_url = workflow.setup_steps.iter().find_map(|step| {
            // Check command field for curl to navigate endpoint
            let cmd = step.get("command").and_then(|v| v.as_str()).unwrap_or("");
            if cmd.contains("ui-bridge/sdk/page/navigate") {
                // Try to extract the URL from the -d JSON body in the curl command
                if let Some(d_pos) = cmd.find("-d") {
                    let after_d = &cmd[d_pos + 2..].trim_start();
                    // Find JSON body (single-quoted or double-quoted)
                    let body_str = if let Some(stripped) = after_d.strip_prefix('\'') {
                        stripped.split('\'').next()
                    } else if let Some(stripped) = after_d.strip_prefix('"') {
                        stripped.split('"').next()
                    } else {
                        after_d.split_whitespace().next()
                    };
                    body_str
                        .and_then(|body| serde_json::from_str::<Value>(body).ok())
                        .and_then(|parsed| {
                            parsed.get("url").and_then(|u| u.as_str()).map(String::from)
                        })
                } else {
                    None
                }
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
            if let Some(wd) = step.get("working_directory").and_then(|v| v.as_str()) {
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
/// - `command` steps with mode "test" and `playwright` test_type when SDK is connected
/// - `command` steps with mode "shell" hitting SDK endpoints without content validation
/// - Agentic steps that lack corresponding verification coverage
pub fn should_harden_verification(workflow: &UnifiedWorkflow) -> bool {
    let has_sdk_connect = {
        let json = serde_json::to_string(workflow).unwrap_or_default();
        json.contains("ui-bridge/sdk/connect")
    };

    let has_hardenable_steps = workflow.verification_steps.iter().any(|step| {
        let step_type = step.get("type").and_then(|v| v.as_str()).unwrap_or("");
        let mode = step.get("mode").and_then(|v| v.as_str()).unwrap_or("");

        match step_type {
            // Prompt steps are always candidates
            "prompt" => true,

            // Command steps with mode "test" are candidates when SDK is connected
            "command" if mode == "test" || step.get("test_type").is_some() => {
                if !has_sdk_connect {
                    return false;
                }
                let test_type = step.get("test_type").and_then(|v| v.as_str()).unwrap_or("");
                // Playwright UI tests can be replaced by SDK checks
                test_type == "playwright" || is_ui_verification_test(step)
            }

            // Command steps with mode "shell" that call SDK with weak assertions
            "command" if mode == "shell" && has_sdk_connect => {
                let cmd = step.get("command").and_then(|v| v.as_str()).unwrap_or("");
                if !cmd.contains("ui-bridge/sdk") {
                    return false;
                }
                // If using curl to SDK but not grepping for specific content, it's weak
                !cmd.contains("grep") && !cmd.contains("jq")
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
                t != "prompt"
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
    let description = step
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let name = step.get("name").and_then(|v| v.as_str()).unwrap_or("");

    let combined = format!("{} {} {}", command, description, name).to_lowercase();

    // UI-related keywords suggest this test verifies UI state
    combined.contains("ui")
        || combined.contains("element")
        || combined.contains("render")
        || combined.contains("visual")
        || combined.contains("page")
        || combined.contains("tab")
        || combined.contains("button")
        || combined.contains("component")
        || combined.contains("graph editor")
        || combined.contains("thumbnail")
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

    workflow
        .verification_steps
        .iter()
        .filter(|step| {
            let t = step.get("type").and_then(|v| v.as_str()).unwrap_or("");
            let mode = step.get("mode").and_then(|v| v.as_str()).unwrap_or("");
            match t {
                "prompt" => true,
                "command" if (mode == "test" || step.get("test_type").is_some()) && has_sdk => {
                    let tt = step.get("test_type").and_then(|v| v.as_str()).unwrap_or("");
                    tt == "playwright" || is_ui_verification_test(step)
                }
                "command" if mode == "shell" && has_sdk => {
                    let cmd = step.get("command").and_then(|v| v.as_str()).unwrap_or("");
                    cmd.contains("ui-bridge/sdk") && !cmd.contains("grep") && !cmd.contains("jq")
                }
                _ => false,
            }
        })
        .count()
}

// ============================================================================
// Prompt builder
// ============================================================================

fn build_hardener_prompt(
    workflow_json: &str,
    description: &str,
    app_context: &AppContext,
    conn: Option<&Connection>,
) -> String {
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
            prompt.push_str(&format!(
                "\n### Rule {}: {}\n\n{}\n",
                rule.rule_number, rule.title, rule.content
            ));
        }
    } else {
        // Fallback: hardcoded conversion rules
        prompt.push_str(
            r#"
### Rule 1: Convert `prompt` steps to deterministic equivalents

Only 3 step types are valid: `command`, `ui_bridge`, and `prompt`. Convert prompt steps to the appropriate type.

| Prompt check type | Convert to | Method |
|---|---|---|
| UI element presence/structure | `command` | curl to UI Bridge SDK endpoint, pipe to grep for content check |
| Content/text on page | `command` | curl to UI Bridge SDK `/ai/search`, pipe to grep for expected text |
| File existence | `command` | `check_type: "custom_command"` with `test -f <path>` |
| File content | `command` | `check_type: "custom_command"` with `grep -q <pattern> <file>` |
| Code quality (lint) | `command` | `check_type: "lint"` with appropriate command |
| Code quality (typecheck) | `command` | `check_type: "typecheck"` with appropriate command |
| API health/response | `command` | curl to endpoint, check exit code |
| UI assertion | `ui_bridge` | Use assert action with target and expected value |
| Subjective/qualitative | Keep as `prompt` | Cannot be made deterministic |
"#,
        );

        if app_context.has_sdk_connect {
            prompt.push_str(r#"
### Rule 2: Replace Playwright UI tests with UI Bridge SDK checks

When the UI Bridge SDK is connected (it IS connected in this workflow's setup), Playwright-based UI
verification tests should be converted to `command` steps (using curl) or `ui_bridge` steps. The SDK provides
direct programmatic access to registered UI elements without requiring a Playwright browser instance.

**Why:** Playwright tests require browser binaries (often missing), are slow, flaky, and test through
the browser rendering layer. The UI Bridge SDK communicates directly with the app's element registry,
making checks faster, more reliable, and always available.

**How to convert a Playwright test step to SDK-based steps:**

If a single Playwright test checks multiple things (e.g., "verify tabs exist AND thumbnails render AND
buttons work"), split it into multiple `command` or `ui_bridge` steps — one per distinct verification concern.
Each step should check one thing well rather than trying to replicate the entire test.

Common conversions:
| Playwright test checks... | SDK equivalent |
|---|---|
| Element exists on page | `command`: `curl -sf http://localhost:9876/ui-bridge/sdk/elements?contentOnly=true \| grep "elementId"` |
| Text content visible | `command`: `curl -sf -X POST http://localhost:9876/ui-bridge/sdk/ai/search -H "Content-Type: application/json" -d '{"text":"..."}' \| grep "expected-text"` |
| Tab/section present | `command`: `curl -sf http://localhost:9876/ui-bridge/sdk/elements?contentOnly=true \| grep "tab-name"` |
| Page loads without errors | `command`: `curl -sf http://localhost:9876/ui-bridge/sdk/snapshot \| grep "elements"` |
| Element state assertion | `ui_bridge`: Use assert action with target element and expected value |
| Element count/state | `command`: `curl -sf http://localhost:9876/ui-bridge/sdk/elements \| grep "expected-element"` |

**URL base for all SDK endpoints:** `http://localhost:9876/ui-bridge/sdk/`

Keep the original step's `id` on ONE of the replacement steps, and generate new UUIDs for additional steps.

**IMPORTANT — SDK capability limits.** Do NOT convert Playwright tests that involve any of these
interactions, because the UI Bridge SDK cannot perform them:
- **Keyboard shortcuts** (Ctrl+Z, Delete, Ctrl+S, etc.)
- **File upload dialogs**
- **Form submit/reset** (not implemented in SDK)
- **Multi-tab or multi-window** interactions
- **Screenshot pixel comparisons**

These tests MUST remain as `command` steps with `test_type: "playwright"`.

Note: **Drag-and-drop CAN be converted** to SDK. Use `POST /ui-bridge/sdk/ai/execute` with
`{"action":"drag","elementId":"<source>","params":{"target":{"elementId":"<target>"}}}` or
with `{"action":"drag","elementId":"<source>","params":{"targetPosition":{"x":N,"y":N}}}`.
"#);
        }

        if app_context.has_sdk_connect {
            prompt.push_str(r#"
### Rule 3: Strengthen weak SDK verification commands

If an existing `command` step calls a UI Bridge SDK endpoint via curl but only checks exit code (no grep),
add a pipe to `grep` to verify meaningful content. A successful curl to the SDK just means the
endpoint is reachable — it doesn't verify the UI state. SDK endpoints return 200 even for EMPTY results.

**Specific endpoint guidance:**

- **`POST /ui-bridge/sdk/ai/search`**: The response is `{"results": [...], "total": N}`.
  When `total` is 0, the search found NOTHING — but curl still succeeds!
  **FIX:** Pipe to `grep` with the expected element text (e.g., `| grep "Settings"`).

- **`GET /ui-bridge/sdk/elements`** or **`GET /ui-bridge/sdk/elements?contentOnly=true`**:
  Pipe to `grep "id"` at minimum (verifies elements exist). Better: grep for the expected element ID or text.

- **`GET /ui-bridge/sdk/snapshot`**: Pipe to `grep "elements"` (verifies snapshot has data).
  Better: grep for the expected page title or a known element's text content.

- **`POST /ui-bridge/sdk/discover`**: Pipe to `grep` with expected element attributes or text.

- **`GET /ui-bridge/sdk/ai/summary`** or **`GET /ui-bridge/sdk/ai/snapshot`**:
  Pipe to `grep` with a keyword from the expected page content.
"#);
        }

        if app_context.has_sdk_connect {
            prompt.push_str(r#"
### Rule 4: Inject page navigation before SDK verification checks

If the workflow's setup_steps include a page navigation step (curl POST to `/ui-bridge/sdk/page/navigate` or
a `ui_bridge` step with `action: "navigate"`), the verification phase MUST also navigate to that same URL
before any SDK element checks. This is because the agentic phase may navigate the browser away from the
target page (e.g., to documentation, error pages, or other app routes), and verification needs to be on the
correct page to check UI state.

**How to apply:**
1. Look in `setup_steps` for any step that navigates to a URL (curl to `/ui-bridge/sdk/page/navigate` or `ui_bridge` navigate action)
2. Extract the target URL
3. If the FIRST SDK-related step in `verification_steps` is NOT a navigation step, INSERT a new `command`
   step at the beginning of verification_steps:
   ```json
   {
     "id": "<new-uuid>",
     "type": "command",
     "phase": "verification",
     "name": "Navigate to target page",
     "command": "curl -sf -X POST http://localhost:9876/ui-bridge/sdk/page/navigate -H \"Content-Type: application/json\" -d \"{\\\"url\\\": \\\"<extracted-url>\\\"}\"",
     "fail_on_error": true
   }
   ```
   Or use a `ui_bridge` step with `action: "navigate"` and the target URL.
4. If verification already starts with a navigate step to the same URL, skip this rule.
"#);
        }

        prompt.push_str(r#"
### Rule 5: Ensure every agentic step has corresponding verification coverage

Examine EACH prompt step in `agentic_steps` and identify the distinct goals/features it describes.
Then check whether `verification_steps` has at least one deterministic step (`command` or `ui_bridge`)
that would FAIL if that specific goal was NOT implemented.

**Common gaps:**
- Agentic step says "implement drag-and-drop" but verification only checks tab existence
- Agentic step says "add thumbnails to state nodes" but no verification checks for thumbnails
- Agentic step says "add keyboard shortcuts" but no verification for keyboard functionality

**How to fix gaps:**
1. For each uncovered agentic goal, ADD a new `command` verification step (e.g., curl to SDK endpoint
   piped to grep for expected content) or `ui_bridge` step with assert action
2. Generate new UUIDs for added steps
3. If an agentic step has multiple distinct goals (e.g., "implement thumbnails AND drag-and-drop"),
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
            prompt
                .push_str("- Page snapshot: `GET http://localhost:9876/ui-bridge/sdk/snapshot`\n");
            prompt.push_str("- Execute action: `POST http://localhost:9876/ui-bridge/sdk/ai/execute` with body `{\"action\":\"click\",\"elementId\":\"id\"}`\n");
            if let Some(ref nav_url) = app_context.setup_navigate_url {
                prompt.push_str(&format!(
                    "- Setup navigates to: `{}` — verification should navigate here before SDK checks (Rule 4)\n",
                    nav_url
                ));
            }
        } else {
            prompt
                .push_str("No SDK connect step found — prefer file-based or API health checks.\n");
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
4. **Adding steps is allowed**: If a Playwright test step checks multiple things, you MAY replace it with multiple `command` or `ui_bridge` steps. You MAY also add NEW verification steps to cover uncovered agentic goals. Keep original `id`s on existing steps and generate new UUIDs for additions.
5. **Keep subjective prompts**: If a prompt step is genuinely subjective (e.g., "Is the UX intuitive?"), keep it as `prompt`
6. **Complete required fields**: Every converted step must have all required fields for its new type
7. **Only 3 step types**: All steps must use `command`, `ui_bridge`, or `prompt`. Do NOT output `api_request`, `check`, `test`, `gate`, or `spec` types.
8. **Command with check_type fields**: For check conversions, include `mode: "check"`, `check_type`, `command`, and `working_directory` on the `command` step
9. **Do not convert existing command+check_type steps**: Do NOT convert `command` steps that already have `check_type` set (lint, typecheck, etc.) — they are already deterministic
10. **SDK verification uses command+curl**: Use `command` steps with `mode: "shell"` and curl piped to grep for SDK-based verification, not `api_request`
11. **Always set mode on command steps**: Every `command` step must include a `mode` field (`shell`, `check`, `check_group`, or `test`) matching the fields present"#);
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

    // Build a map of original step IDs to their types/modes/names for comparison
    let orig_map: std::collections::HashMap<&str, (&str, &str, &str)> = original
        .verification_steps
        .iter()
        .filter_map(|s| {
            let id = s.get("id").and_then(|v| v.as_str())?;
            let t = s.get("type").and_then(|v| v.as_str()).unwrap_or("unknown");
            let m = s.get("mode").and_then(|v| v.as_str()).unwrap_or("");
            let n = s.get("name").and_then(|v| v.as_str()).unwrap_or("");
            Some((id, (t, m, n)))
        })
        .collect();

    // Check each hardened step against the original
    for hard_step in &hardened.verification_steps {
        let hard_id = hard_step.get("id").and_then(|v| v.as_str()).unwrap_or("");
        let hard_type = hard_step
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let hard_mode = hard_step.get("mode").and_then(|v| v.as_str()).unwrap_or("");

        if let Some(&(orig_type, orig_mode, orig_name)) = orig_map.get(hard_id) {
            // This step existed in the original — check if type or mode changed
            let type_changed = orig_type != hard_type;
            let mode_changed = orig_type == "command"
                && hard_type == "command"
                && !orig_mode.is_empty()
                && !hard_mode.is_empty()
                && orig_mode != hard_mode;

            if type_changed || mode_changed {
                let orig_label = if orig_mode.is_empty() {
                    orig_type.to_string()
                } else {
                    format!("{}:{}", orig_type, orig_mode)
                };
                let new_label = if hard_mode.is_empty() {
                    hard_type.to_string()
                } else {
                    format!("{}:{}", hard_type, hard_mode)
                };
                converted_count += 1;
                conversions.push(HardeningConversion {
                    step_id: hard_id.to_string(),
                    original_name: orig_name.to_string(),
                    original_type: orig_label.clone(),
                    new_type: new_label.clone(),
                    explanation: format!("Converted from {} to {}", orig_label, new_label),
                });
            } else if orig_type == "prompt" {
                kept_as_prompt_count += 1;
            }
        } else {
            // New step (from splitting) — count as a conversion
            let hard_name = hard_step.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let hard_label = if hard_mode.is_empty() {
                hard_type.to_string()
            } else {
                format!("{}:{}", hard_type, hard_mode)
            };
            converted_count += 1;
            conversions.push(HardeningConversion {
                step_id: hard_id.to_string(),
                original_name: format!("(split from parent) {}", hard_name),
                original_type: "split".to_string(),
                new_type: hard_label.clone(),
                explanation: format!(
                    "New {} step from splitting a multi-concern step",
                    hard_label
                ),
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
            enable_sweep: false,
            max_sweep_iterations: 5,
            generated_by_task_run_id: None,
            created_at: "2025-01-01T00:00:00Z".to_string(),
            updated_at: "2025-01-01T00:00:00Z".to_string(),
        }
    }

    fn make_sdk_workflow(verification_steps: Vec<Value>) -> UnifiedWorkflow {
        let mut w = make_test_workflow(verification_steps);
        w.setup_steps = vec![json!({
            "id": "setup-sdk",
            "type": "command",
            "mode": "shell",
            "command": "curl -X POST http://localhost:9876/ui-bridge/sdk/connect -H 'Content-Type: application/json' -d '{\"url\": \"http://localhost:3001\"}'"
        })];
        w
    }

    // === should_harden_verification tests ===

    #[test]
    fn test_should_harden_returns_false_for_all_deterministic() {
        let workflow = make_test_workflow(vec![
            json!({"id": "s1", "type": "command", "mode": "check", "check_type": "typecheck", "command": "npx tsc --noEmit"}),
            json!({"id": "s2", "type": "command", "mode": "shell", "command": "curl http://localhost:3001/health | grep ok"}),
        ]);
        assert!(!should_harden_verification(&workflow));
    }

    #[test]
    fn test_should_harden_returns_true_for_prompt_verification() {
        let workflow = make_test_workflow(vec![
            json!({"id": "s1", "type": "command", "mode": "check", "check_type": "typecheck", "command": "npx tsc --noEmit"}),
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
            json!({"id": "s1", "type": "command", "mode": "test", "test_type": "playwright", "command": "npx playwright test ui-check"}),
        ]);
        assert!(should_harden_verification(&workflow));
    }

    #[test]
    fn test_should_harden_ignores_playwright_without_sdk() {
        let workflow = make_test_workflow(vec![
            json!({"id": "s1", "type": "command", "mode": "test", "test_type": "playwright", "command": "npx playwright test"}),
        ]);
        assert!(!should_harden_verification(&workflow));
    }

    #[test]
    fn test_should_harden_detects_ui_test_with_sdk() {
        let workflow = make_sdk_workflow(vec![
            json!({"id": "s1", "type": "command", "mode": "test", "test_type": "custom_command", "name": "Verify page renders correctly", "command": "check-ui"}),
        ]);
        assert!(should_harden_verification(&workflow));
    }

    #[test]
    fn test_should_harden_detects_weak_sdk_shell_command() {
        let workflow = make_sdk_workflow(vec![json!({
            "id": "s1", "type": "command", "mode": "shell",
            "command": "curl http://localhost:9876/ui-bridge/sdk/elements"
        })]);
        assert!(should_harden_verification(&workflow));
    }

    #[test]
    fn test_should_harden_ignores_strong_sdk_shell_command() {
        let workflow = make_sdk_workflow(vec![json!({
            "id": "s1", "type": "command", "mode": "shell",
            "command": "curl http://localhost:9876/ui-bridge/sdk/elements | grep button-submit"
        })]);
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
        let hardened = make_test_workflow(vec![
            json!({"id": "a", "type": "command", "mode": "check", "check_type": "lint"}),
        ]);

        let error = validate_hardened_output(&original, &hardened);
        assert!(error.is_some());
        assert!(error.unwrap().contains("Step count decreased"));
    }

    #[test]
    fn test_validate_allows_step_count_increase_from_splitting() {
        let original = make_test_workflow(vec![
            json!({"id": "a", "type": "command", "mode": "test", "test_type": "playwright"}),
        ]);
        let hardened = make_test_workflow(vec![
            json!({"id": "a", "type": "command", "mode": "shell", "command": "curl http://localhost:9876/ui-bridge/sdk/elements | grep button"}),
            json!({"id": "new-1", "type": "command", "mode": "shell", "command": "curl http://localhost:9876/ui-bridge/sdk/ai/search | grep nav"}),
        ]);

        let error = validate_hardened_output(&original, &hardened);
        assert!(error.is_none());
    }

    #[test]
    fn test_validate_rejects_missing_original_id() {
        let original = make_test_workflow(vec![json!({"id": "step-1", "type": "prompt"})]);
        let hardened = make_test_workflow(vec![
            json!({"id": "step-2", "type": "command", "mode": "check", "check_type": "lint"}),
        ]);

        let error = validate_hardened_output(&original, &hardened);
        assert!(error.is_some());
        assert!(error.unwrap().contains("missing"));
    }

    #[test]
    fn test_validate_rejects_setup_modification() {
        let mut original = make_test_workflow(vec![json!({"id": "a", "type": "prompt"})]);
        original.setup_steps = vec![json!({"id": "setup-1", "type": "command", "mode": "shell"})];

        let mut hardened = make_test_workflow(vec![
            json!({"id": "a", "type": "command", "mode": "check", "check_type": "lint"}),
        ]);
        hardened.setup_steps = vec![
            json!({"id": "setup-1", "type": "command", "mode": "shell", "command": "curl http://example.com"}),
        ];

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
            json!({"id": "step-1", "type": "command", "mode": "shell", "command": "curl http://localhost:9876/ui-bridge/sdk/elements | grep button"}),
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
            json!({"id": "s3", "name": "Lint check", "type": "command", "mode": "check", "check_type": "lint"}),
        ]);
        let hardened = make_test_workflow(vec![
            json!({"id": "s1", "name": "Check code", "type": "command", "mode": "check", "check_type": "lint"}),
            json!({"id": "s2", "name": "Check UI", "type": "prompt"}),
            json!({"id": "s3", "name": "Lint check", "type": "command", "mode": "check", "check_type": "lint"}),
        ]);

        let summary = build_summary(&original, &hardened);
        assert_eq!(summary.converted_count, 1);
        assert_eq!(summary.kept_as_prompt_count, 1);
        assert_eq!(summary.conversions.len(), 1);
        assert_eq!(summary.conversions[0].step_id, "s1");
        assert_eq!(summary.conversions[0].original_type, "prompt");
        assert!(summary.conversions[0].new_type.contains("command"));
    }

    #[test]
    fn test_summary_tracks_mode_conversion() {
        let original = make_test_workflow(vec![
            json!({"id": "s1", "name": "Playwright UI test", "type": "command", "mode": "test", "test_type": "playwright"}),
        ]);
        let hardened = make_test_workflow(vec![
            json!({"id": "s1", "name": "Verify elements via SDK", "type": "command", "mode": "shell", "command": "curl http://localhost:9876/ui-bridge/sdk/elements | grep button"}),
        ]);

        let summary = build_summary(&original, &hardened);
        assert_eq!(summary.converted_count, 1);
        assert!(summary.conversions[0].original_type.contains("test"));
        assert!(summary.conversions[0].new_type.contains("shell"));
    }

    #[test]
    fn test_summary_tracks_split_steps() {
        let original = make_test_workflow(vec![
            json!({"id": "s1", "name": "Big Playwright test", "type": "command", "mode": "test", "test_type": "playwright"}),
        ]);
        let hardened = make_test_workflow(vec![
            json!({"id": "s1", "name": "Check elements", "type": "command", "mode": "shell", "command": "curl http://localhost:9876/ui-bridge/sdk/elements | grep button"}),
            json!({"id": "new-1", "name": "Check tabs", "type": "command", "mode": "shell", "command": "curl http://localhost:9876/ui-bridge/sdk/elements | grep tab"}),
            json!({"id": "new-2", "name": "Check content", "type": "command", "mode": "shell", "command": "curl http://localhost:9876/ui-bridge/sdk/elements | grep content"}),
        ]);

        let summary = build_summary(&original, &hardened);
        assert_eq!(summary.converted_count, 3); // 1 mode change + 2 new splits
        assert_eq!(summary.conversions.len(), 3);
    }

    // === AppContext tests ===

    #[test]
    fn test_app_context_detects_web_app() {
        let mut workflow = make_test_workflow(vec![]);
        workflow.setup_steps = vec![json!({
            "id": "s1", "type": "command", "mode": "shell",
            "command": "curl http://localhost:3001/api/test"
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
            "type": "command",
            "mode": "shell",
            "command": "curl -X POST http://localhost:9876/ui-bridge/sdk/page/navigate -H 'Content-Type: application/json' -d '{\"url\": \"http://localhost:3001/management\"}'"
        }));
        let ctx = AppContext::from_workflow(&workflow, "test");
        assert_eq!(
            ctx.setup_navigate_url.as_deref(),
            Some("http://localhost:3001/management")
        );
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
            json!({"id": "s1", "type": "command", "command": "curl -s http://localhost:9876/ui-bridge/sdk/ai/search", "mode": "shell"}),
        ]);
        let ctx = AppContext::from_workflow(&workflow, "test");
        let prompt = build_hardener_prompt("{}", "test", &ctx, None);
        assert!(prompt.contains("ai/search"));
        assert!(prompt.contains("total"));
        assert!(prompt.contains("grep"));
    }

    #[test]
    fn test_should_harden_detects_agentic_verification_gap() {
        // 3 agentic steps but only 2 deterministic verification steps → gap
        let mut workflow = make_test_workflow(vec![
            json!({"id": "s1", "type": "command", "mode": "check", "check_type": "typecheck", "command": "npx tsc --noEmit"}),
            json!({"id": "s2", "type": "command", "mode": "check", "check_type": "lint", "command": "npx eslint ."}),
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
            json!({"id": "s1", "type": "command", "mode": "check", "check_type": "typecheck", "command": "npx tsc --noEmit"}),
            json!({"id": "s2", "type": "command", "mode": "shell", "command": "curl http://localhost:3001/health | grep ok"}),
            json!({"id": "s3", "type": "command", "mode": "shell", "command": "curl http://localhost:3001/api/test | grep pass"}),
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
            json!({"id": "s1", "type": "command", "mode": "check", "check_type": "typecheck", "command": "npx tsc --noEmit"}),
        ]);
        workflow.agentic_steps =
            vec![json!({"id": "a1", "type": "prompt", "content": "Implement thumbnails"})];
        let ctx = AppContext::from_workflow(&workflow, "test");
        let prompt = build_hardener_prompt("{}", "test", &ctx, None);
        assert!(prompt.contains("Rule 5"));
        assert!(prompt.contains("agentic step"));
        assert!(prompt.contains("verification coverage"));
    }
}

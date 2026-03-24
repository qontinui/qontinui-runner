//! Helper and utility functions extracted from `phases.rs`.
//!
//! Contains free functions that support the phase executors but are not
//! phase executors themselves:
//!
//! - **Token usage tracking** — `record_phase_token_usage()`
//! - **LLM metrics conversion** — build_llm_metrics() for Opik integration
//! - **Reflection mode preamble** — `REFLECTION_MODE_PREAMBLE` constant
//! - **UI Bridge integration** — console error fetching, health checks, browser events, network failures
//! - **Environment readiness** — `check_environment_readiness()` and `EnvironmentReadinessResult`
//! - **Response mode execution** — `execute_prompt_response_mode()`, finding parsing/storage
//! - **Token estimation** — `estimate_tokens()`, `compute_embedding_sync()`

use tracing::{info, warn};

use crate::ai_router::TaskContext;
use crate::database::CheckpointDb;
use crate::doctor::DoctorHandle;
use crate::findings::storage as finding_storage;
use crate::findings::{FindingParser, ParsedFinding};
use crate::mcp::types::MCP_API_PORT;
use crate::commands::execution_reporting::LLMMetrics;
use crate::step_executor::ExecutionStepConfig;

// =============================================================================
// Token Usage Tracking
// =============================================================================

/// Record token usage for a phase to the database.
/// Silently ignores errors (best-effort tracking).
pub(super) fn record_phase_token_usage(
    db: &CheckpointDb,
    task_run_id: &str,
    phase: &str,
    stage_index: Option<u32>,
    iteration: Option<u32>,
    model_used: Option<&str>,
    provider_used: Option<&str>,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    duration_ms: Option<u64>,
) {
    let input = input_tokens.unwrap_or(0);
    let output = output_tokens.unwrap_or(0);
    // Only record if we have actual token data
    if input == 0 && output == 0 {
        return;
    }
    // Simple cost estimation (cents): rough per-token pricing
    // This is approximate — actual cost depends on model
    let cost_cents = 0u64; // Cost calculation deferred to a pricing table
    info!(
        "Recording phase token usage: task={}, phase={}, input={}, output={}",
        task_run_id, phase, input, output
    );
    if let Err(e) = db.create_phase_token_usage(
        task_run_id,
        phase,
        stage_index,
        iteration,
        model_used,
        provider_used,
        input,
        output,
        cost_cents,
        duration_ms,
    ) {
        warn!("Failed to record phase token usage: {}", e);
    }
}

/// Build LLM metrics for Opik integration from token usage data.
///
/// Returns `None` if both input and output tokens are zero (no meaningful data).
pub(super) fn build_llm_metrics(
    model: Option<&str>,
    provider: Option<&str>,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
) -> Option<LLMMetrics> {
    let input = input_tokens.unwrap_or(0) as i64;
    let output = output_tokens.unwrap_or(0) as i64;
    if input == 0 && output == 0 {
        return None;
    }
    Some(LLMMetrics {
        model: model.map(|s| s.to_string()),
        provider: provider.map(|s| s.to_string()),
        tokens_input: Some(input),
        tokens_output: Some(output),
        tokens_total: Some(input + output),
        cost_usd: None,
        generation_params: None,
    })
}

// =============================================================================
// Reflection Mode Preamble
// =============================================================================

/// Preamble injected into the agentic phase prompt when reflection mode is enabled.
/// Instructs the AI to investigate root causes before implementing fixes.
pub(super) const REFLECTION_MODE_PREAMBLE: &str = r#"## Reflection Mode: Investigate Before Fixing

IMPORTANT: Do NOT jump straight to fixing the failed checks. Follow this investigation protocol first.

### Phase 1: Root Cause Investigation

Before writing any fixes, investigate the failures thoroughly:

1. **Read the failing code paths end-to-end.** Don't just look at the line that errors — trace the call chain. Use subagents (the Task tool) to explore multiple code paths in parallel.

2. **Distinguish symptoms from root causes.** A check failing may be a symptom of:
   - A bug several layers deeper
   - A missing configuration or selector
   - An incorrect assumption in the check itself
   - An API contract mismatch between modules

3. **Check if the fix belongs at a different layer.** Ask yourself:
   - Am I about to add a workaround, or fix the actual problem?
   - Should this fix be in the library/framework rather than the application?
   - Would fixing this at the source prevent an entire class of similar failures?

4. **Research related code.** Use grep/glob to find:
   - Other callers of the same function — are they also affected?
   - Similar patterns elsewhere — is this a systemic inconsistency?
   - Related selector lists, configs, or constants that should be in sync

### Phase 2: Document Findings

Before implementing any fix, write a brief analysis:
- **Root cause:** What is the actual underlying issue
- **Proposed fix:** What to change and why it addresses the root cause, not just the symptom
- **Risk assessment:** What else might this change affect

### Phase 3: Implement Fix

Now implement the fix, prioritizing:
- Root-cause fixes over workarounds
- Fixes that prevent entire classes of failures over point fixes
- Minimal, targeted changes unless the architecture is fundamentally wrong"#;

// =============================================================================
// Console Error Fetching (UI Bridge)
// =============================================================================

/// Clear accumulated console errors from the UI Bridge (best-effort).
///
/// Called at the start of each verification phase so each iteration
/// only captures its own console errors.
pub(super) async fn clear_console_errors() {
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()
    {
        Ok(c) => c,
        Err(_) => return,
    };

    // Clear runner's own frontend console errors
    let control_url = format!(
        "http://127.0.0.1:{}/ui-bridge/control/console-errors/clear",
        MCP_API_PORT
    );
    let _ = client.post(&control_url).send().await;

    // Clear SDK app console errors (if connected)
    let sdk_url = format!(
        "http://127.0.0.1:{}/ui-bridge/sdk/console-errors/clear",
        MCP_API_PORT
    );
    let _ = client.post(&sdk_url).send().await;
}

/// Extract console errors from a JSON response body.
///
/// Handles both formats:
/// - Runner control: `{ "success": true, "data": { "errors": [...] } }`
/// - SDK proxy: `{ "success": true, "data": { "errors": [...] } }` or `{ "errors": [...] }`
pub(super) fn extract_console_errors_from_response(
    body: &serde_json::Value,
) -> Vec<serde_json::Value> {
    // Try { "data": { "errors": [...] } } first (control endpoint)
    body.get("data")
        .and_then(|d| d.get("errors"))
        .and_then(|v| v.as_array())
        .cloned()
        // Fallback: { "errors": [...] } (SDK proxy may return unwrapped)
        .or_else(|| body.get("errors").and_then(|v| v.as_array()).cloned())
        .unwrap_or_default()
}

/// Fetch accumulated console errors from both the runner frontend and SDK app.
///
/// This is best-effort — if endpoints aren't available (e.g., headless execution,
/// no SDK app connected), they silently return empty results.
pub(super) async fn fetch_console_errors_from_ui_bridge() -> Result<Vec<serde_json::Value>, String>
{
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| format!("Failed to create HTTP client: {}", e))?;

    let control_url = format!(
        "http://127.0.0.1:{}/ui-bridge/control/console-errors?limit=50",
        MCP_API_PORT
    );
    let sdk_url = format!(
        "http://127.0.0.1:{}/ui-bridge/sdk/console-errors?limit=50",
        MCP_API_PORT
    );

    // Fetch from both endpoints concurrently
    let (control_result, sdk_result) =
        tokio::join!(client.get(&control_url).send(), client.get(&sdk_url).send(),);

    let mut all_errors = Vec::new();

    // Extract control endpoint errors
    if let Ok(response) = control_result {
        if response.status().is_success() {
            if let Ok(body) = response.json::<serde_json::Value>().await {
                all_errors.extend(extract_console_errors_from_response(&body));
            }
        }
    }

    // Extract SDK endpoint errors
    if let Ok(response) = sdk_result {
        if response.status().is_success() {
            if let Ok(body) = response.json::<serde_json::Value>().await {
                all_errors.extend(extract_console_errors_from_response(&body));
            }
        }
    }

    Ok(all_errors)
}

/// Fetch the health status from the SDK app's UI Bridge.
///
/// Returns the health assessment (status, score, breakdown, top issue).
/// Best-effort — returns None if the SDK app isn't connected.
pub(super) async fn fetch_health_from_ui_bridge() -> Option<serde_json::Value> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .ok()?;

    let url = format!("http://127.0.0.1:{}/ui-bridge/sdk/health", MCP_API_PORT);

    let response = client.get(&url).send().await.ok()?;
    if !response.status().is_success() {
        return None;
    }

    let body: serde_json::Value = response.json().await.ok()?;

    // Extract the data field from { "success": true, "data": { ... } }
    body.get("data").cloned()
}

/// Fetch browser events from the SDK app's UI Bridge.
///
/// Returns deduplicated error-level events (console errors, React errors,
/// HMR failures, network errors, resource errors) since the given timestamp.
/// These provide richer context than console-errors alone.
pub(super) async fn fetch_browser_events_from_ui_bridge(since: u64) -> Vec<serde_json::Value> {
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
    {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };

    let url = format!(
        "http://127.0.0.1:{}/ui-bridge/sdk/browser-events?deduplicate=true&since={}&limit=50",
        MCP_API_PORT, since
    );

    let response = match client.get(&url).send().await {
        Ok(r) if r.status().is_success() => r,
        _ => return Vec::new(),
    };

    let body: serde_json::Value = match response.json().await {
        Ok(b) => b,
        Err(_) => return Vec::new(),
    };

    // With deduplicate=true, the response has data.deduplicated (grouped events)
    // and data.events (raw). We prefer deduplicated for conciseness.
    if let Some(deduped) = body
        .get("data")
        .and_then(|d| d.get("deduplicated"))
        .and_then(|v| v.as_array())
    {
        return deduped.clone();
    }

    // Fallback to raw events
    body.get("data")
        .and_then(|d| d.get("events"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default()
}

/// Fetch failed network requests from the SDK app's UI Bridge.
///
/// Returns HTTP requests that failed (4xx/5xx, timeouts, CORS) since the
/// given timestamp. Helps identify broken API calls during verification.
pub(super) async fn fetch_network_failures_from_ui_bridge(since: u64) -> Vec<serde_json::Value> {
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
    {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };

    let url = format!(
        "http://127.0.0.1:{}/ui-bridge/sdk/network-requests?failuresOnly=true&since={}&limit=20",
        MCP_API_PORT, since
    );

    let response = match client.get(&url).send().await {
        Ok(r) if r.status().is_success() => r,
        _ => return Vec::new(),
    };

    let body: serde_json::Value = match response.json().await {
        Ok(b) => b,
        Err(_) => return Vec::new(),
    };

    body.get("data")
        .and_then(|d| d.get("requests"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default()
}

// =============================================================================
// Environment Readiness Doctor
// =============================================================================

/// Result of the environment readiness check run before each verification iteration.
#[derive(Debug)]
pub struct EnvironmentReadinessResult {
    /// Whether the environment is ready for verification.
    pub ready: bool,
    /// Whether automated recovery was attempted and succeeded.
    pub recovery_attempted: bool,
    /// Summary of what was checked and any issues found.
    pub summary: String,
    /// If the environment is NOT ready, this contains context for the agentic phase
    /// so it knows the issue is environmental, not a code problem.
    pub env_failure_context: Option<String>,
}

/// Check environment readiness and attempt automated recovery before verification.
///
/// This "environment doctor" runs before each verification iteration to ensure:
/// 1. The runner API is responsive
/// 2. The UI Bridge SDK app is connected and reporting elements
/// 3. The app is healthy (not crashed or showing error overlays)
///
/// If issues are detected, it attempts automated recovery (reconnect SDK, refresh page)
/// before returning. This separates "is the environment working?" from "did the code
/// change work?" — preventing wasted agentic iterations on environment problems.
pub async fn check_environment_readiness(
    iteration: u32,
    workflow_name: &str,
) -> EnvironmentReadinessResult {
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(8))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return EnvironmentReadinessResult {
                ready: false,
                recovery_attempted: false,
                summary: format!("Failed to create HTTP client: {}", e),
                env_failure_context: Some(
                    "## Environment Issue\n\nCannot create HTTP client to check environment readiness."
                        .to_string(),
                ),
            };
        }
    };

    let base_url = format!("http://127.0.0.1:{}", MCP_API_PORT);
    let mut issues: Vec<String> = Vec::new();

    // ── Check 1: Runner API health ──
    let runner_ok = match client.get(format!("{}/health", base_url)).send().await {
        Ok(resp) if resp.status().is_success() => true,
        Ok(resp) => {
            issues.push(format!("Runner API returned status {}", resp.status()));
            false
        }
        Err(e) => {
            issues.push(format!("Runner API unreachable: {}", e));
            false
        }
    };

    if !runner_ok {
        // If the runner itself is down, nothing else will work
        let summary = format!(
            "ENV-DOCTOR (iter {}): Runner API is not responding — {}",
            iteration,
            issues.join("; ")
        );
        warn!("{}", summary);
        return EnvironmentReadinessResult {
            ready: false,
            recovery_attempted: false,
            summary,
            env_failure_context: Some(format!(
                "## Environment Issue — Runner Not Responding\n\n\
                 The runner API at {} is not responding. The verification steps \
                 will fail because they depend on the runner's UI Bridge endpoints.\n\n\
                 Issues: {}",
                base_url,
                issues.join(", ")
            )),
        };
    }

    // ── Check 2: SDK connection status ──
    let sdk_connected = match client
        .get(format!("{}/ui-bridge/sdk/status", base_url))
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            if let Ok(body) = resp.json::<serde_json::Value>().await {
                let connected = body
                    .get("data")
                    .and_then(|d| d.get("connected"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                if !connected {
                    issues.push("SDK app is not connected".to_string());
                }
                connected
            } else {
                issues.push("SDK status response not parseable".to_string());
                false
            }
        }
        _ => {
            issues.push("SDK status endpoint not available".to_string());
            false
        }
    };

    // ── Check 3: SDK app health (only if connected) ──
    let mut app_healthy = true;
    if sdk_connected {
        if let Some(health) = fetch_health_from_ui_bridge().await {
            let status = health
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            if status == "broken" {
                let summary = health
                    .get("summary")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown error");
                issues.push(format!("SDK app is broken: {}", summary));
                app_healthy = false;
            }
        }
    }

    // ── If everything is fine, return immediately ──
    if issues.is_empty() {
        return EnvironmentReadinessResult {
            ready: true,
            recovery_attempted: false,
            summary: format!(
                "ENV-DOCTOR (iter {}): Environment ready (runner OK, SDK connected, app healthy)",
                iteration
            ),
            env_failure_context: None,
        };
    }

    // ── Automated Recovery ──
    info!(
        "ENV-DOCTOR (iter {}): Issues detected: {:?} — attempting recovery for '{}'",
        iteration, issues, workflow_name
    );
    let mut recovery_actions: Vec<String> = Vec::new();

    // Recovery 1: If SDK not connected, try reconnecting
    if !sdk_connected {
        info!("ENV-DOCTOR: Attempting SDK reconnect...");
        // Try to get the last known connected URL from connections list
        let reconnect_url = match client
            .get(format!("{}/ui-bridge/sdk/connections", base_url))
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => {
                if let Ok(body) = resp.json::<serde_json::Value>().await {
                    body.get("data")
                        .and_then(|d| d.as_array())
                        .and_then(|arr| arr.first())
                        .and_then(|c| c.get("url"))
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                } else {
                    None
                }
            }
            _ => None,
        };

        if let Some(url) = reconnect_url {
            let connect_body = serde_json::json!({ "url": url });
            match client
                .post(format!("{}/ui-bridge/sdk/connect", base_url))
                .json(&connect_body)
                .send()
                .await
            {
                Ok(resp) if resp.status().is_success() => {
                    recovery_actions.push(format!("Reconnected SDK to {}", url));
                    info!("ENV-DOCTOR: SDK reconnect succeeded to {}", url);
                    // Wait for WebSocket to stabilize
                    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                }
                Ok(resp) => {
                    recovery_actions
                        .push(format!("SDK reconnect failed (status {})", resp.status()));
                    warn!(
                        "ENV-DOCTOR: SDK reconnect failed with status {}",
                        resp.status()
                    );
                }
                Err(e) => {
                    recovery_actions.push(format!("SDK reconnect failed: {}", e));
                    warn!("ENV-DOCTOR: SDK reconnect failed: {}", e);
                }
            }
        } else {
            recovery_actions.push("No previous SDK connection URL found for reconnect".to_string());
            warn!("ENV-DOCTOR: No previous connection URL found, cannot auto-reconnect");
        }
    }

    // Recovery 2: If app is broken (but connected), try page refresh
    if sdk_connected && !app_healthy {
        info!("ENV-DOCTOR: App is broken, attempting page refresh...");
        match client
            .post(format!("{}/ui-bridge/sdk/page/refresh", base_url))
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => {
                recovery_actions.push("Refreshed page to recover from broken state".to_string());
                info!("ENV-DOCTOR: Page refresh succeeded, waiting for stabilization...");
                // Wait for page to reload and WebSocket to reconnect
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            }
            _ => {
                recovery_actions.push("Page refresh failed".to_string());
                warn!("ENV-DOCTOR: Page refresh failed");
            }
        }
    }

    // ── Post-recovery verification ──
    let post_recovery_ok = if !recovery_actions.is_empty() {
        // Re-check SDK status after recovery attempts
        let recheck = match client
            .get(format!("{}/ui-bridge/sdk/status", base_url))
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => {
                if let Ok(body) = resp.json::<serde_json::Value>().await {
                    body.get("data")
                        .and_then(|d| d.get("connected"))
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false)
                } else {
                    false
                }
            }
            _ => false,
        };

        // Also recheck health
        let recheck_healthy = if recheck {
            fetch_health_from_ui_bridge()
                .await
                .map(|h| {
                    h.get("status")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown")
                        != "broken"
                })
                .unwrap_or(true) // No health data = assume OK
        } else {
            false
        };

        recheck && recheck_healthy
    } else {
        false
    };

    if post_recovery_ok {
        let summary = format!(
            "ENV-DOCTOR (iter {}): Recovery succeeded — {} (original issues: {})",
            iteration,
            recovery_actions.join("; "),
            issues.join("; ")
        );
        info!("{}", summary);
        EnvironmentReadinessResult {
            ready: true,
            recovery_attempted: true,
            summary,
            env_failure_context: None,
        }
    } else {
        let summary = format!(
            "ENV-DOCTOR (iter {}): Environment NOT ready — issues: {}, recovery: {}",
            iteration,
            issues.join("; "),
            if recovery_actions.is_empty() {
                "none attempted".to_string()
            } else {
                recovery_actions.join("; ")
            }
        );
        warn!("{}", summary);
        EnvironmentReadinessResult {
            ready: false,
            recovery_attempted: !recovery_actions.is_empty(),
            summary,
            env_failure_context: Some(format!(
                "## Environment Issue (Pre-Verification)\n\n\
                 The environment is not ready for UI Bridge verification assertions.\n\n\
                 **Issues detected:**\n{}\n\n\
                 **Recovery actions taken:**\n{}\n\n\
                 **Impact:** Verification steps that query UI Bridge SDK endpoints will fail \
                 due to environment issues, NOT due to code problems. The agentic phase should \
                 focus on fixing the environment (ensuring the app is running, SDK is connected, \
                 and the correct page is loaded) before making code changes.",
                issues
                    .iter()
                    .map(|i| format!("- {}", i))
                    .collect::<Vec<_>>()
                    .join("\n"),
                if recovery_actions.is_empty() {
                    "- None attempted".to_string()
                } else {
                    recovery_actions
                        .iter()
                        .map(|a| format!("- {}", a))
                        .collect::<Vec<_>>()
                        .join("\n")
                }
            )),
        }
    }
}

// =============================================================================
// UI-Focused Workflow Auto-Connect
// =============================================================================

/// Keywords that indicate a workflow is UI-focused and may need the SDK connected.
const UI_WORKFLOW_KEYWORDS: &[&str] = &[
    "ui bridge",
    "dropdown",
    "style",
    "visual",
    "contrast",
    "design audit",
    "css",
    "theme",
];

/// Returns true if the workflow name suggests it is UI-focused.
pub fn is_ui_focused_workflow(workflow_name: &str) -> bool {
    let lower = workflow_name.to_lowercase();
    UI_WORKFLOW_KEYWORDS
        .iter()
        .any(|keyword| lower.contains(keyword))
}

/// Attempt to auto-connect the SDK for UI-focused workflows.
///
/// If the workflow name contains UI-related keywords and the SDK is not connected,
/// opens the runner's UI Bridge States page in the system browser and waits up to
/// 5 seconds for the SDK connection to establish.
///
/// Falls back gracefully if connection is still unavailable.
pub async fn try_auto_connect_sdk_for_ui_workflow(workflow_name: &str) {
    if !is_ui_focused_workflow(workflow_name) {
        return;
    }

    let base_url = format!("http://127.0.0.1:{}", MCP_API_PORT);
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
    {
        Ok(c) => c,
        Err(_) => return,
    };

    // Check if SDK is already connected
    let status_url = format!("{}/ui-bridge/sdk/status", base_url);
    let already_connected = match client.get(&status_url).send().await {
        Ok(resp) => {
            if let Ok(body) = resp.json::<serde_json::Value>().await {
                body.get("data")
                    .and_then(|d| d.get("connected"))
                    .and_then(|c| c.as_bool())
                    .unwrap_or(false)
            } else {
                false
            }
        }
        Err(_) => false,
    };

    if already_connected {
        info!(
            "SDK already connected for UI-focused workflow '{}'",
            workflow_name
        );
        return;
    }

    info!(
        "UI-focused workflow '{}' detected — attempting to open UI Bridge States page",
        workflow_name
    );

    // Try to open the runner's UI Bridge States page in the system browser
    let states_url = format!("{}/ui-bridge/states", base_url);
    if let Err(e) = open::that(&states_url) {
        warn!(
            "Failed to open UI Bridge States page in browser: {} — SDK auto-connect skipped",
            e
        );
        return;
    }

    // Wait up to 5 seconds for SDK connection
    for i in 0..10 {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        if let Ok(resp) = client.get(&status_url).send().await {
            if let Ok(body) = resp.json::<serde_json::Value>().await {
                let connected = body
                    .get("data")
                    .and_then(|d| d.get("connected"))
                    .and_then(|c| c.as_bool())
                    .unwrap_or(false);
                if connected {
                    info!(
                        "SDK connected after {}ms for UI-focused workflow '{}'",
                        (i + 1) * 500,
                        workflow_name
                    );
                    return;
                }
            }
        }
    }

    warn!(
        "SDK did not connect within 5s for UI-focused workflow '{}' — continuing without SDK",
        workflow_name
    );
}

// =============================================================================
// Prompt Response Mode Helper
// =============================================================================

/// Result of a prompt-response mode execution, including token usage.
pub(super) struct PromptResponseResult {
    /// The AI output text.
    pub output: String,
    /// Input tokens consumed (API providers only).
    pub input_tokens: Option<u64>,
    /// Output tokens generated (API providers only).
    pub output_tokens: Option<u64>,
}

/// Execute a single prompt step in "response" mode.
///
/// This runs a simple prompt->response AI call instead of a full Claude CLI session.
/// Used for meta-workflows and other cases where a full session is overkill.
///
/// Findings ([FINDING:...] markers) in the AI response are parsed and stored
/// in the database. Finding markers are stripped from the output before saving
/// to output_path so the saved artifact contains clean content.
pub(super) async fn execute_prompt_response_mode(
    step: &ExecutionStepConfig,
    db: &CheckpointDb,
    task_run_id: Option<&str>,
    doctor_handle: Option<DoctorHandle>,
    model_override: Option<String>,
    provider_override: Option<String>,
    temperature_override: Option<f32>,
    max_tokens_override: Option<u32>,
    fallback_model: Option<String>,
    fallback_provider: Option<String>,
) -> Result<PromptResponseResult, String> {
    // Build the prompt content
    let mut prompt = step.prompt_content.clone().unwrap_or_default();

    // If input_path is specified, read the file and append to prompt.
    // Missing input files are treated as warnings — the input is supplementary context
    // from a prior step that may have failed or been skipped (e.g., non-required spec step).
    // This allows the step to proceed with its own prompt content.
    if let Some(ref input_path) = step.input_path {
        match std::fs::read_to_string(input_path) {
            Ok(content) => {
                prompt = format!(
                    "{}\n\n## Input File Content (Supplementary Context)\n\n{}\n\n---\nREMINDER: The above is supplementary context from a prior step. Stay focused on your primary task and output format.",
                    prompt, content
                );
            }
            Err(e) => {
                tracing::warn!(
                    "Input file '{}' not found ({}), proceeding without supplementary context",
                    input_path,
                    e
                );
            }
        }
    }

    if prompt.trim().is_empty() {
        return Err("Prompt content is empty for response mode step".to_string());
    }

    // Build task context for AI routing
    let task_context = TaskContext::from_prompt(&prompt);

    // Run in blocking task since run_prompt_with_model_override is sync
    let result = tokio::task::spawn_blocking(move || {
        crate::ai_provider::run_prompt_with_model_override(
            &prompt,
            &task_context,
            doctor_handle.as_ref(),
            model_override.as_deref(),
            provider_override.as_deref(),
            temperature_override,
            max_tokens_override,
            fallback_model.as_deref(),
            fallback_provider.as_deref(),
        )
    })
    .await
    .map_err(|e| format!("Prompt execution task panicked: {}", e))?;

    if !result.success {
        return Err(format!(
            "AI prompt failed: {}",
            result.error.unwrap_or_else(|| "Unknown error".to_string())
        ));
    }

    let input_tokens = result.input_tokens;
    let output_tokens = result.output_tokens;
    let output_text = result.output;

    // Detect empty AI response — Claude CLI can exit 0 with no output
    if output_text.trim().is_empty() {
        return Err(
            "AI returned an empty response. The CLI process exited successfully but produced no output. \
             This can happen if the prompt was too long, the model timed out, or there was a transient API error. \
             Try running again."
                .to_string(),
        );
    }

    // Detect AI misunderstanding: if the response is mostly asking for permission or
    // clarification, the AI likely interpreted its instructions as a task to execute
    // rather than a prompt to respond to. This is a common failure mode for meta-workflows.
    {
        let lower = output_text.to_lowercase();
        let permission_patterns = [
            "could you please approve",
            "i need your approval",
            "can you confirm",
            "could you approve",
            "please approve the",
            "i need permission",
            "could you please confirm",
        ];
        let question_density = output_text.matches('?').count();
        let is_short = output_text.trim().len() < 500;
        let has_permission_pattern = permission_patterns.iter().any(|p| lower.contains(p));

        if has_permission_pattern && is_short {
            warn!(
                "Response mode: AI output appears to be asking for permission ({} bytes, {} questions). \
                 This suggests the AI misinterpreted its role. Treating as failure.",
                output_text.len(),
                question_density
            );
            return Err(format!(
                "AI misinterpreted its task and asked for permission instead of producing the expected output. \
                 This typically happens when the task description is phrased as an imperative instruction. \
                 The AI should be generating content, not executing actions. Output: {}",
                &output_text[..output_text.len().min(200)]
            ));
        }
    }

    // Parse findings from the AI response and store them
    let findings = parse_findings_from_response(&output_text);
    if !findings.is_empty() {
        if let Some(task_id) = task_run_id {
            store_parsed_findings(db, task_id, &findings);
        } else {
            info!(
                "Response mode: found {} findings but no task_run_id to store them",
                findings.len()
            );
        }
    }

    // Strip finding markers from the output before saving to file
    let clean_output = if findings.is_empty() {
        output_text.clone()
    } else {
        crate::summary_generator::strip_output_markers(&output_text)
    };

    // Write to output_path if specified (using clean output without finding markers)
    if let Some(ref output_path) = step.output_path {
        // If the output path is a .json file, extract JSON from the response.
        // AI models often include explanatory text before/after JSON output.
        let write_content = if output_path.ends_with(".json") {
            let extracted = crate::workflow_generation::extract_json_from_response(&clean_output);
            tracing::info!(
                "Response mode: extracted JSON ({} bytes) from response ({} bytes)",
                extracted.len(),
                clean_output.len()
            );
            if extracted.trim().is_empty() {
                return Err(format!(
                    "AI response ({} bytes) did not contain valid JSON for output file '{}'",
                    clean_output.len(),
                    output_path
                ));
            }
            extracted
        } else {
            clean_output.clone()
        };

        // Ensure parent directory exists
        if let Some(parent) = std::path::Path::new(output_path).parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                return Err(format!("Failed to create output directory: {}", e));
            }
        }
        if let Err(e) = std::fs::write(output_path, &write_content) {
            return Err(format!(
                "Failed to write output to '{}': {}",
                output_path, e
            ));
        }
        tracing::info!(
            "Response mode: wrote {} bytes to '{}'",
            write_content.len(),
            output_path
        );
    }

    Ok(PromptResponseResult {
        output: output_text,
        input_tokens,
        output_tokens,
    })
}

/// Parse [FINDING:...] markers from a response-mode AI output.
///
/// Uses FindingParser to extract structured findings from the full text.
pub(super) fn parse_findings_from_response(text: &str) -> Vec<ParsedFinding> {
    let mut parser = FindingParser::new();
    let mut findings = Vec::new();

    for line in text.lines() {
        if let Some(finding) = parser.process_line(line) {
            findings.push(finding);
        }
    }

    if !findings.is_empty() {
        info!(
            "Response mode: parsed {} findings from AI output",
            findings.len()
        );
    }

    findings
}

/// Store parsed findings in the database for a given task run.
pub(super) fn store_parsed_findings(
    db: &CheckpointDb,
    task_run_id: &str,
    findings: &[ParsedFinding],
) {
    let conn = match db.connection() {
        Ok(c) => c,
        Err(e) => {
            warn!(
                "Failed to get database connection for storing findings: {}",
                e
            );
            return;
        }
    };

    for (idx, parsed) in findings.iter().enumerate() {
        let session_num = (idx + 1) as u32;
        match finding_storage::insert_finding(&conn, task_run_id, session_num, parsed) {
            Ok(finding) => {
                info!(
                    "Response mode: stored finding [{}:{}] '{}'",
                    finding.category.as_str(),
                    finding.severity.as_str(),
                    finding.title
                );
            }
            Err(e) => {
                warn!("Failed to store finding from response mode: {}", e);
            }
        }
    }
}

// =============================================================================
// Token Budget Helpers
// =============================================================================

/// Rough token estimate: ~4 chars per token for English text.
///
/// This is intentionally approximate -- we don't need a real tokenizer here.
/// The goal is to prevent context from growing unbounded, not to hit an
/// exact token count.
pub(super) fn estimate_tokens(text: &str) -> usize {
    text.len() / 4
}

/// Compute a text embedding synchronously using an isolated async runtime.
///
/// Creates a fresh single-threaded tokio runtime on a dedicated OS thread,
/// runs an async reqwest call inside it, then tears it down cleanly.
/// This avoids the "cannot drop a runtime in a context where blocking is
/// not allowed" panic that occurred when reqwest::blocking created its own
/// internal runtime inside an existing tokio async context (observed on Windows).
pub(super) fn compute_embedding_sync(text: &str) -> Result<Vec<f32>, String> {
    #[derive(serde::Serialize)]
    struct Req {
        text: String,
        model: String,
    }
    #[derive(serde::Deserialize)]
    struct Resp {
        embedding: Vec<f32>,
    }

    let truncated = if text.len() > 2000 {
        text[..2000].to_string()
    } else {
        text.to_string()
    };

    // Spawn a dedicated OS thread that creates its own single-threaded tokio
    // runtime. This guarantees no tokio thread-local state leaks from the
    // parent runtime, which was the root cause of the Windows crash.
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| format!("Failed to build isolated runtime: {}", e))?;

        rt.block_on(async {
            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .map_err(|e| format!("Failed to build HTTP client: {}", e))?;

            let response = client
                .post("http://127.0.0.1:8001/api/embeddings/compute-text")
                .json(&Req {
                    text: truncated,
                    model: "minilm".to_string(),
                })
                .send()
                .await
                .map_err(|e| format!("Embedding API request failed: {}", e))?;

            if !response.status().is_success() {
                return Err(format!("Embedding API returned {}", response.status()));
            }

            let resp: Resp = response
                .json()
                .await
                .map_err(|e| format!("Failed to parse embedding response: {}", e))?;

            Ok(resp.embedding)
        })
    })
    .join()
    .map_err(|_| "Embedding thread join failed".to_string())?
}

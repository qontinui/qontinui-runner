//! Health monitoring and UI context functions for the agentic verification loop.
//!
//! Extracted from `loop_controller.rs` — provides UI Bridge snapshot fetching,
//! health baseline capture, regression detection, and resume context building.

use std::sync::Arc;

use tracing::info;

use crate::database::CheckpointDb;
use crate::mcp::types::MCP_API_PORT;
use crate::str_utils::truncate_str;

// =============================================================================
// Verifier UI Context (Snapshot, Console Errors, App Health)
// =============================================================================

/// Fetch live UI Bridge data for the agentic verification loop's verifier.
///
/// Concurrently fetches the UI snapshot, console errors, and app health status.
/// Each fetch is gated by its corresponding config flag and is best-effort:
/// failures are silently ignored (the SDK app may not be connected).
pub async fn fetch_verifier_ui_context(
    use_screenshots: bool,
    include_console_errors: bool,
    include_app_health: bool,
) -> String {
    if !use_screenshots && !include_console_errors && !include_app_health {
        return String::new();
    }

    let port = MCP_API_PORT;

    // Build futures for each data source (no-ops when disabled)
    let snapshot_fut = async {
        if !use_screenshots {
            return None;
        }
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .ok()?;
        let url = format!("http://127.0.0.1:{}/ui-bridge/sdk/control/snapshot", port);
        let resp = client.get(&url).send().await.ok()?;
        if !resp.status().is_success() {
            return None;
        }
        resp.json::<serde_json::Value>().await.ok()
    };

    let console_fut = async {
        if !include_console_errors {
            return Vec::new();
        }
        let client = match reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
        {
            Ok(c) => c,
            Err(_) => return Vec::new(),
        };
        let control_url = format!(
            "http://127.0.0.1:{}/ui-bridge/control/console-errors?limit=50",
            port
        );
        let sdk_url = format!(
            "http://127.0.0.1:{}/ui-bridge/sdk/console-errors?limit=50",
            port
        );
        let (control_result, sdk_result) =
            tokio::join!(client.get(&control_url).send(), client.get(&sdk_url).send(),);
        let mut all_errors: Vec<serde_json::Value> = Vec::new();
        for response in [control_result, sdk_result].into_iter().flatten() {
            if response.status().is_success() {
                if let Ok(body) = response.json::<serde_json::Value>().await {
                    if let Some(errors) = body.get("errors").and_then(|v| v.as_array()) {
                        all_errors.extend(errors.iter().cloned());
                    } else if let Some(data) = body.get("data") {
                        if let Some(errors) = data.get("errors").and_then(|v| v.as_array()) {
                            all_errors.extend(errors.iter().cloned());
                        }
                    }
                }
            }
        }
        all_errors
    };

    let health_fut = async {
        if !include_app_health {
            return None;
        }
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .ok()?;
        let url = format!("http://127.0.0.1:{}/ui-bridge/sdk/health", port);
        let resp = client.get(&url).send().await.ok()?;
        if !resp.status().is_success() {
            return None;
        }
        let body: serde_json::Value = resp.json().await.ok()?;
        body.get("data").cloned()
    };

    // Fetch all concurrently
    let (snapshot, console_errors, health) = tokio::join!(snapshot_fut, console_fut, health_fut);

    let mut sections = String::new();

    // ── Format snapshot ──
    if let Some(snap) = snapshot {
        sections.push_str("\n\n## Current UI State (Snapshot)\n");

        // Page context
        if let Some(page) = snap.get("pageContext") {
            if let Some(url) = page.get("url").and_then(|v| v.as_str()) {
                sections.push_str(&format!("**Page URL:** {}\n", url));
            }
            if let Some(title) = page.get("title").and_then(|v| v.as_str()) {
                sections.push_str(&format!("**Page Title:** {}\n", title));
            }
            if let Some(route) = page.get("route").and_then(|v| v.as_str()) {
                sections.push_str(&format!("**Route:** {}\n", route));
            }
        }

        // Viewport
        if let Some(viewport) = snap.get("viewport") {
            if let (Some(w), Some(h)) = (
                viewport.get("width").and_then(|v| v.as_u64()),
                viewport.get("height").and_then(|v| v.as_u64()),
            ) {
                sections.push_str(&format!("**Viewport:** {}x{}\n", w, h));
            }
        }

        // Error summary
        if let Some(errors) = snap.get("errorSummary") {
            let count = errors.get("count").and_then(|v| v.as_u64()).unwrap_or(0);
            if count > 0 {
                sections.push_str(&format!("**Errors:** {} error(s) detected\n", count));
                if let Some(items) = errors.get("errors").and_then(|v| v.as_array()) {
                    for item in items.iter().take(5) {
                        if let Some(msg) = item.get("message").and_then(|v| v.as_str()) {
                            sections.push_str(&format!("  - {}\n", msg));
                        }
                    }
                }
            }
        }

        // Modals
        if let Some(modals) = snap.get("modals").and_then(|v| v.as_array()) {
            if !modals.is_empty() {
                sections.push_str(&format!("**Active Modals:** {}\n", modals.len()));
                for modal in modals.iter().take(3) {
                    if let Some(title) = modal.get("title").and_then(|v| v.as_str()) {
                        sections.push_str(&format!("  - {}\n", title));
                    }
                }
            }
        }

        // Toasts
        if let Some(toasts) = snap.get("toasts").and_then(|v| v.as_array()) {
            if !toasts.is_empty() {
                sections.push_str(&format!("**Active Toasts:** {}\n", toasts.len()));
                for toast in toasts.iter().take(3) {
                    let msg = toast
                        .get("message")
                        .and_then(|v| v.as_str())
                        .unwrap_or("(no message)");
                    let level = toast
                        .get("type")
                        .or_else(|| toast.get("level"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("info");
                    sections.push_str(&format!("  - [{}] {}\n", level, msg));
                }
            }
        }

        // Interactive elements (summarized)
        if let Some(elements) = snap.get("elements").and_then(|v| v.as_array()) {
            let interactive: Vec<_> = elements
                .iter()
                .filter(|el| {
                    let el_type = el.get("type").and_then(|v| v.as_str()).unwrap_or("");
                    matches!(
                        el_type,
                        "button" | "input" | "select" | "textarea" | "link" | "checkbox" | "radio"
                    )
                })
                .collect();

            if !interactive.is_empty() {
                sections.push_str(&format!(
                    "**Interactive Elements:** {} total\n",
                    interactive.len()
                ));
                for el in interactive.iter().take(20) {
                    let el_type = el.get("type").and_then(|v| v.as_str()).unwrap_or("?");
                    let label = el
                        .get("label")
                        .and_then(|v| v.as_str())
                        .or_else(|| {
                            el.get("state")
                                .and_then(|s| s.get("text"))
                                .and_then(|v| v.as_str())
                        })
                        .unwrap_or("(unlabeled)");
                    let state = el.get("state");
                    let visible = state
                        .and_then(|s| s.get("visible"))
                        .and_then(|v| v.as_bool())
                        .unwrap_or(true);
                    let enabled = state
                        .and_then(|s| s.get("enabled"))
                        .and_then(|v| v.as_bool())
                        .unwrap_or(true);
                    let state_flags = match (visible, enabled) {
                        (true, true) => "",
                        (true, false) => " [disabled]",
                        (false, true) => " [hidden]",
                        (false, false) => " [hidden, disabled]",
                    };
                    sections.push_str(&format!("  - [{}] {}{}\n", el_type, label, state_flags));
                }
                if interactive.len() > 20 {
                    sections.push_str(&format!("  - ... and {} more\n", interactive.len() - 20));
                }
            }
        }
    }

    // ── Format console errors ──
    if !console_errors.is_empty() {
        sections.push_str(&format!(
            "\n\n## Console Errors ({} total)\n",
            console_errors.len()
        ));
        for (i, err) in console_errors.iter().take(10).enumerate() {
            let msg = err
                .get("message")
                .and_then(|v| v.as_str())
                .or_else(|| err.as_str())
                .unwrap_or("(unknown error)");
            let source = err.get("source").and_then(|v| v.as_str()).unwrap_or("");
            if source.is_empty() {
                sections.push_str(&format!("{}. {}\n", i + 1, msg));
            } else {
                sections.push_str(&format!("{}. {} ({})\n", i + 1, msg, source));
            }
        }
        if console_errors.len() > 10 {
            sections.push_str(&format!(
                "... and {} more errors\n",
                console_errors.len() - 10
            ));
        }
    }

    // ── Format app health ──
    if let Some(health_data) = health {
        sections.push_str("\n\n## App Health\n");
        let status = health_data
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let score = health_data
            .get("score")
            .and_then(|v| v.as_u64())
            .map(|s| format!("{}", s))
            .unwrap_or_else(|| "N/A".to_string());
        sections.push_str(&format!("**Status:** {} (score: {})\n", status, score));

        if let Some(summary) = health_data.get("summary").and_then(|v| v.as_str()) {
            sections.push_str(&format!("**Summary:** {}\n", summary));
        }
        if let Some(top_issue) = health_data.get("topIssue") {
            if let Some(msg) = top_issue.get("message").and_then(|v| v.as_str()) {
                let severity = top_issue
                    .get("severity")
                    .and_then(|v| v.as_str())
                    .unwrap_or("error");
                sections.push_str(&format!("**Top Issue:** [{}] {}\n", severity, msg));
            }
        }
        if let Some(breakdown) = health_data.get("breakdown").and_then(|v| v.as_object()) {
            sections.push_str("**Breakdown:**\n");
            for (category, detail) in breakdown {
                let cat_status = detail
                    .get("status")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                let cat_score = detail
                    .get("score")
                    .and_then(|v| v.as_u64())
                    .map(|s| format!("{}", s))
                    .unwrap_or_else(|| "?".to_string());
                sections.push_str(&format!(
                    "  - {}: {} (score: {})\n",
                    category, cat_status, cat_score
                ));
            }
        }
    }

    sections
}

// =============================================================================
// Health Baseline & Regression Detection (UI Bridge)
// =============================================================================

/// Lightweight health baseline captured before the agentic phase.
/// Used to detect if the AI's changes degraded app health.
#[derive(Debug)]
pub struct HealthBaseline {
    pub status: String,
    pub score: u64,
    pub error_count: u64,
}

/// Capture a health baseline from the SDK app before the agentic phase.
/// Returns None if the SDK app isn't connected (best-effort).
pub async fn fetch_pre_agentic_health_baseline() -> Option<HealthBaseline> {
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
    let data = body.get("data")?;

    Some(HealthBaseline {
        status: data
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string(),
        score: data.get("score").and_then(|v| v.as_u64()).unwrap_or(100),
        error_count: data
            .get("breakdown")
            .map(|b| {
                let crashes = b.get("crashes").and_then(|v| v.as_u64()).unwrap_or(0);
                let errors = b.get("errors").and_then(|v| v.as_u64()).unwrap_or(0);
                crashes + errors
            })
            .unwrap_or(0),
    })
}

/// Compare post-agentic health with the pre-agentic baseline.
/// Returns a warning string if the AI's changes degraded health.
pub async fn detect_health_regression(baseline: &Option<HealthBaseline>) -> Option<String> {
    let baseline = baseline.as_ref()?;

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
    let data = body.get("data")?;

    let post_status = data
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let post_score = data.get("score").and_then(|v| v.as_u64()).unwrap_or(100);
    let post_error_count = data
        .get("breakdown")
        .map(|b| {
            let crashes = b.get("crashes").and_then(|v| v.as_u64()).unwrap_or(0);
            let errors = b.get("errors").and_then(|v| v.as_u64()).unwrap_or(0);
            crashes + errors
        })
        .unwrap_or(0);

    // Detect meaningful degradation
    let status_degraded = baseline.status == "healthy" && post_status != "healthy";
    let newly_broken = baseline.status != "broken" && post_status == "broken";
    let new_errors = post_error_count.saturating_sub(baseline.error_count);
    let score_drop = baseline.score.saturating_sub(post_score);

    if !newly_broken && !status_degraded && new_errors == 0 && score_drop < 20 {
        return None;
    }

    let mut warning = String::from("### App Health Regression\n\n");
    warning.push_str(
        "**Your changes degraded the app's health.** The following issues were detected after your code changes:\n\n",
    );

    if newly_broken {
        let summary = data.get("summary").and_then(|v| v.as_str()).unwrap_or("");
        warning.push_str(&format!(
            "- App went from **{}** to **BROKEN** (score: {} → {})\n",
            baseline.status.to_uppercase(),
            baseline.score,
            post_score
        ));
        if !summary.is_empty() {
            warning.push_str(&format!("- Health summary: {}\n", summary));
        }
    } else if status_degraded {
        warning.push_str(&format!(
            "- App health degraded from **{}** to **{}** (score: {} → {})\n",
            baseline.status.to_uppercase(),
            post_status.to_uppercase(),
            baseline.score,
            post_score
        ));
    }

    if new_errors > 0 {
        warning.push_str(&format!(
            "- {} new error(s) introduced (was {}, now {})\n",
            new_errors, baseline.error_count, post_error_count
        ));
    }

    if let Some(top_issue) = data.get("topIssue") {
        if let Some(msg) = top_issue.get("message").and_then(|v| v.as_str()) {
            let severity = top_issue
                .get("severity")
                .and_then(|v| v.as_str())
                .unwrap_or("error");
            warning.push_str(&format!("- Top issue: [{}] {}\n", severity, msg));
        }
    }

    warning.push_str(
        "\nPlease check that your changes don't break the app. Fix any compilation or runtime errors before proceeding.\n",
    );

    Some(warning)
}

// =============================================================================
// Regression Detection
// =============================================================================

/// Compare current verification results with the previous iteration to detect regressions.
///
/// Returns a warning string if regressions are found (steps that were passing before
/// but now fail), or None if no regressions detected.
pub fn detect_regression(
    checkpoint_db: &CheckpointDb,
    execution_id: &str,
    current_iteration: u32,
    current_result: &crate::step_executor::VerificationPhaseResult,
) -> Option<String> {
    if current_iteration <= 1 {
        return None;
    }

    // Retrieve previous iteration result
    let prev_result =
        match checkpoint_db.get_verification_phase_result(execution_id, current_iteration - 1) {
            Ok(Some(val)) => val,
            _ => return None,
        };

    // Extract previous step results
    let prev_step_results = prev_result.get("step_results").and_then(|v| v.as_array())?;

    // Build a map of step_name -> success for previous iteration
    let mut prev_step_status: std::collections::HashMap<String, bool> =
        std::collections::HashMap::new();
    for step in prev_step_results {
        if let (Some(name), Some(success)) = (
            step.get("step_name").and_then(|v| v.as_str()),
            step.get("success").and_then(|v| v.as_bool()),
        ) {
            prev_step_status.insert(name.to_string(), success);
        }
    }

    // Find regressions: steps that were passing before but now fail
    let mut newly_broken: Vec<String> = Vec::new();
    for result in &current_result.step_results {
        if !result.success {
            if let Some(&prev_passed) = prev_step_status.get(&result.step_name) {
                if prev_passed {
                    newly_broken.push(result.step_name.clone());
                }
            }
        }
    }

    // Compare overall scores
    let prev_passed = prev_result
        .get("passed_steps")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let prev_total = prev_result
        .get("total_steps")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let curr_passed = current_result.passed_steps as u64;
    let curr_total = current_result.total_steps as u64;

    // Only warn if there are actual regressions
    if newly_broken.is_empty() && curr_passed >= prev_passed {
        return None;
    }

    let mut warning = String::new();
    warning.push_str("## REGRESSION WARNING\n\n");
    warning.push_str("Your changes in the previous iteration caused regressions.\n\n");

    if !newly_broken.is_empty() {
        warning.push_str(&format!(
            "**Previously passing, now failing:** {}\n",
            newly_broken.join(", ")
        ));
    }

    if curr_passed < prev_passed {
        warning.push_str(&format!(
            "**Score change:** {}/{} passed -> {}/{} passed ({} more failures)\n",
            prev_passed,
            prev_total,
            curr_passed,
            curr_total,
            prev_passed - curr_passed
        ));
    }

    warning.push_str(
        "\nConsider whether your changes were correct or if they had unintended side effects.\n",
    );

    info!(
        "REGRESSION-DETECT: iteration {} has {} newly broken steps (was {}/{}, now {}/{})",
        current_iteration,
        newly_broken.len(),
        prev_passed,
        prev_total,
        curr_passed,
        curr_total
    );

    Some(warning)
}

// =============================================================================
// Resume Context Builder
// =============================================================================

/// Build agentic context from stored verification data when resuming from an
/// interrupted agentic phase.
///
/// Tries multiple data sources in order:
/// 1. Verification phase result from database (full structured result)
/// 2. Step checkpoints from database (step names + error messages)
/// 3. Fallback generic message
pub fn build_resume_agentic_context(
    checkpoint_db: &Arc<CheckpointDb>,
    execution_id: &str,
    iteration: u32,
) -> String {
    // Strategy 1: Try loading the full verification phase result.
    // The result may be stored under the execution_id or a child ID.
    if let Ok(Some(result_json)) =
        checkpoint_db.get_verification_phase_result(execution_id, iteration)
    {
        // Try to deserialize into VerificationPhaseResult and use build_failure_context()
        if let Ok(result) =
            serde_json::from_value::<crate::step_executor::VerificationPhaseResult>(result_json)
        {
            let context = result.build_failure_context();
            if !context.is_empty() {
                info!(
                    "RESUME: Built agentic context from verification phase result ({} chars)",
                    context.len()
                );
                return context;
            }
        }
    }

    // Strategy 2: Build context from step checkpoints (which remap to parent ID).
    let checkpoint_mgr =
        crate::workflow_state::CheckpointManager::new(checkpoint_db.clone(), "unified");
    if let Ok(checkpoints) =
        checkpoint_mgr.get_completed_steps(execution_id, "verification", Some(iteration))
    {
        if !checkpoints.is_empty() {
            let failed: Vec<_> = checkpoints
                .iter()
                .filter(|cp| {
                    matches!(
                        cp.status,
                        crate::workflow_state::StepCheckpointStatus::Failed
                    )
                })
                .collect();

            let total = checkpoints.len();
            let passed = checkpoints
                .iter()
                .filter(|cp| {
                    matches!(
                        cp.status,
                        crate::workflow_state::StepCheckpointStatus::Success
                    )
                })
                .count();

            if !failed.is_empty() {
                let mut context = String::new();
                context.push_str("## Verification Results (Resumed)\n\n");
                context.push_str(&format!(
                    "**Status:** {} of {} verification steps passed\n\n",
                    passed, total
                ));
                context.push_str("### Failed Steps\n\n");

                for cp in &failed {
                    let name = cp.step_name.as_deref().unwrap_or("unknown");
                    let step_type = &cp.step_type;
                    context.push_str(&format!("#### {} ({})\n", name, step_type));

                    if let Some(ref error) = cp.error {
                        context.push_str(&format!("**Error:** {}\n", error));
                    }

                    // If result_json is available (e.g., for successful steps that later
                    // became relevant), include it
                    if let Some(ref result_str) = cp.result_json {
                        if let Ok(result_data) =
                            serde_json::from_str::<serde_json::Value>(result_str)
                        {
                            // Extract stdout/output if present
                            if let Some(output) = result_data
                                .get("stdout")
                                .or_else(|| result_data.get("output"))
                                .and_then(|v| v.as_str())
                            {
                                if !output.is_empty() {
                                    let truncated = if output.len() > 2000 {
                                        let t = truncate_str(output, 2000);
                                        format!(
                                            "{}...\n[truncated, {} more chars]",
                                            t,
                                            output.len() - t.len()
                                        )
                                    } else {
                                        output.to_string()
                                    };
                                    context.push_str(&format!(
                                        "**Output:**\n```\n{}\n```\n",
                                        truncated
                                    ));
                                }
                            }
                        }
                    }

                    context.push('\n');
                }

                info!(
                    "RESUME: Built agentic context from step checkpoints ({} chars, {} failed steps)",
                    context.len(),
                    failed.len()
                );
                return context;
            } else {
                // All verification steps passed — no failures to fix
                info!(
                    "RESUME: All {} verification steps passed, no failures to fix",
                    total
                );
                return format!(
                    "All {} verification steps passed. No failures to investigate. \
                     Proceed with any analysis or improvements based on the workflow context provided.",
                    total
                );
            }
        }
    }

    // Strategy 3: Fallback — no verification data available at all
    info!("RESUME: No verification data found, using fallback context");
    "Resuming agentic phase. No verification data is available. \
     Proceed with analysis based on the workflow context and instructions provided."
        .to_string()
}

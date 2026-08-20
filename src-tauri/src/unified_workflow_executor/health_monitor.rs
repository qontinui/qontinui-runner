//! Health monitoring and UI context functions for the agentic verification loop.
//!
//! Extracted from `loop_controller.rs` — provides UI Bridge snapshot fetching,
//! health baseline capture, regression detection, and resume context building.

use std::sync::Arc;

use futures_util::FutureExt;
use tracing::{info, warn};

use crate::mcp::types::MCP_API_PORT;
use crate::str_utils::truncate_str;
use crate::vision::annotator::{AnnotatedElement, AnnotationResult};
use crate::vision::element_collector;

// =============================================================================
// Verifier UI Context (Snapshot, Console Errors, App Health, Annotations)
// =============================================================================

/// Rich UI context returned by `fetch_verifier_ui_context`.
///
/// Contains both text-based context (for text-only prompts) and optional
/// annotation data (for multimodal prompts with annotated screenshots).
pub struct VerifierUiContext {
    /// Markdown-formatted text context (snapshot summary, console errors, health).
    pub text: String,
    /// Annotated elements extracted from the UI Bridge snapshot.
    /// Empty if no elements were found or UI Bridge is not connected.
    pub elements: Vec<AnnotatedElement>,
    /// Screenshot annotation result (annotated image + text index).
    /// None if no screenshot was captured or annotation failed.
    pub annotation: Option<AnnotationResult>,
}

impl VerifierUiContext {
    /// Empty context (no UI Bridge connected).
    pub fn empty() -> Self {
        Self {
            text: String::new(),
            elements: Vec::new(),
            annotation: None,
        }
    }

    /// Check if this context has any useful data.
    pub fn is_empty(&self) -> bool {
        self.text.is_empty() && self.elements.is_empty()
    }
}

/// Fetch live UI Bridge data for the agentic verification loop's verifier.
///
/// Concurrently fetches the UI snapshot, console errors, and app health status.
/// Also extracts interactive elements for screenshot annotation.
/// Each fetch is gated by its corresponding config flag and is best-effort:
/// failures are silently ignored (the SDK app may not be connected).
pub async fn fetch_verifier_ui_context(
    use_screenshots: bool,
    include_console_errors: bool,
    include_app_health: bool,
) -> VerifierUiContext {
    if !use_screenshots && !include_console_errors && !include_app_health {
        return VerifierUiContext::empty();
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

    // Extract elements for annotation from the snapshot
    let elements = if let Some(ref snap) = snapshot {
        element_collector::extract_elements_from_snapshot(snap)
    } else {
        Vec::new()
    };

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

    // Include element index text in the sections if we have elements
    if !elements.is_empty() {
        sections.push_str(&format!(
            "\n\n## Screen Elements ({} interactive)\n",
            elements.len()
        ));
        for el in &elements {
            sections.push_str(&format!(
                "[{}] \"{}\" {} at ({:.3}, {:.3})\n",
                el.index, el.label, el.element_type, el.normalized_rect.x, el.normalized_rect.y,
            ));
        }
    }

    VerifierUiContext {
        text: sections,
        elements,
        annotation: None, // Annotation produced by caller when screenshot is available
    }
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
pub async fn detect_regression(
    execution_id: &str,
    current_iteration: u32,
    current_result: &crate::step_executor::VerificationPhaseResult,
    pg_db: &std::sync::Arc<crate::database::pg::PgDb>,
) -> Option<String> {
    if current_iteration <= 1 {
        return None;
    }

    // Retrieve previous iteration result from PostgreSQL. catch_unwind
    // around the lookup (not just `?`/`.ok()`) preserves the original
    // sync-fn behavior: a panic here degrades to "no regression data",
    // matching build_resume_agentic_context's identical fix above.
    let prev_result = {
        let prev_iter = current_iteration - 1;
        std::panic::AssertUnwindSafe(pg_db.get_verification_phase_result(execution_id, prev_iter))
            .catch_unwind()
            .await
            .ok()
            .and_then(|r| r.ok())
            .flatten()
    };
    let prev_result = prev_result?;

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
pub async fn build_resume_agentic_context(
    execution_id: &str,
    iteration: u32,
    stage_index: Option<u32>,
    pg_db: &std::sync::Arc<crate::database::pg::PgDb>,
) -> String {
    // Strategy 1: Try loading the full verification phase result.
    // The result may be stored under the execution_id or a child ID.
    // catch_unwind around the lookup (not just `?`/`.ok()`) preserves the
    // original sync-fn behavior: a panic here degrades to Strategy 2/3
    // instead of aborting the caller's resume path.
    let phase_result =
        std::panic::AssertUnwindSafe(pg_db.get_verification_phase_result(execution_id, iteration))
            .catch_unwind()
            .await
            .ok()
            .and_then(|r| r.ok())
            .flatten();
    if let Some(result_json) = phase_result {
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

    // Strategy 2: Build the context from this iteration's verification
    // checkpoints.
    //
    // RECONCILED WITH THE PHASE-2 REPLAY READER. Two readers now consume the
    // same journal, and they must never disagree about a single row:
    //
    // * Phase 2 (`phases::journalled_step`) reuses a step's journalled result
    //   INSTEAD of executing it. It consumes exactly `is_replayable()` rows.
    // * This function narrates to the model the work that is still outstanding.
    //   It consumes exactly `is_outstanding()` rows.
    //
    // Those two predicates are disjoint by construction
    // (`checkpoint_reader_partition_is_total_and_disjoint` in
    // `workflow_state::checkpoint`), so a step can never be both skipped as
    // already-done AND described to the model as still to do. Belt and braces,
    // the verification phase is excluded from the replay set outright
    // (`phases::phase_is_replayable`), so nothing this function reads has been
    // replayed at all.
    //
    // Consequently this reader NO LONGER touches `result_json`. That field is
    // the replay reader's payload; a failed checkpoint never carries one
    // anyway, because the upsert writes `result_json = NULL` on the failure
    // path (`database/pg/workflow_state.rs`).
    //
    // The two readers agree on the SLICE as well as on the predicate.
    // `get_completed_steps` filters on `(execution_id, phase, iteration)` only,
    // and that key is NOT unique — it is precisely the key
    // `CheckpointManager::completed_step` documents as insufficient. Every
    // stage of a multi-stage workflow writes its verification steps under the
    // same phase and iteration, so a stage-blind read hands a stage-2 resume
    // stage 0's and stage 1's rows too: the "N of M passed" header counts
    // every stage's steps, and the failed-steps list describes failures that an
    // earlier stage already fixed. Hence the explicit `stage_index` filter
    // below, normalized with `unwrap_or(0)` exactly as `select_replayable`
    // does, because the write path COALESCEs a missing stage to 0.
    //
    // NOTE: `get_completed_steps` does NOT remap a composed-run child to its
    // parent id — only the WRITE path does. A child therefore reads back
    // nothing here and falls through to Strategy 3, which is the correct
    // outcome: children of one composed run share a single checkpoint
    // keyspace, so a hit would be a sibling's result, not this workflow's.
    let checkpoint_mgr = crate::workflow_state::CheckpointManager::new("unified");
    match checkpoint_mgr.get_completed_steps(execution_id, "verification", Some(iteration)) {
        Ok(checkpoints) => {
            let this_stage = checkpoints_in_stage(&checkpoints, stage_index);
            if let Some(context) = narrate_verification_checkpoints(&this_stage) {
                info!(
                    "RESUME: Built agentic context from step checkpoints ({} chars)",
                    context.len()
                );
                return context;
            }
        }
        Err(e) => {
            // Not knowing is not the same as "no verification ran".
            warn!(
                execution_id = %execution_id,
                iteration = %iteration,
                stage_index = ?stage_index,
                error = %e,
                "RESUME: checkpoint read failed, falling back to the generic context"
            );
        }
    }

    // Strategy 3: Fallback — no verification data available at all
    info!("RESUME: No verification data found, using fallback context");
    "Resuming agentic phase. No verification data is available. \
     Proceed with analysis based on the workflow context and instructions provided."
        .to_string()
}

/// Narrow a `(execution_id, phase, iteration)` checkpoint slice to ONE stage.
///
/// `get_completed_steps` cannot filter on stage — the column is not part of its
/// key — but `(phase, iteration, step_index)` is not unique across stages, so
/// the caller has to. A missing stage index and stage 0 are the same thing: the
/// write path `COALESCE`s a missing stage to 0
/// (`database/pg/workflow_state.rs`), which is the same normalization
/// `workflow_state::checkpoint::select_replayable` applies on the replay side.
pub(crate) fn checkpoints_in_stage(
    checkpoints: &[crate::workflow_state::StepCheckpoint],
    stage_index: Option<u32>,
) -> Vec<crate::workflow_state::StepCheckpoint> {
    checkpoints
        .iter()
        .filter(|cp| cp.stage_index.unwrap_or(0) == stage_index.unwrap_or(0))
        .cloned()
        .collect()
}

/// Narrate a verification iteration's checkpoints as agentic context.
///
/// Pure, so the partition against the Phase-2 replay reader is testable without
/// a database. Returns `None` when the slice carries no TERMINAL row at all —
/// there is nothing to say, and the caller should fall through to its generic
/// context.
///
/// `Pending`/`Running` rows are excluded from the counts as well as from the
/// narration. They are the debris of the crash itself (a step was marked
/// started and never finished), so counting them would report verification
/// steps that never produced a result — and Phase 2 will re-execute exactly
/// those, because `is_replayable()` rejects them too.
pub(crate) fn narrate_verification_checkpoints(
    checkpoints: &[crate::workflow_state::StepCheckpoint],
) -> Option<String> {
    let terminal: Vec<&crate::workflow_state::StepCheckpoint> = checkpoints
        .iter()
        .filter(|cp| cp.status.is_terminal())
        .collect();

    if terminal.is_empty() {
        return None;
    }

    let total = terminal.len();
    let passed = terminal
        .iter()
        .filter(|cp| cp.status.is_replayable())
        .count();
    let failed: Vec<&&crate::workflow_state::StepCheckpoint> = terminal
        .iter()
        .filter(|cp| cp.status.is_outstanding())
        .collect();

    if failed.is_empty() {
        info!(
            "RESUME: All {} verification steps passed, no failures to fix",
            total
        );
        return Some(format!(
            "All {} verification steps passed. No failures to investigate. \
             Proceed with any analysis or improvements based on the workflow context provided.",
            total
        ));
    }

    let mut context = String::new();
    context.push_str("## Verification Results (Resumed)\n\n");
    context.push_str(&format!(
        "**Status:** {} of {} verification steps passed\n\n",
        passed, total
    ));
    context.push_str("### Failed Steps\n\n");

    for cp in &failed {
        let name = cp.step_name.as_deref().unwrap_or("unknown");
        context.push_str(&format!("#### {} ({})\n", name, cp.step_type));

        if let Some(ref error) = cp.error {
            let shown = if error.len() > 2000 {
                let t = truncate_str(error, 2000);
                format!(
                    "{}...\n[truncated, {} more chars]",
                    t,
                    error.len() - t.len()
                )
            } else {
                error.clone()
            };
            context.push_str(&format!("**Error:** {}\n", shown));
        }

        context.push('\n');
    }

    info!(
        "RESUME: Narrated {} failed verification step(s) of {} terminal checkpoint(s)",
        failed.len(),
        total
    );
    Some(context)
}

#[cfg(test)]
mod resume_context_tests {
    use super::*;
    use crate::workflow_state::{StepCheckpoint, StepCheckpointStatus};

    fn cp(
        step_index: usize,
        name: &str,
        status: StepCheckpointStatus,
        error: Option<&str>,
        result_json: Option<&str>,
    ) -> StepCheckpoint {
        let mut c = StepCheckpoint::new(
            "exec-1",
            "unified",
            "verification",
            Some(3),
            step_index,
            "playwright",
        )
        .with_step_name(name);
        c.status = status;
        c.error = error.map(str::to_string);
        c.result_json = result_json.map(str::to_string);
        c
    }

    /// THE reconciliation test: the rows Phase 2 replays and the rows this
    /// narration describes are disjoint, and together they cover every
    /// terminal row. A step can never be skipped as done AND narrated as
    /// outstanding.
    #[test]
    fn replayed_and_narrated_checkpoints_are_disjoint() {
        let rows = vec![
            cp(
                0,
                "login works",
                StepCheckpointStatus::Success,
                None,
                Some("{\"ok\":true}"),
            ),
            cp(
                1,
                "dashboard renders",
                StepCheckpointStatus::Failed,
                Some("expected Submit"),
                None,
            ),
            cp(2, "logout works", StepCheckpointStatus::Skipped, None, None),
            cp(
                3,
                "half-run step",
                StepCheckpointStatus::Running,
                None,
                None,
            ),
        ];

        let replayed: Vec<usize> = rows
            .iter()
            .filter(|c| c.status.is_replayable())
            .map(|c| c.step_index)
            .collect();
        let narrated: Vec<usize> = rows
            .iter()
            .filter(|c| c.status.is_outstanding())
            .map(|c| c.step_index)
            .collect();

        assert_eq!(replayed, vec![0, 2]);
        assert_eq!(narrated, vec![1]);
        assert!(
            replayed.iter().all(|i| !narrated.contains(i)),
            "a replayed step must never also be narrated as work to do"
        );

        let context = narrate_verification_checkpoints(&rows).expect("has a failure to narrate");
        assert!(context.contains("dashboard renders"), "{}", context);
        assert!(
            !context.contains("login works"),
            "a replayed step must not be narrated: {}",
            context
        );
        assert!(
            !context.contains("half-run step"),
            "crash debris must not be narrated: {}",
            context
        );
    }

    /// The counts describe TERMINAL rows only: a `Running` row left behind by
    /// the crash is not a verification step that produced a result.
    #[test]
    fn counts_exclude_crash_debris() {
        let rows = vec![
            cp(0, "a", StepCheckpointStatus::Success, None, None),
            cp(1, "b", StepCheckpointStatus::Failed, Some("boom"), None),
            cp(2, "c", StepCheckpointStatus::Running, None, None),
            cp(3, "d", StepCheckpointStatus::Pending, None, None),
        ];
        let context = narrate_verification_checkpoints(&rows).expect("has a failure");
        assert!(
            context.contains("1 of 2 verification steps passed"),
            "counts must cover terminal rows only: {}",
            context
        );
    }

    /// A fully-passing iteration says so rather than inventing failures.
    #[test]
    fn all_passed_reports_no_failures() {
        let rows = vec![
            cp(0, "a", StepCheckpointStatus::Success, None, None),
            cp(1, "b", StepCheckpointStatus::Skipped, None, None),
        ];
        let context = narrate_verification_checkpoints(&rows).expect("has terminal rows");
        assert!(
            context.starts_with("All 2 verification steps passed"),
            "{}",
            context
        );
    }

    /// Nothing terminal — including an empty slice, and a slice of pure crash
    /// debris — means the caller must fall through, not narrate an empty list.
    #[test]
    fn nothing_terminal_falls_through() {
        assert!(narrate_verification_checkpoints(&[]).is_none());
        let debris = vec![
            cp(0, "a", StepCheckpointStatus::Running, None, None),
            cp(1, "b", StepCheckpointStatus::Pending, None, None),
        ];
        assert!(narrate_verification_checkpoints(&debris).is_none());
    }

    /// The narration slice is per-STAGE.
    ///
    /// `get_completed_steps` keys on `(execution_id, phase, iteration)` only,
    /// and every stage writes its verification steps under the same phase and
    /// iteration. Without the stage filter a stage-2 resume was narrated stage
    /// 0's and stage 1's rows as well: the header counted all three stages, and
    /// the failed-steps list handed the stage-2 model failures that stage 0 had
    /// already fixed.
    #[test]
    fn narration_covers_only_the_resuming_stage() {
        let staged = |step_index: usize,
                      name: &str,
                      status: StepCheckpointStatus,
                      error: Option<&str>,
                      stage: Option<u32>| {
            let mut c = cp(step_index, name, status, error, None);
            c.stage_index = stage;
            c
        };

        let rows = vec![
            staged(
                0,
                "stage 0 login broken",
                StepCheckpointStatus::Failed,
                Some("stage 0 boom"),
                Some(0),
            ),
            staged(
                1,
                "stage 0 ok",
                StepCheckpointStatus::Success,
                None,
                Some(0),
            ),
            staged(
                0,
                "stage 2 checkout broken",
                StepCheckpointStatus::Failed,
                Some("stage 2 boom"),
                Some(2),
            ),
            staged(
                1,
                "stage 2 ok",
                StepCheckpointStatus::Success,
                None,
                Some(2),
            ),
        ];

        let this_stage = checkpoints_in_stage(&rows, Some(2));
        assert_eq!(this_stage.len(), 2, "only stage 2's rows");

        let context = narrate_verification_checkpoints(&this_stage).expect("stage 2 has a failure");
        assert!(
            context.contains("1 of 2 verification steps passed"),
            "the header must count this stage only: {}",
            context
        );
        assert!(context.contains("stage 2 checkout broken"), "{}", context);
        assert!(
            !context.contains("stage 0 login broken"),
            "an earlier stage's failure must not be handed to this stage's model: {}",
            context
        );
    }

    /// A missing stage index and stage 0 are the same thing — the write path
    /// `COALESCE`s a missing stage to 0, so a `None` caller and a `Some(0)` row
    /// must match each other in both directions.
    #[test]
    fn missing_stage_index_is_stage_zero() {
        let mut none_row = cp(0, "unstaged", StepCheckpointStatus::Success, None, None);
        none_row.stage_index = None;
        let mut zero_row = cp(1, "stage zero", StepCheckpointStatus::Success, None, None);
        zero_row.stage_index = Some(0);
        let mut one_row = cp(2, "stage one", StepCheckpointStatus::Success, None, None);
        one_row.stage_index = Some(1);

        let rows = vec![none_row, zero_row, one_row];
        assert_eq!(checkpoints_in_stage(&rows, None).len(), 2);
        assert_eq!(checkpoints_in_stage(&rows, Some(0)).len(), 2);
        assert_eq!(checkpoints_in_stage(&rows, Some(1)).len(), 1);
    }

    /// A long error is truncated rather than blowing out the prompt.
    #[test]
    fn long_errors_are_truncated() {
        let long = "x".repeat(5000);
        let rows = vec![cp(0, "a", StepCheckpointStatus::Failed, Some(&long), None)];
        let context = narrate_verification_checkpoints(&rows).expect("has a failure");
        assert!(
            context.contains("[truncated,"),
            "{}",
            &context[..200.min(context.len())]
        );
        assert!(context.len() < 3000);
    }
}

use async_trait::async_trait;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{error, info, warn};

use super::{ExecutionStepConfig, HandlerContext, StepHandler, StepHandlerResult};

// ---------------------------------------------------------------------------
// Pure helper functions (#1 — foundation for typed parsing and compare logic)
// ---------------------------------------------------------------------------

/// Map a severity threshold string to a numeric order for comparison.
///
/// "critical" → 4, "major" → 3, "minor" → 2, "info" → 1.
/// Unknown values log a warning and default to "major" (3).
fn severity_to_order(threshold: &str) -> u32 {
    match threshold {
        "critical" => 4,
        "major" => 3,
        "minor" => 2,
        "info" => 1,
        other => {
            warn!(
                "Unknown severity threshold '{}', defaulting to 'major'",
                other
            );
            3
        }
    }
}

/// Count the number of findings at or above the given threshold order.
fn compute_failing_count(
    threshold_order: u32,
    critical: u64,
    major: u64,
    minor: u64,
    info: u64,
) -> u64 {
    match threshold_order {
        4 => critical,
        3 => critical + major,
        2 => critical + major + minor,
        1 => critical + major + minor + info,
        _ => critical + major,
    }
}

// ---------------------------------------------------------------------------
// Typed comparison result (#6 — replace dynamic JSON field access)
// ---------------------------------------------------------------------------

fn default_summary() -> String {
    "Comparison completed".into()
}

/// Typed representation of a UI Bridge comparison result.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ComparisonResult {
    #[serde(default)]
    critical_count: u64,
    #[serde(default)]
    major_count: u64,
    #[serde(default)]
    minor_count: u64,
    #[serde(default)]
    info_count: u64,
    #[serde(default)]
    total_differences: u64,
    #[serde(default = "default_summary")]
    summary: String,
}

// ---------------------------------------------------------------------------
// Extract origin helper
// ---------------------------------------------------------------------------

/// Extract the origin (scheme + host + port) from a URL.
///
/// Given `http://localhost:9876/ui-bridge`, returns `http://localhost:9876`.
/// Falls back to the default `http://localhost:9876` if parsing fails.
fn extract_origin(url: &str) -> String {
    // Find the start of the path after "scheme://host[:port]"
    if let Some(scheme_end) = url.find("://") {
        let after_scheme = &url[scheme_end + 3..];
        // The origin ends at the first '/' after the host (or end of string)
        if let Some(path_start) = after_scheme.find('/') {
            return url[..scheme_end + 3 + path_start].to_string();
        }
        // No path — the entire URL is the origin
        return url.to_string();
    }
    // Fallback
    "http://localhost:9876".to_string()
}

// ---------------------------------------------------------------------------
// Retry helper (#6 — retry with exponential backoff for transient failures)
// ---------------------------------------------------------------------------

/// Whether an error is transient and should be retried.
fn is_retryable_error(err: &reqwest::Error) -> bool {
    err.is_connect() || err.is_timeout() || err.is_request()
}

/// Whether an HTTP status code indicates a transient server error.
fn is_retryable_status(status: reqwest::StatusCode) -> bool {
    status.is_server_error() // 5xx
}

// ---------------------------------------------------------------------------
// UiBridgeFailureTracker (#9 — AI diagnostic after N consecutive failures)
// ---------------------------------------------------------------------------

/// Tracks consecutive UI Bridge failures per URL for AI diagnostic triggering.
pub struct UiBridgeFailureTracker {
    failures: Arc<tokio::sync::Mutex<HashMap<String, Vec<String>>>>,
}

impl UiBridgeFailureTracker {
    pub fn new() -> Self {
        Self {
            failures: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
        }
    }

    /// Record a failure for a URL. Returns the current consecutive failure count.
    pub async fn record_failure(&self, url: &str, error: &str) -> u32 {
        let mut map = self.failures.lock().await;
        let entries = map.entry(url.to_string()).or_default();
        entries.push(error.to_string());
        entries.len() as u32
    }

    /// Record a success for a URL, resetting its failure counter.
    pub async fn record_success(&self, url: &str) {
        let mut map = self.failures.lock().await;
        map.remove(url);
    }

    /// Get the N most recent errors for a URL.
    pub async fn get_recent_errors(&self, url: &str, n: usize) -> Vec<String> {
        let map = self.failures.lock().await;
        match map.get(url) {
            Some(entries) => entries.iter().rev().take(n).cloned().collect(),
            None => Vec::new(),
        }
    }

    /// Reset failure tracking for a URL.
    pub async fn reset(&self, url: &str) {
        let mut map = self.failures.lock().await;
        map.remove(url);
    }
}

impl Default for UiBridgeFailureTracker {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// UiBridgeHandler
// ---------------------------------------------------------------------------

/// Handler for UI Bridge steps.
///
/// Performs UI Bridge SDK operations:
/// - navigate: Navigate to a URL
/// - execute: Execute a natural language instruction
/// - assert: Assert a condition on a UI element
/// - snapshot: Capture a snapshot of the current UI state
/// - compare: Compare current snapshot against a reference
pub struct UiBridgeHandler;

#[async_trait]
impl StepHandler for UiBridgeHandler {
    fn step_type(&self) -> &'static str {
        "ui_bridge"
    }

    fn display_name(&self) -> &'static str {
        "UI Bridge"
    }

    async fn execute(
        &self,
        step: &ExecutionStepConfig,
        context: &HandlerContext,
    ) -> StepHandlerResult {
        let action = step.ui_bridge_action.as_deref().unwrap_or("snapshot");
        let raw_url = step
            .ui_bridge_url
            .as_deref()
            .unwrap_or("http://localhost:9876/ui-bridge");

        // SDK operations (navigate, execute, assert) always go through the runner's SDK proxy.
        // The `ui_bridge_url` may be a page URL (e.g., "http://localhost:3001/build/page-sweep")
        // rather than a UI Bridge API base — detect this and use the runner proxy instead.
        let base_url: &str = if matches!(action, "navigate" | "execute" | "assert") {
            // Always use runner SDK proxy for SDK operations
            "http://localhost:9876/ui-bridge"
        } else if raw_url.contains("/ui-bridge") {
            // URL already contains UI Bridge path — use as-is
            raw_url
        } else {
            // Fallback: use runner SDK proxy
            "http://localhost:9876/ui-bridge"
        };

        let timeout_ms =
            step.ui_bridge_timeout_ms
                .unwrap_or(if action == "compare" { 120000 } else { 30000 });

        info!(
            "UI Bridge step: action={}, url={}, base={}",
            action, raw_url, base_url
        );

        // (#7) Clean up stale locks before acquiring — only in workflow context
        if let Some(ref task_run_id) = context.task_run_id {
            context
                .app_state
                .url_lock_manager
                .cleanup_stale_locks(&context.app_state.checkpoint_db)
                .await;

            // Acquire per-URL lock if running inside a workflow (has task_run_id).
            // The lock is held for the workflow's lifetime, not just this step,
            // ensuring consecutive UI Bridge steps don't interleave with other workflows.
            let workflow_name = step.name.as_deref().unwrap_or("unnamed workflow");
            context
                .app_state
                .url_lock_manager
                .acquire(
                    base_url,
                    task_run_id,
                    workflow_name,
                    Some(&context.app_state.checkpoint_db),
                )
                .await;
        }

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_millis(timeout_ms))
            .build()
            .map_err(|e| format!("Failed to create HTTP client: {}", e));

        let client = match client {
            Ok(c) => c,
            Err(e) => {
                return StepHandlerResult::failure(e);
            }
        };

        // (#2) Health check before operations — skip for snapshot (it IS the health check)
        if action != "snapshot" {
            let is_sdk_op = matches!(action, "navigate" | "execute" | "assert");

            if is_sdk_op {
                // For SDK operations, check SDK connection status (not runner webview).
                // The SDK proxy is independent of the runner's Tauri webview — the proxy
                // can be functional even when the webview is temporarily unavailable.
                let status_url = format!("{}/sdk/status", base_url.trim_end_matches('/'));
                let health_client = reqwest::Client::builder()
                    .timeout(std::time::Duration::from_secs(5))
                    .build()
                    .ok();

                if let Some(hc) = &health_client {
                    let mut connected = false;

                    // Check SDK connection status with retry
                    for attempt in 1..=3u32 {
                        match hc.get(&status_url).send().await {
                            Ok(resp) if resp.status().is_success() => {
                                if let Ok(body) = resp.text().await {
                                    connected = body.contains("\"connected\":true");
                                    if connected {
                                        break;
                                    }
                                }
                            }
                            Ok(_) => {}
                            Err(e) => {
                                warn!("SDK status check attempt {}/3 failed: {}", attempt, e);
                            }
                        }

                        if !connected && attempt < 3 {
                            // Try to auto-reconnect using origin from step URL
                            let app_origin = step
                                .ui_bridge_url
                                .as_deref()
                                .and_then(|u| {
                                    if let Some(scheme_end) = u.find("://") {
                                        let after = &u[scheme_end + 3..];
                                        if let Some(path_start) = after.find('/') {
                                            Some(&u[..scheme_end + 3 + path_start])
                                        } else {
                                            Some(u)
                                        }
                                    } else {
                                        None
                                    }
                                })
                                .unwrap_or("http://localhost:3001");

                            info!(
                                "SDK not connected, attempting reconnect to {} (attempt {}/3)",
                                app_origin, attempt
                            );

                            let connect_url =
                                format!("{}/sdk/connect", base_url.trim_end_matches('/'));
                            let connect_body = serde_json::json!({ "url": app_origin });

                            match hc.post(&connect_url).json(&connect_body).send().await {
                                Ok(r) if r.status().is_success() => {
                                    info!("SDK reconnect succeeded");
                                    // Wait briefly for connection to stabilize
                                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                                }
                                Ok(r) => {
                                    warn!("SDK reconnect returned status {}", r.status());
                                }
                                Err(e) => {
                                    warn!("SDK reconnect failed: {}", e);
                                }
                            }
                        }
                    }

                    if !connected {
                        // Final check after reconnect attempts
                        if let Ok(resp) = hc.get(&status_url).send().await {
                            if let Ok(body) = resp.text().await {
                                connected = body.contains("\"connected\":true");
                            }
                        }
                    }

                    if !connected {
                        let msg = format!(
                            "UI Bridge SDK is not connected (checked {} — no active connection after reconnect attempts)",
                            status_url
                        );
                        error!("{}", msg);
                        return StepHandlerResult::failure(msg);
                    }
                }
            } else {
                // For non-SDK operations (compare, etc.), check control endpoint
                let health_url = format!("{}/control/snapshot", base_url.trim_end_matches('/'));
                let health_client = reqwest::Client::builder()
                    .timeout(std::time::Duration::from_secs(5))
                    .build()
                    .ok();

                if let Some(hc) = health_client {
                    let mut reachable = false;
                    for attempt in 1..=3u32 {
                        match hc.get(&health_url).send().await {
                            Ok(_) => {
                                reachable = true;
                                break;
                            }
                            Err(e) => {
                                if attempt < 3 {
                                    warn!(
                                        "Health check attempt {}/3 failed: {}, retrying...",
                                        attempt, e
                                    );
                                    tokio::time::sleep(std::time::Duration::from_secs(
                                        attempt as u64,
                                    ))
                                    .await;
                                } else {
                                    let msg = format!(
                                        "UI Bridge at {} is not reachable (health check failed after 3 attempts: {})",
                                        base_url, e
                                    );
                                    error!("{}", msg);
                                    return StepHandlerResult::failure(msg);
                                }
                            }
                        }
                    }
                    if !reachable {
                        let msg = format!(
                            "UI Bridge at {} is not reachable after 3 attempts",
                            base_url,
                        );
                        error!("{}", msg);
                        return StepHandlerResult::failure(msg);
                    }
                }
            }
        }

        // Handle compare action separately — it has multi-step logic
        if action == "compare" {
            let result = self
                .execute_compare(step, context, &client, base_url, timeout_ms)
                .await;
            self.track_result(context, base_url, &result).await;
            return result;
        }

        // (#1) Retry loop for transient failures — 3 attempts, exponential backoff
        let max_attempts: u32 = 3;
        let mut last_error: Option<String> = None;

        for attempt in 1..=max_attempts {
            let start = std::time::Instant::now();

            let result = match action {
                "navigate" => {
                    let url = match &step.ui_bridge_url {
                        Some(u) => u.clone(),
                        None => {
                            return StepHandlerResult::failure(
                                "UI Bridge navigate requires 'url' field",
                            );
                        }
                    };
                    let endpoint = format!("{}/sdk/page/navigate", base_url.trim_end_matches('/'));
                    let body = serde_json::json!({ "url": url });
                    client.post(&endpoint).json(&body).send().await
                }
                "execute" => {
                    let instruction = step.ui_bridge_instruction.as_deref().unwrap_or("");
                    let endpoint = format!("{}/sdk/ai/execute", base_url.trim_end_matches('/'));
                    let mut body = serde_json::json!({ "instruction": instruction });
                    if let Some(timeout) = step.ui_bridge_timeout_ms {
                        body["timeout"] = serde_json::json!(timeout);
                    }
                    client.post(&endpoint).json(&body).send().await
                }
                "assert" => {
                    let target = step.ui_bridge_target.as_deref().unwrap_or("");
                    let assert_type = step.ui_bridge_assert_type.as_deref().unwrap_or("exists");
                    let expected = step.ui_bridge_expected.as_deref();
                    let endpoint = format!("{}/sdk/ai/assert", base_url.trim_end_matches('/'));
                    let mut body = serde_json::json!({
                        "target": target,
                        "type": assert_type,
                    });
                    if let Some(exp) = expected {
                        body["expected"] = serde_json::json!(exp);
                    }
                    client.post(&endpoint).json(&body).send().await
                }
                "snapshot" => {
                    let endpoint = format!("{}/control/snapshot", base_url.trim_end_matches('/'));
                    client.get(&endpoint).send().await
                }
                other => {
                    return StepHandlerResult::failure(format!(
                        "Unknown UI Bridge action: {}",
                        other
                    ));
                }
            };

            match result {
                Ok(response) => {
                    let status = response.status();
                    let elapsed = start.elapsed();

                    // (#3) Proper error propagation for response body read
                    let body_text = match response.text().await {
                        Ok(t) => t,
                        Err(e) => {
                            let msg = format!("Failed to read response body: {}", e);
                            if is_retryable_error(&e) && attempt < max_attempts {
                                warn!(
                                    "UI Bridge {} attempt {}/{} failed: {}, retrying...",
                                    action, attempt, max_attempts, msg
                                );
                                last_error = Some(msg);
                                tokio::time::sleep(std::time::Duration::from_millis(
                                    200 * (1 << (attempt - 1)),
                                ))
                                .await;
                                continue;
                            }
                            let result = StepHandlerResult::failure(msg);
                            self.track_result(context, base_url, &result).await;
                            return result;
                        }
                    };

                    // (#1) Retry on 5xx server errors
                    if is_retryable_status(status) && attempt < max_attempts {
                        let msg = format!(
                            "UI Bridge {} returned server error {}: {}",
                            action, status, body_text
                        );
                        warn!(
                            "UI Bridge {} attempt {}/{} failed: {}, retrying...",
                            action, attempt, max_attempts, msg
                        );
                        last_error = Some(msg);
                        tokio::time::sleep(std::time::Duration::from_millis(
                            200 * (1 << (attempt - 1)),
                        ))
                        .await;
                        continue;
                    }

                    // (#4) Warn on JSON parse failures
                    let output_data: serde_json::Value = match serde_json::from_str(&body_text) {
                        Ok(v) => v,
                        Err(e) => {
                            warn!("Failed to parse UI Bridge response as JSON: {}", e);
                            serde_json::json!({ "raw_response": body_text })
                        }
                    };

                    let success = status.is_success();
                    let error = if !success {
                        Some(format!(
                            "UI Bridge {} returned status {}: {}",
                            action, status, body_text
                        ))
                    } else {
                        None
                    };

                    // (#5) Response time logging
                    info!(
                        "UI Bridge {} completed in {:?} (status: {})",
                        action, elapsed, status
                    );

                    let result = StepHandlerResult {
                        success,
                        error,
                        output_data: Some(output_data),
                        screenshot_path: None,
                    };
                    self.track_result(context, base_url, &result).await;
                    return result;
                }
                Err(e) => {
                    let msg = format!("UI Bridge {} request failed: {}", action, e);

                    // (#1) Retry on transient network errors
                    if is_retryable_error(&e) && attempt < max_attempts {
                        warn!(
                            "UI Bridge {} attempt {}/{} failed: {}, retrying...",
                            action, attempt, max_attempts, msg
                        );
                        last_error = Some(msg);
                        tokio::time::sleep(std::time::Duration::from_millis(
                            200 * (1 << (attempt - 1)),
                        ))
                        .await;
                        continue;
                    }

                    error!("{}", msg);
                    let result = StepHandlerResult::failure(msg);
                    self.track_result(context, base_url, &result).await;
                    return result;
                }
            }
        }

        // All retries exhausted
        let msg = last_error.unwrap_or_else(|| {
            format!(
                "UI Bridge {} failed after {} attempts",
                action, max_attempts
            )
        });
        error!("{}", msg);
        let result = StepHandlerResult::failure(msg);
        self.track_result(context, base_url, &result).await;
        result
    }
}

impl UiBridgeHandler {
    /// Track success/failure in the failure tracker and optionally run AI diagnosis.
    async fn track_result(
        &self,
        context: &HandlerContext,
        base_url: &str,
        result: &StepHandlerResult,
    ) {
        let tracker = &context.app_state.ui_bridge_failure_tracker;

        if result.success {
            tracker.record_success(base_url).await;
        } else if let Some(ref err) = result.error {
            let count = tracker.record_failure(base_url, err).await;
            if count >= 3 {
                // Run AI diagnostic
                let recent_errors = tracker.get_recent_errors(base_url, 5).await;
                tracker.reset(base_url).await;
                self.run_ai_diagnostic(context, base_url, &recent_errors)
                    .await;
            }
        }
    }

    /// Run an AI diagnostic prompt after repeated failures.
    async fn run_ai_diagnostic(
        &self,
        context: &HandlerContext,
        base_url: &str,
        recent_errors: &[String],
    ) {
        let errors_text = recent_errors
            .iter()
            .enumerate()
            .map(|(i, e)| format!("{}. {}", i + 1, e))
            .collect::<Vec<_>>()
            .join("\n");

        // Best-effort snapshot for additional context
        let snapshot_text = match reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(2))
            .build()
        {
            Ok(hc) => {
                let snap_url = format!("{}/control/snapshot", base_url.trim_end_matches('/'));
                match hc.get(&snap_url).send().await {
                    Ok(resp) => resp.text().await.unwrap_or_default(),
                    Err(_) => String::new(),
                }
            }
            Err(_) => String::new(),
        };

        let snapshot_section = if snapshot_text.is_empty() {
            String::new()
        } else {
            format!(
                "\n\nCurrent UI Bridge snapshot (if available):\n{}",
                &snapshot_text[..snapshot_text.len().min(2000)]
            )
        };

        let prompt = format!(
            "The UI Bridge at {} has failed 3+ times consecutively. \
             Diagnose the likely root cause and suggest a fix.\n\n\
             Recent errors:\n{}{}\n\n\
             Respond with a concise diagnosis (2-3 sentences max).",
            base_url, errors_text, snapshot_section
        );

        let task_context = crate::ai_router::TaskContext::from_prompt(&prompt);
        let prompt_clone = prompt.clone();
        let doctor_handle = context.app_state.doctor_handle.lock().await.clone();

        let ai_result = tokio::task::spawn_blocking(move || {
            crate::ai_provider::run_prompt_with_routing(
                &prompt_clone,
                &task_context,
                doctor_handle.as_ref(),
            )
        })
        .await;

        match ai_result {
            Ok(response) if response.success => {
                warn!(
                    "AI diagnosis for UI Bridge at {}: {}",
                    base_url, response.output
                );
            }
            Ok(response) => {
                warn!(
                    "AI diagnosis request failed for {}: {}",
                    base_url,
                    response.error.unwrap_or_else(|| "unknown".to_string())
                );
            }
            Err(e) => {
                warn!("AI diagnosis task panicked for {}: {}", base_url, e);
            }
        }
    }

    /// Execute a "compare" action:
    /// 1. Take a snapshot of the current app state
    /// 2. Load the reference snapshot (from step config or saved snapshot store)
    /// 3. Call the AI compare endpoint
    /// 4. Return ComparisonResult as step output
    /// 5. Pass/fail based on severity_threshold
    async fn execute_compare(
        &self,
        step: &ExecutionStepConfig,
        _context: &HandlerContext,
        client: &reqwest::Client,
        base_url: &str,
        _timeout_ms: u64,
    ) -> StepHandlerResult {
        let comparison_mode = step
            .ui_bridge_compare_mode
            .as_deref()
            .unwrap_or("structural");
        let severity_threshold = step
            .ui_bridge_severity_threshold
            .as_deref()
            .unwrap_or("major");

        // Step 1: Take a snapshot of the current app state
        info!("Compare: taking target snapshot...");
        let snapshot_start = std::time::Instant::now();
        let snapshot_endpoint = format!("{}/control/snapshot", base_url.trim_end_matches('/'));
        let target_snapshot = match client.get(&snapshot_endpoint).send().await {
            Ok(resp) if resp.status().is_success() => {
                // (#3) Proper error propagation
                let body = match resp.text().await {
                    Ok(t) => t,
                    Err(e) => {
                        return StepHandlerResult::failure(format!(
                            "Failed to read target snapshot response body: {}",
                            e
                        ));
                    }
                };
                // (#4) Warn on parse failure
                match serde_json::from_str::<serde_json::Value>(&body) {
                    Ok(v) => v,
                    Err(e) => {
                        warn!("Failed to parse target snapshot as JSON: {}", e);
                        serde_json::json!({ "raw": body })
                    }
                }
            }
            Ok(resp) => {
                let status = resp.status();
                return StepHandlerResult::failure(format!(
                    "Failed to take target snapshot: HTTP {}",
                    status
                ));
            }
            Err(e) => {
                return StepHandlerResult::failure(format!(
                    "Failed to take target snapshot: {}",
                    e
                ));
            }
        };
        info!(
            "Compare: target snapshot taken in {:?}",
            snapshot_start.elapsed()
        );

        // Step 2: Load reference snapshot
        let ref_start = std::time::Instant::now();
        let reference_snapshot = if let Some(ref snap) = step.ui_bridge_reference_snapshot {
            // Inline reference snapshot provided in step config
            snap.clone()
        } else if let Some(ref snap_id) = step.ui_bridge_reference_snapshot_id {
            // Load from saved snapshots via runner API
            info!("Compare: loading saved reference snapshot {}...", snap_id);
            let origin = extract_origin(base_url);
            let snap_endpoint = format!("{}/comparison-snapshots/{}", origin, snap_id);
            match client.get(&snap_endpoint).send().await {
                Ok(resp) if resp.status().is_success() => {
                    // (#3) Proper error propagation
                    let body = match resp.text().await {
                        Ok(t) => t,
                        Err(e) => {
                            return StepHandlerResult::failure(format!(
                                "Failed to read reference snapshot response body: {}",
                                e
                            ));
                        }
                    };
                    // (#4) Warn on parse failure
                    let saved: serde_json::Value = match serde_json::from_str(&body) {
                        Ok(v) => v,
                        Err(e) => {
                            warn!("Failed to parse reference snapshot as JSON: {}", e);
                            serde_json::json!({ "raw": body })
                        }
                    };
                    // The saved snapshot has snapshot_data field
                    saved.get("snapshot_data").cloned().unwrap_or(saved)
                }
                Ok(resp) => {
                    return StepHandlerResult::failure(format!(
                        "Failed to load reference snapshot {}: HTTP {}",
                        snap_id,
                        resp.status()
                    ));
                }
                Err(e) => {
                    return StepHandlerResult::failure(format!(
                        "Failed to load reference snapshot {}: {}",
                        snap_id, e
                    ));
                }
            }
        } else {
            return StepHandlerResult::failure(
                "Compare action requires either reference_snapshot or reference_snapshot_id",
            );
        };
        info!(
            "Compare: reference snapshot loaded in {:?}",
            ref_start.elapsed()
        );

        // Step 3: Call AI comparison endpoint
        info!(
            "Compare: running AI comparison (mode={})...",
            comparison_mode
        );
        let compare_start = std::time::Instant::now();
        let origin = extract_origin(base_url);
        let compare_endpoint = format!("{}/ai/compare-snapshots", origin);
        let compare_body = serde_json::json!({
            "reference_snapshot": reference_snapshot,
            "target_snapshot": target_snapshot,
            "comparison_mode": comparison_mode,
        });

        let comparison_json = match client
            .post(compare_endpoint)
            .json(&compare_body)
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => {
                // (#3) Proper error propagation
                let body = match resp.text().await {
                    Ok(t) => t,
                    Err(e) => {
                        return StepHandlerResult::failure(format!(
                            "Failed to read comparison response body: {}",
                            e
                        ));
                    }
                };
                body
            }
            Ok(resp) => {
                let status = resp.status();
                // (#3) Proper error propagation
                let body = match resp.text().await {
                    Ok(t) => t,
                    Err(e) => format!("(failed to read body: {})", e),
                };
                return StepHandlerResult::failure(format!(
                    "AI comparison failed: HTTP {} - {}",
                    status, body
                ));
            }
            Err(e) => {
                return StepHandlerResult::failure(format!("AI comparison request failed: {}", e));
            }
        };
        info!(
            "Compare: AI comparison completed in {:?}",
            compare_start.elapsed()
        );

        // (#6) Typed deserialization — clear error on parse failure
        let comparison = match serde_json::from_str::<ComparisonResult>(&comparison_json) {
            Ok(c) => c,
            Err(e) => {
                warn!(
                    "Failed to parse comparison result as typed struct: {}. Raw: {}",
                    e,
                    &comparison_json[..comparison_json.len().min(500)]
                );
                return StepHandlerResult::failure(format!(
                    "Failed to parse comparison result: {}",
                    e
                ));
            }
        };

        // Preserve the raw JSON for frontend rendering
        let comparison_result_json: serde_json::Value =
            serde_json::from_str(&comparison_json).unwrap_or_default();

        // Step 4: Evaluate pass/fail based on severity threshold
        let threshold_order = severity_to_order(severity_threshold);
        let failing_count = compute_failing_count(
            threshold_order,
            comparison.critical_count,
            comparison.major_count,
            comparison.minor_count,
            comparison.info_count,
        );

        // Build output with comparison_result embedded for frontend rendering
        let output_data = serde_json::json!({
            "comparison_result": comparison_result_json,
            "severity_threshold": severity_threshold,
            "failing_count": failing_count,
            "total_differences": comparison.total_differences,
        });

        if failing_count > 0 {
            warn!(
                "Compare: {} findings at or above '{}' threshold (total: {})",
                failing_count, severity_threshold, comparison.total_differences
            );
            StepHandlerResult::failure_with_data(
                format!(
                    "Comparison found {} finding(s) at or above '{}' severity: {}",
                    failing_count, severity_threshold, comparison.summary
                ),
                output_data,
            )
        } else {
            info!(
                "Compare: passed (total differences: {}, none at or above '{}')",
                comparison.total_differences, severity_threshold
            );
            StepHandlerResult::success_with_data(output_data)
        }
    }
}

// ---------------------------------------------------------------------------
// Unit tests (#10)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // --- severity_to_order tests ---

    #[test]
    fn test_severity_to_order_critical() {
        assert_eq!(severity_to_order("critical"), 4);
    }

    #[test]
    fn test_severity_to_order_major() {
        assert_eq!(severity_to_order("major"), 3);
    }

    #[test]
    fn test_severity_to_order_minor() {
        assert_eq!(severity_to_order("minor"), 2);
    }

    #[test]
    fn test_severity_to_order_info() {
        assert_eq!(severity_to_order("info"), 1);
    }

    #[test]
    fn test_severity_to_order_unknown_defaults_to_major() {
        // Unknown threshold should default to major (3)
        assert_eq!(severity_to_order("banana"), 3);
        assert_eq!(severity_to_order(""), 3);
        assert_eq!(severity_to_order("CRITICAL"), 3); // case-sensitive
    }

    // --- compute_failing_count tests ---

    #[test]
    fn test_compute_failing_count_critical_only() {
        assert_eq!(compute_failing_count(4, 2, 5, 3, 1), 2);
    }

    #[test]
    fn test_compute_failing_count_major_and_above() {
        assert_eq!(compute_failing_count(3, 2, 5, 3, 1), 7);
    }

    #[test]
    fn test_compute_failing_count_minor_and_above() {
        assert_eq!(compute_failing_count(2, 2, 5, 3, 1), 10);
    }

    #[test]
    fn test_compute_failing_count_all() {
        assert_eq!(compute_failing_count(1, 2, 5, 3, 1), 11);
    }

    #[test]
    fn test_compute_failing_count_all_zeros() {
        assert_eq!(compute_failing_count(1, 0, 0, 0, 0), 0);
        assert_eq!(compute_failing_count(4, 0, 0, 0, 0), 0);
    }

    #[test]
    fn test_compute_failing_count_unknown_threshold_defaults() {
        // Threshold order 99 falls to default branch: critical + major
        assert_eq!(compute_failing_count(99, 2, 5, 3, 1), 7);
    }

    // --- ComparisonResult deserialization tests ---

    #[test]
    fn test_comparison_result_full_json() {
        let json = r#"{
            "criticalCount": 2,
            "majorCount": 3,
            "minorCount": 5,
            "infoCount": 10,
            "totalDifferences": 20,
            "summary": "Found issues"
        }"#;
        let result: ComparisonResult = serde_json::from_str(json).unwrap();
        assert_eq!(result.critical_count, 2);
        assert_eq!(result.major_count, 3);
        assert_eq!(result.minor_count, 5);
        assert_eq!(result.info_count, 10);
        assert_eq!(result.total_differences, 20);
        assert_eq!(result.summary, "Found issues");
    }

    #[test]
    fn test_comparison_result_missing_fields_default() {
        let json = r#"{ "criticalCount": 1 }"#;
        let result: ComparisonResult = serde_json::from_str(json).unwrap();
        assert_eq!(result.critical_count, 1);
        assert_eq!(result.major_count, 0);
        assert_eq!(result.minor_count, 0);
        assert_eq!(result.info_count, 0);
        assert_eq!(result.total_differences, 0);
        assert_eq!(result.summary, "Comparison completed");
    }

    #[test]
    fn test_comparison_result_empty_json() {
        let json = r#"{}"#;
        let result: ComparisonResult = serde_json::from_str(json).unwrap();
        assert_eq!(result.critical_count, 0);
        assert_eq!(result.major_count, 0);
        assert_eq!(result.total_differences, 0);
        assert_eq!(result.summary, "Comparison completed");
    }

    #[test]
    fn test_comparison_result_invalid_json() {
        let json = r#"not valid json"#;
        let result = serde_json::from_str::<ComparisonResult>(json);
        assert!(result.is_err());
    }

    // --- extract_origin tests ---

    #[test]
    fn test_extract_origin_with_path() {
        assert_eq!(
            extract_origin("http://localhost:9876/ui-bridge"),
            "http://localhost:9876"
        );
    }

    #[test]
    fn test_extract_origin_no_path() {
        assert_eq!(
            extract_origin("http://localhost:9876"),
            "http://localhost:9876"
        );
    }

    #[test]
    fn test_extract_origin_deep_path() {
        assert_eq!(
            extract_origin("http://localhost:3001/api/ui-bridge/sdk/execute"),
            "http://localhost:3001"
        );
    }

    #[test]
    fn test_extract_origin_no_scheme_fallback() {
        assert_eq!(extract_origin("localhost:9876"), "http://localhost:9876");
    }

    // --- UiBridgeFailureTracker tests ---

    #[tokio::test]
    async fn test_failure_tracker_record_and_count() {
        let tracker = UiBridgeFailureTracker::new();
        assert_eq!(
            tracker
                .record_failure("http://localhost:9876", "timeout")
                .await,
            1
        );
        assert_eq!(
            tracker
                .record_failure("http://localhost:9876", "connection refused")
                .await,
            2
        );
        assert_eq!(
            tracker
                .record_failure("http://localhost:9876", "500 error")
                .await,
            3
        );
    }

    #[tokio::test]
    async fn test_failure_tracker_success_resets() {
        let tracker = UiBridgeFailureTracker::new();
        tracker
            .record_failure("http://localhost:9876", "error1")
            .await;
        tracker
            .record_failure("http://localhost:9876", "error2")
            .await;
        tracker.record_success("http://localhost:9876").await;
        assert_eq!(
            tracker
                .record_failure("http://localhost:9876", "error3")
                .await,
            1 // reset to 1 after success
        );
    }

    #[tokio::test]
    async fn test_failure_tracker_get_recent_errors() {
        let tracker = UiBridgeFailureTracker::new();
        tracker
            .record_failure("http://localhost:9876", "error1")
            .await;
        tracker
            .record_failure("http://localhost:9876", "error2")
            .await;
        tracker
            .record_failure("http://localhost:9876", "error3")
            .await;

        let recent = tracker.get_recent_errors("http://localhost:9876", 2).await;
        assert_eq!(recent.len(), 2);
        // Most recent first
        assert_eq!(recent[0], "error3");
        assert_eq!(recent[1], "error2");
    }

    #[tokio::test]
    async fn test_failure_tracker_different_urls_independent() {
        let tracker = UiBridgeFailureTracker::new();
        tracker.record_failure("http://url-a", "err1").await;
        tracker.record_failure("http://url-a", "err2").await;
        assert_eq!(tracker.record_failure("http://url-b", "err3").await, 1);
        assert_eq!(tracker.record_failure("http://url-a", "err4").await, 3);
    }

    #[tokio::test]
    async fn test_failure_tracker_reset() {
        let tracker = UiBridgeFailureTracker::new();
        tracker.record_failure("http://localhost:9876", "err").await;
        tracker.reset("http://localhost:9876").await;
        let recent = tracker.get_recent_errors("http://localhost:9876", 10).await;
        assert!(recent.is_empty());
    }
}

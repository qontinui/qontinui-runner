//! UI Bridge handlers for MCP API
//!
//! Provides HTTP handlers for UI Bridge control (React UI automation)
//! and UI Bridge exploration (qontinui library via Python bridge).

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::Json,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Instant;
use tracing::{debug, error, info, warn};

use tauri::Emitter;

use crate::executor::with_default_bridge;
use crate::mcp::types::{api_error, api_error_detailed, ApiResponse, ApiState};
use crate::timeout_config::Timeouts;

// ============================================================================
// Types
// ============================================================================

/// Request to execute an action on an element
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UIBridgeActionRequest {
    action: String,
    #[serde(default)]
    params: Option<serde_json::Value>,
    #[serde(default)]
    wait_options: Option<serde_json::Value>,
    /// Capture any extra top-level fields (e.g., targetPosition, text, clear)
    /// so they can be merged into params for actions that accept flat format.
    #[serde(flatten)]
    extra: serde_json::Map<String, serde_json::Value>,
}

/// Request to execute an action on a component
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UIBridgeComponentActionRequest {
    #[serde(default)]
    params: Option<serde_json::Value>,
}

/// Discovery options request
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UIBridgeDiscoveryRequest {
    #[serde(default)]
    root: Option<String>,
    #[serde(default, alias = "interactive_only")]
    interactive_only: Option<bool>,
    #[serde(default, alias = "include_hidden")]
    include_hidden: Option<bool>,
    #[serde(default)]
    limit: Option<u32>,
    #[serde(default)]
    types: Option<Vec<String>>,
    #[serde(default)]
    selector: Option<String>,
}

/// Request to start UI Bridge exploration
#[derive(Debug, Deserialize)]
pub struct StartUIBridgeExplorationRequest {
    /// Target type: "web", "desktop", or "mobile"
    #[serde(default = "default_target_type")]
    pub target_type: String,
    /// Connection URL for the target application
    pub connection_url: String,
    /// Maximum navigation depth (default: 2)
    #[serde(default)]
    pub max_depth: Option<i32>,
    /// Maximum elements per page (default: 20)
    #[serde(default)]
    pub max_elements_per_page: Option<i32>,
    /// Maximum total elements to explore (default: 100)
    #[serde(default)]
    pub max_total_elements: Option<i32>,
    /// Delay between actions in milliseconds (default: 500)
    #[serde(default)]
    pub action_delay_ms: Option<i32>,
    /// Keywords in element text/id to skip
    #[serde(default)]
    pub blocked_keywords: Option<Vec<String>>,
    /// Keywords that are always safe to interact with
    #[serde(default)]
    pub safe_keywords: Option<Vec<String>>,
    /// CSS selectors to skip
    #[serde(default)]
    pub blocked_selectors: Option<Vec<String>>,
    /// Whether to capture screenshots (default: false)
    #[serde(default)]
    pub capture_screenshots: Option<bool>,
    /// Whether to run state discovery on results (default: true)
    #[serde(default)]
    pub run_state_discovery: Option<bool>,
}

fn default_target_type() -> String {
    "web".to_string()
}

/// Request to write to the clipboard
#[derive(Debug, Deserialize)]
pub struct ClipboardWriteRequest {
    pub text: String,
    #[serde(default)]
    pub html: Option<String>,
}

/// Request for getting UI Bridge exploration status
#[derive(Debug, Deserialize)]
pub struct UIBridgeExplorationStatusRequest {
    pub job_id: Option<String>,
}

/// Request for discovering states from render logs
#[derive(Debug, Deserialize)]
pub struct DiscoverStatesRequest {
    /// Array of DOM snapshot render log entries
    pub render_logs: Vec<serde_json::Value>,
}

// ============================================================================
// Helper Functions
// ============================================================================

/// UI Bridge timeout is fetched from centralized config
/// This needs a reasonable timeout since it's synchronous communication with the frontend.
fn get_ui_bridge_timeout_ms() -> u64 {
    Timeouts::ui_bridge_ipc().as_millis() as u64
}

// ============================================================================
// Structured Error Taxonomy
// ============================================================================

/// Machine-readable error codes for UI Bridge operations.
/// Enables AI agents to match on error type rather than parsing strings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum UiBridgeErrorCode {
    // Transport errors
    Timeout,
    CircuitBreakerOpen,
    ConcurrencyLimitReached,
    FrontendUnresponsive,
    WindowNotFound,
    // Element targeting errors
    ElementNotFound,
    ElementNotVisible,
    ElementNotEnabled,
    ElementStale,
    // Action errors
    ActionFailed,
    // Assertion errors
    AssertionFailed,
    UnknownAssertionType,
    // System errors
    InternalError,
}

/// Machine-readable recovery hints telling AI agents what to do next.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RecoveryHint {
    /// Re-take a DOM snapshot; element refs may have changed
    Resnapshot,
    /// Retry the same operation after a delay (milliseconds)
    RetryAfterMs(u64),
    /// Wait for the circuit breaker cooldown to expire
    WaitForRecovery,
    /// Scroll or navigate to make the element visible
    ScrollIntoView,
    /// The element exists but is disabled; wait for it to become enabled
    WaitForEnabled,
    /// Use a different selector or broaden the search criteria
    BroadenSelector,
    /// The operation cannot be recovered; skip or report failure
    Unrecoverable,
}

/// Structured error with machine-readable code and recovery hint.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiBridgeError {
    pub code: UiBridgeErrorCode,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recovery: Option<RecoveryHint>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<serde_json::Value>,
}

impl UiBridgeError {
    pub fn timeout(duration_ms: u64) -> Self {
        Self {
            code: UiBridgeErrorCode::Timeout,
            message: format!("UI Bridge request timed out after {}ms", duration_ms),
            recovery: Some(RecoveryHint::RetryAfterMs(1000)),
            context: Some(serde_json::json!({"timeout_ms": duration_ms})),
        }
    }

    pub fn circuit_breaker_open() -> Self {
        Self {
            code: UiBridgeErrorCode::CircuitBreakerOpen,
            message: "UI Bridge temporarily unavailable (circuit breaker open)".to_string(),
            recovery: Some(RecoveryHint::WaitForRecovery),
            context: None,
        }
    }

    pub fn concurrency_limit() -> Self {
        Self {
            code: UiBridgeErrorCode::ConcurrencyLimitReached,
            message: "UI Bridge concurrency limit reached (timeout acquiring permit)".to_string(),
            recovery: Some(RecoveryHint::RetryAfterMs(500)),
            context: None,
        }
    }

    pub fn window_not_found() -> Self {
        Self {
            code: UiBridgeErrorCode::WindowNotFound,
            message: "Main webview window not found".to_string(),
            recovery: Some(RecoveryHint::Unrecoverable),
            context: None,
        }
    }

    pub fn element_not_found(selector: &str) -> Self {
        Self {
            code: UiBridgeErrorCode::ElementNotFound,
            message: format!("No element found matching criteria {}", selector),
            recovery: Some(RecoveryHint::Resnapshot),
            context: Some(serde_json::json!({"selector": selector})),
        }
    }

    pub fn element_not_visible(selector: &str, total_found: usize) -> Self {
        Self {
            code: UiBridgeErrorCode::ElementNotVisible,
            message: format!(
                "Found {} element(s) matching '{}' but none are visible",
                total_found, selector
            ),
            recovery: Some(RecoveryHint::ScrollIntoView),
            context: Some(serde_json::json!({"selector": selector, "total_found": total_found})),
        }
    }

    pub fn action_failed(message: impl Into<String>) -> Self {
        Self {
            code: UiBridgeErrorCode::ActionFailed,
            message: message.into(),
            recovery: Some(RecoveryHint::Resnapshot),
            context: None,
        }
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self {
            code: UiBridgeErrorCode::InternalError,
            message: message.into(),
            recovery: Some(RecoveryHint::Unrecoverable),
            context: None,
        }
    }
}

/// Classify a transport-level error string into a structured error.
/// Used by wrap_ipc_result and batch handler to convert legacy string errors.
/// Checks both transport-level and assertion-level patterns so that frontend
/// error messages like "No element found" get the correct error code.
pub fn classify_transport_error(error_msg: &str) -> UiBridgeError {
    // Transport-level errors
    if error_msg.contains("timed out") {
        UiBridgeError::timeout(0)
    } else if error_msg.contains("circuit breaker") {
        UiBridgeError::circuit_breaker_open()
    } else if error_msg.contains("concurrency limit") {
        UiBridgeError::concurrency_limit()
    } else if error_msg.contains("window not found") || error_msg.contains("Window not found") {
        UiBridgeError::window_not_found()
    }
    // Assertion/element-level errors (from frontend IPC responses)
    else if error_msg.contains("No element found") || error_msg.contains("no element found") {
        UiBridgeError::element_not_found(error_msg)
    } else if error_msg.contains("none are visible") {
        UiBridgeError::element_not_visible(error_msg, 0)
    } else if error_msg.contains("Operation failed") || error_msg.contains("action failed") {
        UiBridgeError::action_failed(error_msg)
    } else {
        UiBridgeError::internal(error_msg)
    }
}

/// Classify an assertion failure detail string into an error code.
/// Used by the verification phase to annotate failure context for AI agents.
pub fn classify_assertion_failure(detail: &str) -> UiBridgeErrorCode {
    if detail.contains("No element found") {
        UiBridgeErrorCode::ElementNotFound
    } else if detail.contains("none are visible") {
        UiBridgeErrorCode::ElementNotVisible
    } else if detail.contains("Unknown assertion type") {
        UiBridgeErrorCode::UnknownAssertionType
    } else {
        UiBridgeErrorCode::AssertionFailed
    }
}

/// Get the recovery hint for an error code.
pub fn recovery_hint_for(code: &UiBridgeErrorCode) -> RecoveryHint {
    match code {
        UiBridgeErrorCode::Timeout => RecoveryHint::RetryAfterMs(1000),
        UiBridgeErrorCode::CircuitBreakerOpen => RecoveryHint::WaitForRecovery,
        UiBridgeErrorCode::ConcurrencyLimitReached => RecoveryHint::RetryAfterMs(500),
        UiBridgeErrorCode::FrontendUnresponsive => RecoveryHint::WaitForRecovery,
        UiBridgeErrorCode::WindowNotFound => RecoveryHint::Unrecoverable,
        UiBridgeErrorCode::ElementNotFound => RecoveryHint::Resnapshot,
        UiBridgeErrorCode::ElementNotVisible => RecoveryHint::ScrollIntoView,
        UiBridgeErrorCode::ElementNotEnabled => RecoveryHint::WaitForEnabled,
        UiBridgeErrorCode::ElementStale => RecoveryHint::Resnapshot,
        UiBridgeErrorCode::ActionFailed => RecoveryHint::Resnapshot,
        UiBridgeErrorCode::AssertionFailed => RecoveryHint::BroadenSelector,
        UiBridgeErrorCode::UnknownAssertionType => RecoveryHint::Unrecoverable,
        UiBridgeErrorCode::InternalError => RecoveryHint::Unrecoverable,
    }
}

// ============================================================================
// Circuit Breaker
// ============================================================================

/// Circuit breaker states for UI Bridge
#[derive(Debug, Clone, PartialEq)]
pub enum CircuitBreakerState {
    Closed,
    Open,
    HalfOpen,
}

/// Circuit breaker to prevent cascading failures when the webview is unresponsive.
///
/// Uses a rolling-window failure counter instead of a simple consecutive counter.
/// Failures older than `window_ms` are pruned automatically.
pub struct UiBridgeCircuitBreaker {
    state: tokio::sync::Mutex<CircuitBreakerState>,
    /// Rolling window of failure timestamps (epoch ms)
    failure_timestamps: tokio::sync::Mutex<Vec<u64>>,
    last_failure_time: std::sync::atomic::AtomicU64,
    /// Threshold: failures within the rolling window before opening
    threshold: u32,
    /// Cooldown in ms before transitioning from Open to HalfOpen
    cooldown_ms: u64,
    /// Rolling window size in ms — failures older than this are pruned
    window_ms: u64,
    /// Counts recovery attempts since last success to prevent infinite loops
    recovery_attempts: std::sync::atomic::AtomicU32,
    /// Timestamp of the last recovery attempt in ms
    last_recovery_time: std::sync::atomic::AtomicU64,
}

impl UiBridgeCircuitBreaker {
    pub fn new() -> Self {
        Self {
            state: tokio::sync::Mutex::new(CircuitBreakerState::Closed),
            failure_timestamps: tokio::sync::Mutex::new(Vec::new()),
            last_failure_time: std::sync::atomic::AtomicU64::new(0),
            threshold: 5,
            cooldown_ms: 15000,
            window_ms: 30000,
            recovery_attempts: std::sync::atomic::AtomicU32::new(0),
            last_recovery_time: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// Check if a request should be allowed through
    pub async fn check(&self) -> Result<(), String> {
        let mut state = self.state.lock().await;
        match *state {
            CircuitBreakerState::Closed => Ok(()),
            CircuitBreakerState::Open => {
                let last_failure = self
                    .last_failure_time
                    .load(std::sync::atomic::Ordering::Relaxed);
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64;
                if now - last_failure >= self.cooldown_ms {
                    *state = CircuitBreakerState::HalfOpen;
                    info!("UI Bridge circuit breaker: Open -> HalfOpen (cooldown elapsed)");
                    Ok(())
                } else {
                    Err("UI Bridge temporarily unavailable (circuit breaker open)".to_string())
                }
            }
            CircuitBreakerState::HalfOpen => Ok(()),
        }
    }

    /// Record a successful request
    pub async fn record_success(&self) {
        // Clear the rolling window on success
        {
            let mut timestamps = self.failure_timestamps.lock().await;
            timestamps.clear();
        }
        self.recovery_attempts
            .store(0, std::sync::atomic::Ordering::Relaxed);
        let mut state = self.state.lock().await;
        if *state != CircuitBreakerState::Closed {
            info!(
                "UI Bridge circuit breaker: {:?} -> Closed (success)",
                *state
            );
            *state = CircuitBreakerState::Closed;
        }
    }

    /// Record a failed request (timeout).
    ///
    /// Uses a rolling window: only failures within the last `window_ms` count
    /// towards the threshold.
    pub async fn record_failure(&self) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        self.last_failure_time
            .store(now, std::sync::atomic::Ordering::Relaxed);

        let count = {
            let mut timestamps = self.failure_timestamps.lock().await;
            // Add current failure
            timestamps.push(now);
            // Prune entries older than the rolling window
            let cutoff = now.saturating_sub(self.window_ms);
            timestamps.retain(|&ts| ts >= cutoff);
            timestamps.len() as u32
        };

        if count >= self.threshold {
            let mut state = self.state.lock().await;
            if *state != CircuitBreakerState::Open {
                warn!(
                    "UI Bridge circuit breaker: {:?} -> Open ({} failures in {}s window)",
                    *state,
                    count,
                    self.window_ms / 1000
                );
                *state = CircuitBreakerState::Open;
            }
        }
    }

    /// Attempt recovery by emitting an event instead of destructively navigating.
    ///
    /// The frontend can listen for `ui-bridge-circuit-open` and show a toast or
    /// attempt reconnection without losing page state.
    pub fn attempt_recovery(&self, app_handle: &tauri::AppHandle) {
        let attempts = self
            .recovery_attempts
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        self.last_recovery_time
            .store(now, std::sync::atomic::Ordering::Relaxed);

        warn!(
            "UI Bridge: Emitting circuit-open event (attempt {})",
            attempts
        );
        if let Err(e) = app_handle.emit(
            "ui-bridge-circuit-open",
            serde_json::json!({
                "recovery_attempt": attempts,
                "timestamp": now,
            }),
        ) {
            error!("UI Bridge: Failed to emit circuit-open event: {}", e);
        }
    }

    /// Manually reset the circuit breaker to Closed state.
    ///
    /// Clears failure timestamps and recovery attempt counters.
    pub async fn reset(&self) {
        {
            let mut timestamps = self.failure_timestamps.lock().await;
            timestamps.clear();
        }
        self.recovery_attempts
            .store(0, std::sync::atomic::Ordering::Relaxed);
        self.last_recovery_time
            .store(0, std::sync::atomic::Ordering::Relaxed);
        self.last_failure_time
            .store(0, std::sync::atomic::Ordering::Relaxed);
        let mut state = self.state.lock().await;
        info!(
            "UI Bridge circuit breaker: {:?} -> Closed (manual reset)",
            *state
        );
        *state = CircuitBreakerState::Closed;
    }

    /// Get current state for diagnostics
    pub async fn get_state(&self) -> CircuitBreakerState {
        self.state.lock().await.clone()
    }

    /// Get failure count within the rolling window
    pub async fn get_failure_count(&self) -> u32 {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let timestamps = self.failure_timestamps.lock().await;
        let cutoff = now.saturating_sub(self.window_ms);
        timestamps.iter().filter(|&&ts| ts >= cutoff).count() as u32
    }

    /// Get the configured failure threshold.
    pub fn get_threshold(&self) -> u32 {
        self.threshold
    }

    /// Get the configured cooldown period in milliseconds.
    pub fn get_cooldown_ms(&self) -> u64 {
        self.cooldown_ms
    }
}

/// Send a UI Bridge request and wait for the response synchronously.
///
/// This creates a oneshot channel, stores the sender in the pending map,
/// emits the request to the frontend, and waits for the response with a timeout.
///
/// Includes circuit breaker, concurrency limiting, frontend liveness check,
/// and request deduplication for read-only operations.
pub async fn ui_bridge_request_sync(
    state: &Arc<ApiState>,
    request_type: &str,
    additional_payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    // 1. Check circuit breaker
    state.ui_bridge_circuit_breaker.check().await?;

    // 1.5. Wait for frontend readiness if no pong has ever been received.
    // This prevents the race condition where requests arrive before React's
    // event listeners are set up after a supervisor-triggered restart.
    {
        let pong_check = state
            .ui_bridge_last_pong
            .load(std::sync::atomic::Ordering::Relaxed);
        if pong_check == 0 {
            tracing::info!("UI Bridge: Waiting for frontend readiness (no pong received yet)");
            let ready_timeout = std::time::Duration::from_secs(10);
            if tokio::time::timeout(ready_timeout, state.ui_bridge_ready.notified())
                .await
                .is_err()
            {
                return Err(
                    "UI Bridge: Frontend did not become ready within 10s. Is the WebView running?"
                        .to_string(),
                );
            }
            tracing::info!("UI Bridge: Frontend is now ready");
        }
    }

    // 2. Check frontend liveness (warn if stale, but don't fail — let IPC timeout handle it)
    let last_pong = state
        .ui_bridge_last_pong
        .load(std::sync::atomic::Ordering::Relaxed);
    if last_pong > 0 {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let pong_age = now - last_pong;
        if pong_age > 15000 {
            warn!(
                "UI Bridge: Frontend may be unresponsive (last pong {}ms ago)",
                pong_age
            );
        }
    }

    // 3. Check for dedup opportunity on read-only requests
    let dedup_key = match request_type {
        "get_elements" | "get_snapshot" | "get_components" => Some(request_type.to_string()),
        _ => None,
    };

    if let Some(ref key) = dedup_key {
        let dedup = state.ui_bridge_dedup.lock().await;
        if let Some(tx) = dedup.get(key) {
            // Subscribe to existing in-flight request
            let mut rx = tx.subscribe();
            drop(dedup);
            debug!("UI Bridge: Deduplicating {} request", key);
            // Apply the same timeout to dedup waits so stale entries don't block forever
            let timeout_ms = get_ui_bridge_timeout_ms();
            match tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), rx.recv())
                .await
            {
                Ok(Ok(result)) => return result,
                Ok(Err(_)) => return Err("Dedup channel closed".to_string()),
                Err(_) => {
                    // Dedup wait timed out — remove stale entry and fall through
                    // to make a fresh request
                    warn!(
                        "UI Bridge: Dedup wait timed out for {}, removing stale entry and retrying",
                        key
                    );
                    let mut dedup_map = state.ui_bridge_dedup.lock().await;
                    dedup_map.remove(key);
                    // Fall through to make a fresh request below
                }
            }
        }
    }

    // 4. Acquire semaphore permit (max 6 concurrent, 2s timeout)
    let _permit = match tokio::time::timeout(
        std::time::Duration::from_secs(2),
        state.ui_bridge_semaphore.acquire(),
    )
    .await
    {
        Ok(Ok(permit)) => permit,
        Ok(Err(_)) => return Err("UI Bridge semaphore closed".to_string()),
        Err(_) => {
            return Err(
                "UI Bridge concurrency limit reached (timeout acquiring permit)".to_string(),
            )
        }
    };

    // 5. Set up dedup broadcast for read-only requests
    let dedup_tx = if let Some(ref key) = dedup_key {
        let (tx, _) = tokio::sync::broadcast::channel(1);
        let mut dedup = state.ui_bridge_dedup.lock().await;
        dedup.insert(key.clone(), tx.clone());
        Some(tx)
    } else {
        None
    };

    // 6. Execute the actual request
    let result = ui_bridge_request_inner(state, request_type, additional_payload).await;

    // 7. Update circuit breaker and attempt recovery if it opens
    match &result {
        Ok(_) => state.ui_bridge_circuit_breaker.record_success().await,
        Err(e) if e.contains("timed out") => {
            state.ui_bridge_circuit_breaker.record_failure().await;
            // If circuit breaker just opened, attempt auto-recovery
            if state.ui_bridge_circuit_breaker.get_state().await == CircuitBreakerState::Open {
                state
                    .ui_bridge_circuit_breaker
                    .attempt_recovery(&state.app_handle);
            }
        }
        Err(_) => {} // Non-timeout errors don't trigger circuit breaker
    }

    // 8. Broadcast dedup result
    if let (Some(ref key), Some(tx)) = (&dedup_key, &dedup_tx) {
        let _ = tx.send(result.clone());
        let mut dedup = state.ui_bridge_dedup.lock().await;
        dedup.remove(key);
    }

    result
}

/// Inner implementation of ui_bridge_request_sync (the actual IPC logic)
async fn ui_bridge_request_inner(
    state: &Arc<ApiState>,
    request_type: &str,
    additional_payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let request_id = uuid::Uuid::new_v4().to_string();

    // Create the full event payload
    let mut event_payload = serde_json::json!({
        "requestId": request_id,
        "type": request_type
    });

    // Merge additional payload fields
    if let (Some(base), Some(additional)) = (
        event_payload.as_object_mut(),
        additional_payload.as_object(),
    ) {
        for (key, value) in additional {
            base.insert(key.clone(), value.clone());
        }
    }

    // Create oneshot channel for the response
    let (tx, rx) = tokio::sync::oneshot::channel::<serde_json::Value>();

    // Store the sender in the pending map
    {
        let mut pending = state.ui_bridge_pending.lock().await;
        pending.insert(request_id.clone(), tx);
        state
            .ui_bridge_pending_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    // Emit request to React frontend
    if let Err(e) = state.app_handle.emit("ui-bridge-request", &event_payload) {
        // Clean up the pending entry
        let mut pending = state.ui_bridge_pending.lock().await;
        if pending.remove(&request_id).is_some() {
            state
                .ui_bridge_pending_count
                .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        }
        return Err(format!("Failed to emit UI Bridge request: {}", e));
    }

    // Wait for response with timeout
    let timeout_duration = std::time::Duration::from_millis(get_ui_bridge_timeout_ms());
    match tokio::time::timeout(timeout_duration, rx).await {
        Ok(Ok(response)) => Ok(response),
        Ok(Err(_)) => Err("UI Bridge request channel closed unexpectedly".to_string()),
        Err(_) => {
            // Timeout - clean up the pending entry
            let mut pending = state.ui_bridge_pending.lock().await;
            if pending.remove(&request_id).is_some() {
                state
                    .ui_bridge_pending_count
                    .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
            }
            Err(format!(
                "UI Bridge request timed out after {}ms. Is the frontend running?",
                get_ui_bridge_timeout_ms()
            ))
        }
    }
}

// ============================================================================
// Public: Response Handler (used by Tauri event listener in create_router)
// ============================================================================

/// Handle incoming UI Bridge response from the frontend.
///
/// This is called by the Tauri event listener set up in create_router.
pub async fn handle_ui_bridge_response(
    pending: Arc<
        tokio::sync::Mutex<
            std::collections::HashMap<String, tokio::sync::oneshot::Sender<serde_json::Value>>,
        >,
    >,
    pending_count: Arc<std::sync::atomic::AtomicUsize>,
    response: serde_json::Value,
) {
    let request_id = response
        .get("requestId")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    if let Some(request_id) = request_id {
        let mut pending_map = pending.lock().await;
        if let Some(sender) = pending_map.remove(&request_id) {
            pending_count.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
            // Extract the data portion of the response
            let data = response.get("data").cloned().unwrap_or(response.clone());
            if sender.send(data).is_err() {
                warn!(
                    "UI Bridge: Failed to send response, receiver dropped for request {}",
                    request_id
                );
            } else {
                debug!("UI Bridge: Delivered response for request {}", request_id);
            }
        } else {
            warn!(
                "UI Bridge: No pending request found for response {}",
                request_id
            );
        }
    } else {
        warn!("UI Bridge: Response missing requestId: {:?}", response);
    }
}

// ============================================================================
// Control Handlers
// ============================================================================

/// Wrap a UI Bridge IPC result into an API response, propagating inner success/error status.
///
/// When the frontend returns `{success: false, error: "..."}` in the IPC data,
/// this propagates the failure to the outer API envelope instead of wrapping
/// it in `ApiResponse::success()` (which would create a misleading double-envelope:
/// `{success: true, data: {success: false, error: "..."}}`).
///
/// Also populates `error_detail` with a structured `UiBridgeError` for machine-readable
/// error handling by AI agents.
fn wrap_ipc_result(
    result: Result<serde_json::Value, String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    match result {
        Ok(data) => {
            // Check if the IPC response indicates failure
            if data.get("success").and_then(|v| v.as_bool()) == Some(false) {
                let error_msg = data
                    .get("error")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Operation failed")
                    .to_string();
                let error_detail = classify_transport_error(&error_msg);
                Ok(Json(ApiResponse {
                    success: false,
                    data: Some(data),
                    error: Some(error_msg),
                    error_detail: Some(error_detail),
                }))
            } else {
                Ok(Json(ApiResponse::success(data)))
            }
        }
        Err(e) => {
            error!("UI Bridge API: {}", e);
            let detail = classify_transport_error(&e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error_detailed(e, detail)),
            ))
        }
    }
}

/// Get all registered UI elements from the React UI Bridge.
pub async fn ui_bridge_get_elements_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Getting all elements");

    match ui_bridge_request_sync(&state, "get_elements", serde_json::json!({})).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Get a specific element by ID.
pub async fn ui_bridge_get_element_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Getting element {}", id);

    wrap_ipc_result(
        ui_bridge_request_sync(
            &state,
            "get_element",
            serde_json::json!({ "elementId": id }),
        )
        .await,
    )
}

/// Execute an action on an element.
/// Optional query parameters for action execution (e.g., task_run_id for persistence).
#[derive(Debug, Deserialize, Default)]
pub struct ActionQueryParams {
    /// When provided, the action event is persisted to ui_bridge_events for cross-run analysis.
    #[serde(default)]
    pub task_run_id: Option<i64>,
}

pub async fn ui_bridge_execute_action_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Query(query): Query<ActionQueryParams>,
    Json(request): Json<UIBridgeActionRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!(
        "UI Bridge API: Executing action {} on element {}",
        request.action, id
    );

    let action_name = request.action.clone();
    let task_run_id = query.task_run_id;
    let start = Instant::now();

    // Merge flat top-level fields into params so actions like drag work with
    // both {"action":"drag","params":{"targetPosition":{...}}} and
    // {"action":"drag","targetPosition":{...}} formats.
    let merged_params = if request.extra.is_empty() {
        request.params
    } else {
        let mut base = match request.params {
            Some(serde_json::Value::Object(m)) => m,
            _ => serde_json::Map::new(),
        };
        for (k, v) in request.extra {
            base.entry(k).or_insert(v);
        }
        Some(serde_json::Value::Object(base))
    };

    let payload = serde_json::json!({
        "elementId": id,
        "action": {
            "action": action_name,
            "params": merged_params,
            "waitOptions": request.wait_options
        }
    });

    let result = wrap_ipc_result(ui_bridge_request_sync(&state, "execute_action", payload).await);

    // Persist the action event when task_run_id is provided (non-blocking)
    if let Some(tr_id) = task_run_id {
        let duration_ms = start.elapsed().as_secs_f64() * 1000.0;
        let seq = state
            .ui_bridge_event_sequence
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        let (success, error_msg, result_json) = match &result {
            Ok(json_resp) => {
                let s = json_resp.success;
                let err = json_resp.error.clone();
                // Truncate result to 1KB to respect memory budgets
                let res = json_resp.data.as_ref().map(|d| {
                    let s = d.to_string();
                    if s.len() > 1024 {
                        format!("{}...", &s[..1024])
                    } else {
                        s
                    }
                });
                (s, err, res)
            }
            Err(_) => (false, Some("transport error".to_string()), None),
        };

        let pg_db = state.app_state.pg_db.clone();
        let element_id = id.clone();
        let action_for_db = action_name.clone();

        // Async PG write — fire-and-forget, never blocks the response
        tokio::spawn(async move {
            match pg_db
                .insert_ui_bridge_event(
                    Some(tr_id),
                    seq,
                    "action_executed",
                    Some(&element_id),
                    None,
                    None,
                    Some(&action_for_db),
                    None,
                    result_json.as_deref(),
                    Some(duration_ms),
                    success,
                    error_msg.as_deref(),
                    None,
                )
                .await
            {
                Ok(row_id) => info!(
                    "UI Bridge event persisted: element={}, row_id={}",
                    element_id, row_id
                ),
                Err(e) => warn!("UI Bridge event persist failed: {}", e),
            }
        });
    }

    result
}

/// Get all registered components.
pub async fn ui_bridge_get_components_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Getting all components");

    match ui_bridge_request_sync(&state, "get_components", serde_json::json!({})).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Get a specific component by ID.
pub async fn ui_bridge_get_component_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Getting component {}", id);

    match ui_bridge_request_sync(
        &state,
        "get_component",
        serde_json::json!({ "componentId": id }),
    )
    .await
    {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Execute an action on a component.
pub async fn ui_bridge_execute_component_action_handler(
    State(state): State<Arc<ApiState>>,
    Path((id, action_id)): Path<(String, String)>,
    Json(request): Json<UIBridgeComponentActionRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!(
        "UI Bridge API: Executing action {} on component {}",
        action_id, id
    );

    let payload = serde_json::json!({
        "componentId": id,
        "actionId": action_id,
        "params": request.params
    });

    wrap_ipc_result(ui_bridge_request_sync(&state, "execute_component_action", payload).await)
}

/// Discover controllable elements in the UI.
pub async fn ui_bridge_discover_handler(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<UIBridgeDiscoveryRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Discovering elements");

    let payload = serde_json::json!({
        "options": {
            "root": request.root,
            "interactiveOnly": request.interactive_only,
            "includeHidden": request.include_hidden,
            "limit": request.limit,
            "types": request.types,
            "selector": request.selector
        }
    });

    match ui_bridge_request_sync(&state, "discover", payload).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Get a full snapshot of the UI Bridge state.
pub async fn ui_bridge_get_snapshot_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Getting snapshot");

    match ui_bridge_request_sync(&state, "get_snapshot", serde_json::json!({})).await {
        Ok(mut data) => {
            // Enrich snapshot with architecture spec summaries from the database
            let arch_result = state.app_state.pg_db.get_all_cached_specs().await;

            match arch_result {
                Ok(specs) => {
                    let summaries: Vec<serde_json::Value> = specs
                        .iter()
                        .filter(|s| crate::spec_utils::is_architecture_spec_str(&s.spec_json))
                        .filter_map(|s| {
                            let parsed: serde_json::Value =
                                serde_json::from_str(&s.spec_json).ok()?;
                            Some(crate::spec_utils::format_architecture_summary(
                                &parsed,
                                &s.app_name,
                            ))
                        })
                        .collect();

                    if !summaries.is_empty() {
                        if let Some(obj) = data.as_object_mut() {
                            obj.insert(
                                "architecture".to_string(),
                                serde_json::json!({ "projects": summaries }),
                            );
                        }
                    }
                }
                Err(e) => {
                    warn!(
                        "Snapshot enrichment: failed to fetch cached specs from database: {}",
                        e
                    );
                }
            }

            Ok(Json(ApiResponse::success(data)))
        }
        Err(e) => {
            error!("UI Bridge API: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Read the current system clipboard content.
pub async fn ui_bridge_clipboard_read_handler(
    State(_state): State<Arc<ApiState>>,
) -> Json<ApiResponse<serde_json::Value>> {
    info!("UI Bridge API: Reading clipboard");

    match arboard::Clipboard::new() {
        Ok(mut clipboard) => {
            let text = clipboard.get_text().ok();
            let has_text = text.is_some();
            Json(ApiResponse::success(serde_json::json!({
                "text": text,
                "formats": if has_text { vec!["text/plain"] } else { vec![] as Vec<&str> },
            })))
        }
        Err(e) => {
            error!("UI Bridge API: Clipboard read failed: {}", e);
            Json(ApiResponse::error(format!("Clipboard read failed: {}", e)))
        }
    }
}

/// Write text to the system clipboard.
pub async fn ui_bridge_clipboard_write_handler(
    State(_state): State<Arc<ApiState>>,
    Json(body): Json<ClipboardWriteRequest>,
) -> Json<ApiResponse<serde_json::Value>> {
    info!("UI Bridge API: Writing to clipboard");

    match arboard::Clipboard::new() {
        Ok(mut clipboard) => {
            if let Some(html) = &body.html {
                // Write both HTML and plain text alternatives
                let alt_text = body.text.clone();
                match clipboard.set_html(html.as_str(), Some(&alt_text)) {
                    Ok(()) => Json(ApiResponse::success(serde_json::json!({
                        "written": true,
                        "formats": ["text/html", "text/plain"],
                    }))),
                    Err(e) => {
                        error!("UI Bridge API: Clipboard HTML write failed: {}", e);
                        Json(ApiResponse::error(format!("Clipboard write failed: {}", e)))
                    }
                }
            } else {
                match clipboard.set_text(&body.text) {
                    Ok(()) => Json(ApiResponse::success(serde_json::json!({
                        "written": true,
                        "formats": ["text/plain"],
                    }))),
                    Err(e) => {
                        error!("UI Bridge API: Clipboard write failed: {}", e);
                        Json(ApiResponse::error(format!("Clipboard write failed: {}", e)))
                    }
                }
            }
        }
        Err(e) => {
            error!("UI Bridge API: Clipboard init failed: {}", e);
            Json(ApiResponse::error(format!(
                "Clipboard initialization failed: {}",
                e
            )))
        }
    }
}

/// Get undo/redo state from the UI Bridge.
pub async fn ui_bridge_get_undo_state_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Getting undo state");

    match ui_bridge_request_sync(&state, "get_undo_state", serde_json::json!({})).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Execute undo via the UI Bridge.
pub async fn ui_bridge_undo_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Undo");

    match ui_bridge_request_sync(&state, "undo", serde_json::json!({})).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Undo failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Execute redo via the UI Bridge.
pub async fn ui_bridge_redo_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Redo");

    match ui_bridge_request_sync(&state, "redo", serde_json::json!({})).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Redo failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Get form state awareness data from the UI Bridge.
pub async fn ui_bridge_get_forms_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Getting form state");

    match ui_bridge_request_sync(&state, "get_forms", serde_json::json!({})).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Smart form fill action via the UI Bridge.
pub async fn ui_bridge_fill_form_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Fill form");

    match ui_bridge_request_sync(&state, "fill_form", body).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Fill form failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Capture a form state snapshot via the UI Bridge.
pub async fn ui_bridge_snapshot_forms_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Snapshot forms");

    match ui_bridge_request_sync(&state, "snapshot_forms", serde_json::json!({})).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Snapshot forms failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Diff two form snapshots via the UI Bridge.
pub async fn ui_bridge_diff_forms_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Diff forms");

    match ui_bridge_request_sync(&state, "diff_forms", body).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Diff forms failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Get console errors captured by the UI Bridge ConsoleCapture.
pub async fn ui_bridge_get_console_errors_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<ConsoleErrorsQuery>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Getting console errors");

    let payload = serde_json::json!({
        "params": {
            "since": query.since,
            "limit": query.limit
        }
    });

    match ui_bridge_request_sync(&state, "get_console_errors", payload).await {
        Ok(data) => {
            // Update the console error count for the health endpoint
            if let Some(errors) = data.get("errors").and_then(|e| e.as_array()) {
                state
                    .ui_bridge_console_error_count
                    .store(errors.len() as u64, std::sync::atomic::Ordering::Relaxed);
            }
            Ok(Json(ApiResponse::success(data)))
        }
        Err(e) => {
            error!("UI Bridge API: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Clear console errors captured by the UI Bridge ConsoleCapture.
pub async fn ui_bridge_clear_console_errors_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Clearing console errors");

    match ui_bridge_request_sync(&state, "clear_console_errors", serde_json::json!({})).await {
        Ok(data) => {
            state
                .ui_bridge_console_error_count
                .store(0, std::sync::atomic::Ordering::Relaxed);
            Ok(Json(ApiResponse::success(data)))
        }
        Err(e) => {
            error!("UI Bridge API: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

// ============================================================================
// Browser Events & Timeline Handlers
// ============================================================================

/// Query parameters for browser events endpoint
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserEventsQuery {
    #[serde(default, rename = "type")]
    event_type: Option<String>,
    #[serde(default, deserialize_with = "deserialize_timestamp")]
    since: Option<f64>,
    #[serde(default)]
    limit: Option<u32>,
}

/// Query parameters for timeline endpoint
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelineQuery {
    #[serde(default, deserialize_with = "deserialize_timestamp")]
    since: Option<f64>,
    #[serde(default)]
    limit: Option<u32>,
}

/// Query parameters for error snapshots endpoint
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorSnapshotsQuery {
    #[serde(default)]
    limit: Option<u32>,
}

/// Query parameters for network chains endpoint
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkChainsQuery {
    #[serde(default)]
    limit: Option<u32>,
}

/// Request body for starting an error session
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorSessionStartRequest {
    #[serde(default)]
    label: Option<String>,
}

/// Request body for capturing/comparing an error baseline
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorBaselineRequest {
    label: String,
}

/// Get browser events captured by the UI Bridge BrowserCapture.
pub async fn ui_bridge_get_browser_events_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<BrowserEventsQuery>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Getting browser events");

    let payload = serde_json::json!({
        "params": {
            "type": query.event_type,
            "since": query.since,
            "limit": query.limit
        }
    });

    wrap_ipc_result(ui_bridge_request_sync(&state, "get_browser_events", payload).await)
}

/// Get timeline events captured by the UI Bridge BrowserCapture.
pub async fn ui_bridge_get_timeline_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<TimelineQuery>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Getting timeline");

    let payload = serde_json::json!({
        "params": {
            "since": query.since,
            "limit": query.limit
        }
    });

    wrap_ipc_result(ui_bridge_request_sync(&state, "get_timeline", payload).await)
}

/// Get a health report from the UI Bridge BrowserCapture.
pub async fn ui_bridge_get_health_report_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Getting health report");

    wrap_ipc_result(
        ui_bridge_request_sync(&state, "get_health_report", serde_json::json!({})).await,
    )
}

/// Get network request chains from the UI Bridge BrowserCapture.
pub async fn ui_bridge_get_network_chains_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<NetworkChainsQuery>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Getting network chains");

    let payload = serde_json::json!({
        "params": {
            "limit": query.limit
        }
    });

    wrap_ipc_result(ui_bridge_request_sync(&state, "get_network_chains", payload).await)
}

/// Get recent error snapshots from the UI Bridge BrowserCapture.
pub async fn ui_bridge_get_error_snapshots_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<ErrorSnapshotsQuery>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Getting error snapshots");

    let payload = serde_json::json!({
        "params": {
            "limit": query.limit
        }
    });

    wrap_ipc_result(ui_bridge_request_sync(&state, "get_error_snapshots", payload).await)
}

/// Get a comprehensive error report from the UI Bridge.
pub async fn ui_bridge_get_error_report_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Getting error report");

    wrap_ipc_result(ui_bridge_request_sync(&state, "get_error_report", serde_json::json!({})).await)
}

// ============================================================================
// Error Session Handlers
// ============================================================================

/// Start an error monitoring session.
pub async fn ui_bridge_start_error_session_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<ErrorSessionStartRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Starting error session");

    let payload = serde_json::json!({
        "params": {
            "label": body.label
        }
    });

    wrap_ipc_result(ui_bridge_request_sync(&state, "start_error_session", payload).await)
}

/// End the active error monitoring session.
pub async fn ui_bridge_end_error_session_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Ending error session");

    wrap_ipc_result(
        ui_bridge_request_sync(&state, "end_error_session", serde_json::json!({})).await,
    )
}

/// Get all error sessions (completed and active).
pub async fn ui_bridge_get_error_sessions_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Getting error sessions");

    wrap_ipc_result(
        ui_bridge_request_sync(&state, "get_error_sessions", serde_json::json!({})).await,
    )
}

// ============================================================================
// Error Baseline Handlers
// ============================================================================

/// Capture an error baseline with a given label.
pub async fn ui_bridge_capture_error_baseline_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<ErrorBaselineRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Capturing error baseline '{}'", body.label);

    let payload = serde_json::json!({
        "params": {
            "label": body.label
        }
    });

    wrap_ipc_result(ui_bridge_request_sync(&state, "capture_error_baseline", payload).await)
}

/// Compare current errors against a previously captured baseline.
pub async fn ui_bridge_compare_error_baseline_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<ErrorBaselineRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Comparing error baseline '{}'", body.label);

    let payload = serde_json::json!({
        "params": {
            "label": body.label
        }
    });

    wrap_ipc_result(ui_bridge_request_sync(&state, "compare_error_baseline", payload).await)
}

// ============================================================================
// Network Request Monitoring Handlers
// ============================================================================

/// List network requests with optional filters.
pub async fn ui_bridge_get_network_requests_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<NetworkRequestsQuery>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Getting network requests");

    let payload = serde_json::json!({
        "params": {
            "status": query.status,
            "method": query.method,
            "urlPattern": query.url_pattern,
            "failuresOnly": query.failures_only,
            "since": query.since,
            "limit": query.limit
        }
    });

    match ui_bridge_request_sync(&state, "get_network_requests", payload).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Get currently in-flight network requests.
pub async fn ui_bridge_get_network_requests_in_flight_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Getting in-flight network requests");

    match ui_bridge_request_sync(
        &state,
        "get_network_requests_in_flight",
        serde_json::json!({}),
    )
    .await
    {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Wait for a specific network request matching criteria.
pub async fn ui_bridge_wait_for_network_request_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Wait for network request");

    match ui_bridge_request_sync(&state, "wait_for_network_request", body).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Get a specific network request by ID.
pub async fn ui_bridge_get_network_request_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Getting network request {}", id);

    match ui_bridge_request_sync(
        &state,
        "get_network_request",
        serde_json::json!({ "id": id }),
    )
    .await
    {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Get all loaded specs from the SpecStore.
pub async fn ui_bridge_get_specs_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Getting all specs");

    match ui_bridge_request_sync(&state, "get_specs", serde_json::json!({})).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Get a specific spec by ID from the SpecStore.
pub async fn ui_bridge_get_spec_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Getting spec {}", id);

    match ui_bridge_request_sync(&state, "get_spec", serde_json::json!({ "specId": id })).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

// ============================================================================
// Page Navigation Handlers
// ============================================================================

/// Query parameters for console errors endpoint.
/// Accepts `since` as either numeric (epoch ms) or ISO 8601 string.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConsoleErrorsQuery {
    #[serde(default, deserialize_with = "deserialize_timestamp")]
    since: Option<f64>,
    #[serde(default)]
    limit: Option<u32>,
}

/// Deserialize a timestamp that can be either a number (epoch ms) or an ISO 8601 string.
fn deserialize_timestamp<'de, D>(deserializer: D) -> Result<Option<f64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de;

    struct TimestampVisitor;
    impl<'de> de::Visitor<'de> for TimestampVisitor {
        type Value = Option<f64>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("a number (epoch ms) or ISO 8601 string")
        }

        fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_some<D: serde::Deserializer<'de>>(
            self,
            deserializer: D,
        ) -> Result<Self::Value, D::Error> {
            deserializer.deserialize_any(TimestampInnerVisitor)
        }

        fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }
    }

    struct TimestampInnerVisitor;
    impl<'de> de::Visitor<'de> for TimestampInnerVisitor {
        type Value = Option<f64>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("a number or ISO 8601 string")
        }

        fn visit_f64<E: de::Error>(self, v: f64) -> Result<Self::Value, E> {
            Ok(Some(v))
        }

        fn visit_i64<E: de::Error>(self, v: i64) -> Result<Self::Value, E> {
            Ok(Some(v as f64))
        }

        fn visit_u64<E: de::Error>(self, v: u64) -> Result<Self::Value, E> {
            Ok(Some(v as f64))
        }

        fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
            // Try parsing as float first
            if let Ok(f) = v.parse::<f64>() {
                return Ok(Some(f));
            }
            // Try ISO 8601
            if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(v) {
                return Ok(Some(dt.timestamp_millis() as f64));
            }
            // Try common ISO variants
            if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(v, "%Y-%m-%dT%H:%M:%S") {
                return Ok(Some(dt.and_utc().timestamp_millis() as f64));
            }
            Err(de::Error::custom(format!(
                "invalid timestamp: expected number (epoch ms) or ISO 8601 string, got '{}'",
                v
            )))
        }
    }

    deserializer.deserialize_option(TimestampVisitor)
}

/// Query parameters for network requests endpoint
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkRequestsQuery {
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    method: Option<String>,
    #[serde(default)]
    url_pattern: Option<String>,
    #[serde(default)]
    failures_only: Option<bool>,
    #[serde(default, deserialize_with = "deserialize_timestamp")]
    since: Option<f64>,
    #[serde(default)]
    limit: Option<u32>,
}

/// Request for page navigation
#[derive(Debug, Deserialize)]
pub struct PageNavigateRequest {
    url: String,
}

/// Request for CSS selector query
#[derive(Debug, Deserialize)]
pub struct QuerySelectorRequest {
    pub selector: String,
    /// Optional action to perform on matched element(s): "click"
    pub action: Option<String>,
    /// Index of the matched element to perform the action on (default: 0)
    pub index: Option<u32>,
}

/// Request for page evaluation
#[derive(Debug, Deserialize)]
pub struct PageEvaluateRequest {
    pub expression: String,
}

/// Request to evaluate multiple JS expressions in one round-trip.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchEvaluateRequest {
    pub expressions: Vec<BatchExpression>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchExpression {
    pub id: String,
    pub expression: String,
}

/// Request for a structured UI assertion.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StructuredAssertRequest {
    /// CSS selector or text to search for
    pub query: String,
    /// How to search: "css" (CSS selector), "text" (text content match), "textContains" (substring)
    #[serde(default = "default_query_type")]
    pub query_type: String,
    /// What to assert: "exists", "notExists", "count", "hasText", "hasClass", "isVisible", "hasAttribute"
    pub assertion: String,
    /// Expected value (for count, hasText, hasClass, hasAttribute assertions)
    #[serde(default)]
    pub expected: Option<serde_json::Value>,
    /// Attribute name (for hasAttribute assertion)
    #[serde(default)]
    pub attribute: Option<String>,
}

fn default_query_type() -> String {
    "css".to_string()
}

/// Result of a structured assertion.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AssertResult {
    pub passed: bool,
    pub assertion: String,
    pub query: String,
    pub actual: serde_json::Value,
    pub expected: serde_json::Value,
    pub message: String,
}

/// Result of a single batch expression evaluation.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchExpressionResult {
    pub id: String,
    pub success: bool,
    pub value: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Refresh the page.
pub async fn ui_bridge_page_refresh_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Page refresh");

    match ui_bridge_request_sync(&state, "page_refresh", serde_json::json!({})).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Hard refresh the page, bypassing browser cache.
/// Uses Tauri's webview eval to clear caches and force reload.
pub async fn ui_bridge_page_hard_refresh_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    use tauri::Manager;
    info!("UI Bridge API: Hard refresh (cache bypass)");

    if let Some(window) = state.app_handle.get_webview_window("main") {
        // Use fetch cache-busting + location replacement to bypass browser cache.
        // This is safer than deleting the EBWebView Cache directory.
        let js = r#"
            (function() {
                // Add cache-buster to current URL and navigate
                var url = new URL(location.href);
                url.searchParams.set('_hrc', Date.now());
                location.replace(url.toString());
            })();
        "#;
        window.eval(js).map_err(|e| {
            let msg = format!("Failed to hard refresh: {}", e);
            error!("{}", msg);
            (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(msg)))
        })?;
        Ok(Json(ApiResponse::success(serde_json::json!({
            "success": true,
            "message": "Hard refresh triggered"
        }))))
    } else {
        Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error("Main webview window not found".to_string())),
        ))
    }
}

/// Navigate to a URL.
pub async fn ui_bridge_page_navigate_handler(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<PageNavigateRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Page navigate to {}", request.url);

    // Validate URL
    let url = request.url.trim();
    if url.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(api_error("URL cannot be empty".to_string())),
        ));
    }
    if url.starts_with("about:") || url.starts_with("javascript:") {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(api_error(format!("Unsafe URL scheme rejected: {}", url))),
        ));
    }
    if !url.starts_with('/')
        && !url.starts_with("http://localhost")
        && !url.starts_with("https://localhost")
    {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(api_error(format!(
                "Only relative URLs (starting with /) or localhost URLs are allowed, got: {}",
                url
            ))),
        ));
    }

    let payload = serde_json::json!({ "url": url });

    match ui_bridge_request_sync(&state, "page_navigate", payload).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Go back in browser history.
pub async fn ui_bridge_page_go_back_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Page go back");

    match ui_bridge_request_sync(&state, "page_go_back", serde_json::json!({})).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Go forward in browser history.
pub async fn ui_bridge_page_go_forward_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Page go forward");

    match ui_bridge_request_sync(&state, "page_go_forward", serde_json::json!({})).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Query elements by CSS selector, optionally performing an action.
pub async fn ui_bridge_query_selector_handler(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<QuerySelectorRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Query selector '{}'", request.selector);

    let payload = serde_json::json!({
        "selector": request.selector,
        "index": request.index,
        "params": {
            "action": request.action,
        },
    });

    match ui_bridge_request_sync(&state, "query_selector", payload).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Evaluate a JavaScript expression in the webview.
///
/// First attempts the IPC path (through the SDK event handlers).
/// If IPC fails (SDK not responding, timeout), falls back to direct
/// WebView evaluation via Tauri's window.eval() + a response callback.
pub async fn ui_bridge_page_evaluate_handler(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<PageEvaluateRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!(
        "UI Bridge API: Page evaluate ({}...)",
        &request.expression[..request.expression.len().min(80)]
    );

    let payload = serde_json::json!({ "expression": request.expression });

    // Try IPC path first (fastest, uses SDK event handlers)
    match ui_bridge_request_sync(&state, "page_evaluate", payload).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(ipc_err) => {
            // IPC failed — try direct WebView evaluation as fallback
            debug!(
                "UI Bridge: IPC evaluate failed ({}), trying direct WebView eval",
                ipc_err
            );

            match direct_webview_evaluate_with_result(&state, &request.expression).await {
                Ok(result) => Ok(Json(ApiResponse::success(serde_json::json!({
                    "result": { "value": result },
                    "source": "direct_eval"
                })))),
                Err(direct_err) => {
                    error!(
                        "UI Bridge API: Both IPC and direct eval failed. IPC: {}, Direct: {}",
                        ipc_err, direct_err
                    );
                    Err((
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(api_error(format!(
                            "IPC: {}. Direct eval: {}",
                            ipc_err, direct_err
                        ))),
                    ))
                }
            }
        }
    }
}

/// Evaluate JS in the WebView using the IPC response channel as a callback.
///
/// This wraps the expression in a function that sends the result back via
/// POST to the IPC response endpoint, bypassing the Tauri event system.
/// Used as a fallback when the SDK's event handlers aren't responding.
async fn direct_webview_evaluate_with_result(
    state: &Arc<ApiState>,
    expression: &str,
) -> Result<String, String> {
    use tauri::Manager;

    let window = state
        .app_handle
        .get_webview_window("main")
        .ok_or_else(|| "WebView window 'main' not found".to_string())?;

    let request_id = uuid::Uuid::new_v4().to_string();
    let (tx, rx) = tokio::sync::oneshot::channel::<serde_json::Value>();

    // Register the pending request
    {
        let mut pending = state.ui_bridge_pending.lock().await;
        pending.insert(request_id.clone(), tx);
        state
            .ui_bridge_pending_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    // Build JS that evaluates the expression and POSTs the result back
    // via the IPC response HTTP endpoint
    let callback_js = format!(
        r#"(async function() {{
            var reqId = "{}";
            try {{
                var result = (function() {{ return {}; }})();
                var value = (result === undefined) ? null : result;
                await fetch("http://127.0.0.1:" + location.port + "/ui-bridge/ipc-response", {{
                    method: "POST",
                    headers: {{ "Content-Type": "application/json" }},
                    body: JSON.stringify({{
                        requestId: reqId,
                        type: "page_evaluate",
                        success: true,
                        data: {{ result: {{ value: (typeof value === "string") ? value : JSON.stringify(value) }} }}
                    }})
                }});
            }} catch(e) {{
                await fetch("http://127.0.0.1:" + location.port + "/ui-bridge/ipc-response", {{
                    method: "POST",
                    headers: {{ "Content-Type": "application/json" }},
                    body: JSON.stringify({{
                        requestId: reqId,
                        type: "page_evaluate",
                        success: false,
                        error: e.message
                    }})
                }}).catch(function() {{}});
            }}
        }})()"#,
        request_id, expression
    );

    window
        .eval(&callback_js)
        .map_err(|e| format!("WebView eval dispatch failed: {}", e))?;

    // Wait for the response with a timeout
    match tokio::time::timeout(std::time::Duration::from_secs(10), rx).await {
        Ok(Ok(data)) => {
            if let Some(result) = data
                .get("result")
                .and_then(|r| r.get("value"))
                .and_then(|v| v.as_str())
            {
                Ok(result.to_string())
            } else if let Some(err) = data.get("error").and_then(|e| e.as_str()) {
                Err(format!("JS error: {}", err))
            } else {
                Ok(serde_json::to_string(&data).unwrap_or_default())
            }
        }
        Ok(Err(_)) => Err("Response channel dropped".to_string()),
        Err(_) => {
            // Clean up pending request
            let mut pending = state.ui_bridge_pending.lock().await;
            if pending.remove(&request_id).is_some() {
                state
                    .ui_bridge_pending_count
                    .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
            }
            Err("Direct eval timed out after 10s".to_string())
        }
    }
}

// ============================================================================
// Direct WebView Evaluate (bypasses IPC, works without SDK)
// ============================================================================

/// Evaluate a JS expression directly in the Tauri WebView using window.eval().
///
/// This bypasses the IPC event system entirely, so it works even when the
/// UI Bridge SDK hasn't initialized. The expression is wrapped in a try/catch
/// to prevent evaluation errors from crashing the WebView connection.
///
/// Returns the stringified result. For structured data, the expression should
/// return JSON.stringify(...).
async fn direct_webview_evaluate(
    app_handle: &tauri::AppHandle,
    expression: &str,
) -> Result<String, String> {
    use tauri::Manager;

    let window = app_handle
        .get_webview_window("main")
        .ok_or_else(|| "WebView window 'main' not found".to_string())?;

    // Wrap in try/catch with timeout guard to prevent crashes
    let safe_js = format!(
        r#"(function() {{
            try {{
                var __result = (function() {{ return {}; }})();
                if (__result === undefined) return "undefined";
                if (__result === null) return "null";
                return String(__result);
            }} catch(e) {{
                return "ERROR:" + e.message;
            }}
        }})()"#,
        expression
    );

    // Tauri's eval() is fire-and-forget (returns Ok(()) on success).
    // To get a return value, we use a callback pattern via a Tauri event.
    // However, for simplicity and reliability, we use the IPC response channel
    // if available, or fall back to a polling approach.
    //
    // The most robust approach: use the existing page_evaluate IPC path first,
    // and only fall back to direct eval for side-effect-only operations.
    //
    // For the new endpoints, we'll use a hybrid: construct the full JS inline
    // and use IPC to get the result back, but with error wrapping.

    // Use the existing IPC path but with our safe-wrapped expression
    window
        .eval(&safe_js)
        .map_err(|e| format!("WebView eval failed: {}", e))?;

    // Since eval() is fire-and-forget in Tauri v2, we can't get a return value
    // directly. Instead, we'll use the IPC request_sync path with our wrapped expression.
    Ok("eval_dispatched".to_string())
}

/// Evaluate a JS expression via IPC with automatic error wrapping.
/// This is the safe version of page_evaluate that wraps expressions in try/catch
/// so errors return as JSON instead of crashing the connection.
async fn safe_evaluate(
    state: &Arc<ApiState>,
    expression: &str,
) -> Result<serde_json::Value, String> {
    // Wrap the expression in try/catch for safety
    let safe_expr = format!(
        r#"(() => {{ try {{ return JSON.stringify({{ success: true, value: (function() {{ {} }})() }}); }} catch(e) {{ return JSON.stringify({{ success: false, error: e.message, stack: e.stack }}); }} }})()"#,
        expression
    );

    let payload = serde_json::json!({ "expression": safe_expr });

    match ui_bridge_request_sync(state, "page_evaluate", payload).await {
        Ok(data) => {
            // Try to parse the inner result
            if let Some(result) = data
                .get("result")
                .and_then(|r| r.get("value"))
                .and_then(|v| v.as_str())
            {
                if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(result) {
                    if parsed.get("success") == Some(&serde_json::Value::Bool(false)) {
                        let error_msg = parsed
                            .get("error")
                            .and_then(|e| e.as_str())
                            .unwrap_or("Unknown error");
                        return Err(format!("JavaScript evaluation error: {}", error_msg));
                    }
                    return Ok(parsed);
                }
            }
            Ok(data)
        }
        Err(e) => Err(e),
    }
}

// ============================================================================
// Safe Page Evaluate (Task 20: error capture)
// ============================================================================

/// POST /ui-bridge/control/page/evaluate-safe
///
/// Like page/evaluate but wraps the expression in try/catch so evaluation
/// errors return as JSON responses instead of crashing the HTTP connection.
pub async fn ui_bridge_page_evaluate_safe_handler(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<PageEvaluateRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!(
        "UI Bridge API: Safe evaluate ({}...)",
        &request.expression[..request.expression.len().min(80)]
    );

    match safe_evaluate(&state, &format!("return {}", request.expression)).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            // Return the error as a structured response, not an HTTP error
            Ok(Json(ApiResponse::success(serde_json::json!({
                "success": false,
                "error": e,
            }))))
        }
    }
}

// ============================================================================
// Batch Evaluate (Task 19: multiple expressions in one round-trip)
// ============================================================================

/// POST /ui-bridge/control/page/evaluate-batch
///
/// Evaluate multiple JS expressions in a single IPC round-trip.
/// Each expression runs sequentially in the same JS context, preventing
/// re-render races that occur with sequential HTTP evaluate calls.
pub async fn ui_bridge_page_evaluate_batch_handler(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<BatchEvaluateRequest>,
) -> Result<Json<ApiResponse<Vec<BatchExpressionResult>>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!(
        "UI Bridge API: Batch evaluate ({} expressions)",
        request.expressions.len()
    );

    if request.expressions.is_empty() {
        return Ok(Json(ApiResponse::success(vec![])));
    }

    if request.expressions.len() > 50 {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(api_error("Maximum 50 expressions per batch".to_string())),
        ));
    }

    // Build a single JS expression that evaluates all sub-expressions
    // and returns an array of results
    let mut js_parts = Vec::new();
    for (i, expr) in request.expressions.iter().enumerate() {
        js_parts.push(format!(
            r#"(() => {{ try {{ var v = (function() {{ return {}; }})(); return {{ id: "{}", success: true, value: v === undefined ? null : v }}; }} catch(e) {{ return {{ id: "{}", success: false, error: e.message }}; }} }})()"#,
            expr.expression,
            expr.id.replace('"', r#"\""#),
            expr.id.replace('"', r#"\""#),
        ));
    }

    let combined = format!("return JSON.stringify([{}])", js_parts.join(","));
    let payload = serde_json::json!({ "expression": format!("(() => {{ {} }})()", combined) });

    match ui_bridge_request_sync(&state, "page_evaluate", payload).await {
        Ok(data) => {
            // Parse the combined result array
            let result_str = data
                .get("result")
                .and_then(|r| r.get("value"))
                .and_then(|v| v.as_str())
                .unwrap_or("[]");

            let results: Vec<BatchExpressionResult> = serde_json::from_str(result_str)
                .unwrap_or_else(|_| {
                    request
                        .expressions
                        .iter()
                        .map(|e| BatchExpressionResult {
                            id: e.id.clone(),
                            success: false,
                            value: serde_json::Value::Null,
                            error: Some("Failed to parse batch result".to_string()),
                        })
                        .collect()
                });

            Ok(Json(ApiResponse::success(results)))
        }
        Err(e) => {
            error!("UI Bridge API: Batch evaluate failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

// ============================================================================
// Structured Assert (Task 18: declarative assertions)
// ============================================================================

/// POST /ui-bridge/control/assert
///
/// Evaluate a structured UI assertion without writing JavaScript.
/// Supports CSS selectors and text search with common assertion types.
/// Returns a structured pass/fail result.
pub async fn ui_bridge_structured_assert_handler(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<StructuredAssertRequest>,
) -> Result<Json<ApiResponse<AssertResult>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!(
        "UI Bridge API: Structured assert ({} {} on '{}')",
        request.assertion, request.query_type, request.query
    );

    // Build the JS expression for this assertion
    let query_js = match request.query_type.as_str() {
        "css" => format!(
            "document.querySelectorAll('{}')",
            request.query.replace('\'', "\\'")
        ),
        "text" => format!(
            r#"Array.from(document.querySelectorAll('*')).filter(el => el.childNodes.length <= 3 && el.textContent.trim() === '{}')"#,
            request.query.replace('\'', "\\'")
        ),
        "textContains" => format!(
            r#"Array.from(document.querySelectorAll('*')).filter(el => el.childNodes.length <= 3 && el.textContent.includes('{}'))"#,
            request.query.replace('\'', "\\'")
        ),
        _ => {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(api_error(format!(
                    "Invalid query_type '{}'. Use: css, text, textContains",
                    request.query_type
                ))),
            ));
        }
    };

    let assertion_js = match request.assertion.as_str() {
        "exists" => format!(
            r#"return JSON.stringify({{ found: ({}).length, passed: ({}).length > 0 }})"#,
            query_js, query_js
        ),
        "notExists" => format!(
            r#"return JSON.stringify({{ found: ({}).length, passed: ({}).length === 0 }})"#,
            query_js, query_js
        ),
        "count" => {
            let expected = request
                .expected
                .as_ref()
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            format!(
                r#"return JSON.stringify({{ found: ({}).length, passed: ({}).length === {}, expected: {} }})"#,
                query_js, query_js, expected, expected
            )
        }
        "isVisible" => {
            format!(
                r#"var els = {}; var visible = Array.from(els).filter(function(el) {{ var r = el.getBoundingClientRect(); return r.width > 0 && r.height > 0 && getComputedStyle(el).display !== 'none'; }}); return JSON.stringify({{ found: els.length, visible: visible.length, passed: visible.length > 0 }})"#,
                query_js
            )
        }
        "hasText" => {
            let expected_text = request
                .expected
                .as_ref()
                .and_then(|v| v.as_str())
                .unwrap_or("");
            format!(
                r#"(() => {{ var els = {}; var texts = Array.from(els).map(el => el.textContent.trim()); var found = texts.some(t => t.includes('{}')); return JSON.stringify({{ found: els.length, texts: texts.slice(0, 3), passed: found }}); }})()"#,
                query_js,
                expected_text.replace('\'', "\\'")
            )
        }
        "hasClass" => {
            let expected_class = request
                .expected
                .as_ref()
                .and_then(|v| v.as_str())
                .unwrap_or("");
            format!(
                r#"(() => {{ var els = {}; var found = Array.from(els).some(el => el.classList.contains('{}')); var classes = Array.from(els).slice(0, 3).map(el => Array.from(el.classList).join(' ')); return JSON.stringify({{ found: els.length, classes: classes, passed: found }}); }})()"#,
                query_js,
                expected_class.replace('\'', "\\'")
            )
        }
        "hasAttribute" => {
            let attr = request.attribute.as_deref().unwrap_or("title");
            let expected_val = request.expected.as_ref().and_then(|v| v.as_str());
            let check = if let Some(val) = expected_val {
                format!(
                    "el.getAttribute('{}') === '{}'",
                    attr.replace('\'', "\\'"),
                    val.replace('\'', "\\'")
                )
            } else {
                format!("el.hasAttribute('{}')", attr.replace('\'', "\\'"))
            };
            format!(
                r#"(() => {{ var els = {}; var found = Array.from(els).some(el => {}); return JSON.stringify({{ found: els.length, passed: found }}); }})()"#,
                query_js, check
            )
        }
        _ => {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(api_error(format!(
                    "Invalid assertion '{}'. Use: exists, notExists, count, isVisible, hasText, hasClass, hasAttribute",
                    request.assertion
                ))),
            ));
        }
    };

    // Use the safe evaluate path
    let full_expr = format!("(() => {{ {} }})()", assertion_js);
    let payload = serde_json::json!({ "expression": full_expr });

    match ui_bridge_request_sync(&state, "page_evaluate", payload).await {
        Ok(data) => {
            let result_str = data
                .get("result")
                .and_then(|r| r.get("value"))
                .and_then(|v| v.as_str())
                .unwrap_or("{}");

            let parsed: serde_json::Value =
                serde_json::from_str(result_str).unwrap_or(serde_json::json!({"passed": false}));

            let passed = parsed
                .get("passed")
                .and_then(|p| p.as_bool())
                .unwrap_or(false);

            let result = AssertResult {
                passed,
                assertion: request.assertion.clone(),
                query: request.query.clone(),
                actual: parsed.clone(),
                expected: request.expected.clone().unwrap_or(serde_json::Value::Null),
                message: if passed {
                    format!(
                        "Assertion '{}' passed for '{}'",
                        request.assertion, request.query
                    )
                } else {
                    format!(
                        "Assertion '{}' failed for '{}': {:?}",
                        request.assertion, request.query, parsed
                    )
                },
            };

            Ok(Json(ApiResponse::success(result)))
        }
        Err(e) => {
            let result = AssertResult {
                passed: false,
                assertion: request.assertion,
                query: request.query,
                actual: serde_json::Value::Null,
                expected: request.expected.unwrap_or(serde_json::Value::Null),
                message: format!("Evaluation failed: {}", e),
            };
            Ok(Json(ApiResponse::success(result)))
        }
    }
}

// ============================================================================
// Exploration Handlers
// ============================================================================

/// Start UI Bridge exploration using qontinui library
/// Returns a job_id that can be used to poll for status and results
pub async fn start_ui_bridge_exploration(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let request: StartUIBridgeExplorationRequest = match serde_json::from_value(body) {
        Ok(r) => r,
        Err(e) => {
            return Err((
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(ApiResponse::error(format!(
                    "Invalid request: {}. Required fields: connection_url (string). \
                     Optional: target_type (\"web\"|\"desktop\"|\"mobile\", default \"web\"), \
                     max_depth (int, default 2), max_elements_per_page (int, default 20), \
                     max_total_elements (int, default 100), action_delay_ms (int, default 500), \
                     blocked_keywords (string[]), safe_keywords (string[]), \
                     blocked_selectors (string[]), capture_screenshots (bool, default false), \
                     run_state_discovery (bool, default true). \
                     Example: {{\"connection_url\": \"http://localhost:3001\", \"target_type\": \"web\"}}",
                    e
                ))),
            ));
        }
    };
    info!(
        "MCP API: Starting UI Bridge exploration for URL: {} (type: {})",
        request.connection_url, request.target_type
    );

    let app_state = state.app_state.clone();

    // Build parameters for Python command
    let params = serde_json::json!({
        "target_type": request.target_type,
        "connection_url": request.connection_url,
        "max_depth": request.max_depth.unwrap_or(2),
        "max_elements_per_page": request.max_elements_per_page.unwrap_or(20),
        "max_total_elements": request.max_total_elements.unwrap_or(100),
        "action_delay_ms": request.action_delay_ms.unwrap_or(500),
        "blocked_keywords": request.blocked_keywords.clone().unwrap_or_default(),
        "safe_keywords": request.safe_keywords.clone().unwrap_or_default(),
        "blocked_selectors": request.blocked_selectors.clone().unwrap_or_default(),
        "capture_screenshots": request.capture_screenshots.unwrap_or(false),
        "run_state_discovery": request.run_state_discovery.unwrap_or(true),
    });

    // Short timeout since this just starts the background job
    let timeout = std::time::Duration::from_secs(30);

    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }
            bridge.send_command_and_wait("start_ui_bridge_exploration", Some(params), timeout)
        })?
    })
    .await
    .map_err(|e| {
        error!("MCP API: spawn_blocking error: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    match result {
        Ok(response) => {
            if response.success {
                info!("MCP API: UI Bridge exploration job started");
                if let Some(data) = response.data {
                    Ok(Json(ApiResponse::success(data)))
                } else {
                    Ok(Json(ApiResponse::success(serde_json::json!({
                        "success": true
                    }))))
                }
            } else {
                let error_msg = response
                    .error
                    .unwrap_or_else(|| "Failed to start UI Bridge exploration".to_string());
                error!(
                    "MCP API: Failed to start UI Bridge exploration: {}",
                    error_msg
                );
                Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(api_error(error_msg)),
                ))
            }
        }
        Err(e) => {
            error!("MCP API: Failed to start UI Bridge exploration: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Get UI Bridge exploration status
pub async fn get_ui_bridge_exploration_status(
    State(state): State<Arc<ApiState>>,
    Query(request): Query<UIBridgeExplorationStatusRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let app_state = state.app_state.clone();

    let params = serde_json::json!({
        "job_id": request.job_id,
    });

    let timeout = std::time::Duration::from_secs(10);

    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }
            bridge.send_command_and_wait("get_ui_bridge_exploration_status", Some(params), timeout)
        })?
    })
    .await
    .map_err(|e| {
        error!("MCP API: spawn_blocking error: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    match result {
        Ok(response) => {
            if response.success {
                if let Some(data) = response.data {
                    Ok(Json(ApiResponse::success(data)))
                } else {
                    Ok(Json(ApiResponse::success(serde_json::json!({
                        "status": "unknown"
                    }))))
                }
            } else {
                let error_msg = response
                    .error
                    .unwrap_or_else(|| "Failed to get exploration status".to_string());
                Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(api_error(error_msg)),
                ))
            }
        }
        Err(e) => {
            error!("MCP API: Failed to get UI Bridge exploration status: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Get UI Bridge exploration results
pub async fn get_ui_bridge_exploration_results(
    State(state): State<Arc<ApiState>>,
    Query(request): Query<UIBridgeExplorationStatusRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let app_state = state.app_state.clone();

    let params = serde_json::json!({
        "job_id": request.job_id,
    });

    let timeout = std::time::Duration::from_secs(30);

    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }
            bridge.send_command_and_wait("get_ui_bridge_exploration_results", Some(params), timeout)
        })?
    })
    .await
    .map_err(|e| {
        error!("MCP API: spawn_blocking error: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    match result {
        Ok(response) => {
            if response.success {
                if let Some(data) = response.data {
                    Ok(Json(ApiResponse::success(data)))
                } else {
                    Ok(Json(ApiResponse::success(serde_json::json!({
                        "data": null
                    }))))
                }
            } else {
                let error_msg = response
                    .error
                    .unwrap_or_else(|| "Failed to get exploration results".to_string());
                Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(api_error(error_msg)),
                ))
            }
        }
        Err(e) => {
            error!(
                "MCP API: Failed to get UI Bridge exploration results: {}",
                e
            );
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Stop UI Bridge exploration
pub async fn stop_ui_bridge_exploration(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("MCP API: Stopping UI Bridge exploration");

    let app_state = state.app_state.clone();

    let timeout = std::time::Duration::from_secs(10);

    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }
            bridge.send_command_and_wait("stop_ui_bridge_exploration", None, timeout)
        })?
    })
    .await
    .map_err(|e| {
        error!("MCP API: spawn_blocking error: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    match result {
        Ok(response) => {
            if response.success {
                info!("MCP API: UI Bridge exploration stop requested");
                Ok(Json(ApiResponse::success(serde_json::json!({
                    "message": "Stop requested"
                }))))
            } else {
                let error_msg = response
                    .error
                    .unwrap_or_else(|| "Failed to stop exploration".to_string());
                Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(api_error(error_msg)),
                ))
            }
        }
        Err(e) => {
            error!("MCP API: Failed to stop UI Bridge exploration: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Discover states from render logs using co-occurrence analysis
/// This endpoint runs state discovery on existing render logs without exploration
pub async fn discover_states_from_renders(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<DiscoverStatesRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!(
        "MCP API: Discovering states from {} render logs",
        request.render_logs.len()
    );

    let app_state = state.app_state.clone();

    // Build parameters for Python command
    let params = serde_json::json!({
        "render_logs": request.render_logs,
    });

    // Allow more time for analysis of large render logs
    let timeout = std::time::Duration::from_secs(60);

    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }
            bridge.send_command_and_wait("discover_states_from_renders", Some(params), timeout)
        })?
    })
    .await
    .map_err(|e| {
        error!("MCP API: spawn_blocking error: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    match result {
        Ok(response) => {
            if response.success {
                info!("MCP API: State discovery completed successfully");
                if let Some(data) = response.data {
                    Ok(Json(ApiResponse::success(data)))
                } else {
                    Ok(Json(ApiResponse::success(serde_json::json!({
                        "states": [],
                        "elements": [],
                        "elementToRenders": {},
                        "renderCount": 0,
                        "uniqueElementCount": 0
                    }))))
                }
            } else {
                let error_msg = response
                    .error
                    .unwrap_or_else(|| "Failed to discover states from renders".to_string());
                error!("MCP API: Failed to discover states: {}", error_msg);
                Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(api_error(error_msg)),
                ))
            }
        }
        Err(e) => {
            error!("MCP API: Failed to discover states from renders: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

// =============================================================================
// Window Listing & App-Specific Screenshots (xcap)
// =============================================================================

/// Info about a capturable window
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowInfo {
    id: u32,
    title: String,
    app_name: String,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    is_minimized: bool,
    is_maximized: bool,
    is_focused: bool,
}

/// List all capturable windows using xcap.
fn list_windows_native() -> Result<Vec<WindowInfo>, String> {
    use xcap::Window;

    let windows = Window::all().map_err(|e| format!("Failed to enumerate windows: {}", e))?;
    let mut result = Vec::new();

    for w in &windows {
        let id = w.id().unwrap_or(0);
        let title = w.title().unwrap_or_default();
        let app_name = w.app_name().unwrap_or_default();

        // Skip windows with no title (background/system windows)
        if title.is_empty() {
            continue;
        }

        result.push(WindowInfo {
            id,
            title,
            app_name,
            x: w.x().unwrap_or(0),
            y: w.y().unwrap_or(0),
            width: w.width().unwrap_or(0),
            height: w.height().unwrap_or(0),
            is_minimized: w.is_minimized().unwrap_or(false),
            is_maximized: w.is_maximized().unwrap_or(false),
            is_focused: w.is_focused().unwrap_or(false),
        });
    }

    Ok(result)
}

/// GET /ui-bridge/control/windows — List all capturable windows
pub async fn ui_bridge_list_windows_handler(
    State(_state): State<Arc<ApiState>>,
) -> Json<ApiResponse<Vec<WindowInfo>>> {
    match tokio::task::spawn_blocking(list_windows_native).await {
        Ok(Ok(windows)) => {
            info!("UI Bridge: Listed {} capturable windows", windows.len());
            Json(ApiResponse::success(windows))
        }
        Ok(Err(e)) => {
            error!("UI Bridge: Failed to list windows: {}", e);
            Json(ApiResponse::error(format!("Failed to list windows: {}", e)))
        }
        Err(e) => {
            error!("UI Bridge: Window list task failed: {}", e);
            Json(ApiResponse::error(format!(
                "Window list task failed: {}",
                e
            )))
        }
    }
}

/// Query parameters for annotated screenshot
#[derive(Debug, Deserialize)]
pub struct AnnotatedScreenshotQuery {
    /// Monitor index (0-based), None for primary monitor. Used for full-screen capture.
    #[serde(default)]
    monitor: Option<i32>,
    /// Capture a specific window by title (case-insensitive substring match)
    #[serde(default)]
    window_title: Option<String>,
    /// Capture a specific window by app name (case-insensitive substring match)
    #[serde(default)]
    app_name: Option<String>,
    /// Capture a specific window by its ID (HWND as u32)
    #[serde(default)]
    window_id: Option<u32>,
    /// Capture the runner's own window
    #[serde(default)]
    runner: Option<bool>,
}

/// Annotated screenshot response
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AnnotatedScreenshotData {
    screenshot: String,
    width: i32,
    height: i32,
    /// Device pixel ratio (physical pixels / CSS pixels).
    /// Use this to scale CSS element bounds to screenshot pixel coordinates.
    #[serde(skip_serializing_if = "Option::is_none")]
    scale_factor: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    monitor: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    window_title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    window_app_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    window_id: Option<u32>,
}

/// Encode a DynamicImage as base64 PNG.
fn encode_image_to_base64(image: &image::DynamicImage) -> Result<String, String> {
    use base64::Engine;
    let mut png_bytes = std::io::Cursor::new(Vec::new());
    image
        .write_to(&mut png_bytes, image::ImageFormat::Png)
        .map_err(|e| format!("Failed to encode PNG: {}", e))?;
    Ok(base64::engine::general_purpose::STANDARD.encode(png_bytes.into_inner()))
}

/// Capture a specific window by matching criteria.
fn capture_window_screenshot(
    window_title: Option<String>,
    app_name: Option<String>,
    window_id: Option<u32>,
) -> Result<AnnotatedScreenshotData, String> {
    use xcap::Window;

    let windows = Window::all().map_err(|e| format!("Failed to enumerate windows: {}", e))?;

    let target = if let Some(id) = window_id {
        windows
            .iter()
            .find(|w| w.id().unwrap_or(0) == id)
            .ok_or_else(|| format!("No window found with id {}", id))?
    } else if let Some(ref title_query) = window_title {
        let query_lower = title_query.to_lowercase();
        windows
            .iter()
            .find(|w| {
                w.title()
                    .unwrap_or_default()
                    .to_lowercase()
                    .contains(&query_lower)
            })
            .ok_or_else(|| {
                let available: Vec<String> = windows
                    .iter()
                    .filter_map(|w| {
                        let t = w.title().unwrap_or_default();
                        if t.is_empty() {
                            None
                        } else {
                            Some(t)
                        }
                    })
                    .take(10)
                    .collect();
                format!(
                    "No window found matching title '{}'. Available: {:?}",
                    title_query, available
                )
            })?
    } else if let Some(ref app_query) = app_name {
        let query_lower = app_query.to_lowercase();
        windows
            .iter()
            .find(|w| {
                w.app_name()
                    .unwrap_or_default()
                    .to_lowercase()
                    .contains(&query_lower)
            })
            .ok_or_else(|| {
                let available: Vec<String> = windows
                    .iter()
                    .filter_map(|w| {
                        let a = w.app_name().unwrap_or_default();
                        if a.is_empty() {
                            None
                        } else {
                            Some(a)
                        }
                    })
                    .collect::<std::collections::HashSet<_>>()
                    .into_iter()
                    .take(10)
                    .collect();
                format!(
                    "No window found matching app_name '{}'. Available: {:?}",
                    app_query, available
                )
            })?
    } else {
        return Err("No window selection criteria provided".to_string());
    };

    let title = target.title().unwrap_or_default();
    let app = target.app_name().unwrap_or_default();
    let id = target.id().unwrap_or(0);

    if target.is_minimized().unwrap_or(false) {
        return Err(format!(
            "Window '{}' ({}) is minimized — cannot capture",
            title, app
        ));
    }

    let image = target
        .capture_image()
        .map_err(|e| format!("Failed to capture window '{}': {}", title, e))?;

    // Determine the scale factor from the monitor the window is on
    let scale = {
        use xcap::Monitor;
        let win_x = target.x().unwrap_or(0);
        let win_y = target.y().unwrap_or(0);
        Monitor::all()
            .ok()
            .and_then(|monitors| {
                monitors.iter().find_map(|m| {
                    let mx = m.x().unwrap_or(0);
                    let my = m.y().unwrap_or(0);
                    let ms = m.scale_factor().unwrap_or(1.0) as f64;
                    let mw = (m.width().unwrap_or(0) as f64 / ms) as i32;
                    let mh = (m.height().unwrap_or(0) as f64 / ms) as i32;
                    if win_x >= mx && win_x < mx + mw && win_y >= my && win_y < my + mh {
                        Some(ms)
                    } else {
                        None
                    }
                })
            })
            .unwrap_or(1.0)
    };

    let width = image.width() as i32;
    let height = image.height() as i32;
    let dynamic = image::DynamicImage::ImageRgba8(image);
    let b64 = encode_image_to_base64(&dynamic)?;

    Ok(AnnotatedScreenshotData {
        screenshot: b64,
        width,
        height,
        scale_factor: Some(scale),
        monitor: None,
        window_title: Some(title),
        window_app_name: Some(app),
        window_id: Some(id),
    })
}

/// Capture the runner's own window by cropping from a monitor screenshot.
/// xcap skips same-process windows, so we capture the monitor and crop.
///
/// DPI handling:
/// - Tauri `outer_position()` / `outer_size()` return physical pixels.
/// - xcap `Monitor::x()` / `y()` return logical coordinates (dmPosition).
/// - xcap `Monitor::width()` / `height()` return physical pixels (dmPelsWidth/Height).
/// - The captured image is at physical resolution.
///
/// To match monitors: convert Tauri physical position to logical using scale_factor.
/// To crop the image: work in physical pixels (image coords = physical).
fn capture_runner_window(
    phys_x: i32,
    phys_y: i32,
    phys_w: u32,
    phys_h: u32,
    scale: f64,
    title: &str,
) -> Result<AnnotatedScreenshotData, String> {
    use xcap::Monitor;

    let monitors = Monitor::all().map_err(|e| format!("Failed to enumerate monitors: {}", e))?;
    if monitors.is_empty() {
        return Err("No monitors found".to_string());
    }

    // Convert Tauri physical position to logical for monitor matching.
    // xcap monitor x/y are logical (dmPosition), width/height are physical (dmPelsWidth).
    let logical_x = (phys_x as f64 / scale) as i32;
    let logical_y = (phys_y as f64 / scale) as i32;
    let logical_center_x = logical_x + (phys_w as f64 / scale / 2.0) as i32;
    let logical_center_y = logical_y + (phys_h as f64 / scale / 2.0) as i32;

    let (monitor, mon_logical_x, mon_logical_y) = monitors
        .iter()
        .find_map(|m| {
            let mx = m.x().unwrap_or(0);
            let my = m.y().unwrap_or(0);
            let mon_scale = m.scale_factor().unwrap_or(1.0) as f64;
            // Monitor logical dimensions = physical / scale
            let mw_logical = (m.width().unwrap_or(0) as f64 / mon_scale) as i32;
            let mh_logical = (m.height().unwrap_or(0) as f64 / mon_scale) as i32;
            if logical_center_x >= mx
                && logical_center_x < mx + mw_logical
                && logical_center_y >= my
                && logical_center_y < my + mh_logical
            {
                Some((m, mx, my))
            } else {
                None
            }
        })
        .ok_or_else(|| "Runner window not on any monitor".to_string())?;

    let mon_scale = monitor.scale_factor().unwrap_or(1.0) as f64;
    let full_image = monitor
        .capture_image()
        .map_err(|e| format!("Failed to capture monitor: {}", e))?;

    // Convert logical window position to physical pixel offset in the captured image.
    // Offset in logical coords relative to monitor origin, then scale to physical.
    let rel_logical_x = logical_x - mon_logical_x;
    let rel_logical_y = logical_y - mon_logical_y;
    let rel_phys_x = (rel_logical_x as f64 * mon_scale) as i32;
    let rel_phys_y = (rel_logical_y as f64 * mon_scale) as i32;

    // Handle negative offsets (window partially off-screen)
    let crop_x = rel_phys_x.max(0) as u32;
    let crop_y = rel_phys_y.max(0) as u32;
    let crop_w = if rel_phys_x < 0 {
        phys_w.saturating_sub((-rel_phys_x) as u32)
    } else {
        phys_w
    }
    .min(full_image.width().saturating_sub(crop_x));
    let crop_h = if rel_phys_y < 0 {
        phys_h.saturating_sub((-rel_phys_y) as u32)
    } else {
        phys_h
    }
    .min(full_image.height().saturating_sub(crop_y));

    if crop_w == 0 || crop_h == 0 {
        return Err(format!(
            "Runner window has zero visible area (crop: {}x{} at ({}, {}), image: {}x{}, scale: {})",
            crop_w, crop_h, crop_x, crop_y, full_image.width(), full_image.height(), mon_scale
        ));
    }

    let full_dynamic = image::DynamicImage::ImageRgba8(full_image);
    let cropped = full_dynamic.crop_imm(crop_x, crop_y, crop_w, crop_h);
    let b64 = encode_image_to_base64(&cropped)?;

    Ok(AnnotatedScreenshotData {
        screenshot: b64,
        width: crop_w as i32,
        height: crop_h as i32,
        scale_factor: Some(scale),
        monitor: None,
        window_title: Some(title.to_string()),
        window_app_name: Some("Qontinui Runner".to_string()),
        window_id: None,
    })
}

/// Capture a full monitor screenshot.
fn capture_monitor_screenshot(
    monitor_index: Option<i32>,
) -> Result<AnnotatedScreenshotData, String> {
    use xcap::Monitor;

    let monitors = Monitor::all().map_err(|e| format!("Failed to enumerate monitors: {}", e))?;
    if monitors.is_empty() {
        return Err("No monitors found".to_string());
    }

    let monitor = if let Some(idx) = monitor_index {
        if idx < 0 {
            return Err(format!("Monitor index must be non-negative, got {}", idx));
        }
        monitors
            .into_iter()
            .nth(idx as usize)
            .ok_or_else(|| format!("Monitor index {} out of range", idx))?
    } else {
        monitors.into_iter().next().unwrap()
    };

    let scale = monitor.scale_factor().unwrap_or(1.0) as f64;
    let image = monitor
        .capture_image()
        .map_err(|e| format!("Failed to capture monitor: {}", e))?;

    let width = image.width() as i32;
    let height = image.height() as i32;
    let dynamic = image::DynamicImage::ImageRgba8(image);
    let b64 = encode_image_to_base64(&dynamic)?;

    Ok(AnnotatedScreenshotData {
        screenshot: b64,
        width,
        height,
        scale_factor: Some(scale),
        monitor: monitor_index,
        window_title: None,
        window_app_name: None,
        window_id: None,
    })
}

/// GET /ui-bridge/control/annotated-screenshot — Screenshot with metadata
///
/// Captures natively via xcap (Rust). No Python executor dependency.
///
/// Query params (all optional, first match wins):
/// - `runner=true` — capture the runner's own Tauri window
/// - `window_title=...` — case-insensitive substring match on window title
/// - `app_name=...` — case-insensitive substring match on app name
/// - `window_id=N` — exact window ID (HWND)
/// - `monitor=N` — full monitor capture (0-based index, default: primary)
/// - (none) — captures primary monitor
pub async fn ui_bridge_annotated_screenshot_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<AnnotatedScreenshotQuery>,
) -> Json<ApiResponse<AnnotatedScreenshotData>> {
    let is_window_capture = query.runner.unwrap_or(false)
        || query.window_title.is_some()
        || query.app_name.is_some()
        || query.window_id.is_some();

    if is_window_capture {
        info!(
            runner = ?query.runner,
            window_title = ?query.window_title,
            app_name = ?query.app_name,
            window_id = ?query.window_id,
            "UI Bridge API: Capturing window screenshot (native)"
        );

        // For runner's own window, xcap skips same-process windows,
        // so we capture the monitor and crop to the window bounds.
        if query.runner.unwrap_or(false) {
            use tauri::Manager;
            let window = state.app_handle.get_webview_window("main");
            if let Some(win) = window {
                let scale = win.scale_factor().unwrap_or(1.0);
                // Use inner_position/inner_size for the content area (viewport).
                // Element bounds from the UI Bridge SDK are relative to the viewport,
                // not the outer window frame (which includes title bar).
                let pos = win.inner_position().unwrap_or_default();
                let size = win.inner_size().unwrap_or_default();
                let x = pos.x;
                let y = pos.y;
                let w = size.width;
                let h = size.height;
                let title = win
                    .title()
                    .unwrap_or_else(|_| "Qontinui Runner".to_string());

                return match tokio::task::spawn_blocking(move || {
                    capture_runner_window(x, y, w, h, scale, &title)
                })
                .await
                {
                    Ok(Ok(data)) => {
                        info!(
                            "UI Bridge screenshot: Captured runner window ({}x{})",
                            data.width, data.height
                        );
                        Json(ApiResponse::success(data))
                    }
                    Ok(Err(e)) => {
                        error!("UI Bridge screenshot: Runner capture failed: {}", e);
                        Json(ApiResponse::error(format!(
                            "Runner screenshot failed: {}",
                            e
                        )))
                    }
                    Err(e) => {
                        error!("UI Bridge screenshot: Task join error: {}", e);
                        Json(ApiResponse::error(format!(
                            "Screenshot capture task failed: {}",
                            e
                        )))
                    }
                };
            } else {
                return Json(ApiResponse::error("Runner window not found".to_string()));
            }
        }

        let window_title = query.window_title;
        let app_name = query.app_name;
        let window_id = query.window_id;

        match tokio::task::spawn_blocking(move || {
            capture_window_screenshot(window_title, app_name, window_id)
        })
        .await
        {
            Ok(Ok(data)) => {
                info!(
                    "UI Bridge screenshot: Captured window '{}' ({}x{}, id={})",
                    data.window_title.as_deref().unwrap_or("?"),
                    data.width,
                    data.height,
                    data.window_id.unwrap_or(0),
                );
                Json(ApiResponse::success(data))
            }
            Ok(Err(e)) => {
                error!("UI Bridge screenshot: Window capture failed: {}", e);
                Json(ApiResponse::error(format!(
                    "Window screenshot failed: {}",
                    e
                )))
            }
            Err(e) => {
                error!("UI Bridge screenshot: Task join error: {}", e);
                Json(ApiResponse::error(format!(
                    "Screenshot capture task failed: {}",
                    e
                )))
            }
        }
    } else {
        // Full monitor capture (existing behavior)
        info!(
            monitor = ?query.monitor,
            "UI Bridge API: Capturing monitor screenshot (native)"
        );

        let monitor = query.monitor;
        match tokio::task::spawn_blocking(move || capture_monitor_screenshot(monitor)).await {
            Ok(Ok(data)) => {
                info!(
                    "UI Bridge screenshot: Captured {}x{} from monitor {:?}",
                    data.width, data.height, data.monitor
                );
                Json(ApiResponse::success(data))
            }
            Ok(Err(e)) => {
                error!("UI Bridge screenshot: Monitor capture failed: {}", e);
                Json(ApiResponse::error(format!(
                    "Screenshot capture failed: {}",
                    e
                )))
            }
            Err(e) => {
                error!("UI Bridge screenshot: Task join error: {}", e);
                Json(ApiResponse::error(format!(
                    "Screenshot capture task failed: {}",
                    e
                )))
            }
        }
    }
}

// ============================================================================
// Design Review Handlers (Control Mode)
// ============================================================================

/// Get extended computed styles for a single element.
pub async fn ui_bridge_design_element_styles_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Design - get element styles for {}", id);

    let payload = serde_json::json!({
        "elementId": id
    });

    match ui_bridge_request_sync(&state, "design_get_element_styles", payload).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Design element styles failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Get styles across interaction states (hover, focus, active, disabled).
pub async fn ui_bridge_design_state_styles_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    body: Option<Json<serde_json::Value>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Design - get state styles for {}", id);

    let mut payload = serde_json::json!({
        "elementId": id
    });

    if let Some(Json(body)) = body {
        if let (Some(base), Some(extra)) = (payload.as_object_mut(), body.as_object()) {
            for (key, value) in extra {
                base.insert(key.clone(), value.clone());
            }
        }
    }

    match ui_bridge_request_sync(&state, "design_get_state_styles", payload).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Design state styles failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Get design snapshot for all or filtered elements.
pub async fn ui_bridge_design_snapshot_handler(
    State(state): State<Arc<ApiState>>,
    body: Option<Json<serde_json::Value>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Design - get design snapshot");

    let payload = body.map(|Json(b)| b).unwrap_or(serde_json::json!({}));

    match ui_bridge_request_sync(&state, "design_get_snapshot", payload).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Design snapshot failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Capture responsive snapshots at multiple viewport widths.
pub async fn ui_bridge_design_responsive_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Design - get responsive snapshots");

    match ui_bridge_request_sync(&state, "design_get_responsive", body).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Design responsive failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Run a style audit against a loaded or provided style guide.
pub async fn ui_bridge_design_audit_handler(
    State(state): State<Arc<ApiState>>,
    body: Option<Json<serde_json::Value>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Design - run style audit");

    let payload = body.map(|Json(b)| b).unwrap_or(serde_json::json!({}));

    match ui_bridge_request_sync(&state, "design_run_audit", payload).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Design audit failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Load a style guide for subsequent audits.
pub async fn ui_bridge_design_load_style_guide_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Design - load style guide");

    match ui_bridge_request_sync(&state, "design_load_style_guide", body).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Design load style guide failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Get the currently loaded style guide.
pub async fn ui_bridge_design_get_style_guide_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Design - get style guide");

    let payload = serde_json::json!({});

    match ui_bridge_request_sync(&state, "design_get_style_guide", payload).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Design get style guide failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Clear the currently loaded style guide.
pub async fn ui_bridge_design_clear_style_guide_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Design - clear style guide");

    let payload = serde_json::json!({});

    match ui_bridge_request_sync(&state, "design_clear_style_guide", payload).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Design clear style guide failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

// ── Change tracking handlers ─────────────────────────────────────────

/// Save a bookmark (snapshot) by name.
pub async fn ui_bridge_save_bookmark_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Save bookmark");
    match ui_bridge_request_sync(&state, "save_bookmark", body).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Save bookmark failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Get a bookmark by name.
pub async fn ui_bridge_get_bookmark_handler(
    State(state): State<Arc<ApiState>>,
    Path(name): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Get bookmark '{}'", name);
    match ui_bridge_request_sync(&state, "get_bookmark", serde_json::json!({"name": name})).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Get bookmark failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Delete a bookmark by name.
pub async fn ui_bridge_delete_bookmark_handler(
    State(state): State<Arc<ApiState>>,
    Path(name): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Delete bookmark '{}'", name);
    match ui_bridge_request_sync(&state, "delete_bookmark", serde_json::json!({"name": name})).await
    {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Delete bookmark failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// List all bookmarks.
pub async fn ui_bridge_list_bookmarks_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: List bookmarks");
    match ui_bridge_request_sync(&state, "list_bookmarks", serde_json::json!({})).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: List bookmarks failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Diff current state from a named bookmark.
pub async fn ui_bridge_diff_from_bookmark_handler(
    State(state): State<Arc<ApiState>>,
    Path(name): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Diff from bookmark '{}'", name);
    match ui_bridge_request_sync(
        &state,
        "diff_from_bookmark",
        serde_json::json!({"name": name}),
    )
    .await
    {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Diff from bookmark failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Execute an action and return the diff.
pub async fn ui_bridge_execute_with_diff_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Execute with diff");
    match ui_bridge_request_sync(&state, "execute_with_diff", body).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Execute with diff failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Wait for a change to occur.
pub async fn ui_bridge_wait_for_change_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Wait for change");
    match ui_bridge_request_sync(&state, "wait_for_change", body).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Wait for change failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Categorize the last diff.
pub async fn ui_bridge_categorize_last_diff_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Categorize last diff");
    match ui_bridge_request_sync(&state, "categorize_last_diff", serde_json::json!({})).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Categorize last diff failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Compute a scoped diff.
pub async fn ui_bridge_scoped_diff_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Scoped diff");
    match ui_bridge_request_sync(&state, "scoped_diff", body).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Scoped diff failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Summarize a diff.
pub async fn ui_bridge_summarize_diff_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Summarize diff");
    match ui_bridge_request_sync(&state, "summarize_diff", body).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Summarize diff failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Get structured changes.
pub async fn ui_bridge_structured_changes_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Structured changes");
    match ui_bridge_request_sync(&state, "structured_changes", body).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Structured changes failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Enable the change buffer.
pub async fn ui_bridge_enable_change_buffer_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Enable change buffer");
    match ui_bridge_request_sync(&state, "enable_change_buffer", serde_json::json!({})).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Enable change buffer failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Disable the change buffer.
pub async fn ui_bridge_disable_change_buffer_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Disable change buffer");
    match ui_bridge_request_sync(&state, "disable_change_buffer", serde_json::json!({})).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Disable change buffer failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Drain the change buffer.
pub async fn ui_bridge_drain_change_buffer_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Drain change buffer");
    match ui_bridge_request_sync(&state, "drain_change_buffer", serde_json::json!({})).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Drain change buffer failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Get the change buffer size.
pub async fn ui_bridge_get_change_buffer_size_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Get change buffer size");
    match ui_bridge_request_sync(&state, "get_change_buffer_size", serde_json::json!({})).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Get change buffer size failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

// ============================================================================
// Keyboard Shortcuts Handler
// ============================================================================

/// Get discovered keyboard shortcuts.
pub async fn ui_bridge_get_keyboard_shortcuts_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Getting keyboard shortcuts");

    match ui_bridge_request_sync(&state, "get_keyboard_shortcuts", serde_json::json!({})).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

// ============================================================================
// Idle Detection Handlers
// ============================================================================

/// Get composite idle status.
pub async fn ui_bridge_get_idle_status_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Getting idle status");

    match ui_bridge_request_sync(&state, "get_idle_status", serde_json::json!({})).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Wait for composite idle state.
pub async fn ui_bridge_wait_for_idle_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Wait for idle");

    let payload = serde_json::json!({
        "params": {
            "timeout": body.get("timeout").and_then(|v| v.as_i64()).unwrap_or(30000),
            "minStableMs": body.get("minStableMs").and_then(|v| v.as_i64()).unwrap_or(500),
            "exclude": body.get("exclude")
        }
    });

    match ui_bridge_request_sync(&state, "wait_for_idle", payload).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Wait for idle failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

// ============================================================================
// Stuck Screen Diagnosis Handler
// ============================================================================

/// Internal capture result for diagnosis.
struct DiagnosisCapture {
    image: image::DynamicImage,
    base64: String,
    width: i32,
    height: i32,
    source: String,
}

/// Capture the runner window for diagnosis, falling back to primary monitor.
fn capture_for_diagnosis(app_handle: &tauri::AppHandle) -> Result<DiagnosisCapture, String> {
    use base64::Engine;
    use tauri::Manager;

    // Try runner window first
    if let Some(win) = app_handle.get_webview_window("main") {
        let scale = win.scale_factor().unwrap_or(1.0);
        let pos = win.outer_position().unwrap_or_default();
        let size = win.outer_size().unwrap_or_default();

        if size.width > 0 && size.height > 0 {
            match capture_runner_window(
                pos.x,
                pos.y,
                size.width,
                size.height,
                scale,
                "Qontinui Runner",
            ) {
                Ok(data) => {
                    let bytes = base64::engine::general_purpose::STANDARD
                        .decode(&data.screenshot)
                        .map_err(|e| format!("Base64 decode failed: {}", e))?;
                    let img = image::load_from_memory(&bytes)
                        .map_err(|e| format!("Image decode failed: {}", e))?;
                    return Ok(DiagnosisCapture {
                        image: img,
                        base64: data.screenshot,
                        width: data.width,
                        height: data.height,
                        source: "runner_window".to_string(),
                    });
                }
                Err(e) => {
                    warn!(
                        "Runner window capture failed, falling back to monitor: {}",
                        e
                    );
                }
            }
        }
    }

    // Fallback: primary monitor
    let data = capture_monitor_screenshot(None)?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&data.screenshot)
        .map_err(|e| format!("Base64 decode failed: {}", e))?;
    let img = image::load_from_memory(&bytes).map_err(|e| format!("Image decode failed: {}", e))?;
    Ok(DiagnosisCapture {
        image: img,
        base64: data.screenshot,
        width: data.width,
        height: data.height,
        source: "primary_monitor".to_string(),
    })
}

/// Compare two screenshots by sampling pixels. Returns similarity 0.0-1.0.
fn compute_screenshot_similarity(img1: &image::DynamicImage, img2: &image::DynamicImage) -> f64 {
    let rgba1 = img1.to_rgba8();
    let rgba2 = img2.to_rgba8();

    if rgba1.dimensions() != rgba2.dimensions() {
        return 0.0;
    }

    let (w, h) = rgba1.dimensions();
    let total = w as u64 * h as u64;
    if total == 0 {
        return 1.0;
    }

    let pixels1 = rgba1.as_raw();
    let pixels2 = rgba2.as_raw();

    // Sample ~10,000 pixels evenly for speed
    let step = (total / 10_000u64).max(1) as usize;
    let mut matching = 0u64;
    let mut sampled = 0u64;

    for i in (0..total as usize).step_by(step) {
        let offset = i * 4;
        if offset + 3 >= pixels1.len() {
            break;
        }

        let diff: u32 = (0..4)
            .map(|c| (pixels1[offset + c] as i32 - pixels2[offset + c] as i32).unsigned_abs())
            .sum();

        // Tolerance for rendering/compression artifacts
        if diff <= 20 {
            matching += 1;
        }
        sampled += 1;
    }

    if sampled == 0 {
        1.0
    } else {
        matching as f64 / sampled as f64
    }
}

/// Try to get DOM-based idle signals from the React frontend.
/// Returns None if React hasn't mounted or doesn't respond within timeout.
async fn try_get_dom_signals(state: &Arc<ApiState>, timeout_ms: u64) -> Option<serde_json::Value> {
    match tokio::time::timeout(
        std::time::Duration::from_millis(timeout_ms),
        ui_bridge_request_sync(state, "get_idle_status", serde_json::json!({})),
    )
    .await
    {
        Ok(Ok(data)) => Some(data),
        _ => None,
    }
}

/// Diagnose whether the app is stuck on a loading screen.
///
/// Uses native screenshot capture (xcap) to compare visual state across an
/// observation window. Optionally enriches with DOM signals from the React
/// UI Bridge if it's responsive. Works even if React hasn't mounted.
pub async fn ui_bridge_diagnose_stuck_screen_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Json<ApiResponse<serde_json::Value>> {
    info!("UI Bridge API: Diagnose stuck screen (native)");

    let observation_ms = body
        .get("observationWindowMs")
        .and_then(|v| v.as_u64())
        .unwrap_or(3000);

    // Phase 1: Capture initial screenshot
    let app_handle1 = state.app_handle.clone();
    let cap1 = match tokio::task::spawn_blocking(move || capture_for_diagnosis(&app_handle1)).await
    {
        Ok(Ok(cap)) => cap,
        Ok(Err(e)) => {
            error!("Diagnosis: initial screenshot failed: {}", e);
            return Json(ApiResponse::error(format!(
                "Screenshot capture failed: {}",
                e
            )));
        }
        Err(e) => {
            return Json(ApiResponse::error(format!("Capture task failed: {}", e)));
        }
    };

    // Phase 2: Try to get DOM signals (short timeout — don't block if React hasn't mounted)
    let dom_status1 = try_get_dom_signals(&state, 2000).await;

    // Phase 3: Wait observation window
    tokio::time::sleep(std::time::Duration::from_millis(observation_ms)).await;

    // Phase 4: Capture second screenshot
    let app_handle2 = state.app_handle.clone();
    let cap2 = match tokio::task::spawn_blocking(move || capture_for_diagnosis(&app_handle2)).await
    {
        Ok(Ok(cap)) => cap,
        Ok(Err(e)) => {
            error!("Diagnosis: second screenshot failed: {}", e);
            return Json(ApiResponse::error(format!(
                "Second screenshot capture failed: {}",
                e
            )));
        }
        Err(e) => {
            return Json(ApiResponse::error(format!("Capture task failed: {}", e)));
        }
    };

    // Phase 5: Try DOM signals again
    let dom_status2 = try_get_dom_signals(&state, 2000).await;

    // Phase 6: Compare screenshots
    let img1 = cap1.image;
    let img2 = cap2.image;
    let similarity =
        tokio::task::spawn_blocking(move || compute_screenshot_similarity(&img1, &img2))
            .await
            .unwrap_or(0.5); // Couldn't compare — inconclusive
    let screenshot_changed = similarity < 0.95;

    // Phase 7: Extract DOM signal details
    let ui_bridge_responsive = dom_status1.is_some() || dom_status2.is_some();

    let dom_ref = dom_status2.as_ref().or(dom_status1.as_ref());
    let signals = dom_ref.and_then(|d| d.get("signals"));

    let has_loading_indicators = signals
        .and_then(|s| s.get("loading-indicators"))
        .and_then(|li| li.get("idle"))
        .and_then(|v| v.as_bool())
        .map(|idle| !idle)
        .unwrap_or(false);

    let loading_indicators_list = signals
        .and_then(|s| s.get("loading-indicators"))
        .and_then(|li| li.get("status"))
        .and_then(|s| s.get("indicators"))
        .cloned()
        .unwrap_or(serde_json::json!([]));

    let network_busy = signals
        .and_then(|s| s.get("network"))
        .and_then(|net| net.get("idle"))
        .and_then(|v| v.as_bool())
        .map(|idle| !idle)
        .unwrap_or(false);

    let pending_requests = signals
        .and_then(|s| s.get("network"))
        .and_then(|net| net.get("status"))
        .and_then(|s| s.get("pendingCount"))
        .and_then(|c| c.as_u64())
        .unwrap_or(0);

    // Phase 8: Determine verdict
    let obs_secs = observation_ms / 1000;

    let (verdict, confidence, summary, suggestions) =
        if !screenshot_changed && !ui_bridge_responsive {
            (
                "stuck",
                0.95f64,
                format!(
                    "The app appears stuck. The screen has not changed during the \
                     {obs_secs}s observation window and the UI Bridge is not responding \
                     (React may not have mounted)."
                ),
                vec![
                    "Check if the Tauri webview loaded successfully.",
                    "Check the browser console for JavaScript errors.",
                    "Check if the API server started (ports 9876-9878).",
                    "Try restarting the runner.",
                ],
            )
        } else if !screenshot_changed && has_loading_indicators {
            (
                "stuck",
                0.95,
                format!(
                    "The app appears stuck on a loading screen. Loading indicators \
                     are visible, the screen has not changed during the {obs_secs}s \
                     observation window, and no content is being rendered."
                ),
                vec![
                    "Check if a required backend service is running.",
                    "Check the browser console for JavaScript errors.",
                    "Try refreshing the page.",
                ],
            )
        } else if !screenshot_changed && network_busy {
            (
                "stuck",
                0.7,
                format!(
                    "The app appears stuck. The screen has not changed during the \
                     {obs_secs}s observation window but {pending_requests} network \
                     request(s) are still in flight. A request may be hanging."
                ),
                vec![
                    "Check if a network request is hanging.",
                    "Verify the API server is reachable.",
                ],
            )
        } else if !screenshot_changed && ui_bridge_responsive && !has_loading_indicators {
            (
                "idle",
                0.9,
                "The app appears to be in a normal resting state. No loading \
                 indicators detected and the screen is stable."
                    .to_string(),
                vec![],
            )
        } else if screenshot_changed && has_loading_indicators {
            (
                "loading",
                0.85,
                format!(
                    "The app is loading. The screen changed during the {obs_secs}s \
                     observation window and loading indicators are visible, indicating \
                     content is being rendered."
                ),
                vec![],
            )
        } else if screenshot_changed && !ui_bridge_responsive {
            (
                "unknown",
                0.5,
                "The screen is changing but the UI Bridge is not responding. \
                 The app may be loading or recovering."
                    .to_string(),
                vec!["Wait a few seconds and try again."],
            )
        } else {
            (
                "idle",
                0.7,
                "The app appears to be in a normal state. The screen changed \
                 slightly during observation but no loading indicators are present."
                    .to_string(),
                vec![],
            )
        };

    let evidence = serde_json::json!({
        "screenshotSimilarity": similarity,
        "screenshotChanged": screenshot_changed,
        "uiBridgeResponsive": ui_bridge_responsive,
        "loadingIndicators": loading_indicators_list,
        "networkBusy": network_busy,
        "pendingNetworkRequests": pending_requests,
    });

    let diagnosis = serde_json::json!({
        "verdict": verdict,
        "confidence": confidence,
        "summary": summary,
        "evidence": evidence,
        "observationWindowMs": observation_ms,
        "suggestions": suggestions,
        "screenshot": cap2.base64,
        "screenshotWidth": cap2.width,
        "screenshotHeight": cap2.height,
        "captureSource": cap2.source,
        "timestamp": chrono::Utc::now().timestamp_millis(),
    });

    Json(ApiResponse::success(diagnosis))
}

// ============================================================================
// Page Health Analysis
// ============================================================================

/// Optional request body for page-health endpoint.
#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct PageHealthRequest {
    /// Reserved for future per-check toggles.
    #[serde(default)]
    pub options: Option<serde_json::Value>,
}

/// Analyse the current page by running discover internally and returning a
/// structured `PageHealthReport` with spatial coverage, layout regions,
/// element diversity, text signal scanning, interactive readiness, visual
/// anomalies and an ASCII heatmap.
pub async fn ui_bridge_page_health_handler(
    State(state): State<Arc<ApiState>>,
    body: Option<Json<PageHealthRequest>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Page health analysis");

    // --- Step 1: run discover to get all elements -------------------------
    let _body = body.map(|b| b.0).unwrap_or_default();

    let discover_payload = serde_json::json!({
        "options": {
            "includeHidden": true
        }
    });

    let discover_data = match ui_bridge_request_sync(&state, "discover", discover_payload).await {
        Ok(d) => d,
        Err(e) => {
            error!("UI Bridge API: page-health discover failed: {}", e);
            return Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))));
        }
    };

    // Elements live under "elements" key returned by discover.
    let elements = discover_data
        .get("elements")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let element_count = elements.len();

    // Visible elements: state.visible == true and state.normalizedRect present.
    let visible: Vec<&serde_json::Value> = elements
        .iter()
        .filter(|el| {
            let state = el.get("state");
            let is_visible = state
                .and_then(|s| s.get("visible"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let has_rect = state.and_then(|s| s.get("normalizedRect")).is_some();
            is_visible && has_rect
        })
        .collect();
    let visible_count = visible.len();

    let mut findings: Vec<serde_json::Value> = Vec::new();

    // --- Step 2: Spatial coverage (20x20 grid) ----------------------------
    const GRID: usize = 20;
    let mut grid = [[false; GRID]; GRID];

    for el in &visible {
        if let Some(rect) = el.get("state").and_then(|s| s.get("normalizedRect")) {
            let x = rect.get("x").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let y = rect.get("y").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let w = rect.get("width").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let h = rect.get("height").and_then(|v| v.as_f64()).unwrap_or(0.0);

            let col_start = (x * GRID as f64).floor().max(0.0) as usize;
            let col_end = ((x + w) * GRID as f64).ceil().min(GRID as f64) as usize;
            let row_start = (y * GRID as f64).floor().max(0.0) as usize;
            let row_end = ((y + h) * GRID as f64).ceil().min(GRID as f64) as usize;

            for row in grid.iter_mut().take(row_end.min(GRID)).skip(row_start) {
                for cell in row.iter_mut().take(col_end.min(GRID)).skip(col_start) {
                    *cell = true;
                }
            }
        }
    }

    let total_cells = (GRID * GRID) as f64;
    let filled_cells = grid.iter().flatten().filter(|&&v| v).count() as f64;
    let coverage_pct = (filled_cells / total_cells * 100.0).round();

    // Left half = columns 0..10, right half = columns 10..20
    let left_filled = grid
        .iter()
        .flat_map(|row| row[..GRID / 2].iter())
        .filter(|&&v| v)
        .count() as f64;
    let right_filled = grid
        .iter()
        .flat_map(|row| row[GRID / 2..].iter())
        .filter(|&&v| v)
        .count() as f64;
    let half_cells = (GRID * GRID / 2) as f64;
    let left_half_pct = (left_filled / half_cells * 100.0).round();
    let right_half_pct = (right_filled / half_cells * 100.0).round();

    let spatial_severity = if coverage_pct < 15.0 {
        "CRITICAL"
    } else if coverage_pct < 30.0 {
        "WARNING"
    } else {
        "OK"
    };

    // Sidebar-only: right < 5% and left > 20%
    let spatial_severity = if right_half_pct < 5.0 && left_half_pct > 20.0 {
        "CRITICAL"
    } else {
        spatial_severity
    };

    findings.push(serde_json::json!({
        "check": "spatial_coverage",
        "severity": spatial_severity,
        "detail": format!(
            "Elements cover {}% of viewport. Left={}%, Right={}%",
            coverage_pct, left_half_pct, right_half_pct
        ),
        "data": {
            "coverage_pct": coverage_pct,
            "left_half_pct": left_half_pct,
            "right_half_pct": right_half_pct
        }
    }));

    // --- Step 3: Layout regions -------------------------------------------
    let mut sidebar_count: usize = 0;
    let mut header_count: usize = 0;
    let mut content_count: usize = 0;

    for el in &visible {
        if let Some(rect) = el.get("state").and_then(|s| s.get("normalizedRect")) {
            let x = rect.get("x").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let w = rect.get("width").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let y = rect.get("y").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let h = rect.get("height").and_then(|v| v.as_f64()).unwrap_or(0.0);

            let cx = x + w / 2.0;
            let cy = y + h / 2.0;

            if cx < 0.2 {
                sidebar_count += 1;
            } else if cy < 0.08 {
                header_count += 1;
            } else {
                content_count += 1;
            }
        }
    }

    let layout_severity = if content_count == 0 {
        "CRITICAL"
    } else if content_count < 3 {
        "WARNING"
    } else {
        "OK"
    };

    findings.push(serde_json::json!({
        "check": "layout_regions",
        "severity": layout_severity,
        "detail": format!(
            "sidebar={}, header={}, content={}",
            sidebar_count, header_count, content_count
        ),
        "data": {
            "sidebar": sidebar_count,
            "header": header_count,
            "content": content_count
        }
    }));

    // --- Step 4: Element diversity ----------------------------------------
    let nav_types: &[&str] = &["button", "heading", "badge", "status-message"];
    let mut type_counts: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    for el in &elements {
        let t = el.get("type").and_then(|v| v.as_str()).unwrap_or("unknown");
        *type_counts.entry(t.to_string()).or_insert(0) += 1;
    }

    let all_nav = element_count > 5 && type_counts.keys().all(|k| nav_types.contains(&k.as_str()));

    let diversity_severity = if all_nav { "WARNING" } else { "OK" };

    findings.push(serde_json::json!({
        "check": "element_diversity",
        "severity": diversity_severity,
        "detail": format!(
            "{} type(s) across {} elements{}",
            type_counts.len(),
            element_count,
            if all_nav { " (navigation-only)" } else { "" }
        ),
        "data": {
            "types": type_counts
        }
    }));

    // --- Step 5: Text signal scanning -------------------------------------
    let skip_types: &[&str] = &["button", "link", "tab", "menuitem"];

    let error_phrases: &[&str] = &[
        "error occurred",
        "failed to",
        "exception",
        "crash",
        "unavailable",
        "something went wrong",
        "could not",
    ];
    let loading_phrases: &[&str] = &[
        "loading",
        "starting",
        "connecting",
        "please wait",
        "initializing",
        "fetching",
    ];
    let empty_phrases: &[&str] = &[
        "no data",
        "no results",
        "nothing here",
        "empty",
        "no items",
        "get started",
    ];
    let css_signals: &[&str] = &["spin", "pulse", "skeleton", "loading", "shimmer"];

    let mut detected_errors: Vec<String> = Vec::new();
    let mut detected_loading: Vec<String> = Vec::new();
    let mut detected_empty: Vec<String> = Vec::new();
    let mut detected_css: Vec<String> = Vec::new();

    for el in &elements {
        let el_type = el.get("type").and_then(|v| v.as_str()).unwrap_or("");
        let text = el
            .get("state")
            .and_then(|s| s.get("textContent"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        // "classes" is a top-level array of strings
        let classes_arr = el.get("classes").and_then(|v| v.as_array());
        let classes_str: String = classes_arr
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str())
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .unwrap_or_default();

        // Check CSS class signals on all elements
        let classes_lower = classes_str.to_lowercase();
        for sig in css_signals {
            if classes_lower.contains(sig) {
                detected_css.push(format!("class contains '{}' on {}", sig, el_type));
                break;
            }
        }

        // Skip navigation types for text scanning
        if skip_types.contains(&el_type) {
            continue;
        }

        let text_lower = text.to_lowercase();
        for phrase in error_phrases {
            if text_lower.contains(phrase) {
                detected_errors.push(text.chars().take(120).collect());
                break;
            }
        }
        for phrase in loading_phrases {
            if text_lower.contains(phrase) {
                detected_loading.push(text.chars().take(120).collect());
                break;
            }
        }
        for phrase in empty_phrases {
            if text_lower.contains(phrase) {
                detected_empty.push(text.chars().take(120).collect());
                break;
            }
        }
    }

    let text_severity = if !detected_errors.is_empty() {
        "CRITICAL"
    } else if !detected_loading.is_empty() || !detected_css.is_empty() || !detected_empty.is_empty()
    {
        "WARNING"
    } else {
        "OK"
    };

    findings.push(serde_json::json!({
        "check": "text_signals",
        "severity": text_severity,
        "detail": format!(
            "errors={}, loading={}, empty={}, css_signals={}",
            detected_errors.len(),
            detected_loading.len(),
            detected_empty.len(),
            detected_css.len()
        ),
        "data": {
            "errors": detected_errors,
            "loading": detected_loading,
            "empty": detected_empty,
            "css_signals": detected_css
        }
    }));

    // --- Step 6: Interactive readiness ------------------------------------
    let mut interactive_total: usize = 0;
    let mut interactive_disabled: usize = 0;

    for el in &elements {
        let cat = el.get("category").and_then(|v| v.as_str()).unwrap_or("");
        if cat == "interactive" {
            interactive_total += 1;
            let enabled = el
                .get("state")
                .and_then(|s| s.get("enabled"))
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            if !enabled {
                interactive_disabled += 1;
            }
        }
    }

    let interactive_severity = if interactive_total > 0
        && (interactive_disabled as f64 / interactive_total as f64) > 0.5
    {
        "WARNING"
    } else {
        "OK"
    };

    findings.push(serde_json::json!({
        "check": "interactive_readiness",
        "severity": interactive_severity,
        "detail": format!(
            "{} interactive elements, {} disabled",
            interactive_total, interactive_disabled
        ),
        "data": {
            "total": interactive_total,
            "disabled": interactive_disabled
        }
    }));

    // --- Step 7: Visual anomalies -----------------------------------------
    let mut zero_size: usize = 0;
    let mut outside_viewport: usize = 0;

    for el in &visible {
        if let Some(rect) = el.get("state").and_then(|s| s.get("normalizedRect")) {
            let x = rect.get("x").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let y = rect.get("y").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let w = rect.get("width").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let h = rect.get("height").and_then(|v| v.as_f64()).unwrap_or(0.0);

            if w == 0.0 || h == 0.0 {
                zero_size += 1;
            }
            if x + w < 0.0 || y + h < 0.0 || x > 1.0 || y > 1.0 {
                outside_viewport += 1;
            }
        }
    }

    let anomaly_severity = if zero_size > 0 || outside_viewport > 0 {
        "WARNING"
    } else {
        "OK"
    };

    findings.push(serde_json::json!({
        "check": "visual_anomalies",
        "severity": anomaly_severity,
        "detail": format!(
            "zero_size={}, outside_viewport={}",
            zero_size, outside_viewport
        ),
        "data": {
            "zero_size": zero_size,
            "outside_viewport": outside_viewport
        }
    }));

    // --- Step 8: ASCII heatmap --------------------------------------------
    let heatmap: Vec<String> = grid
        .iter()
        .map(|row| {
            row.iter()
                .map(|&filled| if filled { '#' } else { '.' })
                .collect()
        })
        .collect();

    // --- Step 9: Determine worst severity ---------------------------------
    let severity_rank = |s: &str| -> u8 {
        match s {
            "CRITICAL" => 3,
            "WARNING" => 2,
            "OK" => 1,
            _ => 0,
        }
    };

    let worst = findings
        .iter()
        .filter_map(|f| f.get("severity").and_then(|s| s.as_str()))
        .max_by_key(|s| severity_rank(s))
        .unwrap_or("OK");

    let report = serde_json::json!({
        "summary": worst,
        "findings": findings,
        "heatmap": heatmap,
        "element_count": element_count,
        "visible_count": visible_count
    });

    Ok(Json(ApiResponse::success(report)))
}

// ============================================================================
// AI Search & Find Handlers
// ============================================================================

/// AI-powered element search.
pub async fn ui_bridge_ai_search_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: AI search");

    let payload = serde_json::json!({ "params": body });

    match ui_bridge_request_sync(&state, "ai_search", payload).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: AI search failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Natural language element find.
pub async fn ui_bridge_ai_find_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: AI find");

    let payload = serde_json::json!({ "params": body });

    match ui_bridge_request_sync(&state, "ai_find", payload).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: AI find failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Natural language action execution.
pub async fn ui_bridge_ai_execute_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: AI execute");
    let payload = serde_json::json!({ "params": body });
    match ui_bridge_request_sync(&state, "ai_execute", payload).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: AI execute failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// AI assertion evaluation.
pub async fn ui_bridge_ai_assert_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: AI assert");
    let payload = serde_json::json!({ "params": body });
    match ui_bridge_request_sync(&state, "ai_assert", payload).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: AI assert failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Batch AI assertion evaluation.
pub async fn ui_bridge_ai_assert_batch_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: AI assert batch");
    let payload = serde_json::json!({ "params": body });
    match ui_bridge_request_sync(&state, "ai_assert_batch", payload).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: AI assert batch failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Query parameters for the AI snapshot endpoint.
#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AiSnapshotQuery {
    /// Maximum token budget for the snapshot (0 = unlimited).
    /// When set, elements are pruned by region priority.
    #[serde(default)]
    pub max_tokens: Option<u32>,
}

/// Semantic AI snapshot of the page.
pub async fn ui_bridge_ai_snapshot_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<AiSnapshotQuery>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!(
        "UI Bridge API: AI snapshot (maxTokens={:?})",
        query.max_tokens
    );
    let mut payload = serde_json::json!({});
    if let Some(max_tokens) = query.max_tokens {
        payload["maxTokens"] = serde_json::json!(max_tokens);
    }
    match ui_bridge_request_sync(&state, "ai_snapshot", payload).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: AI snapshot failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Natural language page summary.
pub async fn ui_bridge_ai_summary_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: AI summary");
    match ui_bridge_request_sync(&state, "ai_summary", serde_json::json!({})).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: AI summary failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Capabilities endpoint — static listing of all supported features.
pub async fn ui_bridge_capabilities_handler() -> Json<ApiResponse<serde_json::Value>> {
    Json(ApiResponse::success(serde_json::json!({
        "version": "0.3.1",
        "appId": "qontinui-runner",
        "appType": "desktop",
        "framework": "tauri",
        "categories": {
            "control": {
                "description": "Element discovery, actions, navigation, forms",
                "endpoints": ["snapshot", "elements", "element/:id", "element/:id/state", "element/:id/action",
                    "discover", "find", "components", "component/:id/state", "page/navigate", "page/refresh",
                    "page/back", "page/forward", "page/evaluate", "forms", "fill", "forms/snapshot", "forms/diff",
                    "workflows", "specs", "query-selector", "keyboard-shortcuts"]
            },
            "ai": {
                "description": "AI-powered search, assertions, semantic snapshots, NL actions",
                "endpoints": ["find", "search", "execute", "assert", "assert-batch", "snapshot", "summary",
                    "semantic-search", "diff", "execute-with-diff", "bookmarks", "scoped-diff",
                    "summarize-diff", "categorize-last-diff"]
            },
            "media": {
                "description": "Media element discovery, audits, and analysis",
                "endpoints": ["media/find", "media/audit/accessibility", "media/audit/performance",
                    "media/snapshot", "media/analyze", "media/analyze/batch", "media/analyze/page", "media/compare"]
            },
            "stateMachine": {
                "description": "State discovery, activation, transitions, navigation",
                "endpoints": ["states", "states/active", "states/snapshot", "states/find-path",
                    "states/navigate", "state/:id", "state/:id/activate", "state/:id/deactivate",
                    "state-groups", "transitions", "transition/:id/can-execute", "transition/:id/execute"]
            },
            "idle": {
                "description": "Composite and per-signal idle detection",
                "endpoints": ["idle-status", "idle-status/:signal", "wait-for-idle",
                    "wait-for-idle/:signal", "wait-for-targets"]
            },
            "design": {
                "description": "Design inspection, styles, audit, responsive, evaluation",
                "endpoints": ["design/element/{id}/styles", "design/snapshot", "design/audit", "design/responsive",
                    "design/evaluate", "design/evaluate/baseline", "design/evaluate/contexts", "design/evaluate/diff"]
            },
            "network": {
                "description": "Network request monitoring",
                "endpoints": ["network-requests", "network-requests/in-flight", "network-request/:id",
                    "network-requests/wait"]
            },
            "errorTracking": {
                "description": "Error sessions, baselines, reports",
                "endpoints": ["error-sessions/start", "error-sessions", "error-sessions/end",
                    "error-baselines/capture", "error-baselines/compare", "error-report", "error-snapshots"]
            },
            "clipboard": {
                "description": "System clipboard read/write (OS-level via arboard)",
                "available": true
            },
            "jsEvaluation": {
                "description": "Arbitrary JavaScript evaluation in webview",
                "available": true
            },
            "annotations": {
                "description": "Element annotation CRUD with coverage tracking",
                "endpoints": ["annotations", "annotation/{id}", "annotations/coverage", "annotations/export", "annotations/import"]
            },
            "intents": {
                "description": "Intent registration, discovery, and execution",
                "endpoints": ["intents", "intents/find", "intents/execute", "intents/execute-from-query"]
            },
            "history": {
                "description": "Action history, interaction metrics, performance entries",
                "endpoints": ["action-history", "metrics", "performance-entries"]
            },
            "timeline": {
                "description": "Performance timeline, browser events, error snapshots",
                "endpoints": ["timeline", "browser-events", "error-snapshots"]
            },
            "debug": {
                "description": "Debug tools for element inspection and highlighting",
                "endpoints": ["element-tree", "highlight/{id}"]
            },
            "analysis": {
                "description": "AI-powered analysis: data extraction, regions, structured data, cross-app",
                "endpoints": ["analyze/data", "analyze/regions", "analyze/structured-data",
                    "analyze/cross-app-compare", "recovery/attempt"]
            }
        }
    })))
}

/// Get individual idle signal status.
pub async fn ui_bridge_get_idle_signal_handler(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(signal): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Get idle signal '{}'", signal);
    let payload = serde_json::json!({ "params": { "signal": signal } });
    match ui_bridge_request_sync(&state, "get_idle_signal", payload).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Get idle signal failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Wait for a specific idle signal.
pub async fn ui_bridge_wait_for_idle_signal_handler(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(signal): axum::extract::Path<String>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Wait for idle signal '{}'", signal);
    let mut params = body;
    if let Some(obj) = params.as_object_mut() {
        obj.insert("signal".to_string(), serde_json::json!(signal));
    } else {
        params = serde_json::json!({ "signal": signal });
    }
    let payload = serde_json::json!({ "params": params });
    match ui_bridge_request_sync(&state, "wait_for_idle_signal", payload).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Wait for idle signal failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Wait for multiple idle targets.
pub async fn ui_bridge_wait_for_targets_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Wait for targets");
    let payload = serde_json::json!({ "params": body });
    match ui_bridge_request_sync(&state, "wait_for_targets", payload).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Wait for targets failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Get action history.
pub async fn ui_bridge_get_action_history_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Get action history");
    match ui_bridge_request_sync(&state, "get_action_history", serde_json::json!({})).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Get action history failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Get interaction metrics.
pub async fn ui_bridge_get_interaction_metrics_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Get interaction metrics");
    match ui_bridge_request_sync(&state, "get_interaction_metrics", serde_json::json!({})).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Get interaction metrics failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// List all annotations.
pub async fn ui_bridge_annotations_list_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: List annotations");
    match ui_bridge_request_sync(&state, "annotations_list", serde_json::json!({})).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: List annotations failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Create annotation.
pub async fn ui_bridge_annotations_create_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Create annotation");
    let payload = serde_json::json!({ "params": body });
    match ui_bridge_request_sync(&state, "annotations_create", payload).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Create annotation failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Get single annotation.
pub async fn ui_bridge_annotations_get_handler(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Get annotation '{}'", id);
    let payload = serde_json::json!({ "params": { "id": id } });
    match ui_bridge_request_sync(&state, "annotations_get", payload).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Get annotation failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Update annotation.
pub async fn ui_bridge_annotations_update_handler(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Update annotation '{}'", id);
    let payload = serde_json::json!({ "params": { "id": id, "updates": body } });
    match ui_bridge_request_sync(&state, "annotations_update", payload).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Update annotation failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Delete annotation.
pub async fn ui_bridge_annotations_delete_handler(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Delete annotation '{}'", id);
    let payload = serde_json::json!({ "params": { "id": id } });
    match ui_bridge_request_sync(&state, "annotations_delete", payload).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Delete annotation failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Get annotation coverage metrics.
pub async fn ui_bridge_annotations_coverage_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Annotation coverage");
    match ui_bridge_request_sync(&state, "annotations_coverage", serde_json::json!({})).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Annotation coverage failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Export annotations.
pub async fn ui_bridge_annotations_export_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Export annotations");
    match ui_bridge_request_sync(&state, "annotations_export", serde_json::json!({})).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: Export annotations failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

// ============================================================================
// Media routes (IPC to webview SDK)
// ============================================================================

/// Find media elements.
pub async fn ui_bridge_media_find_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let payload = serde_json::json!({ "params": body });
    match ui_bridge_request_sync(&state, "find_media", payload).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// Media audit (accessibility or performance).
pub async fn ui_bridge_media_audit_handler(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(audit_type): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let payload = serde_json::json!({ "params": { "auditType": audit_type } });
    match ui_bridge_request_sync(&state, "media_audit", payload).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// Capture media snapshot.
pub async fn ui_bridge_media_snapshot_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let payload = serde_json::json!({ "params": body });
    match ui_bridge_request_sync(&state, "capture_media_snapshot", payload).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// Analyze media elements.
pub async fn ui_bridge_media_analyze_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let payload = serde_json::json!({ "params": body });
    match ui_bridge_request_sync(&state, "analyze_media", payload).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

// ============================================================================
// State machine routes (IPC to webview SDK)
// ============================================================================

macro_rules! ipc_handler_get {
    ($fn_name:ident, $ipc_type:expr) => {
        pub async fn $fn_name(
            State(state): State<Arc<ApiState>>,
        ) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
            match ui_bridge_request_sync(&state, $ipc_type, serde_json::json!({})).await {
                Ok(data) => Ok(Json(ApiResponse::success(data))),
                Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
            }
        }
    };
}

macro_rules! ipc_handler_post {
    ($fn_name:ident, $ipc_type:expr) => {
        pub async fn $fn_name(
            State(state): State<Arc<ApiState>>,
            Json(body): Json<serde_json::Value>,
        ) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
            let payload = serde_json::json!({ "params": body });
            match ui_bridge_request_sync(&state, $ipc_type, payload).await {
                Ok(data) => Ok(Json(ApiResponse::success(data))),
                Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
            }
        }
    };
}

macro_rules! ipc_handler_path_get {
    ($fn_name:ident, $ipc_type:expr, $param_name:expr) => {
        pub async fn $fn_name(
            State(state): State<Arc<ApiState>>,
            axum::extract::Path(id): axum::extract::Path<String>,
        ) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
            let payload = serde_json::json!({ "params": { $param_name: id } });
            match ui_bridge_request_sync(&state, $ipc_type, payload).await {
                Ok(data) => Ok(Json(ApiResponse::success(data))),
                Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
            }
        }
    };
}

macro_rules! ipc_handler_path_post {
    ($fn_name:ident, $ipc_type:expr, $param_name:expr) => {
        pub async fn $fn_name(
            State(state): State<Arc<ApiState>>,
            axum::extract::Path(id): axum::extract::Path<String>,
            Json(body): Json<serde_json::Value>,
        ) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
            let mut params = body;
            if let Some(obj) = params.as_object_mut() {
                obj.insert($param_name.to_string(), serde_json::json!(id));
            } else {
                // Body wasn't an object; create one with just the path param
                params = serde_json::json!({ $param_name: id });
            }
            let payload = serde_json::json!({ "params": params });
            match ui_bridge_request_sync(&state, $ipc_type, payload).await {
                Ok(data) => Ok(Json(ApiResponse::success(data))),
                Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
            }
        }
    };
}

// State machine
ipc_handler_get!(ui_bridge_get_states_handler, "get_states");
ipc_handler_get!(ui_bridge_get_active_states_handler, "get_active_states");
ipc_handler_get!(ui_bridge_get_state_snapshot_handler, "get_state_snapshot");
ipc_handler_path_get!(ui_bridge_get_state_handler, "get_state", "stateId");
ipc_handler_path_post!(
    ui_bridge_activate_state_handler,
    "activate_state",
    "stateId"
);
ipc_handler_path_post!(
    ui_bridge_deactivate_state_handler,
    "deactivate_state",
    "stateId"
);
ipc_handler_get!(ui_bridge_get_state_groups_handler, "get_state_groups");
ipc_handler_path_post!(
    ui_bridge_activate_state_group_handler,
    "activate_state_group",
    "groupId"
);
ipc_handler_path_post!(
    ui_bridge_deactivate_state_group_handler,
    "deactivate_state_group",
    "groupId"
);
ipc_handler_get!(ui_bridge_get_transitions_handler, "get_transitions");
ipc_handler_path_get!(
    ui_bridge_can_execute_transition_handler,
    "can_execute_transition",
    "transitionId"
);
ipc_handler_path_post!(
    ui_bridge_execute_transition_handler,
    "execute_transition",
    "transitionId"
);
ipc_handler_post!(ui_bridge_find_state_path_handler, "find_state_path");
ipc_handler_post!(ui_bridge_navigate_to_state_handler, "navigate_to_state");

// Runner-specific: tab navigation and storage management
ipc_handler_post!(ui_bridge_navigate_tab_handler, "navigate_tab");
ipc_handler_post!(ui_bridge_clear_storage_handler, "clear_storage");

// AI semantic search & diff
ipc_handler_post!(ui_bridge_ai_semantic_search_handler, "ai_semantic_search");
ipc_handler_get!(ui_bridge_ai_diff_handler, "ai_diff");

// Intents
ipc_handler_get!(ui_bridge_get_intents_handler, "get_intents");
ipc_handler_post!(ui_bridge_register_intent_handler, "register_intent");
ipc_handler_post!(ui_bridge_find_intent_handler, "find_intent");
ipc_handler_post!(ui_bridge_execute_intent_handler, "execute_intent");

// Component state
ipc_handler_path_get!(
    ui_bridge_get_component_state_handler,
    "get_component_state",
    "componentId"
);

// Page scroll
ipc_handler_post!(ui_bridge_scroll_page_handler, "scroll_page");

// Performance entries
ipc_handler_get!(
    ui_bridge_get_performance_entries_handler,
    "get_performance_entries"
);
ipc_handler_get!(
    ui_bridge_clear_performance_entries_handler,
    "clear_performance_entries"
);

// AI analysis
ipc_handler_post!(ui_bridge_ai_analyze_data_handler, "ai_analyze_data");
ipc_handler_post!(ui_bridge_ai_analyze_regions_handler, "ai_analyze_regions");
ipc_handler_post!(
    ui_bridge_ai_analyze_structured_handler,
    "ai_analyze_structured_data"
);
ipc_handler_post!(
    ui_bridge_ai_analyze_cross_app_handler,
    "ai_analyze_cross_app"
);
ipc_handler_post!(ui_bridge_ai_recovery_attempt_handler, "ai_recovery_attempt");

// Design evaluation
ipc_handler_post!(ui_bridge_design_evaluate_handler, "design_evaluate");
ipc_handler_post!(
    ui_bridge_design_evaluate_baseline_handler,
    "design_evaluate_baseline"
);
ipc_handler_get!(
    ui_bridge_design_evaluate_contexts_handler,
    "design_evaluate_contexts"
);
ipc_handler_post!(
    ui_bridge_design_evaluate_diff_handler,
    "design_evaluate_diff"
);

// Media compare
ipc_handler_post!(ui_bridge_media_compare_handler, "media_compare");

// Annotations import
ipc_handler_post!(ui_bridge_annotations_import_handler, "annotations_import");

// Intents from NL query
ipc_handler_post!(
    ui_bridge_execute_intent_from_query_handler,
    "execute_intent_from_query"
);

// Debug
ipc_handler_get!(ui_bridge_get_element_tree_handler, "get_element_tree");
ipc_handler_path_get!(
    ui_bridge_highlight_element_handler,
    "highlight_element",
    "elementId"
);

/// Find elements matching criteria.
pub async fn ui_bridge_find_handler(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Find elements");

    match ui_bridge_request_sync(&state, "find", request).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// POST /ui-bridge/control/capture-element-images — Capture element images directly from the DOM.
///
/// Uses html2canvas in the frontend to render each element to a canvas, bypassing
/// screen capture entirely. This produces correct images even when other windows
/// cover the runner.
///
/// Body: `{ "element_ids": ["btn-save", "input-name"] }` (optional — null captures all visible)
/// Returns: `{ "captures": { "btn-save": { "base64_png": "...", "width": 80, "height": 30 }, ... } }`
pub async fn ui_bridge_capture_element_images_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Capture element images");

    // Use a longer timeout for element capture — html2canvas rendering 30+ elements
    // can take 30-60 seconds. The default 10s UI Bridge IPC timeout is too short.
    let request_id = uuid::Uuid::new_v4().to_string();
    let event_payload = serde_json::json!({
        "requestId": request_id,
        "type": "capture_element_images",
        "params": body,
    });

    let (tx, rx) = tokio::sync::oneshot::channel::<serde_json::Value>();
    {
        let mut pending = state.ui_bridge_pending.lock().await;
        pending.insert(request_id.clone(), tx);
        state
            .ui_bridge_pending_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    if let Err(e) = state.app_handle.emit("ui-bridge-request", &event_payload) {
        let mut pending = state.ui_bridge_pending.lock().await;
        if pending.remove(&request_id).is_some() {
            state
                .ui_bridge_pending_count
                .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        }
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to emit request: {}", e))),
        ));
    }

    // 120 second timeout for element capture (vs 10s default)
    let timeout = std::time::Duration::from_secs(120);
    match tokio::time::timeout(timeout, rx).await {
        Ok(Ok(data)) => Ok(Json(ApiResponse::success(data))),
        Ok(Err(_)) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error("Request channel closed".to_string())),
        )),
        Err(_) => {
            let mut pending = state.ui_bridge_pending.lock().await;
            if pending.remove(&request_id).is_some() {
                state
                    .ui_bridge_pending_count
                    .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
            }
            error!("UI Bridge API: Capture element images timed out after 120s");
            Err((
                StatusCode::GATEWAY_TIMEOUT,
                Json(api_error(
                    "Element capture timed out after 120s".to_string(),
                )),
            ))
        }
    }
}

/// POST /ui-bridge/control/get-element-images — Read <img> src attributes from the DOM.
///
/// A lightweight alternative to capture-element-images that reads image metadata
/// (src, alt, dimensions) without rendering via html2canvas. Useful for verifying
/// which images are displayed (e.g., thumbnail URLs on state cards).
///
/// Body: `{ "element_id": "some-container", "max_images": 50, "full_src": false, "image_index": 0 }`
/// - `element_id` (optional): Scope search to a specific UI Bridge element
/// - `max_images` (optional, default 50): Maximum number of images to return
/// - `full_src` (optional, default false): Return full data: URIs instead of truncated
/// - `image_index` (optional): When `full_src` is true, only return full src for this index
///
/// Returns: `{ "images": [...], "total": N }`
pub async fn ui_bridge_get_element_images_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Get element images");

    let payload = serde_json::json!({ "params": body });
    match ui_bridge_request_sync(&state, "get_element_images", payload).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: get_element_images failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// POST /ui-bridge/control/workflow/:id/run — Run a workflow via the unified workflow engine.
/// Proxies to the runner's existing `/unified-workflows/:id/run` endpoint via internal HTTP.
pub async fn ui_bridge_run_workflow_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    body: Option<Json<serde_json::Value>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Running workflow {}", id);

    let base_url = crate::mcp::types::get_self_base_url(&state.app_state);
    let url = format!("{}/unified-workflows/{}/run", base_url, id);

    let request_body = body
        .map(|Json(v)| v)
        .unwrap_or_else(|| serde_json::json!({ "force_fresh_start": false }));

    let client = reqwest::Client::new();
    match client.post(&url).json(&request_body).send().await {
        Ok(resp) => {
            let status = resp.status();
            match resp.json::<serde_json::Value>().await {
                Ok(result) => {
                    if status.is_success() {
                        Ok(Json(ApiResponse::success(result)))
                    } else {
                        let err_msg = result
                            .get("error")
                            .and_then(|e| e.as_str())
                            .unwrap_or("Workflow execution failed")
                            .to_string();
                        Err((
                            StatusCode::from_u16(status.as_u16())
                                .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                            Json(api_error(err_msg)),
                        ))
                    }
                }
                Err(e) => {
                    error!("UI Bridge API: Failed to parse run response: {}", e);
                    Err((
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(api_error(format!("Failed to parse response: {}", e))),
                    ))
                }
            }
        }
        Err(e) => {
            error!("UI Bridge API: Failed to call unified workflow run: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to run workflow: {}", e))),
            ))
        }
    }
}

/// GET /ui-bridge/control/workflow/:run_id/status — Get workflow run status.
/// Reads the task run directly from the checkpoint database.
pub async fn ui_bridge_get_workflow_status_handler(
    State(state): State<Arc<ApiState>>,
    Path(run_id): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Getting workflow status for run {}", run_id);

    let task_run_result = state.app_state.pg_db.get_task_run(&run_id).await;
    match task_run_result {
        Ok(Some(task_run)) => {
            let status = match task_run.status.as_str() {
                "running" | "in_progress" => "running",
                "complete" | "completed" | "success" => "completed",
                "failed" | "error" => "failed",
                "stopped" | "cancelled" => "cancelled",
                _ => "pending",
            };
            let is_terminal = matches!(status, "completed" | "failed" | "cancelled");
            Ok(Json(ApiResponse::success(serde_json::json!({
                "workflowId": task_run.workflow_id.unwrap_or_default(),
                "runId": run_id,
                "status": status,
                "steps": [],
                "totalSteps": 0,
                "success": if is_terminal { Some(status == "completed") } else { None::<bool> },
                "taskName": task_run.task_name,
                "sessionsCount": task_run.sessions_count,
                "startedAt": task_run.created_at,
                "completedAt": task_run.completed_at,
                "error": task_run.error_message,
            }))))
        }
        Ok(None) => Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Task run not found: {}", run_id))),
        )),
        Err(e) => {
            error!("UI Bridge API: Failed to get task run: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to get task run: {}", e))),
            ))
        }
    }
}

/// Get all workflows.
pub async fn ui_bridge_get_workflows_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Getting workflows");

    match ui_bridge_request_sync(&state, "get_workflows", serde_json::json!({})).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Get element state by ID.
pub async fn ui_bridge_get_element_state_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Getting element state for {}", id);

    match ui_bridge_request_sync(
        &state,
        "get_element_state",
        serde_json::json!({ "elementId": id }),
    )
    .await
    {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Get render log entries.
pub async fn ui_bridge_get_render_log_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Getting render log");

    let log = state.ui_bridge_render_log.lock().await;
    Ok(Json(ApiResponse::success(serde_json::json!({
        "entries": *log,
        "count": log.len()
    }))))
}

/// Append an entry to the render log.
pub async fn ui_bridge_append_render_log_handler(
    State(state): State<Arc<ApiState>>,
    Json(entry): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let mut log = state.ui_bridge_render_log.lock().await;
    // Keep render log bounded to prevent memory leaks
    if log.len() >= 1000 {
        log.drain(0..100);
    }
    log.push(entry);
    Ok(Json(ApiResponse::success(serde_json::json!({
        "count": log.len()
    }))))
}

/// Manually reset the UI Bridge circuit breaker to Closed state.
pub async fn ui_bridge_circuit_breaker_reset_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Circuit breaker manual reset");
    state.ui_bridge_circuit_breaker.reset().await;
    Ok(Json(ApiResponse::success(serde_json::json!({
        "reset": true,
        "state": "Closed"
    }))))
}

/// UI Bridge diagnostics endpoint.
pub async fn ui_bridge_diagnostics_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Diagnostics");

    let cb_state = state.ui_bridge_circuit_breaker.get_state().await;
    let failure_count = state.ui_bridge_circuit_breaker.get_failure_count().await;
    let available_permits = state.ui_bridge_semaphore.available_permits();
    let last_pong = state
        .ui_bridge_last_pong
        .load(std::sync::atomic::Ordering::Relaxed);
    let pending_count = state
        .ui_bridge_pending_count
        .load(std::sync::atomic::Ordering::Relaxed);

    Ok(Json(ApiResponse::success(serde_json::json!({
        "circuitBreaker": {
            "state": format!("{:?}", cb_state),
            "failuresInWindow": failure_count
        },
        "semaphore": {
            "availablePermits": available_permits,
            "maxPermits": 6
        },
        "frontend": {
            "lastPongTimestamp": last_pong,
            "lastPongAgeMs": if last_pong > 0 {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64;
                now - last_pong
            } else { 0 }
        },
        "pendingRequestCount": pending_count
    }))))
}

/// Handle UI Bridge pong from frontend.
pub async fn ui_bridge_pong_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    state
        .ui_bridge_last_pong
        .store(now, std::sync::atomic::Ordering::Relaxed);
    // Unblock any requests waiting for frontend readiness
    state.ui_bridge_ready.notify_waiters();
    Ok(Json(ApiResponse::success(
        serde_json::json!({ "pong": true }),
    )))
}

/// Accept an IPC response via HTTP (fallback when Tauri event system is unavailable).
/// The frontend can POST responses here instead of using emit("ui-bridge-response").
pub async fn ui_bridge_ipc_response_handler(
    State(state): State<Arc<ApiState>>,
    Json(response): Json<serde_json::Value>,
) -> Json<ApiResponse<serde_json::Value>> {
    let pending = state.ui_bridge_pending.clone();
    let pending_count = state.ui_bridge_pending_count.clone();
    handle_ui_bridge_response(pending, pending_count, response).await;
    // Also update pong timestamp since this proves the frontend is alive
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    state
        .ui_bridge_last_pong
        .store(now, std::sync::atomic::Ordering::Relaxed);
    Json(ApiResponse::success(
        serde_json::json!({ "received": true }),
    ))
}

// ============================================================================
// Structured Action Plan Execution
// ============================================================================

/// A target specification for finding an element.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionPlanElementTarget {
    /// Direct element ID from a prior snapshot
    #[serde(default)]
    pub element_id: Option<String>,
    /// data-testid attribute value
    #[serde(default)]
    pub test_id: Option<String>,
    /// Natural language description for fuzzy search
    #[serde(default)]
    pub search_text: Option<String>,
    /// Element type hint (e.g., "button", "input")
    #[serde(default)]
    pub element_type: Option<String>,
    /// CSS selector
    #[serde(default)]
    pub selector: Option<String>,
}

/// A single planned action in an action plan.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlannedAction {
    /// Action type to execute (click, type, select, etc.)
    pub action: String,
    /// How to find the target element
    pub target: ActionPlanElementTarget,
    /// LLM's reasoning for this action (audit trail)
    #[serde(default)]
    pub reasoning: Option<String>,
    /// LLM's confidence (0.0–1.0)
    #[serde(default = "default_confidence")]
    pub confidence: f64,
    /// Action-specific parameters (text, value, direction, etc.)
    #[serde(default)]
    pub params: Option<serde_json::Value>,
    /// Separates generic intent from specific data (for caching).
    /// Example: "What email should be entered?"
    #[serde(default)]
    pub user_detail_query: Option<String>,
    /// The specific data for the query.
    /// Example: "test@example.com"
    #[serde(default)]
    pub user_detail_answer: Option<String>,
}

fn default_confidence() -> f64 {
    1.0
}

/// Request to execute a structured action plan.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionPlanRequest {
    /// Ordered list of actions to execute
    pub actions: Vec<PlannedAction>,
    /// High-level goal this plan achieves
    #[serde(default)]
    pub goal: Option<String>,
    /// Minimum confidence threshold — actions below are skipped (default: 0.5)
    #[serde(default = "default_confidence_threshold")]
    pub confidence_threshold: f64,
    /// Whether to stop on first action failure (default: true)
    #[serde(default = "default_stop_on_failure")]
    pub stop_on_failure: bool,
    /// Page URL for cache keying (when provided, enables plan caching)
    #[serde(default)]
    pub page_url: Option<String>,
    /// Element snapshot for cache fingerprinting (array of {id, type, role, label})
    #[serde(default)]
    pub element_snapshot: Option<serde_json::Value>,
}

fn default_confidence_threshold() -> f64 {
    0.5
}

fn default_stop_on_failure() -> bool {
    true
}

/// Result of a single planned action execution.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlannedActionResult {
    pub index: usize,
    pub success: bool,
    pub action: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_element_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default)]
    pub skipped_low_confidence: bool,
    pub duration_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub element_state: Option<serde_json::Value>,
}

/// Aggregated result of executing a full action plan.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionPlanResponse {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub goal: Option<String>,
    pub results: Vec<PlannedActionResult>,
    pub executed_count: usize,
    pub skipped_count: usize,
    pub failed_count: usize,
    pub total_duration_ms: u64,
    /// Whether this plan was stored in the cache for future reuse
    #[serde(default)]
    pub cached: bool,
}

/// Execute a structured action plan: an ordered sequence of typed UI actions.
///
/// Each action is resolved (element lookup) then executed via the standard
/// IPC path. This avoids a second LLM call for instruction interpretation —
/// the agentic-phase LLM directly specifies element targets and action types.
///
/// Element resolution priority:
///   1. element_id (direct registry lookup)
///   2. test_id (data-testid attribute)
///   3. selector (CSS selector)
///   4. search_text + element_type (fuzzy AI search)
pub async fn ui_bridge_execute_action_plan_handler(
    State(state): State<Arc<ApiState>>,
    Json(plan): Json<ActionPlanRequest>,
) -> Result<Json<ApiResponse<ActionPlanResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    let total_start = Instant::now();
    let action_count = plan.actions.len();

    if action_count == 0 {
        return Ok(Json(ApiResponse::success(ActionPlanResponse {
            success: true,
            goal: plan.goal,
            results: vec![],
            executed_count: 0,
            skipped_count: 0,
            failed_count: 0,
            total_duration_ms: 0,
            cached: false,
        })));
    }

    if action_count > 50 {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(api_error(format!(
                "Action plan has {} actions, maximum is 50",
                action_count
            ))),
        ));
    }

    info!(
        "UI Bridge API: Executing action plan with {} actions (goal: {:?})",
        action_count, plan.goal
    );

    let mut results = Vec::with_capacity(action_count);
    let mut executed_count = 0usize;
    let mut skipped_count = 0usize;
    let mut failed_count = 0usize;
    let mut all_success = true;

    for (i, planned) in plan.actions.iter().enumerate() {
        let action_start = Instant::now();

        // Skip low-confidence actions
        if planned.confidence < plan.confidence_threshold {
            info!(
                "UI Bridge action plan: Skipping action {} ({}) — confidence {:.2} < threshold {:.2}",
                i, planned.action, planned.confidence, plan.confidence_threshold
            );
            skipped_count += 1;
            results.push(PlannedActionResult {
                index: i,
                success: true,
                action: planned.action.clone(),
                resolved_element_id: None,
                error: None,
                skipped_low_confidence: true,
                duration_ms: 0,
                element_state: None,
            });
            continue;
        }

        // Handle non-element actions (navigate, wait)
        if planned.action == "navigate" {
            let url = planned
                .params
                .as_ref()
                .and_then(|p| p.get("url"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let result =
                ui_bridge_request_sync(&state, "navigate", serde_json::json!({ "url": url })).await;
            let duration = action_start.elapsed().as_millis() as u64;
            let (success, error) = match result {
                Ok(_) => (true, None),
                Err(e) => (false, Some(e)),
            };
            if !success {
                all_success = false;
                failed_count += 1;
            } else {
                executed_count += 1;
            }
            results.push(PlannedActionResult {
                index: i,
                success,
                action: "navigate".into(),
                resolved_element_id: None,
                error,
                skipped_low_confidence: false,
                duration_ms: duration,
                element_state: None,
            });
            if !success && plan.stop_on_failure {
                break;
            }
            continue;
        }

        if planned.action == "wait" {
            let ms = planned
                .params
                .as_ref()
                .and_then(|p| p.get("ms"))
                .and_then(|v| v.as_u64())
                .unwrap_or(1000)
                .min(30000); // Cap at 30s
            tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
            executed_count += 1;
            results.push(PlannedActionResult {
                index: i,
                success: true,
                action: "wait".into(),
                resolved_element_id: None,
                error: None,
                skipped_low_confidence: false,
                duration_ms: ms,
                element_state: None,
            });
            continue;
        }

        // Resolve element ID from target specification
        let resolved_id = resolve_action_plan_target(&state, &planned.target).await;

        let resolved_id = match resolved_id {
            Ok(id) => id,
            Err(e) => {
                let duration = action_start.elapsed().as_millis() as u64;
                all_success = false;
                failed_count += 1;
                results.push(PlannedActionResult {
                    index: i,
                    success: false,
                    action: planned.action.clone(),
                    resolved_element_id: None,
                    error: Some(format!("Element resolution failed: {}", e)),
                    skipped_low_confidence: false,
                    duration_ms: duration,
                    element_state: None,
                });
                if plan.stop_on_failure {
                    break;
                }
                continue;
            }
        };

        // Execute the action via standard IPC
        let action_payload = serde_json::json!({
            "elementId": resolved_id,
            "action": {
                "action": planned.action,
                "params": planned.params,
            }
        });

        let result = ui_bridge_request_sync(&state, "execute_action", action_payload).await;
        let duration = action_start.elapsed().as_millis() as u64;

        match result {
            Ok(data) => {
                let action_success = data
                    .get("success")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true);
                let element_state = data.get("elementState").cloned();
                let action_error = if action_success {
                    None
                } else {
                    data.get("error")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                };

                if !action_success {
                    all_success = false;
                    failed_count += 1;
                } else {
                    executed_count += 1;
                }

                results.push(PlannedActionResult {
                    index: i,
                    success: action_success,
                    action: planned.action.clone(),
                    resolved_element_id: Some(resolved_id.clone()),
                    error: action_error,
                    skipped_low_confidence: false,
                    duration_ms: duration,
                    element_state,
                });

                if !action_success && plan.stop_on_failure {
                    break;
                }
            }
            Err(e) => {
                all_success = false;
                failed_count += 1;
                results.push(PlannedActionResult {
                    index: i,
                    success: false,
                    action: planned.action.clone(),
                    resolved_element_id: Some(resolved_id.clone()),
                    error: Some(e),
                    skipped_low_confidence: false,
                    duration_ms: duration,
                    element_state: None,
                });
                if plan.stop_on_failure {
                    break;
                }
            }
        }
    }

    let total_duration = total_start.elapsed().as_millis() as u64;
    info!(
        "UI Bridge action plan complete: {}/{} succeeded, {} skipped, {} failed ({}ms)",
        executed_count, action_count, skipped_count, failed_count, total_duration
    );

    // Cache successful plans for future reuse
    let mut was_cached = false;
    if all_success {
        if let (Some(ref url), Some(ref snapshot)) = (&plan.page_url, &plan.element_snapshot) {
            if let Some((norm_url, fingerprint)) =
                super::action_plan_cache::ActionPlanCache::build_key(url, snapshot)
            {
                let plan_json = serde_json::to_value(&plan.actions).unwrap_or_default();
                super::action_plan_cache::global_action_plan_cache().put(
                    &norm_url,
                    &fingerprint,
                    plan_json,
                    plan.goal.clone(),
                );
                was_cached = true;
            }
        }
    } else if let (Some(ref url), Some(ref snapshot)) = (&plan.page_url, &plan.element_snapshot) {
        // Mark failed plans so they're not reused
        if let Some((norm_url, fingerprint)) =
            super::action_plan_cache::ActionPlanCache::build_key(url, snapshot)
        {
            super::action_plan_cache::global_action_plan_cache()
                .mark_failed(&norm_url, &fingerprint);
        }
    }

    Ok(Json(ApiResponse::success(ActionPlanResponse {
        success: all_success,
        goal: plan.goal,
        results,
        executed_count,
        skipped_count,
        failed_count,
        total_duration_ms: total_duration,
        cached: was_cached,
    })))
}

/// Resolve an element target to a concrete element ID.
///
/// Tries resolution strategies in order of specificity:
/// 1. Direct element_id
/// 2. test_id → find by data-testid
/// 3. CSS selector → find by selector
/// 4. search_text → fuzzy AI search
async fn resolve_action_plan_target(
    state: &Arc<ApiState>,
    target: &ActionPlanElementTarget,
) -> Result<String, String> {
    // 1. Direct element ID
    if let Some(ref id) = target.element_id {
        return Ok(id.clone());
    }

    // 2. Find by data-testid attribute
    if let Some(ref test_id) = target.test_id {
        let find_payload = serde_json::json!({
            "testId": test_id
        });
        if let Ok(data) = ui_bridge_request_sync(state, "find", find_payload).await {
            if let Some(id) = extract_first_element_id(&data) {
                return Ok(id);
            }
        }
    }

    // 3. Find by CSS selector
    if let Some(ref selector) = target.selector {
        let find_payload = serde_json::json!({
            "selector": selector
        });
        if let Ok(data) = ui_bridge_request_sync(state, "find", find_payload).await {
            if let Some(id) = extract_first_element_id(&data) {
                return Ok(id);
            }
        }
    }

    // 4. Fuzzy search by text + element type
    if let Some(ref text) = target.search_text {
        let mut find_payload = serde_json::json!({
            "text": text,
            "fuzzy": true
        });
        if let Some(ref el_type) = target.element_type {
            find_payload["element_type"] = serde_json::json!(el_type);
        }
        if let Ok(data) = ui_bridge_request_sync(state, "find", find_payload).await {
            if let Some(id) = extract_first_element_id(&data) {
                return Ok(id);
            }
        }
    }

    Err("No element target specified or element not found".to_string())
}

/// Extract the first element ID from a find/search result.
///
/// Handles multiple response formats:
///   - `{ results: [{ elementId, ... }] }` (ai_search)
///   - `{ elements: [{ id, ... }] }` (find)
///   - `{ id, ... }` (direct element)
///   - `[{ id, ... }]` (array of elements)
fn extract_first_element_id(data: &serde_json::Value) -> Option<String> {
    // find returns { elements: [{ id, ... }] }
    if let Some(elements) = data.get("elements").and_then(|v| v.as_array()) {
        if let Some(first) = elements.first() {
            if let Some(id) = first.get("id").and_then(|v| v.as_str()) {
                return Some(id.to_string());
            }
        }
    }
    // ai_search returns { results: [{ elementId, ... }] }
    if let Some(results) = data.get("results").and_then(|v| v.as_array()) {
        if let Some(first) = results.first() {
            if let Some(id) = first
                .get("elementId")
                .or_else(|| first.get("id"))
                .and_then(|v| v.as_str())
            {
                return Some(id.to_string());
            }
        }
    }
    // Direct element response
    if let Some(id) = data.get("id").and_then(|v| v.as_str()) {
        return Some(id.to_string());
    }
    // Array of elements
    if let Some(arr) = data.as_array() {
        if let Some(first) = arr.first() {
            if let Some(id) = first.get("id").and_then(|v| v.as_str()) {
                return Some(id.to_string());
            }
        }
    }
    None
}

// ============================================================================
// Action Plan Cache Endpoints
// ============================================================================

/// Query params for action plan cache lookup.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionPlanCacheLookupQuery {
    /// Page URL to look up
    pub url: String,
    /// Element snapshot JSON (array of {id, type, role, label})
    pub elements: String,
}

/// Look up a cached action plan by page URL and element fingerprint.
async fn ui_bridge_action_plan_cache_lookup_handler(
    Query(query): Query<ActionPlanCacheLookupQuery>,
) -> Json<ApiResponse<serde_json::Value>> {
    let elements: serde_json::Value = match serde_json::from_str(&query.elements) {
        Ok(v) => v,
        Err(_) => {
            return Json(ApiResponse::error("Invalid elements JSON".to_string()));
        }
    };

    if let Some((norm_url, fingerprint)) =
        super::action_plan_cache::ActionPlanCache::build_key(&query.url, &elements)
    {
        if let Some(cached) =
            super::action_plan_cache::global_action_plan_cache().get(&norm_url, &fingerprint)
        {
            return Json(ApiResponse::success(serde_json::json!({
                "hit": true,
                "plan": cached.plan,
                "goal": cached.goal,
                "hitCount": cached.hit_count,
                "lastSuccess": cached.last_success,
            })));
        }
    }

    Json(ApiResponse::success(serde_json::json!({ "hit": false })))
}

/// Get action plan cache statistics.
async fn ui_bridge_action_plan_cache_stats_handler() -> Json<ApiResponse<serde_json::Value>> {
    let stats = super::action_plan_cache::global_action_plan_cache().stats();
    Json(ApiResponse::success(stats))
}

// ============================================================================
// Batch Execution
// ============================================================================

/// Request to execute multiple UI Bridge operations in a single HTTP call.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchRequest {
    pub operations: Vec<BatchOperation>,
    /// If true, stop executing on first error. Default: false (execute all).
    #[serde(default)]
    pub stop_on_error: bool,
}

/// A single operation within a batch request.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchOperation {
    /// Unique ID for this operation within the batch (for result correlation).
    pub id: String,
    /// The operation type (e.g., "get_elements", "execute_action", "discover").
    pub operation: String,
    /// Operation-specific parameters (merged into the IPC payload).
    #[serde(default)]
    pub params: serde_json::Value,
}

/// Response from a batch execution.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchResponse {
    /// True only if all operations succeeded.
    pub success: bool,
    /// Per-operation results in request order.
    pub results: Vec<BatchOperationResult>,
    /// Total wall-clock time for the entire batch.
    pub total_duration_ms: u64,
}

/// Result of a single operation within a batch.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchOperationResult {
    pub id: String,
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_detail: Option<UiBridgeError>,
    pub duration_ms: u64,
}

/// Execute multiple UI Bridge operations in a single HTTP call.
///
/// Each operation is executed sequentially via the existing `ui_bridge_request_sync`
/// path, reusing all existing concurrency, circuit breaker, and timeout logic.
/// Max 20 operations per batch.
async fn ui_bridge_batch_handler(
    State(state): State<Arc<ApiState>>,
    Json(batch): Json<BatchRequest>,
) -> Result<Json<ApiResponse<BatchResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    const MAX_BATCH_SIZE: usize = 20;

    if batch.operations.len() > MAX_BATCH_SIZE {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(api_error(format!(
                "Batch size {} exceeds maximum of {}",
                batch.operations.len(),
                MAX_BATCH_SIZE
            ))),
        ));
    }

    if batch.operations.is_empty() {
        return Ok(Json(ApiResponse::success(BatchResponse {
            success: true,
            results: vec![],
            total_duration_ms: 0,
        })));
    }

    info!(
        "UI Bridge API: Batch executing {} operations (stop_on_error={})",
        batch.operations.len(),
        batch.stop_on_error
    );

    let start = Instant::now();
    let mut results = Vec::with_capacity(batch.operations.len());

    for op in &batch.operations {
        let op_start = Instant::now();
        let result = ui_bridge_request_sync(&state, &op.operation, op.params.clone()).await;

        let (success, data, error, error_detail) = match result {
            Ok(data) => {
                // Check for nested frontend failure (same logic as wrap_ipc_result)
                if data.get("success").and_then(|v| v.as_bool()) == Some(false) {
                    let error_msg = data
                        .get("error")
                        .and_then(|v| v.as_str())
                        .unwrap_or("Operation failed")
                        .to_string();
                    let detail = classify_transport_error(&error_msg);
                    (false, Some(data), Some(error_msg), Some(detail))
                } else {
                    (true, Some(data), None, None)
                }
            }
            Err(e) => {
                let detail = classify_transport_error(&e);
                (false, None, Some(e), Some(detail))
            }
        };

        let op_duration = op_start.elapsed().as_millis() as u64;

        results.push(BatchOperationResult {
            id: op.id.clone(),
            success,
            data,
            error,
            error_detail,
            duration_ms: op_duration,
        });

        if !success && batch.stop_on_error {
            info!(
                "UI Bridge batch: stopping on error at operation '{}' ({}ms)",
                op.id, op_duration
            );
            break;
        }
    }

    let total_duration = start.elapsed().as_millis() as u64;
    let all_success = results.iter().all(|r| r.success);

    info!(
        "UI Bridge batch: {}/{} succeeded in {}ms",
        results.iter().filter(|r| r.success).count(),
        results.len(),
        total_duration
    );

    let response = BatchResponse {
        success: all_success,
        results,
        total_duration_ms: total_duration,
    };

    Ok(Json(ApiResponse::success(response)))
}

// ============================================================================
// Analytics endpoints (selector performance, cross-run quality)
// ============================================================================

#[derive(Debug, Deserialize)]
pub struct AnalyticsDaysQuery {
    #[serde(default = "default_analytics_days")]
    pub days: u32,
    #[serde(default = "default_analytics_limit")]
    pub limit: i64,
}
fn default_analytics_days() -> u32 {
    7
}
fn default_analytics_limit() -> i64 {
    20
}

#[derive(Debug, Deserialize)]
pub struct DecayCurveQuery {
    pub element_id: String,
    #[serde(default = "default_window_ms")]
    pub window_ms: i64,
    #[serde(default = "default_num_windows")]
    pub windows: i64,
}
fn default_window_ms() -> i64 {
    86_400_000
} // 1 day
fn default_num_windows() -> i64 {
    7
}

fn days_to_epoch_ms(days: u32) -> i64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64;
    now - (days as i64 * 86_400_000)
}

fn days_to_sqlite_datetime(days: u32) -> String {
    format!("-{} days", days)
}

/// GET /ui-bridge/analytics/decay-curve
pub async fn analytics_decay_curve_handler(
    State(state): State<Arc<ApiState>>,
    Query(q): Query<DecayCurveQuery>,
) -> Result<
    Json<ApiResponse<Vec<crate::database::ui_bridge_ops::DecayCurveBucket>>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    match state
        .app_state
        .pg_db
        .get_element_decay_curve(&q.element_id, q.window_ms, q.windows)
        .await
    {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// GET /ui-bridge/analytics/action-baselines
pub async fn analytics_action_baselines_handler(
    State(state): State<Arc<ApiState>>,
    Query(q): Query<AnalyticsDaysQuery>,
) -> Result<
    Json<ApiResponse<Vec<crate::database::ui_bridge_ops::ActionBaseline>>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    let since = days_to_epoch_ms(q.days);
    match state
        .app_state
        .pg_db
        .get_action_latency_baselines(since)
        .await
    {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// GET /ui-bridge/analytics/failure-taxonomy
pub async fn analytics_failure_taxonomy_handler(
    State(state): State<Arc<ApiState>>,
    Query(q): Query<AnalyticsDaysQuery>,
) -> Result<
    Json<ApiResponse<Vec<crate::database::ui_bridge_ops::FailureCluster>>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    let since = days_to_epoch_ms(q.days);
    match state
        .app_state
        .pg_db
        .get_failure_taxonomy(since, q.limit)
        .await
    {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// GET /ui-bridge/analytics/fragility-heatmap
pub async fn analytics_fragility_heatmap_handler(
    State(state): State<Arc<ApiState>>,
    Query(q): Query<AnalyticsDaysQuery>,
) -> Result<
    Json<ApiResponse<Vec<crate::database::ui_bridge_ops::ElementFragility>>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    let since = days_to_epoch_ms(q.days);
    match state
        .app_state
        .pg_db
        .get_element_fragility_by_region(since)
        .await
    {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// GET /ui-bridge/analytics/regressions
pub async fn analytics_regressions_handler(
    State(state): State<Arc<ApiState>>,
    Query(q): Query<AnalyticsDaysQuery>,
) -> Result<
    Json<ApiResponse<Vec<crate::database::ui_bridge_ops::AutomationRegression>>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    let since = days_to_epoch_ms(q.days);
    match state
        .app_state
        .pg_db
        .get_automation_regressions(since, q.limit)
        .await
    {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// GET /ui-bridge/analytics/stall-frequency
pub async fn analytics_stall_frequency_handler(
    State(state): State<Arc<ApiState>>,
    Query(q): Query<AnalyticsDaysQuery>,
) -> Result<
    Json<ApiResponse<Vec<crate::database::ui_bridge_ops::StallFrequency>>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    let since = (chrono::Utc::now() - chrono::Duration::days(q.days as i64)).to_rfc3339();
    match state.app_state.pg_db.get_stall_frequency(&since).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// GET /ui-bridge/analytics/intervention-effectiveness
pub async fn analytics_intervention_handler(
    State(state): State<Arc<ApiState>>,
    Query(q): Query<AnalyticsDaysQuery>,
) -> Result<
    Json<ApiResponse<Vec<crate::database::ui_bridge_ops::InterventionStats>>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    let since = (chrono::Utc::now() - chrono::Duration::days(q.days as i64)).to_rfc3339();
    match state
        .app_state
        .pg_db
        .get_intervention_effectiveness(&since)
        .await
    {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

#[derive(Debug, Deserialize)]
pub struct StateCoverageQuery {
    pub task_run_id: i64,
}

/// GET /ui-bridge/analytics/state-coverage
pub async fn analytics_state_coverage_handler(
    State(state): State<Arc<ApiState>>,
    Query(q): Query<StateCoverageQuery>,
) -> Result<Json<ApiResponse<Vec<String>>>, (StatusCode, Json<ApiResponse<()>>)> {
    match state
        .app_state
        .pg_db
        .get_state_coverage(q.task_run_id)
        .await
    {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

#[derive(Debug, Deserialize)]
pub struct AnnotationGapQuery {
    #[serde(default = "default_annotation_min")]
    pub min_interactions: i64,
}
fn default_annotation_min() -> i64 {
    10
}

/// GET /ui-bridge/analytics/annotation-gaps
pub async fn analytics_annotation_gaps_handler(
    State(state): State<Arc<ApiState>>,
    Query(q): Query<AnnotationGapQuery>,
) -> Result<
    Json<ApiResponse<Vec<crate::database::ui_bridge_ops::AnnotationGap>>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    match state
        .app_state
        .pg_db
        .get_unannotated_high_interaction_elements(q.min_interactions)
        .await
    {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// GET /ui-bridge/analytics/health-score
pub async fn analytics_health_score_handler(
    State(state): State<Arc<ApiState>>,
    Query(q): Query<AnalyticsDaysQuery>,
) -> Result<
    Json<ApiResponse<crate::database::ui_bridge_ops::AutomationHealthScore>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    let since = days_to_epoch_ms(q.days);
    let data = state
        .app_state
        .pg_db
        .compute_automation_health_score(since)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))?;
    // Deserialize from serde_json::Value to the expected type
    let typed: crate::database::ui_bridge_ops::AutomationHealthScore = serde_json::from_value(data)
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Deserialization error: {}", e))),
            )
        })?;
    Ok(Json(ApiResponse::success(typed)))
}

/// GET /ui-bridge/analytics/recommendations
pub async fn analytics_recommendations_handler(
    State(state): State<Arc<ApiState>>,
    Query(q): Query<AnalyticsDaysQuery>,
) -> Result<
    Json<ApiResponse<Vec<crate::database::ui_bridge_ops::Recommendation>>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    let since = days_to_epoch_ms(q.days);
    let data = state
        .app_state
        .pg_db
        .generate_recommendations(since)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))?;
    // Deserialize from serde_json::Value to the expected type
    let typed: Vec<crate::database::ui_bridge_ops::Recommendation> = data
        .into_iter()
        .filter_map(|v| serde_json::from_value(v).ok())
        .collect();
    Ok(Json(ApiResponse::success(typed)))
}

// Create routes for this module.

// ============================================================================
// Health signals endpoint (combined idle + stuck screen diagnosis)
// ============================================================================

/// Combined UI Bridge health signals for stall detection integration.
#[derive(Debug, Serialize)]
pub struct UiBridgeHealthSignals {
    pub idle: serde_json::Value,
    pub stuck_screen: serde_json::Value,
}

/// Get combined health signals from the UI Bridge SDK.
///
/// Combines idle status and stuck screen diagnosis into a single response
/// for use by the stall detection system.
pub async fn ui_bridge_health_signals_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<UiBridgeHealthSignals>>, (StatusCode, Json<ApiResponse<()>>)> {
    // Fetch idle status and stuck screen diagnosis in parallel
    let idle_future = ui_bridge_request_sync(&state, "get_idle_status", serde_json::json!({}));
    let stuck_future = ui_bridge_request_sync(
        &state,
        "diagnose_stuck_screen",
        serde_json::json!({"observationWindowMs": 2000}),
    );

    let (idle_result, stuck_result) = tokio::join!(idle_future, stuck_future);

    let idle = idle_result.unwrap_or_else(|e| {
        warn!("Failed to get idle status: {}", e);
        serde_json::json!({"error": e})
    });

    let stuck_screen = stuck_result.unwrap_or_else(|e| {
        warn!("Failed to diagnose stuck screen: {}", e);
        serde_json::json!({"error": e})
    });

    Ok(Json(ApiResponse::success(UiBridgeHealthSignals {
        idle,
        stuck_screen,
    })))
}

// ============================================================================
// Element interaction history endpoints (persisted cross-run data)
// ============================================================================

/// Query parameters for element interaction history.
#[derive(Debug, Deserialize)]
pub struct HistoryElementsQuery {
    pub task_run_id: i64,
}

/// Get all UI Bridge events for a specific task run.
pub async fn ui_bridge_history_elements_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<HistoryElementsQuery>,
) -> Result<
    Json<ApiResponse<Vec<crate::database::ui_bridge_ops::UiBridgeEvent>>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    match state
        .app_state
        .pg_db
        .get_element_interactions(query.task_run_id)
        .await
    {
        Ok(events) => Ok(Json(ApiResponse::success(events))),
        Err(e) => {
            error!("UI Bridge history: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Query parameters for single element history.
#[derive(Debug, Deserialize)]
pub struct HistoryElementQuery {
    #[serde(default = "default_history_limit")]
    pub limit: i64,
}

fn default_history_limit() -> i64 {
    50
}

/// Get cross-run interaction history for a single element.
pub async fn ui_bridge_history_element_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Query(query): Query<HistoryElementQuery>,
) -> Result<
    Json<ApiResponse<Vec<crate::database::ui_bridge_ops::UiBridgeEvent>>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    match state
        .app_state
        .pg_db
        .get_element_history(&id, query.limit)
        .await
    {
        Ok(events) => Ok(Json(ApiResponse::success(events))),
        Err(e) => {
            error!("UI Bridge element history: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Query parameters for flaky element detection.
#[derive(Debug, Deserialize)]
pub struct FlakyElementsQuery {
    #[serde(default = "default_min_interactions")]
    pub min_interactions: i64,
    #[serde(default = "default_max_success_rate")]
    pub max_success_rate: f64,
}

fn default_min_interactions() -> i64 {
    10
}

fn default_max_success_rate() -> f64 {
    0.8
}

/// Get elements with high failure rates across runs.
pub async fn ui_bridge_history_flaky_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<FlakyElementsQuery>,
) -> Result<
    Json<ApiResponse<Vec<crate::database::ui_bridge_ops::ElementReliability>>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    match state
        .app_state
        .pg_db
        .get_flaky_elements(query.min_interactions, query.max_success_rate)
        .await
    {
        Ok(elements) => Ok(Json(ApiResponse::success(elements))),
        Err(e) => {
            error!("UI Bridge flaky elements: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Query parameters for element reliability.
#[derive(Debug, Deserialize)]
pub struct ElementReliabilityQuery {
    pub element_id: String,
}

/// Get reliability data for a single element across all runs.
pub async fn ui_bridge_element_reliability_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<ElementReliabilityQuery>,
) -> Result<
    Json<ApiResponse<Option<crate::database::ui_bridge_ops::ElementReliability>>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    match state
        .app_state
        .pg_db
        .get_element_reliability(&query.element_id)
        .await
    {
        Ok(reliability) => Ok(Json(ApiResponse::success(reliability))),
        Err(e) => {
            error!("UI Bridge element reliability: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

// =========================================================================
// Convenience endpoints — app-agnostic DOM interaction helpers
// =========================================================================

/// Evaluate a JS expression, trying IPC first then direct WebView eval.
/// Returns the raw string result from the evaluation.
async fn evaluate_js_expression(state: &Arc<ApiState>, expression: &str) -> Result<String, String> {
    let payload = serde_json::json!({ "expression": expression });

    // Try IPC path first (uses SDK event handlers, fastest)
    match ui_bridge_request_sync(state, "page_evaluate", payload).await {
        Ok(data) => {
            // Check for inner error (e.g., "Expression rejected: contains prohibited pattern")
            if data.get("success") == Some(&serde_json::Value::Bool(false))
                || data.get("error").is_some()
            {
                // IPC returned an error — fall back to direct eval
                return direct_webview_evaluate_with_result(state, expression).await;
            }
            // Extract the result value from the IPC response
            if let Some(result) = data.get("result").and_then(|r| r.get("value")) {
                match result {
                    serde_json::Value::String(s) => Ok(s.clone()),
                    other => Ok(other.to_string()),
                }
            } else {
                Ok(data.to_string())
            }
        }
        Err(_ipc_err) => {
            // Fallback to direct WebView evaluation
            direct_webview_evaluate_with_result(state, expression).await
        }
    }
}

/// Find elements by visible text content.
/// POST /ui-bridge/control/page/find-by-text
/// Body: { "text": "Submit", "tag": "button", "exact": true }
pub async fn ui_bridge_find_by_text_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let text = body.get("text").and_then(|v| v.as_str()).unwrap_or("");
    let tag = body.get("tag").and_then(|v| v.as_str()).unwrap_or("*");
    let exact = body.get("exact").and_then(|v| v.as_bool()).unwrap_or(false);

    if text.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(api_error("'text' field is required")),
        ));
    }

    let escaped_text = text.replace('\\', "\\\\").replace('\'', "\\'");
    let match_expr = if exact {
        format!("el.textContent.trim() === '{}'", escaped_text)
    } else {
        format!(
            "el.textContent.trim().toLowerCase().includes('{}')",
            escaped_text.to_lowercase()
        )
    };

    let js = format!(
        r#"JSON.stringify(Array.from(document.querySelectorAll('{tag}'))
            .filter(el => el.offsetParent !== null && {match_expr})
            .map((el, i) => ({{
                index: i,
                tag: el.tagName,
                text: el.textContent.trim().slice(0, 100),
                id: el.id || null,
                disabled: el.disabled || false,
                visible: el.offsetParent !== null,
                rect: (() => {{ const r = el.getBoundingClientRect(); return {{ x: Math.round(r.x), y: Math.round(r.y), width: Math.round(r.width), height: Math.round(r.height) }}; }})()
            }})))"#,
        tag = tag,
        match_expr = match_expr
    );

    match evaluate_js_expression(&state, &js).await {
        Ok(result) => {
            let parsed: serde_json::Value =
                serde_json::from_str(&result).unwrap_or(serde_json::Value::Array(vec![]));
            Ok(Json(ApiResponse::success(serde_json::json!({
                "matches": parsed,
                "count": parsed.as_array().map(|a| a.len()).unwrap_or(0),
                "query": { "text": text, "tag": tag, "exact": exact }
            }))))
        }
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// Click an element by its visible text content.
/// POST /ui-bridge/control/page/click-by-text
/// Body: { "text": "Submit", "tag": "button", "exact": true, "index": 0 }
pub async fn ui_bridge_click_by_text_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let text = body.get("text").and_then(|v| v.as_str()).unwrap_or("");
    let tag = body.get("tag").and_then(|v| v.as_str()).unwrap_or("*");
    let exact = body.get("exact").and_then(|v| v.as_bool()).unwrap_or(false);
    let index = body.get("index").and_then(|v| v.as_u64()).unwrap_or(0);

    if text.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(api_error("'text' field is required")),
        ));
    }

    let escaped_text = text.replace('\\', "\\\\").replace('\'', "\\'");
    let match_expr = if exact {
        format!("el.textContent.trim() === '{}'", escaped_text)
    } else {
        format!(
            "el.textContent.trim().toLowerCase().includes('{}')",
            escaped_text.to_lowercase()
        )
    };

    let js = format!(
        r#"(() => {{
            const matches = Array.from(document.querySelectorAll('{tag}'))
                .filter(el => el.offsetParent !== null && {match_expr});
            if (matches.length === 0) return JSON.stringify({{ clicked: false, error: 'No matching elements found' }});
            const idx = {index};
            if (idx >= matches.length) return JSON.stringify({{ clicked: false, error: 'Index ' + idx + ' out of range (found ' + matches.length + ')' }});
            const el = matches[idx];
            el.scrollIntoView({{ block: 'center' }});
            el.click();
            return JSON.stringify({{
                clicked: true,
                tag: el.tagName,
                text: el.textContent.trim().slice(0, 100),
                index: idx,
                totalMatches: matches.length
            }});
        }})()"#,
        tag = tag,
        match_expr = match_expr,
        index = index
    );

    match evaluate_js_expression(&state, &js).await {
        Ok(result) => {
            let parsed: serde_json::Value = serde_json::from_str(&result)
                .unwrap_or(serde_json::json!({"clicked": false, "error": "Parse error"}));
            Ok(Json(ApiResponse::success(parsed)))
        }
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// Click an element by CSS selector.
/// POST /ui-bridge/control/page/click-by-selector
/// Body: { "selector": "button[type='submit']", "index": 0 }
pub async fn ui_bridge_click_by_selector_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let selector = body.get("selector").and_then(|v| v.as_str()).unwrap_or("");
    let index = body.get("index").and_then(|v| v.as_u64()).unwrap_or(0);

    if selector.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(api_error("'selector' field is required")),
        ));
    }

    let escaped_selector = selector.replace('\\', "\\\\").replace('\'', "\\'");

    let js = format!(
        r#"(() => {{
            const matches = Array.from(document.querySelectorAll('{selector}'))
                .filter(el => el.offsetParent !== null);
            if (matches.length === 0) return JSON.stringify({{ clicked: false, error: 'No elements match selector' }});
            const idx = {index};
            if (idx >= matches.length) return JSON.stringify({{ clicked: false, error: 'Index ' + idx + ' out of range (found ' + matches.length + ')' }});
            const el = matches[idx];
            el.scrollIntoView({{ block: 'center' }});
            el.click();
            return JSON.stringify({{
                clicked: true,
                tag: el.tagName,
                text: el.textContent.trim().slice(0, 100),
                index: idx,
                totalMatches: matches.length
            }});
        }})()"#,
        selector = escaped_selector,
        index = index
    );

    match evaluate_js_expression(&state, &js).await {
        Ok(result) => {
            let parsed: serde_json::Value = serde_json::from_str(&result)
                .unwrap_or(serde_json::json!({"clicked": false, "error": "Parse error"}));
            Ok(Json(ApiResponse::success(parsed)))
        }
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// Read the value of a form element by CSS selector.
/// POST /ui-bridge/control/page/read-value
/// Body: { "selector": "textarea", "index": 0 }
pub async fn ui_bridge_read_value_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let selector = body.get("selector").and_then(|v| v.as_str()).unwrap_or("");
    let index = body.get("index").and_then(|v| v.as_u64()).unwrap_or(0);

    if selector.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(api_error("'selector' field is required")),
        ));
    }

    let escaped_selector = selector.replace('\\', "\\\\").replace('\'', "\\'");

    let js = format!(
        r#"(() => {{
            const matches = Array.from(document.querySelectorAll('{selector}'));
            if (matches.length === 0) return JSON.stringify({{ found: false, error: 'No elements match selector' }});
            const idx = {index};
            if (idx >= matches.length) return JSON.stringify({{ found: false, error: 'Index out of range' }});
            const el = matches[idx];
            return JSON.stringify({{
                found: true,
                tag: el.tagName,
                type: el.type || null,
                value: el.value || el.textContent?.trim() || '',
                length: (el.value || el.textContent || '').length,
                placeholder: el.placeholder || null,
                disabled: el.disabled || false,
                readOnly: el.readOnly || false,
                index: idx,
                totalMatches: matches.length
            }});
        }})()"#,
        selector = escaped_selector,
        index = index
    );

    match evaluate_js_expression(&state, &js).await {
        Ok(result) => {
            let parsed: serde_json::Value = serde_json::from_str(&result)
                .unwrap_or(serde_json::json!({"found": false, "error": "Parse error"}));
            Ok(Json(ApiResponse::success(parsed)))
        }
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// Type text into an element by CSS selector or label.
/// POST /ui-bridge/control/page/type-into
/// Body: { "selector": "textarea", "text": "hello", "clear": true, "index": 0 }
/// Or:   { "label": "Email", "text": "user@example.com" }
pub async fn ui_bridge_type_into_handler(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let selector = body.get("selector").and_then(|v| v.as_str());
    let label = body.get("label").and_then(|v| v.as_str());
    let text = body.get("text").and_then(|v| v.as_str()).unwrap_or("");
    let clear = body.get("clear").and_then(|v| v.as_bool()).unwrap_or(false);
    let index = body.get("index").and_then(|v| v.as_u64()).unwrap_or(0);

    let find_expr = if let Some(sel) = selector {
        let escaped = sel.replace('\\', "\\\\").replace('\'', "\\'");
        format!("document.querySelectorAll('{}')", escaped)
    } else if let Some(lbl) = label {
        let escaped = lbl.replace('\\', "\\\\").replace('\'', "\\'");
        format!(
            "(() => {{ const lb = Array.from(document.querySelectorAll('label')).find(l => l.textContent.trim().toLowerCase().includes('{}')); return lb && lb.htmlFor ? [document.getElementById(lb.htmlFor)] : []; }})()",
            escaped.to_lowercase()
        )
    } else {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(api_error("'selector' or 'label' field is required")),
        ));
    };

    let escaped_text = text.replace('\\', "\\\\").replace('\'', "\\'");

    let js = format!(
        r#"(() => {{
            const matches = Array.from({find_expr}).filter(el => el);
            if (matches.length === 0) return JSON.stringify({{ typed: false, error: 'No elements found' }});
            const idx = {index};
            if (idx >= matches.length) return JSON.stringify({{ typed: false, error: 'Index out of range' }});
            const el = matches[idx];
            el.scrollIntoView({{ block: 'center' }});
            el.focus();
            if ({clear}) {{
                el.value = '';
                el.dispatchEvent(new Event('input', {{ bubbles: true }}));
            }}
            const nativeInputValueSetter = Object.getOwnPropertyDescriptor(window.HTMLTextAreaElement.prototype, 'value')?.set
                || Object.getOwnPropertyDescriptor(window.HTMLInputElement.prototype, 'value')?.set;
            if (nativeInputValueSetter) {{
                nativeInputValueSetter.call(el, {clear} ? '{text}' : el.value + '{text}');
            }} else {{
                el.value = {clear} ? '{text}' : el.value + '{text}';
            }}
            el.dispatchEvent(new Event('input', {{ bubbles: true }}));
            el.dispatchEvent(new Event('change', {{ bubbles: true }}));
            return JSON.stringify({{
                typed: true,
                tag: el.tagName,
                valueLength: el.value.length,
                index: idx
            }});
        }})()"#,
        find_expr = find_expr,
        index = index,
        clear = clear,
        text = escaped_text
    );

    match evaluate_js_expression(&state, &js).await {
        Ok(result) => {
            let parsed: serde_json::Value = serde_json::from_str(&result)
                .unwrap_or(serde_json::json!({"typed": false, "error": "Parse error"}));
            Ok(Json(ApiResponse::success(parsed)))
        }
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// Get a summary of the current page state.
/// POST /ui-bridge/control/page/summary
pub async fn ui_bridge_page_summary_handler(
    State(state): State<Arc<ApiState>>,
    Json(_body): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let js = r#"(() => {
        var vis = function(sel) { return Array.from(document.querySelectorAll(sel)).filter(function(el) { return el.offsetParent !== null; }); };
        return JSON.stringify({
            title: document.title,
            url: document.URL,
            headings: vis('h1, h2, h3').map(function(el) { return { tag: el.tagName, text: el.textContent.trim().slice(0, 100) }; }).slice(0, 10),
            buttons: vis('button').filter(function(el) { return el.textContent.trim().length > 0; }).map(function(el) { return el.textContent.trim().slice(0, 40); }).filter(function(t) { return t.length < 30; }).slice(0, 20),
            inputs: vis('input, textarea, select').map(function(el) { return { tag: el.tagName, type: el.type || null, placeholder: (el.placeholder || '').slice(0, 40) || null, hasValue: (el.value || '').length > 0 }; }).slice(0, 15),
            links: vis('a[href]').filter(function(el) { return el.textContent.trim().length > 0; }).map(function(el) { return { text: el.textContent.trim().slice(0, 40), href: (el.getAttribute('href') || '').slice(0, 60) }; }).slice(0, 15),
            modals: vis('[role="dialog"]').map(function(el) { return el.textContent.trim().slice(0, 100); }).slice(0, 3),
            errors: vis('[role="alert"]').filter(function(el) { return el.textContent.trim().length > 0; }).map(function(el) { return el.textContent.trim().slice(0, 100); }).slice(0, 5),
            elementCounts: {
                buttons: document.querySelectorAll('button').length,
                inputs: document.querySelectorAll('input').length,
                textareas: document.querySelectorAll('textarea').length,
                selects: document.querySelectorAll('select').length,
                links: document.querySelectorAll('a').length,
                images: document.querySelectorAll('img').length
            }
        });
    })()"#;

    match evaluate_js_expression(&state, js).await {
        Ok(result) => {
            let parsed: serde_json::Value = serde_json::from_str(&result)
                .unwrap_or(serde_json::json!({"error": "Parse error"}));
            Ok(Json(ApiResponse::success(parsed)))
        }
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

pub fn routes() -> axum::Router<std::sync::Arc<crate::mcp::types::ApiState>> {
    use axum::routing::{get, post};
    axum::Router::new()
        .route(
            "/ui-bridge/control/elements",
            get(ui_bridge_get_elements_handler),
        )
        .route(
            "/ui-bridge/control/element/{id}",
            get(ui_bridge_get_element_handler),
        )
        .route(
            "/ui-bridge/control/element/{id}/action",
            post(ui_bridge_execute_action_handler),
        )
        .route(
            "/ui-bridge/control/components",
            get(ui_bridge_get_components_handler),
        )
        .route(
            "/ui-bridge/control/component/{id}",
            get(ui_bridge_get_component_handler),
        )
        .route(
            "/ui-bridge/control/component/{id}/action/{action_id}",
            post(ui_bridge_execute_component_action_handler),
        )
        .route(
            "/ui-bridge/control/discover",
            post(ui_bridge_discover_handler),
        )
        .route(
            "/ui-bridge/control/snapshot",
            get(ui_bridge_get_snapshot_handler),
        )
        .route(
            "/ui-bridge/control/windows",
            get(ui_bridge_list_windows_handler),
        )
        .route(
            "/ui-bridge/control/annotated-screenshot",
            get(ui_bridge_annotated_screenshot_handler),
        )
        .route(
            "/ui-bridge/control/console-errors",
            get(ui_bridge_get_console_errors_handler),
        )
        .route(
            "/ui-bridge/control/console-errors/clear",
            post(ui_bridge_clear_console_errors_handler),
        )
        // Browser events & timeline
        .route(
            "/ui-bridge/control/browser-events",
            get(ui_bridge_get_browser_events_handler),
        )
        .route(
            "/ui-bridge/control/timeline",
            get(ui_bridge_get_timeline_handler),
        )
        .route(
            "/ui-bridge/control/network-chains",
            get(ui_bridge_get_network_chains_handler),
        )
        .route(
            "/ui-bridge/control/error-snapshots",
            get(ui_bridge_get_error_snapshots_handler),
        )
        .route(
            "/ui-bridge/control/error-report",
            get(ui_bridge_get_error_report_handler),
        )
        // Error sessions
        .route(
            "/ui-bridge/control/error-sessions/start",
            post(ui_bridge_start_error_session_handler),
        )
        .route(
            "/ui-bridge/control/error-sessions/end",
            post(ui_bridge_end_error_session_handler),
        )
        .route(
            "/ui-bridge/control/error-sessions",
            get(ui_bridge_get_error_sessions_handler),
        )
        // Error baselines
        .route(
            "/ui-bridge/control/error-baselines/capture",
            post(ui_bridge_capture_error_baseline_handler),
        )
        .route(
            "/ui-bridge/control/error-baselines/compare",
            post(ui_bridge_compare_error_baseline_handler),
        )
        // Detailed health (separate from /health which is app-level)
        .route(
            "/ui-bridge/control/health",
            get(ui_bridge_get_health_report_handler),
        )
        // Undo/Redo awareness
        .route(
            "/ui-bridge/control/undo-state",
            get(ui_bridge_get_undo_state_handler),
        )
        .route("/ui-bridge/control/undo", post(ui_bridge_undo_handler))
        .route("/ui-bridge/control/redo", post(ui_bridge_redo_handler))
        // Form state awareness
        .route("/ui-bridge/control/forms", get(ui_bridge_get_forms_handler))
        .route("/ui-bridge/control/fill", post(ui_bridge_fill_form_handler))
        .route(
            "/ui-bridge/control/forms/snapshot",
            post(ui_bridge_snapshot_forms_handler),
        )
        .route(
            "/ui-bridge/control/forms/diff",
            post(ui_bridge_diff_forms_handler),
        )
        // Clipboard
        .route(
            "/ui-bridge/control/clipboard",
            get(ui_bridge_clipboard_read_handler).post(ui_bridge_clipboard_write_handler),
        )
        // Network request monitoring
        .route(
            "/ui-bridge/control/network-requests",
            get(ui_bridge_get_network_requests_handler),
        )
        .route(
            "/ui-bridge/control/network-requests/in-flight",
            get(ui_bridge_get_network_requests_in_flight_handler),
        )
        .route(
            "/ui-bridge/control/network-requests/wait",
            post(ui_bridge_wait_for_network_request_handler),
        )
        .route(
            "/ui-bridge/control/network-request/{id}",
            get(ui_bridge_get_network_request_handler),
        )
        .route("/ui-bridge/control/specs", get(ui_bridge_get_specs_handler))
        .route(
            "/ui-bridge/control/spec/{id}",
            get(ui_bridge_get_spec_handler),
        )
        .route(
            "/ui-bridge/control/page/refresh",
            post(ui_bridge_page_refresh_handler),
        )
        .route(
            "/ui-bridge/control/page/hard-refresh",
            post(ui_bridge_page_hard_refresh_handler),
        )
        .route(
            "/ui-bridge/control/page/navigate",
            post(ui_bridge_page_navigate_handler),
        )
        .route(
            "/ui-bridge/control/page/back",
            post(ui_bridge_page_go_back_handler),
        )
        .route(
            "/ui-bridge/control/page/forward",
            post(ui_bridge_page_go_forward_handler),
        )
        .route(
            "/ui-bridge/control/query-selector",
            post(ui_bridge_query_selector_handler),
        )
        .route(
            "/ui-bridge/control/page/evaluate",
            post(ui_bridge_page_evaluate_handler),
        )
        .route(
            "/ui-bridge/control/page/evaluate-safe",
            post(ui_bridge_page_evaluate_safe_handler),
        )
        .route(
            "/ui-bridge/control/page/evaluate-batch",
            post(ui_bridge_page_evaluate_batch_handler),
        )
        // Convenience DOM interaction endpoints
        .route(
            "/ui-bridge/control/page/find-by-text",
            post(ui_bridge_find_by_text_handler),
        )
        .route(
            "/ui-bridge/control/page/click-by-text",
            post(ui_bridge_click_by_text_handler),
        )
        .route(
            "/ui-bridge/control/page/click-by-selector",
            post(ui_bridge_click_by_selector_handler),
        )
        .route(
            "/ui-bridge/control/page/read-value",
            post(ui_bridge_read_value_handler),
        )
        .route(
            "/ui-bridge/control/page/type-into",
            post(ui_bridge_type_into_handler),
        )
        .route(
            "/ui-bridge/control/page/summary",
            post(ui_bridge_page_summary_handler),
        )
        // Aliases under /ai/ namespace
        .route(
            "/ui-bridge/ai/page-summary",
            post(ui_bridge_page_summary_handler),
        )
        .route(
            "/ui-bridge/control/assert",
            post(ui_bridge_structured_assert_handler),
        )
        // Design Review
        .route(
            "/ui-bridge/control/design/element/{id}/styles",
            get(ui_bridge_design_element_styles_handler),
        )
        .route(
            "/ui-bridge/control/design/element/{id}/state-styles",
            post(ui_bridge_design_state_styles_handler),
        )
        .route(
            "/ui-bridge/control/design/snapshot",
            post(ui_bridge_design_snapshot_handler),
        )
        .route(
            "/ui-bridge/control/design/responsive",
            post(ui_bridge_design_responsive_handler),
        )
        .route(
            "/ui-bridge/control/design/audit",
            post(ui_bridge_design_audit_handler),
        )
        .route(
            "/ui-bridge/control/design/style-guide/load",
            post(ui_bridge_design_load_style_guide_handler),
        )
        .route(
            "/ui-bridge/control/design/style-guide",
            get(ui_bridge_design_get_style_guide_handler),
        )
        .route(
            "/ui-bridge/control/design/style-guide/clear",
            post(ui_bridge_design_clear_style_guide_handler),
        )
        // Change tracking
        .route(
            "/ui-bridge/control/ai/bookmarks",
            get(ui_bridge_list_bookmarks_handler).post(ui_bridge_save_bookmark_handler),
        )
        .route(
            "/ui-bridge/control/ai/bookmark/{name}",
            get(ui_bridge_get_bookmark_handler).delete(ui_bridge_delete_bookmark_handler),
        )
        .route(
            "/ui-bridge/control/ai/bookmark/{name}/diff",
            get(ui_bridge_diff_from_bookmark_handler),
        )
        .route(
            "/ui-bridge/control/ai/execute-with-diff",
            post(ui_bridge_execute_with_diff_handler),
        )
        .route(
            "/ui-bridge/control/ai/wait-for-change",
            post(ui_bridge_wait_for_change_handler),
        )
        .route(
            "/ui-bridge/control/ai/categorize-last-diff",
            get(ui_bridge_categorize_last_diff_handler),
        )
        .route(
            "/ui-bridge/control/ai/scoped-diff",
            post(ui_bridge_scoped_diff_handler),
        )
        .route(
            "/ui-bridge/control/ai/summarize-diff",
            post(ui_bridge_summarize_diff_handler),
        )
        .route(
            "/ui-bridge/control/ai/structured-changes",
            post(ui_bridge_structured_changes_handler),
        )
        .route(
            "/ui-bridge/control/ai/change-buffer/enable",
            post(ui_bridge_enable_change_buffer_handler),
        )
        .route(
            "/ui-bridge/control/ai/change-buffer/disable",
            post(ui_bridge_disable_change_buffer_handler),
        )
        .route(
            "/ui-bridge/control/ai/change-buffer/drain",
            post(ui_bridge_drain_change_buffer_handler),
        )
        .route(
            "/ui-bridge/control/ai/change-buffer/size",
            get(ui_bridge_get_change_buffer_size_handler),
        )
        // Keyboard shortcuts
        .route(
            "/ui-bridge/control/keyboard-shortcuts",
            get(ui_bridge_get_keyboard_shortcuts_handler),
        )
        // Idle detection
        .route(
            "/ui-bridge/control/idle-status",
            get(ui_bridge_get_idle_status_handler),
        )
        .route(
            "/ui-bridge/control/wait-for-idle",
            post(ui_bridge_wait_for_idle_handler),
        )
        .route(
            "/ui-bridge/control/diagnose-stuck",
            post(ui_bridge_diagnose_stuck_screen_handler),
        )
        .route(
            "/ui-bridge/control/page-health",
            post(ui_bridge_page_health_handler),
        )
        // AI search & find
        .route(
            "/ui-bridge/control/ai/search",
            post(ui_bridge_ai_search_handler),
        )
        .route(
            "/ui-bridge/control/ai/find",
            post(ui_bridge_ai_find_handler),
        )
        // Element image capture (DOM-based, no screen capture)
        .route(
            "/ui-bridge/control/capture-element-images",
            post(ui_bridge_capture_element_images_handler),
        )
        // Element image metadata (reads <img> src attributes, no rendering)
        .route(
            "/ui-bridge/control/get-element-images",
            post(ui_bridge_get_element_images_handler),
        )
        // Find, workflows, element state, render log
        .route("/ui-bridge/control/find", post(ui_bridge_find_handler))
        .route(
            "/ui-bridge/control/workflows",
            get(ui_bridge_get_workflows_handler),
        )
        .route(
            "/ui-bridge/control/workflow/{id}/run",
            post(ui_bridge_run_workflow_handler),
        )
        .route(
            "/ui-bridge/control/workflow/{run_id}/status",
            get(ui_bridge_get_workflow_status_handler),
        )
        .route(
            "/ui-bridge/control/element/{id}/state",
            get(ui_bridge_get_element_state_handler),
        )
        .route(
            "/ui-bridge/control/render-log",
            get(ui_bridge_get_render_log_handler).post(ui_bridge_append_render_log_handler),
        )
        // Render log alias (matches web's /render-log path for cross-app consistency)
        .route(
            "/ui-bridge/render-log",
            get(ui_bridge_get_render_log_handler).post(ui_bridge_append_render_log_handler),
        )
        // Action plan execution & caching
        .route(
            "/ui-bridge/control/action-plan",
            post(ui_bridge_execute_action_plan_handler),
        )
        .route(
            "/ui-bridge/control/action-plan/cache",
            get(ui_bridge_action_plan_cache_lookup_handler),
        )
        .route(
            "/ui-bridge/control/action-plan/cache/stats",
            get(ui_bridge_action_plan_cache_stats_handler),
        )
        // Batch execution
        .route("/ui-bridge/batch", post(ui_bridge_batch_handler))
        // Diagnostics & health
        .route("/ui-bridge/diagnostics", get(ui_bridge_diagnostics_handler))
        .route(
            "/ui-bridge/circuit-breaker/reset",
            post(ui_bridge_circuit_breaker_reset_handler),
        )
        .route("/ui-bridge/pong", post(ui_bridge_pong_handler))
        .route(
            "/ui-bridge/ipc-response",
            post(ui_bridge_ipc_response_handler),
        )
        // Exploration
        .route("/ui-bridge/explore", post(start_ui_bridge_exploration))
        .route(
            "/ui-bridge/explore/status",
            get(get_ui_bridge_exploration_status),
        )
        .route(
            "/ui-bridge/explore/results",
            get(get_ui_bridge_exploration_results),
        )
        .route("/ui-bridge/explore/stop", post(stop_ui_bridge_exploration))
        .route(
            "/ui-bridge/discover-states",
            post(discover_states_from_renders),
        )
        // AI route aliases (/ui-bridge/ai/* mirrors /ui-bridge/control/ai/*)
        .route(
            "/ui-bridge/ai/bookmarks",
            get(ui_bridge_list_bookmarks_handler).post(ui_bridge_save_bookmark_handler),
        )
        .route(
            "/ui-bridge/ai/bookmark/{name}",
            get(ui_bridge_get_bookmark_handler).delete(ui_bridge_delete_bookmark_handler),
        )
        .route(
            "/ui-bridge/ai/bookmark/{name}/diff",
            get(ui_bridge_diff_from_bookmark_handler),
        )
        .route(
            "/ui-bridge/ai/execute-with-diff",
            post(ui_bridge_execute_with_diff_handler),
        )
        .route(
            "/ui-bridge/ai/wait-for-change",
            post(ui_bridge_wait_for_change_handler),
        )
        .route(
            "/ui-bridge/ai/categorize-last-diff",
            get(ui_bridge_categorize_last_diff_handler),
        )
        .route(
            "/ui-bridge/ai/scoped-diff",
            post(ui_bridge_scoped_diff_handler),
        )
        .route(
            "/ui-bridge/ai/summarize-diff",
            post(ui_bridge_summarize_diff_handler),
        )
        .route(
            "/ui-bridge/ai/structured-changes",
            post(ui_bridge_structured_changes_handler),
        )
        .route(
            "/ui-bridge/ai/change-buffer/enable",
            post(ui_bridge_enable_change_buffer_handler),
        )
        .route(
            "/ui-bridge/ai/change-buffer/disable",
            post(ui_bridge_disable_change_buffer_handler),
        )
        .route(
            "/ui-bridge/ai/change-buffer/drain",
            post(ui_bridge_drain_change_buffer_handler),
        )
        .route(
            "/ui-bridge/ai/change-buffer/size",
            get(ui_bridge_get_change_buffer_size_handler),
        )
        .route("/ui-bridge/ai/search", post(ui_bridge_ai_search_handler))
        .route("/ui-bridge/ai/find", post(ui_bridge_ai_find_handler))
        // Phase 2: AI endpoints
        .route("/ui-bridge/ai/execute", post(ui_bridge_ai_execute_handler))
        .route("/ui-bridge/ai/assert", post(ui_bridge_ai_assert_handler))
        .route(
            "/ui-bridge/ai/assert-batch",
            post(ui_bridge_ai_assert_batch_handler),
        )
        .route("/ui-bridge/ai/snapshot", get(ui_bridge_ai_snapshot_handler))
        .route("/ui-bridge/ai/summary", get(ui_bridge_ai_summary_handler))
        // Phase 3: Capabilities
        .route(
            "/ui-bridge/capabilities",
            get(ui_bridge_capabilities_handler),
        )
        // Phase 4: Idle sub-signals
        .route(
            "/ui-bridge/control/idle-status/{signal}",
            get(ui_bridge_get_idle_signal_handler),
        )
        .route(
            "/ui-bridge/control/wait-for-idle/{signal}",
            post(ui_bridge_wait_for_idle_signal_handler),
        )
        .route(
            "/ui-bridge/control/wait-for-targets",
            post(ui_bridge_wait_for_targets_handler),
        )
        // Phase 4: Action history & metrics
        .route(
            "/ui-bridge/control/action-history",
            get(ui_bridge_get_action_history_handler),
        )
        .route(
            "/ui-bridge/control/metrics",
            get(ui_bridge_get_interaction_metrics_handler),
        )
        // Phase 5: Annotations
        .route(
            "/ui-bridge/control/annotations",
            get(ui_bridge_annotations_list_handler).post(ui_bridge_annotations_create_handler),
        )
        .route(
            "/ui-bridge/control/annotation/{id}",
            get(ui_bridge_annotations_get_handler)
                .put(ui_bridge_annotations_update_handler)
                .delete(ui_bridge_annotations_delete_handler),
        )
        .route(
            "/ui-bridge/control/annotations/coverage",
            get(ui_bridge_annotations_coverage_handler),
        )
        .route(
            "/ui-bridge/control/annotations/export",
            get(ui_bridge_annotations_export_handler),
        )
        // Media routes
        .route(
            "/ui-bridge/ai/media/find",
            post(ui_bridge_media_find_handler),
        )
        .route(
            "/ui-bridge/ai/media/audit/{audit_type}",
            post(ui_bridge_media_audit_handler),
        )
        .route(
            "/ui-bridge/ai/media/snapshot",
            post(ui_bridge_media_snapshot_handler),
        )
        .route(
            "/ui-bridge/ai/media/analyze",
            post(ui_bridge_media_analyze_handler),
        )
        .route(
            "/ui-bridge/ai/media/analyze/batch",
            post(ui_bridge_media_analyze_handler),
        )
        .route(
            "/ui-bridge/ai/media/analyze/page",
            post(ui_bridge_media_analyze_handler),
        )
        // State machine routes
        .route(
            "/ui-bridge/control/states",
            get(ui_bridge_get_states_handler),
        )
        .route(
            "/ui-bridge/control/states/active",
            get(ui_bridge_get_active_states_handler),
        )
        .route(
            "/ui-bridge/control/states/snapshot",
            get(ui_bridge_get_state_snapshot_handler),
        )
        .route(
            "/ui-bridge/control/states/find-path",
            post(ui_bridge_find_state_path_handler),
        )
        .route(
            "/ui-bridge/control/states/navigate",
            post(ui_bridge_navigate_to_state_handler),
        )
        .route(
            "/ui-bridge/control/state/{id}",
            get(ui_bridge_get_state_handler),
        )
        .route(
            "/ui-bridge/control/state/{id}/activate",
            post(ui_bridge_activate_state_handler),
        )
        .route(
            "/ui-bridge/control/state/{id}/deactivate",
            post(ui_bridge_deactivate_state_handler),
        )
        .route(
            "/ui-bridge/control/state-groups",
            get(ui_bridge_get_state_groups_handler),
        )
        .route(
            "/ui-bridge/control/state-group/{id}/activate",
            post(ui_bridge_activate_state_group_handler),
        )
        .route(
            "/ui-bridge/control/state-group/{id}/deactivate",
            post(ui_bridge_deactivate_state_group_handler),
        )
        .route(
            "/ui-bridge/control/transitions",
            get(ui_bridge_get_transitions_handler),
        )
        .route(
            "/ui-bridge/control/transition/{id}/can-execute",
            get(ui_bridge_can_execute_transition_handler),
        )
        .route(
            "/ui-bridge/control/transition/{id}/execute",
            post(ui_bridge_execute_transition_handler),
        )
        // AI semantic search & diff
        .route(
            "/ui-bridge/ai/semantic-search",
            post(ui_bridge_ai_semantic_search_handler),
        )
        .route("/ui-bridge/ai/diff", get(ui_bridge_ai_diff_handler))
        // Intents
        .route(
            "/ui-bridge/control/intents",
            get(ui_bridge_get_intents_handler).post(ui_bridge_register_intent_handler),
        )
        .route(
            "/ui-bridge/control/intents/find",
            post(ui_bridge_find_intent_handler),
        )
        .route(
            "/ui-bridge/control/intents/execute",
            post(ui_bridge_execute_intent_handler),
        )
        // Component state
        .route(
            "/ui-bridge/control/component/{id}/state",
            get(ui_bridge_get_component_state_handler),
        )
        // Page scroll
        .route(
            "/ui-bridge/control/page/scroll",
            post(ui_bridge_scroll_page_handler),
        )
        // Performance entries
        .route(
            "/ui-bridge/control/performance-entries",
            get(ui_bridge_get_performance_entries_handler),
        )
        .route(
            "/ui-bridge/control/performance-entries/clear",
            post(ui_bridge_clear_performance_entries_handler),
        )
        // AI analysis
        .route(
            "/ui-bridge/ai/analyze/data",
            post(ui_bridge_ai_analyze_data_handler),
        )
        .route(
            "/ui-bridge/ai/analyze/regions",
            post(ui_bridge_ai_analyze_regions_handler),
        )
        .route(
            "/ui-bridge/ai/analyze/structured-data",
            post(ui_bridge_ai_analyze_structured_handler),
        )
        .route(
            "/ui-bridge/ai/analyze/cross-app-compare",
            post(ui_bridge_ai_analyze_cross_app_handler),
        )
        .route(
            "/ui-bridge/ai/recovery/attempt",
            post(ui_bridge_ai_recovery_attempt_handler),
        )
        // Design evaluation
        .route(
            "/ui-bridge/control/design/evaluate",
            post(ui_bridge_design_evaluate_handler),
        )
        .route(
            "/ui-bridge/control/design/evaluate/baseline",
            post(ui_bridge_design_evaluate_baseline_handler),
        )
        .route(
            "/ui-bridge/control/design/evaluate/contexts",
            get(ui_bridge_design_evaluate_contexts_handler),
        )
        .route(
            "/ui-bridge/control/design/evaluate/diff",
            post(ui_bridge_design_evaluate_diff_handler),
        )
        // Media compare
        .route(
            "/ui-bridge/ai/media/compare",
            post(ui_bridge_media_compare_handler),
        )
        // Annotations import
        .route(
            "/ui-bridge/control/annotations/import",
            post(ui_bridge_annotations_import_handler),
        )
        // Intents from NL query
        .route(
            "/ui-bridge/control/intents/execute-from-query",
            post(ui_bridge_execute_intent_from_query_handler),
        )
        // Debug
        .route(
            "/ui-bridge/debug/element-tree",
            get(ui_bridge_get_element_tree_handler),
        )
        .route(
            "/ui-bridge/debug/highlight/{id}",
            post(ui_bridge_highlight_element_handler),
        )
        // Combined health signals for stall detection
        .route(
            "/ui-bridge/control/health-signals",
            get(ui_bridge_health_signals_handler),
        )
        // Persisted interaction history (cross-run analysis)
        .route(
            "/ui-bridge/history/elements",
            get(ui_bridge_history_elements_handler),
        )
        .route(
            "/ui-bridge/history/element/{id}",
            get(ui_bridge_history_element_handler),
        )
        .route(
            "/ui-bridge/history/flaky",
            get(ui_bridge_history_flaky_handler),
        )
        .route(
            "/ui-bridge/graph/element-reliability",
            get(ui_bridge_element_reliability_handler),
        )
        // Analytics endpoints (Phase 1 + 2)
        .route(
            "/ui-bridge/analytics/decay-curve",
            get(analytics_decay_curve_handler),
        )
        .route(
            "/ui-bridge/analytics/action-baselines",
            get(analytics_action_baselines_handler),
        )
        .route(
            "/ui-bridge/analytics/failure-taxonomy",
            get(analytics_failure_taxonomy_handler),
        )
        .route(
            "/ui-bridge/analytics/fragility-heatmap",
            get(analytics_fragility_heatmap_handler),
        )
        .route(
            "/ui-bridge/analytics/regressions",
            get(analytics_regressions_handler),
        )
        .route(
            "/ui-bridge/analytics/stall-frequency",
            get(analytics_stall_frequency_handler),
        )
        .route(
            "/ui-bridge/analytics/intervention-effectiveness",
            get(analytics_intervention_handler),
        )
        .route(
            "/ui-bridge/analytics/state-coverage",
            get(analytics_state_coverage_handler),
        )
        .route(
            "/ui-bridge/analytics/annotation-gaps",
            get(analytics_annotation_gaps_handler),
        )
        .route(
            "/ui-bridge/analytics/health-score",
            get(analytics_health_score_handler),
        )
        .route(
            "/ui-bridge/analytics/recommendations",
            get(analytics_recommendations_handler),
        )
        // Runner-specific: tab navigation and storage management
        .route(
            "/ui-bridge/control/navigate-tab",
            post(ui_bridge_navigate_tab_handler),
        )
        .route(
            "/ui-bridge/control/clear-storage",
            post(ui_bridge_clear_storage_handler),
        )
}

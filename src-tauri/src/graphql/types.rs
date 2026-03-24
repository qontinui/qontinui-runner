//! GraphQL type definitions for qontinui-runner.
//!
//! These types mirror existing Rust structs but with async-graphql derives
//! for schema generation. They serve as the typed contract between backend
//! and frontend, replacing untyped serde_json::Value payloads.

use async_graphql::*;

// ==========================================================================
// Circuit Breaker
// ==========================================================================

/// Circuit breaker state for the UI Bridge relay.
#[derive(Enum, Copy, Clone, Eq, PartialEq, Debug)]
pub enum CircuitBreakerState {
    /// Circuit is closed — requests flow normally.
    Closed,
    /// Circuit is open — requests are rejected to prevent cascading failure.
    Open,
    /// Circuit is half-open — a single probe request is allowed through.
    HalfOpen,
}

// ==========================================================================
// Health
// ==========================================================================

/// Real-time health status of the UI Bridge relay.
#[derive(SimpleObject, Clone, Debug)]
pub struct UiBridgeHealth {
    /// Whether the browser frontend is responsive (last pong < 15s).
    pub responsive: bool,
    /// Timestamp of the last frontend heartbeat (epoch ms).
    pub last_heartbeat: String,
    /// Milliseconds since the last heartbeat.
    pub heartbeat_age_ms: String,
    /// Server uptime in seconds.
    pub uptime_seconds: String,
    /// Number of pending UI Bridge requests awaiting browser response.
    pub pending_requests: i32,
    /// Current circuit breaker state.
    pub circuit_breaker: CircuitBreakerState,
    /// Number of available semaphore permits (out of 6 max).
    pub semaphore_available: i32,
}

// ==========================================================================
// Elements
// ==========================================================================

/// Bounding rectangle for a UI element.
#[derive(SimpleObject, Clone, Debug)]
pub struct ElementBounds {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// A UI element discovered via the UI Bridge.
#[derive(SimpleObject, Clone, Debug)]
pub struct UiBridgeElement {
    /// Unique element identifier.
    pub id: String,
    /// HTML tag name.
    pub tag: String,
    /// ARIA role, if present.
    pub role: Option<String>,
    /// Visible text content.
    pub text: Option<String>,
    /// Whether the element is currently visible.
    pub visible: bool,
    /// Bounding rectangle in viewport coordinates.
    pub bounds: Option<ElementBounds>,
    /// Raw attributes as JSON.
    pub attributes: Json<serde_json::Value>,
}

// ==========================================================================
// Snapshots
// ==========================================================================

/// Viewport dimensions.
#[derive(SimpleObject, Clone, Debug)]
pub struct Viewport {
    pub width: i32,
    pub height: i32,
}

/// A complete DOM snapshot from the UI Bridge.
#[derive(SimpleObject, Clone, Debug)]
pub struct DomSnapshot {
    /// Current page URL.
    pub url: String,
    /// Page title.
    pub title: String,
    /// Snapshot capture timestamp (epoch ms).
    pub timestamp: String,
    /// All discovered elements.
    pub elements: Vec<UiBridgeElement>,
    /// Viewport dimensions.
    pub viewport: Option<Viewport>,
}

/// A chunk of a streaming DOM snapshot.
#[derive(SimpleObject, Clone, Debug)]
pub struct SnapshotChunk {
    /// Index of this chunk (0-based).
    pub chunk_index: i32,
    /// Total number of chunks (None if unknown).
    pub total_chunks: Option<i32>,
    /// Elements in this chunk.
    pub elements: Vec<UiBridgeElement>,
    /// Whether this is the final chunk.
    pub complete: bool,
}

// ==========================================================================
// Error Types
// ==========================================================================

/// Machine-readable error codes for UI Bridge operations.
/// Maps 1:1 to the existing UiBridgeErrorCode enum.
#[derive(Enum, Copy, Clone, Eq, PartialEq, Debug)]
pub enum UiBridgeErrorCode {
    Timeout,
    CircuitBreakerOpen,
    ConcurrencyLimitReached,
    FrontendUnresponsive,
    WindowNotFound,
    ElementNotFound,
    ElementNotVisible,
    ElementNotEnabled,
    ElementStale,
    ActionFailed,
    AssertionFailed,
    UnknownAssertionType,
    InternalError,
}

/// Structured error detail with recovery hint.
#[derive(SimpleObject, Clone, Debug)]
pub struct UiBridgeErrorDetail {
    /// Machine-readable error code.
    pub code: UiBridgeErrorCode,
    /// Human-readable error message.
    pub message: String,
    /// Suggested recovery action (e.g., "Resnapshot", "RetryAfterDelay").
    pub recovery: Option<String>,
    /// Additional context as JSON.
    pub context: Option<Json<serde_json::Value>>,
}

// ==========================================================================
// Action Results
// ==========================================================================

/// Result of a UI Bridge action (click, navigate, etc.).
#[derive(SimpleObject, Clone, Debug)]
pub struct ActionResult {
    /// Whether the action succeeded.
    pub success: bool,
    /// Response data from the browser (if successful).
    pub data: Option<Json<serde_json::Value>>,
    /// Structured error detail (if failed).
    pub error: Option<UiBridgeErrorDetail>,
    /// Action execution time in milliseconds.
    pub duration_ms: String,
}

// ==========================================================================
// SDK Connection Info
// ==========================================================================

/// Information about an external SDK app connection.
#[derive(SimpleObject, Clone, Debug)]
pub struct SdkConnectionInfo {
    /// App identifier.
    pub app_id: String,
    /// Display name.
    pub app_name: String,
    /// Base URL of the connected app.
    pub url: String,
    /// Whether the app is currently responsive.
    pub responsive: bool,
    /// Whether this is the active (selected) connection.
    pub is_active: bool,
}

// ==========================================================================
// Subscription Event Types
// ==========================================================================

/// Typed runner event for the `runnerEvents` subscription.
/// Replaces untyped serde_json::Value events from /ws/events.
#[derive(Union, Clone, Debug)]
pub enum RunnerEvent {
    OrchestratorStateChange(OrchestratorStateChangeEvent),
    StepProgress(StepProgressEvent),
    TaskRunUpdate(TaskRunUpdateEvent),
    FindingDetected(FindingDetectedEvent),
    FindingResolved(FindingResolvedEvent),
    AiOutputChunk(AiOutputChunkEvent),
    GenericEvent(GenericRunnerEvent),
}

#[derive(SimpleObject, Clone, Debug)]
pub struct OrchestratorStateChangeEvent {
    pub task_run_id: String,
    pub workflow_stage: String,
    pub iteration: i32,
    pub phase: String,
    pub state_data: Option<Json<serde_json::Value>>,
}

#[derive(SimpleObject, Clone, Debug)]
pub struct StepProgressEvent {
    pub task_run_id: String,
    pub step_index: i32,
    pub step_name: String,
    pub status: String,
    pub details: Option<Json<serde_json::Value>>,
    pub timestamp: String,
}

#[derive(SimpleObject, Clone, Debug)]
pub struct TaskRunUpdateEvent {
    pub task_run_id: String,
    pub status: String,
    pub iteration: Option<i32>,
    pub details: Option<Json<serde_json::Value>>,
    pub timestamp: String,
}

#[derive(SimpleObject, Clone, Debug)]
pub struct FindingDetectedEvent {
    pub finding: Json<serde_json::Value>,
}

#[derive(SimpleObject, Clone, Debug)]
pub struct FindingResolvedEvent {
    pub finding: Json<serde_json::Value>,
}

#[derive(SimpleObject, Clone, Debug)]
pub struct AiOutputChunkEvent {
    pub task_run_id: String,
    pub chunk: String,
    pub accumulated_length: i32,
}

#[derive(SimpleObject, Clone, Debug)]
pub struct GenericRunnerEvent {
    pub event_type: String,
    pub data: Json<serde_json::Value>,
}

// ==========================================================================
// Browser Notification Types
// ==========================================================================

/// Proactive notification from the browser SDK.
#[derive(Union, Clone, Debug)]
pub enum BrowserNotification {
    ModalAppeared(ModalAppearedEvent),
    PageNavigated(PageNavigatedEvent),
    ElementChanged(ElementChangedEvent),
    ConsoleError(ConsoleErrorEvent),
    NetworkRequest(NetworkRequestEvent),
}

#[derive(SimpleObject, Clone, Debug)]
pub struct ModalAppearedEvent {
    pub url: String,
    pub title: Option<String>,
    pub timestamp: String,
}

#[derive(SimpleObject, Clone, Debug)]
pub struct PageNavigatedEvent {
    pub from_url: Option<String>,
    pub to_url: String,
    pub timestamp: String,
}

#[derive(SimpleObject, Clone, Debug)]
pub struct ElementChangedEvent {
    pub observation_id: String,
    pub selector: String,
    pub property: String,
    pub old_value: Option<String>,
    pub new_value: Option<String>,
    pub timestamp: String,
}

#[derive(SimpleObject, Clone, Debug)]
pub struct ConsoleErrorEvent {
    pub message: String,
    pub source: Option<String>,
    pub line: Option<i32>,
    pub timestamp: String,
}

#[derive(SimpleObject, Clone, Debug)]
pub struct NetworkRequestEvent {
    pub url: String,
    pub method: String,
    pub status: i32,
    pub duration_ms: Option<String>,
    pub timestamp: String,
}

// ==========================================================================
// Tab Connection Types
// ==========================================================================

/// Tab connection lifecycle event.
#[derive(SimpleObject, Clone, Debug)]
pub struct TabConnectionEvent {
    /// Unique tab identifier.
    pub tab_id: String,
    /// Event type: "connected", "disconnected", "promoted", "demoted".
    pub event_type: String,
    /// Current page URL (if known).
    pub url: Option<String>,
    /// Current page title (if known).
    pub title: Option<String>,
    /// Whether this tab is the primary command recipient.
    pub is_primary: bool,
    /// Event timestamp (epoch ms).
    pub timestamp: String,
}

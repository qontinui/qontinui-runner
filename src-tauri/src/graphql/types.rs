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
    CostUpdate(GqlCostUpdateEvent),
    BudgetWarning(GqlBudgetWarningEvent),
    CostAnomaly(GqlCostAnomalyEvent),
    GenericEvent(GenericRunnerEvent),
}

#[derive(SimpleObject, Clone, Debug)]
pub struct OrchestratorStateChangeEvent {
    pub task_run_id: String,
    pub workflow_stage: String,
    pub iteration: Option<i32>,
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
// Cost Event Types
// ==========================================================================

/// Real-time cost update after each AI call.
#[derive(SimpleObject, Clone, Debug)]
pub struct GqlCostUpdateEvent {
    pub task_run_id: String,
    pub phase: String,
    pub iteration: Option<i32>,
    /// Input tokens (u64 as string for GraphQL BigInt compat).
    pub input_tokens: String,
    /// Output tokens (u64 as string for GraphQL BigInt compat).
    pub output_tokens: String,
    /// Cache creation tokens (u64 as string for GraphQL BigInt compat).
    pub cache_creation_tokens: String,
    /// Cache read tokens (u64 as string for GraphQL BigInt compat).
    pub cache_read_tokens: String,
    pub cost_usd: f64,
    pub cumulative_cost_usd: f64,
    pub cache_hit_rate: f64,
    pub timestamp: String,
}

/// Budget warning when consumption exceeds a threshold.
#[derive(SimpleObject, Clone, Debug)]
pub struct GqlBudgetWarningEvent {
    pub task_run_id: String,
    pub remaining_fraction: f64,
    pub total_cost_usd: f64,
    pub budget_limit_usd: f64,
    pub message: String,
    pub timestamp: String,
}

/// Cost anomaly detected via statistical analysis.
#[derive(SimpleObject, Clone, Debug)]
pub struct GqlCostAnomalyEvent {
    pub task_run_id: String,
    pub cost_usd: f64,
    pub mean_cost_usd: f64,
    pub std_dev: f64,
    pub z_score: f64,
    pub message: String,
    pub timestamp: String,
}

/// Union of all cost-related events for the `costEvents` subscription.
#[derive(Union, Clone, Debug)]
pub enum CostEvent {
    CostUpdate(GqlCostUpdateEvent),
    BudgetWarning(GqlBudgetWarningEvent),
    CostAnomaly(GqlCostAnomalyEvent),
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

// ==========================================================================
// Task Run Types
// ==========================================================================

/// A task run — the single concept for all runs (AI, automation, or mixed).
#[derive(SimpleObject, Clone, Debug)]
pub struct GqlTaskRun {
    pub id: String,
    pub task_name: String,
    pub prompt: Option<String>,
    pub task_type: String,
    pub status: String,
    pub sessions_count: i32,
    pub max_sessions: Option<i32>,
    pub auto_continue: bool,
    pub error_message: Option<String>,
    pub config_id: Option<String>,
    pub workflow_name: Option<String>,
    pub workflow_id: Option<String>,
    pub summary: Option<String>,
    pub goal_achieved: Option<bool>,
    pub remaining_work: Option<String>,
    pub workflow_type: Option<String>,
    pub workspace_id: Option<String>,
    pub triggered_by: Option<String>,
    pub parent_task_run_id: Option<String>,
    pub root_task_run_id: Option<String>,
    pub depth: i32,
    pub bridge_id: Option<String>,
    pub result_data: Option<Json<serde_json::Value>>,
    pub is_reflection: bool,
    pub reflection_source_task_run_id: Option<String>,
    pub is_follow_up: bool,
    pub follow_up_source_task_run_id: Option<String>,
    pub is_fixer: bool,
    pub fixer_source_task_run_id: Option<String>,
    pub is_meta_optimizer: bool,
    pub is_review: bool,
    pub blocks_parent: bool,
    pub created_at: String,
    pub updated_at: String,
    pub completed_at: Option<String>,
}

impl GqlTaskRun {
    /// Convert from the database TaskRun struct.
    pub fn from_db(tr: crate::database::TaskRun) -> Self {
        let result_data = tr
            .result_data
            .as_ref()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok().map(Json));
        Self {
            id: tr.id,
            task_name: tr.task_name,
            prompt: tr.prompt,
            task_type: tr.task_type,
            status: tr.status,
            sessions_count: tr.sessions_count as i32,
            max_sessions: tr.max_sessions.map(|v| v as i32),
            auto_continue: tr.auto_continue,
            error_message: tr.error_message,
            config_id: tr.config_id,
            workflow_name: tr.workflow_name,
            workflow_id: tr.workflow_id,
            summary: tr.summary,
            goal_achieved: tr.goal_achieved,
            remaining_work: tr.remaining_work,
            workflow_type: tr.workflow_type,
            workspace_id: tr.workspace_id,
            triggered_by: tr.triggered_by,
            parent_task_run_id: tr.parent_task_run_id,
            root_task_run_id: tr.root_task_run_id,
            depth: tr.depth as i32,
            bridge_id: tr.bridge_id,
            result_data,
            is_reflection: tr.is_reflection,
            reflection_source_task_run_id: tr.reflection_source_task_run_id,
            is_follow_up: tr.is_follow_up,
            follow_up_source_task_run_id: tr.follow_up_source_task_run_id,
            is_fixer: tr.is_fixer,
            fixer_source_task_run_id: tr.fixer_source_task_run_id,
            is_meta_optimizer: tr.is_meta_optimizer,
            is_review: tr.is_review,
            blocks_parent: tr.blocks_parent,
            created_at: tr.created_at,
            updated_at: tr.updated_at,
            completed_at: tr.completed_at,
        }
    }
}

// ==========================================================================
// Workflow Types
// ==========================================================================

/// Summary of a unified workflow (lightweight, no step details).
#[derive(SimpleObject, Clone, Debug)]
pub struct GqlWorkflowSummary {
    pub id: String,
    pub name: String,
    pub description: String,
    pub category: String,
    pub tags: Vec<String>,
    /// `None` is rendered as "unlimited" in clients; a positive value is the cap.
    pub max_iterations: Option<i32>,
    pub timeout_seconds: Option<i64>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub is_favorite: bool,
    pub multi_agent_mode: bool,
    pub workflow_architecture: Option<String>,
    pub stage_count: i32,
    pub created_at: String,
    pub updated_at: String,
}

/// Full unified workflow definition including step details as JSON.
#[derive(SimpleObject, Clone, Debug)]
pub struct GqlWorkflow {
    pub id: String,
    pub name: String,
    pub description: String,
    pub category: String,
    pub tags: Vec<String>,
    pub setup_steps: Json<serde_json::Value>,
    pub verification_steps: Json<serde_json::Value>,
    pub agentic_steps: Json<serde_json::Value>,
    pub completion_steps: Json<serde_json::Value>,
    /// `None` is rendered as "unlimited" in clients; a positive value is the cap.
    pub max_iterations: Option<i32>,
    pub timeout_seconds: Option<i64>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub is_favorite: bool,
    pub multi_agent_mode: bool,
    pub reflection_mode: bool,
    pub approval_gate: bool,
    pub stop_on_failure: bool,
    pub use_worktree: bool,
    pub strict_cwd: bool,
    pub enforce_token_budget: bool,
    pub workflow_architecture: Option<String>,
    pub stages: Json<serde_json::Value>,
    pub created_at: String,
    pub updated_at: String,
}

// ==========================================================================
// Finding Types
// ==========================================================================

/// Finding category enum for GraphQL.
#[derive(Enum, Copy, Clone, Eq, PartialEq, Debug)]
pub enum GqlFindingCategory {
    CodeBug,
    Security,
    Performance,
    Todo,
    Enhancement,
    ConfigIssue,
    TestIssue,
    Documentation,
    RuntimeIssue,
    AlreadyFixed,
    ExpectedBehavior,
    Warning,
    DataMigration,
}

/// Finding severity enum for GraphQL.
#[derive(Enum, Copy, Clone, Eq, PartialEq, Debug)]
pub enum GqlFindingSeverity {
    Critical,
    High,
    Medium,
    Low,
    Info,
}

/// Finding lifecycle status.
#[derive(Enum, Copy, Clone, Eq, PartialEq, Debug)]
pub enum GqlFindingStatus {
    Detected,
    InProgress,
    NeedsInput,
    Resolved,
    WontFix,
    Deferred,
}

/// Finding action type.
#[derive(Enum, Copy, Clone, Eq, PartialEq, Debug)]
pub enum GqlFindingActionType {
    AutoFix,
    NeedsUserInput,
    Manual,
    Informational,
}

/// Code context for a finding.
#[derive(SimpleObject, Clone, Debug)]
pub struct GqlFindingCodeContext {
    pub file: Option<String>,
    pub line: Option<i32>,
    pub column: Option<i32>,
    pub snippet: Option<String>,
}

/// A finding detected by AI analysis.
#[derive(SimpleObject, Clone, Debug)]
pub struct GqlFinding {
    pub id: String,
    pub task_run_id: String,
    pub session_num: i32,
    pub category: GqlFindingCategory,
    pub severity: GqlFindingSeverity,
    pub status: GqlFindingStatus,
    pub action_type: GqlFindingActionType,
    pub title: String,
    pub description: String,
    pub resolution: Option<String>,
    pub code_context: Option<GqlFindingCodeContext>,
    pub signature_hash: String,
    pub user_response: Option<String>,
    pub detected_at: String,
    pub resolved_at: Option<String>,
    pub resolved_in_session: Option<i32>,
    pub updated_at: String,
}

impl GqlFinding {
    /// Convert from the domain Finding struct.
    pub fn from_domain(f: crate::findings::types::Finding) -> Self {
        use crate::findings::types::{
            FindingActionType as FA, FindingCategory as FC, FindingSeverity as FS,
            FindingStatus as FSt,
        };
        Self {
            id: f.id,
            task_run_id: f.task_run_id,
            session_num: f.session_num as i32,
            category: match f.category {
                FC::CodeBug => GqlFindingCategory::CodeBug,
                FC::Security => GqlFindingCategory::Security,
                FC::Performance => GqlFindingCategory::Performance,
                FC::Todo => GqlFindingCategory::Todo,
                FC::Enhancement => GqlFindingCategory::Enhancement,
                FC::ConfigIssue => GqlFindingCategory::ConfigIssue,
                FC::TestIssue => GqlFindingCategory::TestIssue,
                FC::Documentation => GqlFindingCategory::Documentation,
                FC::RuntimeIssue => GqlFindingCategory::RuntimeIssue,
                FC::AlreadyFixed => GqlFindingCategory::AlreadyFixed,
                FC::ExpectedBehavior => GqlFindingCategory::ExpectedBehavior,
                FC::Warning => GqlFindingCategory::Warning,
                FC::DataMigration => GqlFindingCategory::DataMigration,
            },
            severity: match f.severity {
                FS::Critical => GqlFindingSeverity::Critical,
                FS::High => GqlFindingSeverity::High,
                FS::Medium => GqlFindingSeverity::Medium,
                FS::Low => GqlFindingSeverity::Low,
                FS::Info => GqlFindingSeverity::Info,
            },
            status: match f.status {
                FSt::Detected => GqlFindingStatus::Detected,
                FSt::InProgress => GqlFindingStatus::InProgress,
                FSt::NeedsInput => GqlFindingStatus::NeedsInput,
                FSt::Resolved => GqlFindingStatus::Resolved,
                FSt::WontFix => GqlFindingStatus::WontFix,
                FSt::Deferred => GqlFindingStatus::Deferred,
            },
            action_type: match f.action_type {
                FA::AutoFix => GqlFindingActionType::AutoFix,
                FA::NeedsUserInput => GqlFindingActionType::NeedsUserInput,
                FA::Manual => GqlFindingActionType::Manual,
                FA::Informational => GqlFindingActionType::Informational,
            },
            title: f.title,
            description: f.description,
            resolution: f.resolution,
            code_context: f.code_context.map(|ctx| GqlFindingCodeContext {
                file: ctx.file,
                line: ctx.line.map(|v| v as i32),
                column: ctx.column.map(|v| v as i32),
                snippet: ctx.snippet,
            }),
            signature_hash: f.signature_hash,
            user_response: f.user_response,
            detected_at: f.detected_at,
            resolved_at: f.resolved_at,
            resolved_in_session: f.resolved_in_session.map(|v| v as i32),
            updated_at: f.updated_at,
        }
    }
}

/// Summary statistics for findings in a task run.
#[derive(SimpleObject, Clone, Debug)]
pub struct GqlFindingSummary {
    pub task_run_id: String,
    pub total: i32,
    pub by_category: Json<serde_json::Value>,
    pub by_severity: Json<serde_json::Value>,
    pub by_status: Json<serde_json::Value>,
    pub needs_input_count: i32,
    pub resolved_count: i32,
    pub outstanding_count: i32,
}

// ==========================================================================
// Runner Status Types
// ==========================================================================

/// Overall runner status.
#[derive(SimpleObject, Clone, Debug)]
pub struct GqlRunnerStatus {
    pub instance_name: Option<String>,
    pub api_port: i32,
    pub executor_running: bool,
    pub executor_state: String,
    pub config_loaded: bool,
    pub config_path: Option<String>,
    pub ai_analysis_running: bool,
}

/// Orchestration loop status.
#[derive(SimpleObject, Clone, Debug)]
pub struct GqlOrchestrationLoopStatus {
    pub running: bool,
    pub phase: String,
    pub current_iteration: i32,
    pub started_at: Option<String>,
    pub error: Option<String>,
}

/// Multi-loop aggregated status.
#[derive(SimpleObject, Clone, Debug)]
pub struct GqlMultiLoopStatus {
    pub loops: Vec<GqlLoopInstanceStatus>,
    pub all_complete: bool,
    pub any_error: bool,
    pub stop_all_on_error: bool,
}

/// Single loop instance status within a multi-loop.
#[derive(SimpleObject, Clone, Debug)]
pub struct GqlLoopInstanceStatus {
    pub loop_id: String,
    pub label: Option<String>,
    pub running: bool,
    pub phase: String,
    pub current_iteration: i32,
    /// `None` means the loop has no iteration cap (unlimited).
    pub max_iterations: Option<i32>,
    pub workflow_id: String,
    pub target_runner_port: i32,
    pub target_runner_id: Option<String>,
    pub is_pipeline: bool,
    pub started_at: Option<String>,
    pub error: Option<String>,
}

/// Workflow queue item.
#[derive(SimpleObject, Clone, Debug)]
pub struct GqlQueueItem {
    pub id: String,
    pub workflow_id: String,
    pub workflow_name: String,
    pub status: String,
    pub queued_at: String,
}

/// Workflow queue status.
#[derive(SimpleObject, Clone, Debug)]
pub struct GqlQueueStatus {
    pub queue_length: i32,
    pub items: Vec<GqlQueueItem>,
}

// ==========================================================================
// Input Types
// ==========================================================================

/// Paginated task run output.
#[derive(SimpleObject, Clone, Debug)]
pub struct GqlTaskRunOutput {
    /// The task run ID.
    pub task_run_id: String,
    /// Output text (may be truncated by offset/limit).
    pub content: String,
    /// Total length of the full output in characters.
    pub total_length: i32,
    /// Character offset of this content within the full output.
    pub offset: i32,
    /// Whether there is more content after this chunk.
    pub has_more: bool,
}

/// Input for creating a new task run.
#[derive(InputObject, Debug)]
pub struct CreateTaskRunInput {
    pub task_name: String,
    pub prompt: Option<String>,
    #[graphql(default_with = "\"task\".to_string()")]
    pub task_type: String,
    pub config_id: Option<String>,
    pub workflow_name: Option<String>,
    pub workflow_id: Option<String>,
    pub max_sessions: Option<i32>,
    #[graphql(default = true)]
    pub auto_continue: bool,
}

/// Input for updating a finding's status.
#[derive(InputObject, Debug)]
pub struct UpdateFindingStatusInput {
    pub finding_id: String,
    pub status: GqlFindingStatus,
    pub resolution: Option<String>,
}

// ==========================================================================
// Error Monitor Types
// ==========================================================================

/// An error event captured from application logs.
#[derive(SimpleObject, Clone, Debug)]
pub struct GqlErrorEvent {
    pub id: i32,
    pub log_source_name: String,
    pub task_run_id: Option<String>,
    pub severity: String,
    pub error_type: Option<String>,
    pub error_code: Option<String>,
    pub message: String,
    pub stack_trace: Option<String>,
    pub file_path: Option<String>,
    pub line_number: Option<i32>,
    pub status: String,
    pub occurrence_count: i32,
    pub first_seen_at: String,
    pub last_seen_at: String,
    pub signature_hash: String,
}

impl GqlErrorEvent {
    /// Convert from a StoredErrorEvent (deserialized from PG JSON).
    pub fn from_stored(e: crate::error_monitor::types::StoredErrorEvent) -> Self {
        Self {
            id: e.id as i32,
            log_source_name: e.log_source_name,
            task_run_id: e.task_run_id,
            severity: e.severity.as_str().to_string(),
            error_type: e.error_type,
            error_code: e.error_code,
            message: e.message,
            stack_trace: e.stack_trace,
            file_path: e.location.as_ref().map(|l| l.file_path.clone()),
            line_number: e
                .location
                .as_ref()
                .and_then(|l| l.line_number.map(|n| n as i32)),
            status: e.status.as_str().to_string(),
            occurrence_count: e.occurrence_count as i32,
            first_seen_at: e.first_seen_at,
            last_seen_at: e.last_seen_at,
            signature_hash: e.signature_hash,
        }
    }
}

/// Summary statistics for error events.
#[derive(SimpleObject, Clone, Debug)]
pub struct GqlErrorSummary {
    pub total_count: i32,
    pub unresolved_count: i32,
    pub critical_count: i32,
    pub error_count: i32,
    pub warning_count: i32,
}

/// A detected pattern across multiple errors.
#[derive(SimpleObject, Clone, Debug)]
pub struct GqlErrorPattern {
    pub pattern_type: String,
    pub name: String,
    pub description: Option<String>,
    pub frequency: i32,
    pub error_ids: Vec<i32>,
}

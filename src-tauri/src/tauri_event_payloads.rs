//! Wire-format payload types for runner-local Tauri events.
//!
//! This module is the single source of truth for the JSON shape of payloads
//! the runner emits on its own (non-`qontinui-types`) Tauri event channels.
//! Bindings generated from these structs flow to the frontend via
//! `qontinui-schemas/ts/src/generated/` so handlers in the React layer can
//! `listen<RunnerFinding>(...)` instead of hand-writing interfaces that drift
//! from the Rust definitions.
//!
//! ## Why this lives in the lib (not under `findings::types`)
//!
//! The schema export pipeline (`schema_export::export_all_schemas`) is part
//! of the `qontinui_runner_lib` library crate. The binary-only `findings`
//! module declared in `main.rs` is invisible to it. Defining the canonical
//! wire-format struct here, and having the binary's `findings::types`
//! re-export it, gives both consumers the same Rust type — no duplication,
//! no schema drift.
//!
//! ## Adding a new event payload
//!
//! When you add a new `AppEvent` variant whose payload is a runner-local
//! struct (i.e., not in `qontinui-types`), define the struct here, register
//! it in `schema_export.rs`, and run `npm run gen-events` to regenerate the
//! TypeScript bindings.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub use qontinui_types::task_run::{
    TaskRunFindingActionType as FindingActionType, TaskRunFindingCategory as FindingCategory,
    TaskRunFindingSeverity as FindingSeverity, TaskRunFindingStatus as FindingStatus,
};

// ============================================================================
// dev:seed-finding payload
// ============================================================================

/// Payload shape emitted on the `dev:seed-finding` Tauri event.
///
/// Field names use camelCase so the TS listener (`TauriFindingsListener.ts`)
/// can spread them directly into a `Finding` object without translation.
/// The actual emit site is in `commands::dev_findings::dev_seed_finding`.
///
/// Renamed in the schema registry to `DevSeedFindingPayload` to disambiguate
/// from the various `Finding*` types in `qontinui_types::findings`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[schemars(title = "DevSeedFindingPayload")]
pub struct DevSeedFindingPayload {
    pub id: String,
    #[serde(rename = "categoryId")]
    pub category_id: String,
    pub severity: String,
    pub status: String,
    pub title: String,
    pub description: String,
    #[serde(rename = "detectedAt")]
    pub detected_at: i64,
    #[serde(rename = "actionType")]
    pub action_type: String,
    pub actionable: bool,
    #[serde(rename = "sourceSessionId", skip_serializing_if = "Option::is_none")]
    pub source_session_id: Option<String>,
}

/// Code context for a finding (runner-local wire shape).
///
/// Renamed in the schema registry to `RunnerFindingCodeContext` to
/// disambiguate from `qontinui_types::findings::FindingCodeContext`.
#[derive(Debug, Clone, Serialize, Deserialize, Default, JsonSchema)]
#[schemars(title = "RunnerFindingCodeContext")]
pub struct FindingCodeContext {
    pub file: Option<String>,
    pub line: Option<u32>,
    pub column: Option<u32>,
    pub snippet: Option<String>,
}

/// User input request for a finding (runner-local wire shape).
///
/// Renamed in the schema registry to `RunnerFindingUserInput` to
/// disambiguate from `qontinui_types::findings::FindingUserInput`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", title = "RunnerFindingUserInput")]
pub struct FindingUserInput {
    pub question: String,
    #[serde(default = "default_input_type")]
    pub input_type: String,
    pub options: Option<Vec<String>>,
}

fn default_input_type() -> String {
    "text".to_string()
}

/// A finding detected by AI analysis (runner-local wire shape).
///
/// Wire format: serialized via `#[serde(rename_all = "camelCase")]` so all
/// snake_case Rust field names ship as camelCase on the Tauri event channels
/// `finding_detected` and `finding_resolved`. The frontend listener in
/// `services/TauriFindingsListener.ts` MUST read these fields by their
/// camelCase names — reading snake_case silently evaluates to `undefined`.
///
/// Renamed in the schema registry to `RunnerFinding` to disambiguate from
/// `qontinui_types::verification::Finding`, which has a different shape
/// (`confidence`, `findingType`, `evidence` vs this struct's flat fields).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(title = "RunnerFinding")]
pub struct Finding {
    pub id: String,
    pub task_run_id: String,
    pub session_num: u32,

    #[serde(rename = "categoryId")]
    pub category: FindingCategory,
    pub severity: FindingSeverity,
    pub status: FindingStatus,
    pub action_type: FindingActionType,

    pub title: String,
    pub description: String,
    pub resolution: Option<String>,

    pub code_context: Option<FindingCodeContext>,
    pub signature_hash: String,

    pub user_input: Option<FindingUserInput>,
    pub user_response: Option<String>,

    pub detected_at: String,
    pub resolved_at: Option<String>,
    pub resolved_in_session: Option<u32>,
    pub updated_at: String,
}

// ============================================================================
// execution-status channel (orchestrator::status_events)
// ============================================================================
//
// Every payload the orchestrator's `StatusEventEmitter` emits on the
// `execution-status` Tauri channel. The frontend hook
// `hooks/useExecutionStatus.ts` dispatches on the `type` tag and folds these
// snake_case wire events into its own camelCase display state.
//
// Registered in the schema registry under the `Raw*` names the TypeScript
// mirror (`qontinui-schemas/ts/src/execution/_api.ts`, Tier 3) has always
// used, so the hand-authored interfaces there become aliases.
//
// The whole event is ONE internally-tagged enum rather than a struct that
// flattens a `{type, task_run_id, timestamp}` base: the tag is then a closed
// set the schema (and so the generated TS union) can name, instead of a free
// `String` each emit site spells by hand. The serialized bytes are identical —
// serde writes the `type` tag first, then the variant struct's fields in
// declaration order, exactly as the flattened base did.

/// Assessed complexity level of a task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TaskComplexity {
    /// Simple tasks: quick fixes, small changes, formatting
    Simple,
    /// Medium tasks: feature additions, bug fixes, moderate refactoring
    Medium,
    /// Complex tasks: architecture changes, major refactoring, security audits
    Complex,
}

impl TaskComplexity {
    /// Get a human-readable name for this complexity level.
    pub fn display_name(&self) -> &'static str {
        match self {
            Self::Simple => "Simple",
            Self::Medium => "Medium",
            Self::Complex => "Complex",
        }
    }
}

/// Events that can trigger hook execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum HookTrigger {
    /// Before task execution starts
    PreExecution,
    /// After task execution completes (success or failure)
    PostExecution,
    /// When an error occurs during execution
    OnError,
    /// When verification fails
    OnVerificationFail,
    /// When task completes successfully
    OnComplete,
    /// Before each iteration
    PreIteration,
    /// After each iteration
    PostIteration,
}

impl HookTrigger {
    /// Get a human-readable name for this trigger.
    pub fn display_name(&self) -> &'static str {
        match self {
            Self::PreExecution => "Pre-Execution",
            Self::PostExecution => "Post-Execution",
            Self::OnError => "On Error",
            Self::OnVerificationFail => "On Verification Fail",
            Self::OnComplete => "On Complete",
            Self::PreIteration => "Pre-Iteration",
            Self::PostIteration => "Post-Iteration",
        }
    }

    /// Parse from string.
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "pre_execution" | "preexecution" => Some(Self::PreExecution),
            "post_execution" | "postexecution" => Some(Self::PostExecution),
            "on_error" | "onerror" => Some(Self::OnError),
            "on_verification_fail" | "onverificationfail" => Some(Self::OnVerificationFail),
            "on_complete" | "oncomplete" => Some(Self::OnComplete),
            "pre_iteration" | "preiteration" => Some(Self::PreIteration),
            "post_iteration" | "postiteration" => Some(Self::PostIteration),
            _ => None,
        }
    }
}

/// Routing decision payload.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[schemars(title = "RawRoutingDecisionPayload")]
pub struct RoutingDecisionPayload {
    /// The assessed complexity level
    pub complexity: TaskComplexity,
    /// Confidence in the assessment (0-1)
    pub confidence: f32,
    /// Factors that contributed to this assessment
    pub factors: Vec<String>,
    /// The model selected
    pub selected_model: String,
    /// Task prompt preview (truncated)
    #[schemars(with = "crate::schema_export::Nullable<String>")]
    pub prompt_preview: Option<String>,
    /// File count if analyzed
    #[schemars(with = "crate::schema_export::Nullable<usize>")]
    pub file_count: Option<usize>,
    /// Criteria count if analyzed
    #[schemars(with = "crate::schema_export::Nullable<usize>")]
    pub criteria_count: Option<usize>,
}

/// One retry attempt.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[schemars(title = "RawRetryAttemptPayload")]
pub struct RetryAttemptPayload {
    pub attempt_number: u32,
    pub error: String,
    pub attempt_timestamp: String,
    pub delay_ms: u64,
    pub feedback_injected: bool,
}

/// Accumulated retry state.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[schemars(title = "RawRetryStatePayload")]
pub struct RetryStatePayload {
    pub attempt: u32,
    #[schemars(with = "crate::schema_export::Nullable<String>")]
    pub last_error: Option<String>,
    #[schemars(with = "crate::schema_export::Nullable<String>")]
    pub last_attempt_at: Option<String>,
    pub total_delay_ms: u64,
    pub error_history: Vec<RetryAttemptPayload>,
}

/// Token count by memory category.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[schemars(title = "RawTokenCountPayload")]
pub struct TokenCountPayload {
    pub total: usize,
    pub findings: usize,
    pub observations: usize,
    pub feedback: usize,
    pub solutions: usize,
    pub other: usize,
    pub entry_count: usize,
}

/// Result of one memory-compression pass.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[schemars(title = "RawCompressionResultPayload")]
pub struct CompressionResultPayload {
    pub original_tokens: usize,
    pub compressed_tokens: usize,
    pub items_summarized: usize,
    pub summary_entries_created: usize,
    pub compressed_categories: Vec<String>,
    pub timestamp: String,
}

/// Result of one lifecycle-hook execution.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[schemars(title = "RawHookExecutionPayload")]
pub struct HookExecutionPayload {
    pub hook_id: String,
    pub hook_name: String,
    pub trigger: HookTrigger,
    pub success: bool,
    #[schemars(with = "crate::schema_export::Nullable<String>")]
    pub output: Option<String>,
    #[schemars(with = "crate::schema_export::Nullable<String>")]
    pub error: Option<String>,
    pub duration_ms: u64,
    pub timestamp: String,
}

/// `routing_decision` event body.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RoutingDecisionEvent {
    pub task_run_id: String,
    /// Unix timestamp in milliseconds
    pub timestamp: i64,
    pub decision: RoutingDecisionPayload,
}

/// `retry_attempt` event body.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RetryAttemptEvent {
    pub task_run_id: String,
    /// Unix timestamp in milliseconds
    pub timestamp: i64,
    pub attempt: RetryAttemptPayload,
    pub state: RetryStatePayload,
    pub exhausted: bool,
    #[schemars(with = "crate::schema_export::Nullable<u64>")]
    pub next_retry_delay_ms: Option<u64>,
}

/// `compression` event body.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CompressionEvent {
    pub task_run_id: String,
    /// Unix timestamp in milliseconds
    pub timestamp: i64,
    pub result: CompressionResultPayload,
    pub current_token_count: TokenCountPayload,
}

/// `token_count_update` event body.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TokenCountUpdateEvent {
    pub task_run_id: String,
    /// Unix timestamp in milliseconds
    pub timestamp: i64,
    pub token_count: TokenCountPayload,
    pub threshold_percentage: f32,
    pub compression_imminent: bool,
}

/// `hook_execution` event body.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct HookExecutionEvent {
    pub task_run_id: String,
    /// Unix timestamp in milliseconds
    pub timestamp: i64,
    pub result: HookExecutionPayload,
}

/// `hook_started` event body.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct HookStartedEvent {
    pub task_run_id: String,
    /// Unix timestamp in milliseconds
    pub timestamp: i64,
    pub hook_id: String,
    pub hook_name: String,
    pub trigger: HookTrigger,
}

/// `status_change` event body.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct StatusChangeEvent {
    pub task_run_id: String,
    /// Unix timestamp in milliseconds
    pub timestamp: i64,
    pub status: String,
    pub iteration: u32,
    #[schemars(with = "crate::schema_export::Nullable<String>")]
    pub task_name: Option<String>,
}

/// One event on the `execution-status` Tauri channel, tagged by `type`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
#[schemars(title = "RawExecutionStatusEvent")]
pub enum ExecutionStatusEvent {
    RoutingDecision(RoutingDecisionEvent),
    RetryAttempt(RetryAttemptEvent),
    Compression(CompressionEvent),
    TokenCountUpdate(TokenCountUpdateEvent),
    HookExecution(HookExecutionEvent),
    HookStarted(HookStartedEvent),
    StatusChange(StatusChangeEvent),
}

impl ExecutionStatusEvent {
    /// The wire value of the `type` tag.
    pub fn type_tag(&self) -> &'static str {
        match self {
            Self::RoutingDecision(_) => "routing_decision",
            Self::RetryAttempt(_) => "retry_attempt",
            Self::Compression(_) => "compression",
            Self::TokenCountUpdate(_) => "token_count_update",
            Self::HookExecution(_) => "hook_execution",
            Self::HookStarted(_) => "hook_started",
            Self::StatusChange(_) => "status_change",
        }
    }
}

#[cfg(test)]
mod execution_status_tests {
    use super::*;

    /// The enum must serialize byte-for-byte like the flattened
    /// `{type, task_run_id, timestamp, ...}` struct it replaced.
    #[test]
    fn status_change_wire_shape_is_unchanged() {
        let event = ExecutionStatusEvent::StatusChange(StatusChangeEvent {
            task_run_id: "tr-1".into(),
            timestamp: 42,
            status: "running".into(),
            iteration: 3,
            task_name: None,
        });
        assert_eq!(
            serde_json::to_string(&event).unwrap(),
            r#"{"type":"status_change","task_run_id":"tr-1","timestamp":42,"status":"running","iteration":3,"task_name":null}"#
        );
        assert_eq!(event.type_tag(), "status_change");
    }

    #[test]
    fn hook_trigger_serializes_as_the_former_string() {
        let event = ExecutionStatusEvent::HookStarted(HookStartedEvent {
            task_run_id: "tr-1".into(),
            timestamp: 1,
            hook_id: "h".into(),
            hook_name: "n".into(),
            trigger: HookTrigger::OnVerificationFail,
        });
        assert_eq!(
            serde_json::to_string(&event).unwrap(),
            r#"{"type":"hook_started","task_run_id":"tr-1","timestamp":1,"hook_id":"h","hook_name":"n","trigger":"on_verification_fail"}"#
        );
    }

    #[test]
    fn routing_decision_complexity_serializes_snake_case() {
        let json = serde_json::to_value(TaskComplexity::Complex).unwrap();
        assert_eq!(json, serde_json::json!("complex"));
    }
}

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

// ============================================================================
// review-approved / review-rejected payloads
// ============================================================================

/// Payload shape for the `review-approved` and `review-rejected` Tauri events
/// emitted by `commands::productivity::approve_recommendation` /
/// `reject_recommendation` after a user resolves a medium-confidence
/// recommendation card. Field names are explicit camelCase to match the
/// `serde_json::json!()` literal previously used at the emit site.
///
/// Single struct shared by both channels: only `user_decision` differs
/// (`"approved"` vs `"rejected"`), so a tagged enum would inflate the wire
/// format without payoff.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[schemars(title = "RecommendationReviewDecisionPayload")]
pub struct RecommendationReviewDecisionPayload {
    #[serde(rename = "reviewId")]
    pub review_id: String,
    #[serde(rename = "taskId")]
    pub task_id: String,
    #[serde(rename = "userDecision")]
    pub user_decision: String,
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
#[schemars(title = "RunnerFindingUserInput")]
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

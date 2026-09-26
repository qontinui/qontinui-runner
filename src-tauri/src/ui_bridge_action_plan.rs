//! Response wire types of the UI Bridge structured action-plan endpoint.
//!
//! `mcp::ui_bridge::ai::ui_bridge_execute_action_plan_handler` answers with an
//! [`ActionPlanResponse`] of [`PlannedActionResult`]s. They live in the lib so
//! the schema-export pipeline (`schema_export::export_all_schemas`) can see
//! them; the binary re-exports them. `ActionPlanResponse` is published under
//! the name the TypeScript mirror always used, `ActionPlanResult`
//! (`qontinui-schemas/ts/src/workflow/action-plan.ts`).
//!
//! Only the RESPONSE side is exported. The request structs
//! (`ActionPlanRequest`, `PlannedAction`, `ActionPlanElementTarget`) are
//! deserialize-only and every optional field carries `#[serde(default)]`; the
//! codegen promotes a defaulted field to REQUIRED, which is right for a value
//! the runner produces and wrong for one a caller writes, so those stay
//! hand-authored on the TypeScript side.
//!
//! The `Option<String>` fields skipped when `None` also carry
//! `#[serde(default)]` + `#[schemars(with = "String")]`: serde ignores
//! `default` on a serialize-only struct, but it tells the schema the key may be
//! ABSENT, so the binding reads `?: string` — what the wire does — rather than
//! `?: string | null`.

use schemars::JsonSchema;
use serde::Serialize;

/// Result of a single planned action execution.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PlannedActionResult {
    pub index: usize,
    pub success: bool,
    pub action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "String")]
    pub resolved_element_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "String")]
    pub error: Option<String>,
    #[serde(default)]
    pub skipped_low_confidence: bool,
    pub duration_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub element_state: Option<serde_json::Value>,
}

/// Aggregated result of executing a full action plan.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(title = "ActionPlanResult")]
pub struct ActionPlanResponse {
    pub success: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "String")]
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

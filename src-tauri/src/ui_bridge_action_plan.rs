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
#[schemars(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
pub struct PlannedActionResult {
    pub index: usize,
    pub success: bool,
    /// The request's `action` echoed back. The request accepts any string and
    /// the runner forwards an unrecognised one to the UI Bridge rather than
    /// rejecting it, so this is open — not the request-side
    /// `PlannedActionType` union.
    pub action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "String")]
    pub resolved_element_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "String")]
    pub error: Option<String>,
    pub skipped_low_confidence: bool,
    pub duration_ms: u64,
    /// The UI Bridge's post-action `elementState` object. Typed as a map so
    /// the schema's `Record<string, unknown>` is enforced by the type: a
    /// non-object value from the bridge (including `null`) is omitted rather
    /// than forwarded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "serde_json::Map<String, serde_json::Value>")]
    pub element_state: Option<serde_json::Map<String, serde_json::Value>>,
}

/// Aggregated result of executing a full action plan.
#[derive(Debug, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
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
    pub cached: bool,
}

/// The UI Bridge's post-action `elementState`, kept only when it is a JSON
/// object (see [`PlannedActionResult::element_state`]).
pub fn element_state_of(
    data: &serde_json::Value,
) -> Option<serde_json::Map<String, serde_json::Value>> {
    data.get("elementState")
        .and_then(|v| v.as_object())
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn element_state_keeps_only_an_object() {
        let obj = serde_json::json!({"elementState": {"checked": true}});
        assert_eq!(
            element_state_of(&obj).map(serde_json::Value::Object),
            Some(serde_json::json!({"checked": true}))
        );
        for v in [
            serde_json::Value::Null,
            serde_json::json!([1]),
            serde_json::json!("s"),
        ] {
            assert_eq!(
                element_state_of(&serde_json::json!({"elementState": v})),
                None
            );
        }
        assert_eq!(element_state_of(&serde_json::json!({})), None);
    }

    #[test]
    fn present_element_state_is_forwarded_as_an_object() {
        let r = PlannedActionResult {
            index: 1,
            success: true,
            action: "check".into(),
            resolved_element_id: Some("e1".into()),
            error: None,
            skipped_low_confidence: false,
            duration_ms: 2,
            element_state: element_state_of(
                &serde_json::json!({"elementState": {"checked": true}}),
            ),
        };
        assert_eq!(
            serde_json::to_string(&r).unwrap(),
            r#"{"index":1,"success":true,"action":"check","resolvedElementId":"e1","skippedLowConfidence":false,"durationMs":2,"elementState":{"checked":true}}"#
        );
    }

    #[test]
    fn absent_element_state_is_omitted_from_the_wire() {
        let r = PlannedActionResult {
            index: 0,
            success: true,
            action: "click".into(),
            resolved_element_id: None,
            error: None,
            skipped_low_confidence: false,
            duration_ms: 1,
            element_state: element_state_of(&serde_json::json!({"elementState": null})),
        };
        assert_eq!(
            serde_json::to_string(&r).unwrap(),
            r#"{"index":0,"success":true,"action":"click","skippedLowConfidence":false,"durationMs":1}"#
        );
    }
}

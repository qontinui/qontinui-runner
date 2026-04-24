//! Flatten the nested `{"retry": {"count": N, "delay_ms": M}}` form into
//! the flat sibling fields the executor expects:
//! `retry_count` / `retry_delay_ms`.
//!
//! Only applied when a nested `retry` object is present; leaves steps
//! that already use the flat form alone.

use serde_json::Value;

use crate::workflow_generation::hardener::rule::{HardenReport, HardenRule, RuleCtx, StepMut};

use super::common::emit_mutated;

pub struct FlattenRetryObject;

impl HardenRule for FlattenRetryObject {
    fn name(&self) -> &'static str {
        "flatten_retry_object"
    }

    fn apply(&self, step: &mut StepMut<'_>, _ctx: &RuleCtx<'_>, report: &mut HardenReport) {
        let raw = step.raw_mut();
        let retry_obj = match raw.get("retry").cloned() {
            Some(o) => o,
            None => return,
        };
        let Some(obj) = raw.as_object_mut() else {
            return;
        };
        if let Some(c) = retry_obj.get("count").and_then(|v| v.as_u64()) {
            obj.insert("retry_count".to_string(), Value::Number(c.into()));
        }
        if let Some(d) = retry_obj.get("delay_ms").and_then(|v| v.as_u64()) {
            obj.insert("retry_delay_ms".to_string(), Value::Number(d.into()));
        }
        obj.remove("retry");
        emit_mutated(
            report,
            "flatten_retry_object",
            "retry→retry_count/retry_delay_ms",
        );
    }
}

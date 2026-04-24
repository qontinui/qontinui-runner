//! Rewrite the common wrong health-check assertion
//! `d.get('status')=='ok'` to match the actual runner API response
//! shape `{"success": true, "data": "ok"}`.
//!
//! Applies only to commands that call `/health` and use `python` with
//! the wrong `status` field.

use serde_json::Value;
use tracing::info;

use crate::workflow_generation::hardener::rule::{HardenReport, HardenRule, RuleCtx, StepMut};

use super::common::{emit_mutated, pick_command_field};

pub struct FixHealthCheckAssertion;

impl HardenRule for FixHealthCheckAssertion {
    fn name(&self) -> &'static str {
        "fix_health_check_assertion"
    }

    fn apply(&self, step: &mut StepMut<'_>, _ctx: &RuleCtx<'_>, report: &mut HardenReport) {
        if step.command_replaced() {
            return;
        }
        let (cmd_key, _cmd) = match pick_command_field(step.raw()) {
            Some(pair) => pair,
            None => return,
        };
        let current = step
            .raw()
            .get(cmd_key)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        if !(current.contains("/health")
            && current.contains("python")
            && current.contains("d.get('status')"))
        {
            return;
        }
        let fixed = current.replace(
            "d.get('status')=='ok'",
            "d.get('success')==True and d.get('data')=='ok'",
        );
        if fixed == current {
            return;
        }
        if let Some(obj) = step.raw_mut().as_object_mut() {
            obj.insert(cmd_key.to_string(), Value::String(fixed));
            info!("Fixed incorrect health check assertion: status → success/data");
            emit_mutated(
                report,
                "fix_health_check_assertion",
                "d.get('status')==ok → d.get('success') and d.get('data')==ok",
            );
        }
    }
}

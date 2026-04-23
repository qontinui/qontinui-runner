//! Inject a missing `mode` field on `command` steps.
//!
//! The executor requires `mode` to be one of `shell`, `check`,
//! `check_group`, `test`. Builder output (and old workflows) occasionally
//! omit it, surfacing as `"invalid or missing mode: '<missing>'"` at run
//! time. Infer the intended mode from sibling fields:
//!   * `test_type` present         → `mode: "test"`
//!   * `check_type` present        → `mode: "check"`
//!   * `check_group` array present → `mode: "check_group"`
//!   * otherwise                   → `mode: "shell"`

use serde_json::Value;
use tracing::info;

use crate::workflow_generation::hardener::rule::{HardenReport, HardenRule, RuleCtx, StepMut};

use super::common::emit_mutated;

pub struct EnsureCommandMode;

impl HardenRule for EnsureCommandMode {
    fn name(&self) -> &'static str {
        "ensure_command_mode"
    }

    fn apply(&self, step: &mut StepMut<'_>, _ctx: &RuleCtx<'_>, report: &mut HardenReport) {
        let raw = step.raw_mut();
        let is_command_step = raw
            .get("type")
            .or_else(|| raw.get("step_type"))
            .and_then(|v| v.as_str())
            == Some("command");
        let has_mode = raw
            .get("mode")
            .or_else(|| raw.get("command_mode"))
            .and_then(|v| v.as_str())
            .map(|s| !s.is_empty())
            .unwrap_or(false);

        if !(is_command_step && !has_mode) {
            return;
        }

        let inferred = if raw.get("test_type").is_some() {
            "test"
        } else if raw
            .get("check_group")
            .map(|v| v.is_array())
            .unwrap_or(false)
        {
            "check_group"
        } else if raw.get("check_type").is_some() {
            "check"
        } else {
            "shell"
        };

        let step_name = raw
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("?")
            .to_string();

        if let Some(obj) = raw.as_object_mut() {
            obj.insert("mode".to_string(), Value::String(inferred.to_string()));
            info!(
                "Injected missing mode='{}' on command step '{}'",
                inferred, step_name
            );
            emit_mutated(
                report,
                "ensure_command_mode",
                format!("mode={}", inferred),
            );
        }
    }
}

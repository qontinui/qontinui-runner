//! Replace the bash negation prefix `! <cmd>` with explicit exit-code
//! handling.
//!
//! `! grep ...` is bash-specific and the Windows shell router may route
//! it to cmd.exe where it fails. Rewrites to:
//! ```text
//! <cmd> && exit 1 || exit 0
//! ```

use serde_json::Value;

use crate::workflow_generation::hardener::rule::{HardenReport, HardenRule, RuleCtx, StepMut};

use super::common::{emit_mutated, pick_command_field};

pub struct FixBashNegation;

impl HardenRule for FixBashNegation {
    fn name(&self) -> &'static str {
        "fix_bash_negation"
    }

    fn apply(&self, step: &mut StepMut<'_>, _ctx: &RuleCtx<'_>, report: &mut HardenReport) {
        if step.command_replaced() {
            return;
        }
        let (cmd_key, _cmd) = match pick_command_field(step.raw()) {
            Some(pair) => pair,
            None => return,
        };

        // Re-read the current command value — prior rules in this pipeline
        // may have mutated it.
        let current = step
            .raw()
            .get(&cmd_key as &str)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if !current.trim().starts_with("! ") {
            return;
        }
        let inner_cmd = current.trim().strip_prefix("! ").unwrap();
        let fixed = format!("{} && exit 1 || exit 0", inner_cmd);
        if let Some(obj) = step.raw_mut().as_object_mut() {
            obj.insert(cmd_key.to_string(), Value::String(fixed));
            emit_mutated(report, "fix_bash_negation", "! prefix → && exit/exit");
        }
    }
}

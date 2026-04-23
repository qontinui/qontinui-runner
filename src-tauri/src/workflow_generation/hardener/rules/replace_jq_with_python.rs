//! Replace `jq` commands with Python equivalents.
//!
//! `jq` is not available on Windows MSYS, so any command of the form
//! `curl ... | jq -e '<expr>'` is rewritten to use `python -c` with an
//! equivalent assertion.
//!
//! Handles four patterns:
//! * `.data | length > N` (SDK `/elements` response format)
//! * `.elements | length > N`
//! * `.total > N` (with SDK-elements special-case: maps to `len(data)`)
//! * `.results | length > N`
//!
//! After a successful replacement the rule calls
//! [`crate::workflow_generation::hardener::rule::StepMut::mark_command_replaced`]
//! so downstream command-string rules skip this step — mirroring the
//! `continue` statement in the original sanitizer loop.

use serde_json::Value;

use crate::workflow_generation::hardener::rule::{HardenReport, HardenRule, RuleCtx, StepMut};

use super::common::{emit_mutated, extract_number_threshold, pick_command_field};

pub struct ReplaceJqWithPython;

impl HardenRule for ReplaceJqWithPython {
    fn name(&self) -> &'static str {
        "replace_jq_with_python"
    }

    fn apply(&self, step: &mut StepMut<'_>, _ctx: &RuleCtx<'_>, report: &mut HardenReport) {
        if step.command_replaced() {
            return;
        }
        let (cmd_key, cmd) = match pick_command_field(step.raw()) {
            Some(pair) => pair,
            None => return,
        };

        if !cmd.contains("| jq ") {
            return;
        }
        let Some(fixed) = replace_jq_with_python(&cmd) else {
            return;
        };
        if let Some(obj) = step.raw_mut().as_object_mut() {
            obj.insert(cmd_key.to_string(), Value::String(fixed));
            emit_mutated(report, "replace_jq_with_python", "jq→python");
        }
        step.mark_command_replaced();
    }
}

/// Pure function: map a command string that pipes into `jq` to an
/// equivalent `python -c` assertion. Returns `None` if no pattern
/// matched.
pub fn replace_jq_with_python(cmd: &str) -> Option<String> {
    let pipe_idx = cmd.find("| jq ")?;
    let curl_part = cmd[..pipe_idx].trim();
    let jq_part = &cmd[pipe_idx + 5..];

    let jq_expr = jq_part
        .trim()
        .trim_start_matches("-e ")
        .trim()
        .trim_matches('"')
        .trim_matches('\'');

    let is_sdk_elements = curl_part.contains("ui-bridge/sdk/elements");

    if jq_expr.contains("data") && jq_expr.contains("length") {
        let threshold = extract_number_threshold(jq_expr).unwrap_or(0);
        return Some(format!(
            "{} | python -c \"import sys,json; d=json.load(sys.stdin); items=d.get('data',[]); assert len(items)>{}, 'Expected >{} items, got '+str(len(items))\"",
            curl_part, threshold, threshold
        ));
    }

    if jq_expr.contains("elements") && jq_expr.contains("length") {
        let threshold = extract_number_threshold(jq_expr).unwrap_or(0);
        if is_sdk_elements {
            return Some(format!(
                "{} | python -c \"import sys,json; d=json.load(sys.stdin); elems=d.get('data',[]); assert len(elems)>{}, 'Expected >{} elements, got '+str(len(elems))\"",
                curl_part, threshold, threshold
            ));
        }
        return Some(format!(
            "{} | python -c \"import sys,json; d=json.load(sys.stdin); elems=d.get('elements',d.get('data',[])); assert len(elems)>{}, 'Expected >{} elements, got '+str(len(elems))\"",
            curl_part, threshold, threshold
        ));
    }

    if jq_expr.contains("total") {
        let threshold = extract_number_threshold(jq_expr).unwrap_or(0);
        if is_sdk_elements {
            return Some(format!(
                "{} | python -c \"import sys,json; d=json.load(sys.stdin); items=d.get('data',[]); assert len(items)>{}, 'Expected >{} elements, got '+str(len(items))\"",
                curl_part, threshold, threshold
            ));
        }
        return Some(format!(
            "{} | python -c \"import sys,json; d=json.load(sys.stdin); t=d.get('total',len(d.get('data',[]))); assert t>{}, 'Expected total>{}, got '+str(t)\"",
            curl_part, threshold, threshold
        ));
    }

    if jq_expr.contains("results") && jq_expr.contains("length") {
        let threshold = extract_number_threshold(jq_expr).unwrap_or(0);
        return Some(format!(
            "{} | python -c \"import sys,json; d=json.load(sys.stdin); r=d.get('results',[]); assert len(r)>{}, 'Expected >{} results, got '+str(len(r))\"",
            curl_part, threshold, threshold
        ));
    }

    None
}

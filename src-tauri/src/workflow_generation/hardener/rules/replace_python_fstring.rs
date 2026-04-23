//! Rewrite Python f-string assertions to plain concatenation so they
//! don't get mangled by shell escape handling.
//!
//! Targets commands of the form:
//! ```text
//! curl ... | python -c "... json.load ... f'...'"
//! ```
//! and rewrites the python portion to an equivalent non-f-string form.

use serde_json::Value;

use crate::workflow_generation::hardener::rule::{HardenReport, HardenRule, RuleCtx, StepMut};

use super::common::{emit_mutated, extract_number_threshold, pick_command_field};

pub struct ReplacePythonFstring;

impl HardenRule for ReplacePythonFstring {
    fn name(&self) -> &'static str {
        "replace_python_fstring"
    }

    fn apply(&self, step: &mut StepMut<'_>, _ctx: &RuleCtx<'_>, report: &mut HardenReport) {
        if step.command_replaced() {
            return;
        }
        let (cmd_key, cmd) = match pick_command_field(step.raw()) {
            Some(pair) => pair,
            None => return,
        };

        if !(cmd.contains("python -c")
            && cmd.contains("json.load")
            && (cmd.contains("f'") || cmd.contains("f\"")))
        {
            return;
        }
        let Some(fixed) = replace_python_fstring_with_clean(&cmd) else {
            return;
        };
        if let Some(obj) = step.raw_mut().as_object_mut() {
            obj.insert(cmd_key.to_string(), Value::String(fixed));
            emit_mutated(report, "replace_python_fstring", "fstring→plain");
        }
    }
}

/// Rewrite a curl|python command containing f-strings to a cleaner
/// non-f-string equivalent. Returns `None` if no pattern matches.
pub fn replace_python_fstring_with_clean(cmd: &str) -> Option<String> {
    let pipe_idx = cmd.find("| python")?;
    let curl_part = cmd[..pipe_idx].trim();
    let python_part = &cmd[pipe_idx..];

    let is_sdk_elements = curl_part.contains("ui-bridge/sdk/elements");

    if python_part.contains("elements") && python_part.contains("len(") {
        let threshold = extract_number_threshold(python_part).unwrap_or(0);
        let key = if is_sdk_elements { "data" } else { "elements" };
        return Some(format!(
            "{} | python -c \"import sys,json; d=json.load(sys.stdin); elems=d.get('{}',d.get('data',[])); assert len(elems)>{}, 'Expected >{} elements, got '+str(len(elems))\"",
            curl_part, key, threshold, threshold
        ));
    }

    if python_part.contains("total") {
        let threshold = extract_number_threshold(python_part).unwrap_or(0);
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

    None
}

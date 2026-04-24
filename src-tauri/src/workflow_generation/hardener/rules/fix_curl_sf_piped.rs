//! Remove the `-f` flag from `curl -sf` (and variants like `-fs`, `-sfL`)
//! when the output is piped to another program.
//!
//! `-f` suppresses all output on HTTP 4xx/5xx, causing the downstream
//! command (python, grep) to receive empty stdin and fail with a confusing
//! error. When piping, keep `-s` but drop `-f`.

use serde_json::Value;

use crate::workflow_generation::hardener::rule::{HardenReport, HardenRule, RuleCtx, StepMut};

use super::common::{emit_mutated, pick_command_field};

pub struct FixCurlSfInPiped;

impl HardenRule for FixCurlSfInPiped {
    fn name(&self) -> &'static str {
        "fix_curl_sf_piped"
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
        if !current.contains("| ") {
            return;
        }
        let fixed = fix_curl_sf_in_piped_commands(&current);
        if fixed == current {
            return;
        }
        if let Some(obj) = step.raw_mut().as_object_mut() {
            obj.insert(cmd_key.to_string(), Value::String(fixed));
            emit_mutated(report, "fix_curl_sf_piped", "stripped -f from curl flags");
        }
    }
}

/// Pure function: drop the `-f` flag from the first `curl -<flags>`
/// invocation in a piped command.
pub fn fix_curl_sf_in_piped_commands(cmd: &str) -> String {
    if !cmd.contains("curl ") || !cmd.contains("| ") {
        return cmd.to_string();
    }

    let mut result = cmd.to_string();
    let mut changed = false;

    for (i, _) in cmd.match_indices("curl ") {
        let after_curl = &cmd[i + 5..];
        if let Some(flags_start) = after_curl.find('-') {
            let flags_abs = i + 5 + flags_start;
            let flags_end = after_curl[flags_start..]
                .find(' ')
                .map(|p| flags_abs + p)
                .unwrap_or(cmd.len());
            let flags = &cmd[flags_abs..flags_end];

            if flags.starts_with('-') && !flags.starts_with("--") && flags.contains('f') {
                let new_flags: String = flags.chars().filter(|c| *c != 'f').collect();
                if new_flags == "-" {
                    result = format!("{}-s{}", &result[..flags_abs], &result[flags_end..]);
                } else {
                    result = format!(
                        "{}{}{}",
                        &result[..flags_abs],
                        new_flags,
                        &result[flags_end..]
                    );
                }
                changed = true;
                break;
            }
        }
    }

    if changed {
        result
    } else {
        cmd.to_string()
    }
}

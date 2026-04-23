//! Helpers shared by multiple per-step rules.
//!
//! These mirror the little utility code that was inlined in the top of
//! `sanitize_commands_in_steps`: picking the right command field key
//! (`command` vs `shell_command`) and building a `RuleEvent::Mutated`.

use serde_json::Value;

use crate::workflow_generation::hardener::rule::{HardenReport, RuleEvent};

/// Pick the command field key + current value off a step. Returns
/// `(key, value)` or `None` if the step has neither field.
///
/// Keys are checked in the same order as the original code:
/// `"command"` first, then `"shell_command"`.
pub fn pick_command_field(step: &Value) -> Option<(&'static str, String)> {
    if let Some(c) = step.get("command").and_then(|v| v.as_str()) {
        return Some(("command", c.to_string()));
    }
    if let Some(c) = step.get("shell_command").and_then(|v| v.as_str()) {
        return Some(("shell_command", c.to_string()));
    }
    None
}

/// Push a standard [`RuleEvent::Mutated`] event onto the report.
pub fn emit_mutated(report: &mut HardenReport, rule: &'static str, detail: impl Into<String>) {
    report.push(RuleEvent::Mutated {
        rule,
        detail: detail.into(),
    });
}

/// Extract a numeric threshold from an assertion string like "len(...)>2"
/// or "... > 0". Shared by the jq→python and python-fstring rules.
pub fn extract_number_threshold(s: &str) -> Option<u32> {
    let bytes = s.as_bytes();
    for i in 0..bytes.len() {
        if bytes[i] == b'>' && i + 1 < bytes.len() {
            let rest = s[i + 1..].trim_start();
            if let Some(end) = rest.find(|c: char| !c.is_ascii_digit()) {
                if let Ok(n) = rest[..end].parse() {
                    return Some(n);
                }
            } else if let Ok(n) = rest.parse() {
                return Some(n);
            }
        }
    }
    None
}

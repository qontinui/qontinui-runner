//! Wrap unquoted `curl` URLs that contain `&` in double quotes so bash
//! doesn't interpret `&` as a background operator.
//!
//! For example, this:
//! ```text
//! curl -sf http://host/api?a=1&b=2 | grep x
//! ```
//! becomes:
//! ```text
//! curl -sf "http://host/api?a=1&b=2" | grep x
//! ```

use serde_json::Value;

use crate::workflow_generation::hardener::rule::{HardenReport, HardenRule, RuleCtx, StepMut};

use super::common::{emit_mutated, pick_command_field};

pub struct QuoteCurlAmpersandUrls;

impl HardenRule for QuoteCurlAmpersandUrls {
    fn name(&self) -> &'static str {
        "quote_curl_ampersand_urls"
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
            .get(&cmd_key as &str)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let Some(fixed) = quote_curl_urls_with_ampersand(&current) else {
            return;
        };
        if let Some(obj) = step.raw_mut().as_object_mut() {
            obj.insert(cmd_key.to_string(), Value::String(fixed));
            emit_mutated(report, "quote_curl_ampersand_urls", "quoted URL with &");
        }
    }
}

/// Pure function: wrap http(s) URLs containing `?` AND `&` in double
/// quotes when they're not already quoted. Returns `None` if nothing to
/// fix.
pub fn quote_curl_urls_with_ampersand(cmd: &str) -> Option<String> {
    if !cmd.contains("curl ") || !cmd.contains('&') {
        return None;
    }

    let mut result = cmd.to_string();
    let mut changed = false;

    let url_prefixes = ["http://", "https://"];
    for prefix in &url_prefixes {
        let mut search_from = 0;
        loop {
            let Some(url_start) = result[search_from..].find(prefix).map(|i| i + search_from)
            else {
                break;
            };

            if url_start > 0 {
                let prev_char = result.as_bytes()[url_start - 1];
                if prev_char == b'"' || prev_char == b'\'' {
                    search_from = url_start + prefix.len();
                    continue;
                }
            }

            let url_end = result[url_start..]
                .find(|c: char| c.is_whitespace() || c == '|' || c == ';' || c == ')' || c == '\'')
                .map(|i| i + url_start)
                .unwrap_or(result.len());

            let url = &result[url_start..url_end];
            if url.contains('?') && url.contains('&') {
                let quoted = format!("\"{}\"", url);
                result = format!("{}{}{}", &result[..url_start], quoted, &result[url_end..]);
                changed = true;
                search_from = url_start + quoted.len();
            } else {
                search_from = url_end;
            }
        }
    }

    if changed {
        Some(result)
    } else {
        None
    }
}

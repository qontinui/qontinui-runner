//! The coord PR-hold contract the runner reads, in ONE place (plan
//! `2026-10-04-coord-pr-hold-without-github-drafts`). Both the loopback
//! forwarder (`mcp_api::forward_vcs_pr_post`) and the `qontinui-pr` CLI decide
//! "did coord confirm the hold I asked for?" with [`hold_echo_confirmed`], and
//! the forwarder's pre-create capability check reads coord's `tools/list` with
//! [`tools_list_serves`]. Keeping them here means the two consumers cannot
//! drift apart.

/// The coord MCP tool whose presence in coord's `tools/list` means coord
/// serves PR holds at all.
pub const COORD_SET_PR_HOLD_TOOL: &str = "coord_set_pr_hold";

/// Whether a coord PR-create success body CONFIRMS a requested hold: a
/// top-level `hold` object carrying non-empty string `kind`, `reason` and
/// `held_by`. A coord that predates holds ignores the request's `hold` and
/// answers a plain 2xx; `hold: null` means "not held". Both read false.
pub fn hold_echo_confirmed(v: &serde_json::Value) -> bool {
    v.get("hold").and_then(|h| h.as_object()).is_some_and(|h| {
        ["kind", "reason", "held_by"].iter().all(|k| {
            h.get(*k)
                .and_then(|x| x.as_str())
                .is_some_and(|x| !x.trim().is_empty())
        })
    })
}

/// Read a coord `/mcp` `tools/list` answer and say whether it lists `tool`.
///
/// `Some(true)`/`Some(false)` only for a well-formed answer carrying a
/// `result.tools` array; `None` for anything else (an error object, an
/// unparseable body, a body with no tools array) — UNKNOWN, never "absent".
/// Accepts a plain JSON body or a streamable-HTTP `text/event-stream` body,
/// whose `data:` lines carry the JSON-RPC message.
pub fn tools_list_serves(body: &[u8], tool: &str) -> Option<bool> {
    let text = std::str::from_utf8(body).ok()?;
    let from_json = |s: &str| -> Option<bool> {
        let v: serde_json::Value = serde_json::from_str(s.trim()).ok()?;
        let tools = v.pointer("/result/tools")?.as_array()?;
        Some(
            tools
                .iter()
                .any(|t| t.get("name").and_then(|n| n.as_str()) == Some(tool)),
        )
    };
    if let Some(found) = from_json(text) {
        return Some(found);
    }
    text.lines()
        .filter_map(|l| l.strip_prefix("data:"))
        .find_map(|d| from_json(d))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_echo_needs_non_empty_kind_reason_and_held_by() {
        let ok = serde_json::json!({"url":"u","hold":{"kind":"create","reason":"r","held_by":"agent:x","created":true}});
        assert!(hold_echo_confirmed(&ok));
        for bad in [
            serde_json::json!({"url":"u"}),
            serde_json::json!({"url":"u","hold":null}),
            serde_json::json!({"url":"u","hold":{"kind":"create","reason":"","held_by":"a"}}),
            serde_json::json!({"url":"u","hold":{"kind":"create","reason":"r"}}),
            serde_json::json!({"url":"u","hold":{"kind":1,"reason":"r","held_by":"a"}}),
        ] {
            assert!(!hold_echo_confirmed(&bad), "{bad}");
        }
    }

    #[test]
    fn tools_list_reads_json_and_sse_and_is_unknown_otherwise() {
        let served = br#"{"jsonrpc":"2.0","id":1,"result":{"tools":[{"name":"coord_orient"},{"name":"coord_set_pr_hold"}]}}"#;
        let absent = br#"{"jsonrpc":"2.0","id":1,"result":{"tools":[{"name":"coord_orient"}]}}"#;
        assert_eq!(
            tools_list_serves(served, COORD_SET_PR_HOLD_TOOL),
            Some(true)
        );
        assert_eq!(
            tools_list_serves(absent, COORD_SET_PR_HOLD_TOOL),
            Some(false)
        );
        let sse = format!(
            "event: message\ndata: {}\n\n",
            std::str::from_utf8(served).unwrap()
        );
        assert_eq!(
            tools_list_serves(sse.as_bytes(), COORD_SET_PR_HOLD_TOOL),
            Some(true)
        );
        for unknown in [
            &br#"{"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"no"}}"#[..],
            b"not json",
            b"",
            br#"{"result":{}}"#,
        ] {
            assert_eq!(tools_list_serves(unknown, COORD_SET_PR_HOLD_TOOL), None);
        }
    }
}

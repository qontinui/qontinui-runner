//! `session/update` -> the runner's transcript line, and usage events.
//!
//! The v1 `SessionUpdate` enum is typed and `#[non_exhaustive]`, so an update
//! kind this crate version does not know cannot be deserialized. That must not
//! drop the update: it is preserved **verbatim** as an `unknown_update`
//! transcript line (the whole notification params, untouched), and counted.
//! The same holds for a notification whose method is not `session/update` at
//! all, and for a `session/update` whose params fail to parse.

use agent_client_protocol::v1::schema::{ContentBlock, SessionNotification, SessionUpdate};
use serde_json::{json, Value};

/// The wire method name of the update notification.
pub(crate) const SESSION_UPDATE_METHOD: &str = "session/update";

/// What `usage_update` reports: how much of the agent's context window is in
/// use, plus an optional cost the agent computed itself.
///
/// This is deliberately NOT per-model input/output tokens, so it cannot be fed
/// to `ai_pricing` (which prices tokens by model id). It is carried as-is and is
/// UNKNOWN for pricing purposes.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct AcpUsage {
    /// Tokens currently in context.
    pub used: u64,
    /// Total context window size in tokens.
    pub size: u64,
    /// `(amount, currency)` when the agent reports a cost.
    pub cost: Option<(f64, String)>,
}

/// One mapped notification.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct MappedUpdate {
    /// The JSONL transcript line (one JSON object).
    pub line: Value,
    /// Set for a `usage_update`.
    pub usage: Option<AcpUsage>,
    /// True when the notification could not be typed and was preserved raw.
    pub unknown: bool,
    /// The text of an `agent_message_chunk`, for the run log.
    pub agent_text: Option<String>,
}

fn unknown_line(method: &str, params: &Value, why: &str) -> MappedUpdate {
    MappedUpdate {
        line: json!({
            "type": "unknown_update",
            "method": method,
            "why": why,
            "raw": params,
        }),
        usage: None,
        unknown: true,
        agent_text: None,
    }
}

/// Map one inbound notification (`method` + raw `params`) to a transcript line.
/// Never fails and never drops: anything unrecognised becomes `unknown_update`.
pub(crate) fn map_notification(method: &str, params: &Value) -> MappedUpdate {
    if method != SESSION_UPDATE_METHOD {
        return unknown_line(method, params, "not a session/update notification");
    }
    let parsed: SessionNotification = match serde_json::from_value(params.clone()) {
        Ok(n) => n,
        Err(e) => return unknown_line(method, params, &format!("unparseable session/update: {e}")),
    };
    // The kind tag, read off the raw update so it is the wire spelling.
    let kind = params
        .pointer("/update/sessionUpdate")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();
    let mut usage = None;
    let mut agent_text = None;
    match &parsed.update {
        SessionUpdate::UsageUpdate(u) => {
            usage = Some(AcpUsage {
                used: u.used,
                size: u.size,
                cost: u.cost.as_ref().map(|c| (c.amount, c.currency.clone())),
            });
        }
        SessionUpdate::AgentMessageChunk(chunk) => {
            if let ContentBlock::Text(t) = &chunk.content {
                agent_text = Some(t.text.clone());
            }
        }
        _ => {}
    }
    MappedUpdate {
        line: json!({
            "type": "acp_update",
            "kind": kind,
            "session_id": parsed.session_id.to_string(),
            "update": params.get("update").cloned().unwrap_or(Value::Null),
        }),
        usage,
        unknown: false,
        agent_text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_known_update_maps_to_a_typed_line_with_its_kind() {
        let params = json!({
            "sessionId": "s1",
            "update": {
                "sessionUpdate": "agent_message_chunk",
                "content": {"type": "text", "text": "hello"}
            }
        });
        let m = map_notification("session/update", &params);
        assert!(!m.unknown);
        assert_eq!(m.line["type"], "acp_update");
        assert_eq!(m.line["kind"], "agent_message_chunk");
        assert_eq!(m.line["session_id"], "s1");
        assert_eq!(m.agent_text.as_deref(), Some("hello"));
    }

    #[test]
    fn a_usage_update_is_extracted() {
        let params = json!({
            "sessionId": "s1",
            "update": {
                "sessionUpdate": "usage_update",
                "used": 1200, "size": 200000,
                "cost": {"amount": 0.5, "currency": "USD"}
            }
        });
        let m = map_notification("session/update", &params);
        assert_eq!(
            m.usage,
            Some(AcpUsage {
                used: 1200,
                size: 200_000,
                cost: Some((0.5, "USD".to_string()))
            })
        );
    }

    #[test]
    fn an_unknown_update_kind_is_preserved_verbatim() {
        let params = json!({
            "sessionId": "s1",
            "update": {"sessionUpdate": "quantum_flux", "x": [1, 2, 3]}
        });
        let m = map_notification("session/update", &params);
        assert!(m.unknown);
        assert_eq!(m.line["type"], "unknown_update");
        assert_eq!(m.line["raw"], params, "the whole params ride verbatim");
    }

    #[test]
    fn a_foreign_notification_method_is_preserved_not_dropped() {
        let params = json!({"anything": true});
        let m = map_notification("vendor/telemetry", &params);
        assert!(m.unknown);
        assert_eq!(m.line["method"], "vendor/telemetry");
        assert_eq!(m.line["raw"], params);
    }
}

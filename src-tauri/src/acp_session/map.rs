//! `session/update` → the runner's transcript line type, usage events, and the
//! prompt stop reason. PURE: nothing here does I/O, so every mapping is tested
//! directly.
//!
//! The v1 `SessionUpdate` enum is typed and `#[non_exhaustive]`, and serde
//! refuses a `sessionUpdate` tag it does not know. An agent built against a
//! newer schema can therefore send an update this runner cannot parse. Such an
//! update is never dropped: [`map_notification`] keeps it verbatim as
//! [`TranscriptLine::UnknownUpdate`].

use agent_client_protocol::v1::schema::{
    ContentBlock, ContentChunk, SessionNotification, SessionUpdate,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The JSON-RPC method that carries every session update.
pub(crate) const SESSION_UPDATE_METHOD: &str = "session/update";

/// One line of an ACP session transcript. Serializes as one JSON object per
/// line (JSONL), tagged by `type`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum TranscriptLine {
    /// A chunk of the user's message, echoed by the agent.
    UserMessageChunk {
        session_id: String,
        /// The chunk's text when it is a text block.
        text: Option<String>,
        /// The full content block, verbatim.
        content: Value,
    },
    /// A chunk of the agent's reply.
    AgentMessageChunk {
        session_id: String,
        text: Option<String>,
        content: Value,
    },
    /// A chunk of the agent's reasoning.
    AgentThoughtChunk {
        session_id: String,
        text: Option<String>,
        content: Value,
    },
    /// A tool call started.
    ToolCall {
        session_id: String,
        tool_call_id: String,
        title: String,
        status: String,
        /// The whole `tool_call` update, verbatim.
        raw: Value,
    },
    /// A tool call's status or result changed.
    ToolCallUpdate {
        session_id: String,
        tool_call_id: String,
        status: Option<String>,
        raw: Value,
    },
    /// Context-window and cost report (`usage_update`).
    Usage { session_id: String, usage: AcpUsage },
    /// A recognised update with no dedicated line (plan, available commands,
    /// mode, config option, session info, notice, compaction, ...), verbatim.
    SessionUpdate {
        session_id: String,
        /// The update's `sessionUpdate` tag.
        kind: String,
        raw: Value,
    },
    /// The runner's answer to a `session/request_permission`.
    Permission {
        session_id: String,
        tool_call_id: String,
        title: Option<String>,
        decision: PermissionDecision,
        /// The option the runner selected; `None` when it answered `cancelled`.
        option_id: Option<String>,
    },
    /// A notification this runner could not parse: an unknown `sessionUpdate`
    /// kind, a malformed `session/update`, or a notification method it does not
    /// handle. Kept verbatim so nothing the agent said is lost.
    UnknownUpdate { method: String, raw: Value },
}

/// What the runner answered to one permission request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PermissionDecision {
    Allow,
    Deny,
    /// No option of the needed kind was offered, so the runner answered
    /// `cancelled` rather than pick an option of the opposite meaning.
    Cancelled,
}

/// One `usage_update`: tokens in context, window size, optional cost.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct AcpUsage {
    pub used: u64,
    pub size: u64,
    /// Cumulative session cost when the agent reports one; `None` is UNKNOWN,
    /// not zero.
    pub cost: Option<AcpCost>,
}

/// A cost figure as the agent reported it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct AcpCost {
    pub amount: f64,
    pub currency: String,
}

/// Why a `session/prompt` turn ended, in the runner's vocabulary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AcpStopReason {
    EndTurn,
    MaxTokens,
    MaxTurnRequests,
    Refusal,
    Cancelled,
    /// The agent's transport closed (the child exited, or its stdio closed)
    /// before the prompt was answered. Never sent by an agent; the runner
    /// observed it.
    TransportLost,
    /// A `stopReason` value outside ACP v1, kept verbatim.
    Unrecognised(String),
}

impl AcpStopReason {
    /// The reason as one token, for a `work_unreported: acp_stop_reason=<reason>`
    /// detail: the ACP wire value for the protocol's reasons, `transport_lost`
    /// for the runner's own, and the raw value for an unrecognised one.
    pub(crate) fn as_token(&self) -> &str {
        match self {
            Self::EndTurn => "end_turn",
            Self::MaxTokens => "max_tokens",
            Self::MaxTurnRequests => "max_turn_requests",
            Self::Refusal => "refusal",
            Self::Cancelled => "cancelled",
            Self::TransportLost => "transport_lost",
            Self::Unrecognised(raw) => raw,
        }
    }
}

/// Map a wire `stopReason` to [`AcpStopReason`].
pub(crate) fn stop_reason_from_wire(raw: &str) -> AcpStopReason {
    match raw {
        "end_turn" => AcpStopReason::EndTurn,
        "max_tokens" => AcpStopReason::MaxTokens,
        "max_turn_requests" => AcpStopReason::MaxTurnRequests,
        "refusal" => AcpStopReason::Refusal,
        "cancelled" => AcpStopReason::Cancelled,
        other => AcpStopReason::Unrecognised(other.to_string()),
    }
}

/// Map one incoming notification to a transcript line. Never fails: anything
/// that is not a parseable `session/update` becomes
/// [`TranscriptLine::UnknownUpdate`] with the params verbatim.
pub(crate) fn map_notification(method: &str, params: &Value) -> TranscriptLine {
    let unknown = || TranscriptLine::UnknownUpdate {
        method: method.to_string(),
        raw: params.clone(),
    };
    if method != SESSION_UPDATE_METHOD {
        return unknown();
    }
    let Ok(notification) = serde_json::from_value::<SessionNotification>(params.clone()) else {
        return unknown();
    };
    let session_id = notification.session_id.0.to_string();
    // The update exactly as the agent sent it, for the lines that keep `raw`.
    let raw_update = params.get("update").cloned().unwrap_or(Value::Null);
    match notification.update {
        SessionUpdate::UserMessageChunk(chunk) => {
            let (text, content) = chunk_parts(&chunk);
            TranscriptLine::UserMessageChunk {
                session_id,
                text,
                content,
            }
        }
        SessionUpdate::AgentMessageChunk(chunk) => {
            let (text, content) = chunk_parts(&chunk);
            TranscriptLine::AgentMessageChunk {
                session_id,
                text,
                content,
            }
        }
        SessionUpdate::AgentThoughtChunk(chunk) => {
            let (text, content) = chunk_parts(&chunk);
            TranscriptLine::AgentThoughtChunk {
                session_id,
                text,
                content,
            }
        }
        SessionUpdate::ToolCall(call) => TranscriptLine::ToolCall {
            session_id,
            tool_call_id: call.tool_call_id.0.to_string(),
            title: call.title.clone(),
            status: wire_token(&call.status),
            raw: raw_update,
        },
        SessionUpdate::ToolCallUpdate(update) => TranscriptLine::ToolCallUpdate {
            session_id,
            tool_call_id: update.tool_call_id.0.to_string(),
            status: update.fields.status.as_ref().map(wire_token),
            raw: raw_update,
        },
        SessionUpdate::UsageUpdate(usage) => TranscriptLine::Usage {
            session_id,
            usage: AcpUsage {
                used: usage.used,
                size: usage.size,
                cost: usage.cost.map(|c| AcpCost {
                    amount: c.amount,
                    currency: c.currency,
                }),
            },
        },
        // Every other recognised kind (plan, commands, mode, config, info,
        // notice, compaction, and any variant a future schema minor adds) is
        // kept whole under its own tag.
        _ => TranscriptLine::SessionUpdate {
            session_id,
            kind: raw_update
                .get("sessionUpdate")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string(),
            raw: raw_update,
        },
    }
}

/// The usage events in a run of transcript lines, in order.
pub(crate) fn usage_events(lines: &[TranscriptLine]) -> Vec<AcpUsage> {
    lines
        .iter()
        .filter_map(|line| match line {
            TranscriptLine::Usage { usage, .. } => Some(usage.clone()),
            _ => None,
        })
        .collect()
}

/// A content chunk's text (for a text block) and its full block, verbatim.
fn chunk_parts(chunk: &ContentChunk) -> (Option<String>, Value) {
    let text = match &chunk.content {
        ContentBlock::Text(t) => Some(t.text.clone()),
        _ => None,
    };
    let content = serde_json::to_value(&chunk.content).unwrap_or(Value::Null);
    (text, content)
}

/// A serde unit-variant enum's wire token (`"in_progress"`), or its JSON text
/// when it does not serialize to a string.
fn wire_token<T: Serialize>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(Value::String(s)) => s,
        Ok(other) => other.to_string(),
        Err(_) => "unknown".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn update(update: Value) -> Value {
        json!({ "sessionId": "s-1", "update": update })
    }

    #[test]
    fn acp_session_map_agent_message_chunk_keeps_text_and_block() {
        let line = map_notification(
            SESSION_UPDATE_METHOD,
            &update(json!({
                "sessionUpdate": "agent_message_chunk",
                "content": { "type": "text", "text": "hello" }
            })),
        );
        match line {
            TranscriptLine::AgentMessageChunk {
                session_id,
                text,
                content,
            } => {
                assert_eq!(session_id, "s-1");
                assert_eq!(text.as_deref(), Some("hello"));
                assert_eq!(content["text"], "hello");
            }
            other => panic!("expected agent_message_chunk, got {other:?}"),
        }
    }

    #[test]
    fn acp_session_map_tool_call_and_update() {
        let call = map_notification(
            SESSION_UPDATE_METHOD,
            &update(json!({
                "sessionUpdate": "tool_call",
                "toolCallId": "t-1",
                "title": "Read file",
                "kind": "read",
                "status": "pending"
            })),
        );
        assert!(matches!(
            &call,
            TranscriptLine::ToolCall { tool_call_id, title, status, .. }
                if tool_call_id == "t-1" && title == "Read file" && status == "pending"
        ));
        let upd = map_notification(
            SESSION_UPDATE_METHOD,
            &update(json!({
                "sessionUpdate": "tool_call_update",
                "toolCallId": "t-1",
                "status": "completed"
            })),
        );
        assert!(matches!(
            &upd,
            TranscriptLine::ToolCallUpdate { tool_call_id, status, .. }
                if tool_call_id == "t-1" && status.as_deref() == Some("completed")
        ));
    }

    #[test]
    fn acp_session_map_usage_update_becomes_usage_event() {
        let line = map_notification(
            SESSION_UPDATE_METHOD,
            &update(json!({
                "sessionUpdate": "usage_update",
                "used": 1200,
                "size": 200000,
                "cost": { "amount": 0.42, "currency": "USD" }
            })),
        );
        let events = usage_events(std::slice::from_ref(&line));
        assert_eq!(
            events,
            vec![AcpUsage {
                used: 1200,
                size: 200_000,
                cost: Some(AcpCost {
                    amount: 0.42,
                    currency: "USD".into()
                }),
            }]
        );
    }

    #[test]
    fn acp_session_map_recognised_kind_without_line_is_kept_whole() {
        let raw = json!({
            "sessionUpdate": "current_mode_update",
            "currentModeId": "code"
        });
        let line = map_notification(SESSION_UPDATE_METHOD, &update(raw.clone()));
        assert_eq!(
            line,
            TranscriptLine::SessionUpdate {
                session_id: "s-1".into(),
                kind: "current_mode_update".into(),
                raw,
            }
        );
    }

    #[test]
    fn acp_session_map_unknown_update_kind_is_preserved_verbatim() {
        let params = update(json!({ "sessionUpdate": "hologram_update", "beam": [1, 2, 3] }));
        let line = map_notification(SESSION_UPDATE_METHOD, &params);
        assert_eq!(
            line,
            TranscriptLine::UnknownUpdate {
                method: SESSION_UPDATE_METHOD.into(),
                raw: params,
            }
        );
    }

    #[test]
    fn acp_session_map_other_notification_method_is_preserved_verbatim() {
        let params = json!({ "anything": true });
        let line = map_notification("_vendor/ping", &params);
        assert_eq!(
            line,
            TranscriptLine::UnknownUpdate {
                method: "_vendor/ping".into(),
                raw: params,
            }
        );
    }

    #[test]
    fn acp_session_map_transcript_line_is_one_jsonl_object() {
        let line = TranscriptLine::UnknownUpdate {
            method: "m".into(),
            raw: json!({"a": "b\nc"}),
        };
        let text = serde_json::to_string(&line).unwrap();
        assert!(!text.contains('\n'), "a JSONL line must not span lines");
        assert_eq!(serde_json::from_str::<TranscriptLine>(&text).unwrap(), line);
        assert!(text.contains("\"type\":\"unknown_update\""));
    }

    #[test]
    fn acp_session_map_stop_reasons() {
        for (wire, expected) in [
            ("end_turn", AcpStopReason::EndTurn),
            ("max_tokens", AcpStopReason::MaxTokens),
            ("max_turn_requests", AcpStopReason::MaxTurnRequests),
            ("refusal", AcpStopReason::Refusal),
            ("cancelled", AcpStopReason::Cancelled),
        ] {
            let mapped = stop_reason_from_wire(wire);
            assert_eq!(mapped, expected);
            assert_eq!(mapped.as_token(), wire);
        }
        assert_eq!(
            stop_reason_from_wire("paused"),
            AcpStopReason::Unrecognised("paused".into())
        );
        assert_eq!(AcpStopReason::TransportLost.as_token(), "transport_lost");
    }
}

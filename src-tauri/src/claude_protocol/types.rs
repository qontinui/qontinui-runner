//! Protocol message types for Claude CLI stream-json format.
//!
//! These types represent the NDJSON messages exchanged between the runner
//! and Claude CLI when using `--input-format stream-json --output-format stream-json`.
//!
//! **The decoder is tolerant by construction** (plan
//! `2026-09-20-ai-session-handling-is-claude-shaped-provider-manifest-and-failure-taxonomy`,
//! Phase 8). A frame whose `type` this file does not list decodes as
//! [`ClaudeOutputMessage::Other`] instead of failing — before that arm a new
//! upstream frame (`rate_limit_event` was one, arriving every turn on Claude
//! Code 2.1.285) failed `decode_message` and was dropped at `debug!`. Enums
//! that carry a CLI-chosen string ([`SystemSubtype`], [`CliSessionState`],
//! [`RateLimitStatus`]) keep a value they do not know as `Unknown(String)`
//! rather than refusing the frame.

use serde::de::{DeserializeOwned, Error as _};
use serde::{Deserialize, Deserializer, Serialize};

// ============================================================================
// Output Messages (Claude CLI -> Runner, via stdout)
// ============================================================================

/// A single NDJSON line from Claude CLI's stdout, dispatched on its `type`.
///
/// Deserialized by hand rather than with `#[serde(tag = "type")]`: an
/// internally tagged enum's `#[serde(other)]` arm cannot carry data, and the
/// [`Other`](Self::Other) arm must keep the frame's type (and the frame) so the
/// dispatcher can count and name what it does not understand.
#[derive(Debug, Clone)]
pub enum ClaudeOutputMessage {
    System(SystemMessage),
    Assistant(AssistantMessage),
    /// User/tool_result messages echoed back by CLI in interactive mode.
    User(UserEchoMessage),
    ContentBlockStart(ContentBlockStartMessage),
    ContentBlockDelta(ContentBlockDeltaMessage),
    ContentBlockStop(ContentBlockStopMessage),
    Result(ResultMessage),
    ControlRequest(CliControlRequest),
    ControlResponse(CliControlResponse),
    /// `rate_limit_event` — the account's rate-limit status, sent once per
    /// turn by default (Phase 2 probe Q2, Claude Code 2.1.285).
    RateLimitEvent(RateLimitEvent),
    /// A frame whose `type` this decoder does not list. Never a decode error.
    Other(UnknownFrame),
}

/// A frame of a type the decoder does not know: its `type` and the whole
/// frame as JSON. The raw value may carry account data (the `initialize`
/// response does) — log [`UnknownFrame::frame_type`], never the value.
#[derive(Debug, Clone)]
pub struct UnknownFrame {
    pub frame_type: String,
    pub raw: serde_json::Value,
}

impl<'de> Deserialize<'de> for ClaudeOutputMessage {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        fn typed<T: DeserializeOwned, E: serde::de::Error>(
            value: serde_json::Value,
        ) -> Result<T, E> {
            serde_json::from_value(value).map_err(E::custom)
        }
        let value = serde_json::Value::deserialize(deserializer)?;
        let frame_type = value
            .get("type")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| D::Error::missing_field("type"))?
            .to_string();
        Ok(match frame_type.as_str() {
            "system" => Self::System(typed(value)?),
            "assistant" => Self::Assistant(typed(value)?),
            "user" => Self::User(typed(value)?),
            "content_block_start" => Self::ContentBlockStart(typed(value)?),
            "content_block_delta" => Self::ContentBlockDelta(typed(value)?),
            "content_block_stop" => Self::ContentBlockStop(typed(value)?),
            "result" => Self::Result(typed(value)?),
            "control_request" => Self::ControlRequest(typed(value)?),
            "control_response" => Self::ControlResponse(typed(value)?),
            "rate_limit_event" => Self::RateLimitEvent(typed(value)?),
            _ => Self::Other(UnknownFrame {
                frame_type,
                raw: value,
            }),
        })
    }
}

/// Declares a string-valued protocol enum whose unknown values are kept, not
/// refused: `Known` variants map from their wire spelling, anything else is
/// `Unknown(String)`.
macro_rules! open_string_enum {
    ($(#[$meta:meta])* $name:ident { $($(#[$vmeta:meta])* $variant:ident = $wire:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq)]
        pub enum $name {
            $($(#[$vmeta])* $variant,)+
            /// A value this runner does not know, verbatim.
            Unknown(String),
        }

        impl $name {
            /// The wire spelling.
            pub fn as_str(&self) -> &str {
                match self {
                    $(Self::$variant => $wire,)+
                    Self::Unknown(s) => s,
                }
            }
        }

        impl From<&str> for $name {
            fn from(s: &str) -> Self {
                match s {
                    $($wire => Self::$variant,)+
                    other => Self::Unknown(other.to_string()),
                }
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                Ok(Self::from(String::deserialize(d)?.as_str()))
            }
        }
    };
}

open_string_enum!(
    /// `system.subtype`.
    SystemSubtype {
        /// Session start: tools, model, session id.
        Init = "init",
        /// `session_state_changed` — sent only when the child env sets
        /// `CLAUDE_CODE_EMIT_SESSION_STATE_EVENTS=1` (probe Q2).
        SessionStateChanged = "session_state_changed",
        /// Thinking-token progress; informational.
        ThinkingTokens = "thinking_tokens",
    }
);

open_string_enum!(
    /// `session_state_changed.state` — the CLI's own turn state. `Idle`
    /// arrives AFTER the turn's `result` frame (probe Q2).
    CliSessionState {
        Idle = "idle",
        Running = "running",
        /// Waiting on a decision (a permission request) — Phase 9's signal.
        RequiresAction = "requires_action",
    }
);

open_string_enum!(
    /// `rate_limit_info.status`. Only `allowed` was observed on 2.1.285; the
    /// spellings of a limited status are the SDK's and are not probe-verified,
    /// which is why an unlisted value is kept rather than refused.
    RateLimitStatus {
        Allowed = "allowed",
        AllowedWarning = "allowed_warning",
        Rejected = "rejected",
    }
);

impl RateLimitStatus {
    /// Whether this status lets requests through. An unknown status is NOT
    /// treated as allowed.
    pub fn is_allowed(&self) -> bool {
        matches!(self, Self::Allowed | Self::AllowedWarning)
    }
}

/// Accept an integer, a float, a numeric string or null; anything else is
/// `None` rather than a decode failure of the whole frame.
fn lenient_i64<'de, D: Deserializer<'de>>(d: D) -> Result<Option<i64>, D::Error> {
    Ok(match serde_json::Value::deserialize(d)? {
        serde_json::Value::Number(n) => n
            .as_i64()
            .or_else(|| n.as_f64().filter(|f| f.is_finite()).map(|f| f as i64)),
        serde_json::Value::String(s) => s.trim().parse().ok(),
        _ => None,
    })
}

fn lenient_string<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    Ok(match serde_json::Value::deserialize(d)? {
        serde_json::Value::String(s) => Some(s),
        _ => None,
    })
}

fn lenient_bool<'de, D: Deserializer<'de>>(d: D) -> Result<Option<bool>, D::Error> {
    Ok(serde_json::Value::deserialize(d)?.as_bool())
}

fn lenient_u16<'de, D: Deserializer<'de>>(d: D) -> Result<Option<u16>, D::Error> {
    Ok(lenient_i64(d)?.and_then(|n| u16::try_from(n).ok()))
}

/// `rate_limit_event` frame.
#[derive(Debug, Clone, Deserialize)]
pub struct RateLimitEvent {
    #[serde(default)]
    pub rate_limit_info: Option<RateLimitInfo>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// `rate_limit_event.rate_limit_info` (camelCase on the wire).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RateLimitInfo {
    #[serde(default)]
    pub status: Option<RateLimitStatus>,
    /// Epoch seconds.
    #[serde(default, deserialize_with = "lenient_i64")]
    pub resets_at: Option<i64>,
    /// `five_hour`, `seven_day`, … — kept as a string; the set grows.
    #[serde(default)]
    pub rate_limit_type: Option<String>,
    #[serde(default)]
    pub overage_status: Option<String>,
    #[serde(default)]
    pub overage_disabled_reason: Option<String>,
    #[serde(default)]
    pub is_using_overage: Option<bool>,
    /// Per-window utilization, keyed by window name (`five_hour`, `seven_day`).
    #[serde(default)]
    pub unified_windows: Option<std::collections::BTreeMap<String, RateLimitWindow>>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// One window of `rate_limit_info.unifiedWindows`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RateLimitWindow {
    #[serde(default)]
    pub utilization: Option<f64>,
    #[serde(default, deserialize_with = "lenient_i64")]
    pub resets_at: Option<i64>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// System message - session initialization info.
#[derive(Debug, Clone, Deserialize)]
pub struct SystemMessage {
    /// `init`, `session_state_changed`, `thinking_tokens`, or a subtype this
    /// runner does not know (kept, not refused).
    #[serde(default)]
    pub subtype: Option<SystemSubtype>,
    /// The new state, on a `session_state_changed` frame.
    #[serde(default)]
    pub state: Option<CliSessionState>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub tools: Option<Vec<serde_json::Value>>,
    #[serde(default)]
    pub model: Option<String>,
    /// Catch-all for unknown fields
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// User message echoed back by CLI in interactive mode (tool results, etc.).
/// We don't need to extract text from these - they're informational only.
#[derive(Debug, Clone, Deserialize)]
pub struct UserEchoMessage {
    #[serde(default)]
    pub message: Option<serde_json::Value>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// Assistant message - contains content blocks (text, tool_use, etc.).
#[derive(Debug, Clone, Deserialize)]
pub struct AssistantMessage {
    pub message: AssistantMessageInner,
    #[serde(default)]
    pub session_id: Option<String>,
    /// The typed error code on an errored turn's synthetic assistant frame
    /// (`model_not_found`, `rate_limit`, … — the `StopFailure` hook's
    /// `error_type` vocabulary; probe Q3). A non-string value is ignored
    /// rather than failing the frame (it would drop the turn's text).
    #[serde(default, deserialize_with = "lenient_string")]
    pub error: Option<String>,
    /// `true` on that synthetic frame.
    #[serde(default, deserialize_with = "lenient_bool")]
    pub is_api_error_message: Option<bool>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AssistantMessageInner {
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub content: Vec<ContentBlock>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub stop_reason: Option<String>,
    #[serde(default)]
    pub usage: Option<UsageInfo>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// A content block within an assistant message.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum ContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: String,
        content: serde_json::Value,
        #[serde(default)]
        is_error: Option<bool>,
    },
    /// Catch-all for unknown block types
    #[serde(other)]
    Unknown,
}

/// Content block start (streaming).
#[derive(Debug, Clone, Deserialize)]
pub struct ContentBlockStartMessage {
    #[serde(default)]
    pub index: Option<u32>,
    #[serde(default)]
    pub content_block: Option<serde_json::Value>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// Content block delta (streaming text).
#[derive(Debug, Clone, Deserialize)]
pub struct ContentBlockDeltaMessage {
    #[serde(default)]
    pub index: Option<u32>,
    #[serde(default)]
    pub delta: Option<DeltaContent>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DeltaContent {
    #[serde(default, rename = "type")]
    pub delta_type: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// Content block stop.
#[derive(Debug, Clone, Deserialize)]
pub struct ContentBlockStopMessage {
    #[serde(default)]
    pub index: Option<u32>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// Result message - signals turn completion.
///
/// A turn that ends on an API failure arrives as **`subtype: "success"` with
/// `is_error: true`** (Phase 2 probe Q3; the reference SDK documents the same),
/// so `subtype` alone never says the turn succeeded — see
/// [`ResultMessage::is_success`].
#[derive(Debug, Clone, Deserialize)]
pub struct ResultMessage {
    /// `success`, or an `error_*` subtype.
    #[serde(default)]
    pub subtype: Option<String>,
    /// The turn failed, whatever `subtype` says.
    #[serde(default, deserialize_with = "lenient_bool")]
    pub is_error: Option<bool>,
    /// Why the turn ended: `completed`, `api_error`, …
    #[serde(default)]
    pub terminal_reason: Option<String>,
    /// The HTTP status the provider API answered on an errored turn.
    #[serde(default, deserialize_with = "lenient_u16")]
    pub api_error_status: Option<u16>,
    /// Error strings, when the CLI lists them (the reference SDK reads this
    /// first; 2.1.285's errored turn did not carry it).
    #[serde(default)]
    pub errors: Option<Vec<serde_json::Value>>,
    /// In inline mode, `result` is an object with `content` blocks.
    /// In interactive mode, `result` is a plain string (the final text output).
    #[serde(default)]
    pub result: Option<ResultField>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// The `result` field can be either a string (interactive mode) or a structured
/// object with content blocks (inline mode).
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum ResultField {
    /// Structured result with content blocks (inline mode).
    Structured(ResultContent),
    /// Plain text result (interactive mode).
    Text(String),
}

#[derive(Debug, Clone, Deserialize)]
pub struct ResultContent {
    #[serde(default)]
    pub content: Option<Vec<ContentBlock>>,
    #[serde(default)]
    pub usage: Option<UsageInfo>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

impl ResultMessage {
    /// The turn succeeded: `subtype == "success"` and not `is_error: true`.
    pub fn is_success(&self) -> bool {
        self.subtype.as_deref() == Some("success") && self.is_error != Some(true)
    }

    /// The human-readable reason an errored turn failed, in the reference
    /// SDK's preference order (idea from claude-agent-sdk-python
    /// `_internal/query.py`, MIT): the `errors` list joined, else the result
    /// text, else the legacy `error` field, else the typed cause
    /// (`terminal_reason` / `api_error_status`). `None` when nothing is stated.
    pub fn error_text(&self) -> Option<String> {
        let listed: Vec<String> = self
            .errors
            .iter()
            .flatten()
            .filter_map(|e| match e {
                serde_json::Value::String(s) => Some(s.trim().to_string()),
                serde_json::Value::Null => None,
                other => Some(other.to_string()),
            })
            .filter(|s| !s.is_empty())
            .collect();
        if !listed.is_empty() {
            return Some(listed.join("; "));
        }
        let result_text = match &self.result {
            Some(ResultField::Text(t)) => Some(t.trim().to_string()),
            Some(ResultField::Structured(c)) => c.content.as_ref().map(|blocks| {
                blocks
                    .iter()
                    .filter_map(|b| match b {
                        ContentBlock::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<String>()
                    .trim()
                    .to_string()
            }),
            None => None,
        };
        if let Some(t) = result_text.filter(|t| !t.is_empty()) {
            return Some(t);
        }
        if let Some(e) = self
            .error
            .as_deref()
            .map(str::trim)
            .filter(|e| !e.is_empty())
        {
            return Some(e.to_string());
        }
        match (self.terminal_reason.as_deref(), self.api_error_status) {
            (Some(r), Some(s)) => Some(format!("{r} (HTTP {s})")),
            (Some(r), None) => Some(r.to_string()),
            (None, Some(s)) => Some(format!("HTTP {s}")),
            (None, None) => None,
        }
    }
}

/// Usage information.
#[derive(Debug, Clone, Deserialize)]
pub struct UsageInfo {
    #[serde(default)]
    pub input_tokens: Option<u64>,
    #[serde(default)]
    pub output_tokens: Option<u64>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// Control request FROM the CLI (e.g., asking permission for tool use).
#[derive(Debug, Clone, Deserialize)]
pub struct CliControlRequest {
    pub request: CliControlRequestPayload,
    #[serde(default)]
    pub request_id: Option<String>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CliControlRequestPayload {
    pub subtype: String,
    #[serde(flatten)]
    pub data: serde_json::Map<String, serde_json::Value>,
}

/// The `subtype` of a control request asking to run a tool.
pub const CAN_USE_TOOL: &str = "can_use_tool";

/// A `can_use_tool` request's payload, as Claude Code 2.1.285 sends it (probe
/// Q1): `tool_name`, `display_name`, `input`, `description`,
/// `permission_suggestions`, `tool_use_id`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct CanUseToolRequest {
    pub tool_name: String,
    /// The tool's arguments. An allow echoes it back as `updatedInput`.
    #[serde(default)]
    pub input: serde_json::Value,
    /// Permission-rule changes the CLI offers alongside the request (e.g.
    /// `{"type":"setMode","mode":"acceptEdits","destination":"session"}`).
    #[serde(default)]
    pub permission_suggestions: Vec<serde_json::Value>,
    #[serde(default)]
    pub tool_use_id: Option<String>,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

impl CliControlRequestPayload {
    /// This payload as a `can_use_tool` request. `None` for another subtype;
    /// `Some(Err)` names what a malformed `can_use_tool` is missing.
    pub fn as_can_use_tool(&self) -> Option<Result<CanUseToolRequest, String>> {
        if self.subtype != CAN_USE_TOOL {
            return None;
        }
        Some(
            serde_json::from_value(serde_json::Value::Object(self.data.clone()))
                .map_err(|e| format!("malformed can_use_tool request: {e}")),
        )
    }
}

/// Control response FROM the CLI (response to our control request).
#[derive(Debug, Clone, Deserialize)]
pub struct CliControlResponse {
    #[serde(default)]
    pub response: Option<serde_json::Value>,
    #[serde(default)]
    pub request_id: Option<String>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

// ============================================================================
// Input Messages (Runner -> Claude CLI, via stdin)
// ============================================================================

/// User message to send to Claude CLI.
#[derive(Debug, Clone, Serialize)]
pub struct UserInputMessage {
    #[serde(rename = "type")]
    pub msg_type: String, // always "user"
    pub message: UserMessagePayload,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_tool_use_id: Option<String>,
    pub session_id: String,
}

impl UserInputMessage {
    pub fn new(content: &str, session_id: &str) -> Self {
        Self {
            msg_type: "user".to_string(),
            message: UserMessagePayload {
                role: "user".to_string(),
                content: content.to_string(),
            },
            parent_tool_use_id: None,
            session_id: session_id.to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct UserMessagePayload {
    pub role: String,
    pub content: String,
}

/// Control request TO the CLI (initialize, interrupt).
#[derive(Debug, Clone, Serialize)]
pub struct OutgoingControlRequest {
    #[serde(rename = "type")]
    pub msg_type: String, // always "control_request"
    pub request: OutgoingControlRequestPayload,
    pub request_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct OutgoingControlRequestPayload {
    pub subtype: String,
    #[serde(flatten)]
    pub data: serde_json::Map<String, serde_json::Value>,
}

impl OutgoingControlRequest {
    pub fn initialize(request_id: &str) -> Self {
        let mut data = serde_json::Map::new();
        data.insert(
            "protocolVersion".to_string(),
            serde_json::Value::String("1".to_string()),
        );
        Self {
            msg_type: "control_request".to_string(),
            request: OutgoingControlRequestPayload {
                subtype: "initialize".to_string(),
                data,
            },
            request_id: request_id.to_string(),
        }
    }

    pub fn interrupt(request_id: &str) -> Self {
        Self {
            msg_type: "control_request".to_string(),
            request: OutgoingControlRequestPayload {
                subtype: "interrupt".to_string(),
                data: serde_json::Map::new(),
            },
            request_id: request_id.to_string(),
        }
    }
}

/// Control response TO the CLI (answering a control request the CLI sent).
///
/// The wire shape is the reference SDK's, and the ONLY one the CLI accepts
/// (plan `2026-09-20-ai-session-handling-is-claude-shaped-provider-manifest-and-failure-taxonomy`,
/// Phase 2 probe Q1, fixtures `tests/fixtures/cli_protocol/claude/2.1.285/can_use_tool_*`):
///
/// ```json
/// {"type":"control_response","response":{"subtype":"success","request_id":"<id>","response":{…}}}
/// {"type":"control_response","response":{"subtype":"error","request_id":"<id>","error":"…"}}
/// ```
///
/// The `request_id` rides INSIDE `response`. The runner's former
/// `{"type":"control_response","response":{"allowed":true},"request_id":…}` was
/// silently ignored by Claude Code 2.1.285 and the turn hung forever
/// (`can_use_tool_runner_shape_ignored`), so it has no constructor here.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct OutgoingControlResponse {
    #[serde(rename = "type")]
    msg_type: &'static str, // always "control_response"
    pub response: ControlResponseBody,
}

/// The body of an [`OutgoingControlResponse`], tagged by `subtype`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "subtype", rename_all = "snake_case")]
pub enum ControlResponseBody {
    /// The request was handled; `response` is the subtype-specific answer.
    Success {
        request_id: String,
        response: serde_json::Value,
    },
    /// The request could not be handled; `error` says why.
    Error { request_id: String, error: String },
}

impl OutgoingControlResponse {
    fn new(response: ControlResponseBody) -> Self {
        Self {
            msg_type: "control_response",
            response,
        }
    }

    /// Allow a `can_use_tool` request. `updated_input` is the input the tool
    /// runs with — the request's own `input` unless the operator edited it.
    pub fn allow_tool_use(request_id: &str, updated_input: serde_json::Value) -> Self {
        Self::new(ControlResponseBody::Success {
            request_id: request_id.to_string(),
            response: serde_json::json!({
                "behavior": "allow",
                "updatedInput": updated_input,
            }),
        })
    }

    /// Deny a `can_use_tool` request. The CLI hands `message` to the model as
    /// the denied call's error `tool_result` and the turn continues; the
    /// result lists the call in `permission_denials` and is NOT an errored
    /// turn (probe Q1, `can_use_tool_sdk_deny`).
    pub fn deny_tool_use(request_id: &str, message: &str) -> Self {
        Self::new(ControlResponseBody::Success {
            request_id: request_id.to_string(),
            response: serde_json::json!({
                "behavior": "deny",
                "message": message,
            }),
        })
    }

    /// Refuse a control request the runner does not handle — never a
    /// fabricated success.
    pub fn error(request_id: &str, error: &str) -> Self {
        Self::new(ControlResponseBody::Error {
            request_id: request_id.to_string(),
            error: error.to_string(),
        })
    }
}

// ============================================================================
// Helper: Extract text from output messages
// ============================================================================

impl ClaudeOutputMessage {
    /// Extract text content from this message, if any.
    pub fn extract_text(&self) -> Option<String> {
        match self {
            ClaudeOutputMessage::Assistant(msg) => {
                let texts: Vec<&str> = msg
                    .message
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        ContentBlock::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect();
                if texts.is_empty() {
                    None
                } else {
                    Some(texts.join(""))
                }
            }
            ClaudeOutputMessage::ContentBlockDelta(msg) => {
                msg.delta.as_ref().and_then(|d| d.text.clone())
            }
            ClaudeOutputMessage::Result(msg) => msg.result.as_ref().and_then(|r| match r {
                ResultField::Text(text) => {
                    if text.is_empty() {
                        None
                    } else {
                        Some(text.clone())
                    }
                }
                ResultField::Structured(content) => content
                    .content
                    .as_ref()
                    .map(|blocks| {
                        blocks
                            .iter()
                            .filter_map(|block| match block {
                                ContentBlock::Text { text } => Some(text.as_str()),
                                _ => None,
                            })
                            .collect::<Vec<&str>>()
                            .join("")
                    })
                    .filter(|s| !s.is_empty()),
            }),
            _ => None,
        }
    }

    /// Check if this is a result message signaling turn completion.
    pub fn is_result(&self) -> bool {
        matches!(self, ClaudeOutputMessage::Result(_))
    }

    /// Check if this is a successful result: `subtype == "success"` and not
    /// `is_error: true` — an API failure arrives as `success` + `is_error`.
    pub fn is_success_result(&self) -> bool {
        matches!(self, ClaudeOutputMessage::Result(r) if r.is_success())
    }

    /// The frame's `type` as it appeared on the wire.
    pub fn frame_type(&self) -> &str {
        match self {
            ClaudeOutputMessage::System(_) => "system",
            ClaudeOutputMessage::Assistant(_) => "assistant",
            ClaudeOutputMessage::User(_) => "user",
            ClaudeOutputMessage::ContentBlockStart(_) => "content_block_start",
            ClaudeOutputMessage::ContentBlockDelta(_) => "content_block_delta",
            ClaudeOutputMessage::ContentBlockStop(_) => "content_block_stop",
            ClaudeOutputMessage::Result(_) => "result",
            ClaudeOutputMessage::ControlRequest(_) => "control_request",
            ClaudeOutputMessage::ControlResponse(_) => "control_response",
            ClaudeOutputMessage::RateLimitEvent(_) => "rate_limit_event",
            ClaudeOutputMessage::Other(f) => &f.frame_type,
        }
    }

    /// Check if this is a control request from CLI.
    pub fn as_control_request(&self) -> Option<&CliControlRequest> {
        match self {
            ClaudeOutputMessage::ControlRequest(req) => Some(req),
            _ => None,
        }
    }

    /// Check if this is a control response from CLI.
    pub fn as_control_response(&self) -> Option<&CliControlResponse> {
        match self {
            ClaudeOutputMessage::ControlResponse(resp) => Some(resp),
            _ => None,
        }
    }

    /// Extract the first tool_use content block from an assistant message.
    /// Returns (tool_name, input_object) if found.
    /// Used to emit tool_activity events in bypassPermissions mode where
    /// can_use_tool control requests are never sent.
    pub fn extract_tool_use(&self) -> Option<(&str, serde_json::Map<String, serde_json::Value>)> {
        match self {
            ClaudeOutputMessage::Assistant(msg) => {
                for block in &msg.message.content {
                    if let ContentBlock::ToolUse { name, input, .. } = block {
                        let data = input.as_object().cloned().unwrap_or_default();
                        return Some((name.as_str(), data));
                    }
                }
                None
            }
            _ => None,
        }
    }

    /// Extract tool_use info from a content_block_start message.
    /// In stream-json mode, tool use is signaled via content_block_start
    /// before the full assistant message arrives.
    /// Returns (tool_name, input_object) if this is a tool_use block start.
    pub fn extract_tool_use_from_block_start(
        &self,
    ) -> Option<(String, serde_json::Map<String, serde_json::Value>)> {
        match self {
            ClaudeOutputMessage::ContentBlockStart(msg) => {
                let block = msg.content_block.as_ref()?;
                let obj = block.as_object()?;
                let block_type = obj.get("type")?.as_str()?;
                if block_type != "tool_use" {
                    return None;
                }
                let name = obj.get("name")?.as_str()?.to_string();
                let input = obj
                    .get("input")
                    .and_then(|v| v.as_object())
                    .cloned()
                    .unwrap_or_default();
                Some((name, input))
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(line: &str) -> ClaudeOutputMessage {
        serde_json::from_str(line).unwrap_or_else(|e| panic!("{e}: {line}"))
    }

    fn fixture_dir() -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/cli_protocol/claude/2.1.285")
    }

    /// The probe's `rate_limit_event` (Q2) decodes typed, every field.
    #[test]
    fn rate_limit_event_decodes_typed() {
        let msg = decode(
            r#"{"type":"rate_limit_event","rate_limit_info":{"status":"allowed","resetsAt":1790727000,"rateLimitType":"five_hour","overageStatus":"rejected","overageDisabledReason":"org_level_disabled","isUsingOverage":false,"unifiedWindows":{"five_hour":{"utilization":0.3,"resetsAt":1790727000},"seven_day":{"utilization":0.44,"resetsAt":1790856000}}},"uuid":"u","session_id":"s"}"#,
        );
        let ClaudeOutputMessage::RateLimitEvent(ev) = msg else {
            panic!("not a RateLimitEvent");
        };
        let info = ev.rate_limit_info.expect("info");
        assert_eq!(info.status, Some(RateLimitStatus::Allowed));
        assert!(info.status.as_ref().unwrap().is_allowed());
        assert_eq!(info.resets_at, Some(1_790_727_000));
        assert_eq!(info.rate_limit_type.as_deref(), Some("five_hour"));
        assert_eq!(info.overage_status.as_deref(), Some("rejected"));
        assert_eq!(
            info.overage_disabled_reason.as_deref(),
            Some("org_level_disabled")
        );
        assert_eq!(info.is_using_overage, Some(false));
        let windows = info.unified_windows.expect("windows");
        assert_eq!(windows["seven_day"].utilization, Some(0.44));
        assert_eq!(windows["seven_day"].resets_at, Some(1_790_856_000));
        assert_eq!(ev.session_id.as_deref(), Some("s"));
    }

    /// A status this runner has never seen still decodes — and is not allowed.
    #[test]
    fn unknown_rate_limit_status_decodes_and_is_not_allowed() {
        let msg = decode(
            r#"{"type":"rate_limit_event","rate_limit_info":{"status":"throttled_v2","resetsAt":"1790727000.5"}}"#,
        );
        let ClaudeOutputMessage::RateLimitEvent(ev) = msg else {
            panic!("not a RateLimitEvent");
        };
        let info = ev.rate_limit_info.unwrap();
        assert_eq!(
            info.status,
            Some(RateLimitStatus::Unknown("throttled_v2".into()))
        );
        assert!(!info.status.unwrap().is_allowed());
        assert_eq!(
            info.resets_at, None,
            "a non-integer string is dropped, not fatal"
        );
        assert_eq!(RateLimitStatus::from("rejected"), RateLimitStatus::Rejected);
        assert_eq!(RateLimitStatus::Rejected.as_str(), "rejected");
    }

    /// `system` subtypes are typed, including the opt-in state event and the
    /// thinking-token frame; an unknown subtype is kept.
    #[test]
    fn system_subtypes_and_session_state() {
        let state = |s: &str| {
            let ClaudeOutputMessage::System(m) = decode(&format!(
                r#"{{"type":"system","subtype":"session_state_changed","state":"{s}","uuid":"u","session_id":"x"}}"#
            )) else {
                panic!("not system");
            };
            assert_eq!(m.subtype, Some(SystemSubtype::SessionStateChanged));
            m.state
        };
        assert_eq!(state("idle"), Some(CliSessionState::Idle));
        assert_eq!(state("running"), Some(CliSessionState::Running));
        assert_eq!(
            state("requires_action"),
            Some(CliSessionState::RequiresAction)
        );
        assert_eq!(
            state("paused"),
            Some(CliSessionState::Unknown("paused".into()))
        );

        let ClaudeOutputMessage::System(m) =
            decode(r#"{"type":"system","subtype":"thinking_tokens","tokens":12}"#)
        else {
            panic!("not system");
        };
        assert_eq!(m.subtype, Some(SystemSubtype::ThinkingTokens));
        let ClaudeOutputMessage::System(m) = decode(r#"{"type":"system","subtype":"brand_new"}"#)
        else {
            panic!("not system");
        };
        assert_eq!(m.subtype, Some(SystemSubtype::Unknown("brand_new".into())));
        let ClaudeOutputMessage::System(m) =
            decode(r#"{"type":"system","subtype":"init","model":"m"}"#)
        else {
            panic!("not system");
        };
        assert_eq!(m.subtype, Some(SystemSubtype::Init));
        assert_eq!(m.model.as_deref(), Some("m"));
    }

    /// `subtype: success` + `is_error: true` is NOT a success (probe Q3).
    #[test]
    fn success_subtype_with_is_error_is_not_success() {
        let msg = decode(
            r#"{"type":"result","subtype":"success","is_error":true,"api_error_status":404,"terminal_reason":"api_error","result":"There's an issue with the selected model."}"#,
        );
        assert!(msg.is_result());
        assert!(!msg.is_success_result());
        let ClaudeOutputMessage::Result(r) = msg else {
            unreachable!()
        };
        assert_eq!(r.is_error, Some(true));
        assert_eq!(r.api_error_status, Some(404));
        assert_eq!(r.terminal_reason.as_deref(), Some("api_error"));
        assert!(
            !r.extra.contains_key("is_error"),
            "lifted out of the extra bag"
        );
        assert_eq!(
            r.error_text().as_deref(),
            Some("There's an issue with the selected model.")
        );

        let ok = decode(r#"{"type":"result","subtype":"success","is_error":false,"result":"hi"}"#);
        assert!(ok.is_success_result());
        let legacy = decode(r#"{"type":"result","subtype":"success"}"#);
        assert!(
            legacy.is_success_result(),
            "no is_error field keeps the old meaning"
        );
        let error_subtype = decode(r#"{"type":"result","subtype":"error_max_turns"}"#);
        assert!(!error_subtype.is_success_result());
    }

    /// Error text follows the reference SDK's order: errors[] joined, else the
    /// result text, else the legacy `error`, else the typed cause.
    #[test]
    fn error_text_preference_order() {
        let text = |line: &str| {
            let ClaudeOutputMessage::Result(r) = decode(line) else {
                panic!("not a result")
            };
            r.error_text()
        };
        assert_eq!(
            text(r#"{"type":"result","subtype":"error_during_execution","is_error":true,"errors":["a","b"],"result":"ignored"}"#)
                .as_deref(),
            Some("a; b")
        );
        assert_eq!(
            text(r#"{"type":"result","subtype":"success","is_error":true,"errors":[],"result":"  the text  "}"#)
                .as_deref(),
            Some("the text")
        );
        assert_eq!(
            text(r#"{"type":"result","subtype":"error","error":"Something went wrong"}"#)
                .as_deref(),
            Some("Something went wrong")
        );
        assert_eq!(
            text(r#"{"type":"result","subtype":"success","is_error":true,"result":"","terminal_reason":"api_error","api_error_status":529}"#)
                .as_deref(),
            Some("api_error (HTTP 529)")
        );
        assert_eq!(
            text(r#"{"type":"result","subtype":"success","is_error":true}"#),
            None
        );
    }

    /// The errored turn's synthetic assistant frame carries its typed code.
    #[test]
    fn assistant_error_code_is_typed() {
        let ClaudeOutputMessage::Assistant(a) = decode(
            r#"{"type":"assistant","error":"model_not_found","is_api_error_message":true,"message":{"model":"<synthetic>","content":[{"type":"text","text":"bad model"}]}}"#,
        ) else {
            panic!("not assistant")
        };
        assert_eq!(a.error.as_deref(), Some("model_not_found"));
        assert_eq!(a.is_api_error_message, Some(true));
        // A non-string `error` does not cost the frame its text.
        let msg = decode(
            r#"{"type":"assistant","error":{"nested":1},"message":{"content":[{"type":"text","text":"still here"}]}}"#,
        );
        assert_eq!(msg.extract_text().as_deref(), Some("still here"));
    }

    /// Every frame of every recorded Claude 2.1.285 fixture decodes, and none
    /// of them falls through to `Other` — the decoder knows the whole recorded
    /// vocabulary (a stdin `control_request`/`user`/`control_response` frame is
    /// also a known type).
    #[test]
    fn every_recorded_frame_decodes_to_a_known_type() {
        let mut checked = 0;
        for entry in std::fs::read_dir(fixture_dir()).expect("fixture dir") {
            let path = entry.unwrap().path();
            if path.extension().and_then(|e| e.to_str()) != Some("ndjson") {
                continue;
            }
            for line in std::fs::read_to_string(&path).unwrap().lines() {
                if line.trim().is_empty() {
                    continue;
                }
                let msg = decode(line);
                assert!(
                    !matches!(msg, ClaudeOutputMessage::Other(_)),
                    "{}: unexpected Other frame {}",
                    path.display(),
                    msg.frame_type()
                );
                checked += 1;
            }
        }
        assert!(checked > 40, "fixtures were read ({checked} frames)");
    }
}

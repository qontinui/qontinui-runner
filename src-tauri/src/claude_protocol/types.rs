//! Protocol message types for Claude CLI stream-json format.
//!
//! These types represent the NDJSON messages exchanged between the runner
//! and Claude CLI when using `--input-format stream-json --output-format stream-json`.

use serde::{Deserialize, Serialize};

// ============================================================================
// Output Messages (Claude CLI -> Runner, via stdout)
// ============================================================================

/// A single NDJSON line from Claude CLI's stdout.
/// Tagged on the `type` field.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum ClaudeOutputMessage {
    #[serde(rename = "system")]
    System(SystemMessage),
    #[serde(rename = "assistant")]
    Assistant(AssistantMessage),
    /// User/tool_result messages echoed back by CLI in interactive mode.
    #[serde(rename = "user")]
    User(UserEchoMessage),
    #[serde(rename = "content_block_start")]
    ContentBlockStart(ContentBlockStartMessage),
    #[serde(rename = "content_block_delta")]
    ContentBlockDelta(ContentBlockDeltaMessage),
    #[serde(rename = "content_block_stop")]
    ContentBlockStop(ContentBlockStopMessage),
    #[serde(rename = "result")]
    Result(ResultMessage),
    #[serde(rename = "control_request")]
    ControlRequest(CliControlRequest),
    #[serde(rename = "control_response")]
    ControlResponse(CliControlResponse),
}

/// System message - session initialization info.
#[derive(Debug, Clone, Deserialize)]
pub struct SystemMessage {
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
#[derive(Debug, Clone, Deserialize)]
pub struct ResultMessage {
    /// "success" or "error"
    #[serde(default)]
    pub subtype: Option<String>,
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

/// Control response TO the CLI (answering CLI's control requests).
#[derive(Debug, Clone, Serialize)]
pub struct OutgoingControlResponse {
    #[serde(rename = "type")]
    pub msg_type: String, // always "control_response"
    pub response: OutgoingControlResponsePayload,
    pub request_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct OutgoingControlResponsePayload {
    #[serde(flatten)]
    pub data: serde_json::Map<String, serde_json::Value>,
}

impl OutgoingControlResponse {
    /// Approve a `can_use_tool` request (auto-allow in bypass mode).
    pub fn allow_tool(request_id: &str) -> Self {
        let mut data = serde_json::Map::new();
        data.insert("allowed".to_string(), serde_json::Value::Bool(true));
        Self {
            msg_type: "control_response".to_string(),
            response: OutgoingControlResponsePayload { data },
            request_id: request_id.to_string(),
        }
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

    /// Check if this is a successful result.
    pub fn is_success_result(&self) -> bool {
        matches!(self, ClaudeOutputMessage::Result(r) if r.subtype.as_deref() == Some("success"))
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
}

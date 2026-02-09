//! Mock Claude CLI binary for integration testing.
//!
//! This binary speaks the Claude CLI stream-json NDJSON protocol.
//! It reads NDJSON from stdin and writes NDJSON to stdout, simulating
//! the Claude CLI's behavior for testing purposes.
//!
//! Supported interactions:
//! - Initialize handshake: responds to `control_request` with `control_response`
//! - User messages: responds with an assistant message + result
//! - Interrupt: responds with a result immediately
//! - Control requests from runner: not applicable (this mock is the "CLI" side)

use std::io::{self, BufRead, Write};

use serde::{Deserialize, Serialize};
use serde_json::Value;

// ============================================================================
// Incoming message types (from runner -> mock CLI)
// ============================================================================

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum IncomingMessage {
    #[serde(rename = "control_request")]
    ControlRequest(IncomingControlRequest),
    #[serde(rename = "user")]
    UserMessage(IncomingUserMessage),
    #[serde(rename = "control_response")]
    ControlResponse(IncomingControlResponse),
}

#[derive(Debug, Deserialize)]
struct IncomingControlRequest {
    request: ControlRequestPayload,
    request_id: String,
}

#[derive(Debug, Deserialize)]
struct ControlRequestPayload {
    subtype: String,
    #[serde(flatten)]
    _data: serde_json::Map<String, Value>,
}

#[derive(Debug, Deserialize)]
struct IncomingUserMessage {
    message: UserMessagePayload,
    #[serde(default)]
    session_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct UserMessagePayload {
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    content: Option<String>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct IncomingControlResponse {
    #[serde(default)]
    request_id: Option<String>,
    #[serde(flatten)]
    _data: serde_json::Map<String, Value>,
}

// ============================================================================
// Outgoing message types (mock CLI -> runner)
// ============================================================================

#[derive(Debug, Serialize)]
struct OutgoingControlResponse {
    #[serde(rename = "type")]
    msg_type: String,
    response: OutgoingControlResponsePayload,
    request_id: String,
}

#[derive(Debug, Serialize)]
struct OutgoingControlResponsePayload {
    subtype: String,
}

#[derive(Debug, Serialize)]
struct OutgoingAssistantMessage {
    #[serde(rename = "type")]
    msg_type: String,
    message: AssistantMessagePayload,
    session_id: String,
}

#[derive(Debug, Serialize)]
struct AssistantMessagePayload {
    role: String,
    content: Vec<ContentBlock>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "type")]
enum ContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
}

#[derive(Debug, Serialize)]
struct OutgoingResult {
    #[serde(rename = "type")]
    msg_type: String,
    subtype: String,
    result: ResultContent,
    session_id: String,
}

#[derive(Debug, Serialize)]
struct ResultContent {
    content: Vec<ContentBlock>,
}

#[derive(Debug, Serialize)]
struct OutgoingToolUseControlRequest {
    #[serde(rename = "type")]
    msg_type: String,
    request: ToolUseRequestPayload,
    request_id: String,
}

#[derive(Debug, Serialize)]
struct ToolUseRequestPayload {
    subtype: String,
    tool_name: String,
}

// ============================================================================
// Protocol helpers
// ============================================================================

fn send_line<T: Serialize>(msg: &T) {
    let json = serde_json::to_string(msg).expect("Failed to serialize message");
    let stdout = io::stdout();
    let mut handle = stdout.lock();
    writeln!(handle, "{}", json).expect("Failed to write to stdout");
    handle.flush().expect("Failed to flush stdout");
}

fn send_assistant_and_result(text: &str, session_id: &str) {
    // Send assistant message
    let assistant = OutgoingAssistantMessage {
        msg_type: "assistant".to_string(),
        message: AssistantMessagePayload {
            role: "assistant".to_string(),
            content: vec![ContentBlock::Text {
                text: text.to_string(),
            }],
        },
        session_id: session_id.to_string(),
    };
    send_line(&assistant);

    // Send result
    let result = OutgoingResult {
        msg_type: "result".to_string(),
        subtype: "success".to_string(),
        result: ResultContent {
            content: vec![ContentBlock::Text {
                text: text.to_string(),
            }],
        },
        session_id: session_id.to_string(),
    };
    send_line(&result);
}

// ============================================================================
// Main loop
// ============================================================================

fn main() {
    // Check for special modes via command-line args
    let args: Vec<String> = std::env::args().collect();
    let mode = args.get(1).map(|s| s.as_str()).unwrap_or("normal");

    let stdin = io::stdin();
    let reader = stdin.lock();

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let msg: IncomingMessage = match serde_json::from_str(trimmed) {
            Ok(m) => m,
            Err(e) => {
                eprintln!(
                    "mock_claude_cli: Failed to parse input: {} (line: {})",
                    e, trimmed
                );
                continue;
            }
        };

        match msg {
            IncomingMessage::ControlRequest(req) => {
                match req.request.subtype.as_str() {
                    "initialize" => {
                        // Respond to init handshake
                        let resp = OutgoingControlResponse {
                            msg_type: "control_response".to_string(),
                            response: OutgoingControlResponsePayload {
                                subtype: "initialize".to_string(),
                            },
                            request_id: req.request_id,
                        };
                        send_line(&resp);
                    }
                    "interrupt" => {
                        // Respond to interrupt with a result
                        let result = OutgoingResult {
                            msg_type: "result".to_string(),
                            subtype: "success".to_string(),
                            result: ResultContent {
                                content: vec![ContentBlock::Text {
                                    text: "Interrupted.".to_string(),
                                }],
                            },
                            session_id: "default".to_string(),
                        };
                        send_line(&result);
                    }
                    other => {
                        eprintln!(
                            "mock_claude_cli: Unknown control request subtype: {}",
                            other
                        );
                    }
                }
            }
            IncomingMessage::UserMessage(user_msg) => {
                let content = user_msg
                    .message
                    .content
                    .unwrap_or_else(|| "<empty>".to_string());
                let session_id = user_msg.session_id.unwrap_or_else(|| "default".to_string());

                match mode {
                    "tool_use" => {
                        // Simulate a tool use request before responding
                        let tool_req = OutgoingToolUseControlRequest {
                            msg_type: "control_request".to_string(),
                            request: ToolUseRequestPayload {
                                subtype: "can_use_tool".to_string(),
                                tool_name: "Bash".to_string(),
                            },
                            request_id: "mock_tool_req_1".to_string(),
                        };
                        send_line(&tool_req);
                        // The runner should auto-approve, then we respond normally
                        // Wait for the approval by reading next line
                        // (handled in next iteration)
                    }
                    "slow" => {
                        // Add a small delay to simulate processing time
                        std::thread::sleep(std::time::Duration::from_millis(100));
                        let response_text = format!("I received your message: {}", content);
                        send_assistant_and_result(&response_text, &session_id);
                    }
                    _ => {
                        // Normal mode: echo back immediately
                        let response_text = format!("I received your message: {}", content);
                        send_assistant_and_result(&response_text, &session_id);
                    }
                }
            }
            IncomingMessage::ControlResponse(_resp) => {
                // This is the runner approving a tool use request.
                // In tool_use mode, now send the actual response.
                if mode == "tool_use" {
                    send_assistant_and_result("Tool approved, executed successfully.", "default");
                }
            }
        }
    }
}

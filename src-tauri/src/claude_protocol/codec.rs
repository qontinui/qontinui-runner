//! NDJSON encode/decode helpers for the Claude CLI protocol.

use serde::Serialize;
use tracing::trace;

use super::types::ClaudeOutputMessage;

/// Truncate a string to at most `max_bytes` bytes, ensuring the cut is on a valid
/// UTF-8 char boundary. Returns the full string if it's already within the limit.
pub fn truncate_str(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    // Find the largest byte index <= max_bytes that is a valid char boundary.
    // Start at max_bytes and walk backward until we find a char boundary.
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Decode a single NDJSON line into a ClaudeOutputMessage.
pub fn decode_message(line: &str) -> Result<ClaudeOutputMessage, String> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Err("Empty line".to_string());
    }
    serde_json::from_str::<ClaudeOutputMessage>(trimmed).map_err(|e| {
        format!(
            "Failed to decode NDJSON: {} (line: {})",
            e,
            truncate_str(trimmed, 200)
        )
    })
}

/// Encode a message as a single NDJSON line (with trailing newline).
pub fn encode_message<T: Serialize>(msg: &T) -> Result<String, String> {
    let json = serde_json::to_string(msg).map_err(|e| format!("Failed to encode NDJSON: {}", e))?;
    trace!("Encoding NDJSON: {}", truncate_str(&json, 200));
    Ok(format!("{}\n", json))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_decode_assistant_message() {
        let line = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Hello world"}],"model":"claude-3-5-sonnet","stop_reason":"end_turn"},"session_id":"test"}"#;
        let msg = decode_message(line).unwrap();
        assert!(matches!(msg, ClaudeOutputMessage::Assistant(_)));
        assert_eq!(msg.extract_text().unwrap(), "Hello world");
    }

    #[test]
    fn test_decode_content_block_delta() {
        let line = r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"partial "}}"#;
        let msg = decode_message(line).unwrap();
        assert!(matches!(msg, ClaudeOutputMessage::ContentBlockDelta(_)));
        assert_eq!(msg.extract_text().unwrap(), "partial ");
    }

    #[test]
    fn test_decode_result_success() {
        let line = r#"{"type":"result","subtype":"success","result":{"content":[{"type":"text","text":"Done"}]},"session_id":"test"}"#;
        let msg = decode_message(line).unwrap();
        assert!(msg.is_result());
        assert!(msg.is_success_result());
    }

    #[test]
    fn test_decode_system() {
        let line = r#"{"type":"system","session_id":"abc123","model":"claude-3-5-sonnet"}"#;
        let msg = decode_message(line).unwrap();
        assert!(matches!(msg, ClaudeOutputMessage::System(_)));
    }

    #[test]
    fn test_decode_control_request() {
        let line = r#"{"type":"control_request","request":{"subtype":"can_use_tool","tool_name":"Bash"},"request_id":"req_1"}"#;
        let msg = decode_message(line).unwrap();
        assert!(msg.as_control_request().is_some());
    }

    #[test]
    fn test_encode_user_message() {
        use super::super::types::UserInputMessage;
        let msg = UserInputMessage::new("Hello", "default");
        let encoded = encode_message(&msg).unwrap();
        assert!(encoded.ends_with('\n'));
        assert!(encoded.contains("\"type\":\"user\""));
        assert!(encoded.contains("\"content\":\"Hello\""));
    }

    #[test]
    fn test_decode_empty_line() {
        assert!(decode_message("").is_err());
        assert!(decode_message("  ").is_err());
    }

    // ============================================================================
    // Protocol simulation tests (dispatcher-level logic without Tauri)
    // ============================================================================

    /// Simulate the init handshake: decode a control_response and verify
    /// it triggers the Initializing -> Ready transition logic.
    #[test]
    fn test_init_handshake_protocol() {
        use super::super::types::OutgoingControlRequest;

        // 1. Encode the init request we would send
        let init_req = OutgoingControlRequest::initialize("req_test_init");
        let encoded = encode_message(&init_req).unwrap();
        assert!(encoded.contains("\"subtype\":\"initialize\""));
        assert!(encoded.contains("\"protocolVersion\":\"1\""));

        // 2. Simulate receiving the init response from CLI
        let response_line = r#"{"type":"control_response","response":{"subtype":"initialize"},"request_id":"req_test_init"}"#;
        let msg = decode_message(response_line).unwrap();

        // 3. Verify it's recognized as a control response
        assert!(msg.as_control_response().is_some());
        assert!(msg.as_control_request().is_none());
        assert!(!msg.is_result());
        assert!(msg.extract_text().is_none());
    }

    /// Simulate receiving an assistant message with text content
    /// and verify text extraction works correctly.
    #[test]
    fn test_assistant_text_extraction() {
        let line = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"I received your message: Hello"}]},"session_id":"default"}"#;
        let msg = decode_message(line).unwrap();

        assert!(matches!(msg, ClaudeOutputMessage::Assistant(_)));
        let text = msg.extract_text().unwrap();
        assert_eq!(text, "I received your message: Hello");
    }

    /// Verify that assistant messages with multiple text blocks
    /// are concatenated correctly.
    #[test]
    fn test_assistant_multiple_text_blocks() {
        let line = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Part 1"},{"type":"text","text":" Part 2"},{"type":"text","text":" Part 3"}]},"session_id":"default"}"#;
        let msg = decode_message(line).unwrap();

        let text = msg.extract_text().unwrap();
        assert_eq!(text, "Part 1 Part 2 Part 3");
    }

    /// Verify that assistant messages with tool_use blocks
    /// still extract text from text blocks.
    #[test]
    fn test_assistant_mixed_content_blocks() {
        let line = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Let me run that."},{"type":"tool_use","id":"tool_1","name":"Bash","input":{"command":"ls"}},{"type":"text","text":" Done."}]},"session_id":"default"}"#;
        let msg = decode_message(line).unwrap();

        let text = msg.extract_text().unwrap();
        assert_eq!(text, "Let me run that. Done.");
    }

    /// Verify that result messages with text content are extracted.
    #[test]
    fn test_result_text_extraction() {
        let line = r#"{"type":"result","subtype":"success","result":{"content":[{"type":"text","text":"Final answer"}]},"session_id":"default"}"#;
        let msg = decode_message(line).unwrap();

        assert!(msg.is_result());
        assert!(msg.is_success_result());
        let text = msg.extract_text().unwrap();
        assert_eq!(text, "Final answer");
    }

    /// Verify result message with error subtype.
    #[test]
    fn test_result_error() {
        let line = r#"{"type":"result","subtype":"error","error":"Something went wrong","session_id":"default"}"#;
        let msg = decode_message(line).unwrap();

        assert!(msg.is_result());
        assert!(!msg.is_success_result());
        // Error results typically don't have text content
        assert!(msg.extract_text().is_none());
    }

    /// Verify content_block_start is parsed but has no extractable text.
    #[test]
    fn test_content_block_start() {
        let line =
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#;
        let msg = decode_message(line).unwrap();

        assert!(matches!(msg, ClaudeOutputMessage::ContentBlockStart(_)));
        assert!(msg.extract_text().is_none());
    }

    /// Verify content_block_stop is parsed but has no extractable text.
    #[test]
    fn test_content_block_stop() {
        let line = r#"{"type":"content_block_stop","index":0}"#;
        let msg = decode_message(line).unwrap();

        assert!(matches!(msg, ClaudeOutputMessage::ContentBlockStop(_)));
        assert!(msg.extract_text().is_none());
    }

    /// Verify control request from CLI (tool use permission) is parsed.
    #[test]
    fn test_control_request_tool_use() {
        let line = r#"{"type":"control_request","request":{"subtype":"can_use_tool","tool_name":"Bash","command":"ls -la"},"request_id":"req_tool_1"}"#;
        let msg = decode_message(line).unwrap();

        let ctrl_req = msg.as_control_request().unwrap();
        assert_eq!(ctrl_req.request.subtype, "can_use_tool");
        assert_eq!(ctrl_req.request_id.as_deref(), Some("req_tool_1"));
    }

    /// Simulate a full turn: user sends message, receives assistant + result.
    /// Verify the state transitions that would occur in the dispatcher.
    #[test]
    fn test_full_turn_state_transitions() {
        use crate::claude_session::state::{SessionState, SessionStateTracker};

        let tracker = SessionStateTracker::new();

        // Phase 1: Init
        tracker.transition(SessionState::Initializing).unwrap();

        // Receive control_response -> transition to Ready
        let init_resp_line = r#"{"type":"control_response","response":{"subtype":"initialize"},"request_id":"req_1"}"#;
        let msg = decode_message(init_resp_line).unwrap();
        assert!(msg.as_control_response().is_some());
        // Dispatcher would transition: Initializing -> Ready
        tracker.transition(SessionState::Ready).unwrap();
        assert_eq!(tracker.get(), SessionState::Ready);

        // Phase 2: User sends message -> Processing
        tracker.transition(SessionState::Processing).unwrap();
        assert_eq!(tracker.get(), SessionState::Processing);

        // Receive assistant message (state stays Processing)
        let assistant_line = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Hello"}]},"session_id":"default"}"#;
        let msg = decode_message(assistant_line).unwrap();
        assert!(msg.extract_text().is_some());
        // State stays Processing (no transition on assistant message)
        assert_eq!(tracker.get(), SessionState::Processing);

        // Receive result -> transition to Ready
        let result_line = r#"{"type":"result","subtype":"success","result":{"content":[{"type":"text","text":"Hello"}]},"session_id":"default"}"#;
        let msg = decode_message(result_line).unwrap();
        assert!(msg.is_result());
        assert!(msg.is_success_result());
        // Dispatcher would transition: Processing -> Ready
        tracker.transition(SessionState::Ready).unwrap();
        assert_eq!(tracker.get(), SessionState::Ready);
    }

    /// Simulate interrupt flow: user sends message, then interrupt.
    #[test]
    fn test_interrupt_state_transitions() {
        use crate::claude_session::state::{SessionState, SessionStateTracker};

        let tracker = SessionStateTracker::new();
        tracker.transition(SessionState::Initializing).unwrap();
        tracker.transition(SessionState::Ready).unwrap();
        tracker.transition(SessionState::Processing).unwrap();

        // Send interrupt -> Interrupting
        tracker.transition(SessionState::Interrupting).unwrap();
        assert_eq!(tracker.get(), SessionState::Interrupting);

        // Receive result -> Ready
        let result_line = r#"{"type":"result","subtype":"success","result":{"content":[{"type":"text","text":"Interrupted."}]},"session_id":"default"}"#;
        let msg = decode_message(result_line).unwrap();
        assert!(msg.is_result());
        // Dispatcher would transition: Interrupting -> Ready
        tracker.transition(SessionState::Ready).unwrap();
        assert_eq!(tracker.get(), SessionState::Ready);
    }

    /// Verify the encode/decode roundtrip for outgoing control requests.
    #[test]
    fn test_encode_decode_roundtrip_control_request() {
        use super::super::types::OutgoingControlRequest;

        let init = OutgoingControlRequest::initialize("req_roundtrip");
        let encoded = encode_message(&init).unwrap();

        // The encoded message should be valid JSON
        let parsed: serde_json::Value = serde_json::from_str(encoded.trim()).unwrap();
        assert_eq!(parsed["type"], "control_request");
        assert_eq!(parsed["request"]["subtype"], "initialize");
        assert_eq!(parsed["request_id"], "req_roundtrip");
    }

    /// Verify the encode/decode roundtrip for outgoing interrupt requests.
    #[test]
    fn test_encode_decode_roundtrip_interrupt() {
        use super::super::types::OutgoingControlRequest;

        let interrupt = OutgoingControlRequest::interrupt("req_int_roundtrip");
        let encoded = encode_message(&interrupt).unwrap();

        let parsed: serde_json::Value = serde_json::from_str(encoded.trim()).unwrap();
        assert_eq!(parsed["type"], "control_request");
        assert_eq!(parsed["request"]["subtype"], "interrupt");
        assert_eq!(parsed["request_id"], "req_int_roundtrip");
    }

    /// Verify the encode/decode roundtrip for control responses (tool approval).
    #[test]
    fn test_encode_decode_roundtrip_control_response() {
        use super::super::types::OutgoingControlResponse;

        let approval = OutgoingControlResponse::allow_tool("req_tool_roundtrip");
        let encoded = encode_message(&approval).unwrap();

        let parsed: serde_json::Value = serde_json::from_str(encoded.trim()).unwrap();
        assert_eq!(parsed["type"], "control_response");
        assert_eq!(parsed["request_id"], "req_tool_roundtrip");
        assert_eq!(parsed["response"]["allowed"], true);
    }

    /// Verify streaming text delta accumulation pattern.
    #[test]
    fn test_streaming_text_accumulation() {
        let deltas = vec![
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello "}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"world, "}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"how are you?"}}"#,
        ];

        let mut accumulated = String::new();
        for delta_line in &deltas {
            let msg = decode_message(delta_line).unwrap();
            if let Some(text) = msg.extract_text() {
                accumulated.push_str(&text);
            }
        }

        assert_eq!(accumulated, "Hello world, how are you?");
    }

    /// Verify that malformed JSON lines are rejected gracefully.
    #[test]
    fn test_malformed_json_rejection() {
        assert!(decode_message("not json at all").is_err());
        assert!(decode_message("{invalid json}").is_err());
        assert!(decode_message(r#"{"type":"unknown_type"}"#).is_err());
        assert!(decode_message(r#"{"no_type_field": true}"#).is_err());
    }

    /// Verify that result messages without content still parse.
    #[test]
    fn test_result_without_content() {
        let line = r#"{"type":"result","subtype":"success","session_id":"default"}"#;
        let msg = decode_message(line).unwrap();

        assert!(msg.is_result());
        assert!(msg.is_success_result());
        // No content -> no text
        assert!(msg.extract_text().is_none());
    }

    /// Verify result message with string result field (interactive mode format).
    #[test]
    fn test_result_string_field_interactive_mode() {
        // Interactive mode sends result as a plain string, not a structured object
        let line = r#"{"type":"result","subtype":"success","is_error":false,"duration_ms":685651,"duration_api_ms":508511,"num_turns":46,"result":"[STEP_COMPLETE:setup-0]\nDone with the task.","session_id":"default"}"#;
        let msg = decode_message(line).unwrap();

        assert!(msg.is_result());
        assert!(msg.is_success_result());
        let text = msg.extract_text().unwrap();
        assert!(text.contains("[STEP_COMPLETE:setup-0]"));
    }

    /// Verify whitespace handling in NDJSON lines.
    #[test]
    fn test_whitespace_handling() {
        // Leading/trailing whitespace should be trimmed
        let line = r#"  {"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"trimmed"}]},"session_id":"default"}  "#;
        let msg = decode_message(line).unwrap();
        assert_eq!(msg.extract_text().unwrap(), "trimmed");
    }
}

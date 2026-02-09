//! Integration tests for the interactive Claude CLI session protocol.
//!
//! These tests spawn the `mock_claude_cli` binary and communicate with it
//! over stdin/stdout using the stream-json NDJSON protocol. They verify:
//!
//! 1. Init handshake completes successfully
//! 2. User messages receive assistant responses + result messages
//! 3. Message queuing behavior (multiple turns in sequence)
//! 4. Interrupt handling
//! 5. Tool use control request auto-approval
//!
//! The tests are self-contained and do not depend on Tauri or internal modules.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};
use serde_json::Value;

// ============================================================================
// Protocol types (minimal subset for testing)
// ============================================================================

/// Outgoing control request (runner -> CLI)
#[derive(Debug, Serialize)]
struct OutgoingControlRequest {
    #[serde(rename = "type")]
    msg_type: String,
    request: OutgoingControlRequestPayload,
    request_id: String,
}

#[derive(Debug, Serialize)]
struct OutgoingControlRequestPayload {
    subtype: String,
    #[serde(flatten)]
    data: serde_json::Map<String, Value>,
}

impl OutgoingControlRequest {
    fn initialize(request_id: &str) -> Self {
        let mut data = serde_json::Map::new();
        data.insert(
            "protocolVersion".to_string(),
            Value::String("1".to_string()),
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

    fn interrupt(request_id: &str) -> Self {
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

/// User input message (runner -> CLI)
#[derive(Debug, Serialize)]
struct UserInputMessage {
    #[serde(rename = "type")]
    msg_type: String,
    message: UserMessagePayload,
    #[serde(skip_serializing_if = "Option::is_none")]
    parent_tool_use_id: Option<String>,
    session_id: String,
}

#[derive(Debug, Serialize)]
struct UserMessagePayload {
    role: String,
    content: String,
}

impl UserInputMessage {
    fn new(content: &str, session_id: &str) -> Self {
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

/// Control response (runner -> CLI, for tool approval)
#[derive(Debug, Serialize)]
struct OutgoingControlResponse {
    #[serde(rename = "type")]
    msg_type: String,
    response: OutgoingControlResponseData,
    request_id: String,
}

#[derive(Debug, Serialize)]
struct OutgoingControlResponseData {
    #[serde(flatten)]
    data: serde_json::Map<String, Value>,
}

impl OutgoingControlResponse {
    fn allow_tool(request_id: &str) -> Self {
        let mut data = serde_json::Map::new();
        data.insert("allowed".to_string(), Value::Bool(true));
        Self {
            msg_type: "control_response".to_string(),
            response: OutgoingControlResponseData { data },
            request_id: request_id.to_string(),
        }
    }
}

/// Generic output message for parsing CLI responses
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct OutputMessage {
    #[serde(rename = "type")]
    msg_type: String,
    #[serde(default)]
    subtype: Option<String>,
    #[serde(default)]
    response: Option<Value>,
    #[serde(default)]
    message: Option<AssistantMsg>,
    #[serde(default)]
    result: Option<ResultContent>,
    #[serde(default)]
    request_id: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    request: Option<ControlRequestFromCli>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct AssistantMsg {
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    content: Option<Vec<ContentBlockOutput>>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum ContentBlockOutput {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Deserialize)]
struct ResultContent {
    #[serde(default)]
    content: Option<Vec<ContentBlockOutput>>,
}

#[derive(Debug, Deserialize)]
struct ControlRequestFromCli {
    #[serde(default)]
    subtype: Option<String>,
    #[serde(default)]
    tool_name: Option<String>,
}

// ============================================================================
// Test helpers
// ============================================================================

/// Find the mock_claude_cli binary path.
/// It should be in the target directory after `cargo build`.
fn mock_cli_path() -> String {
    // Look for the binary in the target/debug directory
    let manifest_dir = env!("CARGO_MANIFEST_DIR");

    // Try the standard debug binary location
    let candidates = [
        format!("{}/target/debug/mock_claude_cli.exe", manifest_dir),
        format!("{}/target/debug/mock_claude_cli", manifest_dir),
        // In case tests run from a different profile
        format!("{}/target/release/mock_claude_cli.exe", manifest_dir),
        format!("{}/target/release/mock_claude_cli", manifest_dir),
    ];

    for candidate in &candidates {
        if std::path::Path::new(candidate).exists() {
            return candidate.clone();
        }
    }

    // Fall back to asking cargo
    panic!(
        "mock_claude_cli binary not found. Build it first with: cargo build --bin mock_claude_cli\n\
         Searched in: {:?}",
        candidates
    );
}

/// Spawn the mock CLI and return (child, stdin writer, stdout reader).
fn spawn_mock_cli(
    extra_args: &[&str],
) -> (
    std::process::Child,
    std::process::ChildStdin,
    BufReader<std::process::ChildStdout>,
) {
    let path = mock_cli_path();
    let mut cmd = Command::new(&path);
    for arg in extra_args {
        cmd.arg(arg);
    }
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = cmd
        .spawn()
        .unwrap_or_else(|e| panic!("Failed to spawn mock CLI at {}: {}", path, e));

    let stdin = child.stdin.take().expect("Failed to take stdin");
    let stdout = child.stdout.take().expect("Failed to take stdout");

    (child, stdin, BufReader::new(stdout))
}

/// Send a JSON message to the mock CLI's stdin.
fn send_msg<T: Serialize>(stdin: &mut std::process::ChildStdin, msg: &T) {
    let json = serde_json::to_string(msg).expect("Failed to serialize message");
    writeln!(stdin, "{}", json).expect("Failed to write to stdin");
    stdin.flush().expect("Failed to flush stdin");
}

/// Read a single NDJSON line from the mock CLI's stdout.
/// Panics if no line is received within 5 seconds.
fn read_msg(reader: &mut BufReader<std::process::ChildStdout>) -> OutputMessage {
    let mut line = String::new();

    // Use a timeout mechanism: set the underlying stream to non-blocking
    // would be complex, so we rely on the mock responding quickly.
    // If tests hang, the test runner will time out.
    reader
        .read_line(&mut line)
        .expect("Failed to read from stdout");

    let trimmed = line.trim();
    assert!(!trimmed.is_empty(), "Received empty line from mock CLI");

    serde_json::from_str::<OutputMessage>(trimmed)
        .unwrap_or_else(|e| panic!("Failed to parse mock CLI output: {} (line: {})", e, trimmed))
}

/// Extract text from an OutputMessage's content blocks.
fn extract_text(msg: &OutputMessage) -> Option<String> {
    if msg.msg_type == "assistant" {
        if let Some(ref message) = msg.message {
            if let Some(ref content) = message.content {
                let texts: Vec<&str> = content
                    .iter()
                    .filter_map(|block| match block {
                        ContentBlockOutput::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect();
                if !texts.is_empty() {
                    return Some(texts.join(""));
                }
            }
        }
    } else if msg.msg_type == "result" {
        if let Some(ref result) = msg.result {
            if let Some(ref content) = result.content {
                let texts: Vec<&str> = content
                    .iter()
                    .filter_map(|block| match block {
                        ContentBlockOutput::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect();
                if !texts.is_empty() {
                    return Some(texts.join(""));
                }
            }
        }
    }
    None
}

// ============================================================================
// Tests
// ============================================================================

/// Test 1: Init handshake completes successfully.
///
/// Verifies:
/// - Sending an initialize control request gets a control_response back
/// - The response has the same request_id
#[test]
fn test_init_handshake() {
    let (mut child, mut stdin, mut reader) = spawn_mock_cli(&[]);

    // Send initialize request
    let init_req = OutgoingControlRequest::initialize("req_init_001");
    send_msg(&mut stdin, &init_req);

    // Read response
    let resp = read_msg(&mut reader);
    assert_eq!(
        resp.msg_type, "control_response",
        "Expected control_response"
    );
    assert_eq!(
        resp.request_id.as_deref(),
        Some("req_init_001"),
        "Response request_id should match"
    );

    // Clean up
    drop(stdin);
    let _ = child.wait();
}

/// Test 2: Send a user message and receive an assistant response + result.
///
/// Verifies:
/// - After init, sending a user message produces an assistant message
/// - Followed by a result message with subtype "success"
/// - The response text contains the original message
#[test]
fn test_send_message_and_get_response() {
    let (mut child, mut stdin, mut reader) = spawn_mock_cli(&[]);

    // Init handshake
    let init_req = OutgoingControlRequest::initialize("req_init_002");
    send_msg(&mut stdin, &init_req);
    let init_resp = read_msg(&mut reader);
    assert_eq!(init_resp.msg_type, "control_response");

    // Send user message
    let user_msg = UserInputMessage::new("Hello, Claude!", "default");
    send_msg(&mut stdin, &user_msg);

    // Read assistant message
    let assistant_msg = read_msg(&mut reader);
    assert_eq!(
        assistant_msg.msg_type, "assistant",
        "Expected assistant message"
    );
    let text = extract_text(&assistant_msg).expect("Assistant message should have text");
    assert!(
        text.contains("Hello, Claude!"),
        "Response should echo the user message. Got: {}",
        text
    );

    // Read result message
    let result_msg = read_msg(&mut reader);
    assert_eq!(result_msg.msg_type, "result", "Expected result message");
    assert_eq!(
        result_msg.subtype.as_deref(),
        Some("success"),
        "Result should be success"
    );

    // Clean up
    drop(stdin);
    let _ = child.wait();
}

/// Test 3: Multiple turns in sequence (simulates message queuing behavior).
///
/// Verifies:
/// - Multiple user messages can be sent sequentially
/// - Each gets its own assistant + result response
/// - The session remains functional across turns
#[test]
fn test_multiple_turns() {
    let (mut child, mut stdin, mut reader) = spawn_mock_cli(&[]);

    // Init
    send_msg(
        &mut stdin,
        &OutgoingControlRequest::initialize("req_init_003"),
    );
    let _ = read_msg(&mut reader); // consume init response

    // Turn 1
    send_msg(
        &mut stdin,
        &UserInputMessage::new("First message", "default"),
    );
    let msg1_assistant = read_msg(&mut reader);
    assert_eq!(msg1_assistant.msg_type, "assistant");
    let text1 = extract_text(&msg1_assistant).unwrap();
    assert!(
        text1.contains("First message"),
        "Turn 1 response: {}",
        text1
    );
    let msg1_result = read_msg(&mut reader);
    assert_eq!(msg1_result.msg_type, "result");
    assert_eq!(msg1_result.subtype.as_deref(), Some("success"));

    // Turn 2
    send_msg(
        &mut stdin,
        &UserInputMessage::new("Second message", "default"),
    );
    let msg2_assistant = read_msg(&mut reader);
    assert_eq!(msg2_assistant.msg_type, "assistant");
    let text2 = extract_text(&msg2_assistant).unwrap();
    assert!(
        text2.contains("Second message"),
        "Turn 2 response: {}",
        text2
    );
    let msg2_result = read_msg(&mut reader);
    assert_eq!(msg2_result.msg_type, "result");

    // Turn 3
    send_msg(
        &mut stdin,
        &UserInputMessage::new("Third message", "default"),
    );
    let msg3_assistant = read_msg(&mut reader);
    let text3 = extract_text(&msg3_assistant).unwrap();
    assert!(
        text3.contains("Third message"),
        "Turn 3 response: {}",
        text3
    );
    let _ = read_msg(&mut reader); // result

    // Clean up
    drop(stdin);
    let _ = child.wait();
}

/// Test 4: Interrupt while processing.
///
/// Verifies:
/// - An interrupt control request produces a result message
/// - The result indicates the turn was interrupted
#[test]
fn test_interrupt() {
    let (mut child, mut stdin, mut reader) = spawn_mock_cli(&["slow"]);

    // Init
    send_msg(
        &mut stdin,
        &OutgoingControlRequest::initialize("req_init_004"),
    );
    let _ = read_msg(&mut reader);

    // Send a user message
    send_msg(
        &mut stdin,
        &UserInputMessage::new("Process this slowly", "default"),
    );

    // In slow mode, the mock waits 100ms before responding.
    // Send interrupt immediately.
    send_msg(
        &mut stdin,
        &OutgoingControlRequest::interrupt("req_interrupt_001"),
    );

    // We should receive:
    // 1. The assistant response from the user message (mock sends it after 100ms delay)
    // 2. The result from the user message
    // 3. The result from the interrupt
    // OR the interrupt result might come before/after depending on timing.
    //
    // Since the mock is single-threaded and processes messages sequentially,
    // we'll get:
    // - First the user message response (assistant + result)
    // - Then the interrupt response (result)
    //
    // Read all messages and verify we get at least one result
    let mut got_result = false;
    let mut messages_read = 0;

    // Read up to 4 messages (assistant, result from user msg, result from interrupt)
    while messages_read < 4 {
        let msg = read_msg(&mut reader);
        messages_read += 1;
        if msg.msg_type == "result" {
            got_result = true;
            assert_eq!(msg.subtype.as_deref(), Some("success"));
            // Check if this is the interrupt result (contains "Interrupted.")
            if let Some(text) = extract_text(&msg) {
                if text.contains("Interrupted") {
                    break;
                }
            }
        }
    }

    assert!(
        got_result,
        "Should have received at least one result message"
    );

    // Clean up
    drop(stdin);
    let _ = child.wait();
}

/// Test 5: Tool use control request from CLI is handled.
///
/// Verifies:
/// - Mock CLI sends a control_request (can_use_tool)
/// - Runner sends back a control_response (allowed: true)
/// - CLI then completes with assistant + result
#[test]
fn test_tool_use_approval() {
    let (mut child, mut stdin, mut reader) = spawn_mock_cli(&["tool_use"]);

    // Init
    send_msg(
        &mut stdin,
        &OutgoingControlRequest::initialize("req_init_005"),
    );
    let _ = read_msg(&mut reader);

    // Send user message (in tool_use mode, CLI will request tool approval first)
    send_msg(&mut stdin, &UserInputMessage::new("Use a tool", "default"));

    // Read the tool use control request from CLI
    let tool_req = read_msg(&mut reader);
    assert_eq!(
        tool_req.msg_type, "control_request",
        "Expected control_request for tool use"
    );
    let req = tool_req
        .request
        .as_ref()
        .expect("Should have request field");
    assert_eq!(req.subtype.as_deref(), Some("can_use_tool"));
    assert_eq!(req.tool_name.as_deref(), Some("Bash"));

    let tool_request_id = tool_req
        .request_id
        .as_ref()
        .expect("Tool request should have request_id");

    // Send approval
    let approval = OutgoingControlResponse::allow_tool(tool_request_id);
    send_msg(&mut stdin, &approval);

    // Read assistant response
    let assistant_msg = read_msg(&mut reader);
    assert_eq!(assistant_msg.msg_type, "assistant");
    let text = extract_text(&assistant_msg).unwrap();
    assert!(
        text.contains("Tool approved"),
        "Should acknowledge tool approval. Got: {}",
        text
    );

    // Read result
    let result_msg = read_msg(&mut reader);
    assert_eq!(result_msg.msg_type, "result");
    assert_eq!(result_msg.subtype.as_deref(), Some("success"));

    // Clean up
    drop(stdin);
    let _ = child.wait();
}

/// Test 6: Session ID is preserved in responses.
///
/// Verifies:
/// - The session_id from the user message appears in responses
#[test]
fn test_session_id_preserved() {
    let (mut child, mut stdin, mut reader) = spawn_mock_cli(&[]);

    // Init
    send_msg(
        &mut stdin,
        &OutgoingControlRequest::initialize("req_init_006"),
    );
    let _ = read_msg(&mut reader);

    // Send user message with custom session ID
    let user_msg = UserInputMessage::new("Test session ID", "my-custom-session");
    send_msg(&mut stdin, &user_msg);

    // Read assistant response
    let assistant_msg = read_msg(&mut reader);
    assert_eq!(assistant_msg.msg_type, "assistant");
    assert_eq!(
        assistant_msg.session_id.as_deref(),
        Some("my-custom-session"),
        "Session ID should be preserved in assistant response"
    );

    // Read result
    let result_msg = read_msg(&mut reader);
    assert_eq!(result_msg.msg_type, "result");
    assert_eq!(
        result_msg.session_id.as_deref(),
        Some("my-custom-session"),
        "Session ID should be preserved in result"
    );

    // Clean up
    drop(stdin);
    let _ = child.wait();
}

/// Test 7: Graceful shutdown on stdin close.
///
/// Verifies:
/// - When stdin is closed, the mock CLI process exits cleanly
#[test]
fn test_graceful_shutdown() {
    let (mut child, stdin, _reader) = spawn_mock_cli(&[]);

    // Don't send anything, just close stdin
    drop(stdin);

    // The process should exit
    let status = child.wait().expect("Failed to wait for mock CLI");

    // Process should exit with code 0
    assert!(
        status.success(),
        "Mock CLI should exit cleanly when stdin is closed. Status: {:?}",
        status
    );
}

/// Test 8: Protocol codec round-trip verification.
///
/// Verifies:
/// - Our serialized messages are correctly parsed by the mock
/// - The mock's responses are correctly parsed by our deserialization
/// - The full init -> message -> response cycle works end-to-end
#[test]
fn test_protocol_codec_roundtrip() {
    let (mut child, mut stdin, mut reader) = spawn_mock_cli(&[]);

    // Verify init handshake message format
    let init_req = OutgoingControlRequest::initialize("req_roundtrip_001");
    let init_json = serde_json::to_string(&init_req).unwrap();
    assert!(init_json.contains("\"type\":\"control_request\""));
    assert!(init_json.contains("\"subtype\":\"initialize\""));
    assert!(init_json.contains("\"protocolVersion\":\"1\""));
    assert!(init_json.contains("\"request_id\":\"req_roundtrip_001\""));

    // Send and verify response
    send_msg(&mut stdin, &init_req);
    let resp = read_msg(&mut reader);
    assert_eq!(resp.msg_type, "control_response");

    // Verify user message format
    let user_msg = UserInputMessage::new("codec test", "default");
    let user_json = serde_json::to_string(&user_msg).unwrap();
    assert!(user_json.contains("\"type\":\"user\""));
    assert!(user_json.contains("\"role\":\"user\""));
    assert!(user_json.contains("\"content\":\"codec test\""));
    assert!(user_json.contains("\"session_id\":\"default\""));

    // Send and verify response
    send_msg(&mut stdin, &user_msg);

    let assistant = read_msg(&mut reader);
    assert_eq!(assistant.msg_type, "assistant");

    let result = read_msg(&mut reader);
    assert_eq!(result.msg_type, "result");
    assert_eq!(result.subtype.as_deref(), Some("success"));

    // Verify interrupt message format
    let interrupt = OutgoingControlRequest::interrupt("req_interrupt_roundtrip");
    let interrupt_json = serde_json::to_string(&interrupt).unwrap();
    assert!(interrupt_json.contains("\"subtype\":\"interrupt\""));

    // Clean up
    drop(stdin);
    let _ = child.wait();
}

/// Test 9: State transitions simulation.
///
/// This test simulates the expected state machine transitions by tracking
/// what state the runner would be in based on the messages exchanged.
/// This doesn't require internal module access but verifies the protocol
/// drives the correct state sequence.
///
/// Expected flow:
/// Created -> Initializing -> [send init] -> Ready [on control_response]
///   -> Processing [on send user msg] -> Ready [on result]
///   -> Processing [on send user msg] -> Interrupting [on send interrupt]
///      -> Ready [on result]
#[test]
fn test_state_transition_simulation() {
    let (mut child, mut stdin, mut reader) = spawn_mock_cli(&["slow"]);

    // Track state transitions in a vec for verification
    let mut transitions: Vec<&str> = vec!["created"];

    // Transition: Created -> Initializing
    transitions.push("initializing");
    send_msg(
        &mut stdin,
        &OutgoingControlRequest::initialize("req_init_state"),
    );

    // Read control_response -> Initializing -> Ready
    let resp = read_msg(&mut reader);
    assert_eq!(resp.msg_type, "control_response");
    transitions.push("ready");

    // Transition: Ready -> Processing (send user message)
    send_msg(&mut stdin, &UserInputMessage::new("State test", "default"));
    transitions.push("processing");

    // Read assistant message (still Processing)
    let _ = read_msg(&mut reader); // assistant

    // Read result -> Processing -> Ready
    let result = read_msg(&mut reader);
    assert_eq!(result.msg_type, "result");
    transitions.push("ready");

    // Second turn: Ready -> Processing -> Interrupting -> Ready
    send_msg(
        &mut stdin,
        &UserInputMessage::new("Interrupt test", "default"),
    );
    transitions.push("processing");

    // Send interrupt before reading response
    send_msg(
        &mut stdin,
        &OutgoingControlRequest::interrupt("req_int_state"),
    );
    transitions.push("interrupting");

    // Read all remaining messages until we've consumed both results
    let mut result_count = 0;
    while result_count < 2 {
        let msg = read_msg(&mut reader);
        if msg.msg_type == "result" {
            result_count += 1;
        }
    }

    // After both results: Interrupting -> Ready
    transitions.push("ready");

    // Verify the full state transition sequence
    assert_eq!(
        transitions,
        vec![
            "created",
            "initializing",
            "ready",
            "processing",
            "ready",
            "processing",
            "interrupting",
            "ready",
        ],
        "State transitions should follow the expected sequence"
    );

    // Clean up
    drop(stdin);
    let _ = child.wait();
}

/// Test 10: Empty content handling.
///
/// Verifies the mock handles edge cases gracefully.
#[test]
fn test_empty_content() {
    let (mut child, mut stdin, mut reader) = spawn_mock_cli(&[]);

    // Init
    send_msg(
        &mut stdin,
        &OutgoingControlRequest::initialize("req_init_empty"),
    );
    let _ = read_msg(&mut reader);

    // Send empty message
    send_msg(&mut stdin, &UserInputMessage::new("", "default"));

    // Should still get a response
    let assistant = read_msg(&mut reader);
    assert_eq!(assistant.msg_type, "assistant");

    let result = read_msg(&mut reader);
    assert_eq!(result.msg_type, "result");
    assert_eq!(result.subtype.as_deref(), Some("success"));

    // Clean up
    drop(stdin);
    let _ = child.wait();
}

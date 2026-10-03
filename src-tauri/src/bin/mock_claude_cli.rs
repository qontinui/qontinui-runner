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
//!
//! **`--replay <fixture.ndjson> [--exit-code N]`** replays a recorded CLI
//! stdout (plan `2026-09-20-ai-session-handling-is-claude-shaped-…` Phase 8;
//! the fixtures are Phase 2's `tests/fixtures/cli_protocol/claude/<ver>/`),
//! honoring the order the real CLI answered in — see [`replay`].

use std::io::{self, BufRead, Write};
use std::path::Path;

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

// ============================================================================
// --replay: a recorded CLI session, frame for frame
// ============================================================================

/// The value after `flag` in `args`, if present.
fn flag_value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .map(String::as_str)
}

/// The scenario the fixture's sibling `manifest.json` records for it (the
/// entry whose `stdout` names the fixture file), if there is one.
fn manifest_scenario(fixture: &Path) -> Option<Value> {
    let manifest: Value = serde_json::from_str(
        &std::fs::read_to_string(fixture.parent()?.join("manifest.json")).ok()?,
    )
    .ok()?;
    let name = fixture.file_name()?.to_str()?;
    manifest
        .get("scenarios")?
        .as_object()?
        .values()
        .find(|sc| sc.get("stdout").and_then(Value::as_str) == Some(name))
        .cloned()
}

/// A process exit status as an exit code: a recorded signal death (`-9`)
/// becomes the shell convention `128 + signal`.
fn as_exit_code(recorded: i64) -> i32 {
    if recorded < 0 {
        128 + (-recorded).min(127) as i32
    } else {
        recorded.min(255) as i32
    }
}

/// Next non-empty stdin frame parsed as JSON; `None` at EOF. A line that is
/// not JSON is reported on stderr and skipped.
fn next_stdin_frame(lines: &mut impl Iterator<Item = io::Result<String>>) -> Option<Value> {
    for line in lines {
        let line = line.ok()?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        match serde_json::from_str(trimmed) {
            Ok(v) => return Some(v),
            Err(e) => eprintln!("mock_claude_cli --replay: unparseable stdin line: {e}"),
        }
    }
    None
}

/// Read stdin until a frame `pred` accepts; `None` at EOF. Frames it does not
/// accept are reported on stderr — the recorded CLI did not see them.
fn await_stdin_frame(
    lines: &mut impl Iterator<Item = io::Result<String>>,
    what: &str,
    pred: impl Fn(&Value) -> bool,
) -> Option<Value> {
    loop {
        let frame = next_stdin_frame(lines)?;
        if pred(&frame) {
            return Some(frame);
        }
        eprintln!(
            "mock_claude_cli --replay: ignoring stdin frame of type {:?} while awaiting {what}",
            frame.get("type").and_then(Value::as_str).unwrap_or("?")
        );
    }
}

fn frame_type(v: &Value) -> Option<&str> {
    v.get("type").and_then(Value::as_str)
}

fn write_raw_line(line: &str) {
    let stdout = io::stdout();
    let mut handle = stdout.lock();
    writeln!(handle, "{line}").expect("Failed to write to stdout");
    handle.flush().expect("Failed to flush stdout");
}

/// Replay `fixture` (a recorded CLI stdout) the way the CLI produced it,
/// returning the process exit code:
///
/// 1. await the runner's `initialize` control request, then write the fixture
///    through its first `control_response` (the handshake answer), with the
///    recorded `request_id` rewritten to the one the runner sent;
/// 2. await the runner's `user` message, then write the rest of the turn —
///    after each `control_request` the CLI sent (a `can_use_tool`), await the
///    runner's `control_response` before writing on, because the CLI waits;
/// 3. like the CLI, exit at stdin EOF, with the recorded exit code
///    (`--exit-code N` overrides the fixture's `manifest.json`). A scenario
///    the probe had to kill (`killed_by_probe_watchdog`) hangs as the CLI did,
///    bounded, then exits `128 + signal`.
fn replay(fixture: &Path, exit_override: Option<i64>) -> i32 {
    let text = match std::fs::read_to_string(fixture) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("mock_claude_cli --replay: cannot read {}: {e}", fixture.display());
            return 2;
        }
    };
    let scenario = manifest_scenario(fixture);
    let recorded_exit = exit_override
        .or_else(|| scenario.as_ref()?.get("exit_code")?.as_i64())
        .unwrap_or(0);
    let killed = scenario
        .as_ref()
        .and_then(|sc| sc.get("killed_by_probe_watchdog")?.as_bool())
        .unwrap_or(false);
    let exit_code = as_exit_code(recorded_exit);

    let frames: Vec<(&str, Value)> = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok().map(|v| (l, v)))
        .collect();
    let handshake_end = frames
        .iter()
        .position(|(_, v)| frame_type(v) == Some("control_response"))
        .map_or(0, |i| i + 1);

    let stdin = io::stdin();
    let mut lines = stdin.lock().lines();

    // 1. The init handshake.
    let Some(init) = await_stdin_frame(&mut lines, "initialize", |v| {
        frame_type(v) == Some("control_request")
            && v.pointer("/request/subtype").and_then(Value::as_str) == Some("initialize")
    }) else {
        return exit_code;
    };
    let runner_request_id = init.get("request_id").and_then(Value::as_str).unwrap_or_default();
    for (line, v) in &frames[..handshake_end] {
        let recorded_id = v.pointer("/response/request_id").and_then(Value::as_str);
        match recorded_id {
            Some(rid) if frame_type(v) == Some("control_response") && rid != runner_request_id => {
                write_raw_line(&line.replacen(
                    &format!("\"request_id\":\"{rid}\""),
                    &format!("\"request_id\":\"{runner_request_id}\""),
                    1,
                ));
            }
            _ => write_raw_line(line),
        }
    }

    // 2. The turn.
    if await_stdin_frame(&mut lines, "a user message", |v| frame_type(v) == Some("user")).is_none() {
        return exit_code;
    }
    for (line, v) in &frames[handshake_end..] {
        write_raw_line(line);
        if frame_type(v) == Some("control_request") {
            let awaited = await_stdin_frame(&mut lines, "a control_response", |r| {
                frame_type(r) == Some("control_response")
            });
            if awaited.is_none() {
                return exit_code;
            }
        }
    }

    // 3. Exit as the CLI did.
    while next_stdin_frame(&mut lines).is_some() {}
    if killed {
        // The CLI never exited on its own; the probe killed it.
        std::thread::sleep(std::time::Duration::from_secs(600));
    }
    exit_code
}

fn main() {
    // Check for special modes via command-line args
    let args: Vec<String> = std::env::args().collect();
    if let Some(fixture) = flag_value(&args, "--replay") {
        let exit_override = match flag_value(&args, "--exit-code").map(str::parse::<i64>) {
            Some(Ok(code)) => Some(code),
            Some(Err(e)) => {
                eprintln!("mock_claude_cli: --exit-code: {e}");
                std::process::exit(2);
            }
            None => None,
        };
        std::process::exit(replay(Path::new(fixture), exit_override));
    }
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

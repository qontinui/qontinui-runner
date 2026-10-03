//! `mock_claude_cli --replay` over every recorded fixture — plan
//! `2026-09-20-ai-session-handling-is-claude-shaped-provider-manifest-and-failure-taxonomy`,
//! Phase 8.
//!
//! The mock is driven with the very frames the probe sent (`<stem>.stdin.ndjson`)
//! in the order the CLI consumed them — `initialize`, then the `user` message,
//! then one `control_response` per `can_use_tool` it asks — and must write the
//! recorded stdout byte for byte, then exit at stdin EOF with the recorded exit
//! code. The dispatcher's own fixture tests
//! (`claude_session::dispatcher::tests`) replay the same files through the
//! frame logic; this keeps the replaying mock honest.

use super::*;

/// Scenarios this harness replays. Every `<stem>.ndjson` in the pinned
/// directory must be listed — [`every_fixture_has_a_replay_test`] checks.
const REPLAYED: &[&str] = &[
    "plain_turn",
    "errored_turn_invalid_model",
    "can_use_tool_runner_shape_ignored",
    "can_use_tool_sdk_allow_with_session_state_events",
    "can_use_tool_sdk_deny",
];

fn mock_bin() -> &'static str {
    env!("CARGO_BIN_EXE_mock_claude_cli")
}

fn manifest_scenario(stem: &str) -> Value {
    let manifest: Value = serde_json::from_str(
        &std::fs::read_to_string(claude_fixture_dir().join("manifest.json")).unwrap(),
    )
    .unwrap();
    manifest["scenarios"][stem].clone()
}

fn fixture_lines(stem: &str) -> Vec<String> {
    std::fs::read_to_string(claude_fixture_dir().join(format!("{stem}.ndjson")))
        .unwrap()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(str::to_string)
        .collect()
}

struct Mock {
    child: Child,
    rx: mpsc::Receiver<String>,
}

impl Mock {
    fn spawn(extra: &[&str], stem: &str) -> Self {
        let fixture = claude_fixture_dir().join(format!("{stem}.ndjson"));
        let mut child = Command::new(mock_bin())
            .arg("--replay")
            .arg(&fixture)
            .args(extra)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn mock_claude_cli");
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        Self { child, rx }
    }

    fn send(&mut self, frame: &Value) {
        let stdin = self.child.stdin.as_mut().expect("stdin open");
        writeln!(stdin, "{frame}").unwrap();
        stdin.flush().unwrap();
    }

    fn recv(&self) -> Option<String> {
        self.rx.recv_timeout(Duration::from_secs(30)).ok()
    }

    /// Nothing more arrives within `wait` — the mock is waiting on stdin.
    fn quiet_for(&self, wait: Duration) -> bool {
        self.rx.recv_timeout(wait).is_err()
    }

    fn close_stdin(&mut self) {
        drop(self.child.stdin.take());
    }

    fn wait_exit(&mut self, within: Duration) -> Option<i32> {
        let deadline = Instant::now() + within;
        loop {
            if let Ok(Some(status)) = self.child.try_wait() {
                return status.code();
            }
            if Instant::now() > deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Mock {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn expected_exit(stem: &str) -> i32 {
    let code = manifest_scenario(stem)["exit_code"].as_i64().unwrap();
    if code < 0 {
        128 + (-code) as i32
    } else {
        code as i32
    }
}

/// Drive one fixture through the mock the way the probe drove the CLI and
/// assert the replay is faithful.
fn replay_and_check(stem: &str) {
    let expected = fixture_lines(stem);
    let stdin = fixture(&format!("{stem}.stdin.ndjson"));
    let mut mock = Mock::spawn(&[], stem);
    let mut got = Vec::new();

    // Ordering is honoured: nothing before the initialize request…
    assert!(
        mock.quiet_for(Duration::from_millis(200)),
        "{stem}: wrote before initialize"
    );
    mock.send(&stdin[0]);
    // …the handshake answer, and nothing of the turn before the user message.
    let handshake = mock.recv().expect("handshake answer");
    assert!(
        handshake.contains("\"type\":\"control_response\""),
        "{stem}"
    );
    got.push(handshake);
    assert!(
        mock.quiet_for(Duration::from_millis(200)),
        "{stem}: turn began before the user message"
    );
    mock.send(&stdin[1]);

    let mut replies = stdin[2..].iter();
    while got.len() < expected.len() {
        let line = mock.recv().unwrap_or_else(|| {
            panic!(
                "{stem}: stalled after {} of {} lines",
                got.len(),
                expected.len()
            )
        });
        let is_request = serde_json::from_str::<Value>(&line)
            .map(|v| is_type(&v, "control_request"))
            .unwrap_or(false);
        got.push(line);
        if is_request {
            // The CLI waits for the answer — so does the mock.
            assert!(
                mock.quiet_for(Duration::from_millis(200)),
                "{stem}: did not wait for the reply"
            );
            mock.send(
                replies
                    .next()
                    .expect("a recorded reply to the control_request"),
            );
        }
    }
    assert_eq!(got, expected, "{stem}: replay is byte-for-byte");

    // Like the CLI, it exits at stdin EOF with the recorded code.
    mock.close_stdin();
    if manifest_scenario(stem)["killed_by_probe_watchdog"] == true {
        assert_eq!(
            mock.wait_exit(Duration::from_millis(500)),
            None,
            "{stem}: the recorded CLI hung until killed; so does the replay"
        );
    } else {
        assert_eq!(
            mock.wait_exit(Duration::from_secs(10)),
            Some(expected_exit(stem)),
            "{stem}"
        );
    }
}

#[test]
fn mock_replay_plain_turn() {
    replay_and_check("plain_turn");
}

#[test]
fn mock_replay_errored_turn_invalid_model_exits_1() {
    assert_eq!(expected_exit("errored_turn_invalid_model"), 1);
    replay_and_check("errored_turn_invalid_model");
}

#[test]
fn mock_replay_can_use_tool_runner_shape_ignored() {
    replay_and_check("can_use_tool_runner_shape_ignored");
}

#[test]
fn mock_replay_can_use_tool_sdk_allow_with_session_state_events() {
    replay_and_check("can_use_tool_sdk_allow_with_session_state_events");
}

#[test]
fn mock_replay_can_use_tool_sdk_deny() {
    replay_and_check("can_use_tool_sdk_deny");
}

#[test]
fn every_fixture_has_a_replay_test() {
    let mut stems: Vec<String> = std::fs::read_dir(claude_fixture_dir())
        .unwrap()
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| n.ends_with(".ndjson") && !n.ends_with(".stdin.ndjson"))
        .map(|n| n.trim_end_matches(".ndjson").to_string())
        .collect();
    stems.sort();
    let mut listed: Vec<String> = REPLAYED.iter().map(|s| s.to_string()).collect();
    listed.sort();
    assert_eq!(stems, listed);
}

/// The runner's own request id replaces the recorded one in the handshake
/// answer, and `--exit-code` overrides the manifest.
#[test]
fn mock_replay_rewrites_the_init_request_id_and_honours_exit_code_override() {
    let mut mock = Mock::spawn(&["--exit-code", "3"], "plain_turn");
    mock.send(&json!({
        "type": "control_request",
        "request": {"subtype": "initialize", "protocolVersion": "1"},
        "request_id": "req_runner_42"
    }));
    let handshake: Value = serde_json::from_str(&mock.recv().unwrap()).unwrap();
    assert_eq!(handshake["response"]["request_id"], "req_runner_42");
    mock.send(&json!({
        "type": "user", "message": {"role": "user", "content": "x"}, "session_id": "default"
    }));
    let n = fixture_lines("plain_turn").len() - 1;
    for _ in 0..n {
        mock.recv().expect("turn line");
    }
    mock.close_stdin();
    assert_eq!(mock.wait_exit(Duration::from_secs(10)), Some(3));
}

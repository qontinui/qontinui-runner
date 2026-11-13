//! CLI protocol probes — plan
//! `2026-09-20-ai-session-handling-is-claude-shaped-provider-manifest-and-failure-taxonomy`,
//! Phase 2.
//!
//! Two kinds of test live here:
//!
//! * **Fixture tests (run in CI).** Every checked-in frame under
//!   `tests/fixtures/cli_protocol/<cli>/<version>/*.ndjson` parses as JSON, the
//!   fixtures are redacted, and the recorded *answers* the plan's decision
//!   rules hang on still hold in the recorded frames (the accepted
//!   `can_use_tool` response shape, `is_error` under `subtype: "success"`, the
//!   `rate_limit_event` / `session_state_changed` frames). Phases 8 and 9
//!   replay these fixtures; these tests keep them honest.
//! * **Live probes (`#[ignore]`, never in CI).** They need a logged-in `claude`
//!   on `PATH` (override with `CLI_PROBES_CLAUDE=<path>`), spend a few cents of
//!   Haiku, and re-run the Q1–Q3 scenarios in a fresh empty temp dir, driving
//!   the CLI with the same frames the runner's structured lane sends
//!   (`claude_protocol/types.rs`: `initialize` control request, then a `user`
//!   message with `session_id: "default"`). Output is recorded into a temp dir
//!   and printed; with `CLI_PROBES_RECORD=1` it is redacted and written over
//!   `tests/fixtures/cli_protocol/claude/<installed version>/` instead.
//!
//! Run the live probes with:
//! `cargo test --test cli_probes -- --ignored --test-threads=1 --nocapture`

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

mod mock_replay;

// ─────────────────────────────────────────────────────────────────────────────
// Shared helpers
// ─────────────────────────────────────────────────────────────────────────────

fn fixtures_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/cli_protocol")
}

/// The Claude fixture set these tests pin. A new recording lands in a new
/// `<version>` directory; bump this when the pinned set moves.
const PINNED_CLAUDE_VERSION: &str = "2.1.285";

fn claude_fixture_dir() -> PathBuf {
    fixtures_root().join("claude").join(PINNED_CLAUDE_VERSION)
}

fn read_frames(path: &Path) -> Vec<Value> {
    let text =
        std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .enumerate()
        .map(|(i, l)| {
            serde_json::from_str(l)
                .unwrap_or_else(|e| panic!("{}:{} is not JSON: {e}", path.display(), i + 1))
        })
        .collect()
}

fn fixture(name: &str) -> Vec<Value> {
    read_frames(&claude_fixture_dir().join(name))
}

fn is_type(f: &Value, t: &str) -> bool {
    f.get("type").and_then(Value::as_str) == Some(t)
}

fn is_system(f: &Value, subtype: &str) -> bool {
    is_type(f, "system") && f.get("subtype").and_then(Value::as_str) == Some(subtype)
}

fn result_frame(frames: &[Value]) -> Option<&Value> {
    frames.iter().find(|f| is_type(f, "result"))
}

fn can_use_tool_request(frames: &[Value]) -> Option<&Value> {
    frames.iter().find(|f| {
        is_type(f, "control_request")
            && f.pointer("/request/subtype").and_then(Value::as_str) == Some("can_use_tool")
    })
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in
        std::fs::read_dir(dir).unwrap_or_else(|e| panic!("read_dir {}: {e}", dir.display()))
    {
        let p = entry.unwrap().path();
        if p.is_dir() {
            walk(&p, out);
        } else {
            out.push(p);
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Fixture tests — cheap, run in CI
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn every_checked_in_fixture_line_parses_as_json() {
    let mut files = Vec::new();
    walk(&fixtures_root(), &mut files);
    let ndjson: Vec<_> = files
        .iter()
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("ndjson"))
        .collect();
    assert!(
        !ndjson.is_empty(),
        "no .ndjson fixtures under {}",
        fixtures_root().display()
    );
    for p in ndjson {
        let frames = read_frames(p);
        assert!(!frames.is_empty(), "{} is empty", p.display());
        for (i, f) in frames.iter().enumerate() {
            assert!(
                f.get("type").and_then(Value::as_str).is_some(),
                "{}:{} has no string `type`",
                p.display(),
                i + 1
            );
        }
    }
    // Every manifest parses and names files that exist.
    for m in files
        .iter()
        .filter(|p| p.file_name().and_then(|n| n.to_str()) == Some("manifest.json"))
    {
        let v: Value = serde_json::from_str(&std::fs::read_to_string(m).unwrap())
            .unwrap_or_else(|e| panic!("{} is not JSON: {e}", m.display()));
        let dir = m.parent().unwrap();
        for (name, sc) in v["scenarios"].as_object().expect("manifest.scenarios") {
            for key in ["stdout", "stdin"] {
                let f = sc[key].as_str().unwrap_or_else(|| panic!("{name}.{key}"));
                assert!(dir.join(f).is_file(), "{name}: {f} missing");
            }
        }
    }
}

#[test]
fn checked_in_fixtures_are_redacted() {
    let mut files = Vec::new();
    walk(&fixtures_root(), &mut files);
    for p in files {
        let text = std::fs::read_to_string(&p).unwrap();
        for needle in ["/home/", "/Users/", "C:\\\\Users", "/run/user/", "sk-ant-"] {
            assert!(
                !text.contains(needle),
                "{} contains unredacted `{needle}`",
                p.display()
            );
        }
        // Account block: identifiers replaced, not dropped.
        for f in text
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        {
            if let Some(acct) = f.pointer("/response/response/account") {
                assert_eq!(acct["email"], "redacted", "{}", p.display());
                assert_eq!(acct["organization"], "redacted", "{}", p.display());
            }
        }
    }
}

/// Q1: `can_use_tool` arrives in non-bypass stream-json with
/// `--permission-prompt-tool stdio`; the SDK-shaped response is accepted.
#[test]
fn q1_can_use_tool_arrives_and_sdk_allow_is_accepted() {
    let frames = fixture("can_use_tool_sdk_allow_with_session_state_events.ndjson");
    let req = can_use_tool_request(&frames).expect("can_use_tool control_request recorded");
    assert!(req["request_id"].is_string());
    assert_eq!(req["request"]["tool_name"], "Write");
    assert!(req["request"]["input"].is_object());
    assert!(req["request"]["tool_use_id"].is_string());

    let sent = fixture("can_use_tool_sdk_allow_with_session_state_events.stdin.ndjson");
    let resp = sent
        .iter()
        .find(|f| is_type(f, "control_response"))
        .expect("response sent");
    assert_eq!(resp["response"]["subtype"], "success");
    assert_eq!(resp["response"]["request_id"], req["request_id"]);
    assert_eq!(resp["response"]["response"]["behavior"], "allow");

    let result = result_frame(&frames).expect("turn completed");
    assert_eq!(result["is_error"], false);
    assert_eq!(result["permission_denials"], json!([]));
    // The tool actually ran: a user frame carries a non-error tool_result.
    assert!(frames.iter().any(|f| is_type(f, "user")
        && f.pointer("/message/content/0/type").and_then(Value::as_str) == Some("tool_result")
        && f.pointer("/message/content/0/is_error") != Some(&Value::Bool(true))));
}

/// Q1: the runner's current `{"allowed": true}` response (types.rs
/// `allow_tool`) is silently ignored — no result frame ever arrives.
#[test]
fn q1_runner_allowed_true_shape_is_ignored_by_the_cli() {
    let frames = fixture("can_use_tool_runner_shape_ignored.ndjson");
    assert!(can_use_tool_request(&frames).is_some());
    let sent = fixture("can_use_tool_runner_shape_ignored.stdin.ndjson");
    let resp = sent
        .iter()
        .find(|f| is_type(f, "control_response"))
        .unwrap();
    assert_eq!(resp["response"], json!({"allowed": true}));
    assert!(
        result_frame(&frames).is_none(),
        "turn must hang with the runner shape"
    );
    let last = frames.last().unwrap();
    assert!(
        !is_type(last, "user"),
        "no tool_result after the runner-shaped reply"
    );
}

/// Q1: an SDK-shaped deny reaches the model as an error tool_result and is
/// listed in `result.permission_denials`.
#[test]
fn q1_sdk_deny_becomes_error_tool_result_and_permission_denial() {
    let frames = fixture("can_use_tool_sdk_deny.ndjson");
    let tool_result = frames
        .iter()
        .find(|f| {
            is_type(f, "user")
                && f.pointer("/message/content/0/type").and_then(Value::as_str)
                    == Some("tool_result")
        })
        .expect("tool_result for the denied call");
    assert_eq!(
        tool_result.pointer("/message/content/0/is_error"),
        Some(&Value::Bool(true))
    );
    let result = result_frame(&frames).unwrap();
    assert_eq!(result["permission_denials"][0]["tool_name"], "Write");
    assert_eq!(result["is_error"], false, "a denial is not an errored turn");
}

/// Q2: `rate_limit_event` arrives by default; `session_state_changed` only
/// with `CLAUDE_CODE_EMIT_SESSION_STATE_EVENTS=1`.
#[test]
fn q2_rate_limit_event_default_session_state_changed_opt_in() {
    let plain = fixture("plain_turn.ndjson");
    let rl = plain
        .iter()
        .find(|f| is_type(f, "rate_limit_event"))
        .expect("rate_limit_event");
    for k in ["status", "resetsAt", "rateLimitType"] {
        assert!(
            rl["rate_limit_info"].get(k).is_some(),
            "rate_limit_info.{k}"
        );
    }
    assert!(!plain.iter().any(|f| is_system(f, "session_state_changed")));

    let with_env = fixture("can_use_tool_sdk_allow_with_session_state_events.ndjson");
    let states: Vec<&str> = with_env
        .iter()
        .filter(|f| is_system(f, "session_state_changed"))
        .filter_map(|f| f["state"].as_str())
        .collect();
    assert!(states.contains(&"requires_action"), "states: {states:?}");
    assert_eq!(states.last(), Some(&"idle"), "states: {states:?}");
}

/// Q3: an invalid model id yields `subtype: "success"` + `is_error: true`,
/// and (manifest) exit code 1.
#[test]
fn q3_invalid_model_is_success_subtype_with_is_error_and_nonzero_exit() {
    let frames = fixture("errored_turn_invalid_model.ndjson");
    let r = result_frame(&frames).unwrap();
    assert_eq!(r["subtype"], "success");
    assert_eq!(r["is_error"], true);
    assert_eq!(r["api_error_status"], 404);
    assert_eq!(r["terminal_reason"], "api_error");
    let assistant = frames.iter().find(|f| is_type(f, "assistant")).unwrap();
    assert_eq!(assistant["error"], "model_not_found");

    let manifest: Value = serde_json::from_str(
        &std::fs::read_to_string(claude_fixture_dir().join("manifest.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        manifest["scenarios"]["errored_turn_invalid_model"]["exit_code"],
        1
    );
    assert_eq!(manifest["scenarios"]["plain_turn"]["exit_code"], 0);
}

// ─────────────────────────────────────────────────────────────────────────────
// Live probes — #[ignore], need a logged-in `claude`
// ─────────────────────────────────────────────────────────────────────────────

const WRITE_PROMPT: &str =
    "Use the Write tool to create a file named probe.txt in the current directory \
containing exactly the text: hello. Do not use any other tool. Then reply DONE.";
const PONG_PROMPT: &str = "Reply with exactly the single word: pong";

#[derive(Clone, Copy)]
enum Responder {
    None,
    /// The runner's current reply (`OutgoingControlResponse::allow_tool`).
    RunnerAllowed,
    SdkAllow,
    SdkDeny,
}

struct Scenario {
    stem: &'static str,
    prompt: &'static str,
    model: &'static str,
    permission_prompt_tool: bool,
    responder: Responder,
    emit_state_events: bool,
    /// Kill the CLI after this long (the runner-shape scenario hangs by design).
    budget: Duration,
}

struct Recording {
    stdout: Vec<String>,
    stdin: Vec<String>,
    exit_code: Option<i32>,
    killed: bool,
    cwd: PathBuf,
    _tmp: tempfile::TempDir,
}

impl Recording {
    fn frames(&self) -> Vec<Value> {
        self.stdout
            .iter()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }
}

fn claude_bin() -> String {
    std::env::var("CLI_PROBES_CLAUDE").unwrap_or_else(|_| "claude".to_string())
}

fn installed_claude_version() -> String {
    let out = Command::new(claude_bin())
        .arg("--version")
        .output()
        .expect("`claude --version` (put a logged-in claude on PATH or set CLI_PROBES_CLAUDE)");
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .next()
        .unwrap_or("unknown")
        .to_string()
}

fn write_line(child: &mut Child, sent: &mut Vec<String>, frame: &Value) {
    let line = serde_json::to_string(frame).unwrap();
    if let Some(stdin) = child.stdin.as_mut() {
        let _ = writeln!(stdin, "{line}");
        let _ = stdin.flush();
    }
    sent.push(line);
}

fn respond(responder: Responder, request_id: &str, request: &Value) -> Option<Value> {
    match responder {
        Responder::None => None,
        Responder::RunnerAllowed => Some(json!({
            "type": "control_response", "response": {"allowed": true}, "request_id": request_id
        })),
        Responder::SdkAllow => Some(json!({
            "type": "control_response",
            "response": {"subtype": "success", "request_id": request_id,
                         "response": {"behavior": "allow", "updatedInput": request["input"].clone()}}
        })),
        Responder::SdkDeny => Some(json!({
            "type": "control_response",
            "response": {"subtype": "success", "request_id": request_id,
                         "response": {"behavior": "deny", "message": "denied by cli probe"}}
        })),
    }
}

fn run_scenario(sc: &Scenario) -> Recording {
    let tmp = tempfile::tempdir().expect("tempdir");
    let cwd = tmp.path().to_path_buf();
    let session_id = uuid::Uuid::new_v4().to_string();
    let mut cmd = Command::new(claude_bin());
    cmd.current_dir(&cwd)
        .args([
            "-p",
            "--output-format",
            "stream-json",
            "--input-format",
            "stream-json",
            "--verbose",
        ])
        .args(["--session-id", &session_id, "--model", sc.model])
        // Isolate from the operator's user settings / hooks / MCP servers: a
        // user-level allowlist or defaultMode would suppress `can_use_tool`.
        .args(["--setting-sources", "project", "--strict-mcp-config"])
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if sc.permission_prompt_tool {
        cmd.args(["--permission-prompt-tool", "stdio"]);
    }
    if sc.emit_state_events {
        cmd.env("CLAUDE_CODE_EMIT_SESSION_STATE_EVENTS", "1");
    }
    let mut child = cmd.spawn().expect("spawn claude");

    let (tx, rx) = mpsc::channel::<String>();
    let stdout = child.stdout.take().unwrap();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });

    let mut sent = Vec::new();
    write_line(
        &mut child,
        &mut sent,
        &json!({
            "type": "control_request", "request": {"subtype": "initialize", "protocolVersion": "1"}, "request_id": "req_1"
        }),
    );
    write_line(
        &mut child,
        &mut sent,
        &json!({
            "type": "user", "message": {"role": "user", "content": sc.prompt}, "session_id": "default"
        }),
    );

    let deadline = Instant::now() + sc.budget;
    let mut out = Vec::new();
    let mut killed = false;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            let _ = child.kill();
            killed = true;
            break;
        }
        match rx.recv_timeout(left.min(Duration::from_millis(500))) {
            Ok(line) => {
                if let Ok(f) = serde_json::from_str::<Value>(&line) {
                    if is_type(&f, "control_request")
                        && f.pointer("/request/subtype").and_then(Value::as_str)
                            == Some("can_use_tool")
                    {
                        let rid = f["request_id"].as_str().unwrap_or_default().to_string();
                        if let Some(r) = respond(sc.responder, &rid, &f["request"]) {
                            write_line(&mut child, &mut sent, &r);
                        }
                    }
                    if is_type(&f, "result") {
                        drop(child.stdin.take()); // EOF → the CLI exits
                    }
                }
                out.push(line);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if let Ok(Some(_)) = child.try_wait() {
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    // Drain anything still buffered, then reap.
    let exit_deadline = Instant::now() + Duration::from_secs(30);
    let status = loop {
        while let Ok(l) = rx.try_recv() {
            out.push(l);
        }
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) if Instant::now() < exit_deadline => {
                std::thread::sleep(Duration::from_millis(100))
            }
            _ => {
                let _ = child.kill();
                killed = true;
                break child.wait().ok();
            }
        }
    };
    while let Ok(l) = rx.try_recv() {
        out.push(l);
    }
    Recording {
        stdout: out,
        stdin: sent,
        exit_code: status.and_then(|s| s.code()),
        killed,
        cwd,
        _tmp: tmp,
    }
}

/// Redaction mirroring the rules the checked-in fixtures were produced with:
/// uuids → `00000000-0000-4000-8000-00000000000N` (one mapping per scenario
/// across stdout and stdin), cwd → `/redacted/cwd`, other home paths →
/// `/redacted/path`, account email/organization → `redacted`, thinking
/// signatures → `redacted`, pid → 0, `toolu_`/`msg_`/`req_` ids → `*_redacted_NN`.
fn redact(rec: &Recording) -> (String, String) {
    use regex::Regex;
    use std::collections::HashMap;
    let mut maps: HashMap<&'static str, HashMap<String, String>> = HashMap::new();
    let cwd = rec.cwd.to_string_lossy().to_string();
    let rules: Vec<(Regex, &str)> = vec![
        (
            Regex::new(r#""signature":"[^"]*""#).unwrap(),
            r#""signature":"redacted""#,
        ),
        (
            Regex::new(r#""organization":"(?:[^"\\]|\\.)*""#).unwrap(),
            r#""organization":"redacted""#,
        ),
        (
            Regex::new(r#""email":"[^"]*""#).unwrap(),
            r#""email":"redacted""#,
        ),
        (Regex::new(r#""pid":\d+"#).unwrap(), r#""pid":0"#),
    ];
    let paths: Vec<(Regex, &str)> = vec![
        (
            Regex::new(r#"/run/user/\d+/[^"\s]*"#).unwrap(),
            "/redacted/run",
        ),
        (
            Regex::new(r#"(?:/home|/Users)/[^"\s]*"#).unwrap(),
            "/redacted/path",
        ),
        (
            Regex::new(r#"-(?:home|Users)-[^/"\s]*"#).unwrap(),
            "-redacted-cwd",
        ),
        (
            Regex::new(r"[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}").unwrap(),
            "redacted",
        ),
    ];
    let ids: Vec<(&'static str, Regex)> = vec![
        (
            "uuid",
            Regex::new(
                r"\b[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}\b",
            )
            .unwrap(),
        ),
        ("toolu", Regex::new(r"toolu_[A-Za-z0-9]+").unwrap()),
        ("msg", Regex::new(r"msg_[A-Za-z0-9]{12,}").unwrap()),
        ("req", Regex::new(r"req_[A-Za-z0-9]{16,}").unwrap()),
    ];
    let mut apply = |text: &str| -> String {
        let mut t = text.to_string();
        for (re, rep) in &rules {
            t = re.replace_all(&t, *rep).into_owned();
        }
        t = t.replace(&cwd, "/redacted/cwd");
        for (re, rep) in &paths {
            t = re.replace_all(&t, *rep).into_owned();
        }
        for (kind, re) in &ids {
            let map = maps.entry(*kind).or_default();
            t = re
                .replace_all(&t, |c: &regex::Captures| {
                    let key = c[0].to_lowercase();
                    let n = map.len() + 1;
                    map.entry(key)
                        .or_insert_with(|| match *kind {
                            "uuid" => format!("00000000-0000-4000-8000-{n:012}"),
                            other => format!("{other}_redacted_{n:02}"),
                        })
                        .clone()
                })
                .into_owned();
        }
        t
    };
    let stdout = apply(&(rec.stdout.join("\n") + "\n"));
    let stdin = apply(&(rec.stdin.join("\n") + "\n"));
    (stdout, stdin)
}

/// Writes the recording to a temp dir (default) or, with `CLI_PROBES_RECORD=1`,
/// over `tests/fixtures/cli_protocol/claude/<installed version>/`.
fn persist(stem: &str, rec: &Recording) -> PathBuf {
    let (stdout, stdin) = redact(rec);
    let dir = if std::env::var("CLI_PROBES_RECORD").as_deref() == Ok("1") {
        fixtures_root()
            .join("claude")
            .join(installed_claude_version())
    } else {
        std::env::temp_dir()
            .join("cli_probes")
            .join(installed_claude_version())
    };
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(format!("{stem}.ndjson")), stdout).unwrap();
    std::fs::write(dir.join(format!("{stem}.stdin.ndjson")), stdin).unwrap();
    // Keep manifest.json (exit code, whether the probe had to kill the CLI) in
    // step with the frames it describes.
    let manifest_path = dir.join("manifest.json");
    let mut manifest: Value = std::fs::read_to_string(&manifest_path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_else(
            || json!({"cli": "claude", "cli_version": installed_claude_version(), "scenarios": {}}),
        );
    let entry = &mut manifest["scenarios"][stem];
    if !entry.is_object() {
        *entry = json!({});
    }
    entry["stdout"] = json!(format!("{stem}.ndjson"));
    entry["stdin"] = json!(format!("{stem}.stdin.ndjson"));
    entry["exit_code"] = json!(rec.exit_code);
    entry["killed_by_probe_watchdog"] = json!(rec.killed);
    std::fs::write(
        &manifest_path,
        serde_json::to_string_pretty(&manifest).unwrap() + "\n",
    )
    .unwrap();
    eprintln!(
        "[cli_probes] {stem}: exit={:?} killed={} frames={} -> {}",
        rec.exit_code,
        rec.killed,
        rec.stdout.len(),
        dir.display()
    );
    dir
}

#[test]
#[ignore = "live: needs a logged-in `claude` on PATH; spends tokens"]
fn live_plain_turn() {
    let sc = Scenario {
        stem: "plain_turn",
        prompt: PONG_PROMPT,
        model: "haiku",
        permission_prompt_tool: false,
        responder: Responder::None,
        emit_state_events: false,
        budget: Duration::from_secs(150),
    };
    let rec = run_scenario(&sc);
    persist(sc.stem, &rec);
    let frames = rec.frames();
    let r = result_frame(&frames).expect("result frame");
    assert_eq!(r["is_error"], false);
    assert_eq!(rec.exit_code, Some(0));
    assert!(
        frames.iter().any(|f| is_type(f, "rate_limit_event")),
        "Q2: rate_limit_event expected"
    );
}

#[test]
#[ignore = "live: needs a logged-in `claude` on PATH; spends tokens"]
fn live_errored_turn_invalid_model() {
    let sc = Scenario {
        stem: "errored_turn_invalid_model",
        prompt: PONG_PROMPT,
        model: "claude-nonexistent-probe-model-0",
        permission_prompt_tool: false,
        responder: Responder::None,
        emit_state_events: false,
        budget: Duration::from_secs(120),
    };
    let rec = run_scenario(&sc);
    persist(sc.stem, &rec);
    let frames = rec.frames();
    let r = result_frame(&frames).expect("result frame");
    assert_eq!(
        r["is_error"], true,
        "Q3: is_error expected on an errored turn"
    );
    assert_ne!(rec.exit_code, Some(0), "Q3: non-zero exit expected");
}

#[test]
#[ignore = "live: needs a logged-in `claude` on PATH; spends tokens"]
fn live_can_use_tool_sdk_allow() {
    let sc = Scenario {
        stem: "can_use_tool_sdk_allow_with_session_state_events",
        prompt: WRITE_PROMPT,
        model: "haiku",
        permission_prompt_tool: true,
        responder: Responder::SdkAllow,
        emit_state_events: true,
        budget: Duration::from_secs(150),
    };
    let rec = run_scenario(&sc);
    persist(sc.stem, &rec);
    let frames = rec.frames();
    assert!(
        can_use_tool_request(&frames).is_some(),
        "Q1: can_use_tool expected"
    );
    assert_eq!(result_frame(&frames).expect("result")["is_error"], false);
    assert_eq!(
        std::fs::read_to_string(rec.cwd.join("probe.txt"))
            .ok()
            .as_deref(),
        Some("hello"),
        "the allowed Write must have run"
    );
    assert!(
        frames.iter().any(|f| is_system(f, "session_state_changed")),
        "Q2: session_state_changed expected under the env opt-in"
    );
}

#[test]
#[ignore = "live: needs a logged-in `claude` on PATH; spends tokens"]
fn live_can_use_tool_sdk_deny() {
    let sc = Scenario {
        stem: "can_use_tool_sdk_deny",
        prompt: WRITE_PROMPT,
        model: "haiku",
        permission_prompt_tool: true,
        responder: Responder::SdkDeny,
        emit_state_events: false,
        budget: Duration::from_secs(150),
    };
    let rec = run_scenario(&sc);
    persist(sc.stem, &rec);
    let frames = rec.frames();
    assert!(can_use_tool_request(&frames).is_some());
    let r = result_frame(&frames).expect("result");
    assert_eq!(r["permission_denials"][0]["tool_name"], "Write");
    assert!(
        !rec.cwd.join("probe.txt").exists(),
        "a denied Write must not run"
    );
}

#[test]
#[ignore = "live: needs a logged-in `claude` on PATH; spends tokens; ~60 s"]
fn live_can_use_tool_runner_shape_is_ignored() {
    let sc = Scenario {
        stem: "can_use_tool_runner_shape_ignored",
        prompt: WRITE_PROMPT,
        model: "haiku",
        permission_prompt_tool: true,
        responder: Responder::RunnerAllowed,
        emit_state_events: false,
        budget: Duration::from_secs(60),
    };
    let rec = run_scenario(&sc);
    persist(sc.stem, &rec);
    let frames = rec.frames();
    assert!(can_use_tool_request(&frames).is_some());
    // If this starts failing, the CLI began accepting the runner's shape:
    // re-record and revisit Phase 9's responder decision.
    assert!(
        result_frame(&frames).is_none(),
        "runner-shaped reply unexpectedly completed the turn"
    );
    assert!(rec.killed);
    assert!(!rec.cwd.join("probe.txt").exists());
}

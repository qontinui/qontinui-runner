//! The native identity shim (`qontinui-shim`, run under the name `claude`)
//! passes a NESTED call through untouched — plan
//! `2026-11-06-nested-claude-rebinds-its-parent-pane`, Phase 2.
//!
//! A `claude` run from inside a runner-pane session's tool call reaches the
//! identity shim with the pane's pin, hook carrier, mcp-config and loopback
//! port in its environment. Before this fix it took all of them and posted
//! `/control/session-open`, rebinding the pane and its coord row to the child.
//! Claude Code marks its tool environment with `CLAUDECODE` /
//! `CLAUDE_CODE_CHILD_SESSION`, and the runner strips both at every spawn
//! site, so a marker present means nested: the real tool gets its original
//! argv and the runner hears nothing — including no self-provision mint
//! request when the env names no coord-mcp config.
//!
//! This runs the BUILT binary end to end, so it is the test that fails if
//! `run_identity` stops consulting `claude_env::inherited_session_markers`.
//! Unix-only: the copy runs as an extensionless `claude` beside a fake real
//! `claude` shell script.
#![cfg(unix)]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use qontinui_runner_lib::process_helpers::output_with_timeout;
use qontinui_runner_lib::{ambient, profile_cli, runner_breadcrumb};

/// A hung shim must fail the test rather than park it.
const BUDGET: Duration = Duration::from_secs(60);

/// The runner's session coord-identity mint route — the bin's
/// `PROVISION_SESSION_PATH` (`src/bin/qontinui_shim.rs`), which an integration
/// test cannot import.
const PROVISION_SESSION_PATH: &str = "/coord-mcp/provision-session";

/// The handshake secret this test writes for the armed mint.
const MINT_KEY: &str = "0123456789abcdef0123456789abcdef";

/// Writing an executable while another test thread forks can leave the write
/// fd open in that child and make the exec fail `ETXTBSY`; serialize every
/// copy-and-run.
static EXE_LOCK: Mutex<()> = Mutex::new(());

/// One request, as the recorder received it.
#[derive(Debug)]
struct Request {
    /// Method and path, without the HTTP version: `POST /control/session-open`.
    line: String,
    /// The request line and headers, verbatim.
    head: String,
}

/// A stand-in for the runner's loopback port: records every request a shim
/// sends it and answers `200` with an empty body at once.
struct LoopbackRecorder {
    port: u16,
    seen: Arc<Mutex<Vec<Request>>>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl LoopbackRecorder {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind 127.0.0.1:0");
        let port = listener.local_addr().expect("listener address").port();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let worker = {
            let (seen, stop) = (Arc::clone(&seen), Arc::clone(&stop));
            std::thread::spawn(move || {
                for conn in listener.incoming() {
                    if stop.load(Ordering::SeqCst) {
                        break;
                    }
                    let Ok(mut conn) = conn else { continue };
                    // Recorded BEFORE the reply, which the shim waits for, so
                    // once the shim has exited every request it made is here.
                    if let Some(request) = read_request(&mut conn) {
                        seen.lock().unwrap_or_else(|e| e.into_inner()).push(request);
                    }
                    let _ = conn.write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    );
                }
            })
        };
        Self {
            port,
            seen,
            stop,
            worker: Some(worker),
        }
    }

    /// Every request received since the previous call, oldest first.
    fn take(&self) -> Vec<Request> {
        std::mem::take(&mut *self.seen.lock().unwrap_or_else(|e| e.into_inner()))
    }
}

impl Drop for LoopbackRecorder {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Wake the blocking `accept` so the worker sees the flag.
        let _ = TcpStream::connect(("127.0.0.1", self.port));
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// Read one HTTP/1.1 request: the head up to its blank line, then
/// `Content-Length` bytes of body. `None` for a connection that sent nothing.
fn read_request(conn: &mut TcpStream) -> Option<Request> {
    conn.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    let mut raw: Vec<u8> = Vec::new();
    let mut buf = [0u8; 4096];
    let head_end = loop {
        if let Some(i) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
        let n = conn.read(&mut buf).ok()?;
        if n == 0 {
            return None;
        }
        raw.extend_from_slice(buf.get(..n)?);
    };
    let head = String::from_utf8_lossy(raw.get(..head_end)?).into_owned();
    let body_len = head
        .lines()
        .find_map(|l| {
            let (name, value) = l.split_once(':')?;
            name.trim()
                .eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .unwrap_or(0);
    while raw.len() < head_end + body_len {
        let n = conn.read(&mut buf).ok()?;
        if n == 0 {
            break;
        }
        raw.extend_from_slice(buf.get(..n)?);
    }
    let request_line = head.lines().next()?;
    let line = request_line
        .rsplit_once(' ')
        .map_or(request_line, |(method_and_path, _version)| method_and_path)
        .to_string();
    Some(Request { line, head })
}

/// The method-and-path line of each request.
fn lines(requests: &[Request]) -> Vec<&str> {
    requests.iter().map(|r| r.line.as_str()).collect()
}

fn write_exe(path: &Path, body: &str) {
    std::fs::write(path, body).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// Where the shim gets its coord-mcp config from in one run.
#[derive(Clone, Copy)]
enum McpConfig {
    /// `QONTINUI_MCP_CONFIG` names a file — the runner-delivered config, so the
    /// shim's self-provision fallback never runs.
    Delivered,
    /// `QONTINUI_MCP_CONFIG` is UNSET and the fallback is armed inside the temp
    /// HOME: the box-wide opt-in marker, a live runner's port breadcrumb naming
    /// the recorder's port, and the per-port handshake key that record names.
    /// A run that reaches `self_provision_mcp_config` then POSTs the mint
    /// request to the recorder.
    MintArmed,
}

/// Lay out what `self_provision_mcp_config` reads, under `home`, exactly where
/// the lib's own resolvers put it for a process whose `HOME` is `home`.
fn arm_self_provision_mint(home: &Path, port: u16) {
    let qontinui = ambient::qontinui_dir_from(None, Some(home.to_path_buf()))
        .expect("a home resolves to a qontinui dir");
    // `ambient::runner_dir()` — `runner_breadcrumb::breadcrumb_dir()` — is
    // `<qontinui dir>/runner`.
    let breadcrumbs = qontinui.join("runner");
    std::fs::create_dir_all(&breadcrumbs).unwrap();
    // `profile_cli::session_identity_marker_path()`: presence is the consent.
    std::fs::write(qontinui.join(profile_cli::SESSION_IDENTITY_MARKER_FILE), "").unwrap();
    let key = qontinui.join(profile_cli::runner_loopback_key_file_name(port));
    std::fs::write(&key, MINT_KEY).unwrap();
    let record = runner_breadcrumb::PortBreadcrumb {
        schema: runner_breadcrumb::BREADCRUMB_SCHEMA,
        port,
        // `resolve_live_runner` keeps a record only when its pid is alive, and
        // this test process is.
        pid: std::process::id(),
        started_at_ms: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX)),
        primary: true,
        loopback_key_path: Some(key),
    };
    std::fs::write(
        breadcrumbs.join(format!("api-port-{port}.json")),
        serde_json::to_vec(&record).unwrap(),
    )
    .unwrap();
}

/// Run the built `qontinui-shim`, copied in as `claude`, with `args`, wired the
/// way a runner pane wires it: the env cleared (so the caller's own Claude Code
/// session markers cannot leak in), the pane's pin, hook carrier, loopback
/// `port` and terminal id set, the coord-mcp config per `mcp`, a temp `HOME`
/// (so nothing of the machine running the test is read), and `extra_env` on
/// top. Returns the argv the fake real `claude` received, the carrier and
/// mcp-config paths read back as `S` and `M`.
fn run_exe_shim(
    args: &[&str],
    extra_env: &[(&str, &str)],
    port: u16,
    mcp: McpConfig,
) -> Vec<String> {
    let _serial = EXE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let (identity, real, home) = (
        tmp.path().join("identity"),
        tmp.path().join("real"),
        tmp.path().join("home"),
    );
    for dir in [&identity, &real, &home] {
        std::fs::create_dir_all(dir).unwrap();
    }
    let shim = identity.join("claude");
    std::fs::copy(env!("CARGO_BIN_EXE_qontinui-shim"), &shim).unwrap();
    let seen = tmp.path().join("argv");
    write_exe(
        &real.join("claude"),
        &format!("#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\n", seen.display()),
    );
    let (settings, mcp_file) = (tmp.path().join("s.json"), tmp.path().join("m.json"));
    std::fs::write(&settings, "{}").unwrap();
    std::fs::write(&mcp_file, "{}").unwrap();

    let mut cmd = Command::new(&shim);
    cmd.args(args)
        .env_clear()
        .env(
            "PATH",
            format!("{}:{}:/usr/bin:/bin", identity.display(), real.display()),
        )
        .env("HOME", &home)
        .env("QONTINUI_PINNED_SESSION_ID", "pin-1")
        .env("QONTINUI_TERMINAL_ID", "term-pane")
        .env("QONTINUI_CLAUDE_HOOK_SETTINGS", &settings)
        .env("QONTINUI_INSTALL_INTERCEPT_PORT", port.to_string())
        .env("QONTINUI_SHIM_RESOLVE_WAIT_SECS", "0");
    match mcp {
        McpConfig::Delivered => {
            cmd.env("QONTINUI_MCP_CONFIG", &mcp_file);
        }
        McpConfig::MintArmed => arm_self_provision_mint(&home, port),
    }
    cmd.envs(extra_env.iter().copied());
    let out = output_with_timeout(cmd, BUDGET).expect("the shim exe runs inside its budget");
    assert!(
        out.status.success(),
        "the shim exe failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    std::fs::read_to_string(&seen)
        .expect("the fake real claude ran")
        .lines()
        .map(|l| {
            if l.ends_with("s.json") {
                "S".to_string()
            } else if l.ends_with("m.json") {
                "M".to_string()
            } else {
                l.to_string()
            }
        })
        .collect()
}

#[test]
fn nested_claude_cannot_rebind_its_pane_via_the_identity_shim_exe() {
    let recorder = LoopbackRecorder::start();
    let port = recorder.port;
    let delivered = McpConfig::Delivered;

    // Controls first, on the same port: the pane's own launches DO carry the
    // pane's identity and DO speak to the runner, so this listener demonstrably
    // sees what the nested runs below must not produce.
    assert_eq!(
        run_exe_shim(&["-p", "x"], &[], port, delivered),
        [
            "-p",
            "x",
            "--settings",
            "S",
            "--mcp-config",
            "M",
            "--session-id",
            "pin-1"
        ]
    );
    assert_eq!(
        lines(&recorder.take()),
        ["POST /control/shim-beacon", "POST /control/session-open"]
    );
    assert_eq!(
        run_exe_shim(
            &["--session-id", "chosen-1", "-p", "x"],
            &[],
            port,
            delivered
        ),
        [
            "--session-id",
            "chosen-1",
            "-p",
            "x",
            "--settings",
            "S",
            "--mcp-config",
            "M"
        ]
    );
    assert_eq!(lines(&recorder.take()), ["POST /control/shim-beacon"]);

    let markers: [&[(&str, &str)]; 4] = [
        &[("CLAUDECODE", "1")],
        &[("CLAUDE_CODE_CHILD_SESSION", "1")],
        &[("CLAUDECODE", "1"), ("CLAUDE_CODE_CHILD_SESSION", "1")],
        // PRESENT but empty is still nested — `claude_env`'s own reading.
        &[("CLAUDE_CODE_CHILD_SESSION", "")],
    ];
    let argvs: [&[&str]; 2] = [&["-p", "x"], &["--session-id", "chosen-1", "-p", "x"]];
    for marker_env in markers {
        for args in argvs {
            assert_eq!(
                run_exe_shim(args, marker_env, port, delivered),
                args,
                "{args:?} under {marker_env:?}: the original argv only"
            );
            let heard = recorder.take();
            assert!(
                heard.is_empty(),
                "{args:?} under {marker_env:?}: a nested call must say nothing to the \
                 runner, but it sent {heard:?}"
            );
        }
    }

    // Repetition: nested calls in a row stay silent; nothing latches.
    for _ in 0..2 {
        assert_eq!(
            run_exe_shim(&["-p", "x"], &[("CLAUDECODE", "1")], port, delivered),
            ["-p", "x"]
        );
    }
    assert!(recorder.take().is_empty());
}

/// With no runner-delivered config, the exe's self-provision step
/// (`self_provision_mcp_config`) asks the live runner to MINT coord identity
/// for the caller's cwd. A nested call must not reach it either, or the child
/// would ask for the pane's device identity on its own. The mint is armed
/// inside the temp HOME and `QONTINUI_MCP_CONFIG` is left unset, so the
/// control demonstrably attempts it.
#[test]
fn nested_claude_skips_the_identity_shim_exe_self_provision_mint() {
    let recorder = LoopbackRecorder::start();
    let port = recorder.port;
    let armed = McpConfig::MintArmed;

    // Control: the pane's own launch ATTEMPTS the mint, presenting the
    // handshake key the breadcrumb names. The recorder's empty 200 carries no
    // `mcpServers` document, so the shim appends no --mcp-config: the attempt
    // is what this pins, not a minted config.
    assert_eq!(
        run_exe_shim(&["-p", "x"], &[], port, armed),
        ["-p", "x", "--settings", "S", "--session-id", "pin-1"]
    );
    let heard = recorder.take();
    let provision_line = format!("POST {PROVISION_SESSION_PATH}");
    assert_eq!(
        lines(&heard),
        [
            provision_line.as_str(),
            "POST /control/shim-beacon",
            "POST /control/session-open"
        ],
        "{heard:?}"
    );
    assert!(
        heard.iter().any(|r| r.line == provision_line
            && r.head.contains(&format!(
                "{}: {MINT_KEY}\r\n",
                profile_cli::RUNNER_LOOPBACK_KEY_HEADER
            ))),
        "the mint request presents the key the breadcrumb names: {heard:?}"
    );

    // Nested: nothing at all — no mint request, no beacon, no session-open.
    for marker_env in [
        &[("CLAUDECODE", "1")][..],
        &[("CLAUDE_CODE_CHILD_SESSION", "1")][..],
    ] {
        assert_eq!(
            run_exe_shim(&["-p", "x"], marker_env, port, armed),
            ["-p", "x"],
            "{marker_env:?}: the original argv only"
        );
        let heard = recorder.take();
        assert!(
            heard.is_empty(),
            "{marker_env:?}: a nested call must not ask the runner for anything, \
             a mint included, but it sent {heard:?}"
        );
    }
}

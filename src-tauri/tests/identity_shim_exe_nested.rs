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
//! argv and the runner hears nothing.
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
use std::time::Duration;

use qontinui_runner_lib::process_helpers::output_with_timeout;

/// A hung shim must fail the test rather than park it.
const BUDGET: Duration = Duration::from_secs(60);

/// A stand-in for the runner's install-effects port: records the request line
/// (`POST /control/...`, without the HTTP version) of everything a shim sends
/// it and answers `200` at once.
struct LoopbackRecorder {
    port: u16,
    seen: Arc<Mutex<Vec<String>>>,
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
                    if let Some(line) = read_request_line(&mut conn) {
                        seen.lock().unwrap_or_else(|e| e.into_inner()).push(line);
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

    /// Every request line received since the previous call, oldest first.
    fn take(&self) -> Vec<String> {
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

/// Read one HTTP/1.1 request (head, then `Content-Length` bytes of body) and
/// return its method and path. `None` for a connection that sent nothing.
fn read_request_line(conn: &mut TcpStream) -> Option<String> {
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
    Some(
        request_line
            .rsplit_once(' ')
            .map_or(request_line, |(method_and_path, _version)| method_and_path)
            .to_string(),
    )
}

fn write_exe(path: &Path, body: &str) {
    std::fs::write(path, body).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// Run the built `qontinui-shim`, copied in as `claude`, with `args`, wired the
/// way a runner pane wires it: the env cleared (so the caller's own Claude Code
/// session markers cannot leak in), the pane's pin, hook carrier, mcp-config,
/// loopback `port` and terminal id set, a temp `HOME` (so no opt-in marker or
/// runner breadcrumb of the machine running the test is read), and `extra_env`
/// on top. Returns the argv the fake real `claude` received, the carrier and
/// mcp-config paths read back as `S` and `M`.
fn run_exe_shim(args: &[&str], extra_env: &[(&str, &str)], port: u16) -> Vec<String> {
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
    let (settings, mcp) = (tmp.path().join("s.json"), tmp.path().join("m.json"));
    std::fs::write(&settings, "{}").unwrap();
    std::fs::write(&mcp, "{}").unwrap();

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
        .env("QONTINUI_MCP_CONFIG", &mcp)
        .env("QONTINUI_INSTALL_INTERCEPT_PORT", port.to_string())
        .env("QONTINUI_SHIM_RESOLVE_WAIT_SECS", "0")
        .envs(extra_env.iter().copied());
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

    // Controls first, on the same port: the pane's own launches DO carry the
    // pane's identity and DO speak to the runner, so this listener demonstrably
    // sees what the nested runs below must not produce.
    assert_eq!(
        run_exe_shim(&["-p", "x"], &[], port),
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
        recorder.take(),
        ["POST /control/shim-beacon", "POST /control/session-open"]
    );
    assert_eq!(
        run_exe_shim(&["--session-id", "chosen-1", "-p", "x"], &[], port),
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
    assert_eq!(recorder.take(), ["POST /control/shim-beacon"]);

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
                run_exe_shim(args, marker_env, port),
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
            run_exe_shim(&["-p", "x"], &[("CLAUDECODE", "1")], port),
            ["-p", "x"]
        );
    }
    assert!(recorder.take().is_empty());
}

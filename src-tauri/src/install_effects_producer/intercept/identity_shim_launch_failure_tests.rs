//! The bash identity shim's LAUNCH-FAILURE beacon (plan
//! `2026-11-10-migration-holds-for-missing-claude-cli`, Phase 2, D2), proven by
//! RUNNING the rendered template against a loopback listener and asserting the
//! literal POST bodies it sends.
//!
//! Every run's PATH is the shim's own dir, a scratch bin dir holding ONLY
//! `curl`, `sleep` and `sed` symlinks, and the fake real-tool dir — never
//! `/usr/bin` or `/bin` — so a system `claude` can never satisfy the shim's scan
//! and let a test pass without meeting the failure it names.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use super::shim_test_support::{identity_shim_cmd, write_exe, EXE_LOCK};
use crate::install_effects_producer::{note_shim_beacon, ShimBeaconRequest};
use crate::terminal::launch_failure::{LaunchFailure, LaunchFailureRegistry};

const BUDGET: Duration = Duration::from_secs(40);

/// Every run is resume-shaped — the population the `launch-failed` close
/// reason covers — with the SessionStart hook delivered.
const RESUME_ARGS: [&str; 2] = ["--resume", "sess-1"];

const BEACON_PATH: &str = "/control/shim-beacon";

const INVOKED_BODY: &str = r#"{"terminal_id":"term-launch","tool":"claude","event":"invoked","detail":"user_session_id=true settings=true mcp_config=false pinned=true"}"#;
const RESOLVE_FAILED_BODY: &str = r#"{"terminal_id":"term-launch","tool":"claude","event":"resolve-failed","detail":"waited=1 stub=false settings=true user_session_id=true"}"#;
const EXEC_FAILED_BODY: &str = r#"{"terminal_id":"term-launch","tool":"claude","event":"exec-failed","detail":"status=126 waited=1 settings=true user_session_id=true"}"#;
/// The same two beacons from a shim that delivers NO SessionStart hook.
const INVOKED_NO_HOOK_BODY: &str = r#"{"terminal_id":"term-launch","tool":"claude","event":"invoked","detail":"user_session_id=true settings=false mcp_config=false pinned=true"}"#;
const RESOLVE_FAILED_NO_HOOK_BODY: &str = r#"{"terminal_id":"term-launch","tool":"claude","event":"resolve-failed","detail":"waited=1 stub=false settings=false user_session_id=true"}"#;

/// One request the listener received: its path and its body, verbatim.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Post {
    path: String,
    body: String,
}

/// A loopback HTTP listener that records every request BEFORE answering
/// `200 {}`. The shim's `curl -fsS` waits for that answer, so by the time the
/// shim exits, everything it posted is recorded.
struct BeaconListener {
    port: u16,
    posts: Arc<Mutex<Vec<Post>>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl BeaconListener {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind a loopback port");
        listener
            .set_nonblocking(true)
            .expect("a non-blocking listener, so the thread can stop");
        let port = listener.local_addr().expect("the bound address").port();
        let posts = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let (posts, stop) = (Arc::clone(&posts), Arc::clone(&stop));
            std::thread::spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((stream, _)) => serve(stream, &posts),
                        Err(_) => std::thread::sleep(Duration::from_millis(5)),
                    }
                }
            })
        };
        Self {
            port,
            posts,
            stop,
            thread: Some(thread),
        }
    }

    fn posts(&self) -> Vec<Post> {
        self.posts.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }
}

impl Drop for BeaconListener {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Read one HTTP/1.1 request, record it, then answer `200 {}`.
fn serve(stream: TcpStream, posts: &Mutex<Vec<Post>>) {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let mut reader = BufReader::new(stream);
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).is_err() {
        return;
    }
    let path = request_line
        .split_whitespace()
        .nth(1)
        .unwrap_or_default()
        .to_string();
    let (mut content_length, mut expect_continue) = (0usize, false);
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            let (name, value) = (name.trim().to_ascii_lowercase(), value.trim());
            if name == "content-length" {
                content_length = value.parse().unwrap_or(0);
            } else if name == "expect" && value.eq_ignore_ascii_case("100-continue") {
                expect_continue = true;
            }
        }
    }
    if expect_continue {
        let _ = reader.get_mut().write_all(b"HTTP/1.1 100 Continue\r\n\r\n");
    }
    let mut body = vec![0u8; content_length];
    if reader.read_exact(&mut body).is_err() {
        return;
    }
    posts.lock().unwrap_or_else(|p| p.into_inner()).push(Post {
        path,
        body: String::from_utf8_lossy(&body).into_owned(),
    });
    let stream = reader.get_mut();
    let _ = stream.write_all(
        b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
    );
    let _ = stream.flush();
}

/// The box's own `tool`, from the usual system dirs or the test's PATH.
fn system_tool(tool: &str) -> Option<PathBuf> {
    let on_path: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    ["/usr/bin", "/bin", "/usr/local/bin"]
        .into_iter()
        .map(PathBuf::from)
        .chain(on_path)
        .map(|dir| dir.join(tool))
        .find(|p| p.is_file())
}

/// A dir holding ONLY `curl`, `sleep` and `sed` — the external commands the
/// shim's give-up paths run — as symlinks to the box's own binaries. A box
/// without one of them cannot run these tests, and fails them loudly rather
/// than passing them unrun.
fn scratch_bin(root: &Path) -> PathBuf {
    let bin = root.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    for tool in ["curl", "sleep", "sed"] {
        let target = system_tool(tool).unwrap_or_else(|| {
            panic!("these tests run the shim's `{tool}`, and this box has none on PATH or in /usr/bin, /bin, /usr/local/bin")
        });
        std::os::unix::fs::symlink(&target, bin.join(tool)).unwrap();
    }
    bin
}

/// What the real-tool dir holds for one run.
#[derive(Debug, Clone, Copy)]
enum Real {
    /// Nothing: the shim's scan never resolves `claude`.
    Absent,
    /// A regular file WITHOUT the exec bit: the scan resolves it (it is a
    /// file) and every exec of it fails with status 126.
    NotExecutable,
    /// A working `claude` that leaves a marker and exits 0.
    Working,
}

struct ShimRun {
    code: Option<i32>,
    stdout: String,
    stderr: String,
    posts: Vec<Post>,
    ran: bool,
}

impl ShimRun {
    /// Every POST body, in order, after asserting each went to the beacon route.
    fn beacon_bodies(&self) -> Vec<&str> {
        for post in &self.posts {
            assert_eq!(post.path, BEACON_PATH, "{:?}", self.posts);
        }
        self.posts.iter().map(|p| p.body.as_str()).collect()
    }

    /// The launch-failure bodies alone: every beacon but `invoked`.
    fn failure_bodies(&self) -> Vec<&str> {
        self.beacon_bodies()
            .into_iter()
            .filter(|body| !body.contains(r#""event":"invoked""#))
            .collect()
    }
}

/// `claude --resume sess-1` through the rendered shim in a runner-terminal
/// environment (port, terminal id, pin, hook settings), with a 1 s resolve
/// wait. `nested` sets an INHERITED recursion guard, as a `claude` started
/// inside another `claude` has.
fn run_shim(real: Real, nested: bool) -> ShimRun {
    run_shim_with(real, nested, true)
}

/// [`run_shim`], with the SessionStart hook's settings file delivered
/// (`QONTINUI_CLAUDE_HOOK_SETTINGS`) only when `hook` is set.
fn run_shim_with(real: Real, nested: bool, hook: bool) -> ShimRun {
    let _serial = EXE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let (shim_dir, real_dir) = (tmp.path().join("shim"), tmp.path().join("real"));
    std::fs::create_dir_all(&shim_dir).unwrap();
    std::fs::create_dir_all(&real_dir).unwrap();
    let bin = scratch_bin(tmp.path());
    let marker = tmp.path().join("ran");
    let real_claude = real_dir.join("claude");
    match real {
        Real::Absent => {}
        Real::NotExecutable => {
            std::fs::write(&real_claude, "#!/bin/sh\necho NOT-EXECUTABLE-RAN\n").unwrap();
            std::fs::set_permissions(&real_claude, std::fs::Permissions::from_mode(0o644)).unwrap();
        }
        Real::Working => write_exe(
            &real_claude,
            &format!("#!/bin/sh\necho ran > '{}'\n", marker.display()),
        ),
    }
    let settings = tmp.path().join("hook-settings.json");
    std::fs::write(&settings, "{}").unwrap();

    let listener = BeaconListener::start();
    let path = format!(
        "{}:{}:{}",
        shim_dir.display(),
        bin.display(),
        real_dir.display()
    );
    let mut cmd = identity_shim_cmd(&shim_dir, &RESUME_ARGS, &path, "1");
    cmd.current_dir(tmp.path())
        .env("QONTINUI_INSTALL_INTERCEPT_PORT", listener.port.to_string())
        .env("QONTINUI_TERMINAL_ID", "term-launch")
        .env("QONTINUI_PINNED_SESSION_ID", "pin-1");
    if hook {
        cmd.env("QONTINUI_CLAUDE_HOOK_SETTINGS", &settings);
    }
    if nested {
        cmd.env("QONTINUI_INSTALL_INTERCEPT_GUARD", "1");
    }
    let out = crate::process_helpers::output_with_timeout(cmd, BUDGET).unwrap();
    ShimRun {
        code: out.status.code(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        posts: listener.posts(),
        ran: marker.exists(),
    }
}

/// No `claude` resolves within the wait: the shim reports `resolve-failed`
/// with the literal body BEFORE its fall-through exec (which still runs and
/// exits 127), and the runner's route body records exactly the flags the
/// `launch-failed` decider reads from that same literal.
#[test]
fn resolve_failed_is_reported_before_the_fall_through() {
    let run = run_shim(Real::Absent, false);
    assert_eq!(
        run.code,
        Some(127),
        "the fall-through still runs: {}",
        run.stderr
    );
    assert!(
        run.stderr
            .contains("the real claude was not found on PATH after waiting 1s"),
        "{}",
        run.stderr
    );
    assert!(
        run.stderr.contains("exec: claude: not found"),
        "{}",
        run.stderr
    );
    assert_eq!(run.beacon_bodies(), [INVOKED_BODY, RESOLVE_FAILED_BODY]);

    let req: ShimBeaconRequest = serde_json::from_str(RESOLVE_FAILED_BODY).unwrap();
    let registry = LaunchFailureRegistry::new();
    let noted = note_shim_beacon(&registry, &req, 1_000);
    let expected = LaunchFailure {
        event: "resolve-failed".to_string(),
        detail: "waited=1 stub=false settings=true user_session_id=true".to_string(),
        settings: true,
        user_chose_session: true,
        at_ms: 1_000,
    };
    assert_eq!(noted.as_ref(), Some(&expected));
    assert_eq!(registry.get("term-launch", 1_000), Some(expected));
}

/// A shim that delivers NO SessionStart hook (no `QONTINUI_CLAUDE_HOOK_SETTINGS`)
/// reports `settings=false` on its failure beacon, as on its `invoked` one --
/// the flag the `launch-failed` decider requires before it may read "no CLI
/// ever started here" from the row's hook stamp. A quoted `"${a[@]+set}"`
/// expands to no word at all for an empty array, which left `[ -n ]` (always
/// true), so the failure beacon claimed a hook the shim never delivered.
#[test]
fn a_launch_failure_without_the_hook_reports_settings_false() {
    let run = run_shim_with(Real::Absent, false, false);
    assert_eq!(run.code, Some(127), "{}", run.stderr);
    assert_eq!(
        run.beacon_bodies(),
        [INVOKED_NO_HOOK_BODY, RESOLVE_FAILED_NO_HOOK_BODY]
    );
}

/// The resolved `claude` will not exec (no exec bit): the shim retries for its
/// bound, then reports `exec-failed` ONCE — not once per attempt — with exec's
/// own status, and exits with that status.
#[test]
fn exec_failed_is_reported_once_with_the_exec_status() {
    let run = run_shim(Real::NotExecutable, false);
    assert_eq!(run.code, Some(126), "{}", run.stderr);
    assert!(!run.stdout.contains("NOT-EXECUTABLE-RAN"), "{}", run.stdout);
    assert_eq!(run.beacon_bodies(), [INVOKED_BODY, EXEC_FAILED_BODY]);
}

/// A nested `claude` (an INHERITED recursion guard) reports neither give-up,
/// so it can never mark a parent pane that may be running fine. Each arm runs
/// top-level first and must see the failure POST there, so the nested arm's
/// silence is the guard's doing and not a harness that cannot see a POST.
#[test]
fn a_nested_invocation_never_reports_a_launch_failure() {
    for (real, failure, code) in [
        (Real::Absent, RESOLVE_FAILED_BODY, 127),
        (Real::NotExecutable, EXEC_FAILED_BODY, 126),
    ] {
        let top = run_shim(real, false);
        assert_eq!(
            top.failure_bodies(),
            [failure],
            "{real:?} top-level: {}",
            top.stderr
        );
        let nested = run_shim(real, true);
        assert_eq!(
            nested.code,
            Some(code),
            "{real:?} nested: {}",
            nested.stderr
        );
        assert!(
            nested.posts.is_empty(),
            "{real:?}: a nested invocation posts nothing at all: {:?}",
            nested.posts
        );
    }
}

/// A launch that works reports no failure: the `invoked` beacon alone.
#[test]
fn a_launch_that_works_reports_no_failure() {
    let run = run_shim(Real::Working, false);
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert!(run.ran, "the real claude never ran: {}", run.stderr);
    assert_eq!(run.beacon_bodies(), [INVOKED_BODY]);
}

/// The guard passthrough reaches the give-up path BEFORE `SETTINGS_ARGS` and
/// `user_chose_session` are assigned, and the shim runs `set -u`. Computing
/// the launch-failure flags there must not abort it: the fall-through exec
/// still runs.
#[test]
fn the_guard_passthrough_under_set_u_still_reaches_the_fall_through() {
    let run = run_shim(Real::Absent, true);
    assert!(!run.stderr.contains("unbound variable"), "{}", run.stderr);
    assert!(
        run.stderr.contains("exec: claude: not found"),
        "the fall-through exec never ran: {}",
        run.stderr
    );
    assert_eq!(run.code, Some(127), "{}", run.stderr);
}

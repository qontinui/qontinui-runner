//! End-to-end: the RUNNER-side spawner (`qontinui_pty_holder::spawn`) starting
//! the real `qontinui-pty-holder`, and the holder outliving a SIGKILL of the
//! process that spawned it.
//!
//! The "spawning parent" is this very test binary, re-executed with
//! `PTY_HOLDER_SPAWN_PARENT_DIR` set so that its
//! `pty_holder_spawn_parent_helper` test acts as the parent: it calls
//! `spawn::spawn_holder` exactly as the runner will, prints one line, and
//! sleeps until killed. The env var is a TEST-ONLY switch of this test file;
//! nothing in the holder or the spawner reads it.
//!
//! Unix only (it SIGKILLs a process group and reads `/proc`). The Windows arm
//! of the spawner is type-checked for `x86_64-pc-windows-msvc`; it is UNRUN
//! here, not passed.
#![cfg(unix)]

use std::io::{BufRead, BufReader};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use qontinui_pty_holder::client::{connect, Event};
use qontinui_pty_holder::pane::PaneId;
use qontinui_pty_holder::spawn::{
    spawn_holder, HostFacts, ResolvedRoute, RouteRequest, SpawnError, SpawnRequest, Unprotected,
    DEFAULT_REPORT_TIMEOUT,
};
use qontinui_pty_holder::spec::ChildSpec;

const BIN: &str = env!("CARGO_BIN_EXE_qontinui-pty-holder");
const PARENT_ENV: &str = "PTY_HOLDER_SPAWN_PARENT_DIR";

fn soon() -> Instant {
    Instant::now() + Duration::from_secs(10)
}

fn pane_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .subsec_nanos();
    PathBuf::from("/tmp").join(format!("ptys-{tag}-{}-{nanos:x}", std::process::id()))
}

fn alive(pid: u32) -> bool {
    // SAFETY: signal 0 only checks for existence.
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

/// `(ppid, sid)` of a live process, from `/proc` (Linux) — `None` elsewhere.
fn ppid_and_session(pid: u32) -> Option<(i32, i32)> {
    let raw = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let (_, rest) = raw.rsplit_once(')')?;
    let f: Vec<&str> = rest.split_whitespace().collect();
    Some((f.get(1)?.parse().ok()?, f.get(3)?.parse().ok()?))
}

/// The process start time (clock ticks since boot, `/proc/<pid>/stat` field
/// 22) — with the pid, the identity of a process: a recycled pid has a new one.
fn start_time(pid: u32) -> Option<u64> {
    let raw = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let (_, rest) = raw.rsplit_once(')')?;
    rest.split_whitespace().nth(19)?.parse().ok()
}

fn sleeper() -> ChildSpec {
    let mut spec = ChildSpec::new(vec!["sleep".into(), "3600".into()]);
    spec.env = std::env::vars_os().collect();
    spec.cwd = Some("/tmp".into());
    spec
}

fn request<'a>(dir: &'a Path, pane: &'a PaneId, child: &'a ChildSpec) -> SpawnRequest<'a> {
    SpawnRequest {
        holder_exe: Path::new(BIN),
        pane_dir: dir,
        pane_id: pane,
        child,
        route: RouteRequest::Auto,
        report_timeout: DEFAULT_REPORT_TIMEOUT,
    }
}

/// End the pane through the protocol and wait for the holder to leave.
fn kill_via_protocol(dir: &Path, pane: &PaneId) {
    if let Ok(c) = connect(dir, pane, soon()) {
        if let Ok(mut s) = c.attach(None, soon()) {
            let _ = s.writer.kill(soon());
            let end = Instant::now() + Duration::from_secs(10);
            while let Ok(Some(ev)) = s.reader.next_event(Some(end)) {
                if matches!(ev, Event::Exit(_)) {
                    break;
                }
            }
        }
    }
}

/// The parent stand-in. A no-op unless re-executed by the survival test.
#[test]
fn pty_holder_spawn_parent_helper() {
    let Some(dir) = std::env::var_os(PARENT_ENV) else {
        return;
    };
    let dir = PathBuf::from(dir);
    let pane = PaneId::new("p1").unwrap();
    let child = sleeper();
    match spawn_holder(&request(&dir, &pane, &child)) {
        Ok(h) => {
            println!(
                "PARENT_READY holder_pid={} child_pid={} route={} unprotected={}",
                h.holder_pid,
                h.child_pid,
                h.route.as_str(),
                h.unprotected.as_ref().map(|u| u.to_string()).unwrap_or_default()
            );
            // Keep the Child handle alive, exactly as a runner would, until
            // the outer test kills this process.
            let _keep = h;
            loop {
                std::thread::sleep(Duration::from_secs(3600));
            }
        }
        Err(e) => println!("PARENT_ERROR {e}"),
    }
}

/// The spawner end to end: ready line parsed, pids verified, the holder in its
/// OWN session (so a kill of the spawner's session or group cannot reach it),
/// the PTY child under it, the spec gone, and teardown leaving nothing alive.
#[test]
fn pty_holder_spawn_spawns_attaches_and_tears_down() {
    let dir = pane_dir("e2e");
    let pane = PaneId::new("p1").unwrap();
    let child = sleeper();
    let h = spawn_holder(&request(&dir, &pane, &child)).expect("spawn");
    let facts = HostFacts::probe();
    // The route is decided by observables; say which one ran.
    eprintln!(
        "pty_holder_spawn: route={} unprotected={:?} facts={facts:?}",
        h.route.as_str(),
        h.unprotected
    );
    if facts.in_unit_cgroup && h.route == ResolvedRoute::Plain {
        assert!(
            h.unprotected.is_some(),
            "a plain holder inside a unit cgroup must be flagged unprotected"
        );
    }
    let (holder, child_pid) = (h.holder_pid, h.child_pid);
    assert!(alive(holder) && alive(child_pid));
    assert!(!dir.join("p1.spec").exists(), "the spec was consumed");
    if let Some((_, sid)) = ppid_and_session(holder) {
        assert_eq!(sid, holder as i32, "the holder leads its own session");
    }
    if let Some((ppid, sid)) = ppid_and_session(child_pid) {
        assert_eq!(ppid, holder as i32, "the PTY child is the holder's");
        assert_eq!(sid, child_pid as i32, "and leads its own session on the PTY");
    }
    let s = connect(&dir, &pane, soon())
        .unwrap()
        .attach(None, soon())
        .unwrap();
    assert_eq!(s.hello_ack.holder_pid, holder);
    assert_eq!(s.info.child_pid, child_pid);
    drop(s);

    h.teardown();
    let end = Instant::now() + Duration::from_secs(5);
    while (alive(holder) || alive(child_pid)) && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(!alive(holder), "teardown reaped the holder");
    assert!(!alive(child_pid), "teardown ended the PTY child");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A second spawn for a live pane is the holder's typed `lock_held`, and the
/// live holder is untouched by the failed attempt's teardown.
#[test]
fn pty_holder_spawn_second_spawn_for_a_live_pane_is_lock_held() {
    let dir = pane_dir("dup");
    let pane = PaneId::new("p1").unwrap();
    let child = sleeper();
    let first = spawn_holder(&request(&dir, &pane, &child)).expect("first");
    let err = spawn_holder(&request(&dir, &pane, &child)).unwrap_err();
    assert_eq!(err.holder_error_kind(), Some("lock_held"), "{err}");
    assert!(matches!(err, SpawnError::HolderFailed { .. }));
    assert!(alive(first.holder_pid) && alive(first.child_pid));
    assert!(!dir.join("p1.spec").exists(), "the failed attempt's spec is gone");
    first.teardown();
    let _ = std::fs::remove_dir_all(&dir);
}

/// A FORCED plain route: inside a systemd unit cgroup the holder is returned
/// flagged `Unprotected::Cgroup` (it will die with that unit) — never silently;
/// outside one it is not flagged.
#[test]
fn pty_holder_spawn_plain_route_flags_an_unprotected_cgroup() {
    let dir = pane_dir("plain");
    let pane = PaneId::new("p1").unwrap();
    let child = sleeper();
    let mut req = request(&dir, &pane, &child);
    req.route = RouteRequest::Plain;
    let h = spawn_holder(&req).expect("plain spawn");
    assert_eq!(h.route, ResolvedRoute::Plain);
    let in_unit = HostFacts::probe().in_unit_cgroup;
    eprintln!("pty_holder_spawn: plain in_unit_cgroup={in_unit} unprotected={:?}", h.unprotected);
    assert_eq!(
        h.unprotected,
        in_unit.then_some(Unprotected::Cgroup {
            fallback_reason: None
        })
    );
    h.teardown();
    let _ = std::fs::remove_dir_all(&dir);
}

/// A holder executable that is not one: typed failure, nothing left behind.
#[test]
fn pty_holder_spawn_bad_executable_fails_typed() {
    let dir = pane_dir("bad");
    let pane = PaneId::new("p1").unwrap();
    let child = sleeper();
    let mut req = request(&dir, &pane, &child);
    req.holder_exe = Path::new("/nonexistent/qontinui-pty-holder");
    req.route = RouteRequest::Plain;
    let err = spawn_holder(&req).unwrap_err();
    assert!(matches!(err, SpawnError::Spawn(_)), "{err}");
    assert!(!dir.join("p1.spec").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

/// The headline property for the spawn path (Phase 0's GO, re-run on the real
/// holder): SIGKILL the spawning parent's whole process group, and the pane's
/// child is still alive — the SAME process (pid and start time) — and its
/// holder still answers and serves it.
#[test]
fn pty_holder_spawn_holder_survives_sigkill_of_its_spawner() {
    let dir = pane_dir("surv");
    let pane = PaneId::new("p1").unwrap();
    let mut parent = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "pty_holder_spawn_parent_helper",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(PARENT_ENV, &dir)
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let out = parent.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        // libtest prints `test <name> ... ` on the same line before the
        // helper's own output, so the marker is found, not prefix-matched.
        for line in BufReader::new(out).lines().map_while(Result::ok) {
            if let Some(i) = line.find("PARENT_") {
                let _ = tx.send(line.get(i..).unwrap_or_default().to_string());
                return;
            }
        }
        let _ = tx.send(String::new());
    });
    let line = rx
        .recv_timeout(Duration::from_secs(60))
        .expect("the parent reported within 60 s");
    assert!(line.starts_with("PARENT_READY"), "{line}");
    eprintln!("pty_holder_spawn: {line}");
    let field = |k: &str| -> u32 {
        line.split_whitespace()
            .find_map(|t| t.strip_prefix(k)?.strip_prefix('='))
            .unwrap()
            .parse()
            .unwrap()
    };
    let (holder, child) = (field("holder_pid"), field("child_pid"));
    let child_start = start_time(child);

    // SIGKILL the parent's whole process group, then reap the parent.
    // SAFETY: the group of a process this test spawned with process_group(0).
    unsafe {
        libc::kill(-(parent.id() as i32), libc::SIGKILL);
    }
    let _ = parent.wait();
    std::thread::sleep(Duration::from_millis(500));

    assert!(alive(holder), "the holder survived its spawner");
    assert!(alive(child), "the pane's child survived");
    assert_eq!(start_time(child), child_start, "the SAME child process");
    let s = connect(&dir, &pane, soon())
        .expect("the orphaned holder still answers")
        .attach(None, soon())
        .unwrap();
    assert_eq!(s.hello_ack.holder_pid, holder);
    assert_eq!(s.info.child_pid, child);
    drop(s);

    kill_via_protocol(&dir, &pane);
    let end = Instant::now() + Duration::from_secs(10);
    while (alive(holder) || alive(child)) && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(!alive(child), "cleanup: the child is gone");
    let _ = std::fs::remove_dir_all(&dir);
}

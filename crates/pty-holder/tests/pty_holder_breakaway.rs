//! Windows: a holder outlives the closing of an OUTER `KILL_ON_JOB_CLOSE` job
//! its spawner sits in — the supervisor's shape (plan D4 as corrected
//! 2026-09-27; Phase 3). The matrix Phase 0 owed and its spike deleted:
//!
//! | spawner's outer job | expected route | holder after the job closes |
//! |---|---|---|
//! | allows breakaway (`BREAKAWAY_OK`) | `breakaway` | alive |
//! | forbids breakaway | `wmi` (Win32_Process.Create) | alive |
//!
//! plus the WMI route's latency, printed and held against the new-tab budget
//! (`spawn::NEW_TAB_SPAWN_BUDGET`), which D13 puts on every new tab's path.
//!
//! The "spawner" is this test binary re-executed with `PTY_HOLDER_BREAKAWAY_PARENT_DIR`
//! set: its `pty_holder_breakaway_parent_helper` waits for a line on stdin (so
//! the outer test can put it in the job first), spawns the holder exactly as
//! the runner does, prints one line, and sleeps until the job kills it. The
//! env var is a TEST-ONLY switch of this file.
//!
//! When the test process is ITSELF inside a job (a CI runner's), every job
//! enclosing the spawner must allow breakaway for that route to be legal, so
//! the route is asserted only outside one; survival is asserted always.
//!
//! **UNRUN on the Linux box this was written on** — it first executes on the
//! `holder-crates (windows-latest)` CI leg. Recorded as UNRUN, never passed.
#![cfg(windows)]

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use qontinui_pty_holder::client::{connect, probe, Event, Probe};
use qontinui_pty_holder::pane::PaneId;
use qontinui_pty_holder::spawn::{
    spawn_holder, RouteRequest, SpawnRequest, DEFAULT_REPORT_TIMEOUT, NEW_TAB_SPAWN_BUDGET,
};
use qontinui_pty_holder::spec::ChildSpec;
use qontinui_runner_win32::holder_spawn::{current_process_in_job, OuterKillOnCloseJob};

const BIN: &str = env!("CARGO_BIN_EXE_qontinui-pty-holder");
const PARENT_ENV: &str = "PTY_HOLDER_BREAKAWAY_PARENT_DIR";
const ROUTE_ENV: &str = "PTY_HOLDER_BREAKAWAY_ROUTE";

fn soon() -> Instant {
    Instant::now() + Duration::from_secs(10)
}

fn pane_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .subsec_nanos();
    std::env::temp_dir().join(format!("ptyw-{tag}-{}-{nanos:x}", std::process::id()))
}

/// An interactive `cmd` on the PTY: alive until killed.
fn shell() -> ChildSpec {
    let mut spec = ChildSpec::new(vec!["cmd.exe".into(), "/Q".into()]);
    spec.env = std::env::vars_os().collect();
    spec.cwd = Some(std::env::temp_dir().into_os_string());
    spec
}

/// The spawner stand-in. A no-op unless re-executed by a test below.
#[test]
fn pty_holder_breakaway_parent_helper() {
    let Some(dir) = std::env::var_os(PARENT_ENV) else {
        return;
    };
    let mut go = String::new();
    std::io::stdin().read_line(&mut go).unwrap();
    let dir = PathBuf::from(dir);
    let pane = PaneId::new("p1").unwrap();
    let child = shell();
    let route = match std::env::var(ROUTE_ENV).as_deref() {
        Ok("wmi") => RouteRequest::Wmi,
        _ => RouteRequest::Auto,
    };
    match spawn_holder(&SpawnRequest {
        holder_exe: Path::new(BIN),
        pane_dir: &dir,
        pane_id: &pane,
        child: &child,
        route,
        report_timeout: DEFAULT_REPORT_TIMEOUT,
    }) {
        Ok(h) => {
            println!(
                "PARENT_READY holder_pid={} child_pid={} route={} ready_after_ms={}",
                h.holder_pid,
                h.child_pid,
                h.route_name(),
                h.ready_after.as_millis()
            );
            let _ = std::io::stdout().flush();
            let _keep = h;
            loop {
                std::thread::sleep(Duration::from_secs(3600));
            }
        }
        Err(e) => {
            println!("PARENT_ERROR {e}");
            let _ = std::io::stdout().flush();
        }
    }
}

/// What the parent reported.
struct Reported {
    holder_pid: u32,
    child_pid: u32,
    route: String,
    ready_after: Duration,
}

/// Run the parent inside an outer job (`breakaway_ok` as given), let it spawn
/// a holder, then CLOSE the job — killing the parent and everything still in
/// the job — and return what the parent reported.
fn spawn_in_outer_job_then_close_it(dir: &Path, breakaway_ok: bool, route: &str) -> Reported {
    let job = OuterKillOnCloseJob::create(breakaway_ok).expect("outer job");
    let mut parent = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "pty_holder_breakaway_parent_helper",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(PARENT_ENV, dir)
        .env(ROUTE_ENV, route)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    job.assign(&parent).expect("parent into the outer job");
    writeln!(parent.stdin.take().unwrap(), "go").unwrap();
    let out = parent.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(out).lines().map_while(Result::ok) {
            if let Some(i) = line.find("PARENT_") {
                let _ = tx.send(line.get(i..).unwrap_or_default().to_string());
                return;
            }
        }
        let _ = tx.send(String::new());
    });
    let line = rx
        .recv_timeout(Duration::from_secs(90))
        .expect("the parent reported within 90 s");
    eprintln!("pty_holder_breakaway: breakaway_ok={breakaway_ok}: {line}");
    assert!(line.starts_with("PARENT_READY"), "{line}");
    let field = |k: &str| -> String {
        line.split_whitespace()
            .find_map(|t| t.strip_prefix(k)?.strip_prefix('='))
            .unwrap()
            .to_string()
    };
    let reported = Reported {
        holder_pid: field("holder_pid").parse().unwrap(),
        child_pid: field("child_pid").parse().unwrap(),
        route: field("route"),
        ready_after: Duration::from_millis(field("ready_after_ms").parse().unwrap()),
    };
    // Closing the job is the supervisor dying: KILL_ON_JOB_CLOSE ends every
    // process still in it.
    drop(job);
    let _ = parent.wait();
    std::thread::sleep(Duration::from_millis(500));
    reported
}

/// The holder still answers the handshake and still serves the same child.
fn assert_survived(dir: &Path, r: &Reported) {
    let pane = PaneId::new("p1").unwrap();
    match probe(dir, &pane, soon()) {
        Probe::Healthy { hello_ack } => {
            assert_eq!(hello_ack.holder_pid, r.holder_pid);
            assert_eq!(hello_ack.child_pid, Some(r.child_pid), "the SAME child");
        }
        other => panic!("the holder did not survive its spawner's job: {other:?}"),
    }
}

/// End the pane through the protocol.
fn kill_via_protocol(dir: &Path) {
    let pane = PaneId::new("p1").unwrap();
    if let Ok(c) = connect(dir, &pane, soon()) {
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

fn in_a_job_already() -> bool {
    current_process_in_job().expect("IsProcessInJob")
}

#[test]
fn pty_holder_breakaway_survives_an_outer_job_that_allows_breakaway() {
    let dir = pane_dir("bk");
    let r = spawn_in_outer_job_then_close_it(&dir, true, "auto");
    if !in_a_job_already() {
        assert_eq!(r.route, "breakaway");
    }
    assert_survived(&dir, &r);
    kill_via_protocol(&dir);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The WMI fallback: breakaway is refused, `Win32_Process.Create` spawns the
/// holder outside every job, and its latency is measured against the
/// new-tab budget.
#[test]
fn pty_holder_breakaway_wmi_fallback_survives_an_outer_job_that_forbids_breakaway() {
    let dir = pane_dir("wmi");
    let r = spawn_in_outer_job_then_close_it(&dir, false, "auto");
    assert_eq!(r.route, "wmi", "a refused breakaway falls back to WMI");
    eprintln!(
        "pty_holder_breakaway: WMI spawn-to-ready {} ms (new-tab budget {} ms, {})",
        r.ready_after.as_millis(),
        NEW_TAB_SPAWN_BUDGET.as_millis(),
        if r.ready_after > NEW_TAB_SPAWN_BUDGET {
            "OVER"
        } else {
            "within"
        }
    );
    assert!(r.ready_after < DEFAULT_REPORT_TIMEOUT);
    assert_survived(&dir, &r);
    kill_via_protocol(&dir);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The WMI route asked for directly, from a spawner in an outer job that
/// ALLOWS breakaway: WMI's process is outside that job without any breakaway
/// at all.
#[test]
fn pty_holder_breakaway_forced_wmi_route_survives_too() {
    let dir = pane_dir("fwmi");
    let r = spawn_in_outer_job_then_close_it(&dir, true, "wmi");
    assert_eq!(r.route, "wmi");
    assert_survived(&dir, &r);
    kill_via_protocol(&dir);
    let _ = std::fs::remove_dir_all(&dir);
}

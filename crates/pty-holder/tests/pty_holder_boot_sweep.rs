//! End-to-end, against the real `qontinui-pty-holder`: daemon-side reaping
//! and the boot sweep (plan `2026-09-12-out-of-process-pty-owner-for-terminal-hosted-sessions`,
//! Phase 3, D8 under D13).
//!
//! - A holder reaps its OWN child tree when its pane is KILLED: a
//!   `SIGHUP`-immune background job does not outlive the kill. When the child
//!   EXITS ON ITS OWN, what it left running is left alone (an in-process
//!   PTY's semantics; decision of 2026-10-14).
//! - A boot sweep leaves a live holder running and hands it back as `Live`.
//! - A boot sweep of a holder that was SIGKILLed — its `SIGHUP`-immune child
//!   orphaned — ends that child, PROVEN by the start token in the lock record,
//!   and removes the pane's files.
//!
//! Every process signalled here was spawned by this test, and every one is
//! guarded ([`PaneSession`], [`HolderGuard`]) so a failed assert leaves no
//! `sleep 300` behind. Unix only (the
//! fixtures are `sh`; the identity proof reads `/proc` on Linux); the Windows
//! sweep is type-checked for `x86_64-pc-windows-msvc` and UNRUN here.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use qontinui_pty_holder::client::{connect, Event};
use qontinui_pty_holder::lock::{read_record, ENDED_BY_EXIT};
use qontinui_pty_holder::pane::{lock_path, socket_path, PaneId};
use qontinui_pty_holder::spawn::{
    spawn_holder, HostFacts, RouteRequest, SpawnRequest, SpawnedHolder, DEFAULT_REPORT_TIMEOUT,
};
use qontinui_pty_holder::spec::ChildSpec;
use qontinui_pty_holder::sweep::{boot_sweep, SweepOutcome};
use qontinui_pty_holder::terminate::terminate_verified_holder;

const BIN: &str = env!("CARGO_BIN_EXE_qontinui-pty-holder");

fn soon() -> Instant {
    Instant::now() + Duration::from_secs(10)
}

fn pane_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .subsec_nanos();
    PathBuf::from("/tmp").join(format!("ptyb-{tag}-{}-{nanos:x}", std::process::id()))
}

/// Alive and not a zombie (a killed orphan is reaped by init, but not
/// instantly).
fn alive(pid: u32) -> bool {
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(raw) => raw
            .rsplit_once(')')
            .and_then(|(_, rest)| rest.split_whitespace().next())
            .is_some_and(|state| state != "Z"),
        // No /proc (macOS): fall back to signal 0.
        // SAFETY: signal 0 only checks for existence.
        Err(_) => unsafe { libc::kill(pid as i32, 0) == 0 },
    }
}

/// The session of a pane's child (it `setsid()`s onto its PTY, so the
/// session id is the child's pid): every member SIGKILLed on drop — the
/// child, a `sleep` it runs in the foreground, a job it put in the background.
/// While any member lives the id is pinned to this session (POSIX), so the
/// sweep reaches only processes this test's holder spawned — which is why a
/// test DISARMS it once it has seen every member dead: from then on the id
/// may be recycled, and a sweep could hit a stranger's session.
struct PaneSession(u32, bool);

impl PaneSession {
    fn new(sid: u32) -> Self {
        PaneSession(sid, true)
    }

    fn disarm(&mut self) {
        self.1 = false;
    }
}

impl Drop for PaneSession {
    fn drop(&mut self) {
        if self.1 {
            let _ = qontinui_pty_holder::reap::reap_session_of_exited_child(self.0);
        }
    }
}

/// A pane's holder: ended through its verified lock on drop (a no-op once it
/// is gone).
struct HolderGuard<'a> {
    dir: &'a Path,
    pane: &'a PaneId,
    pid: u32,
}

impl Drop for HolderGuard<'_> {
    fn drop(&mut self) {
        let _ = terminate_verified_holder(self.dir, self.pane, self.pid);
    }
}

fn wait_dead(pid: u32, what: &str) {
    let end = Instant::now() + Duration::from_secs(10);
    while alive(pid) {
        assert!(Instant::now() < end, "{what} ({pid}) is still alive");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// A pane whose child prints the pid of a background job that ignores
/// `SIGHUP`, then runs `then`.
fn spec_with_background_job(then: &str) -> ChildSpec {
    let mut spec = ChildSpec::new(vec![
        "sh".into(),
        "-c".into(),
        format!("trap '' HUP; sleep 300 & echo BG=$!; {then}").into(),
    ]);
    spec.env = std::env::vars_os().collect();
    spec.cwd = Some("/tmp".into());
    spec
}

fn spawn(dir: &Path, pane: &PaneId, child: &ChildSpec) -> SpawnedHolder {
    // Plain: no transient scope, so the reaping under test is the session
    // sweep, not systemd's.
    spawn_on(dir, pane, child, RouteRequest::Plain)
}

fn spawn_on(dir: &Path, pane: &PaneId, child: &ChildSpec, route: RouteRequest) -> SpawnedHolder {
    spawn_holder(&SpawnRequest {
        holder_exe: Path::new(BIN),
        pane_dir: dir,
        pane_id: pane,
        child,
        route,
        report_timeout: DEFAULT_REPORT_TIMEOUT,
    })
    .expect("spawn")
}

/// Attach from byte 0 and read output until `BG=<pid>` appears — and, with
/// `wait_for_exit`, on until the exit frame (reading it is what delivers it).
fn background_pid(dir: &Path, pane: &PaneId, wait_for_exit: bool) -> u32 {
    let mut s = connect(dir, pane, soon())
        .unwrap()
        .attach(Some(0), soon())
        .unwrap();
    let mut seen = Vec::new();
    let mut bg = None;
    let end = Instant::now() + Duration::from_secs(20);
    while let Ok(Some(ev)) = s.reader.next_event(Some(end)) {
        match ev {
            Event::Output { bytes, .. } => {
                seen.extend_from_slice(&bytes);
                if bg.is_none() {
                    bg = String::from_utf8_lossy(&seen)
                        .lines()
                        .find_map(|l| l.trim().strip_prefix("BG=")?.parse().ok());
                }
                if bg.is_some() && !wait_for_exit {
                    break;
                }
            }
            Event::Exit(_) => break,
            _ => {}
        }
    }
    bg.expect("the child reported its background job")
}

/// D8 as decided 2026-10-14 (review round 2, N1): a child that EXITS ON ITS
/// OWN leaves what it started running — the holder ends the pane without
/// reaping it, its record says so, and the NEXT BOOT SWEEP touches nothing
/// either (on the scope route it would otherwise stop the holder's scope, and
/// the job in it).
fn normal_exit_then_boot_sweep(route: RouteRequest, tag: &str) {
    let dir = pane_dir(tag);
    let pane = PaneId::new("p1").unwrap();
    let h = spawn_on(&dir, &pane, &spec_with_background_job("exit 0"), route);
    let _session = PaneSession::new(h.child_pid);
    let _holder = HolderGuard {
        dir: &dir,
        pane: &pane,
        pid: h.holder_pid,
    };
    let holder = h.holder_pid;
    // Reading to the exit frame delivers it, which is what lets the holder
    // end the pane.
    let bg = background_pid(&dir, &pane, true);
    h.reap_in_background();
    wait_dead(holder, "the holder after its pane ended");
    assert!(
        alive(bg),
        "a normal exit must not reap the child's detached job"
    );
    let record = read_record(&lock_path(&dir, &pane)).expect("the record outlives the holder");
    assert_eq!(record.ended.as_deref(), Some(ENDED_BY_EXIT), "{record:?}");

    let report = boot_sweep(&dir, Duration::from_secs(10), &[], |_, _| {}).unwrap();
    match &report.entries[0].outcome {
        SweepOutcome::Reaped {
            reap,
            orphan_kept: false,
            ..
        } => {
            assert!(reap.killed.is_empty(), "{reap:?}");
            assert!(reap.unattributed.is_empty(), "{reap:?}");
        }
        other => panic!("{other:?}"),
    }
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        alive(bg),
        "the boot sweep must not reap a normally ended pane's job"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn pty_holder_boot_sweep_after_a_normal_exit_leaves_detached_jobs_running() {
    normal_exit_then_boot_sweep(RouteRequest::Plain, "end");
}

/// The same on the scope route, where the sweep's tool is stopping the
/// holder's transient unit — run where a user systemd is reachable.
#[test]
fn pty_holder_boot_sweep_after_a_normal_exit_on_the_scope_route_leaves_the_scope() {
    let facts = HostFacts::probe();
    if !(facts.linux && facts.scope_tooling) {
        eprintln!("pty_holder_boot_sweep: no user systemd here — scope-route case not applicable");
        return;
    }
    normal_exit_then_boot_sweep(RouteRequest::Scope, "endscope");
}

/// Review round 2, N2: a KILL that arrives after the child already exited on
/// its own reaches nothing — the pane still ended by exit, and its leftovers
/// are left alone.
#[test]
fn pty_holder_reap_a_kill_after_a_normal_exit_reaps_nothing() {
    let dir = pane_dir("late");
    let pane = PaneId::new("p1").unwrap();
    let h = spawn(&dir, &pane, &spec_with_background_job("exit 0"));
    let _session = PaneSession::new(h.child_pid);
    let _holder = HolderGuard {
        dir: &dir,
        pane: &pane,
        pid: h.holder_pid,
    };
    let (holder, child) = (h.holder_pid, h.child_pid);
    let bg = background_pid(&dir, &pane, false);
    h.reap_in_background();
    wait_dead(child, "the child that exits on its own");
    // The exit is not collected yet, so the holder lingers: now the late kill.
    let mut s = connect(&dir, &pane, soon())
        .unwrap()
        .attach(None, soon())
        .unwrap();
    s.writer.kill(soon()).unwrap();
    let end = Instant::now() + Duration::from_secs(10);
    while let Ok(Some(ev)) = s.reader.next_event(Some(end)) {
        if matches!(ev, Event::Exit(_)) {
            break;
        }
    }
    drop(s);
    wait_dead(holder, "the holder after the late kill");
    assert!(
        alive(bg),
        "a kill that reached no live child must reap nothing"
    );
    let record = read_record(&lock_path(&dir, &pane)).expect("record");
    assert_eq!(record.ended.as_deref(), Some(ENDED_BY_EXIT), "{record:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// D8, kill path: a pane that is KILLED (a tab closed) has its tree reaped —
/// the `SIGHUP`-immune background job included.
#[test]
fn pty_holder_reap_kill_ends_the_child_tree() {
    let dir = pane_dir("kill");
    let pane = PaneId::new("p1").unwrap();
    let h = spawn(&dir, &pane, &spec_with_background_job("sleep 300"));
    let _holder = HolderGuard {
        dir: &dir,
        pane: &pane,
        pid: h.holder_pid,
    };
    let (holder, child) = (h.holder_pid, h.child_pid);
    let mut session = PaneSession::new(child);
    let bg = background_pid(&dir, &pane, false);
    h.reap_in_background();
    let mut s = connect(&dir, &pane, soon())
        .unwrap()
        .attach(None, soon())
        .unwrap();
    s.writer.kill(soon()).unwrap();
    let end = Instant::now() + Duration::from_secs(10);
    while let Ok(Some(ev)) = s.reader.next_event(Some(end)) {
        if matches!(ev, Event::Exit(_)) {
            break;
        }
    }
    drop(s);
    wait_dead(child, "the killed child");
    wait_dead(bg, "the killed pane's SIGHUP-immune background job");
    session.disarm();
    wait_dead(holder, "the holder after its pane was killed");
    let _ = std::fs::remove_dir_all(&dir);
}

/// D8, sweep side: a live holder is listed and left alone.
#[test]
fn pty_holder_boot_sweep_leaves_a_live_holder_running_and_lists_it() {
    let dir = pane_dir("live");
    let pane = PaneId::new("p1").unwrap();
    let h = spawn(&dir, &pane, &spec_with_background_job("sleep 300"));
    let _holder = HolderGuard {
        dir: &dir,
        pane: &pane,
        pid: h.holder_pid,
    };
    let _session = PaneSession::new(h.child_pid);
    let mut adopted = Vec::new();
    let report = boot_sweep(&dir, Duration::from_secs(10), &[], |entries, _dead| {
        adopted = entries
            .iter()
            .map(|e| e.pane_id.clone())
            .collect::<Vec<_>>();
    })
    .unwrap();
    assert_eq!(adopted, vec!["p1".to_string()], "classified before reaping");
    let live: Vec<_> = report
        .live()
        .map(|(id, ack)| (id.to_string(), ack.holder_pid))
        .collect();
    assert_eq!(live, vec![("p1".to_string(), h.holder_pid)], "{report:?}");
    assert!(alive(h.holder_pid) && alive(h.child_pid));
    assert!(lock_path(&dir, &pane).exists());
    let record = read_record(&lock_path(&dir, &pane)).expect("record");
    assert!(
        record.child_start.is_some(),
        "the record carries the child's start token"
    );
    h.teardown();
    let _ = std::fs::remove_dir_all(&dir);
}

/// D8, sweep side: the holder is SIGKILLed; its child ignores the hang-up and
/// survives as an orphan. The next boot sweep finds the lock free, proves the
/// orphan is the recorded child by its start token, ends its tree, and removes
/// the pane's files.
#[test]
fn pty_holder_boot_sweep_reaps_the_orphans_of_a_killed_holder() {
    let dir = pane_dir("orphan");
    let pane = PaneId::new("p1").unwrap();
    let h = spawn(&dir, &pane, &spec_with_background_job("sleep 300"));
    let (holder, child) = (h.holder_pid, h.child_pid);
    let mut session = PaneSession::new(child);
    let bg = background_pid(&dir, &pane, false);
    h.reap_in_background();

    let t = terminate_verified_holder(&dir, &pane, holder);
    assert!(t.signalled, "{t:?}");
    wait_dead(holder, "the SIGKILLed holder");
    // The hang-up reached the child; it ignores SIGHUP, so it and its job are
    // orphans now.
    std::thread::sleep(Duration::from_millis(300));
    assert!(alive(child), "fixture: the child survives its holder");
    assert!(alive(bg), "fixture: the background job survives its holder");

    let report = boot_sweep(&dir, Duration::from_secs(10), &[], |_, _| {}).unwrap();
    assert_eq!(report.entries.len(), 1, "{report:?}");
    match &report.entries[0].outcome {
        SweepOutcome::Reaped {
            record: Some(record),
            reap,
            lock_removed: true,
            orphan_kept: false,
        } => {
            assert_eq!(record.holder_pid, holder);
            if cfg!(target_os = "linux") {
                assert!(reap.killed.contains(&child), "{reap:?}");
                assert!(reap.killed.contains(&bg), "{reap:?}");
            }
        }
        other => panic!("{other:?}"),
    }
    if cfg!(target_os = "linux") {
        wait_dead(child, "the orphaned child");
        wait_dead(bg, "the orphaned background job");
        session.disarm();
    }
    // Off Linux there is no start token, so nothing was provable and nothing
    // signalled: the `PaneSession` guard cleans the fixture up.
    assert!(
        !lock_path(&dir, &pane).exists(),
        "the dead pane's lock is gone"
    );
    assert!(!socket_path(&dir, &pane).exists(), "and its endpoint");
    let _ = std::fs::remove_dir_all(&dir);
}

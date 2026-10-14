//! End-to-end, against the real `qontinui-pty-holder`: daemon-side reaping
//! and the boot sweep (plan `2026-09-12-out-of-process-pty-owner-for-terminal-hosted-sessions`,
//! Phase 3, D8 under D13).
//!
//! - A holder reaps its OWN child tree when its pane ends: a `SIGHUP`-immune
//!   background job the child left behind does not outlive the pane.
//! - A boot sweep leaves a live holder running and hands it back as `Live`.
//! - A boot sweep of a holder that was SIGKILLed — its `SIGHUP`-immune child
//!   orphaned — ends that child, PROVEN by the start token in the lock record,
//!   and removes the pane's files.
//!
//! Every process signalled here was spawned by this test. Unix only (the
//! fixtures are `sh`; the identity proof reads `/proc` on Linux); the Windows
//! sweep is type-checked for `x86_64-pc-windows-msvc` and UNRUN here.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use qontinui_pty_holder::client::{connect, Event};
use qontinui_pty_holder::lock::read_record;
use qontinui_pty_holder::pane::{lock_path, socket_path, PaneId};
use qontinui_pty_holder::spawn::{
    spawn_holder, RouteRequest, SpawnRequest, SpawnedHolder, DEFAULT_REPORT_TIMEOUT,
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
    spawn_holder(&SpawnRequest {
        holder_exe: Path::new(BIN),
        pane_dir: dir,
        pane_id: pane,
        child,
        // Plain: no transient scope, so the reaping under test is the
        // session sweep, not systemd's.
        route: RouteRequest::Plain,
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

/// D8, holder side: when the pane ends, its holder ends what the child left
/// running in its session.
#[test]
fn pty_holder_reap_holder_ends_its_child_tree_when_the_pane_ends() {
    let dir = pane_dir("end");
    let pane = PaneId::new("p1").unwrap();
    let h = spawn(&dir, &pane, &spec_with_background_job("exit 0"));
    let holder = h.holder_pid;
    // Reading to the exit frame delivers it, which is what lets the holder
    // end the pane.
    let bg = background_pid(&dir, &pane, true);
    h.reap_in_background();
    wait_dead(holder, "the holder after its pane ended");
    wait_dead(bg, "the child's SIGHUP-immune background job");
    let _ = std::fs::remove_dir_all(&dir);
}

/// D8, sweep side: a live holder is listed and left alone.
#[test]
fn pty_holder_boot_sweep_leaves_a_live_holder_running_and_lists_it() {
    let dir = pane_dir("live");
    let pane = PaneId::new("p1").unwrap();
    let h = spawn(&dir, &pane, &spec_with_background_job("sleep 300"));
    let report = boot_sweep(&dir, Duration::from_secs(10), &[]).unwrap();
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

    let report = boot_sweep(&dir, Duration::from_secs(10), &[]).unwrap();
    assert_eq!(report.entries.len(), 1, "{report:?}");
    match &report.entries[0].outcome {
        SweepOutcome::Reaped {
            record: Some(record),
            reap,
            lock_removed: true,
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
    } else {
        // No start token off Linux: nothing is provable, so nothing was
        // signalled. Clean up the fixture by hand.
        // SAFETY: the process groups of children this test's holder spawned.
        unsafe {
            libc::kill(-(child as i32), libc::SIGKILL);
        }
    }
    assert!(
        !lock_path(&dir, &pane).exists(),
        "the dead pane's lock is gone"
    );
    assert!(!socket_path(&dir, &pane).exists(), "and its endpoint");
    let _ = std::fs::remove_dir_all(&dir);
}

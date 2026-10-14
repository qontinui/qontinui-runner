//! End-to-end durable scrollback (plan 2026-09-12 Phase 4, D7): the real
//! `qontinui-pty-holder` binary persists its pane's tail on its own — while
//! the child runs (the timer), across a SIGKILL of the holder itself (the
//! unscheduled event D7 exists for), and once more when the holder ends.
//!
//! Every holder here is started through `spawn::spawn_holder`, the runner's
//! production spawn entry (`DaemonPaneIo` reaches it through
//! `spawn_pane_holder`), and ended through `SpawnedHolder::teardown`, which
//! signals the PTY child only after `/proc` proves it is still the holder's —
//! never a pid this test cannot prove is its own.
//!
//! Unix only: the children are POSIX shell scripts and the holder is killed
//! with SIGKILL. The Windows arm of `crate::scrollback` (rename over the
//! target, the pane directory's per-user ACL) is UNRUN here, not passed.
#![cfg(unix)]

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use qontinui_pty_holder::pane::PaneId;
use qontinui_pty_holder::scrollback::{read_scrollback, Scrollback, SCROLLBACK_INTERVAL};
use qontinui_pty_holder::spawn::{
    spawn_holder, RouteRequest, SpawnRequest, SpawnedHolder, DEFAULT_REPORT_TIMEOUT,
};
use qontinui_pty_holder::spec::ChildSpec;

const BIN: &str = env!("CARGO_BIN_EXE_qontinui-pty-holder");

/// A fresh, short pane dir (a Unix socket path is capped at ~104-108 bytes).
fn pane_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .subsec_nanos();
    PathBuf::from("/tmp").join(format!("ptysb-{tag}-{}-{nanos:x}", std::process::id()))
}

fn pid(id: &str) -> PaneId {
    PaneId::new(id).unwrap()
}

fn sh(script: &str) -> ChildSpec {
    let mut spec = ChildSpec::new(vec!["/bin/sh".into(), "-c".into(), script.into()]);
    spec.env = std::env::vars_os().collect();
    spec.cwd = Some(OsString::from("/tmp"));
    spec
}

/// The production spawn request, exactly as the runner builds it.
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

/// A holder this test spawned through `spawn_holder`. Dropping it runs the
/// production teardown (verified-pid only) and removes the pane dir.
struct Pane {
    holder: Option<SpawnedHolder>,
    dir: PathBuf,
    child_pid: u32,
}

impl Pane {
    fn start(tag: &str, spec: ChildSpec) -> Pane {
        let dir = pane_dir(tag);
        let pane = pid("p1");
        let holder = spawn_holder(&request(&dir, &pane, &spec)).expect("spawn_holder");
        Pane {
            child_pid: holder.child_pid,
            holder: Some(holder),
            dir,
        }
    }

    fn holder(&mut self) -> &mut SpawnedHolder {
        self.holder.as_mut().expect("holder still owned")
    }

    /// Whether the holder has exited (it is then reaped, once, here).
    fn exited(&mut self) -> bool {
        self.holder().try_reap().unwrap().is_some()
    }

    fn tail(&self) -> Option<Scrollback> {
        read_scrollback(&self.dir, &pid("p1")).expect("a whole, readable tail")
    }
}

impl Drop for Pane {
    fn drop(&mut self) {
        if let Some(h) = self.holder.take() {
            h.teardown();
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

/// Poll the durable tail until it holds `needle`, for at most `bound`.
fn tail_with(p: &Pane, needle: &[u8], bound: Duration) -> Scrollback {
    let end = Instant::now() + bound;
    loop {
        if let Some(sb) = p.tail() {
            if contains(&sb.bytes, needle) {
                return sb;
            }
        }
        assert!(
            Instant::now() < end,
            "durable tail never held {needle:?} within {bound:?}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The arming proof: a holder started through the PRODUCTION spawn entry,
/// `spawn::spawn_holder`, persists what its child prints — with no switch.
#[test]
fn pty_holder_scrollback_production_spawn_entry_persists_output() {
    let dir = pane_dir("arm");
    let pane = pid("p1");
    let spec = sh("printf 'armed-marker'; sleep 60");
    let holder = spawn_holder(&request(&dir, &pane, &spec)).expect("spawn_holder");
    let mut p = Pane {
        child_pid: holder.child_pid,
        holder: Some(holder),
        dir,
    };
    let sb = tail_with(&p, b"armed-marker", SCROLLBACK_INTERVAL * 3);
    assert_eq!(sb.child_pid, p.child_pid);
    assert_eq!(sb.start_offset, 0);
    assert!(!p.exited(), "the pane is still live: the timer wrote this");
}

#[test]
fn pty_holder_scrollback_timer_persists_and_survives_holder_sigkill() {
    // Output arrives after the holder's first (immediate) tick, so only the
    // TIMER can put it on disk; then the holder is SIGKILLed — no final
    // write, no graceful anything.
    let mut p = Pane::start("kill", sh("sleep 0.5; printf 'durable-marker-1'; sleep 60"));
    let sb = tail_with(&p, b"durable-marker-1", SCROLLBACK_INTERVAL * 3);
    assert_eq!(sb.child_pid, p.child_pid);
    assert_eq!(sb.start_offset, 0, "a short stream's tail starts at 0");

    // The holder is this process's own, still-unreaped child (checked just
    // before), so its pid cannot have been recycled.
    assert!(!p.exited());
    let holder_pid = p.holder().holder_pid;
    // SAFETY: SIGKILL to our own unreaped child process.
    assert_eq!(unsafe { libc::kill(holder_pid as i32, libc::SIGKILL) }, 0);
    let end = Instant::now() + Duration::from_secs(10);
    while !p.exited() {
        assert!(Instant::now() < end, "SIGKILLed holder not reaped in 10 s");
        std::thread::sleep(Duration::from_millis(20));
    }
    let after = p.tail().expect("the killed holder's tail is still there");
    assert!(contains(&after.bytes, b"durable-marker-1"));
    assert_eq!(after.child_pid, p.child_pid);
    assert!(after.end_offset() >= sb.end_offset());
}

#[test]
fn pty_holder_scrollback_final_write_when_holder_ends() {
    // The child prints only AFTER the holder's immediate first tick, and the
    // holder ends ~0.2 s after the child (nobody attaches; short linger) —
    // long before the next 5 s tick. So the bytes can reach disk only
    // through the final write, made before the lock is released.
    let mut spec = sh("sleep 0.5; printf 'last-words-of-the-child'");
    spec.exit_linger_ms = Some(200);
    let mut p = Pane::start("end", spec);
    let started = Instant::now();
    while !p.exited() {
        assert!(
            started.elapsed() < Duration::from_secs(15),
            "holder did not end within 15 s"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        started.elapsed() < SCROLLBACK_INTERVAL,
        "the holder outlived a timer interval ({:?}); this test would no longer isolate the final write",
        started.elapsed()
    );
    let sb = p.tail().expect("a tail was persisted");
    assert!(
        contains(&sb.bytes, b"last-words-of-the-child"),
        "{:?}",
        sb.bytes
    );
    assert_eq!(sb.child_pid, p.child_pid);
    assert_eq!(sb.start_offset, 0);
}

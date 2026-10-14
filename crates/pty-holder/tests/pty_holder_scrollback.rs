//! End-to-end durable scrollback (plan 2026-09-12 Phase 4, D7): the real
//! `qontinui-pty-holder` binary persists its pane's tail on its own — while
//! the child runs (the timer), across a SIGKILL of the holder itself (the
//! unscheduled event D7 exists for), and once more when the holder ends.
//!
//! Unix only: the children are POSIX shell scripts and the holder is killed
//! with SIGKILL. The Windows arm of `crate::scrollback` (rename over the
//! target, the pane directory's per-user ACL) is UNRUN here, not passed.
#![cfg(unix)]

use std::ffi::OsString;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use qontinui_pty_holder::pane::{scrollback_path, PaneId};
use qontinui_pty_holder::scrollback::{read_scrollback, Scrollback, SCROLLBACK_INTERVAL};
use qontinui_pty_holder::spec::{write_spec, ChildSpec};

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

/// A holder this test spawned; killed and reaped on drop.
struct Holder {
    child: Child,
    dir: PathBuf,
    child_pid: u32,
}

impl Drop for Holder {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        // SAFETY: SIGKILL to the pane child's group and pid, which this test
        // started; a stale pid is at worst an ESRCH.
        unsafe {
            libc::kill(-(self.child_pid as i32), libc::SIGKILL);
            libc::kill(self.child_pid as i32, libc::SIGKILL);
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn sh(script: &str) -> ChildSpec {
    let mut spec = ChildSpec::new(vec!["/bin/sh".into(), "-c".into(), script.into()]);
    spec.env = std::env::vars_os().collect();
    spec.cwd = Some(OsString::from("/tmp"));
    spec
}

fn start(tag: &str, spec: ChildSpec) -> Holder {
    let dir = pane_dir(tag);
    write_spec(&dir, &pid("p1"), &spec).unwrap();
    let mut child = Command::new(BIN)
        .arg("--pane-dir")
        .arg(&dir)
        .arg("--pane-id")
        .arg("p1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let out = child.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let _ = BufReader::new(out).read_line(&mut line);
        let _ = tx.send(line);
    });
    let line = rx
        .recv_timeout(Duration::from_secs(15))
        .expect("holder printed no ready line within 15 s");
    assert!(line.starts_with("holder_ready "), "{line}");
    let child_pid: u32 = line
        .split_whitespace()
        .find_map(|t| t.strip_prefix("child_pid="))
        .unwrap()
        .parse()
        .unwrap();
    Holder {
        child,
        dir,
        child_pid,
    }
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

/// Poll the durable tail until it holds `needle`, for at most `bound`.
fn tail_with(h: &Holder, needle: &[u8], bound: Duration) -> Scrollback {
    let end = Instant::now() + bound;
    loop {
        if let Ok(Some(sb)) = read_scrollback(&h.dir, &pid("p1")) {
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

#[test]
fn pty_holder_scrollback_timer_persists_and_survives_holder_sigkill() {
    // Output arrives after the holder's first (immediate) tick, so only the
    // TIMER can put it on disk; then the holder is SIGKILLed — no final
    // write, no graceful anything.
    let mut h = start("kill", sh("sleep 0.5; printf 'durable-marker-1'; sleep 60"));
    let sb = tail_with(&h, b"durable-marker-1", SCROLLBACK_INTERVAL * 3);
    assert_eq!(sb.child_pid, h.child_pid);
    assert_eq!(sb.start_offset, 0, "a short stream's tail starts at 0");

    // SIGKILL the holder: the durable tail is all that is left of the
    // stream, and it is whole — a kill mid-write could leave only a `.tmp`.
    h.child.kill().unwrap();
    h.child.wait().unwrap();
    assert!(scrollback_path(&h.dir, &pid("p1")).is_file());
    let after = read_scrollback(&h.dir, &pid("p1"))
        .expect("the tail a killed holder leaves is whole")
        .unwrap();
    assert!(contains(&after.bytes, b"durable-marker-1"));
    assert_eq!(after.child_pid, h.child_pid);
    assert!(after.end_offset() >= sb.end_offset());
}

#[test]
fn pty_holder_scrollback_final_write_when_holder_ends() {
    // The child prints and exits at once; nobody attaches, so the holder ends
    // after its (short) exit linger. Its last act before releasing the lock
    // is persisting the tail — with the child's final bytes in it.
    let mut spec = sh("printf 'last-words-of-the-child'");
    spec.exit_linger_ms = Some(200);
    let mut h = start("end", spec);
    let end = Instant::now() + Duration::from_secs(15);
    loop {
        if h.child.try_wait().unwrap().is_some() {
            break;
        }
        assert!(Instant::now() < end, "holder did not end within 15 s");
        std::thread::sleep(Duration::from_millis(20));
    }
    let sb = read_scrollback(&h.dir, &pid("p1")).unwrap().unwrap();
    assert!(
        contains(&sb.bytes, b"last-words-of-the-child"),
        "{:?}",
        sb.bytes
    );
    assert_eq!(sb.child_pid, h.child_pid);
    assert_eq!(sb.start_offset, 0);
}

//! End-to-end: the real `qontinui-pty-holder` binary, spawned by the test,
//! talked to through the library's client.
//!
//! Every process these tests kill (or stop) is one they spawned themselves —
//! the `Spawned` guard kills and reaps its own child on drop and nothing else.
//!
//! Cross-platform unless marked: the `#[cfg(unix)]` tests need `SIGSTOP` or
//! Unix file modes; on Windows everything else runs as-is (UNRUN on the Linux
//! box this was written on).

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use qontinui_pty_holder::client::{
    census, census_with_versions, connect, connect_raw, connect_with_versions, probe,
    probe_with_versions, ConnectError, Probe,
};
use qontinui_pty_holder::frame::{KIND_CONTROL, KIND_DATA};
use qontinui_pty_holder::lock::{read_record, PaneLock, TryLock};
use qontinui_pty_holder::pane::{lock_path, PaneId};
use qontinui_pty_holder::protocol::{
    parse_reply, to_payload, RejectReason, Reply, Request, PROTOCOL_VERSIONS,
};

const BIN: &str = env!("CARGO_BIN_EXE_qontinui-pty-holder");

fn soon() -> Instant {
    Instant::now() + Duration::from_secs(5)
}

/// A fresh, short pane dir. Short because a Unix socket path is capped at
/// ~104-108 bytes and `$TMPDIR` can be long.
fn pane_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .subsec_nanos();
    let base = if cfg!(unix) {
        PathBuf::from("/tmp")
    } else {
        std::env::temp_dir()
    };
    base.join(format!("ptyh-{tag}-{}-{nanos:x}", std::process::id()))
}

fn pid(id: &str) -> PaneId {
    PaneId::new(id).unwrap()
}

/// A holder process this test spawned. Killed and reaped on drop.
struct Spawned {
    child: Child,
}

impl Spawned {
    fn pid(&self) -> u32 {
        self.child.id()
    }

    fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Spawned {
    fn drop(&mut self) {
        #[cfg(unix)]
        // A stopped child ignores nothing but SIGKILL, which kill() sends; the
        // SIGCONT just keeps a stopped test holder from lingering as `T`.
        unsafe {
            libc::kill(self.child.id() as i32, libc::SIGCONT);
        }
        self.kill();
    }
}

/// Spawn a holder and wait for its one-line report.
fn spawn_holder(dir: &Path, pane: &str) -> (Spawned, String) {
    let mut child = Command::new(BIN)
        .arg("--pane-dir")
        .arg(dir)
        .arg("--pane-id")
        .arg(pane)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn holder");
    let stdout = child.stdout.take().unwrap();
    // Bounded: a holder that never prints must fail the test, not hang it.
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let _ = BufReader::new(stdout).read_line(&mut line);
        let _ = tx.send(line);
    });
    let spawned = Spawned { child };
    match rx.recv_timeout(Duration::from_secs(15)) {
        Ok(line) => (spawned, line.trim().to_string()),
        // `spawned` is dropped here, killing and reaping the child.
        Err(_) => panic!("holder printed no report line within 15 s"),
    }
}

/// A private pane dir (0700), as the runner will create it — never one whose
/// mode depends on this test process's umask.
fn make_private_dir(dir: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .unwrap();
    }
    #[cfg(not(unix))]
    std::fs::create_dir_all(dir).unwrap();
}

/// Probe until `want` holds or `bound` elapses; returns the last verdict.
/// A holder's lock is released by the OS as the process is torn down, which on
/// Windows can lag the `wait()` that reaped it — so "dead" is polled for, not
/// asserted once.
fn probe_until(dir: &Path, pane: &str, bound: Duration, want: impl Fn(&Probe) -> bool) -> Probe {
    let end = Instant::now() + bound;
    loop {
        let p = probe(dir, &pid(pane), Instant::now() + Duration::from_secs(2));
        if want(&p) || Instant::now() >= end {
            return p;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn spawn_ready(dir: &Path, pane: &str) -> Spawned {
    let (h, line) = spawn_holder(dir, pane);
    assert!(
        line.starts_with("holder_ready "),
        "holder did not start: {line}"
    );
    h
}

fn expect_rejected(frame: Option<qontinui_pty_holder::frame::Frame>, want: RejectReason) {
    let frame = frame.expect("a rejection frame before close");
    assert_eq!(frame.kind, KIND_CONTROL);
    match parse_reply(&frame.payload).unwrap() {
        Reply::Rejected { reason, .. } => assert_eq!(reason, want),
        other => panic!("expected rejected({want:?}), got {other:?}"),
    }
}

#[test]
fn pty_holder_probe_of_a_pane_with_no_lock_is_absent() {
    let dir = pane_dir("absent");
    assert_eq!(probe(&dir, &pid("nobody"), soon()), Probe::Absent);
    assert!(census(&dir, Duration::from_secs(1), &[])
        .unwrap()
        .is_empty());
}

#[test]
fn pty_holder_handshake_ping_census_and_lock_record() {
    let dir = pane_dir("hs");
    let h = spawn_ready(&dir, "p1");

    let mut c = connect(&dir, &pid("p1"), soon()).expect("handshake");
    let ack = c.hello_ack().clone();
    assert_eq!(ack.version, *PROTOCOL_VERSIONS.iter().max().unwrap());
    assert_eq!(ack.holder_pid, h.pid());
    assert_eq!(ack.child_pid, None, "no PTY in Phase 1");
    c.ping(soon()).unwrap();
    let cen = c.census(soon()).unwrap();
    assert_eq!(cen.pane_id, "p1");
    assert_eq!(cen.holder_pid, h.pid());
    assert_eq!(cen.versions, PROTOCOL_VERSIONS.to_vec());

    // prepare_upgrade is in the envelope and answered typed, connection kept.
    match c.request(&Request::PrepareUpgrade, soon()).unwrap() {
        Reply::Unsupported { verb, .. } => assert_eq!(verb, "prepare_upgrade"),
        other => panic!("{other:?}"),
    }
    c.ping(soon()).unwrap();

    // The lock names the holder.
    let rec = read_record(&lock_path(&dir, &pid("p1"))).expect("lock record");
    assert_eq!(rec.holder_pid, h.pid());
    assert_eq!(rec.versions, PROTOCOL_VERSIONS.to_vec());
    assert_eq!(rec.child_pid, None);
    assert!(rec.started_at_unix_ms > 0);

    match probe(&dir, &pid("p1"), soon()) {
        Probe::Healthy { hello_ack } => assert_eq!(hello_ack.holder_pid, h.pid()),
        other => panic!("{other:?}"),
    }
    drop(h);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The positive allowlist (plan D5): an unknown verb gets a typed rejection
/// and the connection is closed.
#[test]
fn pty_holder_unknown_verb_is_rejected_and_closed() {
    let dir = pane_dir("verb");
    let h = spawn_ready(&dir, "p1");

    let mut c = connect(&dir, &pid("p1"), soon()).unwrap();
    // `terminal_create` is the verb the remote path's allowlist-by-omission
    // once let through; it must not exist here.
    c.send_raw_frame(
        KIND_CONTROL,
        br#"{"type":"terminal_create","cwd":"/"}"#,
        soon(),
    )
    .unwrap();
    expect_rejected(c.recv_raw_frame(soon()).unwrap(), RejectReason::UnknownVerb);
    assert!(
        c.recv_raw_frame(soon()).unwrap().is_none(),
        "closed after rejection"
    );

    // The holder itself is unaffected.
    assert!(matches!(
        probe(&dir, &pid("p1"), soon()),
        Probe::Healthy { .. }
    ));
    drop(h);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn pty_holder_refusals_before_and_after_handshake() {
    let dir = pane_dir("refuse");
    let h = spawn_ready(&dir, "p1");
    let pane = pid("p1");

    // A non-hello first frame.
    let mut r = connect_raw(&dir, &pane, soon()).unwrap();
    r.send_frame(KIND_CONTROL, &to_payload(&Request::Ping), soon())
        .unwrap();
    expect_rejected(
        r.recv_frame(soon()).unwrap(),
        RejectReason::HandshakeRequired,
    );
    assert!(r.recv_frame(soon()).unwrap().is_none());

    // A data frame first.
    let mut r = connect_raw(&dir, &pane, soon()).unwrap();
    r.send_frame(KIND_DATA, &[0xFF, 0x00], soon()).unwrap();
    expect_rejected(
        r.recv_frame(soon()).unwrap(),
        RejectReason::UnexpectedDataFrame,
    );

    // An unknown frame kind.
    let mut r = connect_raw(&dir, &pane, soon()).unwrap();
    r.send_frame(0x7F, b"{}", soon()).unwrap();
    expect_rejected(
        r.recv_frame(soon()).unwrap(),
        RejectReason::UnknownFrameKind,
    );

    // Not JSON.
    let mut r = connect_raw(&dir, &pane, soon()).unwrap();
    r.send_frame(KIND_CONTROL, &[0xC3, 0x28], soon()).unwrap();
    expect_rejected(r.recv_frame(soon()).unwrap(), RejectReason::Malformed);

    // A data frame after a good handshake.
    let mut c = connect(&dir, &pane, soon()).unwrap();
    c.send_raw_frame(KIND_DATA, b"x", soon()).unwrap();
    expect_rejected(
        c.recv_raw_frame(soon()).unwrap(),
        RejectReason::UnexpectedDataFrame,
    );

    // A second hello.
    let mut c = connect(&dir, &pane, soon()).unwrap();
    c.send_raw_frame(
        KIND_CONTROL,
        &to_payload(&Request::Hello { versions: vec![1] }),
        soon(),
    )
    .unwrap();
    expect_rejected(
        c.recv_raw_frame(soon()).unwrap(),
        RejectReason::DuplicateHello,
    );

    assert!(matches!(probe(&dir, &pane, soon()), Probe::Healthy { .. }));
    drop(h);
    let _ = std::fs::remove_dir_all(&dir);
}

/// D15: no common version is a visible, typed state — and the envelope verbs
/// still answer, so a runner far ahead can still identify and count the holder.
#[test]
fn pty_holder_no_common_version_is_incompatible_not_dead() {
    let dir = pane_dir("ver");
    let h = spawn_ready(&dir, "p1");
    let pane = pid("p1");
    let future = [9_999u32];

    match connect_with_versions(&dir, &pane, soon(), &future) {
        Err(ConnectError::Incompatible { holder_versions }) => {
            assert_eq!(holder_versions, PROTOCOL_VERSIONS.to_vec())
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        probe_with_versions(&dir, &pane, soon(), &future),
        Probe::Incompatible {
            holder_versions: PROTOCOL_VERSIONS.to_vec()
        }
    );
    let rows = census_with_versions(&dir, Duration::from_secs(3), &[], &future).unwrap();
    assert_eq!(rows.len(), 1);
    assert!(matches!(rows[0].probe, Probe::Incompatible { .. }));

    // Envelope verbs after no_common_version; a version-scoped one is refused.
    let mut r = connect_raw(&dir, &pane, soon()).unwrap();
    r.send_frame(
        KIND_CONTROL,
        &to_payload(&Request::Hello {
            versions: future.to_vec(),
        }),
        soon(),
    )
    .unwrap();
    let f = r.recv_frame(soon()).unwrap().unwrap();
    assert!(matches!(
        parse_reply(&f.payload).unwrap(),
        Reply::NoCommonVersion { .. }
    ));
    r.send_frame(KIND_CONTROL, &to_payload(&Request::Census), soon())
        .unwrap();
    let f = r.recv_frame(soon()).unwrap().unwrap();
    match parse_reply(&f.payload).unwrap() {
        Reply::CensusReply(c) => assert_eq!(c.holder_pid, h.pid()),
        other => panic!("{other:?}"),
    }
    r.send_frame(KIND_CONTROL, &to_payload(&Request::Ping), soon())
        .unwrap();
    expect_rejected(
        r.recv_frame(soon()).unwrap(),
        RejectReason::VerbNotInVersion,
    );

    drop(h);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Lock before endpoint: a second holder for a live pane exits `lock_held`
/// (exit 3) without touching the first's endpoint.
#[test]
fn pty_holder_second_holder_for_a_live_pane_refuses() {
    let dir = pane_dir("dup");
    let first = spawn_ready(&dir, "p1");
    let (mut second, line) = spawn_holder(&dir, "p1");
    assert!(line.starts_with("holder_error=lock_held"), "{line}");
    assert!(
        line.contains(&first.pid().to_string()),
        "names the holder: {line}"
    );
    let status = second.child.wait().unwrap();
    assert_eq!(status.code(), Some(3));
    match probe(&dir, &pid("p1"), soon()) {
        Probe::Healthy { hello_ack } => assert_eq!(hello_ack.holder_pid, first.pid()),
        other => panic!("{other:?}"),
    }
    drop(first);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A killed holder is Dead (its lock is acquirable); a new holder for the same
/// pane recovers the stale endpoint under the lock.
#[test]
fn pty_holder_killed_holder_is_dead_and_the_pane_is_reusable() {
    let dir = pane_dir("dead");
    let mut h = spawn_ready(&dir, "p1");
    let old_pid = h.pid();
    h.kill();

    match probe_until(&dir, "p1", Duration::from_secs(5), |p| {
        matches!(p, Probe::Dead { .. })
    }) {
        Probe::Dead { record } => assert_eq!(record.unwrap().holder_pid, old_pid),
        other => panic!("{other:?}"),
    }
    #[cfg(unix)]
    assert!(
        dir.join("p1.sock").exists(),
        "a SIGKILLed holder leaves its socket file behind"
    );

    let h2 = spawn_ready(&dir, "p1");
    match probe(&dir, &pid("p1"), soon()) {
        Probe::Healthy { hello_ack } => {
            assert_eq!(hello_ack.holder_pid, h2.pid());
            assert_ne!(hello_ack.holder_pid, old_pid);
        }
        other => panic!("{other:?}"),
    }
    drop(h2);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A held lock with no endpoint (a holder between lock and bind, or one that
/// lost its endpoint) is UNKNOWN — never Dead, never Absent, never Healthy.
#[test]
fn pty_holder_held_lock_without_handshake_is_unknown() {
    let dir = pane_dir("held");
    make_private_dir(&dir);
    let _lock = match PaneLock::try_acquire(&lock_path(&dir, &pid("p1"))).unwrap() {
        TryLock::Acquired(l) => l,
        TryLock::Held => panic!("fresh"),
    };
    match probe(&dir, &pid("p1"), soon()) {
        Probe::Unknown { .. } => {}
        other => panic!("held lock must be Unknown, got {other:?}"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// THE health rule: a wedged holder still accepts from the kernel backlog, so
/// a connect succeeds — and the probe must still say Unknown, within its
/// deadline, because the handshake is unanswered.
#[cfg(unix)]
#[test]
fn pty_holder_wedged_holder_is_unknown_within_the_deadline() {
    let dir = pane_dir("wedge");
    let h = spawn_ready(&dir, "p1");
    // SAFETY: stopping a child this test spawned.
    assert_eq!(unsafe { libc::kill(h.pid() as i32, libc::SIGSTOP) }, 0);

    // The kernel still completes a connect with no holder code running.
    let raw = connect_raw(&dir, &pid("p1"), soon());
    assert!(raw.is_ok(), "connect succeeds against a stopped holder");

    let started = Instant::now();
    let verdict = probe(
        &dir,
        &pid("p1"),
        Instant::now() + Duration::from_millis(500),
    );
    let took = started.elapsed();
    match verdict {
        Probe::Unknown { record, .. } => assert_eq!(record.unwrap().holder_pid, h.pid()),
        other => panic!("a wedged holder must be Unknown, got {other:?}"),
    }
    assert!(
        took < Duration::from_secs(3),
        "probe overran its deadline: {took:?}"
    );
    drop(h);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Census: every lock file probed concurrently under ONE deadline, each pane
/// classified, and a wedged pane does not stretch the census past it.
#[cfg(unix)]
#[test]
fn pty_holder_census_classifies_every_pane_under_one_deadline() {
    let dir = pane_dir("census");
    let healthy = spawn_ready(&dir, "a-healthy");
    let mut dead = spawn_ready(&dir, "b-dead");
    let wedged = spawn_ready(&dir, "c-wedged");
    let wedged2 = spawn_ready(&dir, "d-wedged");
    let wedged3 = spawn_ready(&dir, "e-wedged");
    dead.kill();
    assert!(matches!(
        probe_until(&dir, "b-dead", Duration::from_secs(5), |p| matches!(
            p,
            Probe::Dead { .. }
        )),
        Probe::Dead { .. }
    ));
    for w in [&wedged, &wedged2, &wedged3] {
        // SAFETY: stopping children this test spawned.
        assert_eq!(unsafe { libc::kill(w.pid() as i32, libc::SIGSTOP) }, 0);
    }
    // A foreign lock file whose name is not a pane id is reported, not skipped.
    std::fs::write(dir.join("not a pane.lock"), b"").unwrap();

    let timeout = Duration::from_millis(800);
    let started = Instant::now();
    let rows = census(&dir, timeout, &[]).unwrap();
    let took = started.elapsed();
    // Relative bound: three wedged panes probed serially would take >= 3x the
    // deadline; concurrently under one deadline, ~1x. 2x leaves a full
    // deadline of scheduling slack for a loaded box and still fails serial.
    assert!(took < timeout * 2, "census overran: {took:?}");

    let by: std::collections::HashMap<_, _> = rows
        .iter()
        .map(|r| (r.pane_id.as_str(), &r.probe))
        .collect();
    assert_eq!(rows.len(), 6, "{rows:#?}");
    assert!(
        matches!(by["a-healthy"], Probe::Healthy { hello_ack } if hello_ack.holder_pid == healthy.pid())
    );
    assert!(
        matches!(by["b-dead"], Probe::Dead { .. }),
        "{:?}",
        by["b-dead"]
    );
    assert!(
        matches!(by["c-wedged"], Probe::Unknown { .. }),
        "{:?}",
        by["c-wedged"]
    );
    assert!(
        matches!(by["d-wedged"], Probe::Unknown { .. }),
        "{:?}",
        by["d-wedged"]
    );
    assert!(
        matches!(by["e-wedged"], Probe::Unknown { .. }),
        "{:?}",
        by["e-wedged"]
    );
    assert!(matches!(by["not a pane"], Probe::Unknown { .. }));
    drop((healthy, wedged, wedged2, wedged3));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Census without Unix-only machinery: healthy + dead, on every OS.
#[test]
fn pty_holder_census_healthy_and_dead() {
    let dir = pane_dir("census2");
    let healthy = spawn_ready(&dir, "live");
    let mut dead = spawn_ready(&dir, "gone");
    dead.kill();
    assert!(matches!(
        probe_until(&dir, "gone", Duration::from_secs(5), |p| matches!(
            p,
            Probe::Dead { .. }
        )),
        Probe::Dead { .. }
    ));
    let rows = census(&dir, Duration::from_secs(3), &[]).unwrap();
    assert_eq!(rows.len(), 2, "{rows:#?}");
    assert_eq!(rows[0].pane_id, "gone");
    assert!(matches!(rows[0].probe, Probe::Dead { .. }));
    assert_eq!(rows[1].pane_id, "live");
    assert!(matches!(rows[1].probe, Probe::Healthy { .. }));
    drop(healthy);
    let _ = std::fs::remove_dir_all(&dir);
}

/// D5 on Unix: the pane dir is 0700 and the socket 0600, even when the dir
/// pre-existed with a looser mode.
#[cfg(unix)]
#[test]
fn pty_holder_unix_endpoint_and_dir_are_private() {
    use std::os::unix::fs::PermissionsExt;
    let dir = pane_dir("modes");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    let h = spawn_ready(&dir, "p1");
    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&dir), 0o700);
    assert_eq!(mode(&dir.join("p1.sock")), 0o600);
    assert_eq!(mode(&dir.join("p1.lock")) & 0o077, 0);
    drop(h);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn pty_holder_bin_usage_errors_exit_2() {
    for args in [
        vec!["--pane-id", "p1"],
        vec!["--pane-dir", "/tmp/x"],
        vec!["--pane-dir", "/tmp/x", "--pane-id", "../evil"],
        vec!["--bogus"],
    ] {
        let out = Command::new(BIN).args(&args).output().unwrap();
        assert_eq!(out.status.code(), Some(2), "{args:?}");
    }
    let out = Command::new(BIN)
        .args(["--pane-dir", "relative", "--pane-id", "p1"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.starts_with(b"holder_error=start"));
}

/// L4: a pane the caller is spawning is not probed, but still counted.
#[test]
fn pty_holder_census_skip_is_counted_not_probed() {
    let dir = pane_dir("skip");
    let h = spawn_ready(&dir, "spawning");
    let rows = census(&dir, Duration::from_secs(3), &[pid("spawning")]).unwrap();
    assert_eq!(rows.len(), 1, "{rows:#?}");
    match &rows[0].probe {
        Probe::Unknown { reason, record } => {
            assert!(reason.contains("being spawned"), "{reason}");
            assert_eq!(record.as_ref().unwrap().holder_pid, h.pid());
        }
        other => panic!("{other:?}"),
    }
    drop(h);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The connection cap: past MAX_CONNECTIONS live connections a new client
/// gets a typed `busy` rejection, and the holder serves again once one closes.
#[test]
fn pty_holder_connection_cap_rejects_busy() {
    use qontinui_pty_holder::server::MAX_CONNECTIONS;
    let dir = pane_dir("cap");
    let h = spawn_ready(&dir, "p1");
    let pane = pid("p1");
    let mut held: Vec<_> = (0..MAX_CONNECTIONS)
        .map(|_| connect(&dir, &pane, soon()).expect("within the cap"))
        .collect();
    let mut over = connect_raw(&dir, &pane, soon()).unwrap();
    expect_rejected(over.recv_frame(soon()).unwrap(), RejectReason::Busy);
    // Healthy is not claimed while at the cap: the probe's own connection is
    // the one refused.
    assert!(!matches!(probe(&dir, &pane, soon()), Probe::Healthy { .. }));
    held.pop();
    let end = Instant::now() + Duration::from_secs(5);
    loop {
        match probe(&dir, &pane, soon()) {
            Probe::Healthy { .. } => break,
            other if Instant::now() >= end => panic!("never served again: {other:?}"),
            _ => std::thread::sleep(Duration::from_millis(20)),
        }
    }
    drop(held);
    drop(h);
    let _ = std::fs::remove_dir_all(&dir);
}

/// M2: the probe deadline bounds the whole OPERATION, not each syscall. A fake
/// holder that holds the lock, accepts, and then trickles its reply one byte
/// at a time — each byte inside a per-read timeout — must not stretch the
/// probe past its deadline.
#[cfg(unix)]
#[test]
fn pty_holder_trickling_peer_cannot_stretch_the_probe_deadline() {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixListener;

    let dir = pane_dir("trickle");
    make_private_dir(&dir);
    let pane = pid("p1");
    let _lock = match PaneLock::try_acquire(&lock_path(&dir, &pane)).unwrap() {
        TryLock::Acquired(l) => l,
        TryLock::Held => panic!("fresh"),
    };
    let listener = UnixListener::bind(dir.join("p1.sock")).unwrap();
    let step = Duration::from_millis(150);
    std::thread::spawn(move || {
        if let Ok((mut s, _)) = listener.accept() {
            let mut buf = [0u8; 64];
            let _ = s.read(&mut buf);
            // A valid hello_ack, one byte per `step`.
            let ack = qontinui_pty_holder::frame::encode_frame(
                KIND_CONTROL,
                br#"{"type":"hello_ack","version":1,"holder_build":"x","holder_pid":1,"child_pid":null}"#,
            )
            .unwrap();
            for b in ack {
                std::thread::sleep(step);
                if s.write_all(&[b]).is_err() {
                    return;
                }
            }
        }
    });

    let deadline = Duration::from_millis(500);
    let started = Instant::now();
    let verdict = probe(&dir, &pane, started + deadline);
    let took = started.elapsed();
    assert!(
        matches!(verdict, Probe::Unknown { .. }),
        "a reply that cannot arrive by the deadline is Unknown: {verdict:?}"
    );
    // Per-syscall timeouts would have taken ~90 bytes x 150 ms.
    assert!(
        took < deadline + Duration::from_millis(300),
        "probe overran its deadline: {took:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// L1: a pre-existing group/other-WRITABLE pane dir is refused, not repaired.
#[cfg(unix)]
#[test]
fn pty_holder_writable_pane_dir_is_refused() {
    use std::os::unix::fs::PermissionsExt;
    let dir = pane_dir("loose");
    make_private_dir(&dir);
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o777)).unwrap();
    let (mut h, line) = spawn_holder(&dir, "p1");
    assert!(line.starts_with("holder_error=start"), "{line}");
    assert!(line.contains("writable"), "{line}");
    assert_eq!(h.child.wait().unwrap().code(), Some(1));
    let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o777, "refused, not chmod-repaired");
    assert!(
        !dir.join("p1.lock").exists(),
        "nothing created in a refused dir"
    );

    // L2: the client refuses it too — typed, never Absent or Dead.
    std::fs::write(dir.join("p1.lock"), b"").unwrap();
    assert!(matches!(
        probe(&dir, &pid("p1"), soon()),
        Probe::Unknown { .. }
    ));
    let err = census(&dir, Duration::from_secs(1), &[]).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);
    let _ = std::fs::remove_dir_all(&dir);
}

//! In-place holder upgrade (plan 2026-09-12 Phase 8, D12a): the real
//! `qontinui-pty-holder` binary hands a live pane to a successor started from
//! its own executable, and every test here drives that through the frozen
//! `prepare_upgrade` verb.
//!
//! Unix only. A ConPTY cannot be handed to another process (D12), so on
//! Windows this suite has exactly one test, IGNORED with the reason — it
//! reports "not applicable", never "passed".

#[cfg(not(unix))]
#[test]
#[ignore = "not applicable: holder upgrade (plan Phase 8, D12a) is Unix-only — a Windows holder answers prepare_upgrade with `unsupported`"]
fn pty_holder_handoff_not_applicable_on_this_os() {}

#[cfg(unix)]
mod unix {
    use std::ffi::OsString;
    use std::io::{BufRead, BufReader};
    use std::path::{Path, PathBuf};
    use std::process::{Child, Command, Stdio};
    use std::time::{Duration, Instant};

    use qontinui_pty_holder::client::{
        connect, connect_raw, probe, AttachedStream, Event, Probe, UpgradeOutcome,
    };
    use qontinui_pty_holder::frame::KIND_CONTROL;
    use qontinui_pty_holder::handoff::ADOPT_FAULT_ENV;
    use qontinui_pty_holder::lock::read_record;
    use qontinui_pty_holder::pane::{lock_path, snapshot_path, snapshot_tmp_path, PaneId};
    use qontinui_pty_holder::proc_identity::{is_same_process, start_token};
    use qontinui_pty_holder::protocol::ExitReply;
    use qontinui_pty_holder::protocol::{parse_reply, to_payload, Reply, Request};
    use qontinui_pty_holder::spec::{write_spec, ChildSpec};

    const BIN: &str = env!("CARGO_BIN_EXE_qontinui-pty-holder");

    fn soon() -> Instant {
        Instant::now() + Duration::from_secs(10)
    }

    /// Long enough for a whole upgrade exchange (`handoff::ACK_TIMEOUT` plus
    /// the quiesce and input-drain bounds).
    fn upgrade_deadline() -> Instant {
        Instant::now() + Duration::from_secs(40)
    }

    fn pane_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .subsec_nanos();
        PathBuf::from("/tmp").join(format!("ptyh-{tag}-{}-{nanos:x}", std::process::id()))
    }

    fn p1() -> PaneId {
        PaneId::new("p1").unwrap()
    }

    fn alive(pid: u32) -> bool {
        // SAFETY: signal 0 only checks for existence.
        unsafe { libc::kill(pid as i32, 0) == 0 }
    }

    /// Kill `pid` only while it is still the process `token` was taken from —
    /// never a recycled pid. Every pid passed here is one this test started
    /// (a holder, its successor, or the pane's child).
    fn kill_verified(pid: u32, token: &Option<String>) {
        if is_same_process(pid, token.as_deref()) {
            // SAFETY: a signal to a process this test started, verified above.
            unsafe {
                libc::kill(pid as i32, libc::SIGKILL);
            }
        }
    }

    /// A pane this test started: the original holder (our child), and every
    /// other process it may leave behind, ended on drop by verified pid.
    struct Pane {
        holder: Child,
        dir: PathBuf,
        holder_pid: u32,
        child_pid: u32,
        child_token: Option<String>,
        extra: Vec<(u32, Option<String>)>,
    }

    impl Drop for Pane {
        fn drop(&mut self) {
            for (pid, token) in &self.extra {
                kill_verified(*pid, token);
            }
            kill_verified(self.child_pid, &self.child_token);
            let _ = self.holder.kill();
            let _ = self.holder.wait();
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    impl Pane {
        fn adopt_cleanup(&mut self, pid: u32) {
            self.extra.push((pid, start_token(pid)));
        }

        fn holder_exits_within(&mut self, bound: Duration) -> Option<std::process::ExitStatus> {
            let end = Instant::now() + bound;
            loop {
                if let Ok(Some(s)) = self.holder.try_wait() {
                    return Some(s);
                }
                if Instant::now() >= end {
                    return None;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }

    /// The pane's child: echoes a marker, then answers every input line with
    /// `got:<line>`, and exits with `code` on `quit`.
    fn echo_spec(code: u8) -> ChildSpec {
        let script = format!(
            "stty -echo; echo before-swap; while read line; do echo \"got:$line\"; \
             [ \"$line\" = quit ] && exit {code}; done"
        );
        let mut spec = ChildSpec::new(vec!["/bin/sh".into(), "-c".into(), script.into()]);
        spec.env = std::env::vars_os()
            .filter(|(k, _)| k != ADOPT_FAULT_ENV)
            .collect();
        spec.cwd = Some(OsString::from("/tmp"));
        spec
    }

    /// Spawn the holder (with `fault` in ITS environment, which a successor
    /// inherits) and read its ready line.
    fn start(tag: &str, spec: ChildSpec, fault: Option<&str>) -> Pane {
        let dir = pane_dir(tag);
        write_spec(&dir, &p1(), &spec).unwrap();
        let mut cmd = Command::new(BIN);
        cmd.arg("--pane-dir")
            .arg(&dir)
            .arg("--pane-id")
            .arg("p1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        match fault {
            Some(f) => cmd.env(ADOPT_FAULT_ENV, f),
            None => cmd.env_remove(ADOPT_FAULT_ENV),
        };
        let mut holder = cmd.spawn().unwrap();
        let out = holder.stdout.take().unwrap();
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
        let field = |k: &str| -> u32 {
            line.split_whitespace()
                .find_map(|t| t.strip_prefix(k))
                .unwrap()
                .parse()
                .unwrap()
        };
        let child_pid = field("child_pid=");
        Pane {
            holder_pid: holder.id(),
            holder,
            dir,
            child_pid,
            child_token: start_token(child_pid),
            extra: Vec::new(),
        }
    }

    fn attach(dir: &Path, from: Option<u64>) -> AttachedStream {
        connect(dir, &p1(), soon())
            .expect("handshake")
            .attach(from, soon())
            .expect("attach")
    }

    fn send_input(dir: &Path, bytes: &[u8]) {
        let mut input = connect(dir, &p1(), soon())
            .expect("handshake")
            .open_input(soon())
            .expect("open_input");
        input.input(bytes, soon()).expect("input");
    }

    fn count(hay: &[u8], needle: &[u8]) -> usize {
        hay.windows(needle.len()).filter(|w| *w == needle).count()
    }

    /// What one stream yielded: contiguous bytes from `start`, and whether a
    /// loss was ever reported.
    struct Read {
        bytes: Vec<u8>,
        next: u64,
        lost: bool,
        exit: Option<ExitReply>,
        ended: bool,
    }

    /// Read until `done` holds, the stream ends, or `bound` passes. Output
    /// must be CONTIGUOUS from `next`: a gap or an overlap fails the test.
    fn read_until(
        s: &mut AttachedStream,
        mut next: u64,
        bound: Duration,
        done: impl Fn(&Read) -> bool,
    ) -> Read {
        let end = Instant::now() + bound;
        let mut r = Read {
            bytes: Vec::new(),
            next,
            lost: false,
            exit: None,
            ended: false,
        };
        while !done(&r) && Instant::now() < end {
            match s.reader.next_event(Some(end)) {
                Ok(Some(Event::Output { offset, bytes })) => {
                    assert_eq!(
                        offset, next,
                        "output must be contiguous — no gap, no duplicate"
                    );
                    next += bytes.len() as u64;
                    r.bytes.extend_from_slice(&bytes);
                    r.next = next;
                }
                Ok(Some(Event::Lost { .. })) => r.lost = true,
                Ok(Some(Event::Exit(e))) => {
                    r.exit = Some(e);
                    r.ended = true;
                    break;
                }
                Ok(Some(_)) => {}
                Ok(None) | Err(_) => {
                    r.ended = true;
                    break;
                }
            }
        }
        r
    }

    /// Wait until the pane's endpoint answers as `holder_pid`.
    fn served_by(dir: &Path, holder_pid: u32) -> qontinui_pty_holder::protocol::HelloAck {
        let end = Instant::now() + Duration::from_secs(15);
        loop {
            if let Probe::Healthy { hello_ack } = probe(dir, &p1(), soon()) {
                if hello_ack.holder_pid == holder_pid {
                    return hello_ack;
                }
            }
            assert!(
                Instant::now() < end,
                "pane not served by pid {holder_pid} within 15 s"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn no_snapshot_left(dir: &Path) {
        assert!(!snapshot_path(dir, &p1()).exists(), "snapshot left behind");
        assert!(
            !snapshot_tmp_path(dir, &p1()).exists(),
            "snapshot temp left behind"
        );
    }

    /// The successor's pid, from the line a `stall`-faulted successor writes to
    /// the pane's log (`<ms> pid=<n> adopt: test fault: stall`).
    fn stalled_successor_pid(dir: &Path) -> u32 {
        let log = dir.join("p1.log");
        let end = Instant::now() + Duration::from_secs(15);
        loop {
            if let Ok(text) = std::fs::read_to_string(&log) {
                if let Some(pid) = text
                    .lines()
                    .filter(|l| l.contains("adopt: test fault: stall"))
                    .find_map(|l| {
                        l.split_whitespace()
                            .find_map(|t| t.strip_prefix("pid="))
                            .and_then(|p| p.parse().ok())
                    })
                {
                    return pid;
                }
            }
            assert!(
                Instant::now() < end,
                "no stalled successor logged within 15 s"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// The swap itself: same child pid before and after, input still reaches
    /// the child, a client's dropped connection reattaches at its own offset
    /// with nothing lost or repeated, and a LATE attach from offset 0 replays
    /// bytes from both sides of the swap with no loss marker and no duplicate.
    #[test]
    fn pty_holder_handoff_swaps_holders_under_a_live_child() {
        let mut pane = start("swap", echo_spec(7), None);
        let dir = pane.dir.clone();
        let before = served_by(&dir, pane.holder_pid);
        assert_eq!(before.child_pid, Some(pane.child_pid));

        let mut live = attach(&dir, None);
        assert_eq!(
            live.info.start_offset, 0,
            "a short stream is attached from its start"
        );
        let r = read_until(&mut live, 0, Duration::from_secs(10), |r| {
            count(&r.bytes, b"before-swap") > 0
        });
        send_input(&dir, b"one\n");
        let r2 = read_until(&mut live, r.next, Duration::from_secs(10), |r| {
            count(&r.bytes, b"got:one") > 0
        });

        let outcome = connect(&dir, &p1(), soon())
            .expect("handshake")
            .prepare_upgrade(upgrade_deadline())
            .expect("prepare_upgrade answered");
        let UpgradeOutcome::Upgraded {
            successor_pid,
            handoff_offset,
        } = outcome
        else {
            panic!("expected upgraded, got {outcome:?}");
        };
        pane.adopt_cleanup(successor_pid);
        assert_ne!(successor_pid, pane.holder_pid);
        assert!(
            handoff_offset >= r2.next,
            "X covers everything already streamed"
        );

        // The predecessor leaves, and closes its connections on the way.
        let status = pane
            .holder_exits_within(Duration::from_secs(10))
            .expect("the predecessor exits after the handoff");
        assert!(
            status.success(),
            "the predecessor exits cleanly: {status:?}"
        );
        let tail = read_until(&mut live, r2.next, Duration::from_secs(10), |_| false);
        assert!(tail.ended, "the predecessor's connection ends");
        assert!(!tail.lost);

        // Same child, new holder, and the lock names the successor.
        let after = served_by(&dir, successor_pid);
        assert_eq!(
            after.child_pid,
            Some(pane.child_pid),
            "child pid unchanged across the swap"
        );
        assert!(
            is_same_process(pane.child_pid, pane.child_token.as_deref()),
            "the very same child process"
        );
        let record = read_record(&lock_path(&dir, &p1())).expect("a lock record");
        assert_eq!(record.holder_pid, successor_pid);
        assert_eq!(record.child_pid, Some(pane.child_pid));
        no_snapshot_left(&dir);

        // A client's reattach at its own last offset: contiguous, no loss.
        let mut again = attach(&dir, Some(tail.next));
        assert_eq!(again.info.start_offset, tail.next);
        send_input(&dir, b"two\n");
        let r3 = read_until(&mut again, tail.next, Duration::from_secs(10), |r| {
            count(&r.bytes, b"got:two") > 0
        });
        assert!(!r3.lost, "no loss marker across the swap");

        // A LATE attach from the very start: both sides of the swap, each
        // line exactly once, contiguous, no loss marker.
        let mut late = attach(&dir, Some(0));
        let all = read_until(&mut late, 0, Duration::from_secs(10), |r| {
            count(&r.bytes, b"got:two") > 0
        });
        assert!(!all.lost, "no loss marker in the late replay");
        for line in [&b"before-swap"[..], b"got:one", b"got:two"] {
            assert_eq!(
                count(&all.bytes, line),
                1,
                "{:?} exactly once",
                String::from_utf8_lossy(line)
            );
        }
        let pos = |n: &[u8]| all.bytes.windows(n.len()).position(|w| w == n).unwrap();
        assert!(pos(b"before-swap") < pos(b"got:one") && pos(b"got:one") < pos(b"got:two"));
        let mut seen = r.bytes.clone();
        seen.extend_from_slice(&r2.bytes);
        seen.extend_from_slice(&tail.bytes);
        seen.extend_from_slice(&r3.bytes);
        // The live connections saw the stream from offset 0 too (a fresh
        // attach of a short stream starts there): the late replay is the
        // same bytes, byte for byte, across the swap.
        let n = seen.len().min(all.bytes.len());
        assert_eq!(
            &all.bytes[..n],
            &seen[..n],
            "the late replay equals what was streamed live"
        );

        // End the pane through the successor: its exit is reported, as UNKNOWN.
        send_input(&dir, b"quit\n");
        let fin = read_until(&mut again, r3.next, Duration::from_secs(10), |_| false);
        assert_eq!(
            fin.exit,
            Some(ExitReply {
                code: None,
                signal: None
            })
        );
    }

    /// D12a rule 3: an adopted pane's exit code is UNKNOWN — reported as
    /// `code: None`, never as 0, even when the child did exit 0.
    #[test]
    fn pty_holder_handoff_adopted_pane_exit_code_is_unknown() {
        let mut pane = start("exit", echo_spec(0), None);
        let dir = pane.dir.clone();
        served_by(&dir, pane.holder_pid);
        let outcome = connect(&dir, &p1(), soon())
            .unwrap()
            .prepare_upgrade(upgrade_deadline())
            .unwrap();
        let UpgradeOutcome::Upgraded { successor_pid, .. } = outcome else {
            panic!("expected upgraded, got {outcome:?}");
        };
        pane.adopt_cleanup(successor_pid);
        served_by(&dir, successor_pid);
        // An ADOPTED pane can itself be handed on: a second generation.
        let outcome = connect(&dir, &p1(), soon())
            .unwrap()
            .prepare_upgrade(upgrade_deadline())
            .unwrap();
        let UpgradeOutcome::Upgraded {
            successor_pid: second,
            ..
        } = outcome
        else {
            panic!("expected a second upgrade, got {outcome:?}");
        };
        pane.adopt_cleanup(second);
        let hello = served_by(&dir, second);
        assert_eq!(hello.child_pid, Some(pane.child_pid));
        let end = Instant::now() + Duration::from_secs(10);
        while is_same_process(successor_pid, pane.extra[0].1.as_deref()) {
            assert!(Instant::now() < end, "the first successor did not leave");
            std::thread::sleep(Duration::from_millis(20));
        }
        let successor_pid = second;
        let mut s = attach(&dir, None);
        send_input(&dir, b"quit\n");
        let from = s.info.start_offset;
        let r = read_until(&mut s, from, Duration::from_secs(10), |_| false);
        let exit = r.exit.expect("an exit frame");
        assert_eq!(
            exit.code, None,
            "an adopted pane never reports a code — not 0"
        );
        assert_eq!(exit.signal, None);
        assert!(count(&r.bytes, b"got:quit") == 1);
        // The successor then leaves (its exit was delivered).
        let end = Instant::now() + Duration::from_secs(10);
        while alive(successor_pid) && is_same_process(successor_pid, pane.extra[1].1.as_deref()) {
            assert!(
                Instant::now() < end,
                "the successor did not exit after delivering the exit"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// D12a rule 2: SIGKILL the successor BEFORE its ack — the predecessor
    /// rolls back and still serves the same child, input and output intact.
    #[test]
    fn pty_holder_handoff_successor_killed_before_ack_rolls_back() {
        let mut pane = start("kill", echo_spec(7), Some("stall"));
        let dir = pane.dir.clone();
        served_by(&dir, pane.holder_pid);
        let mut live = attach(&dir, None);
        let from = live.info.start_offset;
        let r = read_until(&mut live, from, Duration::from_secs(10), |r| {
            count(&r.bytes, b"before-swap") > 0
        });

        let req_dir = dir.clone();
        let request = std::thread::spawn(move || {
            connect(&req_dir, &p1(), soon())
                .unwrap()
                .prepare_upgrade(upgrade_deadline())
        });
        let successor = stalled_successor_pid(&dir);
        pane.adopt_cleanup(successor);
        // Input sent while the upgrade is in flight is HELD, then delivered
        // by the predecessor once it rolls back.
        send_input(&dir, b"during\n");
        assert_ne!(successor, pane.holder_pid);
        kill_verified(successor, &pane.extra[0].1);
        let outcome = request.join().unwrap().expect("prepare_upgrade answered");
        assert!(
            matches!(&outcome, UpgradeOutcome::Failed(why) if why.contains("without an ack")),
            "{outcome:?}"
        );

        assert!(alive(pane.holder_pid), "the predecessor still runs");
        let hello = served_by(&dir, pane.holder_pid);
        assert_eq!(hello.child_pid, Some(pane.child_pid), "same child pid");
        assert!(is_same_process(pane.child_pid, pane.child_token.as_deref()));
        no_snapshot_left(&dir);

        send_input(&dir, b"after\n");
        let r2 = read_until(&mut live, r.next, Duration::from_secs(10), |r| {
            count(&r.bytes, b"got:after") > 0
        });
        assert!(!r2.lost && !r2.ended, "the live connection kept streaming");
        assert_eq!(
            count(&r2.bytes, b"got:during"),
            1,
            "held input delivered once"
        );
        assert!(
            r2.bytes
                .windows(10)
                .position(|w| w == b"got:during")
                .unwrap()
                < r2.bytes.windows(9).position(|w| w == b"got:after").unwrap()
        );
        let record = read_record(&lock_path(&dir, &p1())).unwrap();
        assert_eq!(record.holder_pid, pane.holder_pid);
    }

    /// A successor that NAKs: rolled back, and no snapshot file is left.
    #[test]
    fn pty_holder_handoff_nak_leaves_no_snapshot() {
        let mut pane = start("nak", echo_spec(7), Some("nak"));
        let dir = pane.dir.clone();
        served_by(&dir, pane.holder_pid);
        let outcome = connect(&dir, &p1(), soon())
            .unwrap()
            .prepare_upgrade(upgrade_deadline())
            .unwrap();
        assert!(
            matches!(&outcome, UpgradeOutcome::Failed(why) if why.contains("test fault: nak")),
            "{outcome:?}"
        );
        no_snapshot_left(&dir);
        assert!(alive(pane.holder_pid));
        assert!(pane.holder.try_wait().unwrap().is_none());
        let mut s = attach(&dir, None);
        send_input(&dir, b"still\n");
        let from = s.info.start_offset;
        let r = read_until(&mut s, from, Duration::from_secs(10), |r| {
            count(&r.bytes, b"got:still") > 0
        });
        assert_eq!(count(&r.bytes, b"got:still"), 1);
        assert!(!r.lost);
    }

    /// D15: `prepare_upgrade` is a FROZEN envelope verb, so a runner that
    /// speaks none of this holder's versions can still replace it.
    #[test]
    fn pty_holder_handoff_works_after_no_common_version() {
        let mut pane = start("ncv", echo_spec(7), None);
        let dir = pane.dir.clone();
        served_by(&dir, pane.holder_pid);
        let mut raw = connect_raw(&dir, &p1(), soon()).unwrap();
        let send = |raw: &mut qontinui_pty_holder::client::RawConn, req: &Request| {
            raw.send_frame(KIND_CONTROL, &to_payload(req).unwrap(), soon())
                .unwrap()
        };
        send(
            &mut raw,
            &Request::Hello {
                versions: vec![999],
            },
        );
        let f = raw.recv_frame(soon()).unwrap().unwrap();
        assert!(matches!(
            parse_reply(&f.payload).unwrap(),
            Reply::NoCommonVersion { .. }
        ));
        send(&mut raw, &Request::PrepareUpgrade);
        let f = raw.recv_frame(upgrade_deadline()).unwrap().unwrap();
        let Reply::Upgraded { successor_pid, .. } = parse_reply(&f.payload).unwrap() else {
            panic!("expected upgraded");
        };
        pane.adopt_cleanup(successor_pid);
        let hello = served_by(&dir, successor_pid);
        assert_eq!(hello.child_pid, Some(pane.child_pid));
    }
}

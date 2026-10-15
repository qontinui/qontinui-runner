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
    use qontinui_pty_holder::handoff::{
        upgrade_loss_notice, ACK_TIMEOUT_ENV, ADOPT_FAULT_ENV, COMMIT_DELAY_ENV,
        SERVING_POLL_DELAY_ENV,
    };
    use qontinui_pty_holder::lock::read_record;
    use qontinui_pty_holder::pane::{lock_path, snapshot_path, snapshot_tmp_path, PaneId};
    use qontinui_pty_holder::proc_identity::{is_same_process, start_token};
    use qontinui_pty_holder::protocol::ExitReply;
    use qontinui_pty_holder::protocol::{parse_reply, to_payload, Reply, Request};
    use qontinui_pty_holder::scrollback::read_scrollback;
    use qontinui_pty_holder::spec::{write_spec, ChildSpec};

    // The debug-built holder: the post-commit test faults some tests use
    // exist in debug builds only (`handoff::POST_COMMIT_FAULTS`).
    const BIN: &str = env!("CARGO_BIN_EXE_qontinui-pty-holder");

    fn soon() -> Instant {
        Instant::now() + Duration::from_secs(10)
    }

    /// Long enough for a whole upgrade exchange (`handoff::ACK_TIMEOUT` plus
    /// the quiesce and input-drain bounds).
    fn upgrade_deadline() -> Instant {
        Instant::now() + Duration::from_secs(60)
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

    /// A test-seam variable, kept out of every CHILD's environment.
    fn is_seam(k: &std::ffi::OsStr) -> bool {
        [
            ADOPT_FAULT_ENV,
            ACK_TIMEOUT_ENV,
            COMMIT_DELAY_ENV,
            SERVING_POLL_DELAY_ENV,
        ]
        .iter()
        .any(|s| k == *s)
    }

    /// [`echo_spec`] that ALSO appends every input line to `file`, so a test
    /// can count what the child received whatever happened to its output.
    fn echo_and_log_spec(file: &Path) -> ChildSpec {
        let script = format!(
            "stty -echo; echo before-swap; while read line; do echo \"got:$line\"; \
             echo \"$line\" >> '{}'; done",
            file.display()
        );
        let mut spec = ChildSpec::new(vec!["/bin/sh".into(), "-c".into(), script.into()]);
        spec.env = std::env::vars_os().filter(|(k, _)| !is_seam(k)).collect();
        spec.cwd = Some(OsString::from("/tmp"));
        spec
    }

    /// The pane's child: echoes a marker, then answers every input line with
    /// `got:<line>`, and exits with `code` on `quit`.
    fn echo_spec(code: u8) -> ChildSpec {
        let script = format!(
            "stty -echo; echo before-swap; while read line; do echo \"got:$line\"; \
             [ \"$line\" = quit ] && exit {code}; done"
        );
        let mut spec = ChildSpec::new(vec!["/bin/sh".into(), "-c".into(), script.into()]);
        spec.env = std::env::vars_os().filter(|(k, _)| !is_seam(k)).collect();
        spec.cwd = Some(OsString::from("/tmp"));
        spec
    }

    /// Spawn the holder (with `fault` in ITS environment, which a successor
    /// inherits) and read its ready line.
    fn start(tag: &str, spec: ChildSpec, fault: Option<&str>) -> Pane {
        start_with(tag, spec, fault, &[])
    }

    /// [`start`] with further test-seam variables in the holder's environment.
    fn start_with(tag: &str, spec: ChildSpec, fault: Option<&str>, seams: &[(&str, &str)]) -> Pane {
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
        cmd.env_remove(ACK_TIMEOUT_ENV)
            .env_remove(COMMIT_DELAY_ENV)
            .env_remove(SERVING_POLL_DELAY_ENV);
        for (k, v) in seams {
            cmd.env(k, v);
        }
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

    /// The pid on the first line of the pane's log that contains `needle`
    /// (`<ms> pid=<n> <message>`).
    fn logged_pid(dir: &Path, needle: &str) -> u32 {
        let log = dir.join("p1.log");
        let end = Instant::now() + Duration::from_secs(20);
        loop {
            if let Ok(text) = std::fs::read_to_string(&log) {
                if let Some(pid) = text.lines().filter(|l| l.contains(needle)).find_map(|l| {
                    l.split_whitespace()
                        .find_map(|t| t.strip_prefix("pid="))
                        .and_then(|p| p.parse().ok())
                }) {
                    return pid;
                }
            }
            assert!(Instant::now() < end, "no {needle:?} logged within 20 s");
            std::thread::sleep(Duration::from_millis(20));
        }
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
        // Only one attached connection remains: a holder leaves once its exit
        // reached ANY client, so a second one (`late`) could take the exit
        // frame and the holder's departure would end `again` first.
        drop(late);
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

    /// Plan D7 across Phase 8: the durable tail outlives the swap. The
    /// predecessor persists everything up to X and stops writing before the
    /// commit; the successor rewrites the tail from its restored ring at the
    /// same absolute offsets, so it holds both sides of the swap, once each.
    #[test]
    fn pty_holder_handoff_scrollback_tail_spans_the_swap() {
        let mut pane = start("tail", echo_spec(7), None);
        let dir = pane.dir.clone();
        let mut live = attach(&dir, None);
        let r = read_until(&mut live, 0, Duration::from_secs(10), |r| {
            count(&r.bytes, b"before-swap") > 0
        });
        let outcome = connect(&dir, &p1(), soon())
            .expect("handshake")
            .prepare_upgrade(upgrade_deadline())
            .expect("prepare_upgrade answered");
        let UpgradeOutcome::Upgraded { successor_pid, .. } = outcome else {
            panic!("expected upgraded, got {outcome:?}");
        };
        pane.adopt_cleanup(successor_pid);
        pane.holder_exits_within(Duration::from_secs(10))
            .expect("the predecessor exits after the handoff");
        drop(live);
        send_input(&dir, b"two\n");
        let mut again = attach(&dir, Some(r.next));
        read_until(&mut again, r.next, Duration::from_secs(10), |r| {
            count(&r.bytes, b"got:two") > 0
        });
        // The successor's timer writes within SCROLLBACK_INTERVAL (5 s).
        let end = Instant::now() + Duration::from_secs(20);
        let tail = loop {
            let t = read_scrollback(&dir, &p1()).expect("a whole, readable tail");
            if t.as_ref().is_some_and(|t| count(&t.bytes, b"got:two") > 0) || Instant::now() >= end
            {
                break t.expect("a tail on disk");
            }
            std::thread::sleep(Duration::from_millis(200));
        };
        assert_eq!(tail.child_pid, pane.child_pid, "the same child's stream");
        assert_eq!(tail.start_offset, 0, "a short stream's tail starts at 0");
        assert_eq!(tail.discarded_bytes, 0);
        for line in [&b"before-swap"[..], b"got:two"] {
            assert_eq!(
                count(&tail.bytes, line),
                1,
                "{:?} exactly once in the tail",
                String::from_utf8_lossy(line)
            );
        }
        drop(again);
        send_input(&dir, b"quit\n");
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

    /// Review r1, findings 1 and 3: a predecessor starved for LONGER than the
    /// successor's old 15 s commit timeout still hands the pane over (the
    /// successor waits as long as its predecessor lives) — and input written
    /// while the swap is in flight is held and reaches the child through the
    /// commit, exactly once.
    #[test]
    fn pty_holder_handoff_slow_commit_and_held_input_survive_a_committed_swap() {
        let mut pane = start_with("slow", echo_spec(7), None, &[(COMMIT_DELAY_ENV, "16000")]);
        let dir = pane.dir.clone();
        served_by(&dir, pane.holder_pid);
        let req_dir = dir.clone();
        let request = std::thread::spawn(move || {
            connect(&req_dir, &p1(), soon())
                .unwrap()
                .prepare_upgrade(upgrade_deadline())
        });
        let successor = logged_pid(&dir, "adopt: acked");
        pane.adopt_cleanup(successor);
        // Held: the predecessor is frozen between ack and commit.
        send_input(&dir, b"held-one\n");
        // Then an input connection that stays OPEN across the swap (the
        // newest one: an older input connection is superseded by design).
        // The commit's drain must read what it carries and end it.
        let mut kept = connect(&dir, &p1(), soon())
            .unwrap()
            .open_input(soon())
            .unwrap();
        kept.input(b"held-kept\n", soon())
            .expect("written while frozen");
        kept.input(b"held-two\n", soon())
            .expect("written while frozen");
        let outcome = request.join().unwrap().expect("prepare_upgrade answered");
        assert!(
            matches!(outcome, UpgradeOutcome::Upgraded { successor_pid, .. } if successor_pid == successor),
            "{outcome:?}"
        );
        let hello = served_by(&dir, successor);
        assert_eq!(hello.child_pid, Some(pane.child_pid));
        let mut s = attach(&dir, Some(0));
        let r = read_until(&mut s, 0, Duration::from_secs(10), |r| {
            count(&r.bytes, b"got:held-two") > 0
        });
        assert_eq!(
            count(&r.bytes, b"got:held-one"),
            1,
            "held input delivered once"
        );
        assert_eq!(
            count(&r.bytes, b"got:held-kept"),
            1,
            "the open connection was drained"
        );
        assert_eq!(count(&r.bytes, b"got:held-two"), 1);
        let pos = |n: &[u8]| r.bytes.windows(n.len()).position(|w| w == n).unwrap();
        assert!(pos(b"got:held-one") < pos(b"got:held-kept"));
        assert!(pos(b"got:held-kept") < pos(b"got:held-two"));
        assert!(!r.lost);
        // A write on the drained connection now FAILS — never vanishes.
        let late = (0..20).try_for_each(|_| kept.input(b"late\n", soon()));
        assert!(
            late.is_err(),
            "a write after the drain must fail at the writer"
        );
    }

    /// Review r1, finding 6: SIGKILL the PREDECESSOR after the successor's ack
    /// and before its commit — the successor takes the pane over, same child.
    #[test]
    fn pty_holder_handoff_predecessor_killed_after_ack_successor_takes_over() {
        let mut pane = start_with("pkill", echo_spec(7), None, &[(COMMIT_DELAY_ENV, "30000")]);
        let dir = pane.dir.clone();
        served_by(&dir, pane.holder_pid);
        let req_dir = dir.clone();
        let request = std::thread::spawn(move || {
            connect(&req_dir, &p1(), soon())
                .unwrap()
                .prepare_upgrade(upgrade_deadline())
        });
        let successor = logged_pid(&dir, "adopt: acked");
        pane.adopt_cleanup(successor);
        // The predecessor is this test's own child: kill it exactly.
        pane.holder.kill().unwrap();
        let _ = pane.holder.wait();
        assert!(
            request.join().unwrap().is_err(),
            "the requester's holder died"
        );
        let hello = served_by(&dir, successor);
        assert_eq!(hello.child_pid, Some(pane.child_pid), "same child pid");
        assert!(is_same_process(pane.child_pid, pane.child_token.as_deref()));
        let record = read_record(&lock_path(&dir, &p1())).expect("a lock record");
        assert_eq!(record.holder_pid, successor);
        let mut s = attach(&dir, Some(0));
        send_input(&dir, b"alive\n");
        let r = read_until(&mut s, 0, Duration::from_secs(10), |r| {
            count(&r.bytes, b"got:alive") > 0
        });
        assert_eq!(count(&r.bytes, b"before-swap"), 1);
        assert!(!r.lost);
        no_snapshot_left(&dir);
    }

    /// Review r1, finding 6: the ack TIMEOUT itself — a successor that never
    /// answers is killed when the (shortened) timeout runs out, and the
    /// predecessor keeps serving.
    #[test]
    fn pty_holder_handoff_ack_timeout_rolls_back() {
        let mut pane = start_with(
            "ackto",
            echo_spec(7),
            Some("stall"),
            &[(ACK_TIMEOUT_ENV, "1500")],
        );
        let dir = pane.dir.clone();
        served_by(&dir, pane.holder_pid);
        let started = Instant::now();
        let outcome = connect(&dir, &p1(), soon())
            .unwrap()
            .prepare_upgrade(upgrade_deadline())
            .unwrap();
        let successor = stalled_successor_pid(&dir);
        pane.adopt_cleanup(successor);
        assert!(
            matches!(&outcome, UpgradeOutcome::Failed(why) if why.contains("waiting for the ack")),
            "{outcome:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the shortened timeout applied"
        );
        assert!(
            !is_same_process(successor, pane.extra[0].1.as_deref()),
            "the silent successor was killed"
        );
        let hello = served_by(&dir, pane.holder_pid);
        assert_eq!(hello.child_pid, Some(pane.child_pid));
        no_snapshot_left(&dir);
        assert!(
            !dir.join("p1.sock.upg").exists(),
            "no staged socket left behind"
        );
        let mut s = attach(&dir, None);
        send_input(&dir, b"still\n");
        let from = s.info.start_offset;
        let r = read_until(&mut s, from, Duration::from_secs(10), |r| {
            count(&r.bytes, b"got:still") > 0
        });
        assert_eq!(count(&r.bytes, b"got:still"), 1);
    }

    /// Review r1, finding 6: a child that writes CONTINUOUSLY across the swap
    /// (so output sits in the kernel while reads are quiesced): a late replay
    /// from offset 0 is a gap-free, duplicate-free counter on both sides of X.
    #[test]
    fn pty_holder_handoff_continuous_output_is_contiguous_across_the_swap() {
        let mut spec = ChildSpec::new(vec![
            "/bin/sh".into(),
            "-c".into(),
            "i=0; while :; do echo \"n=$i\"; i=$((i+1)); sleep 0.002; done".into(),
        ]);
        spec.env = std::env::vars_os().filter(|(k, _)| !is_seam(k)).collect();
        spec.cwd = Some(OsString::from("/tmp"));
        let mut pane = start("count", spec, None);
        let dir = pane.dir.clone();
        served_by(&dir, pane.holder_pid);
        std::thread::sleep(Duration::from_millis(300));
        let outcome = connect(&dir, &p1(), soon())
            .unwrap()
            .prepare_upgrade(upgrade_deadline())
            .unwrap();
        let UpgradeOutcome::Upgraded {
            successor_pid,
            handoff_offset,
        } = outcome
        else {
            panic!("expected upgraded, got {outcome:?}");
        };
        pane.adopt_cleanup(successor_pid);
        assert!(
            handoff_offset > 0,
            "the counter had produced output before X"
        );
        served_by(&dir, successor_pid);
        let mut late = attach(&dir, Some(0));
        let all = read_until(&mut late, 0, Duration::from_secs(15), |r| {
            r.next > handoff_offset + 4096
        });
        assert!(!all.lost, "no loss marker");
        assert!(
            all.next > handoff_offset + 4096,
            "output continued after the swap"
        );
        let mut expect = 0u64;
        let mut lines = all.bytes.split(|b| *b == b'\n').peekable();
        while let Some(line) = lines.next() {
            if lines.peek().is_none() {
                break; // the last, possibly partial, line
            }
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            let n: u64 = std::str::from_utf8(line.strip_prefix(b"n=").expect("a counter line"))
                .unwrap()
                .parse()
                .unwrap();
            assert_eq!(n, expect, "counter gap or duplicate at line {expect}");
            expect += 1;
        }
        assert!(expect > 100, "enough lines to span the swap: {expect}");
    }

    /// Review r2, finding D: a successor that dies AFTER the commit but
    /// before `serving` — at either point: before it touched the lock record,
    /// or after its endpoint rename — leaves the pane with the predecessor,
    /// which takes it back: same child, the lock and the endpoint name it
    /// again, the input held across the swap reaches the child exactly once,
    /// and the stream continues with no loss marker.
    #[test]
    fn pty_holder_handoff_successor_dying_after_commit_gives_the_pane_back() {
        for fault in ["exit-after-commit", "exit-after-publish"] {
            let mut pane = start_with(
                fault.strip_prefix("exit-after-").unwrap_or(fault),
                echo_spec(7),
                Some(fault),
                &[(COMMIT_DELAY_ENV, "1500")],
            );
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
            let successor = logged_pid(&dir, "adopt: acked");
            pane.adopt_cleanup(successor);
            send_input(&dir, b"held\n");
            let outcome = request.join().unwrap().expect("prepare_upgrade answered");
            assert!(
                matches!(&outcome, UpgradeOutcome::Failed(why) if why.contains("took its pane back")),
                "{fault}: {outcome:?}"
            );
            assert!(
                pane.holder.try_wait().unwrap().is_none(),
                "{fault}: the predecessor still runs"
            );
            let hello = served_by(&dir, pane.holder_pid);
            assert_eq!(hello.child_pid, Some(pane.child_pid), "{fault}: same child");
            let record = read_record(&lock_path(&dir, &p1())).expect("a lock record");
            assert_eq!(
                record.holder_pid, pane.holder_pid,
                "{fault}: the lock names it again"
            );
            send_input(&dir, b"after\n");
            let r2 = read_until(&mut live, r.next, Duration::from_secs(10), |r| {
                count(&r.bytes, b"got:after") > 0
            });
            assert!(!r2.lost, "{fault}: no loss marker");
            assert_eq!(count(&r2.bytes, b"got:held"), 1, "{fault}: held input once");
            no_snapshot_left(&dir);
            assert!(
                !dir.join("p1.sock.upg").exists(),
                "{fault}: no staged socket left"
            );
            // The upgrade is not stuck "in progress": a second request runs
            // the whole exchange again (and meets the same fault).
            let again = connect(&dir, &p1(), soon())
                .unwrap()
                .prepare_upgrade(upgrade_deadline())
                .unwrap();
            assert!(
                matches!(&again, UpgradeOutcome::Failed(why) if why.contains("took its pane back")),
                "{fault}: {again:?}"
            );
            if let Ok(pid) = std::fs::read_to_string(dir.join("p1.log")) {
                for l in pid.lines().filter(|l| l.contains("adopt: acked")) {
                    if let Some(p) = l
                        .split_whitespace()
                        .find_map(|t| t.strip_prefix("pid="))
                        .and_then(|p| p.parse().ok())
                    {
                        pane.adopt_cleanup(p);
                    }
                }
            }
        }
    }

    /// Review r3, G: the successor reports `serving` — it has taken over: the
    /// held input is written, output past X read — and dies before the
    /// predecessor notices either. The predecessor takes the pane back (the
    /// child must keep a holder), does NOT replay the held input, and puts an
    /// explicit loss notice into the stream at the resume point.
    #[test]
    fn pty_holder_handoff_successor_dying_after_serving_declares_the_loss() {
        let out = pane_dir("srvout");
        std::fs::create_dir_all(&out).unwrap();
        let file = out.join("lines");
        let mut pane = start_with(
            "srvdie",
            echo_and_log_spec(&file),
            Some("exit-after-serving"),
            &[(COMMIT_DELAY_ENV, "1500"), (SERVING_POLL_DELAY_ENV, "2500")],
        );
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
        let successor = logged_pid(&dir, "adopt: acked");
        pane.adopt_cleanup(successor);
        send_input(&dir, b"held\n");
        let outcome = request.join().unwrap().expect("prepare_upgrade answered");
        let UpgradeOutcome::Failed(why) = &outcome else {
            panic!("expected a declared-loss take-back, got {outcome:?}");
        };
        assert!(why.contains("AFTER reporting `serving`"), "{why}");
        let log = std::fs::read_to_string(dir.join("p1.log")).unwrap();
        assert!(
            log.contains("adopt: test fault: exit-after-serving"),
            "{log}"
        );
        assert!(
            log.contains("upgrade: ERROR: successor"),
            "logged at error: {log}"
        );
        assert!(
            pane.holder.try_wait().unwrap().is_none(),
            "the predecessor still runs"
        );
        let hello = served_by(&dir, pane.holder_pid);
        assert_eq!(hello.child_pid, Some(pane.child_pid), "same child");
        assert!(is_same_process(pane.child_pid, pane.child_token.as_deref()));
        let record = read_record(&lock_path(&dir, &p1())).expect("a lock record");
        assert_eq!(record.holder_pid, pane.holder_pid);
        send_input(&dir, b"after\n");
        let r2 = read_until(&mut live, r.next, Duration::from_secs(10), |r| {
            count(&r.bytes, b"got:after") > 0
        });
        let marker: &[u8] = b"holder upgrade: the successor holder died after taking over";
        let n0 = upgrade_loss_notice(0);
        assert!(count(&n0, marker) == 1);
        // CAN first: a sequence cut at X is aborted before the text.
        assert_eq!(n0.first(), Some(&0x18));
        let at = r2
            .bytes
            .windows(marker.len())
            .position(|w| w == marker)
            .unwrap();
        assert!(
            r2.bytes[..at].ends_with(b"\x18\r\n[qontinui pty holder] "),
            "the notice in the stream starts with CAN"
        );
        assert_eq!(
            count(&r2.bytes, marker),
            1,
            "the loss is declared in the stream: {:?}",
            String::from_utf8_lossy(&r2.bytes)
        );
        // The held input reached the child exactly once — written by the
        // successor, never replayed by the predecessor.
        let got = std::fs::read_to_string(&file).unwrap_or_default();
        assert_eq!(got.matches("held").count(), 1, "held input once: {got:?}");
        assert_eq!(got.matches("after").count(), 1, "{got:?}");
        let _ = std::fs::remove_dir_all(&out);
        no_snapshot_left(&dir);
        assert!(!dir.join("p1.sock.upg").exists());
    }

    /// The first event on a fresh attach, skipping nothing: it must be the
    /// loss report when one is owed.
    fn first_event(s: &mut AttachedStream) -> Event {
        s.reader
            .next_event(Some(soon()))
            .expect("an event")
            .expect("the stream is open")
    }

    /// H1 (coord finding f4672ed2): after a declared-loss take-back at X, a
    /// client whose offset is past X — one that had attached to the dead
    /// successor and read ITS output — reattaches to the predecessor with an
    /// EXPLICIT `output_lost` from X and is never replayed the predecessor's
    /// own bytes at its misaligned offset. An attach at X itself still
    /// replays from X (the notice first), with no loss.
    #[test]
    fn pty_holder_handoff_attach_past_a_declared_loss_is_told_the_loss() {
        let mut pane = start_with(
            "h1",
            echo_spec(0),
            Some("exit-after-serving"),
            &[(COMMIT_DELAY_ENV, "1500"), (SERVING_POLL_DELAY_ENV, "2500")],
        );
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
        let successor = logged_pid(&dir, "adopt: acked");
        pane.adopt_cleanup(successor);
        // Held across the swap: the successor writes it and reads its echo,
        // so its stream really does run past X before it dies.
        send_input(&dir, b"held\n");
        let outcome = request.join().unwrap().expect("prepare_upgrade answered");
        let UpgradeOutcome::Failed(why) = &outcome else {
            panic!("expected a declared-loss take-back, got {outcome:?}");
        };
        let x: u64 = why
            .split("output since offset ")
            .nth(1)
            .and_then(|t| t.split_whitespace().next())
            .and_then(|n| n.parse().ok())
            .unwrap_or_else(|| panic!("no take-back offset in {why:?}"));
        served_by(&dir, pane.holder_pid);
        // The live connection (never on the successor) reads the notice AT X.
        let marker: &[u8] = b"holder upgrade: the successor holder died after taking over";
        let r2 = read_until(&mut live, r.next, Duration::from_secs(10), |r| {
            count(&r.bytes, marker) > 0
        });
        assert!(
            !r2.lost,
            "the connection that stayed on this holder lost nothing"
        );
        assert_eq!(
            r2.bytes.first(),
            Some(&0x18),
            "the stream after X starts with the notice (r.next {} == X {x})",
            r.next
        );
        assert_eq!(r.next, x, "the live connection had read exactly to X");

        // 1. An offset past X but inside this holder's stream: lost from X to
        //    the end, and nothing at an offset below it is ever replayed.
        let mut h1 = attach(&dir, Some(x + 1));
        assert_eq!(h1.info.start_offset, x, "start_offset names the break");
        let end = h1.info.end_offset;
        assert!(end > x + 1, "the notice is in the ring: end {end}, X {x}");
        match first_event(&mut h1) {
            Event::Lost {
                from_offset,
                to_offset,
            } => assert_eq!((from_offset, to_offset), (x, end)),
            other => panic!("expected output_lost first, got {other:?}"),
        }
        send_input(&dir, b"after\n");
        let r3 = read_until(&mut h1, end, Duration::from_secs(10), |r| {
            count(&r.bytes, b"got:after") > 0
        });
        assert!(
            !r3.lost,
            "one loss report, then contiguous output from {end}"
        );
        assert_eq!(count(&r3.bytes, marker), 0, "nothing before `end` replayed");

        // 2. An offset past this holder's END (the dead successor ran ahead):
        //    lost from X to that offset; output resumes exactly there.
        let mut h2 = attach(&dir, None);
        let tail_end = h2.info.end_offset;
        drop(h2);
        let ahead = tail_end + 40;
        let mut h3 = attach(&dir, Some(ahead));
        assert_eq!(h3.info.start_offset, x);
        match first_event(&mut h3) {
            Event::Lost {
                from_offset,
                to_offset,
            } => assert_eq!((from_offset, to_offset), (x, ahead)),
            other => panic!("expected output_lost first, got {other:?}"),
        }
        let long = format!("{}\n", "z".repeat(200));
        send_input(&dir, long.as_bytes());
        let r4 = read_until(&mut h3, ahead, Duration::from_secs(10), |r| {
            r.bytes.len() > 10
        });
        assert!(!r4.lost);
        assert!(r4.next > ahead, "output resumed at the client's offset");

        // 3. An attach AT X is a normal resume: no loss, the notice first.
        let mut at_x = attach(&dir, Some(x));
        assert_eq!(at_x.info.start_offset, x);
        let r5 = read_until(&mut at_x, x, Duration::from_secs(10), |r| {
            count(&r.bytes, marker) > 0
        });
        assert!(!r5.lost, "an offset at X is this holder's own");
        assert_eq!(r5.bytes.first(), Some(&0x18));
        no_snapshot_left(&dir);
    }

    /// Review r3 nit (path C): the successor's endpoint rename FAILS after the
    /// commit. It sends no `serving`, waits for the predecessor to leave
    /// before it touches the master or the held input, then serves the pane
    /// (unreachable: the lock is held, so a probe reads UNKNOWN). The held
    /// input reaches the child exactly once.
    #[test]
    fn pty_holder_handoff_failed_publish_waits_for_the_predecessor_then_takes_over() {
        let out = pane_dir("pubout");
        std::fs::create_dir_all(&out).unwrap();
        let file = out.join("lines");
        let mut spec = ChildSpec::new(vec![
            "/bin/sh".into(),
            "-c".into(),
            format!(
                "stty -echo; while read l; do echo \"$l\" >> '{}'; done",
                file.display()
            )
            .into(),
        ]);
        spec.env = std::env::vars_os().filter(|(k, _)| !is_seam(k)).collect();
        spec.cwd = Some(OsString::from("/tmp"));
        let mut pane = start_with(
            "pubfail",
            spec,
            Some("fail-publish"),
            &[(COMMIT_DELAY_ENV, "1500")],
        );
        let dir = pane.dir.clone();
        served_by(&dir, pane.holder_pid);
        let req_dir = dir.clone();
        let request = std::thread::spawn(move || {
            connect(&req_dir, &p1(), soon())
                .unwrap()
                .prepare_upgrade(upgrade_deadline())
        });
        let successor = logged_pid(&dir, "adopt: acked");
        pane.adopt_cleanup(successor);
        send_input(&dir, b"held-c\n");
        let outcome = request.join().unwrap().expect("prepare_upgrade answered");
        assert!(
            matches!(outcome, UpgradeOutcome::Upgraded { successor_pid, .. } if successor_pid == successor),
            "the successor is alive, so the commit stands: {outcome:?}"
        );
        pane.holder_exits_within(Duration::from_secs(10))
            .expect("the predecessor exits after its serving wait");
        let end = Instant::now() + Duration::from_secs(10);
        loop {
            let got = std::fs::read_to_string(&file).unwrap_or_default();
            if got.contains("held-c") {
                assert_eq!(got.matches("held-c").count(), 1, "held input once");
                break;
            }
            assert!(
                Instant::now() < end,
                "held input never reached the child: {got:?}"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        let log = std::fs::read_to_string(dir.join("p1.log")).unwrap();
        let at = |n: &str| {
            log.find(n)
                .unwrap_or_else(|| panic!("{n:?} not logged: {log}"))
        };
        assert!(at("waiting for the predecessor to leave") < at("the predecessor left; reading"));
        assert!(at("no `serving` from successor") < at("the predecessor left; reading"));
        assert!(
            is_same_process(successor, pane.extra[0].1.as_deref()),
            "the successor serves on"
        );
        assert!(
            is_same_process(pane.child_pid, pane.child_token.as_deref()),
            "same child"
        );
        let record = read_record(&lock_path(&dir, &p1())).expect("a lock record");
        assert_eq!(record.holder_pid, successor);
        assert!(
            matches!(probe(&dir, &p1(), soon()), Probe::Unknown { .. }),
            "unreachable but held: UNKNOWN, never dead"
        );
        let _ = std::fs::remove_dir_all(&out);
    }

    /// Review r4, H2: the faults several tests here rely on exist only in a
    /// debug-built holder. `cargo test` builds `CARGO_BIN_EXE_*` with the
    /// test's own profile, so this crate being built with debug assertions
    /// means the holder under test is too — a release test run fails HERE,
    /// by name, instead of in the fault tests.
    #[test]
    fn pty_holder_handoff_tests_drive_a_debug_holder() {
        assert!(
            std::hint::black_box(cfg!(debug_assertions)),
            "the handoff fault tests need a debug-built holder (post-commit faults are \
             compiled out of release builds)"
        );
    }
}

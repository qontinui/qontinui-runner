//! End-to-end data path (plan 2026-09-12 Phase 2): the real
//! `qontinui-pty-holder` binary owns a real PTY child, and the library's
//! client attaches, sends input, reads output, resizes, pauses, detaches,
//! reattaches at an offset, and collects the exit.
//!
//! Unix only: every child here is a POSIX shell script driving `stty`. The
//! Windows (ConPTY) data path is type-checked for `x86_64-pc-windows-msvc` and
//! first executes on the `holder-crates (windows-latest)` CI leg's transport
//! tests; these scripted cases are UNRUN there, not passed.
#![cfg(unix)]

use std::ffi::OsString;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use qontinui_pty_holder::client::{connect, AttachedStream, ConnectError, Event, InputStream};
use qontinui_pty_holder::pane::PaneId;
use qontinui_pty_holder::protocol::{ExitReply, Reply};
use qontinui_pty_holder::spec::{write_spec, ChildSpec};

const BIN: &str = env!("CARGO_BIN_EXE_qontinui-pty-holder");

fn soon() -> Instant {
    Instant::now() + Duration::from_secs(10)
}

/// A fresh, short pane dir (a Unix socket path is capped at ~104-108 bytes).
fn pane_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .subsec_nanos();
    PathBuf::from("/tmp").join(format!("ptyd-{tag}-{}-{nanos:x}", std::process::id()))
}

fn pid(id: &str) -> PaneId {
    PaneId::new(id).unwrap()
}

fn alive(pid: u32) -> bool {
    // SAFETY: signal 0 only checks for existence; the pid came from our holder.
    unsafe { libc::kill(pid as i32, 0) == 0 }
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
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

impl Holder {
    /// Wait up to `bound` for the holder process to exit on its own.
    fn exits_within(&mut self, bound: Duration) -> Option<std::process::ExitStatus> {
        let end = Instant::now() + bound;
        loop {
            if let Ok(Some(s)) = self.child.try_wait() {
                return Some(s);
            }
            if Instant::now() >= end {
                return None;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

/// `sh -c <script>` with this process's environment, as the pane's child.
fn sh(script: &str) -> ChildSpec {
    let mut spec = ChildSpec::new(vec!["/bin/sh".into(), "-c".into(), script.into()]);
    spec.env = std::env::vars_os().collect();
    spec
}

/// Write the spec, spawn the holder, read its ready line.
fn start(tag: &str, mut spec: ChildSpec) -> Holder {
    let dir = pane_dir(tag);
    if spec.cwd.is_none() {
        spec.cwd = Some(OsString::from("/tmp"));
    }
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

fn attach(h: &Holder, from: Option<u64>) -> AttachedStream {
    connect(&h.dir, &pid("p1"), soon())
        .expect("handshake")
        .attach(from, soon())
        .expect("attach")
}

/// The pane's input stream (input never rides the attached connection).
fn input_of(h: &Holder) -> InputStream {
    connect(&h.dir, &pid("p1"), soon())
        .expect("handshake")
        .open_input(soon())
        .expect("open_input")
}

/// Everything the stream yields until `done(&collected)` holds; panics after
/// `bound`. Output must be CONTIGUOUS from `next` — a gap or an overlap is a
/// failure, not something to paper over. Returns the bytes, the next offset,
/// and every non-output event in order.
struct Collected {
    bytes: Vec<u8>,
    next: u64,
    events: Vec<Event>,
}

fn collect_until(
    s: &mut AttachedStream,
    mut next: u64,
    bound: Duration,
    done: impl Fn(&Collected) -> bool,
) -> Collected {
    let end = Instant::now() + bound;
    let mut c = Collected {
        bytes: Vec::new(),
        next,
        events: Vec::new(),
    };
    while !done(&c) {
        assert!(
            Instant::now() < end,
            "stream did not reach the expected state within {bound:?}; have {} bytes, events {:?}, tail {:?}",
            c.bytes.len(),
            c.events,
            String::from_utf8_lossy(&c.bytes[c.bytes.len().saturating_sub(200)..])
        );
        match s.reader.next_event(Some(end)).expect("stream read") {
            None => break,
            Some(Event::Output { offset, bytes }) => {
                assert_eq!(offset, next, "output must be contiguous");
                next += bytes.len() as u64;
                c.bytes.extend_from_slice(&bytes);
                c.next = next;
            }
            Some(other) => c.events.push(other),
        }
    }
    c
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

fn exit_of(events: &[Event]) -> Option<ExitReply> {
    events.iter().find_map(|e| match e {
        Event::Exit(x) => Some(*x),
        _ => None,
    })
}

/// Input in, output out, a resize the child observes, and the child's real
/// exit code in the `exit` frame — after which the holder itself exits.
#[test]
fn pty_holder_data_path_echo_resize_and_exit_code() {
    let mut h = start(
        "echo",
        sh("stty -echo; read line; echo \"got:$line\"; read x; stty size; read y; exit 7"),
    );
    let mut s = attach(&h, None);
    let mut inp = input_of(&h);
    assert_eq!(s.info.child_pid, h.child_pid);
    assert_eq!((s.info.cols, s.info.rows), (80, 24));
    let start_at = s.info.start_offset;

    inp.input(b"hello\n", soon()).unwrap();
    let c = collect_until(&mut s, start_at, Duration::from_secs(10), |c| {
        contains(&c.bytes, b"got:hello")
    });

    s.writer.resize(100, 40, soon()).unwrap();
    inp.input(b"\n", soon()).unwrap();
    let c2 = collect_until(&mut s, c.next, Duration::from_secs(10), |c| {
        contains(&c.bytes, b"40 100")
    });
    assert!(
        c2.events.contains(&Event::Reply(Reply::Ok {
            verb: "resize".into()
        })),
        "{:?}",
        c2.events
    );

    inp.input(b"\n", soon()).unwrap();
    let c3 = collect_until(&mut s, c2.next, Duration::from_secs(10), |c| {
        exit_of(&c.events).is_some()
    });
    assert_eq!(
        exit_of(&c3.events),
        Some(ExitReply {
            code: Some(7),
            signal: None
        })
    );
    // The exit was delivered, so the holder leaves, with the child's code.
    let status = h
        .exits_within(Duration::from_secs(10))
        .expect("holder exits");
    assert_eq!(status.code(), Some(7));
    assert!(!alive(h.child_pid));
}

/// Byte fidelity end to end: all 256 byte values and invalid UTF-8 travel
/// from the client through input frames into the child's PTY, and from the
/// child's PTY through output frames back, unchanged.
///
/// The PTY line discipline WOULD translate: `ONLCR` turns `\n` into `\r\n` on
/// output, `ICRNL` turns `\r` into `\n` on input, `^C`/`^D`/`^S` are acted on.
/// `stty raw -echo` turns all of that off, which is what makes "unchanged" the
/// honest expectation. The child signals with `R` once raw mode is on, so no
/// input is sent while the discipline is still cooked.
#[test]
fn pty_holder_data_path_byte_fidelity_all_256_and_invalid_utf8() {
    let dir_for_files = pane_dir("fid-files");
    std::fs::create_dir_all(&dir_for_files).unwrap();
    let mut payload: Vec<u8> = (0u8..=255).collect();
    payload.extend_from_slice(&[
        0x80, 0xBF, 0xC0, 0xAF, 0xF0, 0x9F, 0x98, 0xED, 0xA0, 0x80, 0xFF, 0xFE,
    ]);
    payload.extend((0u8..=255).rev());
    let out_file = dir_for_files.join("from-pty-input.bin");
    let src_file = dir_for_files.join("to-pty-output.bin");
    std::fs::write(&src_file, &payload).unwrap();
    let script = format!(
        "stty raw -echo; printf R; head -c {} > '{}'; cat '{}'",
        payload.len(),
        out_file.display(),
        src_file.display()
    );
    let mut h = start("fid", sh(&script));
    let mut s = attach(&h, None);
    let mut inp = input_of(&h);
    let first_offset = s.info.start_offset;
    let ready = collect_until(&mut s, first_offset, Duration::from_secs(10), |c| {
        c.bytes.ends_with(b"R")
    });
    assert_eq!(ready.bytes, b"R", "raw mode: nothing but the marker");

    // Input direction, split across several frames on purpose.
    for chunk in payload.chunks(37) {
        inp.input(chunk, soon()).unwrap();
    }
    let rest = collect_until(&mut s, ready.next, Duration::from_secs(15), |c| {
        exit_of(&c.events).is_some()
    });
    assert_eq!(
        std::fs::read(&out_file).unwrap(),
        payload,
        "input frames reached the child's PTY byte-identical"
    );
    assert_eq!(
        rest.bytes, payload,
        "the child's PTY output reached the client byte-identical"
    );
    assert_eq!(exit_of(&rest.events).unwrap().code, Some(0));
    let _ = h.exits_within(Duration::from_secs(10));
    let _ = std::fs::remove_dir_all(&dir_for_files);
}

/// Reattach at an offset resumes with no duplicate and no gap; detach leaves
/// the child running; a resume from before the ring is reported as a loss of
/// exactly the bytes the ring no longer holds.
///
/// The live parts fit the ring. That is deliberate and honest: the holder
/// NEVER pauses its PTY read (flow control gates emission only), so a burst
/// larger than the ring can roll past even an attached consumer — which the
/// stream then reports as `output_lost`, never silently. The loss under test
/// here is produced while nobody is attached.
#[test]
fn pty_holder_data_path_reattach_resumes_and_gaps_are_reported() {
    const RING: usize = 32 * 1024;
    let files = pane_dir("re-files");
    std::fs::create_dir_all(&files).unwrap();
    // A position-unique pattern, so any duplicate or skipped byte shows.
    let pattern = |n: usize, salt: u32| -> Vec<u8> {
        (0..n as u32)
            .map(|i| (i.wrapping_mul(2_654_435_761).wrapping_add(salt) >> 13) as u8)
            .collect()
    };
    let part1 = pattern(3_000, 1);
    let part2 = pattern(20_000, 2);
    let part3 = pattern(100_000, 3);
    for (name, bytes) in [("p1", &part1), ("p2", &part2), ("p3", &part3)] {
        std::fs::write(files.join(name), bytes).unwrap();
    }
    let script = format!(
        "stty raw -echo; cat '{a}'; read x; cat '{b}'; read y; cat '{c}'; read z",
        a = files.join("p1").display(),
        b = files.join("p2").display(),
        c = files.join("p3").display()
    );
    let mut spec = sh(&script);
    spec.ring_capacity = Some(RING);
    let h = start("re", spec);

    // First consumer: reads part1, then detaches.
    let mut s = attach(&h, None);
    let first_offset = s.info.start_offset;
    let c1 = collect_until(&mut s, first_offset, Duration::from_secs(10), |c| {
        c.bytes.len() >= part1.len()
    });
    assert_eq!(c1.bytes, part1);
    let x = c1.next;
    s.writer.detach(soon()).unwrap();
    let after = collect_until(&mut s, x, Duration::from_secs(10), |c| {
        c.events.contains(&Event::Reply(Reply::Ok {
            verb: "detach".into(),
        }))
    });
    assert!(after.bytes.is_empty());
    assert!(
        matches!(s.reader.next_event(Some(soon())), Ok(None)),
        "the holder closes a detached connection"
    );
    drop(s);
    std::thread::sleep(Duration::from_millis(200));
    assert!(alive(h.child_pid), "detach leaves the child running");

    // Second consumer resumes exactly at X, then drives part2 out.
    let mut s = attach(&h, Some(x));
    let mut inp = input_of(&h);
    assert_eq!(s.info.start_offset, x);
    assert_eq!(s.info.end_offset, x, "nothing new while detached");
    inp.input(b"\n", soon()).unwrap();
    let c2 = collect_until(&mut s, x, Duration::from_secs(15), |c| {
        c.bytes.len() >= part2.len()
    });
    assert!(
        c2.events.iter().all(|e| !matches!(e, Event::Lost { .. })),
        "{:?}",
        c2.events
    );
    assert_eq!(c2.bytes, part2, "resumed with no duplicate and no gap");
    let y = c2.next;

    // Trigger part3 and leave at once: it is produced with nobody reading.
    inp.input(b"\n", soon()).unwrap();
    drop(s);
    let total = (part1.len() + part2.len() + part3.len()) as u64;
    let end = Instant::now() + Duration::from_secs(15);
    let mut s = loop {
        let s = attach(&h, Some(y));
        if s.info.end_offset >= total {
            break s;
        }
        assert!(Instant::now() < end, "part3 never finished: {:?}", s.info);
        drop(s);
        std::thread::sleep(Duration::from_millis(100));
    };
    // The ring holds only its last RING bytes; [y, ring_start) is reported
    // lost, exactly, and the stream continues at ring_start.
    assert_eq!(s.info.start_offset, y);
    assert_eq!(s.info.end_offset, total);
    let ring_start = s.info.ring_start_offset;
    assert_eq!(ring_start, total - RING as u64);
    let first = s.reader.next_event(Some(soon())).unwrap().unwrap();
    assert_eq!(
        first,
        Event::Lost {
            from_offset: y,
            to_offset: ring_start
        }
    );
    let c3 = collect_until(&mut s, ring_start, Duration::from_secs(10), |c| {
        c.next >= total
    });
    let mut whole = part1.clone();
    whole.extend_from_slice(&part2);
    whole.extend_from_slice(&part3);
    assert_eq!(c3.bytes, whole[ring_start as usize..].to_vec());

    // A requested offset the holder never produced is clamped to the end.
    drop(s);
    let s = attach(&h, Some(total + 1_000));
    assert_eq!(s.info.start_offset, total);
    drop(s);
    let _ = std::fs::remove_dir_all(&files);
}

/// `pause` withholds output frames but never the PTY read: the child keeps
/// producing (its output fills the ring), and `resume` delivers it all, from
/// exactly where the pause stopped.
#[test]
fn pty_holder_data_path_pause_gates_emission_not_reads() {
    let mut h = start(
        "pause",
        sh("stty -echo; read x; i=0; while [ $i -lt 200 ]; do echo line-$i; i=$((i+1)); done; read y"),
    );
    let mut s = attach(&h, None);
    let mut inp = input_of(&h);
    let next = s.info.start_offset;
    s.writer.pause(soon()).unwrap();
    let c = collect_until(&mut s, next, Duration::from_secs(5), |c| {
        c.events.contains(&Event::Reply(Reply::Ok {
            verb: "pause".into(),
        }))
    });
    inp.input(b"go\n", soon()).unwrap();
    // While paused nothing arrives, though the child is printing.
    let quiet = s
        .reader
        .next_event(Some(Instant::now() + Duration::from_millis(800)));
    assert!(
        matches!(&quiet, Err(ConnectError::Io(e)) if e.kind() == std::io::ErrorKind::TimedOut),
        "paused connection received {quiet:?}"
    );
    s.writer.resume(soon()).unwrap();
    let c2 = collect_until(&mut s, c.next, Duration::from_secs(10), |c| {
        contains(&c.bytes, b"line-199")
    });
    assert!(contains(&c2.bytes, b"line-0\r\n"));
    assert!(
        c2.events.iter().all(|e| !matches!(e, Event::Lost { .. })),
        "{:?}",
        c2.events
    );
    s.writer.kill(soon()).unwrap();
    drop(s);
    let _ = h.exits_within(Duration::from_secs(10));
}

/// `kill` ends the child (SIGHUP to its process group), the `exit` frame says
/// how — a signal, with no fabricated code — and the holder then exits.
#[test]
fn pty_holder_data_path_kill_ends_the_child() {
    let mut h = start("kill", sh("exec sleep 3600"));
    assert!(alive(h.child_pid));
    let mut s = attach(&h, None);
    s.writer.kill(soon()).unwrap();
    let first_offset = s.info.start_offset;
    let c = collect_until(&mut s, first_offset, Duration::from_secs(10), |c| {
        exit_of(&c.events).is_some()
    });
    assert!(c.events.contains(&Event::Reply(Reply::Ok {
        verb: "kill".into()
    })));
    assert_eq!(
        exit_of(&c.events),
        Some(ExitReply {
            code: None,
            signal: Some(libc::SIGHUP)
        }),
        "a signal death is reported as the signal, never as code 0"
    );
    assert!(!alive(h.child_pid), "the child is gone (and reaped)");
    let status = h
        .exits_within(Duration::from_secs(10))
        .expect("holder exits");
    assert_eq!(status.code(), Some(128 + libc::SIGHUP));
}

/// Review round 1 finding 2, round 2 F2: input the child does not read never
/// stalls the attached connection. 12 MiB of input to a child that never
/// reads its stdin goes on the INPUT stream — far more than the PTY's input
/// buffer, the holder's 64-frame input queue (`INPUT_QUEUE_FRAMES` x 64 KiB)
/// and both socket buffers together — so the writer is still blocked after
/// 1.5 s, which means the queue is FULL. Every control verb on the attached
/// connection is still answered while it is: `resize`, `pause`, `resume`,
/// then `kill`. The child dies with the queue full; the writer discards it,
/// the blocked input returns, and the holder exits.
#[test]
fn pty_holder_data_path_control_is_served_while_input_is_blocked() {
    let mut h = start("blocked", sh("exec sleep 3600"));
    let mut s = attach(&h, None);
    let first_offset = s.info.start_offset;
    let mut inp = input_of(&h);
    let frames = qontinui_pty_holder::pty::INPUT_QUEUE_FRAMES * 3;
    let paster = std::thread::spawn(move || {
        let chunk = vec![b'x'; 64 * 1024];
        let deadline = Instant::now() + Duration::from_secs(60);
        let mut sent = 0;
        for _ in 0..frames {
            if inp.input(&chunk, deadline).is_err() {
                break;
            }
            sent += 1;
        }
        sent
    });
    std::thread::sleep(Duration::from_millis(1500));
    assert!(
        !paster.is_finished(),
        "{frames} frames of 64 KiB did not back up: the input queue is not full"
    );
    let mut next = first_offset;
    for (req, verb) in [
        (
            qontinui_pty_holder::protocol::Request::Resize {
                cols: 100,
                rows: 40,
            },
            "resize",
        ),
        (qontinui_pty_holder::protocol::Request::Pause, "pause"),
        (qontinui_pty_holder::protocol::Request::Resume, "resume"),
    ] {
        s.writer.request(&req, soon()).unwrap();
        let c = collect_until(&mut s, next, Duration::from_secs(3), |c| {
            c.events
                .contains(&Event::Reply(Reply::Ok { verb: verb.into() }))
        });
        next = c.next;
    }
    assert!(
        !paster.is_finished(),
        "still blocked while control was served"
    );
    s.writer.kill(soon()).unwrap();
    let c2 = collect_until(&mut s, next, Duration::from_secs(10), |c| {
        exit_of(&c.events).is_some()
    });
    assert!(c2.events.contains(&Event::Reply(Reply::Ok {
        verb: "kill".into()
    })));
    assert_eq!(exit_of(&c2.events).unwrap().signal, Some(libc::SIGHUP));
    assert!(!alive(h.child_pid));
    let end = Instant::now() + Duration::from_secs(20);
    while !paster.is_finished() {
        assert!(
            Instant::now() < end,
            "the blocked input never returned after the kill"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = paster.join();
    assert!(
        h.exits_within(Duration::from_secs(10)).is_some(),
        "the holder exits once the exit is delivered, queued input notwithstanding"
    );
}

/// Review round 3, M1: a newer input connection SUPERSEDES an older one. A
/// paste is blocked on input connection #1 (the child is not reading yet, the
/// queue is full); the runner gives up on it and opens #2, which sends a
/// marker. Once the child starts reading, every byte of #2 arrives, and NO
/// byte of #1 arrives after any byte of #2 — #1's in-hand and still-buffered
/// frames are discarded, not raced into the queue behind #2's.
#[test]
fn pty_holder_data_path_a_new_input_connection_supersedes_the_old_one() {
    let files = pane_dir("supf");
    std::fs::create_dir_all(&files).unwrap();
    let go = files.join("go");
    let out = files.join("out");
    let script = format!(
        "stty raw -echo; printf R; while [ ! -e '{}' ]; do sleep 0.05; done; exec cat > '{}'",
        go.display(),
        out.display()
    );
    let h = start("sup", sh(&script));
    let mut s = attach(&h, None);
    let first_offset = s.info.start_offset;
    collect_until(&mut s, first_offset, Duration::from_secs(10), |c| {
        c.bytes.ends_with(b"R")
    });
    let mut one = input_of(&h);
    let frames = qontinui_pty_holder::pty::INPUT_QUEUE_FRAMES * 3;
    let paster = std::thread::spawn(move || {
        let chunk = vec![b'A'; 64 * 1024];
        let deadline = Instant::now() + Duration::from_secs(60);
        for _ in 0..frames {
            if one.input(&chunk, deadline).is_err() {
                return false;
            }
        }
        true
    });
    std::thread::sleep(Duration::from_millis(1500));
    assert!(
        !paster.is_finished(),
        "connection #1's paste is not blocked"
    );

    let marker = vec![b'B'; 1000];
    let mut two = input_of(&h);
    let sender =
        std::thread::spawn(move || two.input(&marker, Instant::now() + Duration::from_secs(60)));
    std::thread::sleep(Duration::from_millis(300));
    std::fs::write(&go, b"").unwrap();

    let end = Instant::now() + Duration::from_secs(30);
    let got = loop {
        let got = std::fs::read(&out).unwrap_or_default();
        if got.iter().filter(|b| **b == b'B').count() >= 1000 {
            break got;
        }
        assert!(
            Instant::now() < end,
            "connection #2's input never fully arrived"
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    sender
        .join()
        .unwrap()
        .expect("connection #2's input was accepted");
    // Give any straggler from #1 the chance to show up before judging.
    std::thread::sleep(Duration::from_millis(500));
    let got = std::fs::read(&out).unwrap_or(got);
    let first_b = got.iter().position(|b| *b == b'B').unwrap();
    assert!(
        !got[first_b..].contains(&b'A'),
        "bytes of the superseded connection #1 arrived after connection #2's"
    );
    assert!(
        !paster.join().unwrap(),
        "connection #1 was closed when superseded, not served to completion"
    );
    s.writer.kill(soon()).unwrap();
    let _ = std::fs::remove_dir_all(&files);
}

/// Input is refused on the attached connection: it would put the input queue
/// in front of that connection's control verbs.
#[test]
fn pty_holder_data_path_input_on_the_attached_connection_is_refused() {
    let h = start("noinput", sh("exec sleep 3600"));
    let mut s = attach(&h, None);
    let first_offset = s.info.start_offset;
    // The typed API has no way to send input on an attached connection, so
    // attach a second connection by hand and send a data frame on it.
    let mut raw = connect(&h.dir, &pid("p1"), soon()).expect("handshake");
    raw.send_raw_frame(
        qontinui_pty_holder::frame::KIND_CONTROL,
        br#"{"type":"attach","from_offset":null}"#,
        soon(),
    )
    .unwrap();
    let _attached = raw.recv_raw_frame(soon()).unwrap().expect("attached");
    raw.send_raw_frame(qontinui_pty_holder::frame::KIND_DATA, b"x", soon())
        .unwrap();
    let mut rejected = false;
    for _ in 0..8 {
        match raw.recv_raw_frame(soon()) {
            Ok(Some(f)) if f.kind == qontinui_pty_holder::frame::KIND_CONTROL => {
                if let Ok(Reply::Rejected { reason, .. }) =
                    qontinui_pty_holder::protocol::parse_reply(&f.payload)
                {
                    assert_eq!(
                        reason,
                        qontinui_pty_holder::protocol::RejectReason::UnexpectedDataFrame
                    );
                    rejected = true;
                    break;
                }
            }
            Ok(Some(_)) => {}
            _ => break,
        }
    }
    assert!(rejected, "input on an attached connection must be rejected");
    // The pane is unaffected: the other attached connection still works.
    s.writer.kill(soon()).unwrap();
    let c = collect_until(&mut s, first_offset, Duration::from_secs(10), |c| {
        exit_of(&c.events).is_some()
    });
    assert!(exit_of(&c.events).is_some());
}

/// A child that ignores SIGHUP still dies: SIGKILL follows the grace.
#[test]
fn pty_holder_data_path_kill_escalates_past_an_ignored_hangup() {
    let mut h = start("killhard", sh("trap '' HUP; while :; do sleep 1; done"));
    let mut s = attach(&h, None);
    s.writer.kill(soon()).unwrap();
    let first_offset = s.info.start_offset;
    let c = collect_until(&mut s, first_offset, Duration::from_secs(15), |c| {
        exit_of(&c.events).is_some()
    });
    assert_eq!(exit_of(&c.events).unwrap().signal, Some(libc::SIGKILL));
    assert!(h.exits_within(Duration::from_secs(10)).is_some());
}

/// A child that exits with nobody attached: the holder keeps the tail and the
/// code for a client that arrives later (within the linger), and exits once it
/// has delivered them.
#[test]
fn pty_holder_data_path_late_attach_gets_the_tail_and_the_exit() {
    let mut h = start("late", sh("echo bye-now; exit 3"));
    std::thread::sleep(Duration::from_millis(500));
    assert!(
        h.exits_within(Duration::from_millis(1)).is_none(),
        "no client has collected the exit yet, so the holder waits"
    );
    let mut s = attach(&h, Some(0));
    let c = collect_until(&mut s, 0, Duration::from_secs(10), |c| {
        exit_of(&c.events).is_some()
    });
    assert!(contains(&c.bytes, b"bye-now"));
    assert_eq!(exit_of(&c.events).unwrap().code, Some(3));
    assert_eq!(
        h.exits_within(Duration::from_secs(10))
            .expect("exits after delivery")
            .code(),
        Some(3)
    );
}

/// With nobody ever attaching, the holder still leaves once the linger runs
/// out — it does not wait forever for a runner that is gone.
#[test]
fn pty_holder_data_path_unclaimed_exit_lingers_then_leaves() {
    let mut spec = sh("exit 0");
    spec.exit_linger_ms = Some(300);
    let mut h = start("linger", spec);
    let status = h
        .exits_within(Duration::from_secs(10))
        .expect("holder leaves after the linger");
    assert_eq!(status.code(), Some(0));
}

/// The child gets EXACTLY the spec's environment (plan D6): nothing of the
/// holder's own leaks into the pane.
#[test]
fn pty_holder_data_path_child_env_is_exactly_the_spec() {
    let mut spec = ChildSpec::new(vec!["/usr/bin/env".into()]);
    spec.env = vec![
        ("ONLY_THIS".into(), "yes".into()),
        ("PATH".into(), "/usr/bin:/bin".into()),
    ];
    spec.cwd = Some("/".into());
    // The holder itself is given a marker variable the child must NOT see.
    let dir = pane_dir("env");
    write_spec(&dir, &pid("p1"), &spec).unwrap();
    let mut child = Command::new(BIN)
        .args(["--pane-id", "p1", "--pane-dir"])
        .arg(&dir)
        .env("HOLDER_ONLY_SECRET", "leaked")
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    assert!(line.starts_with("holder_ready "), "{line}");
    let mut s = connect(&dir, &pid("p1"), soon())
        .unwrap()
        .attach(Some(0), soon())
        .unwrap();
    let c = collect_until(&mut s, 0, Duration::from_secs(10), |c| {
        exit_of(&c.events).is_some()
    });
    let out = String::from_utf8_lossy(&c.bytes).to_string();
    assert!(out.contains("ONLY_THIS=yes"), "{out}");
    assert!(!out.contains("HOLDER_ONLY_SECRET"), "{out}");
    // portable-pty always sets SHELL; nothing else beyond the spec appears.
    for l in out.lines().filter(|l| !l.trim().is_empty()) {
        let k = l.split('=').next().unwrap();
        assert!(
            ["ONLY_THIS", "PATH", "SHELL"].contains(&k),
            "unexpected variable in the child: {l}"
        );
    }
    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&dir);
}

/// The spec never outlives start-up: the holder unlinks it.
#[test]
fn pty_holder_data_path_spec_is_consumed() {
    let h = start("spec", sh("exec sleep 3600"));
    assert!(!h.dir.join("p1.spec").exists());
    assert!(Path::new(&h.dir).join("p1.lock").exists());
}

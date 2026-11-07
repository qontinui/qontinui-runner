//! Throughput of the holder's mode tracker on the PTY read path (plan
//! 2026-09-12 Phase 2b, D14). The tracker is fed every byte the PTY produces
//! under the pane lock, so its cost per read is what this measures.
//!
//! `#[ignore]`d — a timing, not a pass/fail property. Run it, in release, with:
//!
//! ```text
//! bash qontinui-claude-config/scripts/cargo-guard.sh test -p qontinui-pty-holder \
//!     --release -- --ignored pty_holder_attach_bench_feed_throughput --nocapture
//! ```
//!
//! It prints three lines: the tracker as the holder feeds it (16 KiB reads),
//! a byte-at-a-time `vte` feed (the first draft's design, with a no-op
//! performer — a lower bound on that design's cost), and chunked `vte` alone
//! (the floor any tracker over `vte` sits above).

use std::time::{Duration, Instant};

use qontinui_pty_holder::modes::ModeTracker;

struct Nop;
impl vte::Perform for Nop {}

/// One read the holder makes of its PTY.
const READ: usize = 16 * 1024;

/// 16 MiB of a full-screen TUI's output: each frame is a synchronized-output
/// block that hides the cursor, redraws 40 coloured lines, retitles the
/// window and shows the cursor again — every frame carries a title change.
fn tui_stream() -> Vec<u8> {
    let mut frame = Vec::new();
    frame.extend_from_slice(b"\x1b[?2026h\x1b[?25l\x1b[H");
    for i in 0..40 {
        frame.extend_from_slice(format!("\x1b[{};1H\x1b[38;5;{}m", i + 1, i % 255).as_bytes());
        frame.extend_from_slice("line of agent output \u{2733} with some text here ok".as_bytes());
        frame.extend_from_slice(b"\x1b[0m");
    }
    frame.extend_from_slice("\x1b]2;\u{2733} Claude Code\x07\x1b[?25h\x1b[?2026l".as_bytes());
    let mut out = Vec::new();
    while out.len() < 16 * 1024 * 1024 {
        out.extend_from_slice(&frame);
    }
    out
}

fn report(what: &str, bytes: usize, took: Duration) {
    let mib = bytes as f64 / (1024.0 * 1024.0);
    let reads = bytes as f64 / READ as f64;
    println!(
        "{what}: {:.0} MiB/s, {:.1} us per 16 KiB read",
        mib / took.as_secs_f64(),
        took.as_secs_f64() * 1e6 / reads
    );
}

#[test]
#[ignore = "timing benchmark; run explicitly in release (see the module docs)"]
fn pty_holder_attach_bench_feed_throughput() {
    let stream = tui_stream();

    let mut tracker = ModeTracker::new();
    let started = Instant::now();
    for read in stream.chunks(READ) {
        tracker.feed(read);
    }
    report(
        "mode tracker (as fed by the holder)",
        stream.len(),
        started.elapsed(),
    );
    assert_eq!(tracker.fed(), stream.len() as u64);

    let mut parser = vte::Parser::new();
    let started = Instant::now();
    for b in &stream {
        parser.advance(&mut Nop, std::slice::from_ref(b));
    }
    report(
        "vte byte-at-a-time, no-op performer",
        stream.len(),
        started.elapsed(),
    );

    let mut parser = vte::Parser::new();
    let started = Instant::now();
    for read in stream.chunks(READ) {
        parser.advance(&mut Nop, read);
    }
    report(
        "vte chunked, no-op performer",
        stream.len(),
        started.elapsed(),
    );
}

//! Pure-CPU cost of the terminal output transport's encoding hops (plan
//! `2026-09-20-terminal-output-transport-is-unmeasured-encoded-broadcast`,
//! Phase 1, "Rust, pure" layer).
//!
//! `terminal` is a bin-crate module, so an external criterion bench cannot
//! reach `TerminalOutputWire` or the pipe's redaction; these are `#[ignore]`d
//! tests instead. Each measurement test prints one JSON line per corpus:
//!
//! ```text
//! {"bench":"encode_wire","corpus":"repaint_200x60","profile":"release+debug-assertions",
//!  "frames":N,"bytes":B,"ns_per_frame":..,"ns_per_byte":..}
//! ```
//!
//! Run:
//! `CARGO_PROFILE_RELEASE_DEBUG_ASSERTIONS=true TRANSPORT_COST_PROFILE=release+debug-assertions bash qontinui-claude-config/scripts/cargo-guard.sh test --release --bin qontinui-runner -- terminal::transport_cost --ignored --nocapture --test-threads=1`
//! (the test build needs `debug_assertions` for the runner's test-support
//! modules; `--test-threads=1` so the benches do not compete for cores).
//!
//! The four benches, each per frame of the corpus:
//! - `encode_wire` — (a) `STANDARD.encode` + `serde_json::to_string` of the
//!   real `TerminalOutputWire` (what `emit_terminal_output` hands Tauri).
//! - `pipe_hop` — (b) the in-process hop the coord output pipe pays today:
//!   encode, the `String` clone the SSE `send` makes, `STANDARD.decode`, the
//!   pipe's real `redact_secrets`, and the re-encode. (The live pipe re-encodes
//!   a ≤16 KiB coalesced buffer at flush rather than each frame; base64 is
//!   linear, so per byte the cost is the same.) Sub-phase ns are reported too.
//! - `ws_json` — (c) the relay leg's wrapper: the encode, the inner
//!   `json!({"terminal_id","data"})` the reader builds, the outer
//!   `{"channel","payload"}` `broadcast_ws_notification` builds, and the
//!   `serde_json::to_string` the relay socket writer pays to put it on the
//!   wire.
//! - `raw_frame` — (d) the Phase 6 baseline: a 24-byte little-endian header
//!   (`start_offset`, `end_offset`, `ring_start_offset`) plus a copy of the
//!   bytes, no encoding.
//!
//! Corpus: generated deterministically here (seeded xorshift, no fixture
//! files) — DEC-2026 synchronized full-screen repaints (`?2026h`, cursor home,
//! per-row CUP, SGR-heavy 256-colour runs with box-drawing glyphs, `?2026l`)
//! at 120×40, 200×60 and 250×80, and a typing trace of 1–8 byte writes.
//!
//! This corpus is NOT the TypeScript side's. `scripts/tui-repaint-generator.mjs`
//! (which feeds `transportCost.bench.ts` and the perf harness) emits its own
//! repaints — 8 frames, truecolour SGR — so its frames differ in count, size
//! and escape density from these 24 seeded 256-colour/truecolour-mixed frames.
//! Cross-language `ns_per_frame` figures are therefore over different bytes
//! and must not be compared directly; compare `ns_per_byte`.

use std::hint::black_box;
use std::time::Instant;

use base64::{engine::general_purpose::STANDARD, Engine};

use super::session::TerminalOutputWire;

/// Frames per repaint corpus (each a distinct seeded screen).
const REPAINT_FRAMES: usize = 24;
/// Writes in the typing trace.
const TYPING_WRITES: usize = 4000;

/// xorshift64* — deterministic, dependency-free.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed.max(1))
    }
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

const GLYPHS: [&str; 12] = ["─", "│", "╭", "╮", "╰", "╯", "●", "✻", "·", "›", "…", "⏺"];

/// One synchronized full-screen repaint of `cols`×`rows` visible cells.
fn repaint_frame(cols: usize, rows: usize, rng: &mut Rng) -> Vec<u8> {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(cols * rows * 6);
    s.push_str("\x1b[?2026h\x1b[H");
    for row in 1..=rows {
        let _ = write!(s, "\x1b[{row};1H");
        let mut col = 0;
        while col < cols {
            let run = (1 + rng.below(8) as usize).min(cols - col);
            let fg = rng.below(256);
            match rng.below(4) {
                0 => {
                    let _ = write!(s, "\x1b[0;38;5;{fg}m");
                }
                1 => {
                    let bg = rng.below(256);
                    let _ = write!(s, "\x1b[0;1;38;5;{fg};48;5;{bg}m");
                }
                2 => {
                    let _ = write!(s, "\x1b[2;38;5;{fg}m");
                }
                _ => {
                    let (r, g, b) = (rng.below(256), rng.below(256), rng.below(256));
                    let _ = write!(s, "\x1b[0;38;2;{r};{g};{b}m");
                }
            }
            for _ in 0..run {
                match rng.below(10) {
                    0 => s.push(' '),
                    1 => s.push_str(GLYPHS[rng.below(GLYPHS.len() as u64) as usize]),
                    _ => s.push((b'!' + rng.below(94) as u8) as char),
                }
            }
            col += run;
        }
    }
    s.push_str("\x1b[0m\x1b[?2026l");
    s.into_bytes()
}

/// A typing trace: `TYPING_WRITES` writes of 1–8 bytes (echoed keystrokes,
/// the odd CR/LF and erase-to-EOL).
fn typing_trace(rng: &mut Rng) -> Vec<Vec<u8>> {
    (0..TYPING_WRITES)
        .map(|_| {
            let len = 1 + rng.below(8) as usize;
            let mut w: Vec<u8> = (0..len).map(|_| b' ' + rng.below(95) as u8).collect();
            match rng.below(16) {
                0 => w = b"\r\n".to_vec(),
                1 => w = b"\x1b[K".to_vec(),
                _ => {}
            }
            w
        })
        .collect()
}

struct Corpus {
    name: &'static str,
    frames: Vec<Vec<u8>>,
}

fn corpora() -> Vec<Corpus> {
    let mut out = Vec::new();
    for (name, cols, rows, seed) in [
        ("repaint_120x40", 120, 40, 0x1200_0040_u64),
        ("repaint_200x60", 200, 60, 0x2000_0060),
        ("repaint_250x80", 250, 80, 0x2500_0080),
    ] {
        let mut rng = Rng::new(seed);
        out.push(Corpus {
            name,
            frames: (0..REPAINT_FRAMES)
                .map(|_| repaint_frame(cols, rows, &mut rng))
                .collect(),
        });
    }
    out.push(Corpus {
        name: "typing_1to8",
        frames: typing_trace(&mut Rng::new(0x7E57)),
    });
    out
}

/// The build profile the numbers came from. `cfg!(debug_assertions)` alone
/// cannot say: the runner's test-support code needs `debug_assertions`, so an
/// optimized run is `--release` with `CARGO_PROFILE_RELEASE_DEBUG_ASSERTIONS=true`.
/// The invoker labels it through `TRANSPORT_COST_PROFILE`; unlabelled runs say
/// so rather than guess.
fn profile() -> String {
    std::env::var("TRANSPORT_COST_PROFILE").unwrap_or_else(|_| "unlabelled".to_string())
}

/// Run `f` over every frame for enough rounds to accumulate ~`budget_bytes`,
/// after one warm-up round. Returns (frames processed, bytes processed, ns).
fn measure(corpus: &Corpus, mut f: impl FnMut(&[u8], u64)) -> (u64, u64, u64) {
    let corpus_bytes: u64 = corpus.frames.iter().map(|f| f.len() as u64).sum();
    let budget_bytes: u64 = 64 << 20;
    let rounds = (budget_bytes / corpus_bytes.max(1)).clamp(3, 2_000);
    for (i, frame) in corpus.frames.iter().enumerate() {
        f(frame, i as u64);
    }
    let start = Instant::now();
    let mut offset = 0u64;
    for _ in 0..rounds {
        for frame in &corpus.frames {
            f(frame, offset);
            offset += frame.len() as u64;
        }
    }
    let ns = u64::try_from(start.elapsed().as_nanos()).unwrap_or(u64::MAX);
    (rounds * corpus.frames.len() as u64, rounds * corpus_bytes, ns)
}

fn report(bench: &str, corpus: &Corpus, (frames, bytes, ns): (u64, u64, u64), extra: serde_json::Value) {
    let mut line = serde_json::json!({
        "bench": bench,
        "corpus": corpus.name,
        "profile": profile(),
        "debug_assertions": cfg!(debug_assertions),
        "frames": frames,
        "bytes": bytes,
        "ns_per_frame": ns as f64 / frames.max(1) as f64,
        "ns_per_byte": ns as f64 / bytes.max(1) as f64,
    });
    if let (Some(obj), serde_json::Value::Object(more)) = (line.as_object_mut(), extra) {
        obj.extend(more);
    }
    println!("{line}");
}

#[test]
#[ignore = "measurement — run with --ignored --nocapture"]
fn bench_encode_wire() {
    for corpus in corpora() {
        let r = measure(&corpus, |frame, offset| {
            let encoded = STANDARD.encode(frame);
            let wire = TerminalOutputWire {
                terminal_id: "3f2b8c1e-5d4a-4e6f-9a7b-0c1d2e3f4a5b",
                data: &encoded,
                offset,
            };
            black_box(serde_json::to_string(&wire).unwrap());
        });
        report("encode_wire", &corpus, r, serde_json::json!({}));
    }
}

#[test]
#[ignore = "measurement — run with --ignored --nocapture"]
fn bench_pipe_hop() {
    for corpus in corpora() {
        // `measure` runs one untimed warm-up round first (it also pays the
        // redaction regexes' lazy compile); sub-phase laps skip it so they sum
        // to the timed total.
        let warmup = corpus.frames.len();
        let mut calls = 0usize;
        let mut phase_ns = [0u64; 5];
        let r = measure(&corpus, |frame, _| {
            let timed = calls >= warmup;
            calls += 1;
            let mut lap = |i: usize, t: &mut Instant| {
                let now = Instant::now();
                if timed {
                    phase_ns[i] += u64::try_from((now - *t).as_nanos()).unwrap_or(0);
                }
                *t = now;
            };
            let mut t = Instant::now();
            let encoded = STANDARD.encode(frame);
            lap(0, &mut t);
            let sent = encoded.clone();
            lap(1, &mut t);
            let raw = STANDARD.decode(sent.as_bytes()).unwrap();
            lap(2, &mut t);
            let redacted = crate::session::redact::redact_secrets(&raw);
            lap(3, &mut t);
            black_box(STANDARD.encode(&redacted));
            lap(4, &mut t);
            black_box(encoded);
        });
        let per = r.0.max(1) as f64;
        report(
            "pipe_hop",
            &corpus,
            r,
            serde_json::json!({
                "encode_ns_per_frame": phase_ns[0] as f64 / per,
                "clone_ns_per_frame": phase_ns[1] as f64 / per,
                "decode_ns_per_frame": phase_ns[2] as f64 / per,
                "redact_ns_per_frame": phase_ns[3] as f64 / per,
                "reencode_ns_per_frame": phase_ns[4] as f64 / per,
            }),
        );
    }
}

#[test]
#[ignore = "measurement — run with --ignored --nocapture"]
fn bench_ws_json() {
    for corpus in corpora() {
        let r = measure(&corpus, |frame, _| {
            let encoded = STANDARD.encode(frame);
            let payload = serde_json::json!({
                "terminal_id": "3f2b8c1e-5d4a-4e6f-9a7b-0c1d2e3f4a5b",
                "data": &encoded,
            });
            let ws_event = serde_json::json!({
                "channel": "terminal-output",
                "payload": &payload,
            });
            black_box(serde_json::to_string(&ws_event).unwrap());
        });
        report("ws_json", &corpus, r, serde_json::json!({}));
    }
}

#[test]
#[ignore = "measurement — run with --ignored --nocapture"]
fn bench_raw_frame() {
    for corpus in corpora() {
        let r = measure(&corpus, |frame, offset| {
            let mut buf = Vec::with_capacity(24 + frame.len());
            buf.extend_from_slice(&offset.to_le_bytes());
            buf.extend_from_slice(&(offset + frame.len() as u64).to_le_bytes());
            buf.extend_from_slice(&0u64.to_le_bytes());
            buf.extend_from_slice(frame);
            black_box(buf);
        });
        report("raw_frame", &corpus, r, serde_json::json!({}));
    }
}

/// The corpus the benches run on is what this module says it is: seeded and
/// reproducible, bracketed repaints of exactly cols×rows visible cells, and a
/// typing trace of 1–8 byte writes. Not ignored, so the module is never
/// vacuous under the anti-vacuity guard.
#[test]
fn corpus_generator_produces_the_documented_shapes() {
    let a = corpora();
    let b = corpora();
    assert_eq!(a.len(), 4);
    for (x, y) in a.iter().zip(b.iter()) {
        assert_eq!(x.name, y.name);
        assert_eq!(x.frames, y.frames, "{} is not deterministic", x.name);
    }

    for (corpus, cells) in a.iter().take(3).zip([120 * 40, 200 * 60, 250 * 80]) {
        assert_eq!(corpus.frames.len(), REPAINT_FRAMES, "{}", corpus.name);
        for frame in &corpus.frames {
            assert!(frame.starts_with(b"\x1b[?2026h\x1b[H"), "{}", corpus.name);
            assert!(frame.ends_with(b"\x1b[0m\x1b[?2026l"), "{}", corpus.name);
            let visible = crate::terminal::strip_ansi(std::str::from_utf8(frame).unwrap())
                .chars()
                .count();
            assert_eq!(visible, cells, "{} visible cells", corpus.name);
            // SGR-heavy: well over one escape byte per visible cell, and
            // never beyond the 256 KiB sync-flush frame cap.
            assert!(frame.len() > cells * 3, "{} too sparse: {}", corpus.name, frame.len());
            assert!(frame.len() < 256 * 1024, "{} over the frame cap", corpus.name);
        }
    }
    // The two larger screens straddle the relay's 49 152-byte hazard; the
    // smallest does not always — which is the point of measuring all three.
    let over = |i: usize| a[i].frames.iter().all(|f| f.len() > 49_152);
    assert!(over(1) && over(2), "200x60 and 250x80 repaints exceed 49152 bytes");

    let typing = &a[3];
    assert_eq!(typing.name, "typing_1to8");
    assert_eq!(typing.frames.len(), TYPING_WRITES);
    assert!(typing.frames.iter().all(|w| (1..=8).contains(&w.len())));
}

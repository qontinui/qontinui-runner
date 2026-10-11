//! Differential test: this crate's `codec.pixel` decoder and span renderer
//! against the reference implementation, `scripts/termhost-bench/marker-raster.mjs`,
//! run in node on the same randomly generated rows.
//!
//! The golden rows pin each rule once; this sweeps the space between them —
//! markers at random cell widths (including narrower and wider than the
//! supported range), blur and sub-pixel phase, damaged and recoloured cells,
//! irregular widths, impure slivers, clipped rows, colour-managed palettes,
//! two markers to a row, random content around them — and requires the two
//! implementations to agree on every pixel of every rendered row and on every
//! result of every row. Seeded, so a failure reproduces; `TERMHOST_DIFF_SEED`
//! and `TERMHOST_DIFF_CASES` explore further.
//!
//! node runs the reference module directly (no install). Where node cannot be
//! run the test says so and passes — except under CI (`CI` set, as on every
//! GitHub-hosted runner), where a missing node fails it rather than letting
//! the comparison silently not happen.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{json, Value};
use termhost_probe::codec::{
    encode_rgb, Marker, MarkerKind, Rejection, CELLS, PALETTE, SEQ_MODULUS, SESSION_LIMIT,
};
use termhost_probe::raster::synth::{encode_rle, render_spans, Span};
use termhost_probe::raster::{classify, find_markers_in_row, Class, DecodeParams};
use termhost_probe::spec::spec;

/// Reads JSON lines `{spans, blur}` on stdin; writes `{rle, expected}` per line.
const NODE_SCRIPT: &str = r#"
import { createInterface } from "node:readline";
import { pathToFileURL } from "node:url";
const raster = await import(pathToFileURL(process.env.TERMHOST_RASTER_MJS).href);
const summarize = (r) =>
  r.ok ? { ok: true, kind: r.kind, session: r.session, seq: r.seq } : { ok: false, reason: r.reason };
const out = [];
for await (const line of createInterface({ input: process.stdin, crlfDelay: Infinity })) {
  if (line.trim() === "") continue;
  const c = JSON.parse(line);
  const pixels = raster.renderSpans(c.spans, { blur: c.blur });
  out.push(JSON.stringify({ rle: raster.encodeRle(pixels), expected: raster.findMarkersInRow(pixels).map(summarize) }));
}
process.stdout.write(out.join("\n") + "\n");
"#;

/// splitmix64.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    fn chance(&mut self, p: f64) -> bool {
        self.unit() < p
    }

    fn range(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * self.unit()
    }

    fn rgb(&mut self) -> [u8; 3] {
        [
            self.below(256) as u8,
            self.below(256) as u8,
            self.below(256) as u8,
        ]
    }
}

#[derive(Clone, Serialize)]
struct SpanJson {
    width: f64,
    rgb: [u8; 3],
}

#[derive(Clone, Serialize)]
struct Case {
    spans: Vec<SpanJson>,
    blur: f64,
}

const GREY: [u8; 3] = [128, 128, 128];

fn background(rng: &mut Rng) -> [u8; 3] {
    match rng.below(6) {
        0 => GREY,
        1 => [24, 24, 24],
        2 => [0, 0, 0],
        3 => [255, 255, 255],
        4 => PALETTE[rng.below(8) as usize].rgb,
        _ => rng.rgb(),
    }
}

fn push(spans: &mut Vec<SpanJson>, width: f64, rgb: [u8; 3]) {
    spans.push(SpanJson { width, rgb });
}

/// Random colours in short spans: mostly impure, sometimes pure (a red then
/// cyan among them is a candidate start of its own).
fn content(rng: &mut Rng, spans: &mut Vec<SpanJson>, width: f64) {
    let mut left = width;
    while left > 0.0 {
        let w = left.min(rng.range(0.5, 4.0));
        let rgb = rng.rgb();
        push(spans, w, rgb);
        left -= w;
    }
}

fn pad(rng: &mut Rng, spans: &mut Vec<SpanJson>, max: f64) {
    let width = rng.range(0.25, max);
    if rng.chance(0.3) {
        content(rng, spans, width);
    } else {
        let rgb = background(rng);
        push(spans, width, rgb);
    }
}

/// One marker, possibly damaged; false when the row ends inside it.
fn marker(rng: &mut Rng, spans: &mut Vec<SpanJson>) -> bool {
    let m = Marker::new(
        MarkerKind::from_digit(rng.below(3) as u8).expect("kind"),
        rng.below(u64::from(SESSION_LIMIT)) as u16,
        rng.below(u64::from(SEQ_MODULUS)) as u32,
    );
    let mut colours = encode_rgb(m).to_vec();
    let w = rng.range(3.0, 32.0);
    let mut widths = [w; CELLS];
    if rng.chance(0.15) {
        let i = rng.below(CELLS as u64) as usize;
        colours[i] = PALETTE[rng.below(8) as usize].rgb;
    }
    if rng.chance(0.08) {
        let i = rng.below(CELLS as u64) as usize;
        colours[i] = rng.rgb();
    }
    if rng.chance(0.15) {
        let i = rng.below(CELLS as u64) as usize;
        widths[i] = w * rng.range(0.2, 2.6);
    }
    let shift = rng
        .chance(0.2)
        .then(|| (rng.below(90) as u8, 255 - rng.below(90) as u8));
    let keep = if rng.chance(0.08) {
        1 + rng.below(CELLS as u64 - 1) as usize
    } else {
        CELLS
    };
    for i in 0..keep {
        let rgb = match shift {
            Some((lo, hi)) => colours[i].map(|c| match c {
                0 => lo,
                255 => hi,
                other => other,
            }),
            None => colours[i],
        };
        push(spans, widths[i], rgb);
        if rng.chance(0.02) {
            let sliver = rng.range(0.3, 4.0);
            let rgb = if rng.chance(0.5) { GREY } else { rng.rgb() };
            push(spans, sliver, rgb);
        }
    }
    keep == CELLS
}

/// Rows no golden row pins, run first: rule 6's `min(k, need)` (found by the
/// qontinui-runner#2154 pre-PR review, round 3 finding 1) — an impure stretch
/// running past the marker's last cell is `off_palette`, never
/// `wrong_length`. The same rows are asserted without node in
/// `tests/golden.rs`.
fn pinned_cases() -> Vec<Case> {
    let mid = Marker::new(MarkerKind::Output, 14, 1234);
    let cells = |n: usize| -> Vec<SpanJson> {
        encode_rgb(mid)
            .iter()
            .take(n)
            .map(|&rgb| SpanJson { width: 10.0, rgb })
            .collect()
    };
    let mut a = vec![SpanJson {
        width: 16.0,
        rgb: GREY,
    }];
    a.extend(cells(17));
    a.push(SpanJson {
        width: 35.0,
        rgb: [135, 0, 0],
    });
    let mut b = vec![SpanJson {
        width: 16.0,
        rgb: GREY,
    }];
    b.extend(cells(15));
    b.push(SpanJson {
        width: 60.0,
        rgb: GREY,
    });
    b.push(SpanJson {
        width: 20.0,
        rgb: [0, 0, 0],
    });
    vec![
        Case {
            spans: a,
            blur: 0.0,
        },
        Case {
            spans: b,
            blur: 0.0,
        },
    ]
}

fn case(rng: &mut Rng) -> Case {
    let mut spans = Vec::new();
    if rng.chance(0.9) {
        pad(rng, &mut spans, 24.0);
    }
    let markers = if rng.chance(0.25) { 2 } else { 1 };
    for k in 0..markers {
        if !marker(rng, &mut spans) {
            break;
        }
        if k + 1 < markers || rng.chance(0.9) {
            pad(rng, &mut spans, 40.0);
        }
    }
    let blur = match rng.below(8) {
        0..=2 => 0.0,
        3 => 0.5,
        4 => 1.0,
        5 => 2.0,
        6 => rng.range(0.0, 3.0),
        _ => rng.below(9) as f64 * 0.25,
    };
    Case { spans, blur }
}

fn summarize(result: &Result<Marker, Rejection>) -> Value {
    match result {
        Ok(m) => json!({"ok": true, "kind": m.kind.digit(), "session": m.session, "seq": m.seq}),
        Err(r) => json!({"ok": false, "reason": r.as_str()}),
    }
}

fn env_u64(name: &str) -> Option<u64> {
    let v = std::env::var(name).ok()?;
    let v = v.trim();
    match v.strip_prefix("0x") {
        Some(hex) => u64::from_str_radix(hex, 16).ok(),
        None => v.parse().ok(),
    }
}

/// How long the reference may take (it needs about a second).
const NODE_LIMIT: Duration = Duration::from_secs(120);

/// Wait for `child` at most `limit`, reading its output meanwhile, and kill
/// it on timeout: a node that never exits must not hold the test until the
/// CI job's own timeout.
fn wait_with_deadline(mut child: Child, limit: Duration) -> Result<Output, String> {
    let deadline = Instant::now() + limit;
    let mut out = child.stdout.take().ok_or("stdout was not captured")?;
    let mut err = child.stderr.take().ok_or("stderr was not captured")?;
    let out_reader = std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = out.read_to_end(&mut b);
        b
    });
    let err_reader = std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = err.read_to_end(&mut b);
        b
    });
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                return Ok(Output {
                    status,
                    stdout: out_reader.join().unwrap_or_default(),
                    stderr: err_reader.join().unwrap_or_default(),
                })
            }
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "the child did not finish within {} s and was killed",
                    limit.as_secs()
                ));
            }
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("waiting for the child: {e}"));
            }
        }
    }
}

/// `node` started with `args`, or `None` (with the skip rule) when node
/// cannot be run here.
fn spawn_node(args: &[&str], envs: &[(&str, &std::path::Path)]) -> Option<Child> {
    let mut cmd = Command::new("node");
    cmd.args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in envs {
        cmd.env(k, v);
    }
    match cmd.spawn() {
        Ok(c) => Some(c),
        Err(e) => {
            assert!(
                std::env::var_os("CI").is_none(),
                "node is required for the differential test under CI, and did not start: {e}"
            );
            eprintln!("SKIPPED: node did not start ({e}); the decoder was NOT compared with the reference here");
            None
        }
    }
}

/// Run the reference on `cases`; `None` when node cannot be run here.
fn reference(cases: &[Case]) -> Option<Vec<Value>> {
    let module = std::path::absolute(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../scripts/termhost-bench/marker-raster.mjs"),
    )
    .expect("an absolute path to marker-raster.mjs");
    assert!(module.is_file(), "{} is missing", module.display());
    let mut child = spawn_node(
        &["--input-type=module", "-e", NODE_SCRIPT],
        &[("TERMHOST_RASTER_MJS", module.as_path())],
    )?;
    let mut input = String::new();
    for c in cases {
        input.push_str(&serde_json::to_string(c).expect("case JSON"));
        input.push('\n');
    }
    let mut stdin = child.stdin.take().expect("node stdin");
    let writer = std::thread::spawn(move || stdin.write_all(input.as_bytes()));
    let output = wait_with_deadline(child, NODE_LIMIT).expect("the reference finished");
    writer
        .join()
        .expect("the stdin writer")
        .expect("cases written to node");
    assert!(
        output.status.success(),
        "the reference failed ({}): {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    let lines: Vec<Value> = String::from_utf8(output.stdout)
        .expect("node wrote UTF-8")
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("node wrote JSON"))
        .collect();
    assert_eq!(lines.len(), cases.len(), "one answer per case");
    Some(lines)
}

#[test]
fn a_child_that_never_exits_is_killed_at_the_deadline() {
    let Some(child) = spawn_node(&["-e", "setInterval(() => {}, 1000)"], &[]) else {
        return;
    };
    let started = Instant::now();
    let r = wait_with_deadline(child, Duration::from_millis(500));
    assert!(r.is_err(), "{r:?}");
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "returned after {:?}",
        started.elapsed()
    );
}

#[test]
fn the_decoder_and_renderer_agree_with_the_reference_on_random_rows() {
    let seed = env_u64("TERMHOST_DIFF_SEED").unwrap_or(0x7E57_C0DE_D1FF);
    let n = env_u64("TERMHOST_DIFF_CASES").unwrap_or(1500) as usize;
    let mut rng = Rng(seed);
    let mut cases = pinned_cases();
    let pinned = cases.len();
    cases.extend((0..n).map(|_| case(&mut rng)));
    let n = cases.len();
    let Some(answers) = reference(&cases) else {
        return;
    };
    // The pinned rows' reference answers, so the fixture's claim is re-checked
    // against the reference itself on every run (not only against this crate).
    for (c, answer) in cases.iter().zip(&answers).take(pinned) {
        assert_eq!(
            answer["expected"],
            json!([{"ok": false, "reason": "off_palette"}]),
            "the reference on a pinned row: {}",
            serde_json::to_string(c).unwrap_or_default()
        );
    }
    let params = DecodeParams::default();
    let p = spec().pixel;
    let mut failures = Vec::new();
    let (mut accepted, mut rejected) = (0usize, 0usize);
    let mut reasons = std::collections::BTreeSet::new();
    for (i, (c, answer)) in cases.iter().zip(&answers).enumerate() {
        let spans: Vec<Span> = c.spans.iter().map(|s| Span::new(s.width, s.rgb)).collect();
        let pixels = render_spans(&spans, c.blur);
        let rle: Vec<Value> = encode_rle(&pixels).iter().map(|r| json!(r)).collect();
        if Value::Array(rle) != answer["rle"] {
            failures.push(format!(
                "case {i}: rendered pixels differ; case {}",
                serde_json::to_string(c).unwrap_or_default()
            ));
            continue;
        }
        let classes: Vec<Class> = pixels
            .iter()
            .map(|&[r, g, b]| classify(&p, r, g, b))
            .collect();
        let got: Vec<Value> = find_markers_in_row(&classes, &params)
            .iter()
            .map(|rm| {
                match rm.result {
                    Ok(_) => accepted += 1,
                    Err(r) => {
                        rejected += 1;
                        reasons.insert(r.as_str());
                    }
                }
                summarize(&rm.result)
            })
            .collect();
        if Value::Array(got.clone()) != answer["expected"] {
            failures.push(format!(
                "case {i}: expected {}, got {}; case {}",
                answer["expected"],
                Value::Array(got),
                serde_json::to_string(c).unwrap_or_default()
            ));
        }
    }
    let shown: Vec<&String> = failures.iter().take(5).collect();
    assert!(
        failures.is_empty(),
        "{} of {n} rows differ from the reference (seed {seed:#x}); first:\n{}",
        failures.len(),
        shown
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    );
    // The corpus exercised the decoder rather than agreeing on empty lists.
    eprintln!("{n} rows agree: {accepted} markers accepted, {rejected} candidates rejected, reasons {reasons:?}");
    if n >= 1000 {
        assert!(
            accepted >= n / 4,
            "only {accepted} accepted markers in {n} rows"
        );
        assert!(
            rejected >= n / 10,
            "only {rejected} rejected candidates in {n} rows"
        );
        assert!(reasons.len() >= 5, "only reasons {reasons:?}");
    }
}

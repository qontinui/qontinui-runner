//! The contract with Phase 1: `scripts/termhost-bench/marker-golden.json`.
//!
//! - Every `valid` vector encodes to exactly its digits / values / indices /
//!   rgb / overlay bytes and decodes back to its (kind, session, seq), and
//!   every `invalid` vector is rejected with exactly its `reason` (cell level).
//! - Every `rows` raster vector — a pixel row drawn by the reference renderer,
//!   with the reference decoder's results — gives exactly its `expected` list
//!   through this crate's pixel decoder (`codec.pixel` rules 1-9): the decoded
//!   values on accepted rows, the exact reason on rejected ones, and nothing on
//!   a `[]` row. The frame scanner `watch` runs must agree.
//! - The crate's port of the reference renderer redraws the rows built from
//!   plain spans byte for byte, so the synthetic rasters the other tests draw
//!   are the reference's.
//!
//! The file is read from the repo, not a copy, so a regenerated golden file is
//! tested the moment it lands.

use std::collections::BTreeSet;
use std::path::PathBuf;

use serde_json::{json, Value};
use termhost_probe::codec::{
    cells_sgr, decode_indices, decode_values, encode_digits, encode_indices, encode_rgb,
    encode_values, overlay_bytes, Marker, MarkerKind, Rejection, BASE, CELLS, CHECKSUM_DIGITS,
    CHECKSUM_MODULUS, CHECKSUM_MULTIPLIER, PALETTE, SEQ_MODULUS, SESSION_LIMIT,
};
use termhost_probe::raster::synth::{decode_rle, encode_rle, render_spans, Canvas, Span};
use termhost_probe::raster::{
    classify, find_markers_in_row, scan, Class, DecodeParams, ScanOptions,
};
use termhost_probe::spec::{spec, CodecSpec, GOLDEN_JSON};

fn golden_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../scripts/termhost-bench/marker-golden.json")
}

fn golden() -> Value {
    let text = std::fs::read_to_string(golden_path()).expect("marker-golden.json is readable");
    serde_json::from_str(&text).expect("marker-golden.json is JSON")
}

fn u64s(v: &Value) -> Vec<u64> {
    v.as_array()
        .expect("array")
        .iter()
        .map(|x| x.as_u64().expect("integer"))
        .collect()
}

fn rgb_cells(v: &Value) -> Vec<[u8; 3]> {
    v.as_array()
        .expect("rgb array")
        .iter()
        .map(|c| {
            let c = u64s(c);
            [c[0] as u8, c[1] as u8, c[2] as u8]
        })
        .collect()
}

fn kind_of(n: u64) -> MarkerKind {
    MarkerKind::from_digit(n as u8).expect("a valid vector's kind")
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A golden reason string -> the decoder's rejection (row stage or cell stage).
fn reason_of(name: &str) -> Rejection {
    *Rejection::ROW_REASONS
        .iter()
        .chain(Rejection::DECODE_ORDER.iter())
        .find(|r| r.as_str() == name)
        .unwrap_or_else(|| panic!("unknown golden reason {name}"))
}

/// A decoder result in the golden file's shape (`marker-golden.mjs`
/// `summarizeResult`), so expected and actual are compared as whole values.
fn summarize(result: &Result<Marker, Rejection>) -> Value {
    match result {
        Ok(m) => json!({"ok": true, "kind": m.kind.digit(), "session": m.session, "seq": m.seq}),
        Err(r) => json!({"ok": false, "reason": r.as_str()}),
    }
}

fn row_pixels(row: &Value) -> Vec<[u8; 3]> {
    let rle: Vec<[u32; 4]> = row["rle"]
        .as_array()
        .expect("rle")
        .iter()
        .map(|run| {
            let run = u64s(run);
            assert_eq!(run.len(), 4, "an RLE run is [r, g, b, count]");
            [run[0] as u32, run[1] as u32, run[2] as u32, run[3] as u32]
        })
        .collect();
    decode_rle(&rle)
}

fn classes_of(pixels: &[[u8; 3]]) -> Vec<Class> {
    let p = spec().pixel;
    pixels
        .iter()
        .map(|&[r, g, b]| classify(&p, r, g, b))
        .collect()
}

#[test]
fn the_embedded_copy_is_the_repo_file() {
    let on_disk = std::fs::read_to_string(golden_path()).expect("readable");
    assert_eq!(
        on_disk, GOLDEN_JSON,
        "the binary must embed the current golden file"
    );
}

#[test]
fn codec_block_matches_the_rust_constants() {
    let g = golden();
    let c = &g["codec"];
    assert_eq!(c["cells"].as_u64(), Some(CELLS as u64));
    assert_eq!(c["base"].as_u64(), Some(u64::from(BASE)));
    assert_eq!(c["session_limit"].as_u64(), Some(u64::from(SESSION_LIMIT)));
    assert_eq!(c["seq_modulus"].as_u64(), Some(u64::from(SEQ_MODULUS)));
    assert_eq!(
        c["checksum"]["modulus"].as_u64(),
        Some(u64::from(CHECKSUM_MODULUS))
    );
    assert_eq!(
        c["checksum"]["multiplier"].as_u64(),
        Some(u64::from(CHECKSUM_MULTIPLIER))
    );
    assert_eq!(
        c["checksum"]["digits"].as_u64(),
        Some(CHECKSUM_DIGITS as u64)
    );
    assert_eq!(c["version"].as_u64(), Some(spec().version));
    let order: Vec<&str> = c["decode_order"]
        .as_array()
        .expect("decode_order")
        .iter()
        .map(|v| v.as_str().expect("reason"))
        .collect();
    let ours: Vec<&str> = Rejection::DECODE_ORDER.iter().map(|r| r.as_str()).collect();
    assert_eq!(order, ours);
    let palette = c["palette"].as_array().expect("palette");
    assert_eq!(palette.len(), PALETTE.len());
    for (entry, ours) in palette.iter().zip(PALETTE) {
        assert_eq!(entry["value"].as_u64(), Some(u64::from(ours.value)));
        assert_eq!(entry["index"].as_u64(), Some(u64::from(ours.index)));
        assert_eq!(
            rgb_cells(&Value::Array(vec![entry["rgb"].clone()]))[0],
            ours.rgb
        );
        assert_eq!(entry["name"].as_str(), Some(ours.name));
    }
    for (name, kind) in [
        ("output", MarkerKind::Output),
        ("echo", MarkerKind::Echo),
        ("calibration", MarkerKind::Calibration),
    ] {
        assert_eq!(
            c["kinds"][name].as_u64(),
            Some(u64::from(kind.digit())),
            "{name}"
        );
    }
}

#[test]
fn pixel_parameters_come_from_the_golden_codec_block() {
    let g = golden();
    let px = &g["codec"]["pixel"];
    let s = CodecSpec::from_golden(&std::fs::read_to_string(golden_path()).expect("readable"))
        .expect("parses");
    assert_eq!(&s, spec());
    assert_eq!(
        px["channel_low_max"].as_u64(),
        Some(u64::from(s.pixel.channel_low_max))
    );
    assert_eq!(
        px["channel_high_min"].as_u64(),
        Some(u64::from(s.pixel.channel_high_min))
    );
    assert_eq!(
        px["width_tolerance"].as_f64(),
        Some(s.pixel.width_tolerance)
    );
    assert_eq!(
        px["boundary_gap_max"].as_f64(),
        Some(s.pixel.boundary_gap_max)
    );
    let row_reasons: Vec<&str> = Rejection::ROW_REASONS.iter().map(|r| r.as_str()).collect();
    assert_eq!(s.row_reasons, row_reasons);
    // The rules are text the decoder implements; there must be nine of them.
    assert_eq!(px["rules"].as_array().map(Vec::len), Some(9));
    assert_eq!(DecodeParams::default().pixel, s.pixel);
}

#[test]
fn every_valid_vector_encodes_and_decodes_exactly() {
    let g = golden();
    let region = &g["codec"]["region"];
    let echo = &g["codec"]["echo_region"];
    let valid = g["valid"].as_array().expect("valid");
    assert!(!valid.is_empty());
    for v in valid {
        let name = v["name"].as_str().expect("name");
        let m = Marker::new(
            kind_of(v["kind"].as_u64().expect("kind")),
            v["session"].as_u64().expect("session") as u16,
            v["seq"].as_u64().expect("seq") as u32,
        );
        let digits: Vec<u64> = encode_digits(m).iter().map(|&d| u64::from(d)).collect();
        assert_eq!(digits, u64s(&v["digits"]), "{name}: digits");
        let values: Vec<u64> = encode_values(m).iter().map(|&d| u64::from(d)).collect();
        assert_eq!(values, u64s(&v["values"]), "{name}: values");
        let indices: Vec<u64> = encode_indices(m).iter().map(|&d| u64::from(d)).collect();
        assert_eq!(indices, u64s(&v["indices"]), "{name}: indices");
        assert_eq!(encode_rgb(m).to_vec(), rgb_cells(&v["rgb"]), "{name}: rgb");
        assert_eq!(
            hex(cells_sgr(&encode_indices(m)).as_bytes()),
            v["cells_hex"].as_str().expect("cells_hex"),
            "{name}: cells_hex"
        );
        let row = region["row"].as_u64().expect("row") as u16;
        let col = region["col"].as_u64().expect("col") as u16;
        assert_eq!(
            hex(&overlay_bytes(m, row, col)),
            v["overlay_hex"].as_str().expect("overlay"),
            "{name}"
        );
        let erow = echo["row"].as_u64().expect("row") as u16;
        let ecol = echo["col"].as_u64().expect("col") as u16;
        assert_eq!(
            hex(&overlay_bytes(m, erow, ecol)),
            v["echo_overlay_hex"].as_str().expect("echo overlay"),
            "{name}"
        );

        let vals: Vec<u8> = u64s(&v["values"]).iter().map(|&x| x as u8).collect();
        assert_eq!(decode_values(&vals), Ok(m), "{name}: decode values");
        let idx: Vec<u16> = u64s(&v["indices"]).iter().map(|&x| x as u16).collect();
        assert_eq!(decode_indices(&idx), Ok(m), "{name}: decode indices");
    }
}

#[test]
fn every_invalid_vector_is_rejected_with_its_reason_at_cell_level() {
    let g = golden();
    let invalid = g["invalid"].as_array().expect("invalid");
    assert!(!invalid.is_empty());
    let mut reasons_seen = BTreeSet::new();
    for v in invalid {
        let name = v["name"].as_str().expect("name");
        let want = reason_of(v["reason"].as_str().expect("reason"));
        reasons_seen.insert(want.as_str());
        let idx: Vec<u16> = u64s(&v["indices"]).iter().map(|&x| x as u16).collect();
        assert_eq!(decode_indices(&idx), Err(want), "{name}");
    }
    // The golden file carries a vector for every cell check.
    assert_eq!(reasons_seen.len(), Rejection::DECODE_ORDER.len());
}

/// Every raster row the golden file holds, however many: the count is Phase
/// 1's to grow, so none is written here. An emptied file still fails — below,
/// on the rows having to pin both row-stage reasons.
#[test]
fn every_golden_row_decodes_to_exactly_its_expected_list() {
    let g = golden();
    let rows = g["rows"].as_array().expect("rows");
    assert!(!rows.is_empty(), "marker-golden.json holds no raster rows");
    let params = DecodeParams::default();
    let mut failures = Vec::new();
    let mut reasons_pinned = BTreeSet::new();
    for (i, row) in rows.iter().enumerate() {
        let name = row["name"].as_str().expect("name");
        let pixels = row_pixels(row);
        assert_eq!(
            Some(pixels.len() as u64),
            row["width_px"].as_u64(),
            "row {i} ({name}): width_px"
        );
        let expected = row["expected"].as_array().expect("expected").clone();
        for e in &expected {
            if let Some(r) = e["reason"].as_str() {
                reasons_pinned.insert(reason_of(r).as_str());
            }
        }
        let got: Vec<Value> = find_markers_in_row(&classes_of(&pixels), &params)
            .iter()
            .map(|rm| summarize(&rm.result))
            .collect();
        if got != expected {
            failures.push(format!(
                "row {i} ({name}): expected {}, got {}",
                Value::Array(expected),
                Value::Array(got)
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} golden rows differ:\n{}",
        failures.len(),
        rows.len(),
        failures.join("\n")
    );
    // The rows exercise both row-stage reasons, not only the cell checks.
    for r in Rejection::ROW_REASONS {
        assert!(
            reasons_pinned.contains(r.as_str()),
            "no golden row pins {}",
            r.as_str()
        );
    }
    eprintln!(
        "all {} golden rows decode to exactly their expected lists",
        rows.len()
    );
}

#[test]
fn the_frame_scanner_agrees_with_every_golden_row() {
    let g = golden();
    for (i, row) in g["rows"].as_array().expect("rows").iter().enumerate() {
        let name = row["name"].as_str().expect("name");
        let expected = row["expected"].as_array().expect("expected");
        let want_markers: Vec<Value> = expected
            .iter()
            .filter(|e| e["ok"] == json!(true))
            .cloned()
            .collect();
        let want_reasons: BTreeSet<&str> = expected
            .iter()
            .filter_map(|e| e["reason"].as_str())
            .collect();
        // The row repeated 16 px tall: a marker one cell high, as on screen.
        let canvas = Canvas::from_row(&row_pixels(row), 16);
        let result = scan(&canvas.view(), &ScanOptions::default());
        let got_markers: Vec<Value> = result
            .markers
            .iter()
            .map(|f| summarize(&Ok(f.marker)))
            .collect();
        assert_eq!(
            got_markers, want_markers,
            "row {i} ({name}): scanner markers"
        );
        for f in &result.markers {
            assert_eq!(
                (f.rect.y, f.rect.h),
                (0, 16),
                "row {i} ({name}): the marker spans every row"
            );
        }
        let got_reasons: BTreeSet<&str> =
            result.rejected.iter().map(|r| r.reason.as_str()).collect();
        if want_markers.is_empty() {
            assert_eq!(
                got_reasons, want_reasons,
                "row {i} ({name}): scanner rejections"
            );
        } else {
            assert!(
                got_reasons.is_subset(&want_reasons),
                "row {i} ({name}): scanner rejections {got_reasons:?}"
            );
        }
    }
}

const GREY: [u8; 3] = [128, 128, 128];
const DARK: [u8; 3] = [24, 24, 24];

/// `marker-golden.mjs` `cellSpans`: one span per cell.
fn cell_spans(m: Marker, w: f64, map: impl Fn(u8) -> u8) -> Vec<Span> {
    encode_rgb(m)
        .iter()
        .map(|rgb| Span::new(w, rgb.map(&map)))
        .collect()
}

fn padded(left: (f64, [u8; 3]), cells: Vec<Span>, right: (f64, [u8; 3])) -> Vec<Span> {
    let mut spans = vec![Span::new(left.0, left.1)];
    spans.extend(cells);
    spans.push(Span::new(right.0, right.1));
    spans
}

#[test]
fn the_renderer_port_redraws_the_golden_span_rows_byte_for_byte() {
    // The rows `marker-golden.mjs` builds from plain spans, rebuilt from its
    // construction; each must come out as the very RLE the golden file holds.
    let mid = Marker::new(MarkerKind::Output, 14, 1234);
    let same = |c: u8| c;
    let cases: Vec<(&str, Vec<Span>, f64)> = vec![
        (
            "valid, 10 px cells, dark pane background",
            padded((16.0, DARK), cell_spans(mid, 10.0, same), (16.0, DARK)),
            0.0,
        ),
        (
            "valid, 6 px cells (the smallest)",
            padded((12.0, GREY), cell_spans(mid, 6.0, same), (12.0, GREY)),
            0.0,
        ),
        (
            "valid, 13.333 px cells (133 % scaling), box-filtered edges",
            padded(
                (10.0, GREY),
                cell_spans(mid, 40.0 / 3.0, same),
                (10.5, GREY),
            ),
            0.0,
        ),
        (
            "valid, 30 px cells (the largest), 2 px blur",
            padded((20.0, GREY), cell_spans(mid, 30.0, same), (20.0, GREY)),
            2.0,
        ),
        (
            "valid, 7.5 px cells, 1 px blur",
            padded((9.0, GREY), cell_spans(mid, 7.5, same), (9.0, GREY)),
            1.0,
        ),
        (
            "colour management: 0 -> 20 and 255 -> 235 still decodes",
            padded(
                (16.0, GREY),
                cell_spans(mid, 10.0, |c| if c == 0 { 20 } else { 235 }),
                (16.0, GREY),
            ),
            0.0,
        ),
        (
            "palette rendered at cube level 175 instead of 255",
            padded(
                (16.0, DARK),
                cell_spans(mid, 10.0, |c| if c == 255 { 175 } else { c }),
                (16.0, DARK),
            ),
            0.0,
        ),
        (
            "valid, 6.5 px cells, 2 px blur, quarter-pixel phase",
            padded((10.25, GREY), cell_spans(mid, 6.5, same), (11.0, GREY)),
            2.0,
        ),
    ];
    let g = golden();
    let rows = g["rows"].as_array().expect("rows");
    for (name, spans, blur) in cases {
        let row = rows
            .iter()
            .find(|r| r["name"].as_str() == Some(name))
            .unwrap_or_else(|| {
                panic!("golden row {name:?} is gone (update this test with the generator)")
            });
        assert_eq!(row["blur_px"].as_f64(), Some(blur), "{name}: blur");
        let ours = render_spans(&spans, blur);
        let golden_rle: Vec<[u32; 4]> = encode_rle(&row_pixels(row));
        assert_eq!(encode_rle(&ours), golden_rle, "{name}");
    }
}

/// Draw cells as spans between grey pads (grey is impure, so nothing merges
/// with the background); a vector shorter than 20 cells ends the row, which is
/// how a clipped marker looks on screen.
fn vector_row(cells: &[[u8; 3]], w: f64, blur: f64) -> Vec<[u8; 3]> {
    let mut spans = vec![Span::new(16.0, GREY)];
    spans.extend(cells.iter().map(|&rgb| Span::new(w, rgb)));
    if cells.len() >= CELLS {
        spans.push(Span::new(16.0, GREY));
    }
    render_spans(&spans, blur)
}

#[test]
fn every_valid_vector_decodes_from_pixels() {
    let g = golden();
    let params = DecodeParams::default();
    for v in g["valid"].as_array().expect("valid") {
        let name = v["name"].as_str().expect("name");
        let want = Marker::new(
            kind_of(v["kind"].as_u64().expect("kind")),
            v["session"].as_u64().expect("session") as u16,
            v["seq"].as_u64().expect("seq") as u32,
        );
        let cells = rgb_cells(&v["rgb"]);
        for w in [6.0, 7.25, 9.0, 12.0, 40.0 / 3.0, 16.5, 22.75, 30.0] {
            for blur in [0.0, 1.0] {
                let pixels = vector_row(&cells, w, blur);
                let got: Vec<_> = find_markers_in_row(&classes_of(&pixels), &params)
                    .iter()
                    .map(|rm| rm.result)
                    .collect();
                assert_eq!(got, vec![Ok(want)], "{name} at {w} px, blur {blur}");
                let result = scan(
                    &Canvas::from_row(&pixels, 2 * w.ceil() as usize).view(),
                    &ScanOptions::default(),
                );
                let found: Vec<Marker> = result.markers.iter().map(|f| f.marker).collect();
                assert_eq!(found, vec![want], "{name} at {w} px, blur {blur}: scanner");
            }
        }
    }
}

#[test]
fn every_invalid_vector_is_rejected_from_pixels_never_misdecoded() {
    let g = golden();
    let params = DecodeParams::default();
    for v in g["invalid"].as_array().expect("invalid") {
        let name = v["name"].as_str().expect("name");
        let want = reason_of(v["reason"].as_str().expect("reason"));
        let cells = rgb_cells(&v["rgb"]);
        // Only a red-then-cyan start is a candidate (rule 5): a vector whose
        // start sentinel is damaged has NO candidate at its own origin.
        let starts = cells.len() >= 2 && cells[0] == PALETTE[4].rgb && cells[1] == PALETTE[3].rgb;
        for w in [6.0, 10.0, 12.5, 21.0] {
            for blur in [0.0, 1.0] {
                let found = find_markers_in_row(&classes_of(&vector_row(&cells, w, blur)), &params);
                assert!(
                    found.iter().all(|rm| rm.result.is_err()),
                    "{name} at {w} px was misdecoded: {found:?}"
                );
                let own: Vec<_> = found
                    .iter()
                    .filter(|rm| (rm.x0() - 16.0).abs() <= w / 2.0)
                    .collect();
                if starts {
                    assert_eq!(own.len(), 1, "{name} at {w} px, blur {blur}: {found:?}");
                    assert_eq!(own[0].result, Err(want), "{name} at {w} px, blur {blur}");
                } else {
                    assert!(
                        own.is_empty(),
                        "{name} at {w} px: a candidate without a start: {found:?}"
                    );
                }
            }
        }
    }
}

/// The marker most golden rows draw (`marker-golden.mjs` `mid`).
fn mid() -> Marker {
    Marker::new(MarkerKind::Output, 14, 1234)
}

/// Rule 6 appends `min(k, need)` impure cells — never past cell 19 — so an
/// occluding toast or tooltip that runs on past the marker's last cell is
/// `off_palette`, not `wrong_length`. No golden row pins this (Phase 1 round-3
/// review finding R3-1); these two rows were checked against the reference to
/// decode `off_palette`, and `tests/differential.rs` runs them through it on
/// every run.
fn rule_6_overflow_rows() -> Vec<(&'static str, Vec<Span>)> {
    let cells = |n: usize| -> Vec<Span> {
        encode_rgb(mid())
            .iter()
            .take(n)
            .map(|&rgb| Span::new(10.0, rgb))
            .collect()
    };
    let mut a = vec![Span::new(16.0, GREY)];
    a.extend(cells(17));
    a.push(Span::new(35.0, [135, 0, 0]));
    let mut b = vec![Span::new(16.0, GREY)];
    b.extend(cells(15));
    b.push(Span::new(60.0, GREY));
    b.push(Span::new(20.0, [0, 0, 0]));
    vec![
        (
            "cells 0-16 at 10 px, then 35 px of rgb(135,0,0) to the row end (k = 4 > need = 3)",
            a,
        ),
        (
            "cells 0-14, then a 60 px impure overlay, then black (k = 6 > need = 5)",
            b,
        ),
    ]
}

#[test]
fn an_impure_gap_never_contributes_more_cells_than_are_still_needed() {
    let params = DecodeParams::default();
    for (name, spans) in rule_6_overflow_rows() {
        let pixels = render_spans(&spans, 0.0);
        let got: Vec<Value> = find_markers_in_row(&classes_of(&pixels), &params)
            .iter()
            .map(|rm| summarize(&rm.result))
            .collect();
        assert_eq!(
            got,
            vec![json!({"ok": false, "reason": "off_palette"})],
            "{name}"
        );
        let result = scan(
            &Canvas::from_row(&pixels, 16).view(),
            &ScanOptions::default(),
        );
        assert!(result.markers.is_empty(), "{name}: {result:?}");
        let reasons: Vec<&str> = result.rejected.iter().map(|r| r.reason.as_str()).collect();
        assert_eq!(reasons, vec!["off_palette"], "{name}: scanner");
    }
}

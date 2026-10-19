//! The raster decoder against the distortions a real capture adds — cell sizes
//! 6-30 px, anti-aliased fractional edges, 100-200 % compositor scaling — and
//! against colour-altered markers, which must be REJECTED, never misdecoded
//! (plan Phase 2: "colour-altered markers, which must be rejected, not
//! misdecoded"; policy testing/a-test-must-be-able-to-fail).
//!
//! These sit on top of the golden rows (`tests/golden.rs`), which pin the exact
//! `codec.pixel` rules; here the same decoder meets the renderers real hosts
//! and compositors are: the reference span renderer swept across widths, blur
//! and sub-pixel phase, three edge-blending models, and resampling filters.

use termhost_probe::codec::{encode_rgb, encode_values, Marker, MarkerKind, CELLS, PALETTE};
use termhost_probe::raster::synth::{render_spans, Canvas, EdgeMode, Filter, Span};
use termhost_probe::raster::{
    classify, find_markers_in_row, scan, Class, DecodeParams, ScanOptions,
};
use termhost_probe::spec::spec;

fn markers() -> Vec<Marker> {
    vec![
        Marker::new(MarkerKind::Output, 0, 0),
        Marker::new(MarkerKind::Output, 14, 1234),
        Marker::new(MarkerKind::Echo, 7, 99),
        Marker::new(MarkerKind::Calibration, 39, 0),
        Marker::new(MarkerKind::Output, 342, 117_648),
        Marker::new(MarkerKind::Output, 48, 16_806),
    ]
}

const BG: [u8; 3] = [24, 24, 32];
const GREY: [u8; 3] = [128, 128, 128];

/// A canvas holding one marker at (x, y) with the given cell size.
fn canvas_with(m: Marker, x: f64, y: f64, cell_w: f64, cell_h: f64, mode: EdgeMode) -> Canvas {
    let w = (x + cell_w * CELLS as f64 + 17.0).ceil() as usize;
    let h = (y + cell_h + 11.0).ceil() as usize;
    let mut c = Canvas::new(w, h, BG);
    c.draw_cells(x, y, cell_w, cell_h, &encode_rgb(m), mode);
    c
}

fn found(c: &Canvas) -> Vec<Marker> {
    scan(&c.view(), &ScanOptions::default())
        .markers
        .iter()
        .map(|f| f.marker)
        .collect()
}

fn classes_of(pixels: &[[u8; 3]]) -> Vec<Class> {
    let p = spec().pixel;
    pixels
        .iter()
        .map(|&[r, g, b]| classify(&p, r, g, b))
        .collect()
}

/// A level in the middle of the impure band.
fn mid_band() -> u8 {
    let p = spec().pixel;
    ((u16::from(p.channel_low_max) + u16::from(p.channel_high_min)) / 2) as u8
}

#[test]
fn reference_rendered_rows_decode_across_widths_blur_and_phase() {
    // The renderer the golden rows were drawn with (`renderSpans`, ported
    // exactly), swept: every cell width 6-30 px in quarter pixels, at four
    // sub-pixel phases, with crisp box-filtered edges and 0.5, 1 and 2 px of
    // blur. A 2 px blur widens every edge to about 3 impure pixels, which the
    // rules can read as a wide gap holding a cell of its own while cells are
    // that narrow: the REFERENCE decoder (`marker-raster.mjs`, run on this
    // exact sweep) rejects some phases at 6, 6.25 and 6.5 px and accepts every
    // other combination here, so 2 px blur starts at 6.75 px. (Its golden row
    // "6.5 px cells, 2 px blur" sits at the one phase that decodes.)
    let params = DecodeParams::default();
    let ms = [markers()[1], markers()[4], markers()[5]];
    let mut rows = 0;
    for quarter in 24..=120 {
        let w = f64::from(quarter) / 4.0;
        for blur in [0.0, 0.5, 1.0, 2.0] {
            if blur >= 2.0 && w < 6.75 {
                continue;
            }
            for phase in [0.0, 0.25, 0.5, 0.75] {
                for m in ms {
                    let mut spans = vec![Span::new(12.0 + phase, GREY)];
                    spans.extend(encode_rgb(m).iter().map(|&rgb| Span::new(w, rgb)));
                    spans.push(Span::new(12.0, GREY));
                    let pixels = render_spans(&spans, blur);
                    let got: Vec<_> = find_markers_in_row(&classes_of(&pixels), &params)
                        .iter()
                        .map(|rm| rm.result)
                        .collect();
                    assert_eq!(
                        got,
                        vec![Ok(m)],
                        "{m:?} at {w} px, blur {blur}, phase {phase}"
                    );
                    rows += 1;
                }
            }
        }
    }
    assert!(rows > 4000, "{rows}");
}

#[test]
fn crisp_cells_from_6_to_30_px_decode_with_exact_rects() {
    for cw in 6..=30usize {
        let ch = cw * 2 + 1;
        for m in markers() {
            let c = canvas_with(m, 9.0, 5.0, cw as f64, ch as f64, EdgeMode::Exact);
            let result = scan(&c.view(), &ScanOptions::default());
            assert_eq!(result.markers.len(), 1, "{m:?} at {cw}px: {result:?}");
            let f = result.markers[0];
            assert_eq!(f.marker, m, "{cw}px");
            assert_eq!((f.rect.x, f.rect.y), (9, 5), "{cw}px");
            assert_eq!(
                (f.rect.w, f.rect.h),
                ((cw * CELLS) as u32, ch as u32),
                "{cw}px"
            );
        }
    }
}

#[test]
fn anti_aliased_fractional_cells_decode_in_every_edge_mode() {
    for mode in [
        EdgeMode::Exact,
        EdgeMode::GammaCorrect,
        EdgeMode::Sequential,
    ] {
        for tenths in (60..=300).step_by(7) {
            let cw = f64::from(tenths) / 10.0;
            for m in markers() {
                let c = canvas_with(m, 5.37, 4.61, cw, cw * 2.1, mode);
                assert_eq!(found(&c), vec![m], "{mode:?} at {cw}px");
            }
        }
    }
}

#[test]
fn compositor_scaling_from_100_to_200_percent_decodes() {
    for cw in [6usize, 7, 8, 9, 10, 12, 14, 16, 20, 24, 30] {
        for m in markers() {
            let crisp = canvas_with(m, 6.0, 4.0, cw as f64, (cw * 2) as f64, EdgeMode::Exact);
            for pct in [100u32, 125, 150, 175, 200] {
                let s = f64::from(pct) / 100.0;
                for filter in [Filter::Bilinear, Filter::Bicubic] {
                    let scaled = crisp.resample(s, filter);
                    assert_eq!(found(&scaled), vec![m], "{cw}px at {pct}% {filter:?}");
                }
                // A DPI-aware host draws at the scaled size directly.
                let direct = canvas_with(
                    m,
                    6.0 * s,
                    4.0 * s,
                    cw as f64 * s,
                    (cw * 2) as f64 * s,
                    EdgeMode::Exact,
                );
                assert_eq!(found(&direct), vec![m], "{cw}px drawn at {pct}%");
            }
        }
    }
}

type Alteration = (&'static str, Box<dyn Fn([u8; 3]) -> [u8; 3]>);

/// Alterations that destroy the colour information. Each must leave NO
/// marker: a misdecode would put a wrong (session, seq) into a latency.
fn destructive_alterations() -> Vec<Alteration> {
    let mid = mid_band();
    vec![
        (
            "swap red/blue (BGR read as RGB)",
            Box::new(|[r, g, b]: [u8; 3]| [b, g, r]),
        ),
        ("swap green/blue", Box::new(|[r, g, b]: [u8; 3]| [r, b, g])),
        ("swap red/green", Box::new(|[r, g, b]: [u8; 3]| [g, r, b])),
        (
            "invert",
            Box::new(|[r, g, b]: [u8; 3]| [255 - r, 255 - g, 255 - b]),
        ),
        ("rotate channels", Box::new(|[r, g, b]: [u8; 3]| [g, b, r])),
        (
            "dim into the mid band",
            Box::new(move |[r, g, b]: [u8; 3]| {
                let f = |c: u8| (u16::from(c) * u16::from(mid) / 255) as u8;
                [f(r), f(g), f(b)]
            }),
        ),
        (
            "grayscale",
            Box::new(|[r, g, b]: [u8; 3]| {
                let y = (0.299 * f64::from(r) + 0.587 * f64::from(g) + 0.114 * f64::from(b)).round()
                    as u8;
                [y, y, y]
            }),
        ),
        (
            "the 256-colour cube rendered at level 175 for 255",
            Box::new(|c: [u8; 3]| c.map(|ch| if ch == 255 { 175 } else { ch })),
        ),
        (
            "an ANSI theme palette (VS Code dark)",
            Box::new(|c: [u8; 3]| match c {
                [255, 0, 0] => [205, 49, 49],
                [0, 255, 0] => [13, 188, 121],
                [255, 255, 0] => [229, 229, 16],
                [0, 0, 255] => [36, 114, 200],
                [255, 0, 255] => [188, 63, 188],
                [0, 255, 255] => [17, 168, 205],
                [255, 255, 255] => [229, 229, 229],
                other => other,
            }),
        ),
    ]
}

#[test]
fn colour_altered_markers_are_rejected_not_misdecoded() {
    let params = DecodeParams::default();
    for (name, f) in destructive_alterations() {
        for cw in [6.0, 11.0, 24.0] {
            for m in markers() {
                let mut c = canvas_with(m, 8.0, 4.0, cw, cw * 2.0, EdgeMode::Exact);
                c.map(&f);
                assert!(found(&c).is_empty(), "{name}: {m:?} at {cw}px was decoded");
                // Row by row as well: no candidate on the marker's middle row
                // decodes, whatever the scanner's zone bookkeeping does.
                let view = c.view();
                let mut classes = Vec::new();
                view.classify_row(4 + cw as usize, &params.pixel, &mut classes);
                let decoded: Vec<_> = find_markers_in_row(&classes, &params)
                    .into_iter()
                    .filter(|rm| rm.result.is_ok())
                    .collect();
                assert!(
                    decoded.is_empty(),
                    "{name}: {m:?} at {cw}px decoded on its middle row: {decoded:?}"
                );
            }
        }
    }
}

#[test]
fn one_cell_recoloured_to_a_mid_band_colour_is_rejected() {
    let mid = mid_band();
    for m in markers() {
        for i in 0..CELLS {
            for colour in [[mid, 0, 0], [0, mid, 255], [255, 255, mid], [mid, mid, mid]] {
                let mut c = canvas_with(m, 8.0, 4.0, 10.0, 20.0, EdgeMode::Exact);
                c.map_rect(8 + 10 * i, 4, 10, 20, |_| colour);
                assert!(found(&c).is_empty(), "{m:?} cell {i} -> {colour:?}");
            }
        }
    }
}

#[test]
fn every_single_cell_recolouring_to_a_palette_colour_is_rejected() {
    for m in [markers()[1], markers()[4]] {
        let values = encode_values(m);
        for cw in [6usize, 12, 30] {
            for (i, &value) in values.iter().enumerate() {
                for p in PALETTE {
                    if p.value == value {
                        continue;
                    }
                    let mut c =
                        canvas_with(m, 8.0, 4.0, cw as f64, (cw * 2) as f64, EdgeMode::Exact);
                    c.map_rect(8 + cw * i, 4, cw, cw * 2, |_| p.rgb);
                    let got = found(&c);
                    assert!(
                        got.is_empty(),
                        "{m:?} at {cw}px, cell {i} -> {}: {got:?}",
                        p.name
                    );
                }
            }
        }
    }
}

#[test]
fn colour_management_shifts_inside_the_thresholds_still_decode() {
    // Lift every 0 channel to just under the low threshold and pull every 255
    // channel to just over the high one: the information survives, so the
    // marker must still decode (and to the SAME marker).
    let p = spec().pixel;
    let lo = p.channel_low_max.saturating_sub(6);
    let hi = p.channel_high_min.saturating_add(6);
    for m in markers() {
        let mut c = canvas_with(m, 8.0, 4.0, 9.0, 18.0, EdgeMode::Exact);
        c.map(|px| px.map(|ch| if ch >= 128 { ch.min(hi) } else { ch.max(lo) }));
        assert_eq!(found(&c), vec![m]);
    }
}

#[test]
fn a_partly_occluded_marker_is_not_reported() {
    for m in markers() {
        for covered_from in [5usize, 10, 15, 19] {
            let mut c = canvas_with(m, 8.0, 4.0, 10.0, 20.0, EdgeMode::Exact);
            // Another window's grey body covers the marker from cell k on.
            c.fill_rect(8 + 10 * covered_from, 0, 400, 40, [140, 140, 150]);
            assert!(
                found(&c).is_empty(),
                "{m:?} covered from cell {covered_from}"
            );
        }
    }
}

#[test]
fn markers_tiled_like_panes_are_all_found() {
    // A 3x3 grid of panes, each with an output marker in its first row and an
    // echo marker directly below it in its second — the tiled arm's layout.
    let mut c = Canvas::new(3 * 300, 3 * 120, BG);
    let mut want = Vec::new();
    for row in 0..3 {
        for col in 0..3 {
            let session = (row * 3 + col) as u16;
            let out = Marker::new(MarkerKind::Output, session, 1000 + u32::from(session));
            let echo = Marker::new(MarkerKind::Echo, session, u32::from(session));
            let (x, y) = ((col * 300 + 4) as f64, (row * 120 + 4) as f64);
            c.draw_cells(x, y, 8.4, 17.0, &encode_rgb(out), EdgeMode::Exact);
            c.draw_cells(x, y + 17.0, 8.4, 17.0, &encode_rgb(echo), EdgeMode::Exact);
            want.push(out);
            want.push(echo);
        }
    }
    let mut got = found(&c);
    got.sort();
    want.sort();
    assert_eq!(got, want);
}

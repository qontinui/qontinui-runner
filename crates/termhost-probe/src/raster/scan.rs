//! Finding every marker in a frame.
//!
//! Pane positions are not known in advance (tiled layouts, scaled windows), so
//! the scanner runs the `codec.pixel` row decoder ([`super::row`]) over the
//! frame's rows: every valid marker starts with a red cell followed by a cyan
//! one, and the checksum makes a coincidental red-cyan pair in content a
//! rejected candidate, never a marker. A decoded marker's rectangle is the rows
//! on which the same marker still decodes at the same place, so the rectangle
//! is a measured fact rather than an assumed cell height.
//!
//! # Which pixel rows are decoded
//!
//! `codec.pixel` decodes ONE pixel row, and its rule 1 leaves the choice of
//! rows to the probe. This is that choice:
//!
//! 1. **The frame is what the capture source delivered.** WGC delivers the
//!    target window as the compositor draws it; Desktop Duplication delivers
//!    the window's on-screen bounds, cropped from the output it duplicates;
//!    X11 delivers, after each batch of DAMAGE events, the full-width band from
//!    64 rows above the topmost damaged rectangle to 64 rows below the
//!    bottommost (clipped to the window), so a marker repainted anywhere inside
//!    the damage lies wholly in the band. Rectangles are found in frame
//!    coordinates; the engine offsets them by the frame's origin into window
//!    coordinates.
//! 2. **Every `row_step`-th row, from the frame's row 0, each in full width.**
//!    The default step is 2. A marker is one terminal cell tall, and the
//!    narrowest supported cell (6 px wide) is about 12 px tall, so a step of 2
//!    reads every marker on at least 5 rows, including the rows clear of the
//!    vertical anti-aliasing at its top and bottom edges. No column window is
//!    applied: each row is decoded edge to edge, left to right (rule 9), so a
//!    marker anywhere in any pane is found.
//! 3. **One marker, one report.** The first scanned row that decodes a marker
//!    reports it, and its rectangle is the contiguous run of rows above and
//!    below on which the same marker decodes within half a cell of the same
//!    place — each of those rows decoded in full by the same rules, over the
//!    marker's span plus two cells either side. Later scanned rows that read
//!    that marker again, within two rows of its run, are the same marker.
//! 4. **A misread of a found marker is not a rejection.** A candidate rejected
//!    within two rows of a found marker's run whose cell 0 lies inside its
//!    footprint (a blended edge row, or a red-cyan pair in its payload) is that
//!    marker misread, and is not reported — even when the row was scanned
//!    before the marker was found.
//! 5. **Different markers are always reported**, including the echo marker on
//!    the row directly below its output marker, and a marker starting where
//!    another ends.

use serde::Serialize;

use super::row::{find_markers_in_row_into, RowMarker, RowScratch};
use super::{Class, DecodeParams, FrameView};
use crate::codec::{Marker, Rejection, CELLS};

/// An axis-aligned rectangle in frame pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

impl Rect {
    pub fn right(&self) -> i32 {
        self.x + self.w as i32
    }

    pub fn bottom(&self) -> i32 {
        self.y + self.h as i32
    }

    pub fn area(&self) -> u64 {
        u64::from(self.w) * u64::from(self.h)
    }

    /// The overlap of two rectangles, if any.
    pub fn intersect(&self, other: &Rect) -> Option<Rect> {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let r = self.right().min(other.right());
        let b = self.bottom().min(other.bottom());
        (r > x && b > y).then(|| Rect {
            x,
            y,
            w: (r - x) as u32,
            h: (b - y) as u32,
        })
    }

    /// This rectangle shifted by (dx, dy).
    pub fn offset(&self, dx: i32, dy: i32) -> Rect {
        Rect {
            x: self.x + dx,
            y: self.y + dy,
            ..*self
        }
    }
}

/// A decoded marker and where it sits.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct FoundMarker {
    pub marker: Marker,
    pub rect: Rect,
    pub cell_w: f32,
}

/// A red-cyan start that did not decode, with its rejection reason.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct RejectedCandidate {
    pub x: i32,
    pub y: i32,
    pub reason: Rejection,
}

/// Scan tuning.
#[derive(Clone, Copy, Debug)]
pub struct ScanOptions {
    /// Scan every `row_step`-th row. A marker is one terminal cell tall, at
    /// least ~12 px for the narrowest supported cells, so 2 never misses one.
    pub row_step: usize,
    /// At most this many distinct rejected candidates are reported per frame.
    pub max_rejections: usize,
    /// The pixel-rule parameters (the golden file's by default).
    pub params: DecodeParams,
}

impl Default for ScanOptions {
    fn default() -> Self {
        ScanOptions {
            row_step: 2,
            max_rejections: 8,
            params: DecodeParams::default(),
        }
    }
}

/// Every marker found in a frame, plus rejected candidates (diagnostics).
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct ScanResult {
    pub markers: Vec<FoundMarker>,
    pub rejected: Vec<RejectedCandidate>,
}

/// A found marker's footprint.
#[derive(Clone, Copy, Debug)]
struct Zone {
    marker: Marker,
    x0: f64,
    cell_w: f64,
    top: usize,
    bottom: usize,
}

/// Rows of vertical slack around a zone: a blended edge row just outside a
/// marker's measured extent still belongs to it.
const ZONE_ROW_SLACK: usize = 2;

impl Zone {
    fn near_rows(&self, y: usize) -> bool {
        y + ZONE_ROW_SLACK >= self.top && y <= self.bottom + ZONE_ROW_SLACK
    }

    /// The same marker read again at the same place (a later row of it).
    fn rereads(&self, marker: Marker, x0: f64, y: usize) -> bool {
        self.marker == marker && self.near_rows(y) && (x0 - self.x0).abs() <= self.cell_w / 2.0
    }

    /// Whether a REJECTED candidate starting at `x0` on row `y` is this marker
    /// misread — a blended edge row, or a red-cyan pair inside its payload. A
    /// different marker that decodes is never claimed: an echo marker sits on
    /// the row directly below its output marker. An adjacent marker starting
    /// where this one ends (x0 + 20 w) is outside the footprint.
    fn claims_rejection(&self, x0: f64, y: usize) -> bool {
        self.near_rows(y)
            && x0 >= self.x0 - self.cell_w / 2.0
            && x0 < self.x0 + (CELLS as f64 - 0.5) * self.cell_w
    }
}

/// Raw rejections kept before zone filtering, as a multiple of the reported cap.
const REJECTION_HEADROOM: usize = 16;

/// Find every marker in `view`.
pub fn scan(view: &FrameView<'_>, opts: &ScanOptions) -> ScanResult {
    let mut result = ScanResult::default();
    let mut classes: Vec<Class> = Vec::with_capacity(view.width());
    let mut scratch = RowScratch::default();
    let mut found: Vec<RowMarker> = Vec::new();
    let mut zones: Vec<Zone> = Vec::new();
    let mut rejected: Vec<(f64, usize, f64, Rejection)> = Vec::new();
    let raw_cap = opts.max_rejections.saturating_mul(REJECTION_HEADROOM);
    let step = opts.row_step.max(1);
    let mut y = 0;
    while y < view.height() {
        view.classify_row(y, &opts.params.pixel, &mut classes);
        find_markers_in_row_into(&classes, &opts.params, &mut scratch, &mut found);
        for rm in &found {
            let x0 = rm.x0();
            match rm.result {
                Ok(marker) => {
                    if zones.iter().any(|z| z.rereads(marker, x0, y)) {
                        continue;
                    }
                    let (top, bottom) =
                        vertical_extent(view, y, marker, x0, rm.cell_w, &opts.params);
                    result.markers.push(FoundMarker {
                        marker,
                        rect: Rect {
                            x: x0.round() as i32,
                            y: top as i32,
                            w: (rm.cell_w * CELLS as f64).round().max(1.0) as u32,
                            h: (bottom - top + 1) as u32,
                        },
                        cell_w: rm.cell_w as f32,
                    });
                    zones.push(Zone {
                        marker,
                        x0,
                        cell_w: rm.cell_w,
                        top,
                        bottom,
                    });
                }
                Err(reason) => {
                    if zones.iter().any(|z| z.claims_rejection(x0, y)) {
                        continue;
                    }
                    let duplicate = rejected
                        .iter()
                        .any(|&(x, _, w, r)| r == reason && (x - x0).abs() <= w.max(rm.cell_w));
                    if !duplicate && rejected.len() < raw_cap {
                        rejected.push((x0, y, rm.cell_w, reason));
                    }
                }
            }
        }
        y += step;
    }
    // A candidate rejected on a row scanned before its marker was found (the
    // blended top edge) is the marker, not a rejection.
    result.rejected = rejected
        .into_iter()
        .filter(|&(x0, y, _, _)| !zones.iter().any(|z| z.claims_rejection(x0, y)))
        .take(opts.max_rejections)
        .map(|(x0, y, _, reason)| RejectedCandidate {
            x: x0.round() as i32,
            y: y as i32,
            reason,
        })
        .collect();
    result
}

/// The contiguous rows around `y` on which the same marker decodes at the same
/// place. Each neighbouring row is decoded over the marker's span plus two
/// cells of margin either side — enough that every segment and gap the walk
/// measures is the one the full row holds, so the slice decodes as the row does.
fn vertical_extent(
    view: &FrameView<'_>,
    y: usize,
    marker: Marker,
    x0: f64,
    cell_w: f64,
    params: &DecodeParams,
) -> (usize, usize) {
    let margin = 2.0 * cell_w + 2.0;
    let xs = (x0 - margin).floor().max(0.0) as usize;
    let xe = ((x0 + CELLS as f64 * cell_w + margin).ceil().max(0.0) as usize).min(view.width());
    let mut classes = Vec::with_capacity(xe.saturating_sub(xs));
    let mut scratch = RowScratch::default();
    let mut found = Vec::new();
    let mut row_matches = |yy: usize| {
        view.classify_span(yy, xs, xe, &params.pixel, &mut classes);
        find_markers_in_row_into(&classes, params, &mut scratch, &mut found);
        found
            .iter()
            .any(|rm| rm.result == Ok(marker) && (xs as f64 + rm.x0() - x0).abs() <= cell_w / 2.0)
    };
    let mut top = y;
    while top > 0 && row_matches(top - 1) {
        top -= 1;
    }
    let mut bottom = y;
    while bottom + 1 < view.height() && row_matches(bottom + 1) {
        bottom += 1;
    }
    (top, bottom)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::{encode_rgb, MarkerKind};
    use crate::raster::synth::Canvas;

    #[test]
    fn finds_several_markers_with_rects() {
        let mut canvas = Canvas::new(700, 200, [30, 30, 30]);
        let a = Marker::new(MarkerKind::Output, 1, 10);
        let b = Marker::new(MarkerKind::Echo, 2, 20);
        let c = Marker::new(MarkerKind::Output, 39, 36_000);
        canvas.draw_marker(10.0, 10.0, 12.0, 24.0, &encode_rgb(a));
        canvas.draw_marker(300.0, 10.0, 12.0, 24.0, &encode_rgb(b));
        canvas.draw_marker(50.0, 120.0, 9.0, 18.0, &encode_rgb(c));
        let result = scan(&canvas.view(), &ScanOptions::default());
        let mut found: Vec<Marker> = result.markers.iter().map(|f| f.marker).collect();
        found.sort();
        let mut want = vec![a, b, c];
        want.sort();
        assert_eq!(found, want);
        let fa = result.markers.iter().find(|f| f.marker == a).expect("a");
        assert_eq!(
            fa.rect,
            Rect {
                x: 10,
                y: 10,
                w: 240,
                h: 24
            }
        );
        let fc = result.markers.iter().find(|f| f.marker == c).expect("c");
        assert_eq!(
            fc.rect,
            Rect {
                x: 50,
                y: 120,
                w: 180,
                h: 18
            }
        );
        assert!(result.rejected.is_empty(), "{:?}", result.rejected);
    }

    #[test]
    fn adjacent_markers_on_one_row_are_both_found() {
        // Touching markers: the second starts exactly where the first ends.
        // (When the first ends in red, the two red cells merge into one
        // segment and rule 9 resumes past it, so the precondition is stated.)
        let a = (41..)
            .map(|seq| Marker::new(MarkerKind::Output, 0, seq))
            .find(|m| crate::codec::encode_values(*m)[CELLS - 1] != 4)
            .expect("a marker not ending in red");
        let b = Marker::new(MarkerKind::Echo, 1, 7);
        let mut canvas = Canvas::new(520, 30, [0, 0, 0]);
        canvas.draw_marker(10.0, 3.0, 12.0, 24.0, &encode_rgb(a));
        canvas.draw_marker(250.0, 3.0, 12.0, 24.0, &encode_rgb(b));
        let result = scan(&canvas.view(), &ScanOptions::default());
        let found: Vec<Marker> = result.markers.iter().map(|f| f.marker).collect();
        assert_eq!(found, vec![a, b]);
    }

    #[test]
    fn an_echo_marker_directly_below_its_output_marker_is_found() {
        // The echo region is row 1, the output region row 0: no gap between.
        let out = Marker::new(MarkerKind::Output, 3, 500);
        let echo = Marker::new(MarkerKind::Echo, 3, 17);
        let mut canvas = Canvas::new(300, 60, [24, 24, 24]);
        canvas.draw_marker(8.0, 6.0, 12.0, 24.0, &encode_rgb(out));
        canvas.draw_marker(8.0, 30.0, 12.0, 24.0, &encode_rgb(echo));
        let result = scan(&canvas.view(), &ScanOptions::default());
        let found: Vec<(Marker, Rect)> =
            result.markers.iter().map(|f| (f.marker, f.rect)).collect();
        assert_eq!(
            found,
            vec![
                (
                    out,
                    Rect {
                        x: 8,
                        y: 6,
                        w: 240,
                        h: 24
                    }
                ),
                (
                    echo,
                    Rect {
                        x: 8,
                        y: 30,
                        w: 240,
                        h: 24
                    }
                )
            ]
        );
        assert!(result.rejected.is_empty(), "{:?}", result.rejected);
    }

    #[test]
    fn content_red_cyan_pairs_are_rejected_not_reported() {
        let mut canvas = Canvas::new(400, 40, [0, 0, 0]);
        // A red cell, a cyan cell, then noise colours: a start with no marker.
        let colors = [
            [255, 0, 0],
            [0, 255, 255],
            [255, 255, 0],
            [0, 0, 255],
            [255, 255, 255],
        ];
        canvas.draw_marker(20.0, 5.0, 10.0, 20.0, &colors);
        let result = scan(&canvas.view(), &ScanOptions::default());
        assert!(result.markers.is_empty());
        assert!(!result.rejected.is_empty());
        assert!(result.rejected.len() <= ScanOptions::default().max_rejections);
    }

    #[test]
    fn rect_intersection() {
        let a = Rect {
            x: 0,
            y: 0,
            w: 10,
            h: 10,
        };
        let b = Rect {
            x: 5,
            y: 5,
            w: 10,
            h: 10,
        };
        assert_eq!(
            a.intersect(&b),
            Some(Rect {
                x: 5,
                y: 5,
                w: 5,
                h: 5
            })
        );
        assert_eq!(
            a.intersect(&Rect {
                x: 10,
                y: 0,
                w: 5,
                h: 5
            }),
            None
        );
    }
}

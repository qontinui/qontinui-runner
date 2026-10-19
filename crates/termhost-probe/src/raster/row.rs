//! One pixel row -> every marker candidate in it: `marker-golden.json`
//! `codec.pixel` rules 1-9, implemented exactly. The reference implementation
//! is `scripts/termhost-bench/marker-raster.mjs`; this module follows it
//! statement for statement, so the two agree on every golden row, and a change
//! to either is a change to the contract.
//!
//! Rule 1 fixes the geometry and the arithmetic: pixel i spans [i, i + 1) and a
//! segment [start, end); gap(A, B) = B.start - A.end and mid(A, B) = (A.end +
//! B.start) / 2; IEEE-754 binary64, evaluated as written; round(x) rounds half
//! up. Every x the rules round is positive, where Rust's `f64::round` (half
//! away from zero) and JavaScript's `Math.round` (half toward +infinity) agree.
//! Every width the rules divide by is positive too: b1 lies at or before cyan's
//! start and b2 at or after cyan's end, and every later right boundary lies past
//! b1, so no division here can produce NaN or infinity.

use super::{Class, DecodeParams};
use crate::codec::{decode_cells, Marker, Rejection, CELLS};

const RED: u8 = 4;
const CYAN: u8 = 3;

/// Rule 3: a maximal run of pure pixels with one value, spanning [start, end).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Segment {
    pub v: u8,
    pub start: usize,
    pub end: usize,
}

/// Rule 3: the row's segments, left to right (impure pixels lie in the gaps).
pub fn pure_segments(classes: &[Class], out: &mut Vec<Segment>) {
    out.clear();
    let mut i = 0;
    while i < classes.len() {
        let c = classes[i];
        let mut j = i + 1;
        while j < classes.len() && classes[j] == c {
            j += 1;
        }
        if let Class::Pure(v) = c {
            out.push(Segment {
                v,
                start: i,
                end: j,
            });
        }
        i = j;
    }
}

/// Rule 4's geometry of a candidate start.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Start {
    b1: f64,
    b2: f64,
    w: f64,
    cyan_wide: bool,
    truncated: bool,
}

/// Rules 4-5: segment `s` starts a candidate when it is red, segment s+1 is
/// cyan, and the gap between them is narrower than `boundary_gap_max` x w.
fn start_geometry(segs: &[Segment], s: usize, row_len: usize, gap_max: f64) -> Option<Start> {
    let red = segs.get(s)?;
    let cyan = segs.get(s + 1)?;
    if red.v != RED || cyan.v != CYAN {
        return None;
    }
    let b1 = (red.end + cyan.start) as f64 / 2.0;
    let after = segs.get(s + 2);
    let next_start = after.map_or(row_len, |a| a.start);
    let cyan_gap = (next_start - cyan.end) as f64;
    let to_middle = (cyan.end + next_start) as f64 / 2.0 - b1;
    let cyan_wide = after.is_some() && cyan_gap >= gap_max * to_middle;
    let b2 = if cyan_wide || after.is_none() {
        cyan.end as f64
    } else {
        (cyan.end + next_start) as f64 / 2.0
    };
    let w = b2 - b1;
    // The reference's `!(gap < limit)`; with no NaN possible (see the module
    // docs) that is exactly `gap >= limit`.
    if (cyan.start - red.end) as f64 >= gap_max * w {
        return None;
    }
    Some(Start {
        b1,
        b2,
        w,
        cyan_wide,
        truncated: after.is_none(),
    })
}

/// One candidate's outcome, and where it sits in the row.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RowMarker {
    pub result: Result<Marker, Rejection>,
    /// Rule 4's b1: the boundary between cells 0 and 1 (row pixels).
    pub b1: f64,
    /// The running-mean cell width when the walk stopped (rule 7).
    pub cell_w: f64,
}

impl RowMarker {
    /// The estimated left edge of cell 0.
    pub fn x0(&self) -> f64 {
        self.b1 - self.cell_w
    }
}

/// Rules 5-8 for the candidate at segment `s`: the result, the segment to
/// resume scanning at (rule 9), and the running width when the walk stopped.
fn decode_candidate(
    segs: &[Segment],
    s: usize,
    row_len: usize,
    g: &Start,
    p: &DecodeParams,
) -> (Result<Marker, Rejection>, usize, f64) {
    let tolerance = p.pixel.width_tolerance;
    let gap_max = p.pixel.boundary_gap_max;
    if g.truncated {
        return (Err(Rejection::WrongLength), s + 1, g.w);
    }
    let mut w = g.w;
    let mut cells: [Option<u8>; CELLS] = [None; CELLS];
    cells[0] = Some(RED);
    cells[1] = Some(CYAN);
    let mut n = 2;
    let mut left = g.b2;
    let mut in_gap = g.cyan_wide;
    let mut j = s + 2;
    while n < CELLS {
        let remaining = CELLS - n;
        if in_gap {
            // Rule 6: an impure gap is k = max(1, round(gap / w)) impure cells,
            // of which only min(k, need) are appended — never past cell 19. An
            // occluding toast or tooltip that runs on past the marker's last
            // cell is therefore `off_palette`, not `wrong_length`.
            let gap_end = segs.get(j).map_or(row_len, |seg| seg.start) as f64;
            let k = ((gap_end - left) / w).round().max(1.0);
            let take = if k >= remaining as f64 {
                remaining
            } else {
                k as usize
            };
            // The slots are already `None`; appending impure cells is counting them.
            n += take;
            if take as f64 == k {
                w = (gap_end - g.b1) / (n - 1) as f64;
            }
            left = gap_end;
            in_gap = false;
            continue;
        }
        // Rule 7: a segment is k = round(span / w) cells of its value.
        let Some(seg) = segs.get(j).copied() else {
            return (Err(Rejection::WrongLength), s + 1, w);
        };
        let after = segs.get(j + 1);
        let gap_after = (after.map_or(row_len, |a| a.start) - seg.end) as f64;
        let wide = gap_after >= gap_max * w;
        let right = if wide {
            seg.end as f64
        } else if let Some(a) = after {
            (seg.end + a.start) as f64 / 2.0
        } else {
            row_len as f64
        };
        let span = (right - left) / w;
        let k = span.round();
        if k >= remaining as f64 {
            // The segment that reaches cell 19 gives only the cells still
            // needed; whatever lies beyond is unchecked.
            cells[n..].fill(Some(seg.v));
            j += 1;
            break;
        }
        if k < 1.0 || (span - k).abs() > tolerance {
            return (Err(Rejection::IrregularCells), s + 1, w);
        }
        // A run k cells wide IS k equal cells (the adjacency check rejects it).
        let k = k as usize;
        cells[n..n + k].fill(Some(seg.v));
        n += k;
        w = (right - g.b1) / (n - 1) as f64;
        left = right;
        in_gap = wide;
        j += 1;
    }
    // Rule 8: the 20 cells through the cell checks, in decode order.
    let decoded = decode_cells(&cells);
    let resume = if decoded.is_ok() { j } else { s + 1 };
    (decoded, resume, w)
}

/// Buffers [`find_markers_in_row_into`] reuses across rows.
#[derive(Clone, Debug, Default)]
pub struct RowScratch {
    segs: Vec<Segment>,
}

/// Rule 9: every marker candidate in one row of classified pixels, left to
/// right. Empty = the row holds no red-then-cyan start, i.e. no detectable
/// marker.
pub fn find_markers_in_row(classes: &[Class], p: &DecodeParams) -> Vec<RowMarker> {
    let mut out = Vec::new();
    find_markers_in_row_into(classes, p, &mut RowScratch::default(), &mut out);
    out
}

/// [`find_markers_in_row`] into `out` (cleared first), reusing `scratch`.
pub fn find_markers_in_row_into(
    classes: &[Class],
    p: &DecodeParams,
    scratch: &mut RowScratch,
    out: &mut Vec<RowMarker>,
) {
    out.clear();
    let segs = &mut scratch.segs;
    pure_segments(classes, segs);
    let mut s = 0;
    while s + 1 < segs.len() {
        let Some(g) = start_geometry(segs, s, classes.len(), p.pixel.boundary_gap_max) else {
            s += 1;
            continue;
        };
        let (result, resume, cell_w) = decode_candidate(segs, s, classes.len(), &g, p);
        out.push(RowMarker {
            result,
            b1: g.b1,
            cell_w,
        });
        s = resume;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::{encode_values, MarkerKind};

    fn params() -> DecodeParams {
        DecodeParams::default()
    }

    fn row(prefix: &[Class], values: &[u8], cell_w: usize, suffix: &[Class]) -> Vec<Class> {
        let mut r = prefix.to_vec();
        for &v in values {
            r.extend(std::iter::repeat_n(Class::Pure(v), cell_w));
        }
        r.extend_from_slice(suffix);
        r
    }

    #[test]
    fn segments_are_maximal_pure_runs_split_by_impure_pixels() {
        let r = [
            Class::Pure(4),
            Class::Pure(4),
            Class::Impure,
            Class::Pure(4),
            Class::Pure(3),
            Class::Impure,
            Class::Impure,
        ];
        let mut segs = Vec::new();
        pure_segments(&r, &mut segs);
        assert_eq!(
            segs,
            vec![
                Segment {
                    v: 4,
                    start: 0,
                    end: 2
                },
                Segment {
                    v: 4,
                    start: 3,
                    end: 4
                },
                Segment {
                    v: 3,
                    start: 4,
                    end: 5
                }
            ]
        );
    }

    #[test]
    fn crisp_markers_decode_at_every_integer_width() {
        let m = Marker::new(MarkerKind::Output, 14, 1234);
        for w in 1..=40 {
            let r = row(
                &[Class::Impure; 5],
                &encode_values(m),
                w,
                &[Class::Impure; 9],
            );
            let found = find_markers_in_row(&r, &params());
            assert_eq!(found.len(), 1, "width {w}");
            assert_eq!(found[0].result, Ok(m), "width {w}");
            assert!((found[0].x0() - 5.0).abs() < 1e-9, "{}", found[0].x0());
            assert!((found[0].cell_w - w as f64).abs() < 1e-9);
        }
    }

    #[test]
    fn a_run_two_cells_wide_is_two_equal_cells() {
        let mut values = encode_values(Marker::new(MarkerKind::Output, 14, 1234));
        values[9] = values[8];
        let r = row(&[Class::Impure; 3], &values, 8, &[Class::Impure; 20]);
        let found = find_markers_in_row(&r, &params());
        assert_eq!(found[0].result, Err(Rejection::AdjacentCellsEqual));
    }

    #[test]
    fn the_row_ending_inside_the_marker_is_wrong_length() {
        let values = encode_values(Marker::new(MarkerKind::Output, 14, 1234));
        let r = row(&[Class::Impure; 3], &values[..15], 8, &[]);
        assert_eq!(
            find_markers_in_row(&r, &params())[0].result,
            Err(Rejection::WrongLength)
        );
        // Red then cyan with nothing after: the truncated start of rule 5.
        let r = row(&[], &values[..2], 8, &[]);
        assert_eq!(
            find_markers_in_row(&r, &params())[0].result,
            Err(Rejection::WrongLength)
        );
    }

    #[test]
    fn an_accepted_marker_is_skipped_whole_and_a_rejected_one_resumes_after_red() {
        // Seq 7 of session 2 carries a red-cyan pair at cells 15-16 (golden row
        // 24): accepted, so scanning resumes past it and the inner pair is not
        // a candidate.
        let m = Marker::new(MarkerKind::Output, 2, 7);
        let values = encode_values(m);
        assert!(values.windows(2).any(|w| w == [RED, CYAN]));
        let r = row(&[Class::Impure; 4], &values, 9, &[Class::Impure; 4]);
        let found = find_markers_in_row(&r, &params());
        assert_eq!(
            found.iter().map(|f| f.result).collect::<Vec<_>>(),
            vec![Ok(m)]
        );
        // The same cells with a damaged checksum: rejected, then the inner pair
        // is tried as a candidate of its own.
        let mut bad = values;
        bad[13] = (bad[13] + 1) % 8;
        if bad[13] == bad[12] || bad[13] == bad[14] {
            bad[13] = (bad[13] + 1) % 8;
        }
        let r = row(&[Class::Impure; 4], &bad, 9, &[Class::Impure; 4]);
        let found = find_markers_in_row(&r, &params());
        assert!(found.len() >= 2, "{found:?}");
        assert!(found[0].result.is_err());
    }
}

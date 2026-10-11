/**
 * termhost-bench marker raster rules (codec v2) — how one row of captured pixels
 * becomes marker cells. This module is the EXECUTABLE SPECIFICATION Phase 2's Rust
 * probe ports: `marker-golden.json` carries these exact parameters (`codec.pixel`) and
 * raster vectors whose expected results this decoder produced.
 *
 * Plan `2026-10-11-terminal-page-performance-kpis-across-setups`, Phase 1.
 *
 * The rules are `PIXEL_RULE_TEXT` in marker-golden.mjs (copied into the golden file as
 * `codec.pixel.rules`), an exact algorithm; in short:
 *   1. Positions are pixel edges (pixel i spans [i, i + 1), a segment [start, end));
 *      gap(A, B) = B.start - A.end, mid(A, B) = (A.end + B.start) / 2; round = half up.
 *   2. A channel <= 63 is 0, >= 192 is 1, anything else is impure; a pixel is pure when
 *      all three channels are, with value 4R + 2G + B.
 *   3. Segments: maximal runs of pure pixels with one value; impure pixels are gaps.
 *   4. The first width w: b1 = mid(red, cyan); b2 = the middle of the gap after cyan, or
 *      cyan's end when that gap is at least `boundary_gap_max` x (its middle - b1) or no
 *      segment follows; w = b2 - b1 (anti-aliasing eats pixels from both sides).
 *   5. A candidate starts at a red segment followed by cyan across a gap < 0.5 w; with
 *      nothing after cyan it is `wrong_length`.
 *   6. An impure gap is max(1, round(gap / w)) impure (null) cells.
 *   7. A segment's right boundary is the middle of a narrow gap after it, its own end
 *      before a wide one, or the row end; it is k = round(span / w) cells of its value
 *      (a run k cells wide IS k equal cells). The segment that reaches cell 19 gives only
 *      the cells still needed, unchecked beyond; otherwise k >= 1 and |span / w - k| <=
 *      `width_tolerance` or `irregular_cells`. After each step w is the running mean
 *      (right boundary - b1) / (cells - 1), so a fractional width never accumulates.
 *   8. The 20 cells go through the cell checks in `DECODE_REASONS` order (marker.mjs
 *      `decodeMarkerValues`); the row ending first is `wrong_length`.
 *   9. Scanning resumes after the segment that gave cell 19 of an accepted marker, or at
 *      the segment after the red one of a rejected candidate.
 */

import { MARKER_CELLS, decodeMarkerValues } from "./marker.mjs";

/** The exact numeric parameters of the rules above. */
export const PIXEL_RULES = Object.freeze({
  channel_low_max: 63,
  channel_high_min: 192,
  width_tolerance: 0.25,
  boundary_gap_max: 0.5,
});

/** Rejections the pixel stage adds before the cell checks. */
export const ROW_REASONS = Object.freeze(["irregular_cells", "wrong_length"]);

const RED = 4;
const CYAN = 3;

/**
 * @param {number} c a channel, 0-255
 * @returns {0 | 1 | -1} -1 = impure
 */
export function channelBit(c) {
  if (c <= PIXEL_RULES.channel_low_max) return 0;
  if (c >= PIXEL_RULES.channel_high_min) return 1;
  return -1;
}

/**
 * @param {number[]} rgb
 * @returns {number} palette value 0-7, or -1 for an impure pixel
 */
export function classifyPixel([r, g, b]) {
  const R = channelBit(r);
  const G = channelBit(g);
  const B = channelBit(b);
  return R < 0 || G < 0 || B < 0 ? -1 : 4 * R + 2 * G + B;
}

/** Maximal runs of pure pixels with one value. */
function pureSegments(values) {
  const segments = [];
  for (let i = 0; i < values.length; ) {
    let j = i + 1;
    while (j < values.length && values[j] === values[i]) j += 1;
    if (values[i] >= 0) segments.push({ v: values[i], start: i, end: j });
    i = j;
  }
  return segments;
}

/**
 * The geometry of a candidate start at segment `s`, or null when `s` is not one: a red
 * segment, then a cyan segment across a gap narrower than `boundary_gap_max` x w. Here
 * w is measured between gap MIDDLES (b1, the middle of the red-cyan gap, to the middle
 * of the gap after cyan), because anti-aliasing eats into a cell's pure pixels from both
 * sides; when the gap after cyan is itself at least `boundary_gap_max` x that distance,
 * it holds impure cells and cyan ends where its pure pixels end.
 */
function startGeometry(segments, s, rowLength) {
  const gapMax = PIXEL_RULES.boundary_gap_max;
  const red = segments[s];
  const cyan = segments[s + 1];
  if (!red || !cyan || red.v !== RED || cyan.v !== CYAN) return null;
  const b1 = (red.end + cyan.start) / 2;
  const afterCyan = segments[s + 2];
  const nextStart = afterCyan ? afterCyan.start : rowLength;
  const cyanGap = nextStart - cyan.end;
  const toMiddle = (cyan.end + nextStart) / 2 - b1;
  const cyanWide = afterCyan !== undefined && cyanGap >= gapMax * toMiddle;
  const b2 = cyanWide || !afterCyan ? cyan.end : (cyan.end + nextStart) / 2;
  const w = b2 - b1;
  if (!(cyan.start - red.end < gapMax * w)) return null;
  return { b1, b2, w, cyanWide, truncated: !afterCyan };
}

/**
 * Decode the candidate starting at segment `s` (see startGeometry).
 *
 * @returns {{result: object, resume: number}} resume = the next segment to scan
 */
function decodeCandidate(segments, s, rowLength, geometry) {
  const { width_tolerance: tolerance, boundary_gap_max: gapMax } = PIXEL_RULES;
  const reject = (reason) => ({ result: { ok: false, reason }, resume: s + 1 });
  if (geometry.truncated) return reject("wrong_length");
  const { b1, b2, cyanWide } = geometry;
  let w = geometry.w;
  const cells = [RED, CYAN];
  let left = b2;
  let inGap = cyanWide;
  let j = s + 2;
  while (cells.length < MARKER_CELLS) {
    const remaining = MARKER_CELLS - cells.length;
    if (inGap) {
      const gapEnd = j < segments.length ? segments[j].start : rowLength;
      const k = Math.max(1, Math.round((gapEnd - left) / w));
      const take = Math.min(k, remaining);
      for (let t = 0; t < take; t += 1) cells.push(null);
      if (take === k) w = (gapEnd - b1) / (cells.length - 1);
      left = gapEnd;
      inGap = false;
      continue;
    }
    if (j >= segments.length) return reject("wrong_length");
    const seg = segments[j];
    const after = segments[j + 1];
    const gapAfter = (after ? after.start : rowLength) - seg.end;
    const wide = gapAfter >= gapMax * w;
    const right = wide ? seg.end : after ? (seg.end + after.start) / 2 : rowLength;
    const span = (right - left) / w;
    const k = Math.round(span);
    if (k >= remaining) {
      for (let t = 0; t < remaining; t += 1) cells.push(seg.v);
      j += 1;
      break;
    }
    if (k < 1 || Math.abs(span - k) > tolerance) return reject("irregular_cells");
    for (let t = 0; t < k; t += 1) cells.push(seg.v);
    w = (right - b1) / (cells.length - 1);
    left = right;
    inGap = wide;
    j += 1;
  }
  const decoded = decodeMarkerValues(cells);
  return { result: decoded, resume: decoded.ok ? j : s + 1 };
}

/**
 * Every marker candidate in one pixel row, left to right. An empty list means the row
 * holds no red-then-cyan start, i.e. no detectable marker.
 *
 * @param {number[][]} pixels [[r, g, b], ...]
 * @returns {object[]} each `{ok: true, kind, kindName, session, seq}` or `{ok: false, reason}`
 */
export function findMarkersInRow(pixels) {
  const segments = pureSegments(pixels.map(classifyPixel));
  const results = [];
  for (let s = 0; s + 1 < segments.length; ) {
    const geometry = startGeometry(segments, s, pixels.length);
    if (!geometry) {
      s += 1;
      continue;
    }
    const { result, resume } = decodeCandidate(segments, s, pixels.length, geometry);
    results.push(result);
    s = resume;
  }
  return results;
}

/**
 * Render spans to one pixel row by area coverage: pixel x covers [x - blur, x + 1 +
 * blur) and takes the coverage-weighted mean colour, rounded per channel. This models
 * fractional cell widths (display scaling) and anti-aliased edges; it is how the
 * golden raster vectors and the tests draw synthetic rows.
 *
 * @param {{width: number, rgb: number[]}[]} spans laid out from x = 0
 * @param {{blur?: number}} [opts]
 * @returns {number[][]}
 */
export function renderSpans(spans, { blur = 0 } = {}) {
  const placed = [];
  let x = 0;
  for (const span of spans) {
    if (!(span.width > 0)) throw new RangeError(`span width must be positive: ${span.width}`);
    placed.push({ from: x, to: x + span.width, rgb: span.rgb });
    x += span.width;
  }
  const total = Math.round(x);
  const pixels = [];
  for (let px = 0; px < total; px += 1) {
    const a = Math.max(0, px - blur);
    const b = Math.min(x, px + 1 + blur);
    const sum = [0, 0, 0];
    for (const p of placed) {
      const overlap = Math.min(b, p.to) - Math.max(a, p.from);
      if (overlap > 0) for (let c = 0; c < 3; c += 1) sum[c] += overlap * p.rgb[c];
    }
    pixels.push(sum.map((v) => Math.round(v / (b - a))));
  }
  return pixels;
}

/**
 * Run-length encode a pixel row as [[r, g, b, count], ...].
 *
 * @param {number[][]} pixels
 * @returns {number[][]}
 */
export function encodeRle(pixels) {
  const out = [];
  for (const [r, g, b] of pixels) {
    const last = out[out.length - 1];
    if (last && last[0] === r && last[1] === g && last[2] === b) last[3] += 1;
    else out.push([r, g, b, 1]);
  }
  return out;
}

/**
 * @param {number[][]} rle [[r, g, b, count], ...]
 * @returns {number[][]}
 */
export function decodeRle(rle) {
  const out = [];
  for (const [r, g, b, n] of rle) for (let i = 0; i < n; i += 1) out.push([r, g, b]);
  return out;
}

#!/usr/bin/env node
/**
 * Builds `marker-golden.json` — the contract between the JS marker encoder and Phase
 * 2's Rust raster decoder (codec v2). The file is self-describing: `codec` states the
 * cell format and the exact pixel rules (`codec.pixel`, from marker-raster.mjs); `valid`
 * and `invalid` are cell-level vectors; `rows` are pixel rows (run-length encoded) with
 * the result list the reference row decoder produces for each.
 *
 * Plan `2026-10-11-terminal-page-performance-kpis-across-setups`, Phase 1.
 *
 * Usage:
 *   node scripts/termhost-bench/marker-golden.mjs --check   (default; exit 1 on drift)
 *   node scripts/termhost-bench/marker-golden.mjs --write
 */

import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import {
  BASE,
  CHECKSUM_DIGITS,
  CHECKSUM_MODULUS,
  CHECKSUM_MULTIPLIER,
  CODEC_VERSION,
  DECODE_REASONS,
  ECHO_REGION,
  END_DIGITS,
  LAYOUT,
  MARKER_CELLS,
  MARKER_KINDS,
  MARKER_MAX_HZ,
  MARKER_REGION,
  PALETTE,
  SEQ_DIGITS,
  SEQ_MODULUS,
  SESSION_DIGITS,
  SESSION_LIMIT,
  START_DIGITS,
  cellsSgr,
  decodeMarkerValues,
  digitsToValues,
  markerDigits,
  markerOverlay,
  markerSeq,
  markerValues,
  xtermRgb,
} from "./marker.mjs";
import {
  PIXEL_RULES,
  ROW_REASONS,
  encodeRle,
  findMarkersInRow,
  renderSpans,
} from "./marker-raster.mjs";
import { mulberry32 } from "./generator/content.mjs";

const HERE = dirname(fileURLToPath(import.meta.url));
export const GOLDEN_PATH = join(HERE, "marker-golden.json");
export const GOLDEN_SCHEMA = "termhost-bench/marker-golden/v2";

/**
 * The pixel rules in words: an exact algorithm a port follows step by step (marker-raster.mjs
 * is its reference implementation, and the golden rows are what it produced).
 */
export const PIXEL_RULE_TEXT = Object.freeze([
  "1. scope and geometry: the rules decode ONE pixel row, and which pixel rows of a captured frame to decode is Phase 2's (the probe's) to define; pixel i spans [i, i + 1); a segment over pixels start..end-1 spans [start, end); for segments A then B, gap(A, B) = B.start - A.end and mid(A, B) = (A.end + B.start) / 2; round(x) is the nearest integer with ties toward +infinity (JS Math.round; Rust f64::round is the same for x >= 0, and every x rounded here is >= 0); arithmetic is IEEE-754 binary64, evaluated as written",
  "2. classify each pixel: a channel <= channel_low_max is 0, >= channel_high_min is 1, anything else is impure; a pixel is pure when all three channels are, value = 4R + 2G + B",
  "3. segments: the maximal runs of pure pixels with one value, numbered left to right; impure pixels lie in the gaps between them",
  "4. first width at segment s, when s is red (4) and s+1 is cyan (3): b1 = mid(s, s+1); if segment s+2 exists, m = mid(s+1, s+2), cyan_wide = gap(s+1, s+2) >= boundary_gap_max * (m - b1) and b2 = cyan_wide ? (s+1).end : m; if it does not, cyan_wide = false and b2 = (s+1).end; w = b2 - b1",
  "5. start: s is a candidate when gap(s, s+1) < boundary_gap_max * w; a candidate with no segment s+2 is wrong_length; otherwise cells = [4, 3], left = b2, next = s + 2, in_gap = cyan_wide, and rules 6-7 repeat while there are fewer than 20 cells (need = 20 - cells)",
  "6. impure gap (in_gap): end = the start of segment next, or the row length when there is none; k = max(1, round((end - left) / w)); append min(k, need) impure cells (null); if all k were appended, w = (end - b1) / (cells - 1); then left = end and in_gap = false",
  "7. segment (not in_gap): if there is no segment next, wrong_length; else g = segment next and h = segment next+1 if any; trailing = (h ? h.start : row length) - g.end; wide = trailing >= boundary_gap_max * w; right = wide ? g.end : (h ? mid(g, h) : row length); span = (right - left) / w; k = round(span); if k >= need, append need cells of g's value, next = next + 1 and stop, leaving whatever lies beyond unchecked (cell 19 may merge into same-coloured content); else if k < 1 or |span - k| > width_tolerance, irregular_cells; else append k cells of g's value (a run k cells wide is k equal cells), w = (right - b1) / (cells - 1), left = right, in_gap = wide, next = next + 1",
  "8. result: the 20 cells (null = impure) go through the cell checks in decode_order, giving {ok: true, kind, session, seq} or {ok: false, reason}; a rejection in rule 5 or 7 is {ok: false, reason}",
  "9. scan: s = 0; while s + 1 < the segment count: if s is not a candidate, s = s + 1; else record its result, then continue at s = next when it decoded ok, or s = s + 1 when it was rejected; the results in order are the row's expected list ([] = no candidate)",
]);

// ---------------------------------------------------------------------------
// Cell-level vectors
// ---------------------------------------------------------------------------

const GOLDEN_VALID = [
  ["first output marker", 0, 0, 0],
  ["session 1 seq 1", 0, 1, 1],
  ["mid-run output", 0, 14, 1234],
  ["N = 40 ladder top, one hour at 10 Hz", 0, 39, 36000],
  [
    "largest session and seq (every session and seq digit 6)",
    0,
    SESSION_LIMIT - 1,
    SEQ_MODULUS - 1,
  ],
  ["seq 0 on session 3 (what a counter of SEQ_MODULUS reduces to)", 0, 3, markerSeq(SEQ_MODULUS)],
  ["first echo", 1, 0, 0],
  ["echo 99 on session 7", 1, 7, 99],
  ["echo seq above 7^5 on session 100", 1, 100, 50000],
  ["calibration session 0", 2, 0, 0],
  ["calibration session 39", 2, 39, 0],
  ["digit-boundary session 48 (066) and seq 16806 (066666)", 0, 48, 16806],
];

function goldenVector(name, kind, session, seq) {
  const digits = markerDigits({ kind, session, seq });
  const values = digitsToValues(digits);
  const indices = values.map((v) => PALETTE[v].index);
  return {
    name,
    kind,
    session,
    seq,
    digits,
    values,
    indices,
    rgb: values.map((v) => [...PALETTE[v].rgb]),
    cells_hex: Buffer.from(cellsSgr(indices), "latin1").toString("hex"),
    overlay_hex: Buffer.from(markerOverlay({ kind, session, seq }), "latin1").toString("hex"),
    echo_overlay_hex: Buffer.from(
      markerOverlay({ kind, session, seq }, ECHO_REGION),
      "latin1",
    ).toString("hex"),
  };
}

function invalidVector(name, indices, reason, note) {
  return { name, reason, note, indices, rgb: indices.map((i) => xtermRgb(i)) };
}

const rechain = (digits) => digitsToValues(digits).map((v) => PALETTE[v].index);

/** Index 88 = cube (2,0,0) = rgb(135,0,0): its red channel sits mid-band, unmistakably impure. */
export const OFF_PALETTE_INDEX = 88;

function buildInvalidVectors() {
  const base = markerDigits({ kind: 0, session: 14, seq: 1234 });
  const baseValues = digitsToValues(base);
  const baseIndices = baseValues.map((v) => PALETTE[v].index);
  const vectors = [];

  vectors.push(
    invalidVector(
      "19 cells",
      baseIndices.slice(0, MARKER_CELLS - 1),
      "wrong_length",
      "the last cell is missing (a clipped marker)",
    ),
  );

  const offPalette = [...baseIndices];
  offPalette[7] = OFF_PALETTE_INDEX;
  vectors.push(
    invalidVector(
      "off-palette cell",
      offPalette,
      "off_palette",
      "cell 7 is index 88, rgb(135,0,0): an impure red channel",
    ),
  );

  const adjacent = [...baseIndices];
  adjacent[9] = adjacent[8];
  vectors.push(
    invalidVector(
      "two adjacent cells share a colour",
      adjacent,
      "adjacent_cells_equal",
      "cell 9 repeats cell 8 (on pixels: one run two cells wide)",
    ),
  );

  const badStart = [...baseValues];
  badStart[0] = [2, 6, 5, 7, 1].find((v) => v !== badStart[1] && v !== 4);
  vectors.push(
    invalidVector(
      "start sentinel not red",
      badStart.map((v) => PALETTE[v].index),
      "bad_start_sentinel",
      "cell 0 is not red (on pixels there is then no candidate at all)",
    ),
  );

  const badEnd = [...base];
  badEnd[MARKER_CELLS - 1] = (END_DIGITS[END_DIGITS.length - 1] + 1) % BASE;
  vectors.push(
    invalidVector(
      "end sentinel digit altered",
      rechain(badEnd),
      "bad_end_sentinel",
      "the last end digit is wrong",
    ),
  );

  const reserved = markerDigits({ kind: 3, session: 14, seq: 1234 }, { rawKind: true });
  vectors.push(
    invalidVector(
      "reserved kind 3 with a correct checksum",
      rechain(reserved),
      "reserved_kind",
      "kind digit 3 is reserved",
    ),
  );

  const badSum = [...base];
  const sumAt = START_DIGITS.length + 1 + SESSION_DIGITS + SEQ_DIGITS;
  badSum[sumAt] = (badSum[sumAt] + 1) % BASE;
  vectors.push(
    invalidVector(
      "checksum digit altered",
      rechain(badSum),
      "checksum_mismatch",
      "first checksum digit +1",
    ),
  );

  const torn = [...baseValues];
  const cell = START_DIGITS.length + 2;
  torn[cell] = [0, 1, 2, 3, 4, 5, 6, 7].find(
    (v) =>
      v !== torn[cell] &&
      v !== torn[cell - 1] &&
      v !== torn[cell + 1] &&
      decodeMarkerValues(Object.assign([...torn], { [cell]: v })).reason === "checksum_mismatch",
  );
  vectors.push(
    invalidVector(
      "payload cell recoloured",
      torn.map((v) => PALETTE[v].index),
      "checksum_mismatch",
      `cell ${cell} recoloured in place, changing two digits`,
    ),
  );
  return vectors;
}

// ---------------------------------------------------------------------------
// Raster vectors
// ---------------------------------------------------------------------------

const DARK = [24, 24, 24];
const GREY = [128, 128, 128];
const BLACK = [0, 0, 0];
const WHITE = [255, 255, 255];
const rgbOf = (value, map = (c) => c) => PALETTE[value].rgb.map(map);
const pad = (width, rgb) => ({ width, rgb });

/** Seeded impure content (every pixel has a channel in 64..191), `width` px wide. */
function impureContent(width, seed) {
  const rng = mulberry32(seed);
  const spans = [];
  let left = width;
  while (left > 0) {
    const w = Math.min(left, 1 + (rng.nextU32() % 4));
    const rgb = [0, 1, 2].map(() => rng.nextU32() % 256);
    rgb[rng.nextU32() % 3] = 64 + (rng.nextU32() % 128);
    spans.push(pad(w, rgb));
    left -= w;
  }
  return spans;
}

/** Cell spans; `cells` holds palette values or explicit [r, g, b] colours. */
function cellSpans(cells, w, { map, widths } = {}) {
  return cells.map((c, i) => ({
    width: widths?.[i] ?? w,
    rgb: Array.isArray(c) ? c : rgbOf(c, map),
  }));
}

function summarizeResult(r) {
  return r.ok
    ? { ok: true, kind: r.kind, session: r.session, seq: r.seq }
    : { ok: false, reason: r.reason };
}

function rowVector(name, note, cellWidth, spans, { blur = 0 } = {}) {
  const pixels = renderSpans(spans, { blur });
  return {
    name,
    note,
    cell_width_px: cellWidth,
    blur_px: blur,
    width_px: pixels.length,
    rle: encodeRle(pixels),
    expected: findMarkersInRow(pixels).map(summarizeResult),
  };
}

/** The first seq (session 2, output) whose marker holds a red-then-cyan pair after cell 1. */
export function markerWithInnerStart() {
  for (let seq = 0; seq < SEQ_MODULUS; seq += 1) {
    const values = markerValues({ kind: 0, session: 2, seq });
    for (let at = 2; at + 1 < MARKER_CELLS; at += 1) {
      if (values[at] === 4 && values[at + 1] === 3) return { kind: 0, session: 2, seq, at };
    }
  }
  throw new Error("no marker holds an inner red-cyan pair");
}

/** The first seq (session 5, output) whose marker's last cell has `value`. */
export function markerEndingIn(value) {
  for (let seq = 0; seq < SEQ_MODULUS; seq += 1) {
    if (markerValues({ kind: 0, session: 5, seq })[MARKER_CELLS - 1] === value) {
      return { kind: 0, session: 5, seq };
    }
  }
  throw new Error(`no marker ends in value ${value}`);
}

/** Cells 0-9 of (session 3, seq s) with cells 10-19 of seq s + 1: a torn read the checksum catches. */
export function tornCells() {
  for (let seq = 500; seq < 600; seq += 1) {
    const a = markerValues({ kind: 0, session: 3, seq });
    const b = markerValues({ kind: 0, session: 3, seq: seq + 1 });
    const cells = [...a.slice(0, 10), ...b.slice(10)];
    if (decodeMarkerValues(cells).reason === "checksum_mismatch") return { seq, cells };
  }
  throw new Error("no torn pair found");
}

function buildRowVectors() {
  const mid = markerValues({ kind: 0, session: 14, seq: 1234 });
  const rows = [];
  rows.push(
    rowVector(
      "valid, 10 px cells, dark pane background",
      "a dark theme's background classifies as pure black; the marker still separates from it",
      10,
      [pad(16, DARK), ...cellSpans(mid, 10), pad(16, DARK)],
    ),
  );
  rows.push(
    rowVector("valid, 6 px cells (the smallest)", "", 6, [
      pad(12, GREY),
      ...cellSpans(mid, 6),
      pad(12, GREY),
    ]),
  );
  rows.push(
    rowVector(
      "valid, 13.333 px cells (133 % scaling), box-filtered edges",
      "fractional cell width: boundaries fall inside pixels",
      40 / 3,
      [pad(10, GREY), ...cellSpans(mid, 40 / 3), pad(10.5, GREY)],
    ),
  );
  rows.push(
    rowVector(
      "valid, 30 px cells (the largest), 2 px blur",
      "soft anti-aliased edges",
      30,
      [pad(20, GREY), ...cellSpans(mid, 30), pad(20, GREY)],
      { blur: 2 },
    ),
  );
  rows.push(
    rowVector(
      "valid, 7.5 px cells, 1 px blur",
      "",
      7.5,
      [pad(9, GREY), ...cellSpans(mid, 7.5), pad(9, GREY)],
      { blur: 1 },
    ),
  );
  rows.push(
    rowVector(
      "colour management: 0 -> 20 and 255 -> 235 still decodes",
      "every channel shifted 20 levels toward the middle",
      10,
      [pad(16, GREY), ...cellSpans(mid, 10, { map: (c) => (c === 0 ? 20 : 235) }), pad(16, GREY)],
    ),
  );
  const endsBlack = markerEndingIn(0);
  rows.push(
    rowVector(
      `last cell black on a black background (seq ${endsBlack.seq})`,
      "rule 7: cell 19 merges with a 37 px background into a 4.7-cell run; the 20-cell bound takes the one cell still needed, with no width check",
      10,
      [pad(16, BLACK), ...cellSpans(markerValues(endsBlack), 10), pad(37, BLACK)],
    ),
  );
  const endsWhite = markerEndingIn(7);
  rows.push(
    rowVector(
      `last cell white on a white background (seq ${endsWhite.seq})`,
      "rule 7: cell 19 merges with a 37 px background into a 4.7-cell run; the 20-cell bound takes the one cell still needed, with no width check",
      10,
      [pad(16, WHITE), ...cellSpans(markerValues(endsWhite), 10), pad(37, WHITE)],
    ),
  );
  rows.push(
    rowVector("valid between impure pane content", "seeded content, every pixel impure", 9, [
      ...impureContent(30, 11),
      ...cellSpans(mid, 9),
      ...impureContent(30, 12),
    ]),
  );
  rows.push(
    rowVector("two markers side by side (tiled panes)", "both decode, left to right", 8, [
      pad(8, DARK),
      ...cellSpans(markerValues({ kind: 0, session: 0, seq: 41 }), 8),
      ...impureContent(37, 13),
      ...cellSpans(markerValues({ kind: 1, session: 1, seq: 7 }), 8),
      pad(8, DARK),
    ]),
  );
  const adjacent = [...mid];
  adjacent[9] = adjacent[8];
  rows.push(
    rowVector("two equal adjacent cells", "one run two cells wide counts as two equal cells", 10, [
      pad(16, GREY),
      ...cellSpans(adjacent, 10),
      pad(16, GREY),
    ]),
  );
  const offPalette = [...mid];
  offPalette[7] = xtermRgb(OFF_PALETTE_INDEX);
  rows.push(
    rowVector(
      "off-palette cell (index 88, rgb(135,0,0))",
      "an impure stretch a cell wide is an impure cell",
      10,
      [pad(16, DARK), ...cellSpans(offPalette, 10), pad(16, DARK)],
    ),
  );
  const badStart = [...mid];
  badStart[0] = 2;
  rows.push(
    rowVector(
      "start sentinel damaged (cell 0 green)",
      "no red-then-cyan start: no candidate at all",
      10,
      [pad(16, GREY), ...cellSpans(badStart, 10), pad(16, GREY)],
    ),
  );
  rows.push(
    rowVector("clipped by the row end after 15 cells", "the row ends inside the marker", 10, [
      pad(16, GREY),
      ...cellSpans(mid.slice(0, 15), 10),
    ]),
  );
  const widths = Array(MARKER_CELLS).fill(10);
  widths[10] = 5;
  rows.push(
    rowVector("one cell half width", "a run 0.5 cells wide is not a whole number of cells", 10, [
      pad(16, GREY),
      ...cellSpans(mid, 10, { widths }),
      pad(16, GREY),
    ]),
  );
  const torn = tornCells();
  rows.push(
    rowVector(
      `torn read: cells 0-9 of seq ${torn.seq}, cells 10-19 of seq ${torn.seq + 1} (session 3)`,
      "the checksum catches a frame composited mid-update",
      10,
      [pad(16, GREY), ...cellSpans(torn.cells, 10), pad(16, GREY)],
    ),
  );
  rows.push(
    rowVector(
      "palette rendered at cube level 175 instead of 255",
      "an altered palette is rejected, never misdecoded: every 175 channel is impure",
      10,
      [pad(16, DARK), ...cellSpans(mid, 10, { map: (c) => (c === 255 ? 175 : c) }), pad(16, DARK)],
    ),
  );

  // Each row below pins one step of codec.pixel.rules: a decoder that departs from the
  // rule its note names gets that row wrong.
  rows.push(
    rowVector(
      "valid, 6.5 px cells, 2 px blur, quarter-pixel phase",
      "rule 4: b1 and b2 are gap MIDDLES, and the gap after cyan is judged against the distance from b1 to its middle, not against cyan's pure pixels",
      6.5,
      [pad(10.25, GREY), ...cellSpans(mid, 6.5), pad(11, GREY)],
      { blur: 2 },
    ),
  );
  const snapped = (cells) => cells.map((_, i) => (i % 2 === 0 ? 8 : 7));
  const triple = [...mid];
  triple[17] = triple[16];
  triple[18] = triple[16];
  rows.push(
    rowVector(
      "pixel-snapped 7/8 px cells, cells 16-18 equal",
      "rule 7: w is the running mean (7.47 px by cell 16), so the 23 px run is 3 equal cells: adjacent_cells_equal. Held to the cyan cell's 7 px it would be irregular_cells",
      7.5,
      [pad(12, GREY), ...cellSpans(triple, 7.5, { widths: snapped(triple) }), pad(12, GREY)],
    ),
  );
  const gapThenRun = [...mid];
  for (const at of [2, 3, 4, 5]) gapThenRun[at] = xtermRgb(OFF_PALETTE_INDEX);
  gapThenRun[7] = gapThenRun[6];
  gapThenRun[8] = gapThenRun[6];
  rows.push(
    rowVector(
      "pixel-snapped 7/8 px cells, cells 2-5 off-palette, cells 6-8 equal",
      "rules 5-6: the impure gap opens right after cyan and updates w to 7.4 px, so the 23 px run after it is 3 cells: off_palette. Without that update it would be irregular_cells",
      7.5,
      [
        pad(12, GREY),
        ...cellSpans(gapThenRun, 7.5, { widths: snapped(gapThenRun) }),
        pad(12, GREY),
      ],
    ),
  );
  rows.push(
    rowVector(
      "6 px cells, a 13 px two-cell run, then a 3 px impure stretch",
      "rules 6-7: the stretch is wide against the w before the run (3 >= 0.5 x 6) but rounds to 0 cells against the w after it (6.09 px), so max(1, ...) counts one impure cell: off_palette",
      6,
      [
        pad(12, GREY),
        ...cellSpans(mid.slice(0, 10), 6),
        pad(13, rgbOf(mid[10])),
        pad(3, GREY),
        ...cellSpans(mid.slice(12), 6),
        pad(12, GREY),
      ],
    ),
  );
  rows.push(
    rowVector(
      "10 px cells, exactly 5 px of impure pixels after cell 9",
      "rule 7: a gap of exactly boundary_gap_max x w is wide (>=), and round(0.5) = 1 impure cell: off_palette",
      10,
      [
        pad(16, GREY),
        ...cellSpans(mid.slice(0, 10), 10),
        pad(5, GREY),
        ...cellSpans(mid.slice(10), 10),
        pad(16, GREY),
      ],
    ),
  );
  rows.push(
    rowVector(
      "clipped inside the last cell: 4 px of cell 19, 2 impure px, then the row ends",
      "rule 7: with no segment after it and a narrow trailing gap, a segment's right boundary is the row end (span 0.6 cell, k = 1); the segment reaching cell 19 needs no width check, so it decodes",
      10,
      [pad(16, GREY), ...cellSpans(mid.slice(0, 19), 10), pad(4, rgbOf(mid[19])), pad(2, GREY)],
    ),
  );
  const strictStart = [
    pad(10, GREY),
    pad(8, rgbOf(mid[0])),
    pad(6, GREY),
    pad(8, rgbOf(mid[1])),
    pad(2, GREY),
  ];
  for (const v of mid.slice(2)) strictStart.push(pad(8, rgbOf(v)), pad(2, GREY));
  strictStart.push(pad(10, GREY));
  rows.push(
    rowVector(
      "red-cyan gap exactly half the first width",
      "rule 5: w = 12 px between gap middles, and a 6 px red-cyan gap is not < 0.5 x w (strict): no start, no candidate",
      10,
      strictStart,
    ),
  );
  const inner = markerWithInnerStart();
  const innerMarker = { kind: inner.kind, session: inner.session, seq: inner.seq };
  rows.push(
    rowVector(
      `valid with a red-cyan pair inside (cells ${inner.at}-${inner.at + 1}, seq ${inner.seq})`,
      "rule 9: scanning resumes after the segment that gave cell 19, so the pair inside an accepted marker starts no second candidate",
      10,
      [pad(16, GREY), ...cellSpans(markerValues(innerMarker), 10), pad(16, GREY)],
    ),
  );
  rows.push(
    rowVector(
      "8 px cells, one cell exactly 1.25 cells wide",
      "rule 7: |span / w - k| <= width_tolerance is inclusive, so a 10 px cell at w = 8 is one cell and the marker decodes",
      8,
      [
        pad(16, GREY),
        ...cellSpans(mid, 8, { widths: mid.map((_, i) => (i === 10 ? 10 : 8)) }),
        pad(16, GREY),
      ],
    ),
  );
  const cell2 = [...mid];
  cell2[2] = xtermRgb(OFF_PALETTE_INDEX);
  rows.push(
    rowVector(
      "off-palette cell 2, right after the start",
      "rules 4-6: the gap after cyan is a whole impure cell (cyan_wide), so decoding starts inside an impure gap: off_palette",
      10,
      [pad(16, GREY), ...cellSpans(cell2, 10), pad(16, GREY)],
    ),
  );
  const sliver = [0, 1, 2, 3, 4, 5, 6, 7].find((v) => v !== mid[9] && v !== mid[10]);
  const sliverRow = [pad(16, GREY)];
  mid.forEach((v, at) => {
    sliverRow.push(pad(7, rgbOf(v)));
    if (at === 9) sliverRow.push(pad(1, GREY), pad(1, rgbOf(sliver)), pad(1, GREY));
    else sliverRow.push(pad(3, GREY));
  });
  sliverRow.push(pad(16, GREY));
  rows.push(
    rowVector(
      "a 1 px pure sliver of another colour between cells 9 and 10",
      "rule 7: the sliver is a segment of its own, 0.2 cell wide; k = 0 fails k >= 1: irregular_cells",
      10,
      sliverRow,
    ),
  );
  const shifted = (low, high) => [
    pad(16, GREY),
    ...cellSpans(mid, 10, { map: (c) => (c === 0 ? low : high) }),
    pad(16, GREY),
  ];
  rows.push(
    rowVector(
      "colour management to the band edges: 0 -> 63 and 255 -> 192 still decodes",
      "rule 2: the bands are inclusive, channel_low_max (63) is 0 and channel_high_min (192) is 1",
      10,
      shifted(63, 192),
    ),
  );
  rows.push(
    rowVector(
      "one level past the low band: 0 -> 64 is impure",
      "rule 2: 64 is impure, so every cell with a 0 channel is impure and no red-cyan start remains: no candidate",
      10,
      shifted(64, 255),
    ),
  );
  rows.push(
    rowVector(
      "one level past the high band: 255 -> 191 is impure",
      "rule 2: 191 is impure, so every cell with a 255 channel is impure and no red-cyan start remains: no candidate",
      10,
      shifted(0, 191),
    ),
  );
  const off = xtermRgb(OFF_PALETTE_INDEX);
  rows.push(
    rowVector(
      "7 px cells with 1 px impure gaps, cell 2 9 px wide",
      "rule 7: a narrow gap's MIDDLE is the boundary, so cell 2 spans 10 px = 1.25 cells and the marker decodes; measured to the next segment's start it would be 10.5 px = 1.31 cells, irregular_cells",
      8,
      [
        pad(16, GREY),
        ...mid.flatMap((v, at) => [pad(at === 2 ? 9 : 7, rgbOf(v)), pad(1, GREY)]),
        pad(16, GREY),
      ],
    ),
  );
  rows.push(
    rowVector(
      "cells 0-5 of (session 1, seq 10), then a whole marker (session 2, seq 20)",
      "rule 9: the first candidate reads 6 + 14 cells and fails (bad_end_sentinel); scanning resumes at the segment after its red one, so the second marker's start is still found; resuming after the cells it read would miss it",
      10,
      [
        pad(16, GREY),
        ...cellSpans(markerValues({ kind: 0, session: 1, seq: 10 }).slice(0, 6), 10),
        ...cellSpans(markerValues({ kind: 0, session: 2, seq: 20 }), 10),
        pad(16, GREY),
      ],
    ),
  );
  rows.push(
    rowVector(
      "cells 17-19 equal, drawn as one 25 px run",
      "rule 1: round is the nearest integer with ties toward +infinity, so 25 / 10 = 2.5 is 3 cells: adjacent_cells_equal; half-even rounding would give 2 cells and irregular_cells",
      10,
      [pad(16, GREY), ...cellSpans(mid.slice(0, 17), 10), pad(25, rgbOf(mid[17])), pad(16, GREY)],
    ),
  );
  rows.push(
    rowVector(
      "cells 18-19 as 20 px of rgb(135,0,0) up to the row end",
      "rule 6: an impure gap that runs to the row end ends at the row length and still counts its cells: off_palette, not wrong_length",
      10,
      [pad(16, GREY), ...cellSpans(mid.slice(0, 18), 10), pad(20, off)],
    ),
  );
  rows.push(
    rowVector(
      "red 10 px, cyan 9 px, exactly 6 px impure, then 10 px cells",
      "rule 4: cyan_wide uses >=: the 6 px gap is exactly half of the 12 px from b1 to its middle, so cyan ends at its pure pixels (w = 9) and the 10 px cells after the impure cell are irregular_cells; with > the marker would decode",
      10,
      [
        pad(16, GREY),
        pad(10, rgbOf(mid[0])),
        pad(9, rgbOf(mid[1])),
        pad(6, GREY),
        ...cellSpans(mid.slice(2), 10),
        pad(16, GREY),
      ],
    ),
  );
  rows.push(
    rowVector(
      "the row ends 10 px after a 4 px cyan",
      "rule 4: with no segment after cyan, b2 is cyan's end (w = 5.5 px), so the 3 px red-cyan gap is no start: no candidate; b2 at the row end would make it a wrong_length candidate",
      10,
      [pad(16, GREY), pad(10, rgbOf(mid[0])), pad(3, GREY), pad(4, rgbOf(mid[1])), pad(10, GREY)],
    ),
  );
  rows.push(
    rowVector(
      "a 22 px two-cell run, a 15 px impure stretch, six cells, then the row ends",
      "rules 6-7: the stretch is counted with the w updated after the run (112 / 11 = 10.18 px): 1 impure cell, 19 cells, wrong_length; counted with the w before the run (10 px) it would be 2 cells and off_palette",
      10,
      [
        pad(16, GREY),
        ...cellSpans(mid.slice(0, 10), 10),
        pad(22, rgbOf(mid[10])),
        pad(15, GREY),
        ...cellSpans(mid.slice(13, 19), 10),
      ],
    ),
  );
  rows.push(
    rowVector(
      "cells 17-19 as 25 px of rgb(135,0,0) up to the row end",
      "rule 6: 25 / 10 = 2.5 rounds (ties toward +infinity) to 3 impure cells: off_palette; floor or half-even rounding give 2 cells and wrong_length",
      10,
      [pad(16, GREY), ...cellSpans(mid.slice(0, 17), 10), pad(25, off)],
    ),
  );
  rows.push(
    rowVector(
      "a 4 px red-cyan gap, a 6 px cyan, then an 8 px impure stretch",
      "rule 5: cyan_wide puts b2 at cyan's end, so w = 8 px and the 4 px gap is not < 0.5 x w: no start, no candidate; measured to the gap middle (12 px) it would start",
      10,
      [
        pad(16, GREY),
        pad(10, rgbOf(mid[0])),
        pad(4, GREY),
        pad(6, rgbOf(mid[1])),
        pad(8, GREY),
        ...cellSpans(mid.slice(2), 10),
        pad(16, GREY),
      ],
    ),
  );
  return rows;
}

// ---------------------------------------------------------------------------
// The file
// ---------------------------------------------------------------------------

/** The golden file contents. */
export function buildGolden() {
  return {
    schema: GOLDEN_SCHEMA,
    description:
      "GENERATED by scripts/termhost-bench/marker-golden.mjs --write — do not edit by hand. The " +
      "contract between the JS encoder and Phase 2's raster decoder. `valid` / `invalid` are " +
      "cell-level vectors: each valid one decodes to its (kind, session, seq) and each invalid " +
      "one is rejected with exactly its `reason`, checks running in `codec.decode_order`. `rows` " +
      "are pixel rows ([r, g, b, count] runs): decoding a row by `codec.pixel` must produce " +
      "exactly its `expected` list, left to right (empty = no candidate in the row). A row " +
      'whose `note` begins "rule N" pins one misreading of that step of `codec.pixel.rules`, ' +
      "named in the note: a decoder that misreads the step that way gets the row wrong. The " +
      "rows check a decoder; the rule text is the authority. Which pixel rows of a captured " +
      "frame to decode is Phase 2's to define.",
    codec: {
      version: CODEC_VERSION,
      cells: MARKER_CELLS,
      region: { ...MARKER_REGION },
      echo_region: { ...ECHO_REGION },
      max_hz: MARKER_MAX_HZ,
      palette: PALETTE.map((p) => ({
        value: p.value,
        index: p.index,
        rgb: [...p.rgb],
        name: p.name,
      })),
      base: BASE,
      virtual_predecessor: 0,
      digit_rule: "value[i] = (value[i-1] + 1 + digit[i]) mod 8, value[-1] = 0, digit in 0..6",
      layout: LAYOUT.map((f) =>
        f.fixed
          ? { name: f.name, digits: f.digits, fixed: [...f.fixed] }
          : { name: f.name, digits: f.digits },
      ),
      kinds: { ...MARKER_KINDS },
      session_limit: SESSION_LIMIT,
      seq_modulus: SEQ_MODULUS,
      checksum: {
        modulus: CHECKSUM_MODULUS,
        multiplier: CHECKSUM_MULTIPLIER,
        digits: CHECKSUM_DIGITS,
        rule: "h = 0; for d in kind,session,seq digits: h = (h * 31 + d + 1) mod 16787; written as 5 big-endian base-7 digits",
      },
      decode_order: [...DECODE_REASONS],
      overlay:
        "ESC 7, ESC [ row+1 ; col+1 H, then per cell ESC [ 48;5;<index> m SPACE, then ESC [ 0 m, ESC 8",
      pixel: {
        ...PIXEL_RULES,
        row_reasons: [...ROW_REASONS],
        rules: [...PIXEL_RULE_TEXT],
      },
    },
    valid: GOLDEN_VALID.map(([name, kind, session, seq]) => goldenVector(name, kind, session, seq)),
    invalid: buildInvalidVectors(),
    rows: buildRowVectors(),
  };
}

/** The exact bytes the committed golden file must hold. */
export function renderGolden() {
  return `${JSON.stringify(buildGolden(), null, 2)}\n`;
}

const invokedDirectly =
  process.argv[1] && resolve(process.argv[1]) === resolve(fileURLToPath(import.meta.url));
if (invokedDirectly) {
  const expected = renderGolden();
  if (process.argv.includes("--write")) {
    writeFileSync(GOLDEN_PATH, expected);
    process.stdout.write(`wrote ${GOLDEN_PATH}\n`);
  } else {
    let current = null;
    try {
      current = readFileSync(GOLDEN_PATH, "utf8");
    } catch {
      // absent counts as drift
    }
    if (current === expected) {
      process.stdout.write("marker-golden.json is in sync with marker-golden.mjs\n");
    } else {
      process.stderr.write("marker-golden.json is STALE: run marker-golden.mjs --write\n");
      process.exitCode = 1;
    }
  }
}

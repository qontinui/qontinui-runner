/**
 * termhost-bench marker codec (v2) — (kind, session, sequence) <-> 20 solid-background
 * terminal cells that an outside-in probe can read off a captured frame.
 *
 * Plan `2026-10-11-terminal-page-performance-kpis-across-setups`, Phase 1. This is
 * the ENCODER and the cell-level decoder. How a row of captured PIXELS becomes cells is
 * `marker-raster.mjs` (exact thresholds and tolerances), and `marker-golden.mjs` builds
 * `marker-golden.json`, which Phase 2's Rust probe must reproduce.
 *
 * Design, each choice for a robust read off a composited, scaled, colour-managed frame:
 *
 *   - Colours: only the 8 corners of the xterm 256-colour cube — every channel 0 or
 *     255 (indices 16, 21, 46, 51, 196, 201, 226, 231). Indices 16-231 are fixed in
 *     xterm-compatible hosts (colour schemes remap only 0-15), and with binary
 *     channels a decoder thresholds each channel independently, which survives
 *     anti-aliased edges, 100-200 % scaling and colour management shifting a channel
 *     by tens of levels (the pixel thresholds are 63 and 192).
 *   - Differential digits: cell i's colour is (colour[i-1] + 1 + digit[i]) mod 8,
 *     digits 0-6, from a virtual colour 0 before cell 0. Adjacent cells therefore
 *     ALWAYS differ, so every cell boundary of a valid marker is a colour edge; a pixel
 *     run k cells wide is k equal cells, which no valid marker contains.
 *   - Layout (20 digits, base 7): start sentinel [3, 6] (always red, cyan), kind (1),
 *     session (3, 0..342), sequence (6, mod 117649 = 3.27 h at 10 Hz), checksum (5),
 *     end sentinel [5, 1, 3].
 *   - Checksum: h = (h * 31 + digit + 1) mod 16787 (the largest prime below 7^5) over
 *     the kind, session and sequence digits, written as 5 base-7 digits. Any
 *     single-cell error is always detected (asserted exhaustively in the tests),
 *     and a torn read of two different markers passes with probability ~1/16787.
 *
 * Markers live in a reserved region — row 0, columns 0-19 — that every frame re-draws
 * until the next sequence supersedes it, at <= 10 Hz. Echo markers (the input arm)
 * live in row 1, columns 0-19, so an output tick never hides an echo the probe has
 * not seen yet. An overlay saves and restores the cursor (DECSC/DECRC), so it can be
 * appended to streaming text or inserted before the end of a synchronized repaint.
 *
 * Version 2 changed no cell or byte of a marker: it added the exact pixel rules
 * (marker-raster.mjs), raster vectors, and an off-palette vector a pixel decoder can
 * reproduce (index 88, rgb(135,0,0)).
 */

export const CODEC_VERSION = 2;
export const MARKER_CELLS = 20;
/** Output markers: row 0, columns 0-19 (0-based). */
export const MARKER_REGION = Object.freeze({ row: 0, col: 0, cells: MARKER_CELLS });
/** Echo markers (`--echo`): row 1, columns 0-19 (0-based). */
export const ECHO_REGION = Object.freeze({ row: 1, col: 0, cells: MARKER_CELLS });
/** Markers advance at most this often. */
export const MARKER_MAX_HZ = 10;

/** The 8 binary-channel cube colours. `value` is 4*R + 2*G + B with each channel 0/1. */
export const PALETTE = Object.freeze(
  ["black", "blue", "green", "cyan", "red", "magenta", "yellow", "white"].map((name, value) => {
    const r = (value >> 2) & 1;
    const g = (value >> 1) & 1;
    const b = value & 1;
    return Object.freeze({
      value,
      index: 16 + 180 * r + 30 * g + 5 * b,
      rgb: Object.freeze([255 * r, 255 * g, 255 * b]),
      name,
    });
  }),
);

const INDEX_TO_VALUE = new Map(PALETTE.map((p) => [p.index, p.value]));

/** Marker kinds. 3-6 are reserved and rejected by the decoder. */
export const MARKER_KINDS = Object.freeze({ output: 0, echo: 1, calibration: 2 });
const KIND_NAMES = Object.freeze(
  Object.fromEntries(Object.entries(MARKER_KINDS).map(([k, v]) => [v, k])),
);

export const BASE = 7;
export const START_DIGITS = Object.freeze([3, 6]);
export const END_DIGITS = Object.freeze([5, 1, 3]);
export const SESSION_DIGITS = 3;
export const SEQ_DIGITS = 6;
export const CHECKSUM_DIGITS = 5;
/** Sessions 0..342. */
export const SESSION_LIMIT = BASE ** SESSION_DIGITS;
/** A marker carries seq mod this (117649; 3.27 h at 10 Hz). */
export const SEQ_MODULUS = BASE ** SEQ_DIGITS;
/** The largest prime below 7^5. */
export const CHECKSUM_MODULUS = 16787;
export const CHECKSUM_MULTIPLIER = 31;

/** Field layout, left to right. */
export const LAYOUT = Object.freeze([
  Object.freeze({ name: "start", digits: START_DIGITS.length, fixed: START_DIGITS }),
  Object.freeze({ name: "kind", digits: 1 }),
  Object.freeze({ name: "session", digits: SESSION_DIGITS }),
  Object.freeze({ name: "seq", digits: SEQ_DIGITS }),
  Object.freeze({ name: "checksum", digits: CHECKSUM_DIGITS }),
  Object.freeze({ name: "end", digits: END_DIGITS.length, fixed: END_DIGITS }),
]);

/** Decoder rejection reasons, in the order the checks run. */
export const DECODE_REASONS = Object.freeze([
  "wrong_length",
  "off_palette",
  "adjacent_cells_equal",
  "bad_start_sentinel",
  "bad_end_sentinel",
  "reserved_kind",
  "checksum_mismatch",
]);

/**
 * @param {number} n
 * @param {number} width
 * @returns {number[]} big-endian base-7 digits
 */
function toBase7(n, width) {
  const out = new Array(width).fill(0);
  let v = n;
  for (let i = width - 1; i >= 0; i -= 1) {
    out[i] = v % BASE;
    v = Math.floor(v / BASE);
  }
  if (v !== 0) throw new RangeError(`${n} does not fit in ${width} base-7 digits`);
  return out;
}

function fromBase7(digits) {
  return digits.reduce((acc, d) => acc * BASE + d, 0);
}

/**
 * The checksum over the content digits (kind, session, seq).
 *
 * @param {number[]} contentDigits
 * @returns {number} in [0, CHECKSUM_MODULUS)
 */
export function checksum(contentDigits) {
  let h = 0;
  for (const d of contentDigits) h = (h * CHECKSUM_MULTIPLIER + d + 1) % CHECKSUM_MODULUS;
  return h;
}

/**
 * @param {{kind: number, session: number, seq: number}} marker
 */
function assertMarker({ kind, session, seq }) {
  if (!Number.isInteger(kind) || kind < 0 || kind > 2)
    throw new RangeError(`bad marker kind ${kind}`);
  if (!Number.isInteger(session) || session < 0 || session >= SESSION_LIMIT) {
    throw new RangeError(`session ${session} outside 0..${SESSION_LIMIT - 1}`);
  }
  if (!Number.isInteger(seq) || seq < 0 || seq >= SEQ_MODULUS) {
    throw new RangeError(`seq ${seq} outside 0..${SEQ_MODULUS - 1} (reduce it with markerSeq)`);
  }
}

/**
 * Reduce an unbounded sequence counter to what a marker carries.
 *
 * @param {number} counter
 * @returns {number}
 */
export function markerSeq(counter) {
  if (!Number.isInteger(counter) || counter < 0)
    throw new RangeError(`bad sequence counter ${counter}`);
  return counter % SEQ_MODULUS;
}

/**
 * The 20 digits of a marker (sentinels included). `rawKind` permits a reserved
 * kind digit; it exists only to build the golden file's reserved-kind vector.
 *
 * @param {{kind: number, session: number, seq: number}} marker
 * @param {{rawKind?: boolean}} [opts]
 * @returns {number[]}
 */
export function markerDigits(marker, { rawKind = false } = {}) {
  if (rawKind) assertMarker({ ...marker, kind: 0 });
  else assertMarker(marker);
  const content = [
    marker.kind,
    ...toBase7(marker.session, SESSION_DIGITS),
    ...toBase7(marker.seq, SEQ_DIGITS),
  ];
  return [
    ...START_DIGITS,
    ...content,
    ...toBase7(checksum(content), CHECKSUM_DIGITS),
    ...END_DIGITS,
  ];
}

/**
 * Digits -> palette values by the differential rule.
 *
 * @param {number[]} digits each 0..6
 * @returns {number[]} palette values 0..7
 */
export function digitsToValues(digits) {
  let prev = 0;
  return digits.map((d) => {
    if (!Number.isInteger(d) || d < 0 || d >= BASE) throw new RangeError(`bad digit ${d}`);
    prev = (prev + 1 + d) % 8;
    return prev;
  });
}

/**
 * Palette values -> digits (the inverse rule). A 7 means two adjacent cells share a colour.
 *
 * @param {number[]} values
 * @returns {number[]}
 */
export function valuesToDigits(values) {
  let prev = 0;
  return values.map((v) => {
    const d = (((v - prev - 1) % 8) + 8) % 8;
    prev = v;
    return d;
  });
}

/**
 * @param {{kind: number, session: number, seq: number}} marker
 * @returns {number[]} palette values
 */
export function markerValues(marker) {
  return digitsToValues(markerDigits(marker));
}

/**
 * @param {{kind: number, session: number, seq: number}} marker
 * @returns {number[]} xterm 256-colour indices
 */
export function markerIndices(marker) {
  return markerValues(marker).map((v) => PALETTE[v].index);
}

/**
 * The cell bytes: one space per cell under a 256-colour background.
 *
 * @param {number[]} indices
 * @returns {string}
 */
export function cellsSgr(indices) {
  return indices.map((i) => `\x1b[48;5;${i}m `).join("");
}

/**
 * A positioned marker overlay: save cursor, move to the region, paint the 20
 * cells, reset SGR, restore cursor.
 *
 * @param {{kind: number, session: number, seq: number}} marker
 * @param {{row: number, col: number}} [region] 0-based
 * @returns {string}
 */
export function markerOverlay(marker, region = MARKER_REGION) {
  return `\x1b7\x1b[${region.row + 1};${region.col + 1}H${cellsSgr(markerIndices(marker))}\x1b[0m\x1b8`;
}

/**
 * Decode palette values (0..7), running the checks in DECODE_REASONS order.
 *
 * @param {number[]} values
 * @returns {{ok: true, kind: number, kindName: string, session: number, seq: number} | {ok: false, reason: string}}
 */
export function decodeMarkerValues(values) {
  if (!Array.isArray(values) || values.length !== MARKER_CELLS)
    return { ok: false, reason: "wrong_length" };
  if (!values.every((v) => Number.isInteger(v) && v >= 0 && v < 8))
    return { ok: false, reason: "off_palette" };
  for (let i = 1; i < values.length; i += 1) {
    if (values[i] === values[i - 1]) return { ok: false, reason: "adjacent_cells_equal" };
  }
  const digits = valuesToDigits(values);
  if (digits[0] !== START_DIGITS[0] || digits[1] !== START_DIGITS[1]) {
    return { ok: false, reason: "bad_start_sentinel" };
  }
  const endAt = MARKER_CELLS - END_DIGITS.length;
  if (!END_DIGITS.every((d, i) => digits[endAt + i] === d))
    return { ok: false, reason: "bad_end_sentinel" };
  let at = START_DIGITS.length;
  const kind = digits[at];
  if (kind > 2) return { ok: false, reason: "reserved_kind" };
  const content = digits.slice(at, at + 1 + SESSION_DIGITS + SEQ_DIGITS);
  at += 1;
  const session = fromBase7(digits.slice(at, at + SESSION_DIGITS));
  at += SESSION_DIGITS;
  const seq = fromBase7(digits.slice(at, at + SEQ_DIGITS));
  at += SEQ_DIGITS;
  if (fromBase7(digits.slice(at, at + CHECKSUM_DIGITS)) !== checksum(content)) {
    return { ok: false, reason: "checksum_mismatch" };
  }
  return { ok: true, kind, kindName: KIND_NAMES[kind], session, seq };
}

/**
 * Decode xterm 256-colour indices; an index outside the 8-colour palette is off_palette.
 *
 * @param {number[]} indices
 */
export function decodeMarkerIndices(indices) {
  if (!Array.isArray(indices) || indices.length !== MARKER_CELLS)
    return { ok: false, reason: "wrong_length" };
  const values = indices.map((i) => INDEX_TO_VALUE.get(i));
  if (values.some((v) => v === undefined)) return { ok: false, reason: "off_palette" };
  return decodeMarkerValues(values);
}

/** The RGB an xterm-compatible host renders for a 256-colour index (16-255). */
export function xtermRgb(index) {
  if (index >= 16 && index <= 231) {
    const levels = [0, 95, 135, 175, 215, 255];
    const n = index - 16;
    return [levels[Math.floor(n / 36)], levels[Math.floor(n / 6) % 6], levels[n % 6]];
  }
  if (index >= 232 && index <= 255) {
    const v = 8 + 10 * (index - 232);
    return [v, v, v];
  }
  throw new RangeError(`index ${index} is theme-dependent (0-15) or out of range`);
}

const OVERLAY_RE = /\x1b7\x1b\[(\d+);(\d+)H((?:\x1b\[48;5;\d{1,3}m )+)\x1b\[0m\x1b8/g;
const CELL_RE = /\x1b\[48;5;(\d{1,3})m /g;

/**
 * Find every marker overlay in a generator byte stream (byte-level reading, for
 * tests and display-free checks; the probe reads pixels instead).
 *
 * @param {string | Buffer} stream
 * @returns {{row: number, col: number, indices: number[], decoded: object}[]} rows/cols 0-based
 */
export function parseOverlays(stream) {
  const text = typeof stream === "string" ? stream : stream.toString("utf8");
  const out = [];
  for (const m of text.matchAll(OVERLAY_RE)) {
    const indices = [...m[3].matchAll(CELL_RE)].map((c) => Number(c[1]));
    out.push({
      row: Number(m[1]) - 1,
      col: Number(m[2]) - 1,
      indices,
      decoded: decodeMarkerIndices(indices),
    });
  }
  return out;
}

/**
 * A stream with every marker overlay removed — the content a session would have
 * produced without the instrument (calibration conformance analyses this).
 *
 * @param {string | Buffer} stream
 * @returns {string}
 */
export function stripOverlays(stream) {
  const text = typeof stream === "string" ? stream : stream.toString("utf8");
  return text.replace(OVERLAY_RE, "");
}

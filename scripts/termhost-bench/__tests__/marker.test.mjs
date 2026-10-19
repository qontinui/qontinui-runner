/**
 * The marker codec: golden vectors byte-exact, decode/reject behaviour, the
 * robustness properties the format claims (single-cell errors always detected,
 * adjacent cells always distinct, binary-channel palette), and the codec-v2 pixel
 * rules — every raster vector decodes as the golden file says and as its name
 * promises, and markers survive 6-30 px cells, blur and colour management.
 *
 *   node --test scripts/termhost-bench/__tests__/marker.test.mjs
 */

import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

import {
  CHECKSUM_MODULUS,
  CODEC_VERSION,
  DECODE_REASONS,
  ECHO_REGION,
  MARKER_CELLS,
  MARKER_REGION,
  PALETTE,
  SEQ_MODULUS,
  SESSION_LIMIT,
  cellsSgr,
  checksum,
  decodeMarkerIndices,
  decodeMarkerValues,
  digitsToValues,
  markerDigits,
  markerIndices,
  markerOverlay,
  markerSeq,
  markerValues,
  parseOverlays,
  stripOverlays,
  valuesToDigits,
  xtermRgb,
} from "../marker.mjs";
import {
  PIXEL_RULES,
  ROW_REASONS,
  channelBit,
  classifyPixel,
  decodeRle,
  encodeRle,
  findMarkersInRow,
  renderSpans,
} from "../marker-raster.mjs";
import { GOLDEN_PATH, GOLDEN_SCHEMA, renderGolden } from "../marker-golden.mjs";
import { mulberry32 } from "../generator/content.mjs";

const golden = JSON.parse(readFileSync(GOLDEN_PATH, "utf8"));
const hex = (s) => Buffer.from(s, "latin1").toString("hex");

test("marker-golden.json is byte-identical to what the encoder generates", () => {
  assert.equal(
    readFileSync(GOLDEN_PATH, "utf8"),
    renderGolden(),
    "run: node scripts/termhost-bench/marker-golden.mjs --write",
  );
});

test("golden vectors: the encoder reproduces every digit, colour and byte", () => {
  assert.ok(golden.valid.length >= 10);
  for (const v of golden.valid) {
    const m = { kind: v.kind, session: v.session, seq: v.seq };
    assert.deepEqual(markerDigits(m), v.digits, v.name);
    assert.deepEqual(markerValues(m), v.values, v.name);
    assert.deepEqual(markerIndices(m), v.indices, v.name);
    assert.deepEqual(
      v.values.map((x) => PALETTE[x].rgb),
      v.rgb,
      v.name,
    );
    assert.equal(hex(cellsSgr(v.indices)), v.cells_hex, v.name);
    assert.equal(hex(markerOverlay(m)), v.overlay_hex, v.name);
    assert.equal(hex(markerOverlay(m, ECHO_REGION)), v.echo_overlay_hex, v.name);
  }
});

test("golden vectors: every valid one decodes, every invalid one is rejected with its reason", () => {
  for (const v of golden.valid) {
    const d = decodeMarkerIndices(v.indices);
    assert.deepEqual(
      { ok: d.ok, kind: d.kind, session: d.session, seq: d.seq },
      { ok: true, kind: v.kind, session: v.session, seq: v.seq },
      v.name,
    );
    assert.deepEqual(decodeMarkerValues(v.values), d, v.name);
  }
  const reasons = new Set();
  for (const v of golden.invalid) {
    assert.deepEqual(decodeMarkerIndices(v.indices), { ok: false, reason: v.reason }, v.name);
    assert.deepEqual(
      v.rgb,
      v.indices.map((i) => xtermRgb(i)),
      v.name,
    );
    reasons.add(v.reason);
  }
  assert.deepEqual(
    [...reasons].sort(),
    [...DECODE_REASONS].sort(),
    "the golden file exercises every rejection reason",
  );
});

test("the golden file documents the codec it was generated from", () => {
  const c = golden.codec;
  assert.equal(golden.schema, GOLDEN_SCHEMA);
  assert.equal(c.version, 2);
  assert.equal(CODEC_VERSION, 2);
  assert.equal(c.cells, MARKER_CELLS);
  assert.deepEqual(c.region, { row: 0, col: 0, cells: 20 });
  assert.deepEqual(c.echo_region, { row: 1, col: 0, cells: 20 });
  assert.equal(c.max_hz, 10);
  assert.equal(
    c.layout.reduce((a, f) => a + f.digits, 0),
    MARKER_CELLS,
  );
  assert.deepEqual(c.decode_order, [...DECODE_REASONS]);
  assert.equal(c.checksum.modulus, CHECKSUM_MODULUS);
});

test("the golden codec block carries the EXACT pixel rules a decoder reads", () => {
  const p = golden.codec.pixel;
  assert.deepEqual(
    {
      channel_low_max: p.channel_low_max,
      channel_high_min: p.channel_high_min,
      width_tolerance: p.width_tolerance,
      boundary_gap_max: p.boundary_gap_max,
    },
    { channel_low_max: 63, channel_high_min: 192, width_tolerance: 0.25, boundary_gap_max: 0.5 },
  );
  assert.deepEqual(
    { ...PIXEL_RULES },
    {
      channel_low_max: 63,
      channel_high_min: 192,
      width_tolerance: 0.25,
      boundary_gap_max: 0.5,
    },
  );
  assert.deepEqual(p.row_reasons, [...ROW_REASONS]);
  assert.equal(p.rules.length, 9);
  for (const key of Object.keys(PIXEL_RULES)) {
    assert.ok(
      p.rules.some((r) => r.includes(key)),
      `the rule text uses ${key} by name`,
    );
  }
  for (const reason of ROW_REASONS) {
    assert.ok(
      p.rules.some((r) => r.includes(reason)),
      `the rule text says when ${reason} applies`,
    );
  }
  assert.ok(
    p.rules[0].includes(
      "round(x) is the nearest integer with ties toward +infinity (JS Math.round; Rust f64::round",
    ),
    "rule 1 names the exact rounding function",
  );
  assert.match(p.rules[0], /which pixel rows of a captured frame to decode is Phase 2's/);
  assert.ok(!JSON.stringify(golden.codec).includes("~"), "no approximate numbers in the contract");
});

test("pixel classification uses exactly 63 and 192", () => {
  assert.equal(channelBit(0), 0);
  assert.equal(channelBit(63), 0);
  assert.equal(channelBit(64), -1);
  assert.equal(channelBit(191), -1);
  assert.equal(channelBit(192), 1);
  assert.equal(channelBit(255), 1);
  assert.equal(classifyPixel([255, 0, 0]), 4);
  assert.equal(
    classifyPixel([235, 20, 20]),
    4,
    "colour management inside the bands still classifies",
  );
  assert.equal(classifyPixel([0, 255, 255]), 3);
  assert.equal(classifyPixel([135, 0, 0]), -1, "index 88's red channel is impure");
  assert.equal(classifyPixel([128, 128, 128]), -1, "an anti-aliased edge is impure");
});

test("the off-palette golden vector is unmistakably off-palette on pixels too (index 88)", () => {
  const v = golden.invalid.find((x) => x.reason === "off_palette");
  assert.equal(v.indices[7], 88);
  assert.deepEqual(v.rgb[7], [135, 0, 0]);
  assert.equal(classifyPixel(v.rgb[7]), -1);
});

const summarize = (r) =>
  r.ok
    ? { ok: true, kind: r.kind, session: r.session, seq: r.seq }
    : { ok: false, reason: r.reason };

test("every golden raster row decodes to exactly its expected list", () => {
  assert.ok(golden.rows.length >= 40);
  for (const row of golden.rows) {
    const pixels = decodeRle(row.rle);
    assert.equal(pixels.length, row.width_px, row.name);
    assert.deepEqual(findMarkersInRow(pixels).map(summarize), row.expected, row.name);
  }
});

test("the golden raster rows exercise exactly what their names promise", () => {
  const byName = (prefix) => {
    const row = golden.rows.find((r) => r.name.startsWith(prefix));
    assert.ok(row, `no golden row "${prefix}"`);
    return row.expected;
  };
  const mid = [{ ok: true, kind: 0, session: 14, seq: 1234 }];
  for (const prefix of [
    "valid, 10 px",
    "valid, 6 px",
    "valid, 13.333 px",
    "valid, 30 px",
    "valid, 7.5 px",
    "colour management",
    "valid between impure",
  ]) {
    assert.deepEqual(byName(prefix), mid, prefix);
  }
  assert.deepEqual(
    byName("last cell black").map((r) => r.ok),
    [true],
  );
  assert.deepEqual(
    byName("last cell white").map((r) => r.ok),
    [true],
  );
  assert.deepEqual(byName("two markers side by side"), [
    { ok: true, kind: 0, session: 0, seq: 41 },
    { ok: true, kind: 1, session: 1, seq: 7 },
  ]);
  assert.deepEqual(byName("two equal adjacent cells"), [
    { ok: false, reason: "adjacent_cells_equal" },
  ]);
  assert.deepEqual(byName("off-palette cell"), [{ ok: false, reason: "off_palette" }]);
  assert.deepEqual(byName("start sentinel damaged"), []);
  assert.deepEqual(byName("clipped by the row end"), [{ ok: false, reason: "wrong_length" }]);
  assert.deepEqual(byName("one cell half width"), [{ ok: false, reason: "irregular_cells" }]);
  assert.deepEqual(byName("torn read"), [{ ok: false, reason: "checksum_mismatch" }]);
  assert.deepEqual(byName("palette rendered at cube level 175"), []);
  // The rows that pin one rule each (their notes name it).
  const rejected = (reason) => [{ ok: false, reason }];
  for (const prefix of [
    "valid, 6.5 px cells, 2 px blur",
    "clipped inside the last cell",
    "8 px cells, one cell exactly 1.25 cells wide",
    "colour management to the band edges",
  ]) {
    assert.deepEqual(byName(prefix), mid, prefix);
  }
  assert.deepEqual(
    byName("pixel-snapped 7/8 px cells, cells 16-18 equal"),
    rejected("adjacent_cells_equal"),
  );
  for (const prefix of [
    "pixel-snapped 7/8 px cells, cells 2-5 off-palette",
    "6 px cells, a 13 px two-cell run",
    "10 px cells, exactly 5 px of impure pixels",
    "off-palette cell 2",
  ]) {
    assert.deepEqual(byName(prefix), rejected("off_palette"), prefix);
  }
  assert.deepEqual(byName("a 1 px pure sliver"), rejected("irregular_cells"));
  for (const prefix of [
    "red-cyan gap exactly half the first width",
    "one level past the low band",
    "one level past the high band",
  ]) {
    assert.deepEqual(byName(prefix), [], prefix);
  }
  assert.deepEqual(byName("valid with a red-cyan pair inside"), [
    { ok: true, kind: 0, session: 2, seq: 7 },
  ]);
  assert.deepEqual(byName("7 px cells with 1 px impure gaps"), mid);
  assert.deepEqual(byName("cells 0-5 of (session 1, seq 10), then a whole marker"), [
    { ok: false, reason: "bad_end_sentinel" },
    { ok: true, kind: 0, session: 2, seq: 20 },
  ]);
  assert.deepEqual(
    byName("cells 17-19 equal, drawn as one 25 px run"),
    rejected("adjacent_cells_equal"),
  );
  for (const prefix of [
    "cells 18-19 as 20 px of rgb(135,0,0) up to the row end",
    "cells 17-19 as 25 px of rgb(135,0,0) up to the row end",
  ]) {
    assert.deepEqual(byName(prefix), rejected("off_palette"), prefix);
  }
  assert.deepEqual(
    byName("red 10 px, cyan 9 px, exactly 6 px impure"),
    rejected("irregular_cells"),
  );
  assert.deepEqual(
    byName("a 22 px two-cell run, a 15 px impure stretch"),
    rejected("wrong_length"),
  );
  for (const prefix of [
    "the row ends 10 px after a 4 px cyan",
    "a 4 px red-cyan gap, a 6 px cyan",
  ]) {
    assert.deepEqual(byName(prefix), [], prefix);
  }
  for (const row of golden.rows.slice(17)) {
    assert.match(row.note, /^rules? \d/, `${row.name}: a rule-pinning row names its rule`);
  }
});

const span = (width, rgb) => ({ width, rgb });
const cells = (values, w, map = (c) => c) =>
  values.map((v) => span(w, Array.isArray(v) ? v : PALETTE[v].rgb.map(map)));

test("every valid golden marker decodes from pixels at 6-30 px cells with 0-2 px blur", () => {
  for (const v of golden.valid) {
    for (const w of [6, 7.5, 9, 10, 12.5, 40 / 3, 16, 20, 30]) {
      for (const blur of [0, 0.5, 1, 2]) {
        const pixels = renderSpans(
          [span(11, [96, 96, 96]), ...cells(v.values, w), span(11, [96, 96, 96])],
          {
            blur,
          },
        );
        assert.deepEqual(
          findMarkersInRow(pixels).map(summarize),
          [{ ok: true, kind: v.kind, session: v.session, seq: v.seq }],
          `${v.name} at ${w} px, blur ${blur}`,
        );
      }
    }
  }
});

test("a pixel run k cells wide counts as k equal cells (adjacent_cells_equal, not a short marker)", () => {
  const values = markerValues({ kind: 0, session: 14, seq: 1234 });
  for (const at of [5, 9, 17]) {
    const equal = [...values];
    equal[at + 1] = equal[at];
    for (const w of [6, 10, 40 / 3]) {
      const pixels = renderSpans([
        span(10, [96, 96, 96]),
        ...cells(equal, w),
        span(10, [96, 96, 96]),
      ]);
      assert.deepEqual(
        findMarkersInRow(pixels).map(summarize),
        [{ ok: false, reason: "adjacent_cells_equal" }],
        `cells ${at},${at + 1} at ${w} px`,
      );
    }
  }
});

test("the 20-cell bound reads a last cell that merges into same-coloured content", () => {
  for (let seq = 0, found = 0; found < 8 && seq < 5000; seq += 1) {
    const values = markerValues({ kind: 0, session: 9, seq });
    const last = values[MARKER_CELLS - 1];
    const pixels = renderSpans([
      span(10, [96, 96, 96]),
      ...cells(values, 9),
      span(120, PALETTE[last].rgb),
    ]);
    assert.deepEqual(
      findMarkersInRow(pixels).map(summarize),
      [{ ok: true, kind: 0, session: 9, seq }],
      `seq ${seq}`,
    );
    found += 1;
  }
});

test("colour-altered markers are rejected, never misdecoded", () => {
  const alterations = [
    (c) => (c === 255 ? 160 : c), // a bright channel pushed into the impure band
    (c) => (c === 0 ? 100 : c), // a dark channel pushed into the impure band
  ];
  for (const v of golden.valid) {
    for (const alter of alterations) {
      const pixels = renderSpans([
        span(10, [96, 96, 96]),
        ...cells(v.values, 10, alter),
        span(10, [96, 96, 96]),
      ]);
      assert.deepEqual(findMarkersInRow(pixels), [], v.name);
    }
    const swapped = v.values.map((x) => [PALETTE[x].rgb[0], PALETTE[x].rgb[2], PALETTE[x].rgb[1]]);
    const decoded = findMarkersInRow(
      renderSpans([span(10, [96, 96, 96]), ...cells(swapped, 10), span(10, [96, 96, 96])]),
    );
    for (const r of decoded) {
      assert.ok(
        !r.ok || (r.session === v.session && r.seq === v.seq && r.kind === v.kind),
        `${v.name}: swapped G/B misdecoded`,
      );
    }
  }
});

test("run-length encoding round-trips a rendered row", () => {
  const pixels = renderSpans(
    [span(3.5, [10, 20, 30]), span(2.25, [200, 100, 0]), span(4, [0, 0, 0])],
    { blur: 0.5 },
  );
  assert.deepEqual(decodeRle(encodeRle(pixels)), pixels);
  assert.equal(pixels.length, 10);
});

test("palette: 8 cube corners, every channel 0 or 255, indices in 16..231", () => {
  assert.equal(PALETTE.length, 8);
  assert.equal(new Set(PALETTE.map((p) => p.index)).size, 8);
  for (const p of PALETTE) {
    assert.ok(p.index >= 16 && p.index <= 231, `${p.name} ${p.index}`);
    assert.ok(
      p.rgb.every((c) => c === 0 || c === 255),
      p.name,
    );
    assert.deepEqual(
      xtermRgb(p.index),
      [...p.rgb],
      `${p.name}: index ${p.index} renders as its rgb in xterm-compatible hosts`,
    );
  }
  assert.deepEqual(
    PALETTE.map((p) => p.index),
    [16, 21, 46, 51, 196, 201, 226, 231],
  );
});

test("every marker is 20 cells, adjacent cells differ, and it starts red-cyan", () => {
  const rng = mulberry32(42);
  for (let i = 0; i < 2000; i += 1) {
    const m = {
      kind: rng.nextU32() % 3,
      session: rng.nextU32() % SESSION_LIMIT,
      seq: rng.nextU32() % SEQ_MODULUS,
    };
    const values = markerValues(m);
    assert.equal(values.length, MARKER_CELLS);
    for (let k = 1; k < values.length; k += 1) assert.notEqual(values[k], values[k - 1]);
    assert.deepEqual(values.slice(0, 2), [4, 3], "red, cyan");
    const d = decodeMarkerValues(values);
    assert.deepEqual({ kind: d.kind, session: d.session, seq: d.seq }, m, JSON.stringify(m));
  }
});

test("any single-cell error is detected: every position, every other colour", () => {
  const markers = [
    { kind: 0, session: 0, seq: 0 },
    { kind: 0, session: 14, seq: 1234 },
    { kind: 1, session: 342, seq: 117648 },
    { kind: 2, session: 39, seq: 0 },
    { kind: 0, session: 7, seq: 65432 },
  ];
  let trials = 0;
  for (const m of markers) {
    const values = markerValues(m);
    for (let cell = 0; cell < MARKER_CELLS; cell += 1) {
      for (let v = 0; v < 8; v += 1) {
        if (v === values[cell]) continue;
        const bad = [...values];
        bad[cell] = v;
        const d = decodeMarkerValues(bad);
        assert.equal(
          d.ok,
          false,
          `${JSON.stringify(m)} cell ${cell} -> ${v} decoded as ${JSON.stringify(d)}`,
        );
        trials += 1;
      }
    }
  }
  assert.equal(trials, markers.length * MARKER_CELLS * 7);
});

test("the checksum modulus is the largest prime below 7^5", () => {
  const isPrime = (n) => {
    for (let d = 2; d * d <= n; d += 1) if (n % d === 0) return false;
    return n > 1;
  };
  assert.ok(isPrime(CHECKSUM_MODULUS));
  for (let n = CHECKSUM_MODULUS + 1; n < 7 ** 5; n += 1)
    assert.ok(!isPrime(n), `${n} is a larger prime`);
  assert.ok(
    checksum([0, 0, 0, 0, 0, 0, 0, 0, 0, 0]) > 0,
    "an all-zero payload still has a non-zero checksum",
  );
});

test("the digit rule is invertible", () => {
  const digits = [3, 6, 0, 1, 2, 3, 4, 5, 6, 0, 0, 6, 6, 1, 2, 3, 4, 5, 1, 3];
  assert.deepEqual(valuesToDigits(digitsToValues(digits)), digits);
  assert.throws(() => digitsToValues([7]), RangeError);
});

test("out-of-range markers are refused, and a counter reduces to the marker's seq", () => {
  assert.throws(() => markerDigits({ kind: 3, session: 0, seq: 0 }), RangeError);
  assert.throws(() => markerDigits({ kind: 0, session: SESSION_LIMIT, seq: 0 }), RangeError);
  assert.throws(() => markerDigits({ kind: 0, session: 0, seq: SEQ_MODULUS }), /markerSeq/);
  assert.equal(markerSeq(SEQ_MODULUS + 5), 5);
  assert.equal(markerSeq(0), 0);
  assert.throws(() => markerSeq(-1), RangeError);
  assert.deepEqual(decodeMarkerIndices([1, 2, 3]), { ok: false, reason: "wrong_length" });
  assert.deepEqual(decodeMarkerValues(null), { ok: false, reason: "wrong_length" });
});

test("an overlay saves the cursor, paints its region, resets SGR and restores the cursor", () => {
  const m = { kind: 0, session: 2, seq: 9 };
  const out = markerOverlay(m);
  assert.ok(out.startsWith("\x1b7\x1b[1;1H"), "output markers: row 0 -> CSI row 1");
  assert.ok(out.endsWith("\x1b[0m\x1b8"));
  assert.ok(
    markerOverlay(m, ECHO_REGION).startsWith("\x1b7\x1b[2;1H"),
    "echo markers: row 1 -> CSI row 2",
  );
  assert.equal((out.match(/\x1b\[48;5;\d+m /g) ?? []).length, MARKER_CELLS);
  assert.equal(MARKER_REGION.cells, 20);
  assert.ok(Buffer.byteLength(out) <= 260, "an overlay is a small write");
});

test("parseOverlays finds and decodes markers in a stream; stripOverlays removes exactly them", () => {
  const content = "\x1b[?2026h\x1b[Hhello\x1b[?2026l";
  const stream = `${content}${markerOverlay({ kind: 0, session: 5, seq: 77 })}tail${markerOverlay({ kind: 1, session: 5, seq: 3 }, ECHO_REGION)}`;
  const found = parseOverlays(stream);
  assert.equal(found.length, 2);
  assert.deepEqual(
    [found[0].row, found[0].col, found[0].decoded.kindName, found[0].decoded.seq],
    [0, 0, "output", 77],
  );
  assert.deepEqual(
    [found[1].row, found[1].decoded.kindName, found[1].decoded.session],
    [1, "echo", 5],
  );
  assert.equal(stripOverlays(stream), `${content}tail`);
  assert.deepEqual(parseOverlays(Buffer.from(stream)), found);
});

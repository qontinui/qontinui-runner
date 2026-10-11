/**
 * The marker codec: golden vectors byte-exact, decode/reject behaviour, and the
 * robustness properties the format claims (single-cell errors always detected,
 * adjacent cells always distinct, binary-channel palette).
 *
 *   node --test scripts/termhost-bench/__tests__/marker.test.mjs
 */

import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

import {
  CHECKSUM_MODULUS,
  DECODE_REASONS,
  ECHO_REGION,
  GOLDEN_PATH,
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
  renderGolden,
  stripOverlays,
  valuesToDigits,
  xtermRgb,
} from "../marker.mjs";
import { mulberry32 } from "../generator/content.mjs";

const golden = JSON.parse(readFileSync(GOLDEN_PATH, "utf8"));
const hex = (s) => Buffer.from(s, "latin1").toString("hex");

test("marker-golden.json is byte-identical to what the encoder generates", () => {
  assert.equal(
    readFileSync(GOLDEN_PATH, "utf8"),
    renderGolden(),
    "run: node scripts/termhost-bench/marker.mjs --write-golden",
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

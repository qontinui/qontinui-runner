/**
 * Unit tests for the TUI repaint load generator's pure frame builders.
 *
 *   node --test scripts/__tests__/tui-repaint-generator.test.mjs
 */

import { test } from "node:test";
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import {
  SYNC_BEGIN,
  SYNC_END,
  CURSOR_HOME,
  buildRepaintFrame,
  buildSpinnerFrame,
  buildTypingTrace,
  buildCorpus,
  parseGeneratorArgs,
} from "../tui-repaint-generator.mjs";

const SCRIPT = join(dirname(fileURLToPath(import.meta.url)), "..", "tui-repaint-generator.mjs");

/** Strip CSI sequences, leaving the painted text. */
const stripCsi = (s) => s.replace(/\x1b\[[0-9;?]*[A-Za-z]/g, "");

/** Split a frame into the text painted after each `ESC[<row>;1H`. */
function paintedRows(frame) {
  const rows = new Map();
  const re = /\x1b\[(\d+);1H/g;
  const marks = [...frame.matchAll(re)];
  marks.forEach((m, i) => {
    const end = i + 1 < marks.length ? marks[i + 1].index : frame.length;
    rows.set(Number(m[1]), stripCsi(frame.slice(m.index + m[0].length, end)));
  });
  return rows;
}

test("a repaint frame is one sync block that homes the cursor first", () => {
  const f = buildRepaintFrame({ cols: 40, rows: 5 });
  assert.ok(f.startsWith(SYNC_BEGIN + CURSOR_HOME));
  assert.ok(f.endsWith(SYNC_END));
  assert.equal(f.indexOf(SYNC_BEGIN, 1), -1, "exactly one sync block");
});

test("a repaint frame paints every row across exactly `cols` columns", () => {
  const cols = 57;
  const rows = 9;
  const painted = paintedRows(buildRepaintFrame({ cols, rows, frame: 3 }));
  assert.deepEqual(
    [...painted.keys()],
    Array.from({ length: rows }, (_, i) => i + 1),
  );
  for (const text of painted.values()) assert.equal([...text].length, cols);
});

test("a repaint frame is SGR-heavy (truecolor fg+bg runs of at most 12 columns)", () => {
  const f = buildRepaintFrame({ cols: 120, rows: 40 });
  const styles = f.match(/\x1b\[(?:[13];)?38;2;\d+;\d+;\d+;48;2;\d+;\d+;\d+m/g) ?? [];
  assert.ok(styles.length >= (120 * 40) / 12, `only ${styles.length} style runs`);
  const bytes = Buffer.byteLength(f);
  assert.ok(bytes > 20000 && bytes < 49152, `120x40 frame is ${bytes} bytes`);
});

test("frames are deterministic per index and differ across indices", () => {
  const a = buildRepaintFrame({ cols: 80, rows: 24, frame: 7 });
  assert.equal(a, buildRepaintFrame({ cols: 80, rows: 24, frame: 7 }));
  assert.notEqual(a, buildRepaintFrame({ cols: 80, rows: 24, frame: 8 }));
});

test("a spinner frame repaints only the last row, full width, in a sync block", () => {
  const f = buildSpinnerFrame({ cols: 60, rows: 20, frame: 2 });
  assert.ok(f.startsWith(SYNC_BEGIN));
  assert.ok(f.endsWith(SYNC_END));
  const painted = paintedRows(f);
  assert.deepEqual([...painted.keys()], [20]);
  assert.equal([...painted.get(20)].length, 60);
  assert.ok(Buffer.byteLength(f) < 400, "a spinner frame is small");
});

test("a spinner frame truncates to a narrow terminal", () => {
  const painted = paintedRows(buildSpinnerFrame({ cols: 5, rows: 2, frame: 0 }));
  assert.equal([...painted.get(2)].length, 5);
});

test("the typing trace is small writes with periodic prompts", () => {
  const trace = buildTypingTrace({ chunks: 100 });
  assert.equal(trace.length, 100);
  assert.ok(trace.every((c) => Buffer.byteLength(c) <= 16));
  assert.equal(trace.filter((c) => c.includes("$")).length, 2);
});

test("the corpus covers the three geometries plus typing", () => {
  const corpus = buildCorpus({ framesPerSet: 2 });
  assert.deepEqual(
    corpus.map((s) => [s.name, s.frames.length]),
    [
      ["repaint-120x40", 2],
      ["repaint-200x60", 2],
      ["repaint-250x80", 2],
      ["typing", 400],
    ],
  );
});

test("bad geometry is refused", () => {
  assert.throws(() => buildRepaintFrame({ cols: 0, rows: 10 }), RangeError);
  assert.throws(() => buildSpinnerFrame({ cols: 10, rows: 1.5 }), RangeError);
});

test("parseGeneratorArgs reads every flag and refuses junk", () => {
  assert.deepEqual(
    parseGeneratorArgs([
      "--fps",
      "60",
      "--cols",
      "200",
      "--rows",
      "50",
      "--spinner",
      "--alt-screen",
      "--duration-ms",
      "500",
    ]),
    {
      fps: 60,
      cols: 200,
      rows: 50,
      spinner: true,
      altScreen: true,
      durationMs: 500,
      corpusOut: null,
      help: false,
    },
  );
  assert.equal(parseGeneratorArgs([]).fps, 30);
  assert.throws(() => parseGeneratorArgs(["--fps", "0"]), /positive/);
  assert.throws(() => parseGeneratorArgs(["--cols"]), /needs a value/);
  assert.throws(() => parseGeneratorArgs(["--bogus"]), /unknown argument/);
});

test("the CLI emits sync-wrapped frames and stops at --duration-ms", () => {
  const r = spawnSync(
    process.execPath,
    [SCRIPT, "--fps", "50", "--cols", "20", "--rows", "4", "--duration-ms", "200"],
    { encoding: "utf8", timeout: 10000 },
  );
  assert.equal(r.status, 0, r.stderr);
  const begins = r.stdout.split(SYNC_BEGIN).length - 1;
  const ends = r.stdout.split(SYNC_END).length - 1;
  assert.ok(begins >= 1, "at least one frame");
  assert.equal(begins, ends, "every frame closes its sync block");
});

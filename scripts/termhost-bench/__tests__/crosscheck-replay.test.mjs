/**
 * The cross-check replay tool (`tools/crosscheck-replay.mjs`) on synthetic probe
 * watch files: its port of `analysis.rs` `crosscheck` reaches the same verdicts
 * the Rust tests pin, and its CLI reads a file path.
 *
 *   node --test scripts/termhost-bench/__tests__/crosscheck-replay.test.mjs
 */

import { test } from "node:test";
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

import { parseWatch, replay, spacingP50 } from "../tools/crosscheck-replay.mjs";

const TOOL = fileURLToPath(new URL("../tools/crosscheck-replay.mjs", import.meta.url));
const MS = 1_000_000n;

/**
 * A watch file as the probe writes it: the desktop presents every `presentEvery`
 * refreshes (R = 16.667 ms); DDA captures each presentation; WGC every
 * `wgcEvery`-th, stamped `stampMs` later, showing content visible `renderMs`
 * before it. Markers every 100 ms at a walking phase.
 */
function watchText({ presentEvery = 1, wgcEvery = 1, renderMs = 0, stampMs = 5 } = {}) {
  const R = 16_666_667n;
  const period = R * BigInt(presentEvery);
  const t0 = 1_000n * MS;
  const emit = Array.from({ length: 200 }, (_, i) => t0 + 500n * MS + BigInt(i) * 100n * MS + ((BigInt(i) * 7_919_000n) % period));
  const latest = (cut) => {
    let s = -1;
    for (let i = 0; i < emit.length && emit[i] <= cut; i++) s = i;
    return s;
  };
  const lines = [];
  const first = { wgc: new Map(), dda: new Map() };
  const frame = (source, index, t, seq) => {
    lines.push({ schema: "termhost-probe/v1", type: "frame", source, index, t_ns: String(t), missed: source === "dda" ? 0 : undefined, markers: seq < 0 ? [] : [{ kind: "output", session: 0, seq }] });
    if (seq >= 0 && !first[source].has(seq)) {
      first[source].set(seq, true);
      lines.push({ schema: "termhost-probe/v1", type: "first_seen", source, kind: "output", session: 0, seq, t_ns: String(t), frame: index });
    }
  };
  for (let k = 0n; t0 + k * period <= t0 + 21_000n * MS; k++) {
    const t = t0 + k * period;
    frame("dda", Number(k), t, latest(t));
    if (k % BigInt(wgcEvery) === 0n) frame("wgc", Number(k), t + BigInt(stampMs) * MS, latest(t - BigInt(renderMs) * MS));
  }
  lines.push({ schema: "termhost-probe/v1", type: "summary", primary: "wgc", secondary: "dda", refresh_ms: 16.666667 });
  return lines.map((l) => JSON.stringify(l)).join("\n");
}

test("a sound 60 Hz watch agrees, F from DDA's spacing", () => {
  const r = replay(parseWatch(watchText()));
  assert.equal(r.verdict, "agree", JSON.stringify(r));
  assert.equal(r.compared, 200);
  assert.equal(r.medianGapMs, 5);
  assert.ok(Math.abs(r.frameMs - 16.667) < 1e-3);
});

test("WGC dropping every other frame disagrees on cadence; its own spacing does not widen F", () => {
  const r = replay(parseWatch(watchText({ wgcEvery: 2 })));
  assert.equal(r.verdict, "disagree", JSON.stringify(r));
  assert.ok(r.why.includes("cadence"));
  assert.ok(Math.abs(r.cadenceRatio - 2) < 1e-3);
  assert.ok(Math.abs(r.frameMs - 16.667) < 1e-3);
});

test("the hosted-VM shape agrees through the measured frame interval, and the retired rule counts what it voided", () => {
  const r = replay(parseWatch(watchText({ presentEvery: 2, renderMs: 10, stampMs: 0 })));
  assert.equal(r.verdict, "agree", JSON.stringify(r));
  assert.ok(Math.abs(r.frameMs - 33.333) < 1e-3);
  assert.equal(r.outside, 0);
  assert.ok(r.retired.overOneRefresh > 40, "about 30% of markers a whole presentation apart");
  assert.equal(r.retired.primaryLater, r.retired.overOneRefresh);
  // Here a WGC frame DID come at DDA's time, rendered before the marker arrived
  // (the render delay), so none count as "no WGC frame between"; in CI run
  // 38362046020 all 359 did (the VM's sampling phase).
  assert.equal(r.retired.noPrimaryFrameBetween, 0);
  // Judged in refresh intervals, the same watch would be voided by its tail.
  const w = parseWatch(watchText({ presentEvery: 2, renderMs: 10, stampMs: 0 }));
  w.frames.set("dda", w.frames.get("dda").slice(0, 5)); // no trusted spacing: F falls back to R
  assert.equal(replay(w).verdict, "disagree");
});

test("a spacing p50 needs 20 deltas", () => {
  const f = (n) => Array.from({ length: n }, (_, i) => ({ t: BigInt(i) * 16n * MS }));
  assert.equal(spacingP50(f(20)), null);
  assert.equal(spacingP50(f(21)), 16);
});

test("the CLI reads a probe watch file and prints one verdict line per file", (t) => {
  const dir = mkdtempSync(join(tmpdir(), "tb-replay-"));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  const file = join(dir, "probe-tiled-soak.jsonl");
  writeFileSync(file, watchText({ wgcEvery: 2 }));
  const out = spawnSync(process.execPath, [TOOL, file, "--json"], { encoding: "utf8" });
  assert.equal(out.status, 0, out.stderr);
  const row = JSON.parse(out.stdout.trim());
  assert.equal(row.verdict, "disagree");
  assert.equal(row.file, file);
  const usage = spawnSync(process.execPath, [TOOL], { encoding: "utf8" });
  assert.equal(usage.status, 2);
});

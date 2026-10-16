/**
 * The cross-check replay tool (`tools/crosscheck-replay.mjs`) on synthetic probe
 * watch files: its port of `analysis.rs` `crosscheck` reaches the verdicts the
 * Rust model tests pin on the same timelines (third review: presented
 * timelines, F derived from DDA's frames), and its CLI reads a file path and
 * refuses a watch that did not record every frame.
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

import { RULE, framesMode, parseWatch, replay, secondaryMissedShare, spacingP50 } from "../tools/crosscheck-replay.mjs";

const TOOL = fileURLToPath(new URL("../tools/crosscheck-replay.mjs", import.meta.url));
const MS = 1_000_000n;
const R_NS = 16_666_667n;
const R_MS = 16.666667;

/**
 * A watch file as the probe writes it. The desktop presents every
 * `presentEvery` refreshes (R = 16.667 ms). DDA captures the presentations
 * `ddaKeep` keeps and reports the ones it coalesced as `missed`; WGC captures
 * the ones `wgcKeep` keeps, stamped `stampMs` later, showing content visible
 * `renderMs` before. Markers every 101.3 ms, so their phase walks.
 */
function watchText({ presentEvery = 1, wgcKeep = () => true, ddaKeep = () => true, renderMs = 0, stampMs = 5, frames = "all", start = true } = {}) {
  const period = R_NS * BigInt(presentEvery);
  const t0 = 1_000n * MS;
  // 101.3 ms apart: not a multiple of any presentation interval, so the phase
  // (and the presentation index modulo a drop pattern) walks across them all.
  const emit = Array.from({ length: 200 }, (_, i) => t0 + 500n * MS + BigInt(i) * 101_300_000n);
  const latest = (cut) => {
    let s = -1;
    for (let i = 0; i < emit.length && emit[i] <= cut; i++) s = i;
    return s;
  };
  const lines = [];
  if (start) lines.push({ schema: "termhost-probe/v1", type: "start", subcommand: "watch", args: ["termhost-probe", "watch", "--run-dir", "x", "--frames", frames] });
  const first = { wgc: new Set(), dda: new Set() };
  const frame = (source, index, t, seq, missed) => {
    lines.push({ schema: "termhost-probe/v1", type: "frame", source, index, t_ns: String(t), missed, markers: seq < 0 ? [] : [{ kind: "output", session: 0, seq }] });
    if (seq >= 0 && !first[source].has(seq)) {
      first[source].add(seq);
      lines.push({ schema: "termhost-probe/v1", type: "first_seen", source, kind: "output", session: 0, seq, t_ns: String(t), frame: index });
    }
  };
  let ddaMissed = 0;
  for (let k = 0n; t0 + k * period <= t0 + 21_000n * MS; k++) {
    const t = t0 + k * period;
    if (ddaKeep(Number(k))) {
      frame("dda", Number(k), t, latest(t), ddaMissed);
      ddaMissed = 0;
    } else {
      ddaMissed++;
    }
    if (wgcKeep(Number(k))) frame("wgc", Number(k), t + BigInt(stampMs) * MS, latest(t - BigInt(renderMs) * MS), undefined);
  }
  lines.push({ schema: "termhost-probe/v1", type: "summary", primary: "wgc", secondary: "dda", refresh_ms: R_MS });
  return lines.map((l) => JSON.stringify(l)).join("\n");
}

const near = (a, b) => Math.abs(a - b) < 1e-3;
const run = (o) => replay(parseWatch(watchText(o)));

test("a sound 60 Hz watch agrees, F from DDA's spacing, nothing late", () => {
  const r = run();
  assert.equal(r.verdict, "agree", JSON.stringify(r));
  assert.equal(r.compared, 200);
  assert.equal(r.medianGapMs, 5);
  assert.equal(r.late, 0);
  assert.ok(near(r.frameMs, R_MS));
  assert.equal(r.secondaryMissedShare, 0);
});

test("WGC dropping every third presentation is LATE (third review M1)", () => {
  const r = run({ wgcKeep: (k) => k % 3 !== 2 });
  assert.equal(r.verdict, "disagree", JSON.stringify(r));
  assert.deepEqual(r.why, ["late"]);
  assert.ok(r.lateShare > 0.25, String(r.lateShare));
  assert.ok(near(r.frameMs, R_MS));
});

test("WGC dropping a pseudo-random 15% of presentations is late", () => {
  const r = run({ wgcKeep: (k) => Number((BigInt(k) * 2_654_435_761n) >> 7n) % 100 >= 15 });
  assert.equal(r.verdict, "disagree", JSON.stringify(r));
  assert.ok(r.why.includes("late"));
  assert.ok(r.lateShare > RULE.maxLateShare, String(r.lateShare));
});

test("the late share at its boundary: presenting every other refresh, WGC 2 ms behind agrees, 5 ms behind is late", () => {
  const two = run({ presentEvery: 2, renderMs: 2, stampMs: 0 });
  assert.equal(two.verdict, "agree", JSON.stringify(two));
  assert.ok(two.late > 0 && two.lateShare < RULE.maxLateShare);
  assert.ok(near(two.frameMs, 2 * R_MS), "the measured F");
  const five = run({ presentEvery: 2, renderMs: 5, stampMs: 0 });
  assert.equal(five.verdict, "disagree", JSON.stringify(five));
  assert.deepEqual(five.why, ["late"]);
});

test("the measured F decides: the same 2 ms tail judged at F = R would be voided", () => {
  const w = parseWatch(watchText({ presentEvery: 2, renderMs: 2, stampMs: 0 }));
  w.frames.set("dda", w.frames.get("dda").slice(0, 20)); // 19 deltas: F falls back to R
  const r = replay(w);
  assert.ok(r.frameFellBackToRefresh);
  assert.equal(r.verdict, "disagree", JSON.stringify(r));
});

test("a DDA coalescing presentations does not widen F: WGC dropping every third is still late", () => {
  const r = run({ ddaKeep: (k) => k % 2 === 0, wgcKeep: (k) => k % 3 !== 2 });
  assert.ok(r.secondaryMissedShare > 0.9);
  assert.ok(near(r.spacingP50.dda, 2 * R_MS), "DDA's own spacing is 2R");
  assert.ok(r.frameFellBackToRefresh);
  assert.ok(near(r.frameMs, R_MS));
  assert.equal(r.verdict, "disagree", JSON.stringify(r));
  // With that fallback switched off the same watch would agree: the hole it closes.
  const open = replay(parseWatch(watchText({ ddaKeep: (k) => k % 2 === 0, wgcKeep: (k) => k % 3 !== 2 })), { rule: { maxSecondaryMissedShare: 1 } });
  assert.equal(open.verdict, "agree", JSON.stringify(open));
});

test("helpers: a spacing p50 needs 20 deltas; the coalesced share counts frames with missed > 0", () => {
  const f = (n) => Array.from({ length: n }, (_, i) => ({ t: BigInt(i) * 16n * MS, missed: i % 4 === 0 ? 1 : 0 }));
  assert.equal(spacingP50(f(20)), null);
  assert.equal(spacingP50(f(21)), 16);
  assert.equal(secondaryMissedShare(f(8)), 2 / 8);
  assert.equal(secondaryMissedShare([{ t: 0n, missed: null }]), null);
});

test("a watch not recorded with --frames all is refused, never sized (third review N2)", () => {
  assert.equal(framesMode({ args: ["p", "watch", "--frames", "changes"] }), "changes");
  assert.equal(framesMode({ args: ["p", "watch", "--frames=none"] }), "none");
  assert.equal(framesMode({ args: ["p", "watch"] }), "all", "the probe's default");
  assert.equal(framesMode(null), null);
  assert.throws(() => run({ frames: "changes" }), /--frames changes/);
  assert.throws(() => run({ start: false }), /no start record/);
});

test("the CLI reads a probe watch file, prints one verdict per file, and exits 2 on a refused one", (t) => {
  const dir = mkdtempSync(join(tmpdir(), "tb-replay-"));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  const file = join(dir, "probe-tiled-soak.jsonl");
  writeFileSync(file, watchText({ wgcKeep: (k) => k % 3 !== 2 }));
  const out = spawnSync(process.execPath, [TOOL, file, "--json"], { encoding: "utf8" });
  assert.equal(out.status, 0, out.stderr);
  const row = JSON.parse(out.stdout.trim());
  assert.equal(row.verdict, "disagree");
  assert.equal(row.file, file);
  const changes = join(dir, "probe-changes.jsonl");
  writeFileSync(changes, watchText({ frames: "changes" }));
  const refused = spawnSync(process.execPath, [TOOL, changes], { encoding: "utf8" });
  assert.equal(refused.status, 2);
  assert.match(refused.stderr, /--frames changes/);
  assert.equal(spawnSync(process.execPath, [TOOL], { encoding: "utf8" }).status, 2);
});

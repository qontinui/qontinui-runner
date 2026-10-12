/**
 * KPI computation from fixture `probe.jsonl` records and generator logs
 * (`kpis.mjs`): the measured values have answers stated in advance, and every
 * unknown path carries its reason and no number.
 *
 *   node --test scripts/termhost-bench/__tests__/kpis.test.mjs
 */

import { test } from "node:test";
import assert from "node:assert/strict";

import { KPI_IDS } from "../kpi-catalog.mjs";
import {
  attachNotes,
  computeRowKpis,
  exclusionReason,
  generatorWindow,
  indexGenerators,
  indexProbeSegment,
  intervalModel,
  parseJsonl,
  primaryFrames,
  seqAtOrAfter,
  segmentWindow,
  vActual,
} from "../kpis.mjs";
import { MS, genLog, operativeCapabilities, watchSegment } from "./helpers/bench-fixtures.mjs";

const T0 = 1_000_000_000_000n;
const SOAK_MS = 40_000;

/** A one-session focus arm: markers at 10 Hz, shown 20 ms after emission, 60 fps. */
function focusFixture(over = {}) {
  const gen = genLog({ session: 0, t0: T0 - 5000n * MS, durationMs: SOAK_MS + 15000, blockedNsPerWrite: 1_000_000 });
  const soak = watchSegment({ t0: T0, durationMs: SOAK_MS, sessions: [{ session: 0, gen, latencyMs: 20 }], ...over.watch });
  return {
    environment: "linux-xvfb",
    setupId: over.setupId ?? "tmux",
    n: 1,
    arm: over.arm ?? "focus",
    vTarget: 1,
    capabilities: operativeCapabilities(over.caps),
    gens: indexGenerators(new Map([[0, gen]])),
    soak: { records: soak },
    input: over.input ?? null,
    switches: over.switches ?? null,
    spawns: over.spawns ?? null,
    armFailure: over.armFailure ?? null,
  };
}

const assertUnknown = (v, reason) => {
  assert.equal(v.state, "unknown", JSON.stringify(v));
  assert.equal(v.reason, reason, JSON.stringify(v));
  assert.equal(v.value, undefined, "an unknown carries no value");
  assert.equal(v.stats, undefined, "an unknown carries no stats");
};

test("every catalog KPI is present, in catalog order", () => {
  const { kpis } = computeRowKpis(focusFixture());
  assert.deepEqual(Object.keys(kpis), [...KPI_IDS]);
});

test("output latency, cadence, jank, hangs and loss measure the stated answers", () => {
  const { kpis, vActual: va } = computeRowKpis(focusFixture());
  assert.equal(va, 1);
  const lat = kpis.output_latency_ms;
  assert.equal(lat.state, "measured");
  // Markers are shown 20 ms after emission; frames tick every 16.67 ms, so the first
  // frame showing a marker is 20..36.7 ms after it.
  assert.ok(lat.stats.p50 >= 20 && lat.stats.p50 <= 37, `p50 ${lat.stats.p50}`);
  assert.ok(lat.stats.p99 <= 37, `p99 ${lat.stats.p99}`);
  assert.ok(lat.n >= 350, `n ${lat.n}`);
  assert.deepEqual(lat.coverage, { observed: 1, expected: 1 });
  const fi = kpis.frame_interval_ms;
  assert.equal(fi.state, "measured");
  assert.ok(Math.abs(fi.stats.p50 - 16.667) < 0.01, `frame p50 ${fi.stats.p50}`);
  assert.equal(kpis.jank_share.state, "measured");
  assert.equal(kpis.jank_share.value, 0);
  assert.equal(kpis.hangs.state, "measured");
  assert.equal(kpis.hangs.value, 0);
  assert.equal(kpis.hangs.n, SOAK_MS / 500);
  assert.equal(kpis.marker_loss.state, "measured");
  assert.equal(kpis.marker_loss.value, 0);
  assert.equal(kpis.marker_skipped.state, "measured");
  assert.equal(kpis.marker_skipped.value, 0);
});

test("process accounting reads the host, workload and compositor groups", () => {
  const { kpis } = computeRowKpis(focusFixture());
  assert.equal(kpis.cpu_pct.state, "measured");
  assert.ok(kpis.cpu_pct.stats.mean >= 25 && kpis.cpu_pct.stats.mean <= 27);
  assert.equal(kpis.memory_mb.state, "measured");
  assert.ok(Math.abs(kpis.memory_mb.value - (300 + 80 * 0.1)) < 1e-9, `memory ${kpis.memory_mb.value}`);
  assert.equal(kpis.memory_growth_mb_per_min.state, "measured");
  assert.ok(Math.abs(kpis.memory_growth_mb_per_min.value - 12) < 0.01, `growth ${kpis.memory_growth_mb_per_min.value}`);
  assert.equal(kpis.workload_cpu_pct.stats.mean, 40);
  assert.equal(kpis.compositor_memory_mb.value, 40);
  assert.equal(kpis.crashes.value, 0);
  assert.equal(kpis.crashes.n, 2);
  // Linux has no per-process GPU engine counters: not applicable, never 0.
  assert.equal(kpis.gpu_pct.state, "not_applicable");
  assert.equal(kpis.gpu_pct.reason, "not_applicable_to_environment");
  assert.equal(kpis.cpu_ms_per_mb.state, "measured");
  assert.equal(kpis.wsl_vm_cpu_pct.state, "not_applicable");
});

test("producer stall and delivered ratio come from the generator's own counters", () => {
  const { kpis } = computeRowKpis(focusFixture());
  // 1 ms blocked per write, 10 writes a second: 1 % of wall time.
  assert.equal(kpis.producer_stall_pct.state, "measured");
  assert.ok(Math.abs(kpis.producer_stall_pct.stats.mean - 1) < 0.05, `stall ${kpis.producer_stall_pct.stats.mean}`);
  assert.equal(kpis.delivered_ratio.state, "measured");
  assert.ok(Math.abs(kpis.delivered_ratio.value - 1) < 1e-9);
});

test("no capture source: every capture KPI is unknown(capture_unavailable)", () => {
  const { kpis } = computeRowKpis(focusFixture({ watch: { captureFailed: true } }));
  for (const id of ["output_latency_ms", "frame_interval_ms", "jank_share", "hangs", "marker_loss", "marker_skipped"]) {
    assertUnknown(kpis[id], "capture_unavailable");
  }
  // Process accounting does not depend on capture.
  assert.equal(kpis.cpu_pct.state, "measured");
});

test("an assumed refresh rate makes refresh-dependent KPIs unknown(refresh_rate_unknown)", () => {
  const assumedSource = computeRowKpis(focusFixture({ watch: { refreshSource: "assumed (RandR reports no rate; Xvfb's vsync is a fake 60 Hz timer)" } }));
  for (const id of ["jank_share", "marker_loss", "marker_skipped"]) assertUnknown(assumedSource.kpis[id], "refresh_rate_unknown");
  assert.equal(assumedSource.kpis.output_latency_ms.state, "measured", "latency needs no refresh rate");
  const nullRate = computeRowKpis(focusFixture({ watch: { refreshHz: null } }));
  assertUnknown(nullRate.kpis.jank_share, "refresh_rate_unknown");
});

test("an unaligned clock or a capture disagreement makes latency unknown, never a number", () => {
  const clock = computeRowKpis(focusFixture({ caps: { "clock.aligned": { state: "UNKNOWN", reason: "node not found" } } }));
  assertUnknown(clock.kpis.output_latency_ms, "clock_unaligned");
  const cc = computeRowKpis(focusFixture({ watch: { crosscheck: "disagree" } }));
  assertUnknown(cc.kpis.output_latency_ms, "capture_disagreement");
});

test("a focus arm showing two sessions is unknown(zone_unresolved) for the runner, sessions_not_rendered otherwise", () => {
  const gen0 = genLog({ session: 0, t0: T0 - 5000n * MS, durationMs: SOAK_MS + 15000 });
  const gen1 = genLog({ session: 1, t0: T0 - 5000n * MS, durationMs: SOAK_MS + 15000 });
  const records = watchSegment({ t0: T0, durationMs: SOAK_MS, sessions: [{ session: 0, gen: gen0, latencyMs: 20 }, { session: 1, gen: gen1, latencyMs: 20 }] });
  const base = { environment: "linux-xvfb", n: 2, arm: "focus", vTarget: 1, capabilities: operativeCapabilities(), gens: indexGenerators(new Map([[0, gen0], [1, gen1]])), soak: { records } };
  const runner = computeRowKpis({ ...base, setupId: "runner" });
  assert.equal(runner.vActual, 2);
  assertUnknown(runner.kpis.output_latency_ms, "zone_unresolved");
  const tmux = computeRowKpis({ ...base, setupId: "tmux" });
  assertUnknown(tmux.kpis.output_latency_ms, "sessions_not_rendered");
  // An adapter-reported arm failure wins over the measured V.
  const told = computeRowKpis({ ...base, setupId: "runner", arm: "tiled", vTarget: 2, armFailure: { reason: "zone_unresolved", detail: "x" } });
  assertUnknown(told.kpis.frame_interval_ms, "zone_unresolved");
});

test("V is the displayed state accumulated across X11 damage bands, not per band", () => {
  const gens = [0, 1, 2].map((s) => genLog({ session: s, t0: T0 - 5000n * MS, durationMs: SOAK_MS + 15000 }));
  const records = watchSegment({
    t0: T0,
    durationMs: SOAK_MS,
    bands: true,
    sessions: gens.map((gen, s) => ({ session: s, gen, latencyMs: 20, rect: { x: 10 + 600 * s, y: 40, w: 160, h: 16 } })),
  });
  const seg = indexProbeSegment(records);
  const frames = primaryFrames(seg, "x11", segmentWindow(seg));
  assert.ok(frames.every((f) => f.v <= 1), "each band carries one session");
  assert.equal(vActual(frames), 3);
  const r = computeRowKpis({ environment: "linux-xvfb", setupId: "runner", n: 3, arm: "tiled", vTarget: 3, capabilities: operativeCapabilities(), gens: indexGenerators(new Map(gens.map((g, s) => [s, g]))), soak: { records } });
  assert.equal(r.kpis.output_latency_ms.state, "measured");
});

test("too few markers is unknown(insufficient_samples) with its count", () => {
  const gen = genLog({ session: 0, t0: T0 - 1000n * MS, durationMs: 9000 });
  const records = watchSegment({ t0: T0, durationMs: 8000, sessions: [{ session: 0, gen, latencyMs: 20 }] });
  const { kpis } = computeRowKpis({ environment: "linux-xvfb", setupId: "tmux", n: 1, arm: "focus", vTarget: 1, capabilities: operativeCapabilities(), gens: indexGenerators(new Map([[0, gen]])), soak: { records } });
  assertUnknown(kpis.output_latency_ms, "insufficient_samples");
  assert.ok(kpis.output_latency_ms.n > 0 && kpis.output_latency_ms.n < 100);
  // 16 valid intervals < 60: not enough to call the hang count.
  assertUnknown(kpis.hangs, "insufficient_samples");
});

test("interval exclusion names occlusion, operator activity, or the probe itself", () => {
  const occluded = computeRowKpis(focusFixture({ watch: { intervalClass: (i) => (i % 2 ? "occluded" : null) } }));
  assertUnknown(occluded.kpis.hangs, "occluded");
  const operator = computeRowKpis(focusFixture({ watch: { intervalClass: (i) => (i % 3 ? "operator_activity" : null) } }));
  assertUnknown(operator.kpis.hangs, "operator_activity");
  const harness = computeRowKpis(focusFixture({ watch: { intervalClass: () => "harness_error" } }));
  assertUnknown(harness.kpis.hangs, "harness_error");
  // With no full interval excluded, too few samples is insufficient_samples (review S4).
  assert.equal(exclusionReason({ valid: 10, occluded: 0, operator: 0, harness: 0 }, 10, 40, 60), "insufficient_samples");
  assert.equal(exclusionReason({ valid: 10, occluded: 30, operator: 20, harness: 0 }, 10, 50, 60), "occluded");
  assert.equal(exclusionReason({ valid: 10, occluded: 20, operator: 30, harness: 0 }, 10, 50, 60), "operator_activity");
  assert.equal(exclusionReason({ valid: 10, occluded: 20, operator: 20, harness: 20 }, 10, 60, 60), "harness_error");
});

test("an empty host group is unknown(harness_error), never a measured 0", () => {
  const { kpis } = computeRowKpis(focusFixture({ watch: { hostPids: [] } }));
  assertUnknown(kpis.cpu_pct, "harness_error");
  assertUnknown(kpis.memory_mb, "harness_error");
  assertUnknown(kpis.crashes, "harness_error");
});

test("a host process present at the start that exits is a crash; a transient one is noted", () => {
  const { kpis } = computeRowKpis(focusFixture({ watch: { hostExits: [{ k: 10, pid: 101 }, { k: 12, pid: 999 }] } }));
  assert.equal(kpis.crashes.value, 1);
  assert.ok(kpis.crashes.notes.some((n) => /short-lived/.test(n)));
});

test("switch latency is not applicable in the tiled arm and at N = 1", () => {
  const tiled = computeRowKpis(focusFixture({ arm: "tiled" }));
  assert.equal(tiled.kpis.switch_latency_ms.state, "not_applicable");
  assert.equal(tiled.kpis.switch_latency_ms.reason, "not_applicable_to_arm");
  const one = computeRowKpis(focusFixture());
  assert.equal(one.kpis.switch_latency_ms.reason, "single_session");
});

test("switch latency: show(k) to k's newest marker on screen", () => {
  const gens = [0, 1].map((s) => genLog({ session: s, t0: T0 - 5000n * MS, durationMs: SOAK_MS + 30000 }));
  const sw0 = T0 + BigInt(SOAK_MS + 1000) * MS;
  // Session 1 becomes visible 70 ms after each command (it is hidden before).
  const list = [];
  const frames = [];
  for (let j = 0; j < 20; j += 1) {
    const tCmd = sw0 + BigInt(j * 500) * MS;
    list.push({ k: 1, tCmdNs: tCmd });
    frames.push(tCmd + 70n * MS);
  }
  const records = watchSegment({ t0: sw0 - 100n * MS, durationMs: 11000, sessions: [{ session: 0, gen: gens[0], latencyMs: 20 }], frameHz: 60 });
  // Add frames that show session 1's newest marker 70 ms after each switch.
  for (const t of frames) {
    let cur = null;
    for (const r of gens[1]) if (r.type === "marker" && BigInt(r.t_emit_ns) <= t) cur = r;
    records.push({ type: "frame", source: "x11", index: 0, t_ns: String(t), w: 1, h: 1, v: 1, markers: [{ kind: "output", session: 1, seq: cur.seq, rect: { x: 0, y: 0, w: 1, h: 1 } }], rejected: 0 });
  }
  const r = computeRowKpis({
    ...focusFixture(),
    n: 2,
    gens: indexGenerators(new Map([[0, gens[0]], [1, gens[1]]])),
    switches: { records, list },
  });
  assert.equal(r.kpis.switch_latency_ms.state, "measured");
  assert.ok(Math.abs(r.kpis.switch_latency_ms.stats.p50 - 70) < 1e-6, `switch p50 ${r.kpis.switch_latency_ms.stats.p50}`);
  assert.equal(r.kpis.switch_latency_ms.n, 20);
});

test("spawn latency maps spawned sessions to slots, and says so when none became visible", () => {
  const base = focusFixture();
  const sp0 = T0 + BigInt(SOAK_MS + 2000) * MS;
  const records = watchSegment({ t0: sp0, durationMs: 30000, sessions: [] });
  const list = [];
  for (let j = 0; j < 20; j += 1) {
    const tCmd = sp0 + BigInt(j * 1000) * MS;
    list.push({ session: 10 + j, tCmdNs: tCmd });
    records.push({ type: "frame", source: "x11", index: 0, t_ns: String(tCmd + 900n * MS), w: 1, h: 1, v: 1, markers: [{ kind: "output", session: 10 + j, seq: 0, rect: { x: 0, y: 0, w: 1, h: 1 } }], rejected: 0 });
  }
  const ok = computeRowKpis({ ...base, spawns: { records, list } });
  assert.equal(ok.kpis.spawn_latency_ms.state, "measured");
  assert.ok(Math.abs(ok.kpis.spawn_latency_ms.stats.p95 - 900) < 1e-6);
  const unseen = computeRowKpis({ ...base, spawns: { records: watchSegment({ t0: sp0, durationMs: 30000, sessions: [] }), list } });
  assertUnknown(unseen.kpis.spawn_latency_ms, "sessions_not_rendered");
  const unmapped = computeRowKpis({ ...base, spawns: { records, list: list.map((s) => ({ ...s, session: null })) } });
  assertUnknown(unmapped.kpis.spawn_latency_ms, "insufficient_samples");
  assert.ok(unmapped.kpis.spawn_latency_ms.notes?.some((n) => /matched to a generator slot/.test(n)));
});

test("input latency joins inject.jsonl's t_inject with the echo marker's first appearance", () => {
  const injectT0 = T0 + BigInt(SOAK_MS + 1000) * MS;
  const echoBase = 7;
  const keys = [];
  const echoes = [];
  const seen = [];
  for (let i = 0; i < 120; i += 1) {
    const tInject = injectT0 + BigInt(i * 100) * MS;
    const key = String.fromCharCode(97 + (i % 26));
    keys.push({ type: "inject", index: i, key, t_inject_ns: String(tInject), t_check_ns: String(tInject), tagged: true });
    echoes.push({ seq: echoBase + i, byte: key.charCodeAt(0), tEmit: tInject + 2n * MS });
    seen.push({ session: 0, seq: echoBase + i, t: tInject + 30n * MS });
  }
  // One echo with the wrong byte must be excluded, not mismatched into a sample.
  echoes[5] = { ...echoes[5], byte: 0x7a };
  const gen = genLog({ session: 0, t0: T0 - 5000n * MS, durationMs: SOAK_MS + 30000, echoes });
  const records = watchSegment({ t0: injectT0 - 200n * MS, durationMs: 13000, sessions: [{ session: 0, gen, latencyMs: 20 }], echoSeen: seen });
  const r = computeRowKpis({
    ...focusFixture(),
    gens: indexGenerators(new Map([[0, gen]])),
    input: { records, injects: keys, focusSession: 0, echoBase },
  });
  const v = r.kpis.input_latency_ms;
  assert.equal(v.state, "measured", JSON.stringify(v));
  assert.equal(v.n, 119);
  assert.ok(v.stats.p50 >= 30 && v.stats.p50 <= 47, `input p50 ${v.stats.p50}`);
  assert.ok(v.notes.some((n) => /no matching echo/.test(n)));
});

test("an input arm that could not run carries its reason", () => {
  const r = computeRowKpis(focusFixture({ input: { unavailable: { reason: "pane_not_mounted", detail: "TERMINAL_NO_MOUNTED_VIEW" } } }));
  assertUnknown(r.kpis.input_latency_ms, "pane_not_mounted");
  const none = computeRowKpis(focusFixture());
  assertUnknown(none.kpis.input_latency_ms, "input_unsafe");
});

test("a generator silent after the window is named, not counted as 0 % stall", () => {
  const gen = genLog({ session: 0, t0: T0 - 5000n * MS, durationMs: 20000, exit: false });
  const gw = generatorWindow(indexGenerators(new Map([[0, gen]])), { t0: T0, t1: T0 + 40000n * MS });
  assert.deepEqual(gw.stall, []);
  assert.deepEqual(gw.silent, [0]);
  assert.ok(gw.notes[0].startsWith("producer_stall_pct: "));
});

test("helpers: JSON lines, sequence order, notes attachment", () => {
  const p = parseJsonl('{"a":1}\nnot json\n{"b":2}\n{"partial":');
  assert.equal(p.records.length, 2);
  assert.equal(p.malformed, 1);
  assert.equal(p.partial, true);
  assert.equal(seqAtOrAfter(5, 5), true);
  assert.equal(seqAtOrAfter(4, 5), false);
  assert.equal(seqAtOrAfter(0, 117648), true, "wraps at the modulus");
  const kpis = { cpu_pct: { state: "measured" } };
  assert.deepEqual(attachNotes(kpis, ["cpu_pct: one", "loose note"]), ["loose note"]);
  assert.deepEqual(kpis.cpu_pct.notes, ["one"]);
  const model = intervalModel(indexProbeSegment(watchSegment({ t0: T0, durationMs: 2100, sessions: [] })));
  assert.equal(model.spanValid(T0, T0 + 1400n * MS), true);
  assert.equal(model.spanValid(T0, T0 + 2050n * MS), false, "touches the trailing partial interval");
  assert.equal(model.spanValid(T0 - 2000n * MS, T0 - 1000n * MS), false, "entirely before the watch");
});

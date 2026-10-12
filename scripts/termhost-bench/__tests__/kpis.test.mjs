/**
 * KPI computation from fixture `probe.jsonl` records and generator logs
 * (`kpis.mjs`): the measured values have answers stated in advance, the probe's
 * summary values reach the row only as catalog values, and every unknown path
 * carries its reason and no number.
 *
 *   node --test scripts/termhost-bench/__tests__/kpis.test.mjs
 */

import { test } from "node:test";
import assert from "node:assert/strict";

import { KPI_IDS } from "../kpi-catalog.mjs";
import {
  attachNotes,
  captureInfo,
  computeRowKpis,
  fromProbe,
  generatorWindow,
  indexGenerators,
  indexProbeSegment,
  injectAbortReason,
  parseJsonl,
  probeHealthGate,
  seqAtOrAfter,
} from "../kpis.mjs";
import { measureKpi } from "../stats.mjs";
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
  const assumed = computeRowKpis(focusFixture({ watch: { refreshAssumed: true } }));
  for (const id of ["jank_share", "marker_loss", "marker_skipped"]) assertUnknown(assumed.kpis[id], "refresh_rate_unknown");
  assert.equal(assumed.kpis.output_latency_ms.state, "measured", "latency needs no refresh rate");
  const seg = indexProbeSegment(watchSegment({ t0: T0, durationMs: 2000, refreshAssumed: true }));
  assert.equal(captureInfo(seg).refreshKnown, false);
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

test("V is the probe's displayed state accumulated across X11 damage bands, not a per-band count", () => {
  const gens = [0, 1, 2].map((s) => genLog({ session: s, t0: T0 - 5000n * MS, durationMs: SOAK_MS + 15000 }));
  const records = watchSegment({
    t0: T0,
    durationMs: SOAK_MS,
    bands: true,
    sessions: gens.map((gen, s) => ({ session: s, gen, latencyMs: 20, rect: { x: 10 + 600 * s, y: 40, w: 160, h: 16 } })),
  });
  const frames = records.filter((r) => r.type === "frame");
  assert.ok(frames.every((f) => f.markers.filter((m) => m.kind === "output").length <= 1), "each band carries one session's marker");
  const r = computeRowKpis({ environment: "linux-xvfb", setupId: "runner", n: 3, arm: "tiled", vTarget: 3, capabilities: operativeCapabilities(), gens: indexGenerators(new Map(gens.map((g, s) => [s, g]))), soak: { records } });
  assert.equal(r.vActual, 3, "the summary's v_actual_p50");
  assert.deepEqual(r.sessionsSeen, [0, 1, 2]);
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

test("interval exclusion reaches the row with the probe's reason: occlusion, operator activity, or the probe itself", () => {
  const occluded = computeRowKpis(focusFixture({ watch: { intervalClass: (i) => (i % 2 ? "occluded" : null) } }));
  assertUnknown(occluded.kpis.hangs, "occluded");
  const operator = computeRowKpis(focusFixture({ watch: { intervalClass: (i) => (i % 3 ? "operator_activity" : null) } }));
  assertUnknown(operator.kpis.hangs, "operator_activity");
  const harness = computeRowKpis(focusFixture({ watch: { intervalClass: () => "harness_error" } }));
  assertUnknown(harness.kpis.hangs, "harness_error");
});

test("the probe's values reach the row only as catalog values", () => {
  const lat = measureKpi("output_latency_ms", Array.from({ length: 150 }, (_, i) => 10 + i / 10));
  assert.deepEqual(fromProbe("output_latency_ms", lat), lat, "a catalog-shaped measured value passes as it is");
  assert.deepEqual(fromProbe("hangs", { state: "measured", value: 2, unit: "count", n: 80 }), measureKpi("hangs", { value: 2, n: 80 }));
  assertUnknown(fromProbe("hangs", { state: "unknown", unit: "count", n: 3, reason: "occluded", detail: "x" }), "occluded");
  // What the catalog cannot hold is the harness's error, never a number.
  assertUnknown(fromProbe("output_latency_ms", undefined), "harness_error");
  assertUnknown(fromProbe("output_latency_ms", { ...lat, unit: "s" }), "harness_error");
  assertUnknown(fromProbe("hangs", { state: "unknown", unit: "count", n: 0, reason: "cosmic_rays" }), "harness_error");
  assertUnknown(fromProbe("output_latency_ms", { ...lat, stats: { p50: 1, p95: 2 } }), "harness_error");
  assertUnknown(fromProbe("hangs", { state: "measured", unit: "count", n: 80, value: Number.NaN }), "harness_error");
  // A measured value under the catalog's minimum is not measured here either.
  const thin = fromProbe("output_latency_ms", { ...lat, n: 99 });
  assertUnknown(thin, "insufficient_samples");
  assert.equal(thin.n, 99);
});

test("a probe that fell behind: its capture KPIs carry its harness_error, and so do the latencies the harness times", () => {
  const r = computeRowKpis(focusFixture({ watch: { behind: true } }));
  for (const id of ["output_latency_ms", "frame_interval_ms", "jank_share", "marker_loss", "marker_skipped"]) {
    assertUnknown(r.kpis[id], "harness_error");
  }
  const lagging = watchSegment({ t0: T0 + BigInt(SOAK_MS + 1000) * MS, durationMs: 11000, behind: true });
  assert.equal(probeHealthGate(indexProbeSegment(lagging)).reason, "harness_error");
  assert.equal(probeHealthGate(indexProbeSegment(watchSegment({ t0: T0, durationMs: 2000 }))), null);
  const sw = computeRowKpis({ ...focusFixture(), n: 2, switches: { records: lagging, list: [{ k: 0, tCmdNs: T0 + BigInt(SOAK_MS + 2000) * MS }] } });
  assertUnknown(sw.kpis.switch_latency_ms, "harness_error");
  const sp = computeRowKpis({ ...focusFixture(), spawns: { records: lagging, list: [{ session: 3, tCmdNs: T0 + BigInt(SOAK_MS + 2000) * MS }] } });
  assertUnknown(sp.kpis.spawn_latency_ms, "harness_error");
});

test("Desktop Duplication alone is no primary source: the capture KPIs are unknown(capture_unavailable)", () => {
  const records = watchSegment({ t0: T0, durationMs: SOAK_MS, primary: "wgc", captureFailed: true });
  records.splice(2, 0, { type: "capture", source: "dda", state: "started", detail: "fixture", refresh_hz: 60, refresh_source: "DXGI", refresh_assumed: false });
  const seg = indexProbeSegment(records);
  assert.equal(captureInfo(seg).primary, null);
  const gen = genLog({ session: 0, t0: T0 - 5000n * MS, durationMs: SOAK_MS + 15000 });
  const { kpis } = computeRowKpis({ ...focusFixture(), gens: indexGenerators(new Map([[0, gen]])), soak: { records } });
  assertUnknown(kpis.output_latency_ms, "capture_unavailable");
  assertUnknown(kpis.hangs, "capture_unavailable");
});

test("a process group the probe could not read whole is unknown(harness_error); a partly withheld one is noted", () => {
  const none = computeRowKpis(focusFixture({ watch: { groupNote: (g) => (g === "host" ? "unreadable_members" : null) } }));
  assertUnknown(none.kpis.cpu_pct, "harness_error");
  assertUnknown(none.kpis.memory_mb, "harness_error");
  const gone = computeRowKpis(focusFixture({ watch: { groupNote: (g) => (g === "workload" ? "no_live_process" : null) } }));
  assertUnknown(gone.kpis.workload_cpu_pct, "harness_error");
  assertUnknown(gone.kpis.workload_memory_mb, "harness_error");
  const some = computeRowKpis(focusFixture({ watch: { groupNote: (g, k) => (g === "host" && k % 10 === 3 ? "unreadable_members" : null) } }));
  assert.equal(some.kpis.cpu_pct.state, "measured", "the probe's mean over the samples it read whole");
  assert.equal(some.kpis.memory_mb.state, "measured");
  assert.ok(some.kpis.memory_mb.notes.some((n) => /withheld/.test(n)));
  // A CPU-time SUM with a withheld sample is not the host's.
  assertUnknown(some.kpis.cpu_ms_per_mb, "harness_error");
});

test("no host group (no live host root) is unknown(harness_error), never a measured 0", () => {
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
  // Commands 50 ms after a marker emission: k's newest marker at the command is 50 ms old.
  const sw0 = T0 + BigInt(SOAK_MS + 1050) * MS;
  // Session 1 shows its newest marker 70 ms after each command, and also flickers
  // into view 20 ms BEFORE it: a frame before the command never counts (it would
  // be a negative latency).
  const list = [];
  const frames = [];
  for (let j = 0; j < 20; j += 1) {
    const tCmd = sw0 + BigInt(j * 500) * MS;
    list.push({ k: 1, tCmdNs: tCmd });
    frames.push(tCmd - 20n * MS, tCmd + 70n * MS);
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

test("an inject abort names its catalog reason; an arm aborted part-way reports the abort, not the count", () => {
  assert.equal(injectAbortReason("operator_input"), "operator_activity");
  assert.equal(injectAbortReason("foreign_injection"), "operator_activity");
  assert.equal(injectAbortReason("unsupported_key"), "harness_error");
  assert.equal(injectAbortReason("send_failed"), "harness_error");
  for (const r of ["not_foreground", "desktop_unavailable", "monitor_down", "monitor_deaf", "foreground_changed_during_injection", "something_new"]) {
    assert.equal(injectAbortReason(r), "input_unsafe", r);
  }
  const injectT0 = T0 + BigInt(SOAK_MS + 1000) * MS;
  const keys = [];
  const echoes = [];
  const seen = [];
  for (let i = 0; i < 30; i += 1) {
    const tInject = injectT0 + BigInt(i * 100) * MS;
    const key = String.fromCharCode(97 + (i % 26));
    keys.push({ type: "inject", index: i, key, t_inject_ns: String(tInject), t_check_ns: String(tInject), tagged: true });
    echoes.push({ seq: i, byte: key.charCodeAt(0), tEmit: tInject + 2n * MS });
    seen.push({ session: 0, seq: i, t: tInject + 30n * MS });
  }
  const gen = genLog({ session: 0, t0: T0 - 5000n * MS, durationMs: SOAK_MS + 30000, echoes });
  const records = watchSegment({ t0: injectT0 - 200n * MS, durationMs: 4000, sessions: [{ session: 0, gen, latencyMs: 20 }], echoSeen: seen });
  const r = computeRowKpis({
    ...focusFixture(),
    gens: indexGenerators(new Map([[0, gen]])),
    input: { records, injects: keys, focusSession: 0, echoBase: 0, abort: { type: "inject_abort", index: 30, reason: "operator_input", detail: "a key was pressed", keys_sent: 30 } },
  });
  assertUnknown(r.kpis.input_latency_ms, "operator_activity");
  assert.equal(r.kpis.input_latency_ms.n, 30);
  assert.match(r.kpis.input_latency_ms.detail, /aborted at key 30 \(operator_input\)/);
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
});

test("amended interval and hang records: the last record per interval index and per hang start is the verdict", () => {
  const seg = indexProbeSegment([
    { type: "start", subcommand: "watch", t0_ns: "1" },
    { type: "interval", index: 0, valid: true },
    { type: "interval", index: 1, valid: true },
    { type: "hang", t_start_ns: "500", valid: true },
    { type: "interval", index: 0, valid: false, exclusion_class: "operator_activity", amended: true },
    { type: "hang", t_start_ns: "500", valid: false, amended: true },
  ]);
  assert.deepEqual(seg.intervals.map((r) => [r.index, r.valid]), [[0, false], [1, true]]);
  assert.deepEqual(seg.hangs.map((h) => h.valid), [false]);
});

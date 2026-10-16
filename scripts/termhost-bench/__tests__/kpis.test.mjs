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
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { KPI_IDS } from "../kpi-catalog.mjs";
import { SEQ_MODULUS } from "../marker.mjs";
import {
  attachNotes,
  captureInfo,
  computeRowKpis,
  fromProbe,
  STALL_HIDDEN_GAP_MS,
  STALL_MIN_EMITTED,
  STALL_MIN_MS,
  STALL_TAIL_MIN_MS,
  STALL_VOIDED_KPIS,
  captureLosses,
  lossEffects,
  lossGroups,
  summarizeLosses,
  LOSS_DETAIL_MAX,
  CAPTURE_LOST_SLACK_MS,
  describeStall,
  excludedSpans,
  stallCoverage,
  generatorExits,
  paneStalls,
  generatorWindow,
  indexGenerators,
  indexProbeSegment,
  INJECT_ABORT_REASONS,
  injectAbortReason,
  parseJsonl,
  LAG_VERDICT_MIN_FRAMES,
  PROBE_CAPTURE_KPIS,
  probeHealthGate,
  seqAtOrAfter,
} from "../kpis.mjs";
import { checkAssertions } from "../report.mjs";
import { measureKpi } from "../stats.mjs";
import { MS, genLog, operativeCapabilities, stallSegment, watchSegment } from "./helpers/bench-fixtures.mjs";

const T0 = 1_000_000_000_000n;
const SOAK_MS = 40_000;
const toBig = (v) => BigInt(v);

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

/** The fixture's soak records with these probe summary values in place of its own. */
function withSummary(fixture, patch) {
  const records = fixture.soak.records.map((r) => (r.type === "summary" ? { ...r, ...patch } : r));
  return { ...fixture, soak: { records } };
}

test("the soak's capture KPIs are the probe's summary values, passed through unchanged", () => {
  // Sentinel values the fixture's own model would never produce: the test checks
  // what kpis.mjs does with the probe's verdict, not the fixture's arithmetic.
  const sentinel = {
    output_latency_ms: { state: "measured", unit: "ms", n: 412, stats: { p50: 11.5, p95: 22.25, p99: 33.125 } },
    frame_interval_ms: { state: "measured", unit: "ms", n: 2399, stats: { p50: 16.5, p95: 18.75 } },
    jank_share: { state: "measured", unit: "%", n: 2399, value: 1.25 },
    hangs: { state: "measured", unit: "count", n: 80, value: 2 },
    marker_loss: { state: "measured", unit: "%", n: 400, value: 0.5 },
    marker_skipped: { state: "measured", unit: "%", n: 400, value: 3.75 },
    v_actual_p50: 1,
  };
  const { kpis, vActual: va } = computeRowKpis(withSummary(focusFixture(), sentinel));
  assert.equal(va, 1);
  const { coverage, ...lat } = kpis.output_latency_ms;
  assert.deepEqual(lat, sentinel.output_latency_ms);
  assert.deepEqual(coverage, { observed: 1, expected: 1 }, "coverage: the displayed sessions that gave a sample");
  for (const id of ["frame_interval_ms", "jank_share", "hangs", "marker_loss", "marker_skipped"]) {
    assert.deepEqual(kpis[id], sentinel[id], id);
  }
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

/** A row whose soak summary's cross-check block is `cc`, in an environment declaring `declared`. */
const profiled = (cc, declared = "software-compositor") =>
  computeRowKpis({ ...withSummary(focusFixture(), { secondary: "dda", crosscheck: cc }), crosscheckProfile: declared, environment: "windows-hosted" });

test("a soak cross-check judged under the declared profile keeps its latency measured (round 4 N5)", () => {
  const ok = profiled({ verdict: "agree", profile: "software-compositor" });
  assert.equal(ok.kpis.output_latency_ms.state, "measured", JSON.stringify(ok.kpis.output_latency_ms));
});

test("a soak judged under another, an absent, an empty or a differently spelled profile makes every probe capture KPI harness_error (round 4 S1; the profile also chooses the lag bound)", () => {
  const good = profiled({ verdict: "agree", profile: "software-compositor" });
  for (const [cc, said] of [
    [{ verdict: "agree", profile: "strict" }, /under the strict profile; the windows-hosted environment declares software-compositor/],
    [{ verdict: "agree" }, /under no recorded profile/],
    [{ verdict: "agree", profile: "" }, /under no recorded profile/],
    [{ verdict: "agree", profile: "Software-Compositor" }, /under the Software-Compositor profile/],
  ]) {
    const r = profiled(cc);
    assertUnknown(r.kpis.output_latency_ms, "harness_error");
    assert.match(r.kpis.output_latency_ms.detail, said, JSON.stringify(cc));
    // The profile also chose the probe-lag bound, whose verdict gates all of these.
    for (const id of ["frame_interval_ms", "jank_share", "hangs", "marker_loss", "marker_skipped"]) {
      assert.equal(good.kpis[id].state, "measured", `${id} under the declared profile`);
      assertUnknown(r.kpis[id], "harness_error");
      assert.match(r.kpis[id].detail, said, `${id} under ${JSON.stringify(cc)}`);
    }
  }
  // No declared profile (an older caller) or no cross-check block: no gate.
  const undeclared = computeRowKpis(withSummary(focusFixture(), { crosscheck: { verdict: "agree" } }));
  assert.equal(undeclared.kpis.output_latency_ms.state, "measured");
});

test("a strict environment fed a software-compositor summary with 80 ms of lag gates every probe capture KPI (head-confirmation review)", () => {
  const fx = focusFixture();
  const probe = fx.soak.records.find((r) => r.type === "summary").probe;
  const lagged = {
    ...probe,
    behind: false,
    lag_ms_judged: 80,
    lag_statistic: "p95",
    lag_bound_ms: 93.75,
    lag_bound_basis: "software-compositor: 93.8 ms, the larger of the strict 50.0 ms and 3 frame intervals of 31.25 ms (WGC's frame pool)",
  };
  const r = computeRowKpis({
    ...withSummary(fx, { secondary: "dda", crosscheck: { verdict: "agree", profile: "software-compositor" }, probe: lagged }),
    environment: "windows-desktop",
    crosscheckProfile: "strict",
  });
  for (const id of PROBE_CAPTURE_KPIS) {
    assertUnknown(r.kpis[id], "harness_error");
    assert.match(r.kpis[id].detail, /under the software-compositor profile; the windows-desktop environment declares strict/, id);
  }
  // The same summary in the environment that declared it: measured.
  const ok = computeRowKpis({
    ...withSummary(fx, { secondary: "dda", crosscheck: { verdict: "agree", profile: "software-compositor" }, probe: lagged }),
    environment: "windows-hosted",
    crosscheckProfile: "software-compositor",
  });
  for (const id of PROBE_CAPTURE_KPIS) assert.equal(ok.kpis[id].state, "measured", `${id}: ${JSON.stringify(ok.kpis[id])}`);
});

test("the switch and spawn watches are held to the declared profile too (head confirmation of 8ead56656)", () => {
  const t0 = T0 + BigInt(SOAK_MS + 1000) * MS;
  const gen = genLog({ session: 0, t0: T0 - 5000n * MS, durationMs: SOAK_MS + 30000 });
  const watch = (profile) => watchSegment({ t0, durationMs: 13000, sessions: [{ session: 0, gen, latencyMs: 20 }], crosscheckProfile: profile });
  const row = (switchProfile, spawnProfile) =>
    computeRowKpis({
      ...withSummary(focusFixture(), { crosscheck: { verdict: "agree", profile: "software-compositor" } }),
      environment: "windows-hosted",
      crosscheckProfile: "software-compositor",
      n: 2,
      gens: indexGenerators(new Map([[0, gen]])),
      switches: { records: watch(switchProfile), list: [{ k: 0, tCmdNs: t0 + 1000n * MS }] },
      spawns: { records: watch(spawnProfile), list: [{ session: 0, tCmdNs: t0 + 1000n * MS }] },
    }).kpis;
  const good = row("software-compositor", "software-compositor");
  for (const id of ["switch_latency_ms", "spawn_latency_ms"]) {
    assert.doesNotMatch(good[id].detail ?? "", /judged the capture cross-check/, `${id}: ${JSON.stringify(good[id])}`);
  }
  for (const [wrong, phase, id, other] of [
    [row("strict", "software-compositor"), "switch", "switch_latency_ms", "spawn_latency_ms"],
    [row("software-compositor", undefined), "spawn", "spawn_latency_ms", "switch_latency_ms"],
  ]) {
    assertUnknown(wrong[id], "harness_error");
    assert.match(wrong[id].detail, new RegExp(`^the ${phase} watch judged the capture cross-check under (the strict|no recorded) profile; the windows-hosted environment declares software-compositor`));
    assert.deepEqual(wrong[other], good[other], `${other} untouched by the ${phase} watch's profile`);
  }
});

test("the input watch's cross-check is held to the declared profile too (round 4 S2)", () => {
  const injectT0 = T0 + BigInt(SOAK_MS + 1000) * MS;
  const keys = [];
  const echoes = [];
  const seen = [];
  for (let i = 0; i < 120; i += 1) {
    const tInject = injectT0 + BigInt(i * 100) * MS;
    keys.push({ type: "inject", index: i, key: "a", t_inject_ns: String(tInject), t_check_ns: String(tInject), tagged: true });
    echoes.push({ seq: i, byte: 97, tEmit: tInject + 2n * MS });
    seen.push({ session: 0, seq: i, t: tInject + 30n * MS });
  }
  const gen = genLog({ session: 0, t0: T0 - 5000n * MS, durationMs: SOAK_MS + 30000, echoes });
  const row = (inputProfile) => {
    const records = watchSegment({ t0: injectT0 - 200n * MS, durationMs: 13000, sessions: [{ session: 0, gen, latencyMs: 20 }], echoSeen: seen, crosscheckProfile: inputProfile });
    const soak = withSummary(focusFixture(), { crosscheck: { verdict: "agree", profile: "software-compositor" } });
    return computeRowKpis({
      ...soak,
      environment: "windows-hosted",
      crosscheckProfile: "software-compositor",
      gens: indexGenerators(new Map([[0, gen]])),
      input: { records, injects: keys, focusSession: 0, echoBase: 0 },
    }).kpis;
  };
  assert.equal(row("software-compositor").input_latency_ms.state, "measured", JSON.stringify(row("software-compositor").input_latency_ms));
  for (const wrong of ["strict", undefined]) {
    const k = row(wrong);
    assertUnknown(k.input_latency_ms, "harness_error");
    assert.match(k.input_latency_ms.detail, /^the input watch judged the capture cross-check/);
    assert.equal(k.output_latency_ms.state, "measured", "the soak's own profile was right");
  }
});

test("the probe-lag bound's basis reaches the row: a note under software-compositor, the gate's detail when behind", () => {
  const basis = "software-compositor: 93.8 ms, the larger of the strict 50.0 ms and 3 frame intervals of 31.25 ms (WGC's frame pool)";
  const fx = focusFixture();
  const probeOf = (rec) => fx.soak.records.find((r) => r.type === "summary").probe;
  const withProbe = (patch) => withSummary(fx, { probe: { ...probeOf(), ...patch } });
  const ok = computeRowKpis(withProbe({ behind: false, lag_ms_judged: 81.7, lag_statistic: "p95", lag_bound_ms: 93.75, lag_bound_basis: basis }));
  assert.equal(ok.kpis.output_latency_ms.state, "measured", JSON.stringify(ok.kpis.output_latency_ms));
  assert.ok(ok.kpis.output_latency_ms.notes.includes(`probe lag 81.7 ms (p95) judged against ${basis}`), JSON.stringify(ok.kpis.output_latency_ms.notes));
  const strict = computeRowKpis(withProbe({ behind: false, lag_bound_ms: 50, lag_bound_basis: "strict: 50.0 ms (three refresh intervals, at least 50 ms)" }));
  assert.ok(!(strict.kpis.output_latency_ms.notes ?? []).some((n) => /probe lag/.test(n)), "strict adds no note");
  const gate = probeHealthGate({ summary: { probe: { behind: true, lag_ms_judged: 120, lag_statistic: "p95", lag_bound_ms: 93.75, lag_bound_basis: basis, frames_over_bound: 40 } } });
  assert.equal(gate.reason, "harness_error");
  assert.ok(gate.detail.includes(`bound 93.75 ms — ${basis}, 40 frame(s) over it`), gate.detail);
});

test("a measured latency whose cross-check was not performed says so in the row's notes (review N3)", () => {
  const cc = { verdict: "not_performed", compared: 2, agreed: 2, disagreed: 0, detail: "only 2 marker(s) seen cleanly by both sources; the median rule needs 3, so neither marker rule decides" };
  const { kpis } = computeRowKpis(withSummary(focusFixture(), { secondary: "dda", crosscheck: cc }));
  assert.equal(kpis.output_latency_ms.state, "measured");
  assert.deepEqual(kpis.output_latency_ms.notes, [`the capture cross-check was not performed: ${cc.detail}`]);
  const none = computeRowKpis(withSummary(focusFixture(), { secondary: null, crosscheck: { ...cc, detail: "no secondary capture source in this run" } }));
  assert.ok(!(none.kpis.output_latency_ms.notes ?? []).some((n) => /cross-check was not performed/.test(n)), "no secondary source: nothing was skipped");
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
  assert.match(probeHealthGate(indexProbeSegment(lagging)).detail, /judged as the p95, bound 100 ms, \d+ frame\(s\) over it/, "which statistic was judged (Phase 2 round 3)");
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

test("memory at the end is the probe's LAST sample: a last sample not read whole is unknown, never an earlier reading (Phase 2 round 2 NM1)", () => {
  const last = SOAK_MS / 500; // the fixture samples every 500 ms, t0..t1 inclusive
  const tail = computeRowKpis(focusFixture({ watch: { groupNote: (g, k) => (g === "host" && k === last ? "unreadable_members" : null) } }));
  assertUnknown(tail.kpis.memory_mb, "harness_error");
  assert.match(tail.kpis.memory_mb.detail, /last host sample was not readable whole/);
  assert.equal(tail.kpis.cpu_pct.state, "measured", "only the end value is missing");
  const gone = computeRowKpis(focusFixture({ watch: { groupNote: (g, k) => (g === "workload" && k === last ? "no_live_process" : null) } }));
  assertUnknown(gone.kpis.workload_memory_mb, "harness_error");
  // The value is the probe's memory_mb_end, not the harness's own last readable sample.
  const soak = watchSegment({ t0: T0, durationMs: SOAK_MS, memMb: 300 });
  const patched = soak.map((r) => (r.type === "summary" ? { ...r, processes: { ...r.processes, host: { ...r.processes.host, memory_mb_end: 512.5 } } } : r));
  const { kpis } = computeRowKpis({ ...focusFixture(), soak: { records: patched } });
  assert.equal(kpis.memory_mb.state, "measured");
  assert.equal(kpis.memory_mb.value, 512.5);
});

test("a null probe.behind with frames is never 'kept up': every KPI it guards is harness_error (Phase 2 round 2 NS1)", () => {
  const r = computeRowKpis(focusFixture({ watch: { behind: null } }));
  for (const id of ["output_latency_ms", "frame_interval_ms", "jank_share", "marker_loss", "marker_skipped", "hangs"]) {
    assertUnknown(r.kpis[id], "harness_error");
  }
  const unknownLag = watchSegment({ t0: T0 + BigInt(SOAK_MS + 1000) * MS, durationMs: 11000, behind: null });
  const gate = probeHealthGate(indexProbeSegment(unknownLag));
  assert.equal(gate.reason, "harness_error");
  assert.match(gate.detail, /could not show it kept up/);
  const sw = computeRowKpis({ ...focusFixture(), n: 2, switches: { records: unknownLag, list: [{ k: 0, tCmdNs: T0 + BigInt(SOAK_MS + 2000) * MS }] } });
  assertUnknown(sw.kpis.switch_latency_ms, "harness_error");
  const sp = computeRowKpis({ ...focusFixture(), spawns: { records: unknownLag, list: [{ session: 3, tCmdNs: T0 + BigInt(SOAK_MS + 2000) * MS }] } });
  assertUnknown(sp.kpis.spawn_latency_ms, "harness_error");
  // A probe that kept up, and one that saw no frame at all, are not gated here.
  assert.equal(probeHealthGate(indexProbeSegment(watchSegment({ t0: T0, durationMs: 2000, behind: false }))), null);
  assert.equal(probeHealthGate(indexProbeSegment(watchSegment({ t0: T0, durationMs: 2000, behind: null, captureFailed: true }))), null);
});

test("a silent session whose generator process the probe saw exit is named a workload death (Phase 2 round 2 NN3)", () => {
  const silent = genLog({ session: 0, t0: T0 - 5000n * MS, durationMs: 20000, exit: false });
  const soak = watchSegment({ t0: T0, durationMs: SOAK_MS });
  let marked = false;
  const records = soak.map((r) => {
    if (r.type !== "sample" || marked || toBig(r.t_ns) < T0 + 16000n * MS) return r;
    marked = true;
    // An exit the probe could not tie to a slot file carries no session and names none.
    return { ...r, exited: [...r.exited, { pid: 4242, group: "workload", name: "node", session: 0 }, { pid: 4243, group: "workload", name: "sh" }] };
  });
  assert.ok(marked);
  assert.deepEqual(generatorExits(indexProbeSegment(records)), [0]);
  const r = computeRowKpis({ ...focusFixture(), gens: indexGenerators(new Map([[0, silent]])), soak: { records } });
  assertUnknown(r.kpis.producer_stall_pct, "harness_error");
  assert.match(r.kpis.producer_stall_pct.detail, /generator process of session\(s\) 0 exit \(a workload death, not a host hang\)/);
  const quiet = computeRowKpis({ ...focusFixture(), gens: indexGenerators(new Map([[0, silent]])) });
  assert.doesNotMatch(quiet.kpis.producer_stall_pct.detail, /workload death/, "no exit seen, none claimed");
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
  assert.equal(injectAbortReason("keyboard_state"), "input_unsafe", "a held modifier or CapsLock: nothing safe to type (Phase 2 keyboard safety)");
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

test("a generator silent after the window makes both generator KPIs unknown, never a smaller sample", () => {
  const silent = genLog({ session: 0, t0: T0 - 5000n * MS, durationMs: 20000, exit: false });
  const healthy = genLog({ session: 1, t0: T0 - 5000n * MS, durationMs: SOAK_MS + 15000 });
  const gw = generatorWindow(indexGenerators(new Map([[0, silent], [1, healthy]])), { t0: T0, t1: T0 + BigInt(SOAK_MS) * MS });
  assert.deepEqual(gw.silent, [0]);
  assert.equal(gw.stall.length, 1, "only the session with a reading");
  const r = computeRowKpis({ ...focusFixture(), gens: indexGenerators(new Map([[0, silent], [1, healthy]])) });
  assertUnknown(r.kpis.producer_stall_pct, "harness_error");
  assertUnknown(r.kpis.delivered_ratio, "harness_error");
  assert.match(r.kpis.producer_stall_pct.detail, /session\(s\) 0 wrote no generator record/);
});

/**
 * The review's reproduction (scratchpad review-p3/stall.mjs): a generator blocked
 * inside one write from t0+20 s until teardown closed the PTY at t0+70 s. Its last
 * progress is at t0+20 s, and its exit record's `blocked_ns` leaves the failed
 * write out (`timedWrite` credits a write only when it returns).
 */
function wedgedGenerator() {
  const S = 1_000_000_000n;
  const t0 = 10_000n * S;
  const recs = [{ type: "start", session: 0, t0_ns: String(t0 - 5n * S) }];
  let blocked = 0;
  for (let t = t0 - 5n * S; t <= t0 + 20n * S; t += S / 2n) {
    recs.push({ type: "progress", t_ns: String(t), blocked_ns: blocked, bytes_written: 1000, bytes_scheduled: 1000 });
    blocked += 1_000_000;
  }
  recs.push({ type: "exit", reason: "stdout_closed", t_ns: String(t0 + 70n * S), blocked_ns: blocked, bytes_written: 1000, bytes_scheduled: 1000 });
  const healthy = [{ type: "start", session: 1, t0_ns: String(t0 - 5n * S) }];
  let b2 = 0;
  for (let t = t0 - 5n * S; t <= t0 + 75n * S; t += S / 2n) {
    healthy.push({ type: "progress", t_ns: String(t), blocked_ns: b2, bytes_written: 1000, bytes_scheduled: 1000 });
    b2 += 1_000_000;
  }
  return { t0, S, gens: indexGenerators(new Map([[0, recs], [1, healthy]])) };
}

test("a generator blocked inside a write that never returned counts as stalled for the whole block", () => {
  const { t0, S, gens } = wedgedGenerator();
  const gw = generatorWindow(gens, { t0, t1: t0 + 40n * S });
  // 40 ordinary 500 ms segments at 1 ms each, then 20 000 ms with no JavaScript run.
  assert.ok(Math.abs(gw.stall[0] - ((40 + 20000) / 40000) * 100) < 1e-9, `wedged ${gw.stall[0]}`);
  assert.ok(Math.abs(gw.stall[1] - 0.2) < 1e-9, `healthy ${gw.stall[1]}`);
  assert.deepEqual(gw.gaps.map((g) => [g.session, Math.round(g.ms)]), [[0, 20000]]);
  assert.match(gw.notes.join("\n"), /ran no JavaScript for part of the window \(session 0: 20000 ms\)/);
  // A heuristic, said as one (review round 2 nit 4): a lower bound, with the look-alikes named.
  assert.match(gw.notes.join("\n"), /Only gaps over 1000 ms are seen, so this is a lower bound/);
  assert.match(gw.notes.join("\n"), /a GC pause or CPU starvation of the generator looks the same/);
  assert.deepEqual(gw.silent, []);
});

test("an ordinary record cadence is interpolated, not mistaken for a block", () => {
  const { kpis } = computeRowKpis(focusFixture());
  // genLog: 1 ms blocked per write at 10 writes a second = 1 %, with records every 500 ms.
  assert.ok(Math.abs(kpis.producer_stall_pct.stats.mean - 1) < 0.05, `stall ${kpis.producer_stall_pct.stats.mean}`);
  assert.ok(!(kpis.producer_stall_pct.notes ?? []).some((n) => /no JavaScript/.test(n)));
});

test("the clock verdict gates exactly the KPIs that compare node's clock with the probe's", () => {
  const caps = { "clock.aligned": { state: "UNKNOWN", reason: "node not found" } };
  const injectT0 = T0 + BigInt(SOAK_MS + 1000) * MS;
  const keys = [];
  const echoes = [];
  const seen = [];
  for (let i = 0; i < 120; i += 1) {
    const tInject = injectT0 + BigInt(i * 100) * MS;
    keys.push({ type: "inject", index: i, key: "a", t_inject_ns: String(tInject), t_check_ns: String(tInject), tagged: true });
    echoes.push({ seq: i, byte: 97, tEmit: tInject + 2n * MS });
    seen.push({ session: 0, seq: i, t: tInject + 30n * MS });
  }
  const gen = genLog({ session: 0, t0: T0 - 5000n * MS, durationMs: SOAK_MS + 30000, echoes });
  const inputRecords = watchSegment({ t0: injectT0 - 200n * MS, durationMs: 13000, sessions: [{ session: 0, gen, latencyMs: 20 }], echoSeen: seen });
  const r = computeRowKpis({
    ...focusFixture({ caps }),
    n: 2,
    gens: indexGenerators(new Map([[0, gen]])),
    input: { records: inputRecords, injects: keys, focusSession: 0, echoBase: 0 },
    switches: { records: inputRecords, list: [{ k: 0, tCmdNs: injectT0 }] },
    spawns: { records: inputRecords, list: [{ session: 0, tCmdNs: injectT0 }] },
  });
  // Both of input latency's times are the probe's (inject's t_inject, watch's first_seen).
  assert.equal(r.kpis.input_latency_ms.state, "measured", JSON.stringify(r.kpis.input_latency_ms));
  // Output, switch and spawn latency each set a node time against a probe time.
  assertUnknown(r.kpis.output_latency_ms, "clock_unaligned");
  assertUnknown(r.kpis.switch_latency_ms, "clock_unaligned");
  assertUnknown(r.kpis.spawn_latency_ms, "clock_unaligned");
});

test("a switch counts the first frame that shows the session ALONE, not a repaint showing it among others", () => {
  const gens = [0, 1].map((s) => genLog({ session: s, t0: T0 - 5000n * MS, durationMs: SOAK_MS + 30000 }));
  const sw0 = T0 + BigInt(SOAK_MS + 1050) * MS;
  const list = [];
  const records = watchSegment({ t0: sw0 - 100n * MS, durationMs: 11000, sessions: [], frameHz: 60 });
  const marker = (session, t) => {
    let cur = null;
    for (const r of gens[session]) if (r.type === "marker" && BigInt(r.t_emit_ns) <= t) cur = r;
    return { kind: "output", session, seq: cur.seq, rect: { x: 0, y: 0, w: 1, h: 1 } };
  };
  for (let j = 0; j < 20; j += 1) {
    const tCmd = sw0 + BigInt(j * 500) * MS;
    list.push({ k: 1, tCmdNs: tCmd });
    // 30 ms: the grid repaints with session 1 shown beside session 0 (v = 2).
    records.push({ type: "frame", source: "x11", index: 0, t_ns: String(tCmd + 30n * MS), w: 1, h: 1, v: 2, markers: [marker(0, tCmd + 30n * MS), marker(1, tCmd + 30n * MS)], rejected: 0 });
    // 90 ms: session 1 alone (v = 1) — the switch.
    records.push({ type: "frame", source: "x11", index: 0, t_ns: String(tCmd + 90n * MS), w: 1, h: 1, v: 1, markers: [marker(1, tCmd + 90n * MS)], rejected: 0 });
  }
  const r = computeRowKpis({ ...focusFixture(), n: 2, gens: indexGenerators(new Map([[0, gens[0]], [1, gens[1]]])), switches: { records, list } });
  assert.equal(r.kpis.switch_latency_ms.state, "measured");
  assert.ok(Math.abs(r.kpis.switch_latency_ms.stats.p50 - 90) < 1e-6, `switch p50 ${r.kpis.switch_latency_ms.stats.p50}`);
});

test("a probe value outside the catalog's range is the harness's error, and its detail never quotes the number", () => {
  const over = fromProbe("jank_share", { state: "measured", unit: "%", n: 500, value: 140.25 });
  assertUnknown(over, "harness_error");
  assert.match(over.detail, /outside the catalog's range \[0, 100\]/);
  assert.doesNotMatch(over.detail, /140/);
  const negative = fromProbe("output_latency_ms", { state: "measured", unit: "ms", n: 400, stats: { p50: -3.5, p95: 4, p99: 5 } });
  assertUnknown(negative, "harness_error");
  assert.match(negative.detail, /p50 lies outside/);
  const fractionalCount = fromProbe("hangs", { state: "measured", unit: "count", n: 80, value: 1.5 });
  assertUnknown(fractionalCount, "harness_error");
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

test("a probe with too few known lags for a verdict: harness_error naming the count, and no statistic is claimed (Phase 2 round 4)", () => {
  const soak = watchSegment({ t0: T0, durationMs: 2000, behind: false });
  const few = (patch) => indexProbeSegment(soak.map((r) => (r.type === "summary" ? { ...r, probe: { ...r.probe, ...patch } } : r)));
  const frames = soak.find((r) => r.type === "summary").probe.frames;
  // 115 of 120 frames without a known lag: the probe gives no verdict.
  const g = probeHealthGate(few({ behind: null, lag_statistic: null, lag_ms_judged: null, frames_lag_unknown: frames - 5 }));
  assert.equal(g.reason, "harness_error");
  assert.equal(g.detail, `the probe could not show it kept up with its capture source: only 5 of ${frames} primary frame(s) had a known presentation time (a verdict needs 10)`);
  // A behind verdict whose statistic is not named reads without one, never "as the null".
  const b = probeHealthGate(few({ behind: true, lag_statistic: null, lag_ms_judged: 250, frames_over_bound: 12 }));
  assert.equal(b.detail, "the probe fell behind its capture source (frame lag 250 ms judged, bound 100 ms, 12 frame(s) over it), so first appearances may be late");
  assert.match(probeHealthGate(few({ behind: true, lag_statistic: "second_largest", lag_ms_judged: 250 })).detail, /judged as the second_largest,/);
});

test("LAG_VERDICT_MIN_FRAMES is the probe's own constant, read from its source (review round 7)", () => {
  // The probe owns the floor (crates/termhost-probe/src/model/mod.rs); kpis.mjs
  // restates it in the text it writes, so the two must never drift apart.
  const src = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "..", "..", "..", "crates", "termhost-probe", "src", "model", "mod.rs"), "utf8");
  const found = [...src.matchAll(/^pub const LAG_VERDICT_MIN_FRAMES: usize = (\d+);\r?$/gm)];
  assert.equal(found.length, 1, "exactly one definition of the probe's constant");
  assert.equal(LAG_VERDICT_MIN_FRAMES, Number(found[0][1]));
});

test("every inject abort reason the probe writes has an explicit catalog mapping, read from its source", () => {
  const src = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "..", "..", "..", "crates", "termhost-probe", "src", "inject.rs"), "utf8");
  const probe = [...src.matchAll(/^\s*AbortReason::\w+ => "([a-z_]+)",\r?$/gm)].map((m) => m[1]).sort();
  assert.ok(probe.includes("keyboard_state"), "the probe writes keyboard_state");
  assert.deepEqual(Object.keys(INJECT_ABORT_REASONS).sort(), probe);
});

/**
 * A soak's raw records with every primary frame between `fromMs` and `toMs` into
 * the watch (`toMs` null = to the end) showing session 0's output marker frozen at
 * the sequence it showed at `fromMs`. After `toMs` the frames are the fixture's
 * own, so the marker advances again: a hitch the pane recovered from.
 */
function freezeFrames(records, fromMs, toMs = null) {
  const start = BigInt(records.find((r) => r.type === "start").t0_ns) + 5n * MS;
  const a = start + BigInt(fromMs) * MS;
  const b = toMs === null ? null : start + BigInt(toMs) * MS;
  let seq = null;
  return records.map((r) => {
    if (r.type !== "frame") return r;
    const t = BigInt(r.t_ns);
    if (t < a || (b !== null && t > b)) return r;
    const m = r.markers.find((x) => x.kind === "output" && x.session === 0);
    if (!m) return r;
    seq ??= m.seq;
    return { ...r, markers: r.markers.map((x) => (x.kind === "output" && x.session === 0 ? { ...x, seq } : x)) };
  });
}

test("a pane frozen to the end of the soak, whole or from half way, voids the per-marker KPIs (ci run 38351621760, review r9 SF)", () => {
  const healthy = computeRowKpis(focusFixture());
  assert.deepEqual(healthy.stalls, [], "a pane whose marker advances is not stalled");
  assert.equal(healthy.kpis.output_latency_ms.state, "measured");
  const f = focusFixture();
  // The whole soak frozen.
  const whole = computeRowKpis({ ...f, soak: { records: stallSegment(f.soak.records) } });
  assert.equal(whole.stalls.length, 1);
  assert.equal(whole.stalls[0].session, 0);
  assert.equal(whole.stalls[0].recovered, false);
  assert.ok(whole.stalls[0].ms > SOAK_MS - 1000, JSON.stringify(whole.stalls[0]));
  // Frozen half way to the end: the first half's samples would read measured; they must not.
  const half = computeRowKpis({ ...f, soak: { records: stallSegment(f.soak.records, { fromMs: SOAK_MS / 2 }) } });
  assert.equal(half.stalls.length, 1, "a partial freeze is found");
  assert.equal(half.stalls[0].recovered, false, "frozen to the end is a wedge");
  assert.ok(Math.abs(half.stalls[0].ms - SOAK_MS / 2) < 1000, JSON.stringify(half.stalls[0]));
  for (const r of [whole, half]) {
    for (const id of STALL_VOIDED_KPIS) {
      assertUnknown(r.kpis[id], "pane_stalled");
      assert.match(r.kpis[id].detail, /^pane stalled: session 0 stayed at seq \d+ for \d+ ms \(\d+ frame\(s\), last shown at \+\d+ ms(, then no frame for \d+ ms)?\) while its generator wrote \d+ marker\(s\), and did not advance in any later frame \(a wedge on what was seen: the still marker alone meets the bar\)$/);
    }
    assert.equal(r.kpis.frame_interval_ms.state, "measured", "the window's own cadence is not the pane's");
    assert.ok((r.kpis.output_latency_ms.notes ?? []).some((n) => /^pane stalled: session 0 /.test(n)), "the wedge note carries its KPI id");
    assert.ok(!r.notes.some((n) => /pane stalled/.test(n)), "no unattached copy");
  }
});

test("a hitch the pane recovers from keeps the marker KPIs measured and is still reported (review r10 MF1)", () => {
  const f = focusFixture();
  const healthy = computeRowKpis(f);
  // A 2.3 s freeze at 10-12.3 s of the 40 s soak, then the marker advances again.
  const r = computeRowKpis({ ...f, soak: { records: freezeFrames(f.soak.records, 10_000, 12_300) } });
  assert.equal(r.stalls.length, 1, "the hitch is reported");
  assert.equal(r.stalls[0].recovered, true, JSON.stringify(r.stalls[0]));
  // The fixture shows a sequence for up to one marker period (100 ms) before and after.
  assert.ok(Math.abs(r.stalls[0].ms - 2300) <= 100, JSON.stringify(r.stalls[0]));
  for (const id of STALL_VOIDED_KPIS) {
    assert.equal(r.kpis[id].state, healthy.kpis[id].state, `${id} keeps its state`);
    assert.notEqual(r.kpis[id].reason, "pane_stalled", `${id}: ${JSON.stringify(r.kpis[id])}`);
  }
  assert.equal(r.kpis.output_latency_ms.state, "measured");
  assert.ok(
    (r.kpis.output_latency_ms.notes ?? []).some((n) => /^pane hitch, recovered .*session 0 stayed at seq \d+ for \d+ ms .*, then advanced again$/.test(n)),
    JSON.stringify(r.kpis.output_latency_ms.notes),
  );
  assert.match(describeStall(r.stalls[0]), /, then advanced again$/);
  // A wedge at the end voids, even when an earlier hitch was longer; both are reported.
  const both = computeRowKpis({ ...f, soak: { records: freezeFrames(freezeFrames(f.soak.records, 5_000, 9_000), SOAK_MS - 2_600) } });
  assert.deepEqual(both.stalls.map((x) => x.recovered), [true, false], JSON.stringify(both.stalls));
  assertUnknown(both.kpis.output_latency_ms, "pane_stalled");
  assert.ok((both.kpis.output_latency_ms.notes ?? []).some((n) => /^pane hitch, recovered/.test(n)), "the hitch is still named");
});

test("the stall rule does not run on an unaligned clock, and never overwrites another reason (review r10 SF2)", () => {
  const caps = { "clock.aligned": { state: "UNKNOWN", reason: "node not found" } };
  const f = focusFixture({ caps });
  const r = computeRowKpis({ ...f, soak: { records: stallSegment(f.soak.records, { fromMs: SOAK_MS / 2 }) } });
  assert.deepEqual(r.stalls, [], "emission times cannot be set against frame times");
  assertUnknown(r.kpis.output_latency_ms, "clock_unaligned");
  for (const id of STALL_VOIDED_KPIS) assert.notEqual(r.kpis[id].reason, "pane_stalled", id);
  // An aligned clock, but a KPI that is already unknown for another reason: the
  // wedge is noted on it, the reason stays.
  const g = focusFixture();
  const refusing = withSummary({ ...g, soak: { records: stallSegment(g.soak.records, { fromMs: SOAK_MS / 2 }) } }, {
    marker_loss: { state: "unknown", unit: "%", n: 0, reason: "refresh_rate_unknown" },
  });
  const w = computeRowKpis(refusing);
  assert.equal(w.stalls.length, 1);
  assertUnknown(w.kpis.marker_loss, "refresh_rate_unknown");
  assert.ok((w.kpis.marker_loss.notes ?? []).some((n) => /^pane stalled: session 0/.test(n)), JSON.stringify(w.kpis.marker_loss));
  assertUnknown(w.kpis.output_latency_ms, "pane_stalled");
});

/** The probe's own probe-behind verdict on its capture KPIs, as its summary carries it. */
const probeBehind = (unit) => ({ state: "unknown", unit, n: 0, reason: "harness_error", detail: "the probe fell behind: its p95 frame lag was 120.0 ms (40 of 400 frames over the 93.8 ms bound; software-compositor), so markers may have appeared only in later frames" });
const withProbeBehind = (records) =>
  records.map((r) => (r.type === "summary" ? { ...r, probe: { ...r.probe, behind: true, lag_ms_judged: 120, lag_bound_ms: 93.75 }, output_latency_ms: probeBehind("ms"), marker_loss: probeBehind("%"), marker_skipped: probeBehind("%") } : r));

/** A windows-hosted WT smoke document holding `kpis` for N = 1 focus, gated as the job gates it. */
function smokeFails(kpis) {
  const doc = {
    machine: { environment: "windows-hosted" },
    capabilities: { "capture.wgc": { state: "OPERATIVE", reason: "fake" } },
    rows: [{ setup: { id: "windows-terminal" }, scenario: { n: 1, arm: "focus", run_index: 0 }, kpis }],
  };
  return checkAssertions(doc, { atN: [1], setups: ["windows-terminal"], arms: ["focus"], requireMeasuredIfOperative: [{ mechanism: "capture.wgc", kpis: ["output_latency_ms"] }], acceptCaptureHealthUnknown: true });
}

test("a voiding wedge overrides a capture-health unknown — probe behind, or capture_disagreement — so a wedged third-party pane FAILS the smoke even with --accept-capture-health-unknown (head confirmation of 8b3f54a98 M1)", () => {
  for (const [label, f, patch, before] of [
    ["probe behind", focusFixture(), withProbeBehind, "harness_error"],
    ["capture_disagreement", focusFixture({ watch: { crosscheck: "disagree" } }), (x) => x, "capture_disagreement"],
  ]) {
    const healthOnly = computeRowKpis({ ...f, environment: "windows-hosted", soak: { records: patch(f.soak.records) } });
    assertUnknown(healthOnly.kpis.output_latency_ms, before);
    assert.deepEqual(smokeFails(healthOnly.kpis), [], `${label} alone: the flag excuses it (plumbing check, D6)`);
    const w = computeRowKpis({ ...f, environment: "windows-hosted", soak: { records: patch(stallSegment(f.soak.records, { fromMs: SOAK_MS / 2 })) } });
    assert.equal(w.stalls.filter((x) => x.voids).length, 1, `${label}: the wedge is seen from frame presence`);
    for (const id of STALL_VOIDED_KPIS) {
      if (w.kpis[id].state === "measured") continue;
      if (healthOnly.kpis[id].state === "unknown" && healthOnly.kpis[id].reason === before) assertUnknown(w.kpis[id], "pane_stalled");
    }
    assertUnknown(w.kpis.output_latency_ms, "pane_stalled");
    assert.ok(
      (w.kpis.output_latency_ms.notes ?? []).some((n) => n.startsWith(`also unknown for capture health: ${before}`)),
      `${label}: the capture-health reading is kept as a note: ${JSON.stringify(w.kpis.output_latency_ms.notes)}`,
    );
    const fails = smokeFails(w.kpis);
    assert.equal(fails.length, 1, `${label}: a wedged pane fails the smoke even with the flag`);
    assert.match(fails[0], /output_latency_ms is unknown\(pane_stalled\)/);
  }
});

/** A probe segment for paneStalls alone: x11 primary, the given frames and intervals. */
function synthSeg(frames, intervals = []) {
  return {
    captures: [{ type: "capture", source: "x11", state: "started" }],
    summary: { primary: "x11" },
    frames,
    intervals,
    firstSeen: [],
  };
}
const at = (ms) => T0 + BigInt(ms) * MS;
/** A frame at `ms` showing session 0's output marker at `seq` (null = not shown). */
const fr = (ms, seq, source = "x11") => ({ source, t_ns: String(at(ms)), markers: seq === null ? [] : [{ kind: "output", session: 0, seq }] });
/** Session 0's generator, with output markers emitted at these ms. */
const gensAt = (msList) => new Map([[0, { markers: msList.map((ms, i) => ({ kind: "output", seq: 10_000 + i, tEmit: at(ms) })) }]]);
/** Emission times every `step` ms over [a, b]. */
const every = (a, b, step) => Array.from({ length: Math.floor((b - a) / step) + 1 }, (_, i) => a + i * step);
const WIN = { t0: at(0), t1: at(60_000) };
/**
 * paneStalls over a synthetic segment. With `WIN`, the window ends at the last
 * frame, so no frameless tail is read into it (that has its own tests).
 */
const paneStallsAt = (seg, gens, win, vis) => {
  const last = seg.frames.reduce((m, f) => (BigInt(f.t_ns) > m ? BigInt(f.t_ns) : m), WIN.t0 + 1n);
  return paneStalls(seg, gens, win === WIN ? { t0: WIN.t0, t1: last } : win, vis);
};
const V0 = new Set([0]);
/** Advancing frames (one seq per 100 ms) before `fromMs`, `seq` 500 still over [fromMs, toMs], advancing after when `recover`. */
function runFrames(fromMs, toMs, { recover = true, step = 100 } = {}) {
  const out = [];
  for (let t = 0, s = 1; t < fromMs; t += 100, s += 1) out.push(fr(t, s));
  for (let t = fromMs; t < toMs; t += step) out.push(fr(t, 500));
  out.push(fr(toMs, 500));
  if (recover) for (let t = toMs + 100, s = 501; t <= toMs + 1000; t += 100, s += 1) out.push(fr(t, s));
  return out;
}

test("the stall rule's boundaries: 2 s, 20 markers, the emission window's edges (review r10 SF5)", () => {
  const dense = gensAt(every(0, 20_000, 10));
  // 1990 ms still is no stall; 2000 and 2010 ms are.
  assert.deepEqual(paneStallsAt(synthSeg(runFrames(1000, 2990)), dense, WIN, V0), [], "1990 ms");
  assert.equal(paneStallsAt(synthSeg(runFrames(1000, 3000)), dense, WIN, V0).length, 1, "2000 ms exactly");
  const st = paneStallsAt(synthSeg(runFrames(1000, 3010)), dense, WIN, V0);
  assert.equal(st.length, 1, "2010 ms");
  assert.deepEqual(st[0], { session: 0, seq: 500, ms: 2010, frames: 22, emitted: 201, recovered: true, lastShownMs: 3010, framelessMs: 0, tailEmitted: 0, basis: "seen", voids: false, tailBarMs: 10000 });
  assert.ok(STALL_MIN_MS === 2000 && STALL_MIN_EMITTED === 20);
  // 19 markers emitted in the run is no stall; 20 is.
  const run = synthSeg(runFrames(1000, 3500));
  assert.deepEqual(paneStalls(run, gensAt(every(1100, 1100 + 18 * 100, 100)), WIN, V0), [], "19 emitted");
  assert.equal(paneStalls(run, gensAt(every(1100, 1100 + 19 * 100, 100)), WIN, V0).length, 1, "20 emitted");
  // The emission window is (first frame, last frame]: one marker exactly at the
  // run's last frame counts, one exactly at its first does not.
  const inside19 = every(1100, 1100 + 18 * 100, 100); // 1100..2900, strictly inside (1000, 3500)
  assert.equal(inside19.length, 19);
  assert.equal(paneStalls(run, gensAt([...inside19, 3500]), WIN, V0)[0]?.emitted, 20, "an emission at t1 counts (t <= t1)");
  assert.deepEqual(paneStalls(run, gensAt([1000, ...inside19]), WIN, V0), [], "an emission at t0 does not (t > t0)");
});

test("the stall rule picks the longest of several hitches, and a wedge over any of them (review r10 SF5)", () => {
  const dense = gensAt(every(0, 30_000, 10));
  // Three recovered runs: 2.5 s, 3.5 s, 2.2 s.
  const frames = [];
  let t = 0;
  let s = 1;
  const advance = (ms) => {
    for (const end = t + ms; t < end; t += 100) frames.push(fr(t, s++));
  };
  const still = (ms) => {
    const seq = s++;
    for (const end = t + ms; t <= end; t += 100) frames.push(fr(t, seq));
    return seq;
  };
  advance(1000);
  still(2500);
  advance(1000);
  const longest = still(3500);
  advance(1000);
  still(2200);
  advance(1000);
  const st = paneStallsAt(synthSeg(frames), dense, WIN, V0);
  assert.equal(st.length, 1);
  assert.equal(st[0].seq, longest, JSON.stringify(st[0]));
  assert.equal(st[0].ms, 3500);
  assert.equal(st[0].recovered, true);
  // The same, then frozen 2.4 s to the last frame: the wedge AND the longest hitch, in time order.
  const wedgeSeq = still(2400);
  const w = paneStallsAt(synthSeg(frames), dense, WIN, V0);
  assert.deepEqual(w.map((x) => [x.seq, x.recovered]), [[longest, true], [wedgeSeq, false]]);
});

test("the stall rule reads primary frames inside the window only, in time order (review r10 SF4/SF5)", () => {
  const dense = gensAt(every(0, 20_000, 10));
  // Frames outside the window are ignored: a 3 s run of which 1.5 s is in the window.
  const frames = runFrames(1000, 4000, { recover: false });
  assert.equal(paneStallsAt(synthSeg(frames), dense, WIN, V0).length, 1, "the whole run qualifies");
  assert.deepEqual(paneStallsAt(synthSeg(frames), dense, { t0: at(2500), t1: at(4000) }, V0), [], "frames before t0 are ignored");
  assert.deepEqual(paneStallsAt(synthSeg(frames), dense, { t0: WIN.t0, t1: at(2500) }, V0), [], "frames after t1 are ignored");
  // Only the primary source: a cross-check source (dda) showing an old marker is not the pane.
  // The primary source shows seq 5 then seq 6 with no frame between (a still screen
  // is no evidence either way); dda shows seq 7 for 4 s in that stretch.
  const primaryOnly = [fr(0, 4), fr(400, 5), fr(4600, 6), fr(5000, 7)];
  const ddaStill = every(500, 4500, 100).map((t) => fr(t, 7, "dda"));
  assert.deepEqual(paneStallsAt(synthSeg([...primaryOnly, ...ddaStill]), dense, WIN, V0), [], "a dda frame is not a primary frame");
  assert.equal(paneStallsAt(synthSeg([...primaryOnly, ...ddaStill.map((x) => ({ ...x, source: "x11" }))]), dense, WIN, V0).length, 1, "the same frames as primary are a stall");
  // Unsorted input reads the same as sorted.
  const sorted = paneStallsAt(synthSeg(runFrames(1000, 3010)), dense, WIN, V0);
  const shuffled = runFrames(1000, 3010).map((x, i) => ({ x, k: (i * 7919) % 101 })).sort((a, b) => a.k - b.k).map((o) => o.x);
  assert.deepEqual(paneStallsAt(synthSeg(shuffled), dense, WIN, V0), sorted);
  assert.equal(sorted.length, 1);
});

test("time inside an interval the probe excluded for visibility or input does not count toward a stall (review r10 SF3)", () => {
  const dense = gensAt(every(0, 20_000, 10));
  const frames = runFrames(1000, 3500);
  const iv = (a, b, cls) => ({ type: "interval", index: 0, t_start_ns: String(at(a)), t_end_ns: String(at(b)), valid: cls === null, exclusion_class: cls });
  assert.equal(paneStallsAt(synthSeg(frames, [iv(1500, 2000, null)]), dense, WIN, V0).length, 1, "a valid interval");
  assert.deepEqual(paneStallsAt(synthSeg(frames, [iv(1500, 2000, "occluded")]), dense, WIN, V0), [], "occluded / not foreground / cloaked / minimized");
  assert.deepEqual(paneStallsAt(synthSeg(frames, [iv(3000, 3500, "operator_activity")]), dense, WIN, V0), [], "real input");
  assert.equal(paneStallsAt(synthSeg(frames, [iv(4000, 4500, "occluded")]), dense, WIN, V0).length, 1, "an excluded interval the run does not touch");
  assert.equal(paneStallsAt(synthSeg(frames, [iv(1500, 2000, "harness_error")]), dense, WIN, V0).length, 1, "a harness-class interval (no input monitor on Xvfb) rules nothing out");
});

test("a marker hidden for longer than the gap and shown again is a redisplay, not a stall (review r10 nit)", () => {
  const dense = gensAt(every(0, 20_000, 10));
  const before = Array.from({ length: 10 }, (_, i) => fr(i * 100, i + 1));
  const still = (a, b) => every(a, b, 100).map((t) => fr(t, 500));
  const after = Array.from({ length: 5 }, (_, i) => fr(4600 + i * 100, 501 + i));
  // Shown 1.0-1.4 s, hidden 1.5-2.9 s (frames without it), the same seq again 3.0-4.5 s.
  const hidden = [...before, ...still(1000, 1400), ...every(1500, 2900, 100).map((t) => fr(t, null)), ...still(3000, 4500), ...after];
  assert.ok(3000 - 1400 > STALL_HIDDEN_GAP_MS);
  assert.deepEqual(paneStallsAt(synthSeg(hidden), dense, WIN, V0), [], "the run ends where the marker was hidden");
  // No frame at all in the same stretch (a still screen) does not end the run.
  const silent = [...before, ...still(1000, 1400), ...still(3000, 4500), ...after];
  const st = paneStallsAt(synthSeg(silent), dense, WIN, V0);
  assert.equal(st.length, 1);
  assert.equal(st[0].ms, 3500);
  // A brief absence (a partial read that skipped the pane) does not end it either.
  const brief = [...before, ...still(1000, 2000), fr(2050, null), ...still(2100, 3500), ...after];
  assert.equal(paneStallsAt(synthSeg(brief), dense, WIN, V0).length, 1);
});

test("the stall rule counts any number of earlier appearances (review r9 nit 1)", () => {
  const f = focusFixture();
  const seg = indexProbeSegment(f.soak.records);
  const t0 = BigInt(seg.frames[0].t_ns);
  const win = { t0, t1: BigInt(seg.frames.at(-1).t_ns) };
  const a = t0 + 150n * MS;
  const b = t0 + 2650n * MS;
  let seq = null;
  const two = {
    ...seg,
    frames: seg.frames.map((fr) => {
      const t = BigInt(fr.t_ns);
      if (t < a || t > b) return fr;
      const m = fr.markers.find((x) => x.kind === "output" && x.session === 0);
      seq ??= m?.seq;
      return { ...fr, markers: fr.markers.map((x) => (x.kind === "output" && x.session === 0 ? { ...x, seq } : x)) };
    }),
  };
  // Session 0's OUTPUT marker showed at least two sequences before the freeze.
  const shownBefore = new Set(
    two.frames
      .filter((fr) => BigInt(fr.t_ns) < a)
      .flatMap((fr) => fr.markers.filter((m) => m.kind === "output" && m.session === 0).map((m) => m.seq)),
  );
  assert.ok(shownBefore.size >= 2, `session 0 output seqs before the freeze: ${[...shownBefore]}`);
  const st = paneStalls(two, f.gens, win, new Set([0]));
  assert.equal(st.length, 1);
  assert.equal(st[0].recovered, true);
  assert.ok(st[0].ms >= STALL_MIN_MS && st[0].emitted >= STALL_MIN_EMITTED, JSON.stringify(st[0]));
  assert.equal(
    describeStall({ session: 3, seq: 7, ms: 2500, frames: 150, emitted: 25, recovered: true, lastShownMs: 4000, framelessMs: 0 }),
    "session 3 stayed at seq 7 for 2500 ms (150 frame(s), last shown at +4000 ms) while its generator wrote 25 marker(s), then advanced again",
  );
  assert.equal(
    describeStall({ session: 3, seq: 7, ms: 2500, frames: 150, emitted: 25, recovered: false, lastShownMs: 1000, framelessMs: 1500 }),
    "session 3 stayed at seq 7 for 2500 ms (150 frame(s), last shown at +1000 ms, then no frame for 1500 ms) while its generator wrote 25 marker(s), and did not advance in any later frame",
  );
});

test("a frozen pane hidden for a while and shown again at the same seq is still a wedge (review r11 MF1, repro A)", () => {
  const dense = gensAt(every(0, 20_000, 10));
  const before = Array.from({ length: 10 }, (_, i) => fr(i * 100, i + 1));
  // seq 500 shown 1.0-3.5 s, hidden 3.6-5.0 s, seq 500 again 5.1-5.5 s to the end.
  const frames = [
    ...before,
    ...every(1000, 3500, 100).map((t) => fr(t, 500)),
    ...every(3600, 5000, 100).map((t) => fr(t, null)),
    ...every(5100, 5500, 100).map((t) => fr(t, 500)),
  ];
  const st = paneStallsAt(synthSeg(frames), dense, WIN, V0);
  assert.equal(st.length, 1, JSON.stringify(st));
  assert.equal(st[0].seq, 500);
  assert.equal(st[0].recovered, false, "no later frame advanced past 500");
  assert.equal(st[0].ms, 2900, "seen 2.5 s + 0.4 s; the hidden 1.6 s is not counted");
  // A BACKWARDS seq after the run is no recovery either.
  const back = [...before, ...every(1000, 3500, 100).map((t) => fr(t, 500)), ...every(3600, 4000, 100).map((t) => fr(t, 400))];
  const b = paneStallsAt(synthSeg(back), dense, WIN, V0);
  assert.deepEqual(b.map((x) => [x.seq, x.recovered]), [[500, false]]);
  // A forward seq after the gap is.
  const fwd = [...before, ...every(1000, 3500, 100).map((t) => fr(t, 500)), ...every(3600, 5000, 100).map((t) => fr(t, null)), fr(5100, 501)];
  assert.deepEqual(paneStallsAt(synthSeg(fwd), dense, WIN, V0).map((x) => x.recovered), [true]);
});

test("absence is measured from the first frame lacking the marker: one band frame after a still screen does not split a stall (review r11 SF3, repro B)", () => {
  const dense = gensAt(every(0, 20_000, 10));
  const before = Array.from({ length: 10 }, (_, i) => fr(i * 100, i + 1));
  // seq 500 at 1.0-1.5 s, no frames 1.5-3.0 s, one frame at 3.0 s without it, 500 again 3.05-4.0 s, then it advances.
  const frames = [...before, ...every(1000, 1500, 100).map((t) => fr(t, 500)), fr(3000, null), ...every(3050, 4000, 50).map((t) => fr(t, 500)), fr(4100, 501)];
  const st = paneStallsAt(synthSeg(frames), dense, WIN, V0);
  assert.equal(st.length, 1, JSON.stringify(st));
  assert.equal(st[0].ms, 3000);
  assert.equal(st[0].recovered, true);
});

test("a hidden stretch just over the gap is not bridged into a stall (review r11 nit, repro C)", () => {
  const dense = gensAt(every(0, 20_000, 10));
  assert.ok(STALL_HIDDEN_GAP_MS * 2 < STALL_MIN_MS, "the gap is strictly under half the stall bar");
  const before = Array.from({ length: 10 }, (_, i) => fr(i * 100, i + 1));
  // seq 500 at 1.00 and 1.05 s, missing 1.06-1.99 s (950 ms to the next showing), 500 again 2.01-3.05 s.
  const frames = [
    ...before,
    fr(1000, 500),
    fr(1050, 500),
    ...every(1060, 1990, 10).map((t) => fr(t, null)),
    ...every(2010, 3050, 10).map((t) => fr(t, 500)),
    fr(3100, 501),
  ];
  assert.deepEqual(paneStallsAt(synthSeg(frames), dense, WIN, V0), [], "seen 1.09 s; 950 ms hidden is not counted");
});

test("an excluded interval inside a wedge splits it; the pieces are still the wedge (review r11 nit)", () => {
  const dense = gensAt(every(0, 20_000, 10));
  const before = Array.from({ length: 10 }, (_, i) => fr(i * 100, i + 1));
  const frames = [...before, ...every(1000, 8000, 100).map((t) => fr(t, 500))];
  const iv = { type: "interval", index: 4, t_start_ns: String(at(2000)), t_end_ns: String(at(2500)), valid: false, exclusion_class: "occluded" };
  const st = paneStallsAt(synthSeg(frames, [iv]), dense, WIN, V0);
  assert.equal(st.length, 1, JSON.stringify(st));
  assert.equal(st[0].recovered, false);
  assert.equal(st[0].ms, 900 + 5500, "1.0-1.9 s and 2.5-8.0 s: the span [2.0, 2.5) is half-open, so the frame at 2.5 s is kept");
});

test("excludedSpans keeps only intervals the probe marked valid: false with a visibility or input class", () => {
  const iv = (index, valid, cls, extra = {}) => ({ type: "interval", index, t_start_ns: String(at(index * 500)), t_end_ns: String(at(index * 500 + 500)), valid, exclusion_class: cls, ...extra });
  const seg = {
    intervals: [
      iv(0, false, "occluded"),
      iv(1, false, "operator_activity"),
      iv(2, true, "occluded"),
      iv(3, undefined, "occluded"),
      iv(4, false, "harness_error"),
      iv(5, false, null, { partial: true }),
    ],
  };
  assert.deepEqual(excludedSpans(seg).map((x) => [Number((x.a - T0) / MS), x.cls]), [[0, "occluded"], [500, "operator_activity"]]);
});

test("markers emitted while the pane was hidden do not count toward a stall (review r11 MF1)", () => {
  const before = Array.from({ length: 10 }, (_, i) => fr(i * 100, i + 1));
  // seq 500 seen 1.0-2.2 s and 4.1-5.0 s (2.1 s), hidden 2.3-4.0 s, then it advances.
  const frames = [
    ...before,
    ...every(1000, 2200, 100).map((t) => fr(t, 500)),
    ...every(2300, 4000, 100).map((t) => fr(t, null)),
    ...every(4100, 5000, 100).map((t) => fr(t, 500)),
    fr(5100, 501),
  ];
  // 8 emitted in each seen piece, 30 while hidden.
  const gens = gensAt([...every(1100, 1800, 100), ...every(2300, 3750, 50).slice(0, 30), ...every(4200, 4900, 100)]);
  assert.deepEqual(paneStallsAt(synthSeg(frames), gens, WIN, V0), [], "16 seen emissions are under 20");
  // The same with 10 per seen piece qualifies, so the 2.1 s is not the reason.
  const enough = gensAt([...every(1100, 2000, 100), ...every(4200, 5000, 100).slice(0, 9), 5000]);
  assert.equal(paneStallsAt(synthSeg(frames), enough, WIN, V0).length, 1);
});

/** The reviewer's repro frames (review r12, repro.mjs): advancing 0-0.9 s. */
const r12Before = every(0, 900, 100).map((t, i) => fr(t, i + 1));
const r12Gens = (sessions) => new Map(sessions.map((k) => [k, { markers: every(0, 20000, 10).map((ms, i) => ({ kind: "output", seq: i, tEmit: at(ms) })) }]));
const ivx = (a, b, cls) => ({ type: "interval", t_start_ns: String(at(a)), t_end_ns: String(at(b)), valid: false, exclusion_class: cls });
const winTo = (ms) => ({ t0: at(0), t1: at(ms) });

test("R1: an excluded interval with no frames in it ends the piece; its time and emissions are not counted (review r12 MF4)", () => {
  const frames = [...r12Before, ...every(1000, 1900, 100).map((t) => fr(t, 500)), ...every(5000, 5900, 100).map((t) => fr(t, 500)), fr(6000, 501)];
  const st = paneStalls(synthSeg(frames), r12Gens([0]), winTo(6000), V0);
  assert.equal(st[0]?.ms, 4900, "without the interval, the frameless 3.1 s is still time");
  assert.deepEqual(paneStalls(synthSeg(frames, [ivx(1950, 4950, "occluded")]), r12Gens([0]), winTo(6000), V0), [], "seen 0.9 s + 0.9 s");
});

test("R2: an advanced seq inside an excluded interval is still proof of repaint (review r12 SF5)", () => {
  const frames = [...r12Before, ...every(1000, 4000, 100).map((t) => fr(t, 500)), ...every(4100, 5000, 100).map((t, i) => fr(t, 501 + i))];
  const st = paneStalls(synthSeg(frames, [ivx(4050, 5001, "operator_activity")]), r12Gens([0]), winTo(5000), V0);
  assert.deepEqual(st.map((x) => [x.seq, x.recovered]), [[500, true]]);
});

test("R3: a seq that wraps the marker counter is recovery (review r12)", () => {
  const frames = [...every(1000, 4000, 100).map((t) => fr(t, SEQ_MODULUS - 1)), fr(4100, 0)];
  const st = paneStalls(synthSeg(frames), r12Gens([0]), winTo(4100), V0);
  assert.deepEqual(st.map((x) => [x.seq, x.recovered]), [[SEQ_MODULUS - 1, true]]);
});

test("R4: a marker never shown again is a wedge, and says when it was last seen, not that it froze to the end (review r12 SF6)", () => {
  const frames = [...r12Before, ...every(1000, 3500, 100).map((t) => fr(t, 500)), ...every(3600, 20000, 100).map((t) => fr(t, null))];
  const st = paneStalls(synthSeg(frames), r12Gens([0]), winTo(20000), V0);
  assert.equal(st.length, 1);
  assert.deepEqual([st[0].recovered, st[0].lastShownMs, st[0].framelessMs, st[0].ms], [false, 3500, 0, 2500]);
  assert.match(describeStall(st[0]), /last shown at \+3500 ms\) .*, and did not advance in any later frame \(a wedge on what was seen: the still marker alone meets the bar\)$/);
  assert.deepEqual([st[0].basis, st[0].voids], ["seen", true]);
});

test("R5/R6/R7: several sessions, a single frame, an excluded span at the run's trailing edge (review r12)", () => {
  const multi = every(0, 5000, 100).map((t, i) => ({ source: "x11", t_ns: String(at(t)), markers: [{ kind: "output", session: 0, seq: i }, { kind: "output", session: 2, seq: 77 }] }));
  assert.deepEqual(paneStalls(synthSeg(multi), r12Gens([0, 1, 2]), winTo(5000), new Set([0, 1, 2])).map((x) => [x.session, x.recovered]), [[2, false]], "session 1 never shown gives no stall, session 0 none");
  // One frame and a window ending 0.5 s later: too short. The same frame and nothing for 4 s: still, by the frameless rule.
  assert.deepEqual(paneStalls(synthSeg([fr(1000, 5)]), r12Gens([0]), winTo(1500), V0), []);
  const one = paneStalls(synthSeg([fr(1000, 5)]), r12Gens([0]), winTo(5000), V0);
  assert.deepEqual(one.map((x) => [x.recovered, x.ms, x.framelessMs]), [[false, 4000, 4000]]);
  // [2950, 3000) ends at the last frame: the frame at 3.0 s is kept, the gap before it is cut.
  const trail = [...r12Before, ...every(1000, 3000, 100).map((t) => fr(t, 500))];
  assert.deepEqual(paneStalls(synthSeg(trail, [ivx(2950, 3000, "occluded")]), r12Gens([0]), winTo(3000), V0), [], "seen 1.9 s");
  assert.equal(paneStalls(synthSeg(trail), r12Gens([0]), winTo(3000), V0).length, 1, "2.0 s without the span");
});

test("a pane frozen when the screen stops sending frames is a wedge: the frameless tail is still time, outside excluded spans (review r12 MF1)", () => {
  const frames = [...r12Before, ...every(1000, 1500, 100).map((t) => fr(t, 500))];
  const st = paneStalls(synthSeg(frames), r12Gens([0]), winTo(4000), V0);
  assert.deepEqual(st.map((x) => [x.recovered, x.ms, x.framelessMs, x.lastShownMs]), [[false, 3000, 2500, 1500]]);
  assert.equal(st[0].emitted, 50 + 250, "emissions in (1.0, 1.5] and (1.5, 4.0], one per 10 ms");
  // An excluded span inside the tail is not counted.
  assert.deepEqual(paneStalls(synthSeg(frames, [ivx(2000, 3500, "occluded")]), r12Gens([0]), winTo(4000), V0), [], "0.5 s + 1.0 s");
  // A later frame of any kind ends the tail: the screen did send something.
  assert.deepEqual(paneStalls(synthSeg([...frames, fr(3900, null)]), r12Gens([0]), winTo(4000), V0), []);
  // A soak whose screen froze at 0.6 s and sent nothing after: its marker KPIs are voided.
  const f = focusFixture();
  const records = stallSegment(f.soak.records);
  const t0 = BigInt(records.find((r) => r.type === "start").t0_ns) + 5n * MS;
  const cut = records.filter((r) => r.type !== "frame" || BigInt(r.t_ns) <= t0 + 600n * MS);
  const r = computeRowKpis({ ...f, soak: { records: cut } });
  assert.deepEqual(r.stalls.map((x) => x.recovered), [false], JSON.stringify(r.stalls));
  assert.ok(r.stalls[0].framelessMs > SOAK_MS - 1000);
  for (const id of STALL_VOIDED_KPIS) assert.notEqual(r.kpis[id].state, "measured", id);
});

test("excluded spans are half-open: a frame at a span's start is dropped, one at its end is kept (review r12 nit)", () => {
  const frames = [fr(1000, 1), fr(2000, 2), fr(2500, 3)];
  const cov = stallCoverage(synthSeg(frames, [ivx(1000, 2000, "occluded")]), winTo(3000), [0]);
  assert.deepEqual(cov.get(0), { frames: 2, seqs: 2, firstMs: 2000, lastMs: 2500, finalSeq: 3, finalFromMs: 2500, finalHeldMs: 0 });
  assert.deepEqual(excludedSpans({ intervals: [ivx(1000, 1000, "occluded")] }), [], "an empty span excludes nothing");
});

test("a frameless tail counts only the markers emitted outside excluded spans (review r12 MF1)", () => {
  const frames = [...r12Before, ...every(1000, 1500, 100).map((t) => fr(t, 500))];
  const span = [ivx(2000, 5000, "occluded")];
  // Seen: 0.5 s of frames + (1.5-2.0 s and 5.0-6.0 s) of tail = 2.0 s.
  const emitsAt = (outside) => gensAt([...every(1100, 1500, 100), ...outside, ...every(2100, 4900, 100)]);
  const few = emitsAt([...every(1600, 1900, 100), ...every(5100, 5600, 100)]); // 5 + 4 + 6 = 15 outside the span, 29 inside
  assert.deepEqual(paneStalls(synthSeg(frames, span), few, winTo(6000), V0), [], "15 seen emissions are under 20");
  // 5 + 4 + 11 outside the span (5.05-5.95 s and one at the window's end), 29 inside: a stall.
  const enough = emitsAt([...every(1600, 1900, 100), ...every(5050, 5950, 100), 6000]);
  const st = paneStalls(synthSeg(frames, span), enough, winTo(6000), V0);
  assert.deepEqual(st.map((x) => [x.ms, x.recovered, x.emitted]), [[2000, false, 5 + 4 + 11]]);
});

// --- review r13 ---------------------------------------------------------------

/** A focus soak whose primary frames stop `tailMs` before the soak's end (a whole-screen freeze at the end). */
function endFreeze(tailMs) {
  const f = focusFixture();
  const t0 = BigInt(f.soak.records.find((r) => r.type === "start").t0_ns) + 5n * MS;
  const cut = t0 + BigInt(SOAK_MS - tailMs) * MS;
  return { ...f, soak: { records: f.soak.records.filter((r) => r.type !== "frame" || BigInt(r.t_ns) <= cut) } };
}
/** Session 0's generator at one output marker per 100 ms over [0, ms]. */
const gens100 = (ms) => new Map([[0, { markers: every(0, ms, 100).map((t, i) => ({ kind: "output", seq: i, tEmit: at(t) })) }]]);

test("MF1: a slow host (a frame every 2.5 or 3 s, marker advancing) whose window ends between frames is not a wedge (review r13 MF1)", () => {
  for (const [period, end] of [[2500, 62_400], [3000, 62_900]]) {
    const frames = every(0, end - (end % period), period).map((t) => fr(t, Math.floor(t / 100)));
    const st = paneStalls(synthSeg(frames), gens100(end), winTo(end), V0);
    assert.equal(st.length, 1, `${period}: the tail is reported`);
    assert.deepEqual([st[0].recovered, st[0].basis, st[0].voids], [false, "tail", false], JSON.stringify(st[0]));
    assert.equal(st[0].tailBarMs, Math.max(STALL_TAIL_MIN_MS, 3 * period), "the bar follows the host's own frame gap");
    assert.match(describeStall(st[0]), /\(tail-dependent, not a wedge: the seen part is under the bar, and the frameless tail of \d+ ms with \d+ marker\(s\) emitted in it does not meet the \d+ ms \/ 20-marker tail bar\)$/);
  }
});

test("MF1: a 2.4 s whole-screen freeze at the soak's end keeps the marker KPIs measured, with a note (review r13 MF1)", () => {
  const r = computeRowKpis(endFreeze(2400));
  assert.equal(r.stalls.length, 1, JSON.stringify(r.stalls));
  assert.deepEqual([r.stalls[0].recovered, r.stalls[0].basis, r.stalls[0].voids], [false, "tail", false]);
  for (const id of STALL_VOIDED_KPIS) assert.notEqual(r.kpis[id].reason, "pane_stalled", id);
  assert.equal(r.kpis.output_latency_ms.state, "measured");
  assert.ok(
    (r.kpis.output_latency_ms.notes ?? []).some((n) => /^pane still at the window's end, tail-dependent \(the marker KPIs stay measured\): session 0 .*tail-dependent, not a wedge/.test(n)),
    JSON.stringify(r.kpis.output_latency_ms.notes),
  );
});

test("MF1: a 15 s freeze at the soak's end after a dense frame history voids the marker KPIs, as tail-dependent (review r13 MF1)", () => {
  const r = computeRowKpis(endFreeze(15_000));
  assert.equal(r.stalls.length, 1, JSON.stringify(r.stalls));
  assert.deepEqual([r.stalls[0].recovered, r.stalls[0].basis, r.stalls[0].voids, r.stalls[0].tailBarMs], [false, "tail", true, STALL_TAIL_MIN_MS]);
  for (const id of STALL_VOIDED_KPIS) assertUnknown(r.kpis[id], "pane_stalled");
  assert.match(r.kpis.output_latency_ms.detail, /\(a tail-dependent wedge: the frameless tail of \d+ ms, with \d+ marker\(s\) emitted in it, meets the 10000 ms tail bar\)$/);
});

test("MF1: the tail bar is max(10 s, 3 x the longest earlier frame gap), and needs 20 markers in the tail (review r13 MF1)", () => {
  // A frame every 4 s (bar 12 s), marker advancing; the last at 40 s.
  const frames = every(0, 40_000, 4000).map((t) => fr(t, t / 100));
  assert.deepEqual(paneStalls(synthSeg(frames), gens100(60_000), winTo(51_900), V0).map((x) => x.voids), [false], "11.9 s < 12 s");
  assert.deepEqual(paneStalls(synthSeg(frames), gens100(60_000), winTo(52_000), V0).map((x) => x.voids), [true], "12 s");
  // Dense frames, still at seq 90 for the last 1 s (10 markers seen), a 10 s tail:
  // the run qualifies either way; it voids at 20 markers in the tail, not at 19.
  const dense = [...every(0, 9000, 100).map((t) => fr(t, t / 100)), ...every(9100, 10_000, 100).map((t) => fr(t, 90))];
  const tailEmits = (k) => new Map([[0, { markers: [...every(0, 10_000, 100), ...every(10_500, 10_500 + (k - 1) * 100, 100)].map((t, i) => ({ kind: "output", seq: i, tEmit: at(t) })) }]]);
  assert.deepEqual(paneStalls(synthSeg(dense), tailEmits(20), winTo(20_000), V0).map((x) => [x.voids, x.tailEmitted]), [[true, 20]]);
  assert.deepEqual(paneStalls(synthSeg(dense), tailEmits(19), winTo(20_000), V0).map((x) => [x.voids, x.tailEmitted]), [[false, 19]]);
  assert.deepEqual(paneStalls(synthSeg(dense), tailEmits(20), winTo(19_999), V0).map((x) => x.voids), [false], "9.999 s");
});

test("SF2(i): a later frame of any kind ends the tail, even one inside an excluded span (review r13 SF2)", () => {
  const frames = [...r12Before, ...every(1000, 1500, 100).map((t) => fr(t, 500)), fr(3000, null)];
  const span = [ivx(2500, 3500, "occluded")];
  assert.deepEqual(paneStalls(synthSeg(frames, span), r12Gens([0]), winTo(20_000), V0), [], "the screen sent a frame at 3 s");
  assert.deepEqual(paneStalls(synthSeg(frames.slice(0, -1), span), r12Gens([0]), winTo(20_000), V0).map((x) => x.voids), [true], "without it, a wedge");
});

test("SF2(ii): an equal sequence inside an operator_activity interval after a still run is not recovery (review r13 SF2)", () => {
  const frames = [...r12Before, ...every(1000, 3500, 100).map((t) => fr(t, 500)), ...every(3600, 5000, 100).map((t) => fr(t, 500))];
  const st = paneStalls(synthSeg(frames, [ivx(3550, 5001, "operator_activity")]), r12Gens([0]), winTo(5000), V0);
  assert.deepEqual(st.map((x) => [x.seq, x.recovered, x.basis, x.voids]), [[500, false, "seen", true]]);
});

test("nit: the hidden gap ends a piece only when OVER 900 ms (review r13 nit)", () => {
  const dense = gensAt(every(0, 20_000, 10));
  const gapped = (gapMs) => [
    ...r12Before,
    ...every(1000, 1500, 100).map((t) => fr(t, 500)),
    ...every(1600, 1600 + gapMs - 100, 100).map((t) => fr(t, null)),
    ...every(1600 + gapMs, 3000 + gapMs - 900, 100).map((t) => fr(t, 500)),
    fr(3000 + gapMs - 800, 501),
  ];
  // Hidden exactly 900 ms (1.6 -> 2.5 s): one piece, 1.0-3.0 s, 2000 ms.
  assert.deepEqual(paneStallsAt(synthSeg(gapped(900)), dense, WIN, V0).map((x) => x.ms), [2000]);
  // Hidden 1000 ms: two pieces, 1.0-1.5 and 2.6-3.1 s, 1000 ms seen.
  assert.deepEqual(paneStallsAt(synthSeg(gapped(1000)), dense, WIN, V0), []);
  assert.equal(STALL_HIDDEN_GAP_MS, 900);
});

test("nit: frozen-blocked — a pane at one sequence whose generator wrote < 20 markers is no stall, and covers one seq (review r13 nit)", () => {
  const frames = every(0, 5000, 50).map((t) => fr(t, 641));
  const gens = new Map([[0, { markers: Array.from({ length: 15 }, (_, i) => ({ kind: "output", seq: 642 + i, tEmit: at(i * 100) })) }]]);
  assert.deepEqual(paneStalls(synthSeg(frames), gens, winTo(5000), V0), []);
  assert.deepEqual(stallCoverage(synthSeg(frames), winTo(5000), [0]).get(0), { frames: 101, seqs: 1, firstMs: 0, lastMs: 5000, finalSeq: 641, finalFromMs: 0, finalHeldMs: 5000 });
});

test("MF1(b/c): the seen basis needs BOTH bars on the seen part alone, 2000 ms inclusive (review r13 MF1)", () => {
  // Seen exactly 2.0 s (1.0-3.0 s) with 20 seen emissions, then a 1 s tail: a seen wedge.
  const frames = [...r12Before, ...every(1000, 3000, 100).map((t) => fr(t, 500))];
  const seen20 = gensAt([...every(1100, 3000, 100), ...every(3100, 4000, 100)]);
  assert.deepEqual(paneStalls(synthSeg(frames), seen20, winTo(4000), V0).map((x) => [x.ms, x.basis, x.voids]), [[3000, "seen", true]]);
  // Seen 2.0 s with 15 seen emissions plus 10 in a 1 s tail: tail-dependent, no void.
  const seen15 = gensAt([...every(1600, 3000, 100), ...every(3100, 4000, 100)]);
  assert.deepEqual(paneStalls(synthSeg(frames), seen15, winTo(4000), V0).map((x) => [x.emitted, x.tailEmitted, x.basis, x.voids]), [[25, 10, "tail", false]]);
});

test("MF1: per session the wedge reported is a voiding one first, then the longest (review r13 MF1)", () => {
  // seq 500 seen 1.0-3.0 s (a seen wedge), then seq 400 (backwards: no recovery) seen 3.1-4.6 s + a 1.4 s tail.
  const frames = [...r12Before, ...every(1000, 3000, 100).map((t) => fr(t, 500)), ...every(3100, 4600, 100).map((t) => fr(t, 400))];
  const st = paneStalls(synthSeg(frames), r12Gens([0]), winTo(6000), V0);
  assert.deepEqual(st.map((x) => [x.seq, x.ms, x.voids]), [[500, 2000, true]], "the 2.9 s tail-dependent one is longer, but does not void");
  // Two seen wedges: the longer is reported.
  const two = [...r12Before, ...every(1000, 4000, 100).map((t) => fr(t, 500)), ...every(4100, 6300, 100).map((t) => fr(t, 400))];
  assert.deepEqual(paneStalls(synthSeg(two), r12Gens([0]), winTo(6300), V0).map((x) => [x.seq, x.ms]), [[500, 3000]]);
});

test("stallCoverage counts only frames showing the session's OUTPUT marker (review r13)", () => {
  const frames = [fr(0, 1), fr(100, 2), { source: "x11", t_ns: String(at(200)), markers: [{ kind: "echo", session: 0, seq: 9 }] }];
  assert.deepEqual(stallCoverage(synthSeg(frames), winTo(300), [0]).get(0), { frames: 2, seqs: 2, firstMs: 0, lastMs: 100, finalSeq: 2, finalFromMs: 100, finalHeldMs: 0 });
});

// --- review r14 ---------------------------------------------------------------

/** A synthetic segment with a start record (t0 = T0 at wall 2026-10-16T00:00:00.000Z) and these incidents. */
function lostSeg(frames, incidents, intervals = []) {
  return { ...synthSeg(frames, intervals), start: { type: "start", t0_ns: String(T0), at: "2026-10-16T00:00:00.000Z" }, incidents };
}
/** A primary capture_lost at `ms` (wall `at` only, unless `exact`): "ended" by default, else "lost" (one failed read). */
const lostAt = (ms, source = "x11", { kind = "ended", exact = false } = {}) => ({
  type: "incident",
  kind: "capture_lost",
  at: new Date(Date.parse("2026-10-16T00:00:00.000Z") + ms).toISOString(),
  detail: kind === "ended" ? `${source} capture ended: the window is gone` : `${source} capture lost (re-establishing): read failed`,
  stops_run: kind === "ended",
  data: { source },
  ...(exact ? { t_ns: String(at(ms)) } : {}),
});
/** The same segment with a WGC primary. */
const wgcSeg = (frames, incidents) => ({ ...lostSeg(frames.map((f) => ({ ...f, source: "wgc" })), incidents), captures: [{ type: "capture", source: "wgc", state: "started" }], summary: { primary: "wgc" } });

test("r14 A: an absent window-state inspector (every interval no_samples) does not hide a 15 s end freeze (review r14 MF1)", () => {
  const frames = every(0, 45_000, 100).map((t) => fr(t, t / 100));
  const noInsp = every(0, 59_500, 500).map((t) => ({ type: "interval", t_start_ns: String(at(t)), t_end_ns: String(at(t + 500)), samples: 0, valid: false, excluded: ["no_samples"], exclusion_class: "harness_error" }));
  assert.deepEqual(paneStalls(synthSeg(frames, noInsp), gens100(60_000), winTo(60_000), V0).map((x) => [x.framelessMs, x.basis, x.voids]), [[15_000, "tail", true]]);
});

test("r14 B: a primary capture lost at 45 s is not a frozen screen: no void, and the row says so (review r14 MF1)", () => {
  const frames = every(0, 45_000, 100).map((t) => fr(t, t / 100));
  assert.deepEqual(paneStalls(lostSeg(frames, [lostAt(45_000)]), gens100(60_000), winTo(60_000), V0), []);
  // Lost at 50 s: the 5 s before it still count, and do not void.
  assert.deepEqual(paneStalls(lostSeg(frames, [lostAt(50_000)]), gens100(60_000), winTo(60_000), V0).map((x) => [x.framelessMs, x.voids]), [[5000, false]]);
  // A loss that cannot be placed in time (no start record) ends every tail; a non-primary source's loss is ignored.
  assert.deepEqual(paneStalls({ ...synthSeg(frames), incidents: [lostAt(50_000)] }, gens100(60_000), winTo(60_000), V0), []);
  assert.equal(paneStalls(lostSeg(frames, [lostAt(45_000, "dda")]), gens100(60_000), winTo(60_000), V0)[0]?.voids, true);
  assert.deepEqual(captureLosses({ ...synthSeg(frames), incidents: [lostAt(50_000)] }).map((x) => x.t), [null]);
  // The row: KPIs stay measured, with a note naming the loss.
  const f0 = focusFixture();
  const f = { ...f0, soak: { records: f0.soak.records.map((r) => (r.type === "start" ? { ...r, at: "2026-10-16T00:00:00.000Z" } : r)) } };
  const start = f.soak.records.find((r) => r.type === "start");
  const t0 = BigInt(start.t0_ns) + 5n * MS;
  const cutT = t0 + BigInt(SOAK_MS - 15_000) * MS;
  const wallCut = Date.parse(start.at) + Number((cutT - BigInt(start.t0_ns)) / MS);
  const records = [
    ...f.soak.records.filter((r) => r.type !== "frame" || BigInt(r.t_ns) <= cutT),
    { type: "incident", kind: "capture_lost", at: new Date(wallCut).toISOString(), detail: "x11 capture ended: gone", stops_run: true, data: { source: captureInfo(indexProbeSegment(f.soak.records)).primary } },
  ];
  const r = computeRowKpis({ ...f, soak: { records } });
  assert.deepEqual(r.stalls, [], JSON.stringify(r.stalls));
  assert.equal(r.kpis.output_latency_ms.state, "measured");
  assert.ok((r.kpis.output_latency_ms.notes ?? []).some((n) => /^1 primary capture loss\(es\) \(\w+; first \+(\d+) ms, last \+\1 ms\): 1 x capture ended: no frameless time after it counts at \+\1 ms; (no frameless tail|frameless tail read \+\d+ ms\.\.\+\1 ms) \(e\.g\. "x11 capture ended: gone"\)$/.test(n)), JSON.stringify(r.kpis.output_latency_ms.notes));
  // Without the incident the same row voids.
  const bare = computeRowKpis({ ...f, soak: { records: records.filter((x) => x.type !== "incident") } });
  assertUnknown(bare.kpis.output_latency_ms, "pane_stalled");
});

test("r15 S1-S4: only an END cuts the tail; one failed read restarts it at the latest such loss (review r15 SF3)", () => {
  const frames = every(0, 20_000, 100).map((t) => fr(t, t / 100));
  const run = (inc) => paneStalls(lostSeg(frames, inc), gens100(60_000), winTo(60_000), V0).map((x) => [x.framelessMs, x.voids]);
  assert.deepEqual(run([]), [[40_000, true]]);
  // S1: one failed read at 20.5 s, then 39.5 s of capture with no frame: voids, the tail counted from the loss.
  assert.deepEqual(run([lostAt(20_500, "x11", { kind: "lost" })]), [[39_500, true]]);
  // The LATEST failed read restarts it.
  assert.deepEqual(run([lostAt(20_500, "x11", { kind: "lost" }), lostAt(30_000, "x11", { kind: "lost" })]), [[30_000, true]]);
  // S2: a failed read at 19.5 s with frames after it: the tail is the whole 40 s.
  assert.deepEqual(run([lostAt(19_500, "x11", { kind: "lost" })]), [[40_000, true]]);
  // S3: capture ENDED, its wall time mapped 1 s early: no frame follows an end, so no tail at all.
  assert.deepEqual(run([lostAt(19_000)]), []);
  // S4: a failed read stamped late (20.1 s) still leaves a voiding tail.
  assert.deepEqual(run([lostAt(20_100, "x11", { kind: "lost" })]), [[39_900, true]]);
  // An end after a failed read: the tail runs from the read to the end.
  assert.deepEqual(run([lostAt(21_000, "x11", { kind: "lost" }), lostAt(33_000)]), [[12_000, true]]);
});

test("r15 SF3: a WGC loss with no later primary frame is read like an end; the slack applies only to a mapped time (review r15 SF3)", () => {
  const frames = every(0, 20_000, 100).map((t) => fr(t, t / 100));
  const run = (inc) => paneStalls(wgcSeg(frames, inc), gens100(60_000), winTo(60_000), V0).map((x) => [x.framelessMs, x.voids]);
  assert.deepEqual(run([lostAt(20_500, "wgc", { kind: "lost" })]), [], "no frame after it");
  // Mapped 1.0 s before the last frame: a frame lies after it, but within the slack, so it may not be later.
  assert.deepEqual(run([lostAt(19_000, "wgc", { kind: "lost" })]), [], "frame at +1000 ms: within the slack");
  assert.deepEqual(run([lostAt(18_999, "wgc", { kind: "lost" })]), [[40_000, true]], "frame at +1001 ms: re-established");
  // With the probe-clock t_ns there is no slack: one frame after it is enough.
  assert.deepEqual(run([lostAt(19_900, "wgc", { kind: "lost", exact: true })]), [[40_000, true]]);
  assert.deepEqual(run([lostAt(20_000, "wgc", { kind: "lost", exact: true })]), [], "at the last frame: nothing after it");
  const xs = captureLosses(wgcSeg(frames, [lostAt(19_900, "wgc", { kind: "lost", exact: true })]), frames.map((f) => ({ t: BigInt(f.t_ns) })));
  assert.deepEqual(xs.map((x) => [x.exact, x.ended, x.cuts, x.later]), [[true, false, false, true]]);
});

test("r15 SF4: an incident's t_ns is preferred to its wall time, which is only a fallback (review r15 SF4)", () => {
  const frames = every(0, 20_000, 100).map((t) => fr(t, t / 100));
  // Wall says 50 s, the probe clock says 30 s: the end cuts at 30 s.
  const inc = { ...lostAt(50_000), t_ns: String(at(30_000)) };
  assert.deepEqual(paneStalls(lostSeg(frames, [inc]), gens100(60_000), winTo(60_000), V0).map((x) => x.framelessMs), [10_000]);
  // A wall time before the start record cannot be placed (wall >= wall0); exactly at it can.
  const before = { ...lostAt(0), at: "2026-10-15T23:59:59.999Z" };
  assert.deepEqual(captureLosses(lostSeg(frames, [before, lostAt(0)])).map((x) => (x.t === null ? null : Number((x.t - T0) / MS))), [null, 0]);
});
test("r14 C: the tail bar's frame gap is measured only where no excluded span or capture loss lies between frames (review r14 SF4)", () => {
  // Frames 0-10 s and 30-45 s, minimized 10-30 s, then a 15 s end freeze.
  const frames = [...every(0, 10_000, 100), ...every(30_000, 45_000, 100)].map((t) => fr(t, t / 100));
  const occ = [{ type: "interval", t_start_ns: String(at(10_000)), t_end_ns: String(at(30_000)), samples: 40, valid: false, excluded: ["minimized"], exclusion_class: "occluded" }];
  assert.deepEqual(paneStalls(synthSeg(frames, occ), gens100(60_000), winTo(60_000), V0).map((x) => [x.tailBarMs, x.voids]), [[10_000, true]]);
  assert.deepEqual(paneStalls(synthSeg(frames), gens100(60_000), winTo(60_000), V0).map((x) => [x.tailBarMs, x.voids]), [[60_000, false]], "a real 20 s gap does raise the bar");
  // A failed read at 15 s: the 20 s gap is the probe's, not the host's.
  assert.deepEqual(paneStalls(lostSeg(frames, [lostAt(15_000, "x11", { kind: "lost" })]), gens100(60_000), winTo(60_000), V0).map((x) => [x.tailBarMs, x.voids]), [[10_000, true]]);
  // Exact time: a loss AT the later frame is between them (t <= q); one at the earlier frame is not (t > p).
  const gap = (ms) => paneStalls(lostSeg(frames, [lostAt(ms, "x11", { kind: "lost", exact: true })]), gens100(60_000), winTo(60_000), V0).map((x) => x.tailBarMs);
  assert.deepEqual(gap(30_000), [10_000]);
  assert.deepEqual(gap(10_000), [60_000]);
  // Mapped time: within the slack of either frame is between them.
  const mgap = (ms) => paneStalls(lostSeg(frames, [lostAt(ms, "x11", { kind: "lost" })]), gens100(60_000), winTo(60_000), V0).map((x) => x.tailBarMs);
  assert.deepEqual(mgap(31_000), [10_000]);
  assert.deepEqual(mgap(31_001), [60_000]);
  assert.deepEqual(mgap(9_001), [10_000]);
  assert.deepEqual(mgap(9_000), [60_000]);
});

test("r14 B: markers emitted after a capture loss do not count toward the tail (review r14 MF1)", () => {
  const frames = every(0, 45_000, 100).map((t) => fr(t, t / 100));
  // The generator wrote nothing between the last frame and the loss at 55 s, then 50 markers.
  const quiet = gensAt([...every(0, 45_000, 100), ...every(55_100, 60_000, 100)]);
  assert.deepEqual(paneStalls(lostSeg(frames, [lostAt(55_000)]), quiet, winTo(60_000), V0), []);
});

test("r15 S5: only an RFC 3339 `at` is placed; a local time or a number is not (review r15)", () => {
  const frames = [fr(0, 1)];
  const placed = (a) => captureLosses(lostSeg(frames, [{ ...lostAt(0), at: a }])).map((x) => (x.t === null ? null : Number((x.t - T0) / MS)))[0];
  assert.equal(placed("2026-10-16T00:00:20.500Z"), 20_500);
  assert.equal(placed("2026-10-16T00:00:20.500123Z"), 20_500);
  assert.equal(placed("2026-10-16T00:00:20.5+00:00"), 20_500);
  assert.equal(placed(1760572820500), null);
  assert.equal(placed("2026-10-16 00:00:20.500"), null);
});

test("r15 nit 6: a backwards seq step is not an advance in the final still run (review r15 nit 6)", () => {
  const back = [...every(0, 2900, 100).map((t) => fr(t, 50)), ...every(3100, 5000, 100).map((t) => fr(t, 49))];
  const c = stallCoverage(synthSeg(back), winTo(5000), [0]).get(0);
  assert.deepEqual([c.finalSeq, c.finalFromMs, c.finalHeldMs], [49, 0, 5000]);
  const fwd = [...every(0, 2900, 100).map((t) => fr(t, 50)), ...every(3100, 5000, 100).map((t) => fr(t, 51))];
  assert.deepEqual(stallCoverage(synthSeg(fwd), winTo(5000), [0]).get(0).finalFromMs, 3100);
  // The held time leaves out excluded spans.
  const c2 = stallCoverage(synthSeg(back, [ivx(1000, 2000, "occluded")]), winTo(5000), [0]).get(0);
  assert.equal(c2.finalHeldMs, 4000);
});

test("r15 S5: a Date.parse-able `at` that is not RFC 3339 is not placed (review r15)", () => {
  const xs = captureLosses(lostSeg([fr(0, 1)], [{ ...lostAt(0), at: "Fri, 16 Oct 2026 00:00:20 GMT" }]));
  assert.deepEqual(xs.map((x) => x.t), [null]);
});

test("r15 SF3: tail markers are counted from the restart, not from the run's last frame (review r15 SF3)", () => {
  const frames = every(0, 20_000, 100).map((t) => fr(t, t / 100));
  // 50 markers between the last frame and a failed read at 25 s, 15 after it.
  const gens = gensAt([...every(0, 20_000, 100), ...every(20_100, 25_000, 100), ...every(26_000, 54_000, 2000)]);
  assert.deepEqual(paneStalls(lostSeg(frames, [lostAt(25_000, "x11", { kind: "lost", exact: true })]), gens, winTo(60_000), V0), []);
});

// --- review r16 ---------------------------------------------------------------

const f20 = every(0, 20_000, 100).map((t) => fr(t, t / 100));
const last20 = at(20_000);
const effectsOf = (incs, frames = f20) => {
  const all = frames.map((f) => ({ t: BigInt(f.t_ns) }));
  const xs = captureLosses(lostSeg(frames, incs), all);
  return lossEffects(xs, all.at(-1).t, winTo(60_000));
};
const L = (ms, o = {}) => lostAt(ms, "x11", { kind: "lost", ...o });

test("r16 S1 R1: only the failed read that set the tail's start says so; an earlier one is superseded (review r16 S1)", () => {
  const e = effectsOf([L(25_000, { exact: true }), L(40_000, { exact: true })]);
  assert.deepEqual(e.effects, ["superseded", "restart"]);
  assert.equal(e.tailStart, at(40_000));
  const line = summarizeLosses(captureLosses(lostSeg(f20, [L(25_000, { exact: true }), L(40_000, { exact: true })]), f20.map((f) => ({ t: BigInt(f.t_ns) }))), last20, winTo(60_000));
  assert.match(line, /^2 primary capture loss\(es\) \(x11; first \+25000 ms, last \+40000 ms\): 1 x a retry, no later frame read: the tail is counted from it at \+40000 ms; 1 x a retry, no later frame read; superseded by a later failed read at \+25000 ms; frameless tail read \+40000 ms\.\.\+60000 ms /);
});

test("r16 S1 R2: a mapped failed read within the slack before the last frame reports the real tail start (review r16 S1)", () => {
  const e = effectsOf([L(19_500)]);
  assert.deepEqual([e.effects, e.tailStart], [["at_last_frame"], last20]);
  const xs = captureLosses(lostSeg(f20, [L(19_500)]), f20.map((f) => ({ t: BigInt(f.t_ns) })));
  assert.match(summarizeLosses(xs, last20, winTo(60_000)), /1 x a retry around the last frame, no later frame read: the tail is counted from the last frame at \+19500 ms; frameless tail read \+20000 ms\.\.\+60000 ms/);
  assert.ok(!/re-established/.test(summarizeLosses(xs, last20, winTo(60_000))));
});

test("r16 S1: every effect a loss can have (review r16 S1)", () => {
  assert.deepEqual(effectsOf([L(5000, { exact: true })]).effects, ["later"]);
  assert.deepEqual(effectsOf([lostAt(30_000, "x11", { exact: true })]).effects, ["ended"]);
  assert.deepEqual(effectsOf([lostAt(30_000, "x11", { exact: true }), lostAt(40_000, "x11", { exact: true })]).effects, ["ended", "cut_superseded"]);
  assert.deepEqual(effectsOf([{ ...lostAt(30_000), at: "nope" }]).effects, ["unplaced_cut"]);
  assert.deepEqual(effectsOf([{ ...L(30_000), at: "nope" }]).effects, ["unplaced_retry"]);
  // R3: an end at 30 s, then a failed read placed after it: no effect, and the tail is 20-30 s.
  const r3 = effectsOf([lostAt(30_000, "x11", { exact: true }), L(45_000, { exact: true })]);
  assert.deepEqual([r3.effects, r3.tailStart, r3.tailEnd], [["ended", "after_end"], last20, at(30_000)]);
  assert.deepEqual(paneStalls(lostSeg(f20, [lostAt(30_000, "x11", { exact: true }), L(45_000, { exact: true })]), gens100(60_000), winTo(60_000), V0).map((x) => x.framelessMs), [10_000]);
});

test("r16 S3: a failed read exactly at the window's end, or at an end's cut, leaves no tail (review r16 S3)", () => {
  const run = (incs) => paneStalls(lostSeg(f20, incs), gens100(60_000), winTo(60_000), V0);
  assert.deepEqual(run([L(60_000, { exact: true })]), [], "at win.t1: it may have consumed the repaint");
  const e = effectsOf([L(59_999, { exact: true })]);
  assert.deepEqual([e.tailStart, e.tailEnd], [at(59_999), at(60_000)]);
  assert.deepEqual(run([lostAt(50_000, "x11", { exact: true }), L(50_000, { exact: true })]), [], "at the cut");
  assert.deepEqual(effectsOf([L(60_000, { exact: true })]).effects, ["at_tail_end"]);
});

test("r16 S3: only a WGC primary cuts on a failed read with no later frame; X11 and other sources do not (review r16 S3)", () => {
  const xs = (primary) => {
    const frames = f20.map((f) => ({ ...f, source: primary }));
    const seg = { ...lostSeg(frames, [lostAt(30_000, primary, { kind: "lost", exact: true })]), captures: [{ type: "capture", source: primary, state: "started" }], summary: { primary } };
    return captureLosses(seg, frames.map((f) => ({ t: BigInt(f.t_ns) }))).map((x) => x.cuts);
  };
  assert.deepEqual(xs("wgc"), [true]);
  assert.deepEqual(xs("x11"), [false]);
  assert.deepEqual(xs("synthetic"), [false]);
});

test("r16 N3: an end is read from stops_run, which the engine sets exactly on the primary's end (review r16 N3)", () => {
  const xs = captureLosses(lostSeg(f20, [{ ...lostAt(30_000), stops_run: false }, { ...L(31_000), stops_run: true }]));
  assert.deepEqual(xs.map((x) => [x.ended, x.cuts]), [[false, false], [true, true]]);
});

test("r16 S2: losses are one bounded summary on the row, unplaced ones included, those outside the window not (review r16 S2, repro2)", () => {
  // 800 failed reads, one every 50 ms after the last frame: one note, of bounded length.
  const incs = every(20_050, 60_000, 50).map((t) => L(t, { exact: true }));
  const all = f20.map((f) => ({ t: BigInt(f.t_ns) }));
  const xs = captureLosses(lostSeg(f20, incs), all);
  assert.equal(xs.length, 800);
  const line = summarizeLosses(xs, last20, winTo(60_000));
  assert.ok(line.length < 600, `${line.length}: ${line}`);
  assert.match(line, /^800 primary capture loss\(es\) \(x11; first \+20050 ms, last \+60000 ms\): 1 x a failed read at the tail's end: no frameless tail at \+60000 ms; 799 x a retry, no later frame read; superseded by a later failed read \+20050 ms\.\.\+59950 ms; no frameless tail /);
  assert.deepEqual(lossGroups(xs, last20, winTo(60_000)).map((g) => [g.effect, g.count, g.firstMs, g.lastMs]), [["at_tail_end", 1, 60_000, 60_000], ["superseded", 799, 20_050, 59_950]]);
  // A long detail is cut.
  const long = captureLosses(lostSeg(f20, [{ ...L(30_000, { exact: true }), detail: `x11 capture lost (re-establishing): ${"x".repeat(500)}` }]), all);
  assert.ok(summarizeLosses(long, last20, winTo(60_000)).length < LOSS_DETAIL_MAX + 400);
  // On the row: one note; a loss before the window is left out, an unplaced one is kept.
  const f0 = focusFixture();
  const primary = captureInfo(indexProbeSegment(f0.soak.records)).primary;
  const start = BigInt(f0.soak.records.find((r) => r.type === "start").t0_ns);
  const inc = (t) => ({ type: "incident", kind: "capture_lost", at: "nope", ...(t === null ? {} : { t_ns: String(t) }), detail: `${primary} capture lost (re-establishing): x`, stops_run: false, data: { source: primary } });
  const notesFor = (incs) => (computeRowKpis({ ...f0, soak: { records: [...f0.soak.records, ...incs] } }).kpis.output_latency_ms.notes ?? []).filter((n) => /primary capture loss/.test(n));
  assert.deepEqual(notesFor([inc(start)]), [], "before the window");
  assert.match(notesFor([inc(start + 5n * MS)])[0], /^1 primary capture loss\(es\) \(\w+; first \+0 ms, last \+0 ms\)/, "at its start");
  assert.match(notesFor([inc(null)])[0], /^1 primary capture loss\(es\) \(\w+; none placed in time\): 1 x a retry whose time could not be placed: no effect/);
  assert.equal(notesFor(every(1000, 30_000, 10).map((ms) => inc(start + 5n * MS + BigInt(ms) * MS))).length, 1);
});

test("r16 S1: a failed read exactly at the last frame does not restart the tail; it is a retry around the last frame (review r16 S1)", () => {
  const e = effectsOf([L(20_000, { exact: true })]);
  assert.deepEqual([e.effects, e.restartBy, e.tailStart], [["at_last_frame"], -1, last20]);
  assert.deepEqual(effectsOf([L(20_001, { exact: true })]).effects, ["restart"]);
});

test("r17: a retry where no tail can follow, or at the tail's end, says so (review r17)", () => {
  const M = 1_000_000n;
  const win = { t0: 0n, t1: 10_000n * M };
  const R = (ms, o = {}) => ({ t: BigInt(ms) * M, exact: true, ended: false, source: "x11", cuts: false, later: false, detail: "d", ...o });
  // A: no primary frame in the window.
  assert.deepEqual(lossEffects([R(3000)], null, win).effects, ["no_tail"]);
  assert.match(summarizeLosses([R(3000)], null, win), /1 x a retry with no frameless tail to start .*: no effect at \+3000 ms \(e\.g\./);
  // B: an end at 4 s before the last frame at 5 s: the retry at 3 s has nothing to start.
  assert.deepEqual(lossEffects([R(3000), R(4000, { ended: true, cuts: true })], 5000n * M, win).effects, ["no_tail", "ended"]);
  // An unplaced WGC cut sets the tail's end at the last frame: not "at_last_frame".
  assert.deepEqual(lossEffects([R(4900, { source: "wgc" }), R(0, { t: null, source: "wgc", cuts: true })], 5000n * M, win).effects, ["no_tail", "unplaced_cut"]);
  // ... and with a tail, the same retry is at_last_frame.
  assert.deepEqual(lossEffects([R(4900)], 5000n * M, win).effects, ["at_last_frame"]);
  // C: a retry exactly at the window's end: it sets the start, and there is no tail.
  const c = lossEffects([R(10_000)], 5000n * M, win);
  assert.deepEqual([c.effects, c.tailStart === c.tailEnd], [["at_tail_end"], true]);
  assert.match(summarizeLosses([R(10_000)], 5000n * M, win), /1 x a failed read at the tail's end: no frameless tail at \+10000 ms; no frameless tail/);
  assert.deepEqual(lossEffects([R(9999)], 5000n * M, win).effects, ["restart"]);
});

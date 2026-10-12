/**
 * `compare` (head build against baseline build) and `report` (rendering, the CI
 * gate's assertions, the runner-only copy): an unknown is never folded into a
 * number, environments never mix, every unknown is rendered with its reason, and
 * nothing from a third-party row leaves the job (D7).
 *
 *   node --test scripts/termhost-bench/__tests__/compare-report.test.mjs
 */

import { test } from "node:test";
import assert from "node:assert/strict";

import { KPI_CATALOG, getKpi } from "../kpi-catalog.mjs";
import { COMPARED_KPIS, compareBuilds, compareStatistic, renderComparison, spreadOf, statesOf, valueOf } from "../compare.mjs";
import { REPORT_KPIS, cell, checkAssertions, prepareReport, renderReport } from "../report.mjs";
import { measureKpi, unknownKpi } from "../stats.mjs";
import { validateResults } from "../validate.mjs";
import { linuxXvfbResults, measuredKpis, row, spread } from "./helpers/results.mjs";

const HEAD = "a".repeat(40);
const BASE = "b".repeat(40);

/** A runner row of one build role at N = 5 focus; `latency` samples, or null for unknown. */
function runnerRow(role, commit, runIndex, latency, extra = {}) {
  return row({
    setupOverrides: { build: { commit, source: "artifact", role }, launch_mode: "subject_instance_root", pty_host_impl: "unix_pty", session_shell: "bash" },
    scenarioOverrides: { n: 5, arm: "focus", v_target: 1, v_actual: 1, run_index: runIndex, run_count: 3 },
    kpis: {
      ...measuredKpis(),
      output_latency_ms: latency ? measureKpi("output_latency_ms", latency) : unknownKpi("output_latency_ms", "capture_unavailable", { detail: "no frames" }),
      ...extra,
    },
  });
}

/** One file holding both builds (the same-VM A/B), plus a tmux row that must never be compared. */
function abDoc() {
  const doc = linuxXvfbResults();
  doc.rows = [
    runnerRow("head", HEAD, 0, spread(600, 10, 50), { hangs: measureKpi("hangs", { value: 0, n: 116 }) }),
    runnerRow("head", HEAD, 1, spread(600, 12, 60), { hangs: measureKpi("hangs", { value: 2, n: 116 }) }),
    runnerRow("head", HEAD, 2, spread(600, 11, 55), { hangs: measureKpi("hangs", { value: 1, n: 116 }) }),
    runnerRow("baseline", BASE, 0, spread(600, 20, 80)),
    runnerRow("baseline", BASE, 1, spread(600, 22, 90)),
    runnerRow("baseline", BASE, 2, null),
    doc.rows.find((r) => r.setup.id === "tmux"),
  ];
  return doc;
}

const entry = (cmp, kpi, n = 5, arm = "focus") => cmp.entries.find((e) => e.kpi === kpi && e.n === n && e.arm === arm);

// ---------------------------------------------------------------------------
// compare
// ---------------------------------------------------------------------------

test("compared KPIs are the host's own comparable ones, each on its headline statistic", () => {
  for (const id of COMPARED_KPIS) {
    const k = getKpi(id);
    assert.ok(k.comparable && k.attribution === "host", id);
  }
  assert.ok(!COMPARED_KPIS.includes("workload_cpu_pct") && !COMPARED_KPIS.includes("compositor_memory_mb"));
  assert.equal(compareStatistic(getKpi("output_latency_ms")), "p95");
  assert.equal(compareStatistic(getKpi("cpu_pct")), "mean");
  assert.equal(compareStatistic(getKpi("hangs")), "count");
  assert.equal(compareStatistic(getKpi("frame_interval_ms")), "p95");
});

test("a value-shape KPI contributes its value whatever its statistic is called; an unknown contributes nothing", () => {
  assert.equal(valueOf(measureKpi("hangs", { value: 3, n: 116 }), getKpi("hangs")), 3);
  assert.equal(valueOf(measureKpi("memory_mb", { value: 812.5, n: 120 }), getKpi("memory_mb")), 812.5);
  const lat = measureKpi("output_latency_ms", spread(600, 8, 70));
  assert.equal(valueOf(lat, getKpi("output_latency_ms")), lat.stats.p95);
  assert.equal(valueOf(unknownKpi("output_latency_ms", "occluded", { n: 3 }), getKpi("output_latency_ms")), null);
  assert.equal(valueOf(undefined, getKpi("hangs")), null);
});

test("spreads are nearest-rank medians with the run range; states count every run", () => {
  assert.equal(spreadOf([]), null);
  assert.deepEqual(spreadOf([3, 1, 2]), { median: 2, min: 1, max: 3, runs: 3 });
  assert.ok([1, 2, 3, 4].includes(spreadOf([4, 1, 3, 2]).median), "an observed value, never an average of two");
  assert.equal(statesOf([{ state: "measured" }, { state: "unknown", reason: "occluded" }, { state: "unknown", reason: "occluded" }, undefined]), "1 measured, 2 unknown(occluded), 1 missing(?)");
});

test("one file's head and baseline rows pair per N and arm; the unknown run is counted, not averaged", () => {
  const doc = abDoc();
  const cmp = compareBuilds(doc);
  assert.equal(cmp.head_commit, HEAD);
  assert.equal(cmp.baseline_commit, BASE);
  assert.equal(cmp.environment, "linux-xvfb");
  const lat = entry(cmp, "output_latency_ms");
  const p95 = (role) => doc.rows.filter((r) => r.setup.build?.role === role && r.kpis.output_latency_ms.state === "measured").map((r) => r.kpis.output_latency_ms.stats.p95);
  const head = p95("head");
  assert.equal(lat.head.spread.runs, 3);
  assert.equal(lat.head.spread.min, Math.min(...head));
  assert.equal(lat.head.spread.max, Math.max(...head));
  assert.equal(lat.head.states, "3 measured");
  assert.equal(lat.baseline.spread.runs, 2, "the unknown baseline run is not a number");
  assert.equal(lat.baseline.states, "2 measured, 1 unknown(capture_unavailable)");
  const hangs = entry(cmp, "hangs");
  assert.deepEqual(hangs.head.spread, { median: 1, min: 0, max: 2, runs: 3 });
  assert.ok(cmp.entries.every((e) => e.head.total === 3 && e.baseline.total === 3), "the tmux row is in no cell");
});

test("compare refuses mixed environments and a file with no baseline build", () => {
  const head = abDoc();
  const other = abDoc();
  other.machine.environment = "windows-hosted";
  assert.throws(() => compareBuilds(head, other), /numbers compare only within one environment \(D6\)/);
  const headOnly = abDoc();
  headOnly.rows = headOnly.rows.filter((r) => r.setup.build?.role !== "baseline");
  assert.throws(() => compareBuilds(headOnly), /run with --ab-baseline/);
});

test("two files pair the first's head (or single) rows with the second's baseline (or single) rows", () => {
  const a = abDoc();
  a.rows = a.rows.filter((r) => r.setup.build?.role === "head");
  const b = abDoc();
  b.rows = b.rows.filter((r) => r.setup.build?.role === "baseline").map((r) => ({ ...r, setup: { ...r.setup, build: { ...r.setup.build, role: "single" } } }));
  const cmp = compareBuilds(a, b);
  assert.equal(entry(cmp, "output_latency_ms").baseline.total, 3);
  assert.equal(cmp.baseline_commit, BASE);
});

test("the comparison renders every side's spread or unknown, and draws no verdict", () => {
  const doc = abDoc();
  doc.rows = doc.rows.map((r) => (r.setup.build?.role === "baseline" ? { ...r, kpis: { ...r.kpis, cpu_pct: unknownKpi("cpu_pct", "harness_error") } } : r));
  const md = renderComparison(compareBuilds(doc));
  assert.match(md, /^### runner: head a{12} vs baseline b{12} \(linux-xvfb\)$/m);
  assert.match(md, /No verdict is drawn here/);
  assert.match(md, /^\| output_latency_ms \| p95 \| 5 \| focus \| [\d.]+ ms \[[\d.]+–[\d.]+\] \| [\d.]+ ms \[[\d.]+–[\d.]+\] \| 3 measured \| 2 measured, 1 unknown\(capture_unavailable\) \|$/m);
  assert.match(md, /^\| cpu_pct \| mean \| 5 \| focus \| .+ \| unknown \| 3 measured \| 3 unknown\(harness_error\) \|$/m);
  assert.match(md, /^\| hangs \| count \| 5 \| focus \| 1\.00 \[0\.00–2\.00\] \|/m);
  const tableRows = md.split(/\r?\n/).filter((l) => l.startsWith("| ") && !l.startsWith("| KPI "));
  assert.ok(tableRows.length > 0);
  assert.ok(tableRows.every((l) => !/regression|improvement|verdict/i.test(l)), "no row carries a verdict (Phase 6)");
});

// ---------------------------------------------------------------------------
// report
// ---------------------------------------------------------------------------

test("a cell is a measured number (stats-shape KPIs name their statistic) or the state with its reason", () => {
  const lat = measureKpi("output_latency_ms", spread(600, 8, 70));
  assert.equal(cell(lat, "output_latency_ms"), `${lat.stats.p95.toFixed(1)} p95`);
  assert.equal(cell(measureKpi("hangs", { value: 0, n: 116 }), "hangs"), "0.00");
  assert.equal(cell(measureKpi("memory_mb", { value: 812.4, n: 120 }), "memory_mb"), "812");
  assert.equal(cell(unknownKpi("hangs", "occluded", { n: 3 }), "hangs"), "unknown(occluded)");
  assert.equal(cell(undefined, "hangs"), "missing");
  for (const id of REPORT_KPIS) assert.ok(KPI_CATALOG.some((k) => k.id === id), id);
});

test("the report renders every row with its setup facts, every unknown with its reason, incidents and mechanisms", () => {
  const md = renderReport(linuxXvfbResults());
  assert.match(md, /^## termhost-bench 2026-10-11T13-00-00Z-xvfb — linux-xvfb$/m);
  assert.match(md, /^### runner \(head 94d141a980a3\)$/m);
  assert.match(md, /^### tmux$/m);
  assert.match(md, /pty host unix_pty/);
  assert.match(md, /\| 5 \| focus \| 1\/2 \| 1\/unknown\(sessions_not_rendered\) \|/, "an unknown V is rendered as an absence, not 0");
  assert.match(md, /unknown\(insufficient_samples\)/);
  assert.match(md, /`hangs` unknown\(occluded\) — the window lost the foreground for most of the soak/);
  assert.doesNotMatch(md, /unknown\(no_baseline_level\)`|`degradation_[a-z_]+` unknown\(no_baseline_level\)/, "expected absences are not listed as findings");
  assert.match(md, /^### Incidents$/m);
  assert.match(md, /^- \*\*harness_error\*\* \(tmux\/default N=5 focus run 0\) at 2026-10-11T13:05:00Z: select-window timed out once$/m, "row incidents are rendered with their row");
  assert.match(md, /^### Mechanisms not OPERATIVE$/m);
  assert.match(md, /`counters\.gpu` INOPERATIVE-ON-THIS-MACHINE: no GPU \(llvmpipe\)/);
});

test("the runner-only copy keeps the runner's rows, validates, and renders with no third-party trace", () => {
  const doc = linuxXvfbResults();
  const prep = prepareReport(doc);
  assert.equal(prep.valid, true);
  assert.deepEqual(prep.redacted.rows.map((r) => r.setup.id), ["runner"]);
  assert.equal(validateResults(prep.redacted).valid, true);
  const md = renderReport(prep.redacted);
  assert.doesNotMatch(md, /tmux/);
  assert.doesNotMatch(md, /select-window/, "the tmux row's incident and detail are gone too");
  const broken = linuxXvfbResults();
  delete broken.rows[0].kpis.cpu_pct.state;
  const bad = prepareReport(broken);
  assert.equal(bad.valid, false);
  assert.equal(bad.redacted, null, "an invalid file is never published");
});

test("the gate holds on present, measured rows", () => {
  const doc = linuxXvfbResults();
  assert.deepEqual(
    checkAssertions(doc, { requireRowsN: [5], setups: ["runner", "tmux"], arms: ["focus"], requireMeasured: ["output_latency_ms", "cpu_pct", "memory_mb", "producer_stall_pct"], atN: [5] }),
    [],
  );
});

test("the gate names a missing row, an unmeasured KPI and a filtered-out role", () => {
  const doc = linuxXvfbResults();
  assert.deepEqual(checkAssertions(doc, { requireRowsN: [5], setups: ["runner", "tmux"], arms: ["focus", "tiled"] }), [
    "no runner row for N = 5, arm tiled",
    "no tmux row for N = 5, arm tiled",
  ]);
  assert.deepEqual(checkAssertions(doc, { requireMeasured: ["input_latency_ms"], atN: [5], setups: ["runner"], arms: ["focus"] }), [
    "runner (head) N = 5 focus run 0: input_latency_ms is unknown(insufficient_samples)",
  ]);
  assert.deepEqual(checkAssertions(doc, { requireMeasured: ["cpu_pct"], atN: [15], setups: ["runner"], arms: ["focus"] }), ["no runner row for N = 15, arm focus to check"]);
  assert.deepEqual(checkAssertions(doc, { requireMeasured: ["cpu_pct"], atN: [5], setups: ["runner"], arms: ["focus"], roles: ["baseline"] }), [
    "no runner row for N = 5, arm focus to check",
  ]);
});

test("a gate failure carries the runner's detail text but only a third party's state and reason (D7)", () => {
  const doc = linuxXvfbResults();
  const runner = checkAssertions(doc, { requireMeasured: ["hangs"], atN: [5], setups: ["runner"], arms: ["focus"] });
  assert.deepEqual(runner, ["runner (head) N = 5 focus run 0: hangs is unknown(occluded) — the window lost the foreground for most of the soak"]);
  const tmux = checkAssertions(doc, { requireMeasured: ["switch_latency_ms"], atN: [5], setups: ["tmux"], arms: ["focus"] });
  assert.deepEqual(tmux, ["tmux N = 5 focus run 0: switch_latency_ms is unknown(harness_error)"]);
});

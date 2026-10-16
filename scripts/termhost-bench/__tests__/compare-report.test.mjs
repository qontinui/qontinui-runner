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
import { COMPARED_KPIS, comparableBuild, compareBuilds, compareStatistic, renderComparison, spreadOf, statesOf, valueOf } from "../compare.mjs";
import { READING_FREE_DETAIL_MAX, REPORT_KPIS, cell, checkAssertions, prepareReport, readingFreeDetail, renderReport } from "../report.mjs";
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

test("the comparison renders every side's spread or unknown; with no verdict applied it draws none", () => {
  const doc = abDoc();
  doc.rows = doc.rows.map((r) => (r.setup.build?.role === "baseline" ? { ...r, kpis: { ...r.kpis, cpu_pct: unknownKpi("cpu_pct", "harness_error") } } : r));
  const md = renderComparison(compareBuilds(doc));
  assert.match(md, /^### runner: head a{12} vs baseline b{12} \(linux-xvfb\)$/m);
  assert.doesNotMatch(md, /^Verdict:/m, "compareBuilds alone judges nothing");
  assert.match(md, /^\| output_latency_ms \| p95 \| 5 \| focus \| [\d.]+ ms \[[\d.]+–[\d.]+\] \| [\d.]+ ms \[[\d.]+–[\d.]+\] \| 3 measured \| 2 measured, 1 unknown\(capture_unavailable\) \| – \|$/m);
  assert.match(md, /^\| cpu_pct \| mean \| 5 \| focus \| .+ \| unknown \| 3 measured \| 3 unknown\(harness_error\) \| – \|$/m);
  assert.match(md, /^\| hangs \| count \| 5 \| focus \| 1\.00 \[0\.00–2\.00\] \|/m);
  const tableRows = md.split(/\r?\n/).filter((l) => l.startsWith("| ") && !l.startsWith("| KPI "));
  assert.ok(tableRows.length > 0);
  assert.ok(tableRows.every((l) => !/regression|improvement|no_change/i.test(l)), "no row carries a verdict until applyVerdict runs");
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
  assert.match(md, /^- \*\*runner\*\* \(head 94d141a980a3\): app 0\.1\.0; launch subject_instance_root; pty host unix_pty;/m);
  assert.match(md, /^- \*\*tmux\*\* \(-\): /m);
  assert.match(md, /^\| runner \| head 94d141a980a3 \| 5 \| focus \| 1\/2 \| 1\/unknown\(sessions_not_rendered\) \| claude-tui \(false\) \| unix_pty \|/m, "an unknown V is rendered as an absence, not 0");
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

test("a gate failure carries the runner's detail text but a third party's only with every number masked (D7)", () => {
  const doc = linuxXvfbResults();
  const runner = checkAssertions(doc, { requireMeasured: ["hangs"], atN: [5], setups: ["runner"], arms: ["focus"] });
  assert.deepEqual(runner, ["runner (head) N = 5 focus run 0: hangs is unknown(occluded) — the window lost the foreground for most of the soak"]);
  const tmux = checkAssertions(doc, { requireMeasured: ["switch_latency_ms"], atN: [5], setups: ["tmux"], arms: ["focus"] });
  assert.equal(tmux.length, 1);
  assert.match(tmux[0], /^tmux N = 5 focus run 0: switch_latency_ms is unknown\(harness_error\)/);
  const r = doc.rows.find((x) => x.setup.id === "tmux" && x.scenario.n === 5 && x.scenario.arm === "focus");
  r.kpis.switch_latency_ms = {
    ...r.kpis.switch_latency_ms,
    detail: "the probe fell behind its capture source (frame lag 56.0 ms judged as the p95, bound 50 ms, 32 frame(s) over it), so first appearances may be late",
  };
  const [line] = checkAssertions(doc, { requireMeasured: ["switch_latency_ms"], atN: [5], setups: ["tmux"], arms: ["focus"] });
  assert.equal(
    line,
    "tmux N = 5 focus run 0: switch_latency_ms is unknown(harness_error) — the probe fell behind its capture source (frame lag # ms judged as the p#, bound # ms, # frame(s) over it), so first appearances may be late",
    "the cause leaves the job; no reading does",
  );
  assert.doesNotMatch(line.split(" — ")[1], /\d/);
});

test("readingFreeDetail masks every number and bounds the length", () => {
  assert.equal(readingFreeDetail("pid 4680 at 1.5 ms, 3% of 0x140198"), "pid # at # ms, #% of #");
  const long = readingFreeDetail("x".repeat(1000));
  assert.equal(long.length, READING_FREE_DETAIL_MAX);
  assert.ok(long.endsWith("…"));
});

test("readingFreeDetail: sign, hex, any script's digits, exponents, Infinity and NaN are numbers (review N3)", () => {
  const cases = [
    ["median -15.63 ms in [-3.91, +13.91]", "median # ms in [#, #]"],
    ["handle 0xFFAA, hwnd 0x1a2b3c", "handle #, hwnd #"],
    ["１２３ ms (fullwidth), ٣٤ ms (arabic-indic)", "# ms (fullwidth), # ms (arabic-indic)"],
    ["1e-5 and 1.5E+3 and .5", "# and # and #"],
    ["lag Infinity ms, ratio -Infinity, mean NaN", "lag # ms, ratio #, mean #"],
    ["1,234,567 frames", "# frames"],
    ["win32-x64 1.139.1", "win#-x# #"],
  ];
  for (const [input, want] of cases) assert.equal(readingFreeDetail(input), want, input);
  for (const [, want] of cases) assert.doesNotMatch(want, /\p{Nd}/u);
});

test("readingFreeDetail keeps a labelled exit code: a cause, not a reading (review N3)", () => {
  assert.equal(readingFreeDetail("Code.exe died: exit code 3221225477 (0xC0000005) after 12 s"), "Code.exe died: exit code 3221225477 (0xC0000005) after # s");
  assert.equal(readingFreeDetail("the soak watch ended (stopped, exit 3) without"), "the soak watch ended (stopped, exit 3) without");
  assert.equal(readingFreeDetail("status 0xC0000005 seen at 42 ms"), "status # seen at # ms", "an unlabelled hex status is masked");
});

test("readingFreeDetail keeps an exit code only by allowlist (second review S1, third review S3)", () => {
  const masked = [
    ["exit code 56.0ms", "exit code #ms"],
    ["exit 56ms", "exit #ms"],
    ["exit 4123 ms of lag", "exit # ms of lag"],
    ["exit status: -75.7", "exit status: #"],
    ["exit: 12e3", "exit: #"],
    ["the probe did not exit 5000 ms after its stop", "the probe did not exit # ms after its stop"],
    ["exit 3%", "exit #%"],
    ["exit 1e5", "exit #"],
    ["exit 12345678901", "exit #"],
    // the third review's probe (mask.mjs): units, words and shapes no denylist names
    ["exit 4123 seconds of lag", "exit # seconds of lag"],
    ["exit 56 msec", "exit # msec"],
    ["exit 5000 milliseconds", "exit # milliseconds"],
    ["exit 120 lines", "exit # lines"],
    ["exit 300 rows", "exit # rows"],
    ["exit 64 MiB", "exit # MiB"],
    ["exit 12 kHz", "exit # kHz"],
    ["exit 3 of 5 frames", "exit # of # frames"],
    ["exit 75 percent", "exit # percent"],
    ["exit 4123\tms", "exit #\tms"],
    ["exit 12 / 60", "exit # / #"],
    ["exit 9999999999 (0xC0000005)", "exit # (#)"],
    ["exit 4294967296", "exit #"],
    ["EXIT CODE 5000 MS", "EXIT CODE # MS"],
    ["exit 4123 µs", "exit # µs"],
    ["exit 56  ms", "exit #  ms"],
    ["exit ٥٦ ms", "exit # ms"],
    ["exit ٥٦", "exit #"],
    ["exited with code 8000 then", "exited with code # then"],
    ["exit 30 fps", "exit # fps"],
    ["exit 16.7ms", "exit #ms"],
    ["exit 250 ticks", "exit # ticks"],
    ["exit 60 Hz", "exit # Hz"],
    ["exit -75", "exit #"],
    ["exit 2e3", "exit #"],
    ["exited with code -1", "exited with code #"],
    ["exit 3 at 4 ms", "exit # at # ms"],
  ];
  for (const [input, want] of masked) assert.equal(readingFreeDetail(input), want, input);
  const kept = [
    "exit 3",
    "exit code 3221225477 (0xC0000005)",
    "exit 4294967295",
    "exit 0x1F4",
    "exit code 0xC0000005",
    "(stopped, exit 3) without",
    "exit 3; retry",
    "exit 3: refused",
    "exit 1234567890",
    "exited with code 1 while starting",
    "exit 3 after the stop",
  ];
  for (const k of kept) assert.equal(readingFreeDetail(k), k, k);
  assert.equal(readingFreeDetail("exit 3 (0xC0000005) after 12 ms"), "exit 3 (0xC0000005) after # ms");
  // Round 4 N3: the status group needs the same terminator after it.
  for (const [input, want] of [
    ["exit 5 (0x5)ms", "exit # (#)ms"],
    ["exit 12 (0xC) 340 ms", "exit # (#) # ms"],
    ["exit 7 (0x7) (0x8)", "exit # (#) (#)"],
    ["exit 3 on-demand", "exit # on-demand"],
    ["exit 3 afterward", "exit # afterward"],
  ]) assert.equal(readingFreeDetail(input), want, input);
  for (const k of ["exit 0x1F;", "exit 7 (0xC0000005);", "exited with code 4294967295)", "exit 3 when stopped"]) assert.equal(readingFreeDetail(k), k, k);
  // The documented limit: a reading worded exactly like an exit code is kept.
  assert.equal(readingFreeDetail("waited until exit 4500; gave up"), "waited until exit 4500; gave up");
});

test("readingFreeDetail cuts by code point, never inside a surrogate pair (review N3)", () => {
  const cut = readingFreeDetail("\u{1F600}".repeat(300));
  assert.equal(Array.from(cut).length, READING_FREE_DETAIL_MAX);
  assert.doesNotMatch(cut, /[\uD800-\uDBFF](?![\uDC00-\uDFFF])/);
  const short = "\u{1F600}".repeat(READING_FREE_DETAIL_MAX);
  assert.equal(readingFreeDetail(short), short, "exactly the cap is not cut");
});

test("an unproven build is never one side of a comparison (D12, review round 2 M1)", () => {
  const doc = abDoc();
  for (const r of doc.rows) {
    if (r.setup.build?.role === "baseline") {
      r.setup.build = { state: "unknown", reason: "harness_error", detail: "never booted", role: "baseline", requested_commit: BASE };
      r.labels = ["unknown_build"];
    }
  }
  assert.equal(validateResults(doc).valid, true, JSON.stringify(validateResults(doc).errors.slice(0, 3)));
  assert.throws(() => compareBuilds(doc), /no runner\/default baseline rows to compare/);
});

test("an A/B gate fails on a runner row whose build is unproven, even a measured one (review round 3)", () => {
  const doc = linuxXvfbResults();
  const gate = { requireMeasured: ["cpu_pct"], atN: [5], setups: ["runner"], arms: ["focus"] };
  assert.deepEqual(checkAssertions(doc, { ...gate, abBaseline: true }), [], "proven builds pass");
  const r = doc.rows.find((x) => x.setup.id === "runner" && x.scenario.n === 5 && x.scenario.arm === "focus");
  r.setup.build = { state: "unknown", reason: "harness_error", detail: "never proven", role: r.setup.build.role, requested_commit: "b".repeat(40) };
  r.labels = [...(r.labels ?? []), "unknown_build"];
  assert.equal(r.kpis.cpu_pct.state, "measured", "the row's numbers are there");
  assert.deepEqual(checkAssertions(doc, gate), [], "outside an A/B run the gate is unchanged");
  assert.deepEqual(checkAssertions(doc, { ...gate, abBaseline: true }), [
    `runner (${r.setup.build.role}) N = 5 focus run 0: the build is unproven (unknown_build); an A/B gate needs both builds proven (D12)`,
  ]);
});

test("only an UNPROVEN build is left out of a comparison: a third party's not_applicable build is compared (Phase 4 finding)", () => {
  const a = abDoc();
  const b = abDoc();
  const tmux = (doc) => doc.rows.find((r) => r.setup.id === "tmux");
  assert.equal(tmux(a).setup.build.state, "not_applicable");
  const cmp = compareBuilds(a, b, { setup: "tmux" });
  assert.ok(cmp.entries.length > 0, "tmux rows pair across two files");
  assert.ok(cmp.entries.some((e) => e.head.total === 1 && e.baseline.total === 1));
  // ... while an unproven runner baseline is still never one side.
  for (const r of b.rows) {
    if (r.setup.id === "runner") r.setup.build = { state: "unknown", reason: "harness_error", detail: "never proven", role: "baseline", requested_commit: BASE };
  }
  assert.throws(() => compareBuilds(a, b), /no runner\/default baseline rows to compare/);
});

test("a runner row is comparable only with a proven build: not_applicable is left out there, kept for a third party (review round 5)", () => {
  const a = abDoc();
  const b = abDoc();
  // The second file's baseline rows only (all of them would collide with its head rows).
  b.rows = b.rows.filter((r) => r.setup.build?.role !== "head");
  for (const r of b.rows) {
    if (r.setup.id === "runner") r.setup.build = { state: "not_applicable", reason: "not_applicable_to_setup" };
  }
  assert.equal(validateResults(b).valid, true, "the schema accepts the shape, so compare must not rely on it never appearing");
  assert.throws(() => compareBuilds(a, b), /no runner\/default baseline rows to compare/);
  assert.equal(comparableBuild(b.rows.find((r) => r.setup.id === "runner")), false);
  assert.equal(comparableBuild(b.rows.find((r) => r.setup.id === "tmux")), true, "a third party's not_applicable build");
  assert.equal(comparableBuild(a.rows.find((r) => r.setup.id === "runner")), true, "a proven runner build");
  assert.equal(comparableBuild({ setup: { id: "tmux", build: { state: "unknown", reason: "harness_error" } } }), false);
});

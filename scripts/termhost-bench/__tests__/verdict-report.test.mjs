/**
 * Phase 6: the regression verdict (`compare --baseline`), the environment report
 * and its headline line, and the derived runner diagnostics.
 *
 *   - compare never fires on an unknown or under-sampled side, fires on a synthetic
 *     regression, and is suppressed by overlapping run spreads;
 *   - no baseline file means unknown("no baseline") and exit 0;
 *   - the report refuses two environments;
 *   - the windows-desktop headline line appears only when all four setups'
 *     headline KPIs read measured at N = 15 in both arms.
 *
 *   node --test scripts/termhost-bench/__tests__/verdict-report.test.mjs
 */

import { test } from "node:test";
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { cpSync, existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import {
  BASELINE_SCHEMA_ID,
  GATE_SIMULATION,
  GATING_KPI,
  HEADLINE_KPIS,
  MIN_RUNS_FLOOR,
  NO_BASELINE,
  PROPOSED_GATE_RELATIVE,
  PROPOSED_THRESHOLDS,
  SCHEDULED_RUNS,
  applyVerdict,
  compareBuilds,
  judgeEntry,
  poolGate,
  proposeBaseline,
  readBaselineFile,
  renderComparison,
  spreadOf,
  thresholdMargin,
  validateBaseline,
  verdictFails,
} from "../compare.mjs";
import { HEADLINE_LINE, headlineStatus, oneEnvironment, renderReport } from "../report.mjs";
import { derivedRunnerDiagnostics, summarizeDiagnostic, transportRates } from "../diagnostics.mjs";
import { main } from "../cli.mjs";
import { measureKpi, notApplicableKpi, unknownKpi } from "../stats.mjs";
import { validateResults } from "../validate.mjs";
import { linuxXvfbResults, measuredKpis, row, windowsDesktopResults } from "./helpers/results.mjs";

const HERE = dirname(fileURLToPath(import.meta.url));
const BASELINES_DIR = join(HERE, "..", "baselines");
const HEAD = "a".repeat(40);
const BASE = "b".repeat(40);
const LEVELS = [1, 5, 15];
const ARM_LIST = ["focus", "tiled"];

/** An output_latency_ms value whose p95 is exactly `p95` (600 identical samples). */
function latencyWithP95(p95) {
  return measureKpi("output_latency_ms", Array.from({ length: 600 }, () => p95));
}

/**
 * A runner row of one build role at (n, arm), run `runIndex`: output_latency_ms
 * p95 = `p95` (null = unknown), and any other KPI overrides.
 */
function runnerRow(role, commit, runIndex, n, arm, p95, extra = {}) {
  return row({
    setupOverrides: { build: { commit, source: "artifact", role }, launch_mode: "subject_instance_root", pty_host_impl: "unix_pty", session_shell: "bash" },
    scenarioOverrides: { n, arm, v_target: 1, v_actual: 1, run_index: runIndex, run_count: 3 },
    kpis: measuredKpis({
      output_latency_ms: p95 === null ? unknownKpi("output_latency_ms", "capture_unavailable", { detail: "no frames" }) : latencyWithP95(p95),
      ...extra,
    }),
  });
}

/**
 * A same-VM A/B document over the whole ladder (N 1, 5, 15 x focus, tiled): each
 * side's output_latency_ms p95 per run, scaled per level (`at(n, arm)` overrides a
 * level), so the gate pools six entries.
 */
function abDoc(headP95s, baseP95s, { at = () => null, extra = () => ({}) } = {}) {
  const doc = linuxXvfbResults();
  doc.rows = [];
  for (const n of LEVELS) {
    for (const arm of ARM_LIST) {
      const scale = 1 + n / 15;
      const o = at(n, arm) ?? {};
      const h = o.head ?? headP95s;
      const b = o.base ?? baseP95s;
      h.forEach((v, i) => doc.rows.push(runnerRow("head", HEAD, i, n, arm, v === null ? null : v * scale, extra("head", n, arm))));
      b.forEach((v, i) => doc.rows.push(runnerRow("baseline", BASE, i, n, arm, v === null ? null : v * scale, extra("baseline", n, arm))));
    }
  }
  return doc;
}

/** A usable baseline file for linux-xvfb: the gate's relative 0.14, min_runs 3. */
function baselineFile(over = {}) {
  return {
    schema: BASELINE_SCHEMA_ID,
    environment: "linux-xvfb",
    setup: "runner",
    variant: "default",
    recorded_from: { run_id: "fixture", recorded_at: "2026-10-16T00:00:00Z", baseline_commit: BASE, head_commit: HEAD },
    min_runs: 3,
    thresholds: { output_latency_ms: { relative: 0.14, absolute: 5 }, cpu_pct: { relative: 0.25, absolute: 5 }, hangs: { relative: 0, absolute: 1 } },
    ...over,
  };
}

const present = (b = baselineFile()) => ({ state: "present", baseline: b });
const judge = (doc, b) => applyVerdict(compareBuilds(doc), present(b));
const lat = (cmp, n = 5, arm = "focus") => cmp.entries.find((e) => e.kpi === "output_latency_ms" && e.n === n && e.arm === arm);

// ---------------------------------------------------------------------------
// compare: the gate and the informational verdicts
// ---------------------------------------------------------------------------

test("a synthetic regression across the ladder fires the gate: beyond the threshold, spreads disjoint, reproduced (mutation M2)", () => {
  const cmp = judge(abDoc([80, 82, 84], [50, 52, 54]));
  assert.equal(cmp.gate.verdict, "regression");
  assert.equal(cmp.gate.gating, true);
  assert.equal(cmp.gate.entries.length, 6);
  assert.equal(cmp.gate.reproduced_at.length, 6);
  assert.equal(cmp.verdict.verdict, "regression");
  assert.deepEqual(cmp.verdict.gating.kpi, GATING_KPI);
  assert.equal(verdictFails(cmp.verdict), true);
  assert.equal(lat(cmp).verdict.gating, false, "each entry is informational; the pool gates");
  const md = renderComparison(cmp);
  assert.match(md, /^Verdict: \*\*regression\*\*/m);
  assert.match(md, /^\| output_latency_ms p95, geometric mean over N x arm, .+ \| \*\*regression\*\* \|$/m);
  assert.match(md, /^\| output_latency_ms \| p95 \| 5 \| focus \| .+ \| \*\*regression\*\* \| no \(pooled into the gate\) \|$/m);
});

test("an improvement is the symmetric case, and never fails the job", () => {
  const cmp = judge(abDoc([30, 31, 32], [50, 52, 54]));
  assert.equal(cmp.gate.verdict, "improvement");
  assert.equal(verdictFails(cmp.verdict), false);
});

test("overlapping run spreads suppress the gate even beyond the threshold (mutation M3)", () => {
  // Head median 80 vs 52 at every level, but one head run (53) inside the baseline spread.
  const cmp = judge(abDoc([53, 80, 84], [50, 52, 54]));
  assert.deepEqual([cmp.gate.verdict, cmp.gate.reason], ["no_change", "spreads_overlap"]);
  assert.deepEqual([lat(cmp).verdict.verdict, lat(cmp).verdict.reason], ["no_change", "spreads_overlap"]);
  assert.equal(verdictFails(cmp.verdict), false);
});

test("a worsening within the threshold is no_change, even with disjoint spreads", () => {
  // 56 vs 52: 7.7 %, under the gate's 14 %.
  const cmp = judge(abDoc([55, 56, 57], [50, 52, 54]));
  assert.deepEqual([cmp.gate.verdict, cmp.gate.reason], ["no_change", "within_threshold"]);
});

test("a regression at one entry alone does not gate: it must reproduce (the pool alone is not enough)", () => {
  // N = 15 focus 4x worse, every other level unchanged: the pooled geometric mean moves (4^(1/6) = 1.26).
  const doc = abDoc([50, 52, 54], [50, 52, 54], { at: (n, arm) => (n === 15 && arm === "focus" ? { head: [200, 208, 216] } : null) });
  const cmp = judge(doc);
  assert.deepEqual([cmp.gate.verdict, cmp.gate.reason], ["no_change", "not_reproduced"]);
  assert.equal(lat(cmp, 15, "focus").verdict.verdict, "regression", "the entry itself still reads regression, informationally");
  assert.equal(verdictFails(cmp.verdict), false);
  // The same regression at two levels reproduces.
  const two = abDoc([50, 52, 54], [50, 52, 54], { at: (n, arm) => (n >= 5 && arm === "focus" ? { head: [200, 208, 216] } : null) });
  assert.equal(judge(two).gate.verdict, "regression");
});

test("only output_latency_ms p95 gates: another KPI's regression is computed, rendered and never fails the job", () => {
  const cpu = (role) => ({ cpu_pct: measureKpi("cpu_pct", Array.from({ length: 120 }, () => (role === "head" ? 300 : 50))) });
  const doc = abDoc([50, 52, 54], [50, 52, 54], { extra: (role) => cpu(role) });
  // Give the cpu rows a run spread: vary by run index.
  for (const r of doc.rows) r.kpis.cpu_pct = measureKpi("cpu_pct", Array.from({ length: 120 }, () => (r.setup.build.role === "head" ? 300 : 50) + r.scenario.run_index));
  const cmp = judge(doc);
  const c = cmp.entries.find((e) => e.kpi === "cpu_pct" && e.n === 15 && e.arm === "tiled");
  assert.deepEqual([c.verdict.verdict, c.verdict.gating], ["regression", false]);
  assert.notEqual(cmp.verdict.verdict, "regression");
  assert.match(cmp.verdict.reason, /informational entries regressed \(never gating\)/);
  assert.equal(verdictFails(cmp.verdict), false);
  assert.match(renderComparison(cmp), /^\| cpu_pct \| mean \| 15 \| tiled \| .+ \| \*\*regression\*\* \| no \(informational\) \|$/m);
});

test("an unknown side never fires, however bad the other looks; unknown runs are never folded in as numbers (mutation M1)", () => {
  const baseUnknown = judge(abDoc([500, 510, 520], [null, null, null]));
  assert.deepEqual([lat(baseUnknown).verdict.verdict, lat(baseUnknown).verdict.reason], ["unknown", "baseline_unknown"]);
  assert.equal(baseUnknown.gate.verdict, "unknown");
  assert.match(baseUnknown.gate.reason, /^too_few_gate_entries; not pooled \(6 of 6\): N=1 focus \(baseline unknown\), /);
  assert.equal(verdictFails(baseUnknown.verdict), false);
  const headUnknown = judge(abDoc([null, null, null], [50, 52, 54]));
  assert.deepEqual([lat(headUnknown).verdict.verdict, lat(headUnknown).verdict.reason], ["unknown", "head_unknown"]);
  assert.equal(verdictFails(headUnknown.verdict), false);
});

test("an under-sampled side never fires: fewer measured runs than the baseline's min_runs (mutation M4)", () => {
  const cmp = judge(abDoc([500, null, null], [50, 52, 54]));
  assert.deepEqual([cmp.gate.verdict, cmp.gate.reason], ["unknown", "head_under_sampled"]);
  assert.deepEqual([lat(cmp).verdict.verdict, lat(cmp).verdict.reason], ["unknown", "head_under_sampled"]);
  assert.equal(verdictFails(cmp.verdict), false);
  const base = judge(abDoc([500, 510, 520], [50, null, null]));
  assert.deepEqual([base.gate.verdict, base.gate.reason], ["unknown", "baseline_under_sampled"]);
  // min_runs comes from the baseline file: 3 here, so two measured runs are too few.
  assert.deepEqual([judge(abDoc([80, 82, null], [50, 52, 54])).gate.reason], ["head_under_sampled"]);
  // A run missing at ANY pooled entry contributes no pooled value.
  const gap = abDoc([80, 82, 84], [50, 52, 54], { at: (n, arm) => (n === 1 && arm === "tiled" ? { head: [80, null, 84] } : null) });
  assert.equal(judge(gap).gate.head.spread.runs, 2);
});

test("a head that lost entries the baseline measured makes the gate unknown, never no_change (the wedge case)", () => {
  // The head wedges every pane at N = 15; N = 1 and 5 are unchanged.
  const doc = abDoc([50, 52, 54], [50, 52, 54], { at: (n) => (n === 15 ? { head: [null, null, null] } : null) });
  const cmp = judge(doc);
  assert.equal(cmp.gate.verdict, "unknown");
  assert.equal(cmp.gate.reason, "head_unknown_where_baseline_measured: N=15 focus, N=15 tiled; not pooled (2 of 6): N=15 focus (head unknown), N=15 tiled (head unknown)");
  assert.equal(cmp.verdict.verdict, "unknown");
  assert.equal(verdictFails(cmp.verdict), false, "an unknown gate never fails the job");
  // Even when the rest regresses, a lost head entry leaves the verdict unknown, not regression.
  const both = abDoc([80, 82, 84], [50, 52, 54], { at: (n, arm) => (n === 5 && arm === "tiled" ? { head: [null, null, null] } : null) });
  assert.deepEqual([judge(both).gate.verdict, judge(both).gate.reason.split(";")[0]], ["unknown", "head_unknown_where_baseline_measured: N=5 tiled"]);
});

test("an entry unknown on both builds, or on the baseline only, leaves the pool by name and the gate judges the rest", () => {
  const bothUnknown = abDoc([80, 82, 84], [50, 52, 54], { at: (n, arm) => (n === 5 && arm === "tiled" ? { head: [null, null, null], base: [null, null, null] } : null) });
  const cmp = judge(bothUnknown);
  assert.equal(cmp.gate.verdict, "regression");
  assert.equal(cmp.gate.entries.length, 5);
  assert.deepEqual(cmp.gate.excluded, [{ n: 5, arm: "tiled", why: "head unknown, baseline unknown" }]);
  assert.equal(cmp.gate.reason, "beyond_threshold_and_spreads_disjoint; not pooled (1 of 6): N=5 tiled (head unknown, baseline unknown)");
  assert.match(cmp.verdict.reason, /not pooled \(1 of 6\): N=5 tiled/, "the overall verdict carries it too");
  assert.match(renderComparison(cmp), /not pooled \(1 of 6\): N=5 tiled/);
  const baseOnly = abDoc([80, 82, 84], [50, 52, 54], { at: (n, arm) => (n === 1 && arm === "focus" ? { base: [null, null, null] } : null) });
  assert.deepEqual([judge(baseOnly).gate.verdict, judge(baseOnly).gate.excluded], ["regression", [{ n: 1, arm: "focus", why: "baseline unknown" }]]);
  assert.equal(judge(abDoc([80, 82, 84], [50, 52, 54])).gate.excluded, undefined, "nothing excluded, nothing said");
});

test("a headline KPI with no recorded threshold is unknown; a non-headline KPI is not judged at all", () => {
  const cmp = judge(abDoc([80, 82, 84], [50, 52, 54]));
  const mem = cmp.entries.find((e) => e.kpi === "memory_mb");
  assert.deepEqual([mem.verdict.verdict, mem.verdict.reason], ["unknown", "no_threshold"]);
  for (const e of cmp.entries) assert.equal(e.verdict !== undefined, HEADLINE_KPIS.includes(e.kpi), e.kpi);
  assert.ok(!HEADLINE_KPIS.includes("frame_interval_ms"));
});

test("a higher-is-better KPI regresses downwards; the margin is drawn from the BASELINE median (mutation N3a)", () => {
  const e = { kpi: "x", direction: "higher_is_better", head: { spread: { median: 0.5, min: 0.4, max: 0.6, runs: 3 } }, baseline: { spread: { median: 0.99, min: 0.98, max: 1, runs: 3 } } };
  assert.equal(judgeEntry(e, { relative: 0.1 }, 2).verdict, "regression");
  assert.equal(judgeEntry({ ...e, direction: "neutral" }, { relative: 0.1 }, 2).verdict, "unknown");
  // 100 -> 130 with 25 %: 30 > 25 from the baseline median (a margin from the head's, 32.5, would say no_change).
  assert.equal(thresholdMargin({ relative: 0.25 }, 100), 25);
  const lower = { kpi: "y", direction: "lower_is_better", head: { spread: { median: 130, min: 128, max: 132, runs: 3 } }, baseline: { spread: { median: 100, min: 98, max: 102, runs: 3 } } };
  assert.equal(judgeEntry(lower, { relative: 0.25 }, 3).verdict, "regression");
});

test("no baseline file: every verdict, the gate's included, is unknown(no baseline), and nothing fails", () => {
  const dir = mkdtempSync(join(tmpdir(), "tb-verdict-"));
  try {
    const missing = readBaselineFile(join(dir, "linux-xvfb.json"));
    assert.deepEqual(missing, { state: "absent", reason: NO_BASELINE });
    const cmp = applyVerdict(compareBuilds(abDoc([800, 820, 840], [50, 52, 54])), missing);
    assert.deepEqual([cmp.verdict.verdict, cmp.verdict.reason], ["unknown", "no baseline"]);
    assert.deepEqual([cmp.gate.verdict, cmp.gate.reason], ["unknown", "no baseline"]);
    assert.ok(cmp.entries.filter((e) => e.verdict).every((e) => e.verdict.verdict === "unknown" && e.verdict.reason === "no baseline"));
    assert.equal(verdictFails(cmp.verdict), false);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("the CLI: a missing baseline file exits 0 with unknown; a regression exits 1; the verdict JSON says which and what gates (mutation M5)", async () => {
  const dir = mkdtempSync(join(tmpdir(), "tb-verdict-cli-"));
  try {
    const results = join(dir, "results.json");
    writeFileSync(results, JSON.stringify(abDoc([80, 82, 84], [50, 52, 54])));
    const out = [];
    const err = [];
    const io = { out: (s) => out.push(s), err: (s) => err.push(s) };
    const vj = join(dir, "verdict.json");
    const code = await main(["compare", results, "--baseline", join(dir, "absent.json"), "--verdict-json", vj], io);
    assert.equal(code, 0, err.join(""));
    assert.equal(JSON.parse(readFileSync(vj, "utf8")).verdict, "unknown");
    assert.equal(JSON.parse(readFileSync(vj, "utf8")).reason, "no baseline");
    assert.match(out.join(""), /^Verdict: \*\*unknown\*\* — no baseline/m);

    const bf = join(dir, "linux-xvfb.json");
    writeFileSync(bf, JSON.stringify(baselineFile()));
    assert.equal(await main(["compare", results, "--baseline", bf, "--verdict-json", vj], io), 1);
    const v = JSON.parse(readFileSync(vj, "utf8"));
    assert.equal(v.verdict, "regression");
    assert.equal(v.gating.kpi, "output_latency_ms");
    assert.equal(v.gating.statistic, "p95");
    assert.equal(v.gating.entries.length, 6);

    const headOnly = join(dir, "head-only.json");
    writeFileSync(headOnly, JSON.stringify(abDoc([80, 82, 84], [])));
    assert.equal(await main(["compare", headOnly, "--baseline", bf, "--verdict-json", vj], io), 0);
    assert.equal(JSON.parse(readFileSync(vj, "utf8")).reason, "no comparison");

    writeFileSync(bf, "{");
    assert.equal(await main(["compare", results, "--baseline", bf, "--verdict-json", vj], io), 0);
    assert.match(JSON.parse(readFileSync(vj, "utf8")).reason, /^baseline is not JSON/);

    assert.equal(await main(["compare", results, "--verdict-json", vj], io), 2, "--verdict-json needs --baseline");
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("compare.mjs run through a symlink still runs (S1): it never does nothing silently", { skip: process.platform === "win32" && "symlinks need privileges on Windows" }, () => {
  const dir = mkdtempSync(join(tmpdir(), "tb-link-"));
  try {
    const link = join(dir, "compare-link.mjs");
    symlinkSync(join(HERE, "..", "compare.mjs"), link);
    const r = spawnSync(process.execPath, [link], { encoding: "utf8" });
    assert.equal(r.status, 2, `stdout ${r.stdout} stderr ${r.stderr}`);
    assert.match(r.stderr, /compare takes one or two results files/);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("a baseline for another environment or setup judges nothing", () => {
  const cmp = judge(abDoc([80, 82, 84], [50, 52, 54]), { ...baselineFile(), environment: "windows-hosted", min_runs: 2 });
  assert.equal(cmp.verdict.verdict, "unknown");
  assert.match(cmp.verdict.reason, /^baseline mismatch: the file is for windows-hosted/);
});

test("validateBaseline refuses every baseline that would disarm the gate (the review's disarm.mjs, S5 and N2)", () => {
  assert.deepEqual(validateBaseline(baselineFile()), []);
  const errs = (over) => validateBaseline(baselineFile(over)).join("\n");
  // disarm.mjs: an empty thresholds object, min_runs above the schedule, 25 meant as 25 %.
  assert.match(errs({ min_runs: 2, thresholds: {} }), /thresholds\.output_latency_ms: required — it is the KPI that gates the job/);
  assert.match(errs({ min_runs: 99 }), /min_runs 99 exceeds the 3 run\(s\) the scheduled linux-xvfb job takes/);
  assert.match(errs({ min_runs: 3, environment: "windows-hosted" }), /exceeds the 2 run\(s\) the scheduled windows-hosted job takes/);
  assert.match(errs({ thresholds: { output_latency_ms: { relative: 25 } } }), /relative: must be a share in \[0, 1\]/);
  assert.match(errs({ thresholds: { output_latency_ms: { absolute: 5 } } }), /output_latency_ms\.relative: required and > 0/);
  assert.match(errs({ thresholds: { output_latency_ms: { relative: 0, absolute: 5 } } }), /output_latency_ms\.relative: required and > 0/);
  // N2: a count KPI's relative-only threshold has margin 0 at a 0 median.
  assert.match(errs({ thresholds: { output_latency_ms: { relative: 0.14 }, hangs: { relative: 0.5 } } }), /hangs\.absolute: a count KPI needs an absolute floor/);
  assert.equal(judgeEntry({ kpi: "hangs", direction: "lower_is_better", head: { spread: { median: 1, min: 1, max: 1, runs: 2 } }, baseline: { spread: { median: 0, min: 0, max: 0, runs: 2 } } }, { relative: 0.5 }, 2).verdict, "regression", "why N2 matters: this is what the validator now refuses");
  // The rest.
  assert.match(errs({ setup: "tmux" }), /runner's own readings only/);
  assert.match(errs({ min_runs: 1 }), /min_runs/);
  assert.match(errs({ thresholds: { output_latency_ms: { relative: 0.14 }, frame_interval_ms: { relative: 0.2 } } }), /not a headline KPI/);
  assert.match(errs({ thresholds: { output_latency_ms: { relative: 0.14 }, cpu_pct: {} } }), /needs relative and\/or absolute/);
  assert.match(errs({ thresholds: { output_latency_ms: { relative: 0.14 }, cpu_pct: { relative: 0 } } }), /zero threshold/);
  assert.match(errs({ schema: "x" }), /schema must be/);
});

test("the scheduled runs compare.mjs assumes are the workflow's own defaults", () => {
  const yml = readFileSync(join(HERE, "..", "..", "..", ".github", "workflows", "termhost-bench.yml"), "utf8");
  const defaults = [...yml.matchAll(/runs="\$\{IN_RUNS:-(\d+)\}"/g)].map((m) => Number(m[1]));
  assert.deepEqual(defaults, [SCHEDULED_RUNS["linux-xvfb"], SCHEDULED_RUNS["windows-hosted"]], "linux-xvfb's job first, then windows-hosted's");
  assert.match(yml, /^ {6}runs:\n {8}description: .+\n {8}type: string\n {8}default: ""$/m, "dispatch leaves runs to each job");
});

test("a proposed baseline holds the baseline build's runner readings, validates, and needs a proven baseline commit", () => {
  const cmp = compareBuilds(abDoc([80, 82, 84], [50, 52, 54]));
  const b = proposeBaseline(cmp, { runId: "ci-1-1", recordedAt: "2026-10-16T00:00:00Z" });
  assert.deepEqual(validateBaseline(b), []);
  assert.equal(b.min_runs, 3, "linux-xvfb's scheduled runs");
  assert.equal(b.thresholds.output_latency_ms.relative, PROPOSED_GATE_RELATIVE[3]);
  assert.equal(b.recorded_from.baseline_commit, BASE);
  assert.deepEqual(Object.keys(b.thresholds).sort(), [...HEADLINE_KPIS].sort());
  const r = b.readings.find((x) => x.kpi === "output_latency_ms" && x.n === 15);
  assert.deepEqual([r.median, r.min, r.max, r.runs], [104, 100, 108, 3]);
  assert.ok(!JSON.stringify(b).includes("tmux"), "runner rows only (D7)");
  assert.throws(() => proposeBaseline({ ...cmp, baseline_commit: null }), /D12/);
  assert.equal(applyVerdict(compareBuilds(abDoc([80, 82, 84], [50, 52, 54])), present(b)).verdict.verdict, "regression");
  const win = proposeBaseline({ ...cmp, environment: "windows-hosted" });
  assert.deepEqual([win.min_runs, win.thresholds.output_latency_ms.relative], [2, PROPOSED_GATE_RELATIVE[2]]);
  assert.deepEqual(validateBaseline(win), []);
});

test("every committed baseline file is valid and armed (a corrupt or disarmed one would silence the scheduled gate)", { skip: !existsSync(BASELINES_DIR) && "no committed baseline yet: deferred to the first scheduled run of each environment" }, () => {
  const files = readdirSync(BASELINES_DIR).filter((f) => f.endsWith(".json"));
  assert.ok(files.length > 0, "baselines/ exists but holds no baseline");
  for (const f of files) {
    const st = readBaselineFile(join(BASELINES_DIR, f));
    assert.equal(st.state, "present", `${f}: ${st.reason}`);
    const b = st.baseline;
    assert.equal(`${b.environment}.json`, f, "a baseline file is named for its environment");
    // The three disarm checks, stated here as well as in the validator.
    assert.ok(b.thresholds[GATING_KPI], `${f}: no threshold for the gating KPI`);
    assert.ok(b.thresholds[GATING_KPI].relative > 0 && b.thresholds[GATING_KPI].relative <= 1, `${f}: the gate's relative threshold is outside (0, 1]`);
    assert.ok(b.min_runs >= MIN_RUNS_FLOOR && b.min_runs <= SCHEDULED_RUNS[b.environment], `${f}: min_runs ${b.min_runs} vs the ${SCHEDULED_RUNS[b.environment]} scheduled runs`);
  }
});

// ---------------------------------------------------------------------------
// the gate's false-red rate and power (the review's fp-sim.mjs, seeded)
// ---------------------------------------------------------------------------

/** mulberry32: a seeded uniform [0, 1) generator, so the bounds below are deterministic. */
function seeded(seed) {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

/** A lognormal factor with mean 1 and coefficient of variation `cv`. */
function lognormal(rand, cv) {
  const s = Math.sqrt(Math.log(1 + cv * cv));
  let u = 0;
  while (u === 0) u = rand();
  return Math.exp(-(s * s) / 2 + s * Math.sqrt(-2 * Math.log(u)) * Math.cos(2 * Math.PI * rand()));
}

/** A standard normal draw (Box-Muller). */
function normal(rand) {
  let u = 0;
  while (u === 0) u = rand();
  return Math.sqrt(-2 * Math.log(u)) * Math.cos(2 * Math.PI * rand());
}

/**
 * The share of trials in which the scheduled job would go red: six gate entries
 * (N 1, 5, 15 x two arms), `runs` runs per build, every value a typical p95
 * (100-300 ms) times lognormal run-to-run noise of `cv`, the head `effect` times
 * worse. `shared` is the share of the log-variance that is launch noise common
 * to both arms of one (N, build, run): run.mjs measures a unit's two arms in one
 * launch, so the six entries are not independent. Drives the real poolGate (and,
 * for contrast, any single entry's judgeEntry).
 */
function redRate({ runs, cv, effect, trials, seed, shared, relative = PROPOSED_GATE_RELATIVE[runs] }) {
  const rand = seeded(seed);
  const sigma = Math.sqrt(Math.log(1 + cv * cv));
  let gateRed = 0;
  let anyEntryRed = 0;
  for (let t = 0; t < trials; t += 1) {
    // One launch per (build, N, run), shared by its two arms.
    const launch = {};
    for (const build of ["head", "baseline"]) for (const n of LEVELS) launch[`${build}${n}`] = Array.from({ length: runs }, () => normal(rand));
    const entries = [];
    for (const n of LEVELS) {
      for (const arm of ARM_LIST) {
        const level = 100 + 200 * rand();
        const side = (build, f) => {
          const vals = Array.from({ length: runs }, (_, i) => {
            const z = Math.sqrt(shared) * launch[`${build}${n}`][i] + Math.sqrt(1 - shared) * normal(rand);
            return level * f * Math.exp(-(sigma * sigma) / 2 + sigma * z);
          });
          return { spread: spreadOf(vals), states: `${runs} measured`, values: vals.map((value, run) => ({ run, value })) };
        };
        entries.push({ kpi: GATING_KPI, direction: "lower_is_better", n, arm, head: side("head", effect), baseline: side("baseline", 1) });
      }
    }
    if (poolGate(entries, { relative }, runs).verdict === "regression") gateRed += 1;
    if (entries.some((e) => judgeEntry(e, PROPOSED_THRESHOLDS.ms, runs).verdict === "regression")) anyEntryRed += 1;
  }
  return { gate: gateRed / trials, anyEntry: anyEntryRed / trials };
}

const pct = (x) => `${(x * 100).toFixed(2)} %`;

test("the gate's false-red rate with nothing changed is <= 2 % at CV 0.2, launch noise independent or half shared, in both environments", (t) => {
  const lines = [];
  for (const shared of [0, 0.5]) {
    const linux = redRate({ runs: 3, cv: 0.2, effect: 1, trials: 10000, seed: 1, shared });
    assert.ok(linux.gate <= 0.02, `linux-xvfb, shared ${shared}: gate red with no change ${pct(linux.gate)}`);
    // What the review measured: any of the six entries alone, uncorrected, goes red far more often.
    assert.ok(linux.anyEntry > 0.1, `per-entry without the gate: ${pct(linux.anyEntry)}`);
    const win = redRate({ runs: 2, cv: 0.2, effect: 1, trials: 10000, seed: 2, shared });
    assert.ok(win.gate <= 0.02, `windows-hosted, shared ${shared}: gate red with no change ${pct(win.gate)}`);
    const key = shared === 0 ? "independent" : "shared";
    assert.equal(pct(linux.gate), pct(GATE_SIMULATION["linux-xvfb"].null[key] / 100), "GATE_SIMULATION quotes the measured figure");
    assert.equal(pct(win.gate), pct(GATE_SIMULATION["windows-hosted"].null[key] / 100), "GATE_SIMULATION quotes the measured figure");
    lines.push(`shared ${shared}: linux-xvfb ${pct(linux.gate)} (any single entry ${pct(linux.anyEntry)}), windows-hosted ${pct(win.gate)}`);
  }
  t.diagnostic(`null, CV 0.2: ${lines.join("; ")}`);
});

test("GATE_SIMULATION matches the thresholds, and the README quotes its figures", () => {
  for (const [env, sim] of Object.entries(GATE_SIMULATION)) {
    assert.equal(sim.runs, SCHEDULED_RUNS[env], env);
    assert.equal(sim.relative, PROPOSED_GATE_RELATIVE[sim.runs], env);
  }
  const readme = readFileSync(join(HERE, "..", "README.md"), "utf8");
  const line = readme.split("\n").find((l) => l.startsWith("Seeded simulation"));
  assert.ok(line, "the README's simulation paragraph");
  // Each figure in its labelled position: a swap of two figures fails.
  const L = GATE_SIMULATION["linux-xvfb"];
  const W = GATE_SIMULATION["windows-hosted"];
  const f = (v) => `${v.toFixed(2)} %`;
  const expected =
    `with nothing changed the gate goes red ${f(L.null.shared)} of the time on linux-xvfb (${L.runs} runs, relative ${L.relative}) ` +
    `and ${f(W.null.shared)} on windows-hosted (${W.runs} runs, ${W.relative}), or ${f(L.null.independent)} / ${f(W.null.independent)} with no sharing; ` +
    `a 30 % regression across every N goes red ${f(L.power.shared)} / ${f(W.power.shared)}, or ${f(L.power.independent)} / ${f(W.power.independent)} with no sharing.`;
  assert.ok(line.includes(expected), `the README states, in place:\n${expected}`);
  const doc = readFileSync(join(HERE, "..", "compare.mjs"), "utf8");
  assert.doesNotMatch(doc, /no change \d+(\.\d+)? % \(/, "compare.mjs prose carries no hand-copied figures");
});

test("the gate's power: a 30 % p95 regression across every N, at CV 0.2, independent and half-shared launch noise", (t) => {
  // The non-overlapping-spreads rule caps the power (3 runs against 3 are
  // disjoint 1 time in 20 under no change), and shared launch noise lowers it
  // further; these bounds pin what the tuned thresholds support. The windows-hosted
  // bound also catches a threshold that disarms its gate (e.g. 0.5).
  const lines = [];
  const bounds = { 0: { linux: 0.82, win: 0.65 }, 0.5: { linux: 0.7, win: 0.6 } };
  for (const shared of [0, 0.5]) {
    const linux = redRate({ runs: 3, cv: 0.2, effect: 1.3, trials: 4000, seed: 4, shared });
    const win = redRate({ runs: 2, cv: 0.2, effect: 1.3, trials: 4000, seed: 5, shared });
    assert.ok(linux.gate >= bounds[shared].linux, `linux-xvfb, shared ${shared}: power ${pct(linux.gate)}`);
    assert.ok(win.gate >= bounds[shared].win, `windows-hosted, shared ${shared}: power ${pct(win.gate)}`);
    const key = shared === 0 ? "independent" : "shared";
    assert.equal(pct(linux.gate), pct(GATE_SIMULATION["linux-xvfb"].power[key] / 100), "GATE_SIMULATION quotes the measured figure");
    assert.equal(pct(win.gate), pct(GATE_SIMULATION["windows-hosted"].power[key] / 100), "GATE_SIMULATION quotes the measured figure");
    lines.push(`shared ${shared}: linux-xvfb ${pct(linux.gate)}, windows-hosted ${pct(win.gate)}`);
  }
  const at15 = redRate({ runs: 3, cv: 0.15, effect: 1.3, trials: 4000, seed: 3, shared: 0.5 });
  assert.ok(at15.gate >= 0.9, `linux-xvfb power at CV 0.15, shared 0.5: ${pct(at15.gate)}`);
  lines.push(`linux-xvfb at CV 0.15, shared 0.5: ${pct(at15.gate)}`);
  t.diagnostic(`power, 30 %, CV 0.2: ${lines.join("; ")}`);
});

// ---------------------------------------------------------------------------
// report: one environment, the headline line
// ---------------------------------------------------------------------------

test("the report refuses two environments, in one call or across files (mutation M6)", async () => {
  const a = linuxXvfbResults();
  const b = windowsDesktopResults();
  assert.throws(() => renderReport([a, b]), /refusing to put linux-xvfb and windows-desktop in one table: numbers compare only within one environment \(D6\)/);
  assert.throws(() => oneEnvironment([a, b]), /D6/);
  assert.match(renderReport([a, linuxXvfbResults()]), /^### Every row \(linux-xvfb\)$/m);
  assert.equal(renderReport([a, linuxXvfbResults()]).match(/^\| setup \| build \| N \|/gm).length, 1, "one table for the environment");
  const dir = mkdtempSync(join(tmpdir(), "tb-report-"));
  try {
    writeFileSync(join(dir, "a.json"), JSON.stringify(a));
    writeFileSync(join(dir, "b.json"), JSON.stringify(b));
    const err = [];
    const code = await main(["report", join(dir, "a.json"), join(dir, "b.json")], { out: () => {}, err: (s) => err.push(s) });
    assert.equal(code, 2);
    assert.match(err.join(""), /refusing to put linux-xvfb and windows-desktop in one table/);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

const HEADLINE_SETUP_FACTS = {
  runner: {},
  vscode: {},
  "windows-terminal": { id: "windows-terminal", launch_mode: "portable_pinned" },
  "tmux-wsl": { id: "tmux-wsl", launch_mode: "private_server", pty_host_impl: "unix_pty", session_shell: "bash" },
};

/** A windows-desktop document with every headline setup at N = 15 in both arms, all measured. */
function headlineDoc() {
  const doc = windowsDesktopResults();
  doc.rows = [];
  for (const [setup, over] of Object.entries(HEADLINE_SETUP_FACTS)) {
    for (const arm of ["focus", "tiled"]) {
      doc.rows.push(
        row({
          setupId: setup === "runner" ? "runner" : "vscode",
          setupOverrides: over,
          scenarioOverrides: { n: 15, arm, v_target: arm === "focus" ? 1 : 9, v_actual: arm === "focus" ? 1 : 9 },
          kpis: measuredKpis(arm === "tiled" ? { switch_latency_ms: notApplicableKpi("switch_latency_ms", "not_applicable_to_arm") } : {}),
        }),
      );
    }
  }
  return doc;
}

const headlineLines = (md) => md.split("\n").filter((l) => /^headline: /.test(l));

test("the headline line is exactly the anchor's line, emitted when all four setups are measured at N = 15 in both arms", () => {
  assert.equal(HEADLINE_LINE, "headline: windows-desktop n=15 setups=runner,vscode,windows-terminal,tmux-wsl arms=focus,tiled state=measured");
  const doc = headlineDoc();
  assert.deepEqual(headlineStatus([doc]), { established: true, missing: [] });
  const md = renderReport(doc);
  assert.deepEqual(headlineLines(md), [HEADLINE_LINE]);
  assert.match(md, /^headline: windows-desktop n=15 setups=runner,vscode,windows-terminal,tmux-wsl arms=focus,tiled state=measured$/m, "the anchor's own regex matches");
  assert.match(md, /^### Headline: N = 15 /m);
  for (const s of Object.keys(HEADLINE_SETUP_FACTS)) assert.match(md, new RegExp(`^\\| ${s} \\| [^|]+ \\| tiled \\| 1 \\| claude-tui \\(false\\) \\| `, "m"), s);
});

test("the headline line is withheld when any headline KPI is unknown, a setup or an arm is missing, or N is not 15 (mutation M7)", () => {
  const cases = {
    "an unknown headline KPI": (d) => {
      d.rows.find((r) => r.setup.id === "tmux-wsl" && r.scenario.arm === "tiled").kpis.input_latency_ms = unknownKpi("input_latency_ms", "input_unsafe");
    },
    "a missing arm": (d) => {
      d.rows = d.rows.filter((r) => !(r.setup.id === "windows-terminal" && r.scenario.arm === "tiled"));
    },
    "a missing setup": (d) => {
      d.rows = d.rows.filter((r) => r.setup.id !== "vscode");
    },
    "N = 5 instead of 15": (d) => {
      for (const r of d.rows.filter((x) => x.setup.id === "runner")) r.scenario.n = 5;
    },
    "an unproven runner build": (d) => {
      for (const r of d.rows.filter((x) => x.setup.id === "runner")) r.setup.build = { state: "unknown", reason: "harness_error", role: "single" };
    },
    "a not_applicable that is not the arm's": (d) => {
      d.rows.find((r) => r.setup.id === "runner" && r.scenario.arm === "focus").kpis.hangs = notApplicableKpi("hangs", "not_applicable_to_setup");
    },
  };
  for (const [name, mutate] of Object.entries(cases)) {
    const doc = headlineDoc();
    mutate(doc);
    const st = headlineStatus([doc]);
    assert.equal(st.established, false, name);
    assert.ok(st.missing.length > 0, name);
    const md = renderReport(doc);
    assert.deepEqual(headlineLines(md), [], name);
    assert.match(md, /^The headline comparison is NOT established/m, name);
  }
  // Not windows-desktop: never a headline line.
  const linux = linuxXvfbResults();
  assert.deepEqual(headlineLines(renderReport(linux)), []);
  assert.equal(headlineStatus([linux]).established, false);
});

test("the headline line is withheld when its rows come from two machines (S3, the review's two-machines.mjs)", () => {
  const doc = (machine, setups) => {
    const d = headlineDoc();
    d.machine.name = machine;
    d.run.id = `run-${machine}`;
    d.rows = d.rows.filter((r) => setups.includes(r.setup.id));
    return d;
  };
  const docs = [doc("gpu-box-A", ["runner", "vscode"]), doc("laptop-B", ["windows-terminal", "tmux-wsl"])];
  const st = headlineStatus(docs);
  assert.equal(st.established, false);
  assert.deepEqual(st.missing, ["the rows come from 2 machines (gpu-box-A, laptop-B): one comparison is one machine's"]);
  assert.deepEqual(headlineLines(renderReport(docs)), []);
  // The same split across two runs on ONE machine is one comparison.
  const same = [doc("gpu-box-A", ["runner", "vscode"]), doc("gpu-box-A", ["windows-terminal", "tmux-wsl"])];
  assert.deepEqual(headlineLines(renderReport(same)), [HEADLINE_LINE]);
});

test("a baseline build's rows never stand behind the headline (mutation N3b)", () => {
  const doc = headlineDoc();
  for (const r of doc.rows.filter((x) => x.setup.id === "runner")) r.setup.build = { ...r.setup.build, role: "baseline" };
  const st = headlineStatus([doc]);
  assert.equal(st.established, false);
  assert.ok(st.missing.includes("runner focus: no N = 15 row") && st.missing.includes("runner tiled: no N = 15 row"), st.missing.join("; "));
});

test("report with several files honours --format json (N4)", async () => {
  const dir = mkdtempSync(join(tmpdir(), "tb-report-json-"));
  try {
    const a = headlineDoc();
    const b = headlineDoc();
    b.run.id = "second-run";
    writeFileSync(join(dir, "a.json"), JSON.stringify(a));
    writeFileSync(join(dir, "b.json"), JSON.stringify(b));
    const out = [];
    assert.equal(await main(["report", join(dir, "a.json"), join(dir, "b.json"), "--format", "json"], { out: (x) => out.push(x), err: () => {} }), 0);
    const j = JSON.parse(out.join(""));
    assert.deepEqual([j.valid, j.environment, j.runs], [true, "windows-desktop", [a.run.id, "second-run"]]);
    assert.deepEqual([j.headline.established, j.headline.line, j.headline.missing], [true, HEADLINE_LINE, []]);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("the N = 15 headline table names each group's labels, e.g. VS Code's dom_renderer_fallback (N5)", () => {
  const doc = linuxXvfbResults();
  const vs = row({ setupId: "vscode", scenarioOverrides: { n: 15, arm: "tiled" }, labels: ["dom_renderer_fallback", "software_rendered"] });
  doc.rows.push(vs);
  const md = renderReport(doc);
  const table = md.split("### Headline: N = 15")[1].split("\n\n")[1].split("\n");
  assert.match(table[0], /\| labels \|$/);
  assert.ok(table.some((l) => /^\| vscode \| .+ \| dom_renderer_fallback, software_rendered \|$/.test(l)), table.join("\n"));
});

test("both report tables name the cross-check profile beside every row, as the row records it (D2)", () => {
  const doc = headlineDoc();
  doc.rows.find((r) => r.setup.id === "tmux-wsl" && r.scenario.arm === "tiled").scenario.crosscheck_profile = "software-compositor";
  const md = renderReport(doc);
  const every = md.split("### Every row (windows-desktop)")[1].split("\n\n")[1].split("\n");
  assert.match(every[0], /\| pty_host_impl \| cross-check \|/);
  const everyBody = every.slice(2);
  assert.equal(everyBody.length, 8);
  for (const l of everyBody) assert.match(l, /\| (system_conhost|bundled_openconsole|unix_pty) \| (strict|software-compositor) \|/, l);
  assert.equal(everyBody.filter((l) => /\| software-compositor \|/.test(l)).length, 1, "the one relaxed row says so");
  const head = md.split("### Headline: N = 15")[1].split("\n\n")[1].split("\n");
  assert.match(head[0], /\| pty_host_impl \| cross-check \|/);
  const headBody = head.slice(2);
  assert.equal(headBody.length, 8);
  for (const l of headBody) assert.match(l, /\| (system_conhost|bundled_openconsole|unix_pty) \| (strict|software-compositor) \|/, l);
  assert.ok(headBody.some((l) => /^\| tmux-wsl \| [^|]+ \| tiled \| .*\| software-compositor \|/.test(l)), head.join("\n"));
});

test("the headline fixture is a valid results file (the line is never drawn from an invalid shape)", () => {
  const res = validateResults(headlineDoc());
  assert.equal(res.valid, true, JSON.stringify(res.errors.slice(0, 5)));
});

test("every row of the environment table carries profile_calibrated and pty_host_impl", () => {
  const md = renderReport(headlineDoc());
  const table = md.split("### Every row (windows-desktop)")[1].split("\n\n")[1].split("\n");
  assert.match(table[0], /\| profile \(calibrated\) \| pty_host_impl \|/);
  const body = table.slice(2);
  assert.equal(body.length, 8);
  for (const l of body) assert.match(l, /\| claude-tui \(false\) \| (system_conhost|bundled_openconsole|unix_pty) \|/);
});

// ---------------------------------------------------------------------------
// diagnostics
// ---------------------------------------------------------------------------

const SNAPSHOT = {
  since_reset_ms: 20000,
  reader: { chunks: 400, bytes: 4_000_000 },
  legs: { sse: { chunks: 0, bytes: 0 }, ws: { chunks: 0, bytes: 0 }, webview: { emits: 2000, emit_ns: 1e9, bytes: 3_900_000 } },
};

test("transport rates are perf-harness's measurements, derived from the counters' own window", () => {
  assert.deepEqual(transportRates(SNAPSHOT), { since_reset_ms: 20000, reader_bytes_per_sec: 200000, webview_events_per_sec: 100, bytes_per_webview_event: 2000 });
  assert.equal(transportRates({ ...SNAPSHOT, since_reset_ms: 0 }), null);
  assert.equal(transportRates({ since_reset_ms: 1000 }), null);
  assert.equal(transportRates({ ...SNAPSHOT, legs: { webview: { emits: 0 } } }).bytes_per_webview_event, null);
});

test("derived runner diagnostics are non-comparable, valid, and never invent a reading", () => {
  const measured = derivedRunnerDiagnostics({ "transport.counters": { comparable: false, source: "x", state: "measured", data: SNAPSHOT } }, { platform: "linux" });
  assert.equal(measured["transport.rates"].state, "measured");
  assert.equal(measured["renderer.memory"].state, "not_applicable");
  const win = derivedRunnerDiagnostics({ "transport.counters": { comparable: false, source: "x", state: "unknown", reason: "not_instrumented" } }, { platform: "win32" });
  assert.deepEqual([win["transport.rates"].state, win["transport.rates"].reason], ["unknown", "not_instrumented"]);
  assert.deepEqual([win["renderer.memory"].state, win["renderer.memory"].reason], ["unknown", "not_instrumented"]);
  assert.match(win["renderer.memory"].detail, /runner#1740/);
  const none = derivedRunnerDiagnostics({}, { platform: "linux" });
  assert.equal(none["transport.rates"].state, "unknown");
  for (const d of [measured, win, none]) for (const v of Object.values(d)) assert.equal(v.comparable, false);
  // Joined into a row, the document still validates.
  const doc = linuxXvfbResults();
  const r = doc.rows.find((x) => x.setup.id === "runner");
  r.diagnostics = { ...r.diagnostics, ...measured, ...win };
  const res = validateResults(doc);
  assert.equal(res.valid, true, JSON.stringify(res.errors.slice(0, 5)));
});

test("the report renders the runner's diagnostics, labelled non-comparable, and a third party's never", () => {
  const doc = linuxXvfbResults();
  const runner = doc.rows.find((x) => x.setup.id === "runner");
  runner.diagnostics = {
    "ui_bridge.reachability": { comparable: false, source: "POST /ui-bridge/control/page/evaluate 1+1", state: "measured", unit: "ms", value: 12, data: { answered: 2 } },
    "spawn.spans": { comparable: false, source: "runner log", state: "measured", data: { pty_open: { n: 5, p50_ms: 3.2, max_ms: 9 } } },
    ...derivedRunnerDiagnostics({ "transport.counters": { comparable: false, source: "x", state: "measured", data: SNAPSHOT } }, { platform: "linux" }),
  };
  const md = renderReport(doc);
  assert.match(md, /^### Runner diagnostics \(non-comparable\)$/m);
  assert.match(md, /^ {2}- `ui_bridge\.reachability`: 12\.0 ms$/m);
  assert.match(md, /^ {2}- `spawn\.spans`: \{pty_open\}$/m);
  assert.match(md, /^ {2}- `transport\.rates`: since_reset_ms 20000, reader_bytes_per_sec 200000, webview_events_per_sec 100, bytes_per_webview_event 2000$/m);
  assert.match(md, /^ {2}- `renderer\.memory`: not_applicable\(not_applicable_to_environment\)/m);
  const diagBlock = md.split("### Runner diagnostics (non-comparable)")[1].split("\n### ")[0];
  assert.doesNotMatch(diagBlock, /tmux/);
  assert.equal(summarizeDiagnostic(undefined), "missing");
});

// ---------------------------------------------------------------------------
// the scheduled job's verdict step, executed
// ---------------------------------------------------------------------------

const REPO = join(HERE, "..", "..", "..");
const WORKFLOW = join(REPO, ".github", "workflows", "termhost-bench.yml");

/** A job's verdict step `run:` script, dedented. */
function verdictStepScript(job) {
  const lines = readFileSync(WORKFLOW, "utf8").split(/\r?\n/);
  const start = lines.findIndex((l) => l === `  ${job}:`);
  assert.ok(start > 0, `the workflow has no job ${job}`);
  const at = lines.findIndex((l, i) => i > start && l === "      - name: Regression verdict (fails on regression only)");
  const next = lines.findIndex((l, i) => i > start && /^ {2}[A-Za-z0-9_-]+:\s*$/.test(l));
  assert.ok(at > 0 && (next < 0 || at < next), `the ${job} job has the verdict step`);
  const run = lines.findIndex((l, i) => i > at && l === "        run: |");
  const body = [];
  for (let i = run + 1; i < lines.length && (lines[i] === "" || /^ {10}/.test(lines[i])); i += 1) body.push(lines[i].slice(10));
  while (body.at(-1) === "") body.pop();
  return body.join("\n");
}

const JOBS = { "linux-xvfb": "run-linux-xvfb", "windows-hosted": "run-windows-hosted" };

test("the retention claim's anchor (rtp-terminal-regression-retention-instrument) matches each scheduled job's verdict step", () => {
  const yml = readFileSync(WORKFLOW, "utf8");
  const lines = yml.split(/\r?\n/);
  const hits = lines.filter((l) => /compare\.mjs.*--baseline/.test(l) && !/^\s*#/.test(l));
  assert.equal(hits.length, 2, "one executed line per scheduled job, not only a comment");
  for (const [env, job] of Object.entries(JOBS)) {
    const start = lines.findIndex((l) => l === `  ${job}:`);
    const next = lines.findIndex((l, i) => i > start && /^ {2}[A-Za-z0-9_-]+:\s*$/.test(l));
    const inJob = lines.slice(start, next < 0 ? undefined : next).filter((l) => hits.includes(l));
    assert.equal(inJob.length, 1, job);
    assert.match(inJob[0], new RegExp(`node scripts/termhost-bench/compare\\.mjs "\\$redacted" --baseline scripts/termhost-bench/baselines/${env}\\.json `));
  }
});

const bash = spawnSync("bash", ["-c", "exit 0"]).status === 0;

test("the verdict steps, executed: unknown passes, regression fails, and no verdict file is an error (mutations M8, S1)", { skip: !bash && "no bash on this machine" }, () => {
  const root = mkdtempSync(join(tmpdir(), "tb-step-"));
  try {
    // The step runs in the head checkout; give it the scripts it calls and nothing else.
    const repo = join(root, "repo");
    for (const p of ["scripts/termhost-bench", "scripts/lib", "scripts/tui-repaint-generator.mjs"]) {
      cpSync(join(REPO, p), join(repo, p), { recursive: true, filter: (src) => !src.includes("__tests__") && !src.endsWith(join("termhost-bench", "baselines")) });
    }
    const compareJs = join(repo, "scripts", "termhost-bench", "compare.mjs");
    const realCompare = readFileSync(compareJs, "utf8");
    for (const [env, job] of Object.entries(JOBS)) {
      const script = verdictStepScript(job);
      assert.match(script, /^set -uo pipefail$/m, "no -e: the step decides its own exit");
      const runStep = (name, { baseline, results, baselineFileContent, compareSource = realCompare }) => {
        const tmp = join(root, `${env}-${name}`);
        mkdirSync(join(tmp, "tb-out"), { recursive: true });
        const bdir = join(repo, "scripts", "termhost-bench", "baselines");
        rmSync(bdir, { recursive: true, force: true });
        if (baselineFileContent) {
          mkdirSync(bdir, { recursive: true });
          writeFileSync(join(bdir, `${env}.json`), JSON.stringify(baselineFileContent));
        }
        writeFileSync(compareJs, compareSource);
        if (results) {
          const doc = structuredClone(results);
          doc.machine.environment = env;
          writeFileSync(join(tmp, "tb-out", `termhost-bench-${env}-runner.json`), JSON.stringify(doc));
        }
        const summary = join(tmp, "summary.md");
        const r = spawnSync("bash", ["-c", script], {
          cwd: repo,
          env: { ...process.env, RUNNER_TEMP: tmp, GITHUB_STEP_SUMMARY: summary, BASELINE: baseline },
          encoding: "utf8",
        });
        return { status: r.status, out: `${r.stdout}${r.stderr}`, summary: existsSync(summary) ? readFileSync(summary, "utf8") : "", dir: join(tmp, "tb-out") };
      };
      const runs = SCHEDULED_RUNS[env];
      const regressing = abDoc([80, 82, 84].slice(0, runs), [50, 52, 54].slice(0, runs));
      const bfile = { ...baselineFile(), environment: env, min_runs: runs, thresholds: { output_latency_ms: { relative: PROPOSED_GATE_RELATIVE[runs], absolute: 5 } } };

      const noFile = runStep("no-file", { baseline: BASE, results: regressing });
      assert.equal(noFile.status, 0, `${env}: ${noFile.out}`);
      assert.match(noFile.summary, /^Verdict: \*\*unknown\*\* — no baseline\./m, env);
      const proposed = join(noFile.dir, `termhost-bench-${env}-baseline-proposed.json`);
      assert.ok(existsSync(proposed), `${env}: the first run proposes a baseline`);
      assert.deepEqual(validateBaseline(JSON.parse(readFileSync(proposed, "utf8"))), [], env);

      const noBuild = runStep("no-build", { baseline: "none", results: regressing, baselineFileContent: bfile });
      assert.equal(noBuild.status, 0, `${env}: ${noBuild.out}`);
      assert.match(noBuild.summary, /unknown — no baseline build in this run/);

      const fires = runStep("fires", { baseline: BASE, results: regressing, baselineFileContent: bfile });
      assert.equal(fires.status, 1, `${env}: ${fires.out}`);
      assert.match(fires.out, /::error::termhost-bench: the head build regressed/);
      assert.equal(JSON.parse(readFileSync(join(fires.dir, `termhost-bench-${env}-verdict.json`), "utf8")).verdict, "regression");

      const unknownSide = runStep("unknown-side", { baseline: BASE, results: abDoc([500, 510, 520].slice(0, runs), [null, null, null].slice(0, runs)), baselineFileContent: bfile });
      assert.equal(unknownSide.status, 0, `${env}: ${unknownSide.out}`);

      // The wedge case: the head lost N = 15. The gate is unknown, and an unknown never fails the step
      // (the job's assert step is what fails on an unmeasured output_latency_ms at the gate N).
      const wedged = runStep("wedged", { baseline: BASE, results: abDoc([50, 52, 54].slice(0, runs), [50, 52, 54].slice(0, runs), { at: (n) => (n === 15 ? { head: [null, null, null].slice(0, runs) } : null) }), baselineFileContent: bfile });
      assert.equal(wedged.status, 0, `${env}: ${wedged.out}`);
      assert.equal(JSON.parse(readFileSync(join(wedged.dir, `termhost-bench-${env}-verdict.json`), "utf8")).verdict, "unknown");
      assert.match(wedged.summary, /head_unknown_where_baseline_measured: N=15 focus, N=15 tiled/);
      assert.match(
        wedged.out,
        /^::warning::termhost-bench: the head build lost readings the baseline build measured \(head_unknown_where_baseline_measured: N=15 focus, N=15 tiled\); the regression gate is unknown, not passed$/m,
        `${env}: the annotation`,
      );
      // An unknown for any other reason carries no such annotation.
      assert.doesNotMatch(noFile.out, /::warning::termhost-bench: the head build lost/, env);

      const noResults = runStep("no-results", { baseline: BASE, results: null, baselineFileContent: bfile });
      assert.equal(noResults.status, 0, `${env}: ${noResults.out}`);
      assert.match(noResults.summary, /unknown — no runner-only results/);

      // S1: a compare that runs nothing and writes no verdict is an error, never "no regression".
      const silent = runStep("silent", { baseline: BASE, results: regressing, baselineFileContent: bfile, compareSource: "// does nothing\n" });
      assert.equal(silent.status, 2, `${env}: ${silent.out}`);
      assert.match(silent.out, /::error::termhost-bench: compare wrote no verdict \(exit 0\): the regression detector did not run/);
    }
    writeFileSync(compareJs, realCompare);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

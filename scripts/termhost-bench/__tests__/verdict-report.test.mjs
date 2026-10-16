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
import { cpSync, existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import {
  BASELINE_SCHEMA_ID,
  HEADLINE_KPIS,
  NO_BASELINE,
  applyVerdict,
  compareBuilds,
  judgeEntry,
  proposeBaseline,
  readBaselineFile,
  renderComparison,
  validateBaseline,
  verdictFails,
} from "../compare.mjs";
import { HEADLINE_LINE, headlineStatus, oneEnvironment, renderReport } from "../report.mjs";
import { derivedRunnerDiagnostics, summarizeDiagnostic, transportRates } from "../diagnostics.mjs";
import { main } from "../cli.mjs";
import { measureKpi, notApplicableKpi, unknownKpi } from "../stats.mjs";
import { validateResults } from "../validate.mjs";
import { linuxXvfbResults, measuredKpis, row, spread, windowsDesktopResults } from "./helpers/results.mjs";

const HERE = dirname(fileURLToPath(import.meta.url));
const BASELINES_DIR = join(HERE, "..", "baselines");
const HEAD = "a".repeat(40);
const BASE = "b".repeat(40);

/** A runner row at N = 5 focus whose output_latency_ms p95 is `p95`, or unknown when null. */
function runnerRow(role, commit, runIndex, p95) {
  return row({
    setupOverrides: { build: { commit, source: "artifact", role }, launch_mode: "subject_instance_root", pty_host_impl: "unix_pty", session_shell: "bash" },
    scenarioOverrides: { n: 5, arm: "focus", v_target: 1, v_actual: 1, run_index: runIndex, run_count: 3 },
    kpis: measuredKpis({
      output_latency_ms: p95 === null ? unknownKpi("output_latency_ms", "capture_unavailable", { detail: "no frames" }) : latencyWithP95(p95),
    }),
  });
}

/** An output_latency_ms value whose p95 is exactly `p95` (600 identical samples). */
function latencyWithP95(p95) {
  return measureKpi("output_latency_ms", Array.from({ length: 600 }, () => p95));
}

/** A same-VM A/B document: head and baseline output_latency_ms p95 per run. */
function abDoc(headP95s, baseP95s) {
  const doc = linuxXvfbResults();
  doc.rows = [
    ...headP95s.map((v, i) => runnerRow("head", HEAD, i, v)),
    ...baseP95s.map((v, i) => runnerRow("baseline", BASE, i, v)),
  ];
  return doc;
}

/** A usable baseline file for linux-xvfb: output_latency_ms worsens beyond 20 % or 5 ms. */
function baselineFile(over = {}) {
  return {
    schema: BASELINE_SCHEMA_ID,
    environment: "linux-xvfb",
    setup: "runner",
    variant: "default",
    recorded_from: { run_id: "fixture", recorded_at: "2026-10-16T00:00:00Z", baseline_commit: BASE, head_commit: HEAD },
    min_runs: 2,
    thresholds: { output_latency_ms: { relative: 0.2, absolute: 5 } },
    ...over,
  };
}

const present = (b = baselineFile()) => ({ state: "present", baseline: b });
const lat = (cmp) => cmp.entries.find((e) => e.kpi === "output_latency_ms" && e.n === 5 && e.arm === "focus");

// ---------------------------------------------------------------------------
// compare: the verdict
// ---------------------------------------------------------------------------

test("a synthetic regression fires: beyond the threshold and spreads disjoint (mutation M2)", () => {
  const cmp = applyVerdict(compareBuilds(abDoc([80, 82, 84], [50, 52, 54])), present());
  assert.equal(lat(cmp).verdict.verdict, "regression");
  assert.equal(cmp.verdict.verdict, "regression");
  assert.equal(verdictFails(cmp.verdict), true);
  assert.match(renderComparison(cmp), /^Verdict: \*\*regression\*\*/m);
  assert.match(renderComparison(cmp), /^\| output_latency_ms \| p95 \| 5 \| focus \| .+ \| \*\*regression\*\* \|$/m);
});

test("an improvement is the symmetric case, and never fails the job", () => {
  const cmp = applyVerdict(compareBuilds(abDoc([30, 31, 32], [50, 52, 54])), present());
  assert.equal(lat(cmp).verdict.verdict, "improvement");
  assert.equal(cmp.verdict.verdict, "improvement");
  assert.equal(verdictFails(cmp.verdict), false);
});

test("overlapping run spreads suppress a regression even beyond the threshold (mutation M3)", () => {
  // Head median 80 is 54 % worse than 52, but one head run (53) falls inside the baseline spread [50, 54].
  const cmp = applyVerdict(compareBuilds(abDoc([53, 80, 84], [50, 52, 54])), present());
  assert.deepEqual([lat(cmp).verdict.verdict, lat(cmp).verdict.reason], ["no_change", "spreads_overlap"]);
  assert.equal(verdictFails(cmp.verdict), false);
});

test("a worsening within the threshold is no_change, even with disjoint spreads", () => {
  // 56 vs 52: worse by 4 ms, under max(20 % x 52, 5) = 10.4.
  const cmp = applyVerdict(compareBuilds(abDoc([55, 56, 57], [50, 52, 54])), present());
  assert.deepEqual([lat(cmp).verdict.verdict, lat(cmp).verdict.reason], ["no_change", "within_threshold"]);
});

test("an unknown side never fires, however bad the other looks; unknown runs are never folded in as numbers (mutation M1)", () => {
  const baseUnknown = applyVerdict(compareBuilds(abDoc([500, 510, 520], [null, null, null])), present());
  assert.deepEqual([lat(baseUnknown).verdict.verdict, lat(baseUnknown).verdict.reason], ["unknown", "baseline_unknown"]);
  assert.notEqual(baseUnknown.verdict.verdict, "regression");
  assert.equal(verdictFails(baseUnknown.verdict), false);
  const headUnknown = applyVerdict(compareBuilds(abDoc([null, null, null], [50, 52, 54])), present());
  assert.deepEqual([lat(headUnknown).verdict.verdict, lat(headUnknown).verdict.reason], ["unknown", "head_unknown"]);
  assert.equal(verdictFails(headUnknown.verdict), false);
});

test("an under-sampled side never fires: fewer measured runs than the baseline's min_runs (mutation M4)", () => {
  const cmp = applyVerdict(compareBuilds(abDoc([500, null, null], [50, 52, 54])), present());
  assert.deepEqual([lat(cmp).verdict.verdict, lat(cmp).verdict.reason], ["unknown", "head_under_sampled"]);
  assert.equal(verdictFails(cmp.verdict), false);
  const base = applyVerdict(compareBuilds(abDoc([500, 510, 520], [50, null, null])), present());
  assert.deepEqual([lat(base).verdict.verdict, lat(base).verdict.reason], ["unknown", "baseline_under_sampled"]);
  const strict = applyVerdict(compareBuilds(abDoc([80, 82, 84], [50, 52, 54])), present(baselineFile({ min_runs: 4 })));
  assert.equal(lat(strict).verdict.reason, "head_under_sampled", "min_runs comes from the baseline file");
});

test("a headline KPI with no recorded threshold is unknown; a non-headline KPI is not judged at all", () => {
  const cmp = applyVerdict(compareBuilds(abDoc([80, 82, 84], [50, 52, 54])), present());
  const cpu = cmp.entries.find((e) => e.kpi === "cpu_pct");
  assert.deepEqual([cpu.verdict.verdict, cpu.verdict.reason], ["unknown", "no_threshold"]);
  for (const e of cmp.entries) assert.equal(e.verdict !== undefined, HEADLINE_KPIS.includes(e.kpi), e.kpi);
  assert.ok(!HEADLINE_KPIS.includes("frame_interval_ms"));
});

test("a higher-is-better KPI regresses downwards (direction from the catalog)", () => {
  const e = { kpi: "x", direction: "higher_is_better", head: { spread: { median: 0.5, min: 0.4, max: 0.6, runs: 3 } }, baseline: { spread: { median: 0.99, min: 0.98, max: 1, runs: 3 } } };
  assert.equal(judgeEntry(e, { relative: 0.1 }, 2).verdict, "regression");
  assert.equal(judgeEntry({ ...e, direction: "neutral" }, { relative: 0.1 }, 2).verdict, "unknown");
});

test("no baseline file: every headline verdict is unknown(no baseline), and nothing fails", () => {
  const dir = mkdtempSync(join(tmpdir(), "tb-verdict-"));
  try {
    const missing = readBaselineFile(join(dir, "linux-xvfb.json"));
    assert.deepEqual(missing, { state: "absent", reason: NO_BASELINE });
    const cmp = applyVerdict(compareBuilds(abDoc([800, 820, 840], [50, 52, 54])), missing);
    assert.deepEqual([cmp.verdict.verdict, cmp.verdict.reason], ["unknown", "no baseline"]);
    assert.ok(cmp.entries.filter((e) => e.verdict).every((e) => e.verdict.verdict === "unknown" && e.verdict.reason === "no baseline"));
    assert.equal(verdictFails(cmp.verdict), false);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("the CLI: a missing baseline file exits 0 with unknown; a regression exits 1; the verdict JSON says which (mutation M5)", async () => {
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
    assert.deepEqual(JSON.parse(readFileSync(vj, "utf8")).verdict, "unknown");
    assert.equal(JSON.parse(readFileSync(vj, "utf8")).reason, "no baseline");
    assert.match(out.join(""), /^Verdict: \*\*unknown\*\* — no baseline/m);

    const bf = join(dir, "linux-xvfb.json");
    writeFileSync(bf, JSON.stringify(baselineFile()));
    assert.equal(await main(["compare", results, "--baseline", bf, "--verdict-json", vj], io), 1);
    assert.equal(JSON.parse(readFileSync(vj, "utf8")).verdict, "regression");

    // A run with no baseline build at all: unknown, exit 0.
    const headOnly = join(dir, "head-only.json");
    const doc = abDoc([80, 82, 84], []);
    writeFileSync(headOnly, JSON.stringify(doc));
    assert.equal(await main(["compare", headOnly, "--baseline", bf, "--verdict-json", vj], io), 0);
    assert.equal(JSON.parse(readFileSync(vj, "utf8")).reason, "no comparison");

    // A corrupt baseline file: unknown, exit 0 (the PR-time test below keeps a committed one valid).
    writeFileSync(bf, "{");
    assert.equal(await main(["compare", results, "--baseline", bf, "--verdict-json", vj], io), 0);
    assert.match(JSON.parse(readFileSync(vj, "utf8")).reason, /^baseline is not JSON/);

    assert.equal(await main(["compare", results, "--verdict-json", vj], io), 2, "--verdict-json needs --baseline");
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("a baseline for another environment or setup judges nothing", () => {
  const cmp = applyVerdict(compareBuilds(abDoc([80, 82, 84], [50, 52, 54])), present(baselineFile({ environment: "windows-hosted" })));
  assert.equal(cmp.verdict.verdict, "unknown");
  assert.match(cmp.verdict.reason, /^baseline mismatch: the file is for windows-hosted/);
});

test("validateBaseline refuses what would make the gate lie", () => {
  assert.deepEqual(validateBaseline(baselineFile()), []);
  const errs = (over) => validateBaseline(baselineFile(over)).join("\n");
  assert.match(errs({ setup: "tmux" }), /runner's own readings only/);
  assert.match(errs({ min_runs: 1 }), /min_runs/);
  assert.match(errs({ thresholds: { frame_interval_ms: { relative: 0.2 } } }), /not a headline KPI/);
  assert.match(errs({ thresholds: { output_latency_ms: {} } }), /needs relative and\/or absolute/);
  assert.match(errs({ thresholds: { output_latency_ms: { relative: 0 } } }), /zero threshold/);
  assert.match(errs({ thresholds: { output_latency_ms: { relative: -1 } } }), /must be a number >= 0/);
  assert.match(errs({ schema: "x" }), /schema must be/);
});

test("a proposed baseline holds the baseline build's runner readings, validates, and needs a proven baseline commit", () => {
  const cmp = compareBuilds(abDoc([80, 82, 84], [50, 52, 54]));
  const b = proposeBaseline(cmp, { runId: "ci-1-1", recordedAt: "2026-10-16T00:00:00Z" });
  assert.deepEqual(validateBaseline(b), []);
  assert.equal(b.recorded_from.baseline_commit, BASE);
  assert.deepEqual(Object.keys(b.thresholds).sort(), [...HEADLINE_KPIS].sort());
  const r = b.readings.find((x) => x.kpi === "output_latency_ms");
  assert.deepEqual([r.median, r.min, r.max, r.runs], [52, 50, 54, 3]);
  assert.ok(!JSON.stringify(b).includes("tmux"), "runner rows only (D7)");
  assert.throws(() => proposeBaseline({ ...cmp, baseline_commit: null }), /D12/);
  // Judged against itself it is consistent: the proposal's own thresholds accept the same-VM regression.
  assert.equal(applyVerdict(compareBuilds(abDoc([80, 82, 84], [50, 52, 54])), present(b)).verdict.verdict, "regression");
});

test("every committed baseline file is valid (a corrupt one would silence the scheduled gate)", { skip: !existsSync(BASELINES_DIR) && "no committed baseline yet: deferred to the first scheduled linux-xvfb run" }, () => {
  const files = readdirSync(BASELINES_DIR).filter((f) => f.endsWith(".json"));
  assert.ok(files.length > 0, "baselines/ exists but holds no baseline");
  for (const f of files) {
    const st = readBaselineFile(join(BASELINES_DIR, f));
    assert.equal(st.state, "present", `${f}: ${st.reason}`);
    assert.equal(`${st.baseline.environment}.json`, f, "a baseline file is named for its environment");
  }
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

/** The verdict step's `run:` script, dedented. */
function verdictStepScript() {
  const lines = readFileSync(WORKFLOW, "utf8").split(/\r?\n/);
  const at = lines.findIndex((l) => l === "      - name: Regression verdict (fails on regression only)");
  assert.ok(at > 0, "the run-linux-xvfb job has the verdict step");
  const run = lines.findIndex((l, i) => i > at && l === "        run: |");
  const body = [];
  for (let i = run + 1; i < lines.length && (lines[i] === "" || /^ {10}/.test(lines[i])); i += 1) body.push(lines[i].slice(10));
  while (body.at(-1) === "") body.pop();
  return body.join("\n");
}

test("the retention claim's anchor (rtp-terminal-regression-retention-instrument) matches the scheduled job", () => {
  const yml = readFileSync(WORKFLOW, "utf8");
  const hits = yml.split(/\r?\n/).filter((l) => /compare\.mjs.*--baseline/.test(l) && !/^\s*#/.test(l));
  assert.equal(hits.length, 1, "exactly one executed line, not only a comment");
  assert.match(hits[0], /node scripts\/termhost-bench\/compare\.mjs "\$redacted" --baseline scripts\/termhost-bench\/baselines\/linux-xvfb\.json /);
  const lines = yml.split(/\r?\n/);
  const job = lines.findIndex((l) => l === "  run-linux-xvfb:");
  const next = lines.findIndex((l, i) => i > job && /^ {2}[A-Za-z0-9_-]+:\s*$/.test(l));
  const idx = lines.findIndex((l) => l === hits[0]);
  assert.ok(idx > job && idx < next, "the line is in the run-linux-xvfb job (the one the schedule runs)");
});

const bash = spawnSync("bash", ["-c", "exit 0"]).status === 0;

test("the verdict step, executed: unknown passes (no baseline file, no baseline build), regression fails (mutation M8)", { skip: !bash && "no bash on this machine" }, () => {
  const script = verdictStepScript();
  assert.match(script, /^set -uo pipefail$/m, "no -e: the step decides its own exit");
  const root = mkdtempSync(join(tmpdir(), "tb-step-"));
  try {
    // The step runs in the head checkout; give it the scripts it calls and nothing else.
    const repo = join(root, "repo");
    for (const p of ["scripts/termhost-bench", "scripts/lib", "scripts/tui-repaint-generator.mjs"]) {
      cpSync(join(REPO, p), join(repo, p), { recursive: true, filter: (src) => !src.includes("__tests__") && !src.endsWith(join("termhost-bench", "baselines")) });
    }
    const runStep = (name, { baseline, results, baselineFileContent }) => {
      const tmp = join(root, name);
      mkdirSync(join(tmp, "tb-out"), { recursive: true });
      const bdir = join(repo, "scripts", "termhost-bench", "baselines");
      rmSync(bdir, { recursive: true, force: true });
      if (baselineFileContent) {
        mkdirSync(bdir, { recursive: true });
        writeFileSync(join(bdir, "linux-xvfb.json"), JSON.stringify(baselineFileContent));
      }
      if (results) writeFileSync(join(tmp, "tb-out", "termhost-bench-linux-xvfb-runner.json"), JSON.stringify(results));
      const summary = join(tmp, "summary.md");
      const r = spawnSync("bash", ["-c", script], {
        cwd: repo,
        env: { ...process.env, RUNNER_TEMP: tmp, GITHUB_STEP_SUMMARY: summary, BASELINE: baseline },
        encoding: "utf8",
      });
      return { status: r.status, out: `${r.stdout}${r.stderr}`, summary: existsSync(summary) ? readFileSync(summary, "utf8") : "", dir: join(tmp, "tb-out") };
    };
    const regressing = abDoc([80, 82, 84], [50, 52, 54]);

    const noFile = runStep("no-file", { baseline: BASE, results: regressing });
    assert.equal(noFile.status, 0, noFile.out);
    assert.match(noFile.summary, /^Verdict: \*\*unknown\*\* — no baseline\./m);
    assert.ok(existsSync(join(noFile.dir, "termhost-bench-linux-xvfb-baseline-proposed.json")), "the first run proposes a baseline");
    assert.deepEqual(validateBaseline(JSON.parse(readFileSync(join(noFile.dir, "termhost-bench-linux-xvfb-baseline-proposed.json"), "utf8"))), []);

    const noBuild = runStep("no-build", { baseline: "none", results: regressing, baselineFileContent: baselineFile() });
    assert.equal(noBuild.status, 0, noBuild.out);
    assert.match(noBuild.summary, /unknown — no baseline build in this run/);

    const fires = runStep("fires", { baseline: BASE, results: regressing, baselineFileContent: baselineFile() });
    assert.equal(fires.status, 1, fires.out);
    assert.match(fires.out, /::error::termhost-bench: the head build regressed/);
    assert.equal(JSON.parse(readFileSync(join(fires.dir, "termhost-bench-linux-xvfb-verdict.json"), "utf8")).verdict, "regression");

    const unknownSide = runStep("unknown-side", { baseline: BASE, results: abDoc([500, 510, 520], [null, null, null]), baselineFileContent: baselineFile() });
    assert.equal(unknownSide.status, 0, unknownSide.out);

    const noResults = runStep("no-results", { baseline: BASE, results: null, baselineFileContent: baselineFile() });
    assert.equal(noResults.status, 0, noResults.out);
    assert.match(noResults.summary, /unknown — no runner-only results/);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

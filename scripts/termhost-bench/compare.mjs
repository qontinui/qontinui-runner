/**
 * termhost-bench `compare`: the runner's head build against its baseline build,
 * per KPI, N and arm — each side's median over its runs, the run spread [min, max],
 * and how many runs were measured (and why the others were not) — and, for the
 * headline KPIs, the regression verdict (plan Phase 6).
 *
 * Plan `2026-10-11-terminal-page-performance-kpis-across-setups`. Phase 3 built the
 * table; Phase 6 adds the verdict here (no second module). For each headline KPI
 * (every catalog row with a `headline_statistic`) at each N and arm:
 *
 *   - **regression** iff the head build's median-of-runs is worse than the baseline
 *     build's by more than the threshold the baseline FILE records for that KPI
 *     (`max(relative x |baseline median|, absolute)`), AND the two run spreads
 *     [min, max] do not overlap;
 *   - **improvement** symmetrically;
 *   - **no_change** when both sides are measured and sampled but either condition
 *     fails (`within_threshold` / `spreads_overlap`);
 *   - **unknown** whenever either side has no measured run, fewer measured runs
 *     than the baseline file's `min_runs` (under-sampled), the KPI has no recorded
 *     threshold, or there is no usable baseline file at all ("no baseline").
 *
 * The two sides are the same-VM A/B (D11): head and baseline builds measured in one
 * run, interleaved. The baseline file (`baselines/<environment>.json`,
 * `termhost-bench-baseline/v1`) supplies the thresholds and the minimum run count,
 * and records the readings it was proposed from (runner rows only, D7). An unknown
 * never fires the detector [policy: verification-and-evidence/an-unknown-input-must-
 * not-fire-a-detector]: the overall verdict is `regression` only when a judged
 * entry is one, and the scheduled job fails only on that.
 *
 * An unknown value is never folded into a median: a side with no measured run reads
 * `unknown`, with its reasons.
 *
 * Run directly, `node scripts/termhost-bench/compare.mjs <args>` is
 * `cli.mjs compare <args>` (the scheduled job calls it this way).
 *
 * Inputs: one results file holding both builds (the same-VM A/B, D11 — rows with
 * `setup.build.role` `head` and `baseline`), or two files (head first). Two files
 * from different environments are refused (D6).
 */

import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { ARMS, ENVIRONMENTS, KPI_CATALOG, getKpi } from "./kpi-catalog.mjs";
import { percentile } from "./stats.mjs";

/**
 * Row labels that keep a row out of every comparison, and so out of the verdict
 * Phase 6 draws here: VS Code rows of a linux-xvfb run fell back from WebGL to the
 * DOM renderer (D6, Phase 4) and describe a configuration no operator runs.
 */
export const VERDICT_EXCLUDED_LABELS = Object.freeze(["dom_renderer_fallback"]);

/** The KPIs compared: the host's own, comparable ones. */
export const COMPARED_KPIS = Object.freeze(
  KPI_CATALOG.filter((k) => k.comparable && k.attribution === "host").map((k) => k.id),
);

/**
 * The statistic a KPI is compared on, as a label: its headline one, else (a
 * value-shape KPI) its one statistic, else p95, else its first.
 */
export function compareStatistic(kpi) {
  if (kpi.headline_statistic) return kpi.headline_statistic;
  if (kpi.shape === "value") return kpi.statistics[0];
  return kpi.statistics.includes("p95") ? "p95" : kpi.statistics[0];
}

/**
 * The number one KPI value contributes, or null when it is not measured: a
 * value-shape KPI's `value` (whatever its statistic is called — `count` for
 * `hangs`, `share`, `slope`), a stats-shape KPI's compared statistic.
 */
export function valueOf(kpiValue, kpi) {
  if (kpiValue?.state !== "measured") return null;
  const v = kpi.shape === "value" ? kpiValue.value : kpiValue.stats?.[compareStatistic(kpi)];
  return typeof v === "number" && Number.isFinite(v) ? v : null;
}

/** median / min / max of a non-empty list (nearest-rank median: an observed value). */
export function spreadOf(values) {
  if (values.length === 0) return null;
  const m = percentile(values, 50);
  return {
    median: m.state === "measured" ? m.value : values[0],
    min: Math.min(...values),
    max: Math.max(...values),
    runs: values.length,
  };
}

/** Summarise one side's states: `2 measured, 1 unknown(capture_unavailable)`. */
export function statesOf(values) {
  const counts = new Map();
  for (const v of values) {
    const key = v?.state === "measured" ? "measured" : `${v?.state ?? "missing"}(${v?.reason ?? "?"})`;
    counts.set(key, (counts.get(key) ?? 0) + 1);
  }
  return [...counts.entries()].map(([k, c]) => `${c} ${k}`).join(", ");
}

/**
 * Whether a row's build lets it be one side of a comparison (D12). A runner row
 * needs a PROVEN build — one with no `state` at all: an unproven build
 * (`unknown`, unprovenBuild's marker) and any absence, `not_applicable` included,
 * are left out (rowLabels gives every one of those `unknown_build`). A third-party
 * setup has no build of ours to prove: its `not_applicable` stays comparable, an
 * `unknown` does not.
 */
export function comparableBuild(row) {
  const state = row.setup.build?.state;
  return row.setup.id === "runner" ? state === undefined : state !== "unknown";
}

/**
 * Pair head and baseline rows.
 *
 * @param {object} head results document (or the one holding both builds)
 * @param {object} [baseline] a second document; omitted = take both roles from `head`
 * @param {{setup?: string, variant?: string}} [o]
 */
export function compareBuilds(head, baseline = null, { setup = "runner", variant = "default" } = {}) {
  if (baseline && head.machine?.environment !== baseline.machine?.environment) {
    throw new Error(
      `refusing to compare ${head.machine?.environment} with ${baseline.machine?.environment}: numbers compare only within one environment (D6)`,
    );
  }
  const pick = (doc, roles) =>
    doc.rows.filter(
      (r) =>
        r.setup.id === setup &&
        r.setup.variant === variant &&
        comparableBuild(r) &&
        roles.includes(r.setup.build?.role ?? "single") &&
        !(r.labels ?? []).some((l) => VERDICT_EXCLUDED_LABELS.includes(l)),
    );
  const headRows = baseline ? pick(head, ["head", "single"]) : pick(head, ["head"]);
  const baseRows = baseline ? pick(baseline, ["baseline", "single"]) : pick(head, ["baseline"]);
  if (headRows.length === 0) throw new Error(`no ${setup}/${variant} head rows to compare`);
  if (baseRows.length === 0) throw new Error(`no ${setup}/${variant} baseline rows to compare (run with --ab-baseline)`);
  const cells = new Map();
  const add = (rows, side) => {
    for (const r of rows) {
      const key = `${r.scenario.n}|${r.scenario.arm}`;
      if (!cells.has(key)) cells.set(key, { n: r.scenario.n, arm: r.scenario.arm, head: [], baseline: [] });
      cells.get(key)[side].push(r);
    }
  };
  add(headRows, "head");
  add(baseRows, "baseline");
  const commit = (rows) => rows.find((r) => r.setup.build?.commit)?.setup.build.commit ?? null;
  const out = {
    environment: head.machine?.environment ?? null,
    setup,
    variant,
    head_commit: commit(headRows),
    baseline_commit: commit(baseRows),
    entries: [],
  };
  const levels = [...cells.values()].sort((a, b) => a.n - b.n || a.arm.localeCompare(b.arm));
  for (const id of COMPARED_KPIS) {
    const kpi = getKpi(id);
    for (const cell of levels) {
      const side = (rows) => {
        const raw = rows.map((r) => r.kpis[id]);
        const nums = raw.map((v) => valueOf(v, kpi)).filter((v) => v !== null);
        return { spread: spreadOf(nums), states: statesOf(raw), total: rows.length };
      };
      out.entries.push({
        kpi: id,
        statistic: compareStatistic(kpi),
        unit: kpi.unit,
        direction: kpi.direction,
        n: cell.n,
        arm: cell.arm,
        head: side(cell.head),
        baseline: side(cell.baseline),
      });
    }
  }
  return out;
}

// ---------------------------------------------------------------------------
// The regression verdict (Phase 6)
// ---------------------------------------------------------------------------

/** The baseline file's schema identifier. */
export const BASELINE_SCHEMA_ID = "termhost-bench-baseline/v1";

/** The KPIs the verdict judges: the headline ones (catalog `headline_statistic`). */
export const HEADLINE_KPIS = Object.freeze(KPI_CATALOG.filter((k) => k.headline_statistic).map((k) => k.id));

/** Fewer measured runs on a side than this and the side is under-sampled. */
export const MIN_RUNS_FLOOR = 2;

/** Verdicts an entry can carry. */
export const VERDICTS = Object.freeze(["regression", "improvement", "no_change", "unknown"]);

/** The reason every verdict reads when there is no usable baseline file. */
export const NO_BASELINE = "no baseline";

/**
 * The threshold a proposed baseline records per headline KPI, by unit: a relative
 * share of the baseline median, floored by an absolute amount (so a median near 0,
 * as `hangs` usually is, does not turn one extra event into a regression). A
 * proposal only: whoever commits the file reviews it against the run's spreads.
 */
export const PROPOSED_THRESHOLDS = Object.freeze({
  ms: Object.freeze({ relative: 0.25, absolute: 5 }),
  "%": Object.freeze({ relative: 0.25, absolute: 2 }),
  "% of one logical core": Object.freeze({ relative: 0.25, absolute: 5 }),
  MB: Object.freeze({ relative: 0.15, absolute: 25 }),
  count: Object.freeze({ relative: 0, absolute: 1 }),
});

const isNum = (x) => typeof x === "number" && Number.isFinite(x);

/**
 * Structural check of a baseline document.
 *
 * @returns {string[]} errors (empty = usable)
 */
export function validateBaseline(b) {
  const errors = [];
  if (!b || typeof b !== "object" || Array.isArray(b)) return ["the baseline is not a JSON object"];
  if (b.schema !== BASELINE_SCHEMA_ID) errors.push(`schema must be ${BASELINE_SCHEMA_ID}, got ${JSON.stringify(b.schema)}`);
  if (!ENVIRONMENTS.includes(b.environment)) errors.push(`environment must be one of ${ENVIRONMENTS.join(", ")}`);
  if (b.setup !== "runner") errors.push("setup must be runner: a baseline holds the runner's own readings only (D7)");
  if (typeof b.variant !== "string" || b.variant.length === 0) errors.push("variant must be a non-empty string");
  if (!Number.isInteger(b.min_runs) || b.min_runs < MIN_RUNS_FLOOR) errors.push(`min_runs must be an integer >= ${MIN_RUNS_FLOOR} (one run has no spread)`);
  if (!b.thresholds || typeof b.thresholds !== "object" || Array.isArray(b.thresholds)) {
    errors.push("thresholds must be an object keyed by headline KPI id");
  } else {
    for (const [id, t] of Object.entries(b.thresholds)) {
      if (!HEADLINE_KPIS.includes(id)) errors.push(`thresholds.${id}: not a headline KPI (${HEADLINE_KPIS.join(", ")})`);
      if (!t || typeof t !== "object") {
        errors.push(`thresholds.${id}: must be {relative?, absolute?}`);
        continue;
      }
      const extra = Object.keys(t).filter((k) => k !== "relative" && k !== "absolute");
      if (extra.length > 0) errors.push(`thresholds.${id}: unknown field(s) ${extra.join(", ")}`);
      if (t.relative === undefined && t.absolute === undefined) errors.push(`thresholds.${id}: needs relative and/or absolute`);
      for (const k of ["relative", "absolute"]) {
        if (t[k] !== undefined && !(isNum(t[k]) && t[k] >= 0)) errors.push(`thresholds.${id}.${k}: must be a number >= 0`);
      }
      if (t.relative === 0 && (t.absolute ?? 0) === 0) errors.push(`thresholds.${id}: a zero threshold would call any worsening a regression`);
    }
  }
  if (b.readings !== undefined) {
    if (!Array.isArray(b.readings)) errors.push("readings must be an array");
    else {
      b.readings.forEach((r, i) => {
        if (!r || !KPI_CATALOG.some((k) => k.id === r.kpi)) errors.push(`readings[${i}].kpi: not a catalog KPI`);
        if (!Number.isInteger(r?.n) || !ARMS.includes(r?.arm)) errors.push(`readings[${i}]: needs an integer n and an arm`);
        if (!["median", "min", "max"].every((k) => isNum(r?.[k]))) errors.push(`readings[${i}]: needs numeric median, min and max`);
      });
    }
  }
  return errors;
}

/**
 * Read a baseline file. A missing file is not an error: it is the state before
 * the first scheduled run's baseline is committed, and every verdict then reads
 * unknown("no baseline").
 *
 * @returns {{state: "present", baseline: object} | {state: "absent" | "invalid", reason: string}}
 */
export function readBaselineFile(path, { readFile = readFileSync } = {}) {
  let text;
  try {
    text = readFile(path, "utf8");
  } catch (e) {
    if (e?.code === "ENOENT") return { state: "absent", reason: NO_BASELINE };
    return { state: "invalid", reason: `baseline unreadable: ${e.message}` };
  }
  let doc;
  try {
    doc = JSON.parse(text);
  } catch (e) {
    return { state: "invalid", reason: `baseline is not JSON: ${e.message}` };
  }
  const errors = validateBaseline(doc);
  if (errors.length > 0) return { state: "invalid", reason: `baseline invalid: ${errors.slice(0, 5).join("; ")}` };
  return { state: "present", baseline: doc };
}

/** The margin a KPI must worsen by before it can be a regression. */
export function thresholdMargin(threshold, baselineMedian) {
  return Math.max((threshold.relative ?? 0) * Math.abs(baselineMedian), threshold.absolute ?? 0);
}

/**
 * Judge one comparison entry.
 *
 * @param {object} e        a `compareBuilds` entry
 * @param {{relative?: number, absolute?: number} | undefined} threshold
 * @param {number} minRuns
 * @returns {{verdict: string, reason: string, detail?: string}}
 */
export function judgeEntry(e, threshold, minRuns) {
  const sign = e.direction === "lower_is_better" ? 1 : e.direction === "higher_is_better" ? -1 : 0;
  if (sign === 0) return { verdict: "unknown", reason: "no_direction" };
  if (!threshold) return { verdict: "unknown", reason: "no_threshold", detail: `the baseline file records no threshold for ${e.kpi}` };
  const h = e.head.spread;
  const b = e.baseline.spread;
  if (!h) return { verdict: "unknown", reason: "head_unknown", detail: e.head.states };
  if (!b) return { verdict: "unknown", reason: "baseline_unknown", detail: e.baseline.states };
  if (h.runs < minRuns) return { verdict: "unknown", reason: "head_under_sampled", detail: `${h.runs} measured run(s), fewer than ${minRuns}` };
  if (b.runs < minRuns) return { verdict: "unknown", reason: "baseline_under_sampled", detail: `${b.runs} measured run(s), fewer than ${minRuns}` };
  // `worse` > 0: the head is worse than the baseline, in the KPI's direction.
  const worse = sign * (h.median - b.median);
  const margin = thresholdMargin(threshold, b.median);
  const overlap = h.min <= b.max && b.min <= h.max;
  const detail = `head median ${h.median} [${h.min}, ${h.max}] vs baseline ${b.median} [${b.min}, ${b.max}], margin ${margin}`;
  if (Math.abs(worse) <= margin) return { verdict: "no_change", reason: "within_threshold", detail };
  if (overlap) return { verdict: "no_change", reason: "spreads_overlap", detail };
  return { verdict: worse > 0 ? "regression" : "improvement", reason: "beyond_threshold_and_spreads_disjoint", detail };
}

/** The overall verdict when no comparison could be drawn at all. */
export function unknownVerdict(reason, detail) {
  return { verdict: "unknown", reason, ...(detail ? { detail } : {}), counts: Object.fromEntries(VERDICTS.map((v) => [v, 0])) };
}

/**
 * Attach a verdict to every headline entry of a comparison, and the overall one.
 * Non-headline entries stay in the table, unjudged.
 *
 * @param {object} cmp       `compareBuilds` output
 * @param {{state: string, baseline?: object, reason?: string}} baselineState `readBaselineFile` output
 * @returns {object} cmp, with `entries[].verdict` and `verdict`
 */
export function applyVerdict(cmp, baselineState) {
  let blocker = null;
  if (baselineState?.state !== "present") {
    blocker = baselineState?.reason ?? NO_BASELINE;
  } else {
    const b = baselineState.baseline;
    if (b.environment !== cmp.environment) blocker = `baseline mismatch: the file is for ${b.environment}, the run is ${cmp.environment} (D6)`;
    else if (b.setup !== cmp.setup || b.variant !== cmp.variant) blocker = `baseline mismatch: the file is for ${b.setup}/${b.variant}, the comparison is ${cmp.setup}/${cmp.variant}`;
  }
  const counts = Object.fromEntries(VERDICTS.map((v) => [v, 0]));
  for (const e of cmp.entries) {
    if (!HEADLINE_KPIS.includes(e.kpi)) continue;
    e.verdict = blocker
      ? { verdict: "unknown", reason: blocker }
      : judgeEntry(e, baselineState.baseline.thresholds[e.kpi], baselineState.baseline.min_runs);
    counts[e.verdict.verdict] += 1;
  }
  const judged = counts.regression + counts.improvement + counts.no_change;
  let overall;
  if (blocker) overall = { verdict: "unknown", reason: blocker };
  else if (counts.regression > 0) overall = { verdict: "regression", reason: `${counts.regression} headline entr${counts.regression === 1 ? "y" : "ies"} regressed` };
  else if (judged === 0) overall = { verdict: "unknown", reason: "no headline entry could be judged" };
  else if (counts.improvement > 0) overall = { verdict: "improvement", reason: `${counts.improvement} improved, none regressed` };
  else overall = { verdict: "no_change", reason: `${judged} judged, none regressed` };
  cmp.verdict = { ...overall, counts, baseline: baselineState?.state === "present" ? { recorded_from: baselineState.baseline.recorded_from ?? null, min_runs: baselineState.baseline.min_runs } : null };
  return cmp;
}

/** Whether a verdict fails the scheduled job: `regression` and nothing else. */
export function verdictFails(verdict) {
  return verdict?.verdict === "regression";
}

/**
 * A baseline document proposed from one comparison: the BASELINE build's
 * readings (runner rows only — the comparison is runner-only by construction,
 * D7), the proposed thresholds by unit, and the run it came from. Refused when
 * the baseline side has no proven commit (D12: a baseline from a build nobody
 * proved is no baseline).
 */
export function proposeBaseline(cmp, { runId = null, recordedAt = new Date().toISOString(), minRuns = MIN_RUNS_FLOOR } = {}) {
  if (cmp.setup !== "runner") throw new Error("a baseline holds the runner's own readings only (D7)");
  if (!cmp.baseline_commit) throw new Error("the baseline build has no proven commit; a baseline from a build nobody proved is no baseline (D12)");
  const thresholds = {};
  for (const id of HEADLINE_KPIS) {
    const t = PROPOSED_THRESHOLDS[getKpi(id).unit];
    if (t) thresholds[id] = { ...t };
  }
  const readings = cmp.entries
    .filter((e) => HEADLINE_KPIS.includes(e.kpi) && e.baseline.spread)
    .map((e) => ({ kpi: e.kpi, statistic: e.statistic, unit: e.unit, n: e.n, arm: e.arm, ...e.baseline.spread }));
  return {
    schema: BASELINE_SCHEMA_ID,
    environment: cmp.environment,
    setup: cmp.setup,
    variant: cmp.variant,
    recorded_from: { run_id: runId, recorded_at: recordedAt, baseline_commit: cmp.baseline_commit, head_commit: cmp.head_commit },
    min_runs: minRuns,
    thresholds,
    readings,
  };
}

function fmt(spread, unit) {
  if (!spread) return "unknown";
  const r = (x) => (Math.abs(x) >= 100 ? x.toFixed(0) : Math.abs(x) >= 10 ? x.toFixed(1) : x.toFixed(2));
  const u = unit === "count" || unit === "ratio" ? "" : ` ${unit}`;
  return `${r(spread.median)}${u} [${r(spread.min)}–${r(spread.max)}]`;
}

const verdictCell = (v) => (!v ? "–" : v.verdict === "unknown" ? `unknown(${v.reason})` : v.verdict === "no_change" ? `no_change(${v.reason})` : `**${v.verdict}**`);

/** The comparison as a markdown table (the job summary's body). */
export function renderComparison(cmp) {
  const v = cmp.verdict;
  const lines = [
    `### ${cmp.setup}${cmp.variant !== "default" ? `/${cmp.variant}` : ""}: head ${cmp.head_commit?.slice(0, 12) ?? "?"} vs baseline ${cmp.baseline_commit?.slice(0, 12) ?? "?"} (${cmp.environment})`,
    "",
  ];
  if (v) {
    lines.push(
      `Verdict: **${v.verdict}** — ${v.reason}. (${VERDICTS.map((k) => `${v.counts[k]} ${k}`).join(", ")} over the headline entries.) Only \`regression\` fails the job; an unknown never does.`,
      "",
    );
  }
  lines.push(
    "Median over runs, run spread in brackets; a side with no measured run reads unknown. Headline KPIs carry a verdict (beyond the baseline file's threshold AND disjoint spreads); the rest are context.",
    "",
    "| KPI | stat | N | arm | head | baseline | head runs | baseline runs | verdict |",
    "|---|---|---|---|---|---|---|---|---|",
  );
  for (const e of cmp.entries) {
    lines.push(
      `| ${e.kpi} | ${e.statistic} | ${e.n} | ${e.arm} | ${fmt(e.head.spread, e.unit)} | ${fmt(e.baseline.spread, e.unit)} | ${e.head.states} | ${e.baseline.states} | ${verdictCell(e.verdict)} |`,
    );
  }
  return `${lines.join("\n")}\n`;
}

const invokedDirectly = process.argv[1] && resolve(process.argv[1]) === resolve(fileURLToPath(import.meta.url));
if (invokedDirectly) {
  // Imported here, not at the top: cli.mjs imports this module, so it loads once
  // this one has finished evaluating (no top-level await: that would deadlock the cycle).
  import("./cli.mjs")
    .then(({ main }) => main(["compare", ...process.argv.slice(2)]))
    .then((code) => {
      process.exitCode = code;
    });
}

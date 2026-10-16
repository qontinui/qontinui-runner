/**
 * termhost-bench `compare`: the runner's head build against its baseline build,
 * per KPI, N and arm — each side's median over its runs, the run spread [min, max],
 * and how many runs were measured (and why the others were not) — and, for the
 * headline KPIs, the regression verdict (plan Phase 6).
 *
 * Plan `2026-10-11-terminal-page-performance-kpis-across-setups`. Phase 3 built the
 * table; Phase 6 adds the verdict here (no second module). ONE verdict gates: the
 * gate, output_latency_ms p95 pooled over every N and arm (the KPI the plan's
 * Phase 6 arming row names; see `poolGate`). Every headline KPI (catalog
 * `headline_statistic`) is also judged at each N and arm, informationally — rendered,
 * `gating: false`, never failing a job. Each judgement is:
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
 * not-fire-a-detector]: the overall verdict is the gate's, and the scheduled job
 * fails only on its `regression`. Judging some 40 entries and failing on any one
 * would go red on most runs where nothing changed (the review's measurement); the
 * one pooled gate keeps that under 2 % (the seeded simulation in the tests, which
 * assumes half the variance is launch noise shared by a unit's two arms; see
 * `PROPOSED_GATE_RELATIVE`).
 *
 * "Control-normalised" (the plan's arming row) is not defined anywhere in the
 * plan, and the results carry no control reading (no unloaded control session,
 * no control KPI). The only control in the design is the same-VM baseline build
 * (D11), which every comparison here is already drawn against; normalising by
 * any further control is deferred until the plan defines one.
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

import { readFileSync, realpathSync } from "node:fs";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { ARMS, ENVIRONMENTS, KPI_CATALOG, getKpi } from "./kpi-catalog.mjs";

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

/**
 * median / min / max of a non-empty list. The median is the midpoint of the two
 * middle values for an even count (with two runs, their mean) — a statistic of
 * the runs, not necessarily one of them.
 */
export function spreadOf(values) {
  if (values.length === 0) return null;
  const sorted = [...values].sort((a, b) => a - b);
  const mid = sorted.length / 2;
  return {
    median: sorted.length % 2 === 1 ? sorted[Math.floor(mid)] : (sorted[mid - 1] + sorted[mid]) / 2,
    min: sorted[0],
    max: sorted[sorted.length - 1],
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
        // Per run, for the pooled gate: the run index and its value (null = not measured).
        const values = rows.map((r) => ({ run: r.scenario.run_index, value: valueOf(r.kpis[id], kpi) }));
        return { spread: spreadOf(nums), states: statesOf(raw), total: rows.length, values };
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

/**
 * The one KPI whose verdict GATES the scheduled job (the plan's Phase 6 arming
 * row names output_latency_ms p95). Every other headline KPI's verdict is
 * computed and rendered, and is informational: it never fails a job.
 */
export const GATING_KPI = "output_latency_ms";

/** Fewer measured runs on a side than this and the side is under-sampled. */
export const MIN_RUNS_FLOOR = 2;

/**
 * Runs per build each scheduled job takes (`runs` in termhost-bench.yml's
 * parameters step; a test holds the two in step). A baseline's `min_runs` may not
 * exceed its environment's, or its gate could never be judged.
 */
export const SCHEDULED_RUNS = Object.freeze({ "linux-xvfb": 3, "windows-hosted": 2 });

/** The fewest pooled entries the gate is drawn from: one entry alone is not a reproduction. */
export const MIN_GATE_ENTRIES = 2;

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

/**
 * The gate's relative threshold, by runs per build — chosen with the seeded
 * null/power simulation in `__tests__/verdict-report.test.mjs`: six pooled
 * entries, run-to-run CV 0.2, a 30 % regression across every N and arm, and the
 * CORRELATION ASSUMPTION that half of each value's log-variance is launch noise
 * shared by the two arms of one (N, build, run) — they share a launch
 * (run.mjs measures both arms of a unit in one launch) — so the six entries are
 * three independent pairs, not six independent draws. Chosen so the job goes
 * red with no change <= 2 % at that 50 % sharing; the measured figures are
 * `GATE_SIMULATION`, which the seeded test reproduces exactly (so they cannot
 * drift from it).
 * Noise shared more widely (one run's time slice across every N and both
 * builds) is not modelled: it cancels in the head/baseline ratio only when the
 * two builds are interleaved within it. The non-overlapping-spreads rule caps
 * the power: with 3 runs against 3 a null comparison is disjoint 1 time in 20
 * before any threshold applies.
 */
export const PROPOSED_GATE_RELATIVE = Object.freeze({ 2: 0.24, 3: 0.16 });

/**
 * What the seeded simulation in `__tests__/verdict-report.test.mjs` measures for
 * the thresholds above, in percent of trials the job goes red: CV 0.2; `null` =
 * nothing changed (10 000 trials), `power` = a 30 % p95 regression across every
 * N and arm (4 000 trials); `independent` = no shared launch noise, `shared` =
 * half the log-variance shared by a unit's two arms. The test asserts these
 * exact figures and the README quotes them (a test holds that too).
 */
export const GATE_SIMULATION = Object.freeze({
  "linux-xvfb": Object.freeze({ runs: 3, relative: 0.16, null: Object.freeze({ independent: 0.83, shared: 1.6 }), power: Object.freeze({ independent: 87.52, shared: 75.75 }) }),
  "windows-hosted": Object.freeze({ runs: 2, relative: 0.24, null: Object.freeze({ independent: 0.4, shared: 1.22 }), power: Object.freeze({ independent: 71.8, shared: 67.4 }) }),
});

const isNum = (x) => typeof x === "number" && Number.isFinite(x);

/**
 * Structural check of a baseline document, including the ways one could disarm
 * the gate: no threshold for the gating KPI, a relative threshold outside (0, 1]
 * (25 meant as 25 %), a `min_runs` above what the environment's scheduled job
 * takes, and a count KPI with no absolute floor. A threshold's `absolute` applies
 * to the per-entry, informational verdicts only: the pooled gate is relative.
 *
 * @param {{scheduledRuns?: Record<string, number>}} [o]
 * @returns {string[]} errors (empty = usable)
 */
export function validateBaseline(b, { scheduledRuns = SCHEDULED_RUNS } = {}) {
  const errors = [];
  if (!b || typeof b !== "object" || Array.isArray(b)) return ["the baseline is not a JSON object"];
  if (b.schema !== BASELINE_SCHEMA_ID) errors.push(`schema must be ${BASELINE_SCHEMA_ID}, got ${JSON.stringify(b.schema)}`);
  if (!ENVIRONMENTS.includes(b.environment)) errors.push(`environment must be one of ${ENVIRONMENTS.join(", ")}`);
  if (b.setup !== "runner") errors.push("setup must be runner: a baseline holds the runner's own readings only (D7)");
  if (typeof b.variant !== "string" || b.variant.length === 0) errors.push("variant must be a non-empty string");
  if (!Number.isInteger(b.min_runs) || b.min_runs < MIN_RUNS_FLOOR) errors.push(`min_runs must be an integer >= ${MIN_RUNS_FLOOR} (one run has no spread)`);
  const scheduled = scheduledRuns[b.environment];
  if (Number.isInteger(b.min_runs) && Number.isInteger(scheduled) && b.min_runs > scheduled) {
    errors.push(`min_runs ${b.min_runs} exceeds the ${scheduled} run(s) the scheduled ${b.environment} job takes: the gate could never be judged`);
  }
  if (!b.thresholds || typeof b.thresholds !== "object" || Array.isArray(b.thresholds)) {
    errors.push("thresholds must be an object keyed by headline KPI id");
  } else {
    if (!b.thresholds[GATING_KPI]) errors.push(`thresholds.${GATING_KPI}: required — it is the KPI that gates the job`);
    else if (!(isNum(b.thresholds[GATING_KPI].relative) && b.thresholds[GATING_KPI].relative > 0)) errors.push(`thresholds.${GATING_KPI}.relative: required and > 0 (the gate is relative)`);
    for (const [id, t] of Object.entries(b.thresholds)) {
      if (!HEADLINE_KPIS.includes(id)) errors.push(`thresholds.${id}: not a headline KPI (${HEADLINE_KPIS.join(", ")})`);
      if (!t || typeof t !== "object") {
        errors.push(`thresholds.${id}: must be {relative?, absolute?}`);
        continue;
      }
      const extra = Object.keys(t).filter((k) => k !== "relative" && k !== "absolute");
      if (extra.length > 0) errors.push(`thresholds.${id}: unknown field(s) ${extra.join(", ")}`);
      if (t.relative === undefined && t.absolute === undefined) errors.push(`thresholds.${id}: needs relative and/or absolute`);
      if (t.relative !== undefined && !(isNum(t.relative) && t.relative >= 0 && t.relative <= 1)) errors.push(`thresholds.${id}.relative: must be a share in [0, 1] (0.25 is 25 %)`);
      if (t.absolute !== undefined && !(isNum(t.absolute) && t.absolute >= 0)) errors.push(`thresholds.${id}.absolute: must be a number >= 0`);
      if (HEADLINE_KPIS.includes(id) && getKpi(id).unit === "count" && !(isNum(t.absolute) && t.absolute > 0)) {
        errors.push(`thresholds.${id}.absolute: a count KPI needs an absolute floor > 0 (its baseline median is usually 0, where a relative margin is 0)`);
      }
      if ((t.relative ?? 0) === 0 && (t.absolute ?? 0) === 0) errors.push(`thresholds.${id}: a zero threshold would call any worsening a regression`);
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

/** The margin a KPI must worsen by before it can be a regression: from the BASELINE median. */
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

/**
 * The GATE: the gating KPI's statistic pooled over every N and arm the two
 * builds share, so one judgement covers the whole ladder (no multiple
 * comparisons) and a regression must show across it to move the pool.
 *
 * Each entry's values are divided by that entry's median over both builds' runs,
 * so the pooled values read as ratios near 1. The normaliser scales both sides
 * alike, so no verdict depends on its choice (a mutation to the baseline's own
 * median is equivalent); what matters is that it is ONE per entry. Each run's
 * pooled value is the geometric mean of its normalised values over the entries; then the two sides' pooled run values are judged as
 * one entry: worse than the baseline's by more than the relative threshold AND
 * spreads that do not overlap. The gate is relative only: a threshold's
 * `absolute` (ms) has no meaning for the dimensionless pooled ratio and applies
 * to the per-entry, informational verdicts alone. An entry measured on only one
 * build (or neither) cannot be pooled; the gate names every such entry in its
 * reason rather than letting it leave silently. An entry the BASELINE measured
 * and the head did not makes the gate `unknown` (`head_unknown_where_baseline_measured`):
 * a head that wedged at one N must not read as no_change on the rest. One
 * unknown on both builds, or on the baseline only, just drops. A pooled regression must also REPRODUCE: at
 * least `MIN_GATE_ENTRIES` of the pooled entries must have the head's median
 * worse than the baseline's by more than the same relative threshold — one
 * entry moving the pool alone is reported as `no_change(not_reproduced)`. A
 * run that lacks a measured value at any pooled
 * entry contributes no pooled value (it is not a number), so a side left with
 * fewer than `minRuns` is under-sampled and the gate unknown.
 *
 * @param {object[]} entries `compareBuilds` entries of the gating KPI (with `values`)
 * @param {{relative: number}} threshold
 * @param {number} minRuns
 * @returns {{verdict: string, reason: string, detail?: string, entries: {n: number, arm: string}[],
 *   head?: object, baseline?: object}}
 */
export function poolGate(entries, threshold, minRuns) {
  const pooled = entries.filter((e) => e.head.spread && e.baseline.spread);
  const keys = pooled.map((e) => ({ n: e.n, arm: e.arm }));
  const excluded = entries
    .filter((e) => !(e.head.spread && e.baseline.spread))
    .map((e) => ({ n: e.n, arm: e.arm, why: [!e.head.spread && "head unknown", !e.baseline.spread && "baseline unknown"].filter(Boolean).join(", ") }));
  const notPooled = excluded.length > 0 ? `; not pooled (${excluded.length} of ${entries.length}): ${excluded.map((x) => `N=${x.n} ${x.arm} (${x.why})`).join(", ")}` : "";
  const out = (fields) => ({ entries: keys, ...(excluded.length > 0 ? { excluded } : {}), ...fields, reason: `${fields.reason}${notPooled}` });
  // The head lost an entry the baseline measured (e.g. every N = 15 pane wedged):
  // judging the rest would read a broken head as no_change, so the gate is
  // unknown. An entry unknown on BOTH builds says nothing about the head; it just drops.
  const headLost = excluded.filter((x) => x.why === "head unknown");
  if (headLost.length > 0) {
    return out({
      verdict: "unknown",
      reason: `head_unknown_where_baseline_measured: ${headLost.map((x) => `N=${x.n} ${x.arm}`).join(", ")}`,
    });
  }
  if (!threshold || !(threshold.relative > 0)) return out({ verdict: "unknown", reason: "no_threshold", detail: `the baseline file records no relative threshold for ${GATING_KPI}` });
  if (pooled.length < MIN_GATE_ENTRIES) {
    return out({ verdict: "unknown", reason: "too_few_gate_entries", detail: `${pooled.length} N/arm entr${pooled.length === 1 ? "y" : "ies"} measured on both builds; the gate pools at least ${MIN_GATE_ENTRIES}` });
  }
  const norms = pooled.map((e) => {
    const all = [...e.head.values, ...e.baseline.values].map((v) => v.value).filter((v) => v !== null);
    return spreadOf(all).median;
  });
  if (pooled.some((e, k) => !(norms[k] > 0) || [...e.head.values, ...e.baseline.values].some((v) => v.value !== null && !(v.value > 0)))) {
    return out({ verdict: "unknown", reason: "non_positive_value", detail: "a geometric mean needs positive values" });
  }
  const side = (which) => {
    const runs = [...new Set(pooled.flatMap((e) => e[which].values.map((v) => v.run)))].sort((a, b) => a - b);
    const vals = [];
    for (const run of runs) {
      let logSum = 0;
      let complete = true;
      pooled.forEach((e, k) => {
        const v = e[which].values.find((x) => x.run === run)?.value ?? null;
        if (v === null) complete = false;
        else logSum += Math.log(v / norms[k]);
      });
      if (complete) vals.push(Math.exp(logSum / pooled.length));
    }
    return { spread: spreadOf(vals), states: `${vals.length} of ${runs.length} run(s) measured at every pooled entry` };
  };
  const head = side("head");
  const baseline = side("baseline");
  const judged = judgeEntry({ kpi: GATING_KPI, direction: getKpi(GATING_KPI).direction, head, baseline }, { relative: threshold.relative }, minRuns);
  const sign = getKpi(GATING_KPI).direction === "lower_is_better" ? 1 : -1;
  const worseAt = pooled.filter((e) => sign * (e.head.spread.median - e.baseline.spread.median) > threshold.relative * Math.abs(e.baseline.spread.median));
  if (judged.verdict === "regression" && worseAt.length < MIN_GATE_ENTRIES) {
    return out({
      verdict: "no_change",
      reason: "not_reproduced",
      detail: `the pool moved, but only ${worseAt.length} of ${pooled.length} entries are worse beyond the threshold (${worseAt.map((e) => `N=${e.n} ${e.arm}`).join(", ") || "none"}); ${judged.detail}`,
      head,
      baseline,
    });
  }
  return out({ ...judged, reproduced_at: worseAt.map((e) => ({ n: e.n, arm: e.arm })), head, baseline });
}

/** The overall verdict when no comparison could be drawn at all. */
export function unknownVerdict(reason, detail) {
  return { verdict: "unknown", reason, ...(detail ? { detail } : {}), gating: null, counts: Object.fromEntries(VERDICTS.map((v) => [v, 0])) };
}

/**
 * Attach a verdict to every headline entry (each `gating: false`, informational)
 * and the gate (`cmp.gate`, `gating: true`); the overall verdict IS the gate's.
 *
 * @param {object} cmp       `compareBuilds` output
 * @param {{state: string, baseline?: object, reason?: string}} baselineState `readBaselineFile` output
 * @returns {object} cmp, with `entries[].verdict`, `gate` and `verdict`
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
    e.verdict = {
      ...(blocker ? { verdict: "unknown", reason: blocker } : judgeEntry(e, baselineState.baseline.thresholds[e.kpi], baselineState.baseline.min_runs)),
      gating: false,
    };
    counts[e.verdict.verdict] += 1;
  }
  const gateEntries = cmp.entries.filter((e) => e.kpi === GATING_KPI);
  const gate = blocker
    ? { verdict: "unknown", reason: blocker, entries: gateEntries.map((e) => ({ n: e.n, arm: e.arm })) }
    : poolGate(gateEntries, baselineState.baseline.thresholds[GATING_KPI], baselineState.baseline.min_runs);
  cmp.gate = { kpi: GATING_KPI, statistic: compareStatistic(getKpi(GATING_KPI)), pooled: "geometric mean over N x arm, each entry normalised by its median over both builds", ...gate, gating: true };
  const informational = counts.regression > 0 ? `; ${counts.regression} informational entr${counts.regression === 1 ? "y" : "ies"} regressed (never gating)` : "";
  cmp.verdict = {
    verdict: gate.verdict,
    reason: `${gate.reason}${informational}`,
    ...(gate.detail ? { detail: gate.detail } : {}),
    gating: { kpi: GATING_KPI, statistic: cmp.gate.statistic, entries: gate.entries },
    counts,
    baseline: baselineState?.state === "present" ? { recorded_from: baselineState.baseline.recorded_from ?? null, min_runs: baselineState.baseline.min_runs } : null,
  };
  return cmp;
}

/** Whether a verdict fails the scheduled job: the gate's `regression` and nothing else. */
export function verdictFails(verdict) {
  return verdict?.verdict === "regression";
}

/**
 * A baseline document proposed from one comparison: the BASELINE build's
 * readings (runner rows only — the comparison is runner-only by construction,
 * D7), the proposed thresholds, `min_runs` = the environment's scheduled runs,
 * and the run it came from. Refused when the baseline side has no proven commit
 * (D12: a baseline from a build nobody proved is no baseline).
 */
export function proposeBaseline(cmp, { runId = null, recordedAt = new Date().toISOString(), minRuns } = {}) {
  if (cmp.setup !== "runner") throw new Error("a baseline holds the runner's own readings only (D7)");
  if (!cmp.baseline_commit) throw new Error("the baseline build has no proven commit; a baseline from a build nobody proved is no baseline (D12)");
  const runs = minRuns ?? SCHEDULED_RUNS[cmp.environment] ?? MIN_RUNS_FLOOR;
  const thresholds = {};
  for (const id of HEADLINE_KPIS) {
    const t = PROPOSED_THRESHOLDS[getKpi(id).unit];
    if (t) thresholds[id] = { ...t };
  }
  thresholds[GATING_KPI] = { ...thresholds[GATING_KPI], relative: PROPOSED_GATE_RELATIVE[runs] ?? PROPOSED_GATE_RELATIVE[3] };
  const readings = cmp.entries
    .filter((e) => HEADLINE_KPIS.includes(e.kpi) && e.baseline.spread)
    .map((e) => ({ kpi: e.kpi, statistic: e.statistic, unit: e.unit, n: e.n, arm: e.arm, ...e.baseline.spread }));
  return {
    schema: BASELINE_SCHEMA_ID,
    environment: cmp.environment,
    setup: cmp.setup,
    variant: cmp.variant,
    recorded_from: { run_id: runId, recorded_at: recordedAt, baseline_commit: cmp.baseline_commit, head_commit: cmp.head_commit },
    min_runs: runs,
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
    const g = cmp.gate;
    lines.push(
      `Verdict: **${v.verdict}** — ${v.reason}. Only the gate (\`${g.kpi}\` ${g.statistic}, pooled over ${g.entries.length} N/arm entr${g.entries.length === 1 ? "y" : "ies"}) fails the job, and only on \`regression\`; every other verdict is informational, and an unknown never fails.`,
      "",
      `| gate | head (pooled, per run) | baseline (pooled, per run) | head runs | baseline runs | verdict |`,
      `|---|---|---|---|---|---|`,
      `| ${g.kpi} ${g.statistic}, ${g.pooled} | ${fmt(g.head?.spread, "ratio")} | ${fmt(g.baseline?.spread, "ratio")} | ${g.head?.states ?? "-"} | ${g.baseline?.states ?? "-"} | ${verdictCell(g)} |`,
      "",
    );
  }
  lines.push(
    "Median over runs (the midpoint for an even count), run spread in brackets; a side with no measured run reads unknown. Headline KPIs carry a verdict (beyond the baseline file's threshold AND disjoint spreads); `gating` says whether it can fail the job.",
    "",
    "| KPI | stat | N | arm | head | baseline | head runs | baseline runs | verdict | gating |",
    "|---|---|---|---|---|---|---|---|---|---|",
  );
  for (const e of cmp.entries) {
    const gating = !e.verdict ? "–" : e.kpi === GATING_KPI ? "no (pooled into the gate)" : "no (informational)";
    lines.push(
      `| ${e.kpi} | ${e.statistic} | ${e.n} | ${e.arm} | ${fmt(e.head.spread, e.unit)} | ${fmt(e.baseline.spread, e.unit)} | ${e.head.states} | ${e.baseline.states} | ${verdictCell(e.verdict)} | ${gating} |`,
    );
  }
  return `${lines.join("\n")}\n`;
}

/** Whether `argv1` names this module, through any symlink (a symlinked path must not run nothing, silently). */
export function isEntryPoint(argv1, moduleUrl = import.meta.url) {
  if (!argv1) return false;
  const real = (p) => {
    try {
      return realpathSync(p);
    } catch {
      return resolve(p);
    }
  };
  return real(resolve(argv1)) === real(fileURLToPath(moduleUrl));
}

const invokedDirectly = isEntryPoint(process.argv[1]);
if (invokedDirectly) {
  // Imported here, not at the top: cli.mjs imports this module, so it loads once
  // this one has finished evaluating (no top-level await: that would deadlock the cycle).
  import("./cli.mjs")
    .then(({ main }) => main(["compare", ...process.argv.slice(2)]))
    .then((code) => {
      process.exitCode = code;
    });
}

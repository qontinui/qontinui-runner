/**
 * termhost-bench `compare`: the runner's head build against its baseline build,
 * per KPI, N and arm — each side's median over its runs, the run spread [min, max],
 * and how many runs were measured (and why the others were not).
 *
 * Plan `2026-10-11-terminal-page-performance-kpis-across-setups`. Phase 3 renders
 * the table; Phase 6 extends THIS module with the regression / improvement verdict
 * (spreads that do not overlap, beyond a threshold the baseline file records) and
 * the scheduled job's failure — no second module. Nothing here judges yet, and an
 * unknown value is never folded into a median: a side with no measured run reads
 * `unknown`, with its reasons.
 *
 * Inputs: one results file holding both builds (the same-VM A/B, D11 — rows with
 * `setup.build.role` `head` and `baseline`), or two files (head first). Two files
 * from different environments are refused (D6).
 */

import { KPI_CATALOG, getKpi } from "./kpi-catalog.mjs";
import { percentile } from "./stats.mjs";

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
    doc.rows.filter((r) => r.setup.id === setup && r.setup.variant === variant && roles.includes(r.setup.build?.role ?? "single"));
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

function fmt(spread, unit) {
  if (!spread) return "unknown";
  const r = (x) => (Math.abs(x) >= 100 ? x.toFixed(0) : Math.abs(x) >= 10 ? x.toFixed(1) : x.toFixed(2));
  const u = unit === "count" || unit === "ratio" ? "" : ` ${unit}`;
  return `${r(spread.median)}${u} [${r(spread.min)}–${r(spread.max)}]`;
}

/** The comparison as a markdown table (the job summary's body). */
export function renderComparison(cmp) {
  const lines = [
    `### ${cmp.setup}${cmp.variant !== "default" ? `/${cmp.variant}` : ""}: head ${cmp.head_commit?.slice(0, 12) ?? "?"} vs baseline ${cmp.baseline_commit?.slice(0, 12) ?? "?"} (${cmp.environment})`,
    "",
    "Median over runs, run spread in brackets; a side with no measured run reads unknown. No verdict is drawn here (the regression rule arrives with Phase 6).",
    "",
    "| KPI | stat | N | arm | head | baseline | head runs | baseline runs |",
    "|---|---|---|---|---|---|---|---|",
  ];
  for (const e of cmp.entries) {
    lines.push(
      `| ${e.kpi} | ${e.statistic} | ${e.n} | ${e.arm} | ${fmt(e.head.spread, e.unit)} | ${fmt(e.baseline.spread, e.unit)} | ${e.head.states} | ${e.baseline.states} |`,
    );
  }
  return `${lines.join("\n")}\n`;
}

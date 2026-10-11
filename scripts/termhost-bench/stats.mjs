/**
 * termhost-bench statistics — nearest-rank percentiles, least-squares slope and the
 * degradation ratio, plus the adapters that turn samples into catalog KPI values.
 *
 * Plan `2026-10-11-terminal-page-performance-kpis-across-setups`, Phase 1. Every
 * function returns a READING, never a bare number:
 *
 *     {state: "measured", value, n}   or   {state: "unknown", reason, n}
 *
 * so a statistic computed from too few samples cannot reach a results file as a
 * plausible-looking number. Minimums:
 *   - a p-th percentile needs n >= ceil(100 / (100 - p)) (kpi-catalog `percentileFloor`):
 *     below that the nearest-rank percentile IS the maximum, and calling it p99 would
 *     mislabel it; a caller may raise the minimum, never lower it;
 *   - a slope needs 3 points and two distinct x values;
 *   - a degradation ratio needs both operands measured and a non-zero N = 1 value; a
 *     not_applicable operand makes it not_applicable (never unknown).
 *
 * Inputs must be finite numbers. A NaN or null sample is a defect upstream, so it
 * throws rather than being silently dropped (silent-empty is unknown, not data).
 */

import { getKpi, minSamplesForStatistic, percentParts, percentileFloor } from "./kpi-catalog.mjs";

/**
 * @typedef {{state: "measured", value: number, n: number}} MeasuredReading
 * @typedef {{state: "unknown", reason: string, n: number}} UnknownReading
 * @typedef {{state: "not_applicable", reason: string, n: number}} NotApplicableReading
 * @typedef {MeasuredReading | UnknownReading | NotApplicableReading} Reading
 */

/**
 * @param {unknown} values
 * @param {string} what
 * @returns {number[]}
 */
function finiteArray(values, what) {
  if (!Array.isArray(values)) throw new TypeError(`${what} must be an array`);
  for (let i = 0; i < values.length; i += 1) {
    if (typeof values[i] !== "number" || !Number.isFinite(values[i])) {
      throw new TypeError(`${what}[${i}] is not a finite number: ${values[i]}`);
    }
  }
  return values;
}

/** @returns {UnknownReading} */
function unknown(reason, n) {
  return { state: "unknown", reason, n };
}

/** @returns {MeasuredReading} */
function measured(value, n) {
  return { state: "measured", value, n };
}

/**
 * The 1-based nearest rank of the p-th percentile among n samples: ceil(p * n / 100),
 * computed in integers (p as an exact decimal fraction) so 95 * 20 / 100 is exactly 19.
 *
 * @param {number} p in (0, 100]
 * @param {number} n >= 1
 * @returns {number}
 */
export function nearestRank(p, n) {
  const { num, den } = percentParts(p);
  if (!Number.isInteger(n) || n < 1) throw new RangeError(`n must be a positive integer: ${n}`);
  const whole = 100 * den;
  const rank = Math.floor((num * n + whole - 1) / whole);
  return Math.min(n, Math.max(1, rank));
}

/**
 * Nearest-rank percentile: the smallest sample with at least p% of the samples at or
 * below it. Never interpolates, so it never reports a value that was not observed.
 *
 * @param {number[]} values
 * @param {number} p in (0, 100]
 * @param {{minN?: number}} [opts] raises the minimum above the percentile's floor
 * @returns {Reading}
 */
export function percentile(values, p, { minN = 0 } = {}) {
  finiteArray(values, "values");
  const n = values.length;
  const floor = Math.max(percentileFloor(p), minN);
  if (n < floor) return unknown("insufficient_samples", n);
  const sorted = [...values].sort((a, b) => a - b);
  return measured(sorted[nearestRank(p, n) - 1], n);
}

/**
 * @param {number[]} values
 * @param {{minN?: number}} [opts]
 * @returns {Reading}
 */
export function mean(values, { minN = 1 } = {}) {
  finiteArray(values, "values");
  const n = values.length;
  if (n < Math.max(1, minN)) return unknown("insufficient_samples", n);
  let sum = 0;
  for (const v of values) sum += v;
  return measured(sum / n, n);
}

/**
 * @param {number[]} values
 * @param {{minN?: number}} [opts]
 * @returns {Reading}
 */
export function max(values, { minN = 1 } = {}) {
  finiteArray(values, "values");
  const n = values.length;
  if (n < Math.max(1, minN)) return unknown("insufficient_samples", n);
  let m = values[0];
  for (const v of values) if (v > m) m = v;
  return measured(m, n);
}

/**
 * @param {number[]} values
 * @param {{minN?: number}} [opts]
 * @returns {Reading}
 */
export function min(values, { minN = 1 } = {}) {
  finiteArray(values, "values");
  const n = values.length;
  if (n < Math.max(1, minN)) return unknown("insufficient_samples", n);
  let m = values[0];
  for (const v of values) if (v < m) m = v;
  return measured(m, n);
}

/**
 * Percentage (0-100) of the samples satisfying `predicate`.
 *
 * @param {number[]} values
 * @param {(v: number) => boolean} predicate
 * @param {{minN?: number}} [opts]
 * @returns {Reading}
 */
export function share(values, predicate, { minN = 1 } = {}) {
  finiteArray(values, "values");
  const n = values.length;
  if (n < Math.max(1, minN)) return unknown("insufficient_samples", n);
  let hits = 0;
  for (const v of values) if (predicate(v)) hits += 1;
  return measured((hits / n) * 100, n);
}

/**
 * Ordinary least-squares slope of y on x.
 *
 * @param {number[]} xs
 * @param {number[]} ys
 * @param {{minN?: number}} [opts]
 * @returns {Reading}
 */
export function slope(xs, ys, { minN = 3 } = {}) {
  finiteArray(xs, "xs");
  finiteArray(ys, "ys");
  if (xs.length !== ys.length) {
    throw new RangeError(`xs and ys differ in length (${xs.length} vs ${ys.length})`);
  }
  const n = xs.length;
  if (n < Math.max(3, minN)) return unknown("insufficient_samples", n);
  let sx = 0;
  let sy = 0;
  for (let i = 0; i < n; i += 1) {
    sx += xs[i];
    sy += ys[i];
  }
  const mx = sx / n;
  const my = sy / n;
  let sxx = 0;
  let sxy = 0;
  for (let i = 0; i < n; i += 1) {
    const dx = xs[i] - mx;
    sxx += dx * dx;
    sxy += dx * (ys[i] - my);
  }
  if (sxx === 0) return unknown("degenerate_input", n);
  return measured(sxy / sxx, n);
}

/**
 * A KPI at N divided by the same setup's value at N = 1. The ratio's n counts its
 * measured operands. A not_applicable operand makes the ratio not_applicable with that
 * operand's reason; an unknown value at N propagates its own reason; an unknown or zero
 * N = 1 value makes the ratio unknown(no_baseline_level / zero_baseline). A value of 0
 * at N is a measured ratio of 0.
 *
 * @param {{state: string, value?: number, reason?: string} | undefined} atN
 * @param {{state: string, value?: number, reason?: string} | undefined} atOne
 * @returns {Reading}
 */
export function degradation(atN, atOne) {
  const nMeasured = (atN?.state === "measured" ? 1 : 0) + (atOne?.state === "measured" ? 1 : 0);
  if (atN?.state === "not_applicable") return notApplicable(atN.reason, nMeasured);
  if (atN?.state !== "measured") return unknown(atN?.reason ?? "insufficient_samples", nMeasured);
  if (atOne?.state === "not_applicable") return notApplicable(atOne.reason, nMeasured);
  if (atOne?.state !== "measured") return unknown("no_baseline_level", nMeasured);
  finiteArray([atN.value, atOne.value], "operands");
  if (atOne.value === 0) return unknown("zero_baseline", nMeasured);
  return measured(atN.value / atOne.value, 2);
}

/** @returns {NotApplicableReading} */
function notApplicable(reason, n) {
  if (!reason) throw new TypeError("a not_applicable operand must carry its reason");
  return { state: "not_applicable", reason, n };
}

/**
 * A `degradation_*` KPI value for one row: the base KPI and statistic come from the
 * catalog's `degradation_of`, read out of the row's `kpis` at N and the matching
 * N = 1 row's `kpis` (pass `undefined` when there is no N = 1 row). An N = 1 row is the
 * baseline itself: pass its `n` (or the same `kpis` object twice) and the value is
 * not_applicable(baseline_level), never a measured ratio of 1.
 *
 * @param {string} id a degradation_* catalog id
 * @param {Record<string, object>} kpisAtN the row's `kpis`
 * @param {Record<string, object> | undefined} kpisAtOne the matching N = 1 row's `kpis`
 * @param {{n?: number}} [opts] the row's N
 * @returns {object}
 */
export function degradationKpi(id, kpisAtN, kpisAtOne, { n } = {}) {
  const of = getKpi(id).degradation_of;
  if (!of) throw new RangeError(`${id} is not a degradation KPI`);
  if (n === 1 || (kpisAtN !== undefined && kpisAtN === kpisAtOne)) {
    return notApplicableKpi(id, "baseline_level");
  }
  const operand = (kpiValue) => {
    if (kpiValue?.state !== "measured") return kpiValue;
    const value = kpiValue.stats?.[of.statistic];
    if (typeof value !== "number") {
      throw new TypeError(`${of.kpi} is measured but carries no ${of.statistic}`);
    }
    return { state: "measured", value };
  };
  return measureKpi(id, degradation(operand(kpisAtN?.[of.kpi]), operand(kpisAtOne?.[of.kpi])));
}

/**
 * Compute several statistics over one sample set. Measured only when every statistic
 * is measured at `minN` (each statistic's own floor still applies).
 *
 * @param {number[]} values
 * @param {string[]} statistics e.g. ["p50", "p95", "p99"] or ["mean", "p95"]
 * @param {{minN?: number}} [opts]
 * @returns {{state: "measured", stats: Record<string, number>, n: number} | UnknownReading}
 */
export function summarize(values, statistics, { minN = 1 } = {}) {
  finiteArray(values, "values");
  const n = values.length;
  const stats = {};
  for (const stat of statistics) {
    const r = statisticReading(values, stat, minN);
    if (r.state !== "measured") return unknown(r.reason, n);
    stats[stat] = r.value;
  }
  return { state: "measured", stats, n };
}

/**
 * @param {number[]} values
 * @param {string} stat
 * @param {number} minN
 * @returns {Reading}
 */
function statisticReading(values, stat, minN) {
  const m = /^p(\d+(?:\.\d+)?)$/.exec(stat);
  if (m) return percentile(values, Number(m[1]), { minN });
  switch (stat) {
    case "mean":
      return mean(values, { minN });
    case "max":
      return max(values, { minN });
    case "min":
      return min(values, { minN });
    default:
      throw new RangeError(`statistic ${stat} is not computable from a single sample set`);
  }
}

/**
 * Turn samples into a catalog KPI value (the `kpis.<id>` object of a results row).
 *
 * For a "stats" KPI pass the raw samples; the catalog's statistics and `min_n` apply.
 * For a "value" KPI pass a Reading (e.g. from `slope`, `share`, `degradation`) or
 * `{value, n}`; `min_n` applies to its n, and a count must be an integer.
 *
 * @param {string} id catalog id
 * @param {number[] | {state?: string, value?: number, n: number, reason?: string}} input
 * @param {{coverage?: {observed: number, expected: number}, notes?: string[]}} [extra]
 * @returns {object}
 */
export function measureKpi(id, input, extra = {}) {
  const row = getKpi(id);
  const base = { unit: row.unit };
  const minN = Math.max(row.min_n, ...row.statistics.map(minSamplesForStatistic));
  let out;
  if (row.shape === "stats") {
    const s = summarize(input, row.statistics, { minN });
    out =
      s.state === "measured"
        ? { state: "measured", ...base, n: s.n, stats: s.stats }
        : { state: "unknown", ...base, n: s.n, reason: s.reason };
  } else {
    const reading = input ?? {};
    const n = reading.n;
    if (!Number.isInteger(n) || n < 0) throw new TypeError(`${id}: input.n must be a count`);
    if (reading.state === "not_applicable") {
      out = { state: "not_applicable", ...base, n, reason: reading.reason };
    } else if (reading.state !== undefined && reading.state !== "measured") {
      out = { state: "unknown", ...base, n, reason: reading.reason ?? "insufficient_samples" };
    } else if (n < minN) {
      out = { state: "unknown", ...base, n, reason: "insufficient_samples" };
    } else {
      finiteArray([reading.value], `${id} value`);
      if (row.integer && !Number.isInteger(reading.value)) {
        throw new TypeError(`${id} is a count; got ${reading.value}`);
      }
      out = { state: "measured", ...base, n, value: reading.value };
    }
  }
  if (extra.coverage) out.coverage = { ...extra.coverage };
  if (extra.notes?.length) out.notes = [...extra.notes];
  return out;
}

/**
 * An unknown KPI value with its reason.
 *
 * @param {string} id
 * @param {string} reason a kpi-catalog REASON_CODES key
 * @param {{n?: number, detail?: string}} [opts]
 * @returns {object}
 */
export function unknownKpi(id, reason, { n, detail } = {}) {
  const out = { state: "unknown", unit: getKpi(id).unit, reason };
  if (n !== undefined) out.n = n;
  if (detail) out.detail = detail;
  return out;
}

/**
 * A not_applicable KPI value with its reason.
 *
 * @param {string} id
 * @param {string} reason a kpi-catalog REASON_CODES key
 * @param {{detail?: string}} [opts]
 * @returns {object}
 */
export function notApplicableKpi(id, reason, { detail } = {}) {
  const out = { state: "not_applicable", unit: getKpi(id).unit, reason };
  if (detail) out.detail = detail;
  return out;
}

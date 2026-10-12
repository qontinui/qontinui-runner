/**
 * termhost-bench results files (`termhost-bench/v1`): the document-level KPIs that
 * need every row (degradation against N = 1, the slope over the N ladder), probe
 * incidents mapped into the schema's incident shape, validation BEFORE every write
 * (a file that fails `validate.mjs` is never written), and the redacted copy public
 * CI may publish — the runner's own rows only (D7).
 *
 * Plan `2026-10-11-terminal-page-performance-kpis-across-setups`, Phase 3.
 */

import { renameSync, writeFileSync } from "node:fs";

import { KPI_CATALOG, SCHEMA_ID, getKpi, INCIDENT_KINDS } from "./kpi-catalog.mjs";
import { degradationKpi, measureKpi, notApplicableKpi, slope, unknownKpi } from "./stats.mjs";
import { subjectKey, validateResults } from "./validate.mjs";

const DEGRADATIONS = KPI_CATALOG.filter((k) => k.degradation_of).map((k) => k.id);
const LADDER = KPI_CATALOG.filter((k) => k.needs.includes("n_ladder"));

/**
 * Fill every row's `degradation_*` (against the N = 1 row of the same setup,
 * variant, build role, arm AND run index) and `memory_mb_per_session` (the slope
 * of `memory_mb` over the distinct N of the same subject and run index; unknown
 * below three levels). Mutates and returns `rows`.
 *
 * @param {object[]} rows
 */
export function finalizeLadder(rows) {
  const key = (r) => `${subjectKey(r)}|${r.scenario.run_index}`;
  const baselines = new Map();
  for (const r of rows) if (r.scenario.n === 1) baselines.set(key(r), r);
  for (const r of rows) {
    for (const id of DEGRADATIONS) {
      r.kpis[id] = r.scenario.n === 1 ? notApplicableKpi(id, "baseline_level") : degradationKpi(id, r.kpis, baselines.get(key(r))?.kpis);
    }
  }
  for (const k of LADDER) {
    const base = k.derived_from[0];
    const ladders = new Map();
    for (const r of rows) {
      if (r.kpis[base]?.state !== "measured") continue;
      if (!ladders.has(key(r))) ladders.set(key(r), new Map());
      ladders.get(key(r)).set(r.scenario.n, r.kpis[base].value);
    }
    for (const r of rows) {
      const ladder = ladders.get(key(r)) ?? new Map();
      const xs = [...ladder.keys()];
      r.kpis[k.id] =
        ladder.size >= getKpi(k.id).min_n
          ? measureKpi(k.id, slope(xs, xs.map((x) => ladder.get(x))))
          : unknownKpi(k.id, "too_few_levels", { n: ladder.size });
    }
  }
  return rows;
}

/**
 * A probe incident record (`termhost-probe/v1` `incident`) in the results schema's
 * shape: `{kind, at, detail, stops_run, data}`. The probe's extra `source` field
 * (not allowed by the schema) moves into `data`, with where it happened.
 *
 * @param {object} rec
 * @param {{unit?: string, phase?: string, setup?: string}} [where]
 */
export function convertProbeIncident(rec, where = {}) {
  const kind = Object.prototype.hasOwnProperty.call(INCIDENT_KINDS, rec?.kind) ? rec.kind : "harness_error";
  // The probe's own `data` (the capture source it is about is `data.source`),
  // then where the harness met it; a pre-N8 top-level `source` still lands there.
  const data = { ...(rec?.data && typeof rec.data === "object" && !Array.isArray(rec.data) ? rec.data : {}), ...where };
  if (data.source === undefined && rec?.source !== undefined && rec.source !== null) data.source = rec.source;
  if (kind !== rec?.kind) data.probe_kind = String(rec?.kind);
  return {
    kind,
    at: typeof rec?.at === "string" && /^\d{4}-\d{2}-\d{2}T/.test(rec.at) ? rec.at : new Date().toISOString(),
    detail: typeof rec?.detail === "string" && rec.detail ? rec.detail : `probe incident ${rec?.kind ?? "?"}`,
    stops_run: rec?.stops_run === true,
    data,
  };
}

/** An incident the harness raises itself. */
export function incident(kind, detail, { stopsRun = false, data, at = new Date() } = {}) {
  if (!Object.prototype.hasOwnProperty.call(INCIDENT_KINDS, kind)) throw new RangeError(`unknown incident kind ${kind}`);
  const out = { kind, at: at instanceof Date ? at.toISOString() : at, detail, stops_run: stopsRun };
  if (data !== undefined) out.data = data;
  return out;
}

/**
 * The document.
 *
 * @param {{run: object, machine: object, capabilities: object, rows: object[], incidents: object[]}} o
 */
export function buildDocument({ run, machine, capabilities, rows, incidents }) {
  return { schema: SCHEMA_ID, run, machine, capabilities, rows, incidents };
}

/** Throw with every error when `doc` is not a valid results file. */
export function assertValid(doc, label = "results") {
  const res = validateResults(doc);
  if (!res.valid) throw new Error(`${label} is not a valid termhost-bench/v1 file: ${errorWhere(res.errors)}`);
  return doc;
}

/**
 * A validator error list as paths and keywords only. The messages are left out
 * on purpose: some quote the offending value (`validate.mjs` `validateNumber`),
 * these lines reach public CI logs, and a third-party reading never leaves the
 * job (D7).
 */
export function errorWhere(errors, max = 40) {
  const shown = errors.slice(0, max).map((e) => `${e.path || "/"} [${e.keyword}]`);
  const more = errors.length > max ? `; … ${errors.length - max} more` : "";
  return `${errors.length} error(s): ${shown.join("; ")}${more}`;
}

/**
 * A runner build that could not be proven (D12), still naming what the unit asked
 * for: its role (so head and baseline rows stay distinct) and, when known, the
 * requested commit and source. Never a proven `commit` — the row carries the
 * `unknown_build` label and compare leaves it out.
 *
 * @param {{role?: string, commit?: string, source?: string} | null | undefined} build
 * @param {string} detail
 */
export function unprovenBuild(build, detail) {
  const out = { state: "unknown", reason: "harness_error", detail, role: build?.role ?? "single" };
  if (typeof build?.commit === "string" && /^[0-9a-f]{40}$/.test(build.commit)) out.requested_commit = build.commit;
  if (["worktree", "git_ref", "artifact", "installed"].includes(build?.source)) out.requested_source = build.source;
  return out;
}

/** The results document could not be written as a valid file. */
export class InvalidResults extends Error {
  constructor(message, asidePath) {
    super(message);
    this.name = "InvalidResults";
    this.asidePath = asidePath;
  }
}

/**
 * Validate, then write atomically; an INVALID document is written to `asidePath`
 * instead (a private path inside the run directory, never the published one) so a
 * long run's data survives a harness bug, and an {@link InvalidResults} names only
 * paths and keywords.
 */
export function writeResultsOrSetAside(path, doc, asidePath) {
  const res = validateResults(doc);
  if (res.valid) return writeResults(path, doc);
  writeFileSync(asidePath, `${JSON.stringify(doc, null, 2)}\n`);
  throw new InvalidResults(`the results document is invalid (${errorWhere(res.errors)}); it was set aside, unpublished, at ${asidePath}`, asidePath);
}

/**
 * Validate, then write atomically (temp file + rename). Nothing is written when
 * the document is invalid.
 */
export function writeResults(path, doc) {
  assertValid(doc, path);
  const tmp = `${path}.tmp-${process.pid}`;
  writeFileSync(tmp, `${JSON.stringify(doc, null, 2)}\n`);
  renameSync(tmp, path);
  return path;
}

/**
 * The copy public CI may publish (D7): the runner's own rows, the incidents that
 * concern the runner or no setup, and nothing about any third-party setup. A
 * runner-to-third-party ratio never appears because only runner rows remain.
 * Returns null when there is no runner row to publish.
 */
export function redactRunnerOnly(doc) {
  const rows = doc.rows.filter((r) => r.setup.id === "runner");
  if (rows.length === 0) return null;
  const incidents = doc.incidents.filter((i) => !i.data?.setup || i.data.setup === "runner");
  const capabilities = Object.fromEntries(
    Object.entries(doc.capabilities).filter(([id]) => !/^setup\.(?!runner)/.test(id)),
  );
  return assertValid({ ...doc, rows, incidents, capabilities }, "the redacted results");
}

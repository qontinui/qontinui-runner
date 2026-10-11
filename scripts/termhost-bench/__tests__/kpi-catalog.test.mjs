/**
 * The KPI catalog is complete against the plan, internally consistent, and the
 * committed result schema is exactly what the catalog generates.
 *
 *   node --test scripts/termhost-bench/__tests__/kpi-catalog.test.mjs
 */

import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

import {
  HEADLINE,
  INCIDENT_KINDS,
  KPI_CATALOG,
  KPI_IDS,
  NEEDS,
  N_LADDER,
  REASON_CODES,
  ROW_LABELS,
  getKpi,
  minSamplesForStatistic,
  percentileFloor,
} from "../kpi-catalog.mjs";
import { renderResultSchema } from "../build-schema.mjs";
import { RESULT_SCHEMA_PATH } from "../validate.mjs";

/** The plan's "KPI catalog (termhost-bench/v1)" table, row by row. */
const PLAN_TABLE_IDS = [
  "output_latency_ms",
  "input_latency_ms",
  "frame_interval_ms",
  "switch_latency_ms",
  "spawn_latency_ms",
  "cpu_pct",
  "cpu_ms_per_mb",
  "memory_mb",
  "memory_mb_per_session",
  "gpu_pct",
  "producer_stall_pct",
  "delivered_ratio",
  "hangs",
  "crashes",
  "marker_loss",
];
/** `degradation_<kpi>`: output and input latency p95 and cpu_pct. */
const PLAN_DEGRADATION = {
  degradation_output_latency_ms: { kpi: "output_latency_ms", statistic: "p95" },
  degradation_input_latency_ms: { kpi: "input_latency_ms", statistic: "p95" },
  degradation_cpu_pct: { kpi: "cpu_pct", statistic: "mean" },
};
/** Named in the plan's rows as reported separately, each with the row it belongs to. */
const PLAN_SEPARATE = {
  jank_share: "frame_interval_ms",
  memory_growth_mb_per_min: "memory_mb",
  workload_cpu_pct: "cpu_pct",
  workload_memory_mb: "memory_mb",
  marker_skipped: "marker_loss",
};
/** "Recorded separately and attributed to no setup": the compositor and the WSL VM. */
const PLAN_SHARED = [
  "compositor_cpu_pct",
  "compositor_memory_mb",
  "wsl_vm_cpu_pct",
  "wsl_vm_memory_mb",
];

const FIELDS = [
  "id",
  "label",
  "description",
  "unit",
  "direction",
  "shape",
  "statistics",
  "sample_unit",
  "min_n",
  "instrument",
  "needs",
  "comparable",
  "attribution",
  "derived_from",
  "degradation_of",
  "exclusion",
  "headline_statistic",
  "value_range",
  "integer",
];

test("every KPI the plan's catalog table names is in the catalog", () => {
  for (const id of [
    ...PLAN_TABLE_IDS,
    ...Object.keys(PLAN_DEGRADATION),
    ...Object.keys(PLAN_SEPARATE),
    ...PLAN_SHARED,
  ]) {
    assert.ok(KPI_IDS.includes(id), `catalog is missing ${id}`);
  }
});

test("the catalog holds nothing the plan does not name", () => {
  const named = new Set([
    ...PLAN_TABLE_IDS,
    ...Object.keys(PLAN_DEGRADATION),
    ...Object.keys(PLAN_SEPARATE),
    ...PLAN_SHARED,
  ]);
  assert.deepEqual(
    KPI_IDS.filter((id) => !named.has(id)),
    [],
  );
  assert.equal(new Set(KPI_IDS).size, KPI_IDS.length, "ids are unique");
});

test("every row carries every field, with valid values", () => {
  for (const row of KPI_CATALOG) {
    assert.deepEqual(Object.keys(row).sort(), [...FIELDS].sort(), `${row.id} fields`);
    assert.match(row.id, /^[a-z][a-z0-9_]*$/);
    for (const key of ["label", "description", "unit", "sample_unit", "instrument"]) {
      assert.ok(typeof row[key] === "string" && row[key].length > 0, `${row.id}.${key}`);
    }
    assert.ok(
      ["lower_is_better", "higher_is_better", "neutral"].includes(row.direction),
      `${row.id}.direction`,
    );
    assert.ok(["stats", "value"].includes(row.shape), `${row.id}.shape`);
    assert.ok(row.statistics.length >= 1, `${row.id}.statistics`);
    if (row.shape === "value")
      assert.equal(row.statistics.length, 1, `${row.id}: a value KPI names one statistic`);
    assert.ok(Number.isInteger(row.min_n) && row.min_n >= 1, `${row.id}.min_n`);
    for (const need of row.needs)
      assert.ok(need in NEEDS, `${row.id} needs unknown capability ${need}`);
    assert.ok(["host", "workload", "shared"].includes(row.attribution), `${row.id}.attribution`);
    assert.equal(typeof row.comparable, "boolean");
    assert.equal(typeof row.integer, "boolean");
    if (row.headline_statistic !== null)
      assert.ok(row.statistics.includes(row.headline_statistic), `${row.id}.headline_statistic`);
    if (row.integer) assert.equal(row.unit, "count", `${row.id}: only counts are integers`);
  }
});

test("min_n never lets a statistic carry a name it has not earned", () => {
  for (const row of KPI_CATALOG) {
    for (const stat of row.statistics) {
      assert.ok(
        row.min_n >= minSamplesForStatistic(stat),
        `${row.id}: min_n ${row.min_n} < ${stat}'s floor ${minSamplesForStatistic(stat)}`,
      );
    }
  }
  assert.equal(getKpi("output_latency_ms").min_n, 100, "a p99 needs 100 samples");
});

test("percentileFloor is ceil(100 / (100 - p))", () => {
  assert.equal(percentileFloor(50), 2);
  assert.equal(percentileFloor(90), 10);
  assert.equal(percentileFloor(95), 20);
  assert.equal(percentileFloor(99), 100);
  assert.equal(percentileFloor(100), 1);
  assert.equal(percentileFloor(99.9), 1000);
  assert.throws(() => percentileFloor(0), RangeError);
  assert.throws(() => percentileFloor(101), RangeError);
  assert.equal(minSamplesForStatistic("slope"), 3);
  assert.equal(minSamplesForStatistic("ratio"), 2);
  assert.throws(() => minSamplesForStatistic("median"), RangeError);
});

test("degradation rows are the N / N=1 ratio of the KPI and statistic the plan names", () => {
  for (const [id, of] of Object.entries(PLAN_DEGRADATION)) {
    const row = getKpi(id);
    assert.deepEqual(row.degradation_of, of);
    assert.ok(getKpi(of.kpi).statistics.includes(of.statistic));
    assert.deepEqual(row.derived_from, [of.kpi]);
    assert.ok(row.needs.includes("baseline_level"));
    assert.equal(row.unit, "ratio");
  }
  for (const row of KPI_CATALOG) {
    if (!(row.id in PLAN_DEGRADATION)) assert.equal(row.degradation_of, null, `${row.id}`);
  }
});

test("derived_from references exist, and separately reported rows point at their parent", () => {
  for (const row of KPI_CATALOG)
    for (const ref of row.derived_from) assert.ok(KPI_IDS.includes(ref), `${row.id} -> ${ref}`);
  assert.deepEqual(getKpi("jank_share").derived_from, ["frame_interval_ms"]);
  assert.deepEqual(getKpi("memory_growth_mb_per_min").derived_from, ["memory_mb"]);
  assert.deepEqual(getKpi("memory_mb_per_session").derived_from, ["memory_mb"]);
});

test("workload rows are attributed to the workload, shared rows to no setup", () => {
  for (const id of ["workload_cpu_pct", "workload_memory_mb"])
    assert.equal(getKpi(id).attribution, "workload");
  for (const id of PLAN_SHARED) {
    assert.equal(getKpi(id).attribution, "shared");
    assert.equal(
      getKpi(id).comparable,
      false,
      `${id} is attributed to no setup, so it is not compared`,
    );
  }
  for (const id of PLAN_TABLE_IDS) assert.equal(getKpi(id).attribution, "host", id);
  assert.match(getKpi("cpu_pct").description, /parent session shell/);
  assert.match(getKpi("cpu_pct").description, /conhost\.exe/);
});

test("hangs, marker_loss and marker_skipped exclude invalid intervals; nothing else does", () => {
  const excluded = KPI_CATALOG.filter((r) => r.exclusion === "valid_intervals").map((r) => r.id);
  assert.deepEqual(excluded.sort(), ["hangs", "marker_loss", "marker_skipped"]);
  assert.match(getKpi("marker_loss").description, /marker_skipped/);
  assert.match(getKpi("marker_skipped").description, /not loss/);
  assert.ok("occluded" in REASON_CODES && "operator_activity" in REASON_CODES);
});

test("the headline KPIs and the N ladder match the plan", () => {
  const headline = KPI_CATALOG.filter((r) => r.headline_statistic !== null).map(
    (r) => `${r.id}:${r.headline_statistic}`,
  );
  assert.deepEqual(headline.sort(), [
    "cpu_pct:mean",
    "hangs:count",
    "input_latency_ms:p95",
    "memory_mb:value",
    "output_latency_ms:p95",
    "producer_stall_pct:mean",
    "spawn_latency_ms:p95",
    "switch_latency_ms:p95",
  ]);
  assert.deepEqual([...N_LADDER], [1, 5, 15, 40]);
  assert.deepEqual(
    { ...HEADLINE, arms: [...HEADLINE.arms] },
    { environment: "windows-desktop", n: 15, arms: ["focus", "tiled"] },
  );
});

test("vocabularies are snake_case codes with a description", () => {
  for (const vocab of [REASON_CODES, ROW_LABELS, INCIDENT_KINDS, NEEDS]) {
    for (const [code, text] of Object.entries(vocab)) {
      assert.match(code, /^[a-z][a-z0-9_]*$/);
      assert.ok(text.length > 10, code);
    }
  }
});

test("the catalog is frozen", () => {
  assert.throws(() => {
    KPI_CATALOG[0].min_n = 1;
  }, TypeError);
  assert.throws(() => getKpi("nope"), RangeError);
});

test("result-schema.json is byte-identical to what the catalog generates", () => {
  assert.equal(
    readFileSync(RESULT_SCHEMA_PATH, "utf8"),
    renderResultSchema(),
    "run: node scripts/termhost-bench/build-schema.mjs --write",
  );
});

test("KPIs counted in refresh intervals need a known refresh rate, with a reason when unknown", () => {
  assert.ok("refresh_rate_unknown" in REASON_CODES);
  assert.ok("refresh_rate" in NEEDS);
  for (const id of ["jank_share", "marker_loss", "marker_skipped"]) {
    assert.ok(getKpi(id).needs.includes("refresh_rate"), `${id} needs the refresh rate`);
  }
});

test("marker_skipped is neutral: coalescing is neither loss nor a gain", () => {
  assert.equal(getKpi("marker_skipped").direction, "neutral");
});

test("a degradation ratio of 0 is a valid measurement (minimum 0, not exclusive)", () => {
  for (const row of KPI_CATALOG.filter((r) => r.degradation_of)) {
    assert.deepEqual({ ...row.value_range }, { minimum: 0 }, row.id);
  }
});

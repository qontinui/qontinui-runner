/**
 * Statistics at 0, 1 and many samples and with ties; every function answers
 * unknown below its minimum instead of a plausible number.
 *
 *   node --test scripts/termhost-bench/__tests__/stats.test.mjs
 */

import { test } from "node:test";
import assert from "node:assert/strict";

import {
  degradation,
  max,
  mean,
  measureKpi,
  min,
  nearestRank,
  notApplicableKpi,
  percentile,
  share,
  slope,
  summarize,
  unknownKpi,
} from "../stats.mjs";
import { validateResults } from "../validate.mjs";
import { row, windowsDesktopResults } from "./helpers/results.mjs";

const range = (a, b) => Array.from({ length: b - a + 1 }, (_, i) => a + i);
const shuffled = (values) =>
  values
    .map((v, i) => [((i * 7919) % values.length) + i / 1e6, v])
    .sort((a, b) => a[0] - b[0])
    .map(([, v]) => v);

test("0 samples: every statistic is unknown(insufficient_samples) with n = 0", () => {
  for (const r of [
    percentile([], 50),
    percentile([], 100),
    mean([]),
    max([]),
    min([]),
    share([], () => true),
    slope([], []),
  ]) {
    assert.deepEqual(r, { state: "unknown", reason: "insufficient_samples", n: 0 });
  }
  assert.deepEqual(summarize([], ["mean"]), {
    state: "unknown",
    reason: "insufficient_samples",
    n: 0,
  });
});

test("1 sample: mean, max, min and p100 are measured; p50, p95, p99 and a slope are not", () => {
  assert.deepEqual(mean([7]), { state: "measured", value: 7, n: 1 });
  assert.deepEqual(max([7]), { state: "measured", value: 7, n: 1 });
  assert.deepEqual(min([7]), { state: "measured", value: 7, n: 1 });
  assert.deepEqual(percentile([7], 100), { state: "measured", value: 7, n: 1 });
  for (const p of [50, 95, 99])
    assert.equal(percentile([7], p).state, "unknown", `p${p} of one sample is just that sample`);
  assert.equal(slope([1], [1]).state, "unknown");
});

test("percentile floors: p50 needs 2, p95 needs 20, p99 needs 100 samples", () => {
  assert.equal(percentile([1, 2], 50).state, "measured");
  assert.equal(percentile(range(1, 19), 95).state, "unknown");
  assert.equal(percentile(range(1, 20), 95).state, "measured");
  assert.equal(percentile(range(1, 99), 99).state, "unknown");
  assert.equal(percentile(range(1, 100), 99).state, "measured");
  assert.equal(
    percentile(range(1, 30), 50, { minN: 31 }).state,
    "unknown",
    "a caller may raise the floor",
  );
  assert.equal(
    percentile(range(1, 10), 95, { minN: 1 }).state,
    "unknown",
    "a caller may not lower it",
  );
});

test("many samples: nearest rank, independent of input order, never interpolated", () => {
  const values = shuffled(range(1, 100));
  assert.deepEqual(percentile(values, 50), { state: "measured", value: 50, n: 100 });
  assert.deepEqual(percentile(values, 95), { state: "measured", value: 95, n: 100 });
  assert.deepEqual(percentile(values, 99), { state: "measured", value: 99, n: 100 });
  assert.deepEqual(percentile(values, 100), { state: "measured", value: 100, n: 100 });
  const halves = [0, 10, 20, 30];
  assert.equal(percentile(halves, 50).value, 10, "nearest rank picks an observed value, not 15");
  assert.equal(mean(values).value, 50.5);
  assert.equal(max(values).value, 100);
  assert.equal(min(values).value, 1);
  assert.equal(nearestRank(95, 20), 19);
  assert.equal(nearestRank(99, 100), 99);
  assert.equal(nearestRank(50, 1), 1);
  assert.equal(nearestRank(99.9, 1000), 999);
  assert.equal(nearestRank(0.1, 1000), 1);
});

test("ties: repeated values are ranked like any other", () => {
  const ties = [5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5];
  for (const p of [50, 95]) assert.equal(percentile(ties, p).value, 5);
  assert.equal(percentile([1, 2, 2, 2, 3], 50).value, 2);
  assert.equal(percentile([3, 1, 2, 2, 2], 50).value, 2);
  const tail = [...range(1, 18), 40, 40];
  assert.equal(percentile(tail, 95).value, 40, "rank 19 of 20 lands on the tied tail");
  assert.equal(share([1, 2, 2, 3], (v) => v === 2).value, 50);
  assert.equal(slope([1, 1, 2, 2], [1, 1, 3, 3]).value, 2, "ties in x still define a slope");
});

test("slope: least squares, at least 3 points, two distinct x values", () => {
  assert.deepEqual(slope([1, 5, 15, 40], [100, 180, 380, 880]), {
    state: "measured",
    value: 20,
    n: 4,
  });
  assert.equal(slope([0, 1, 2], [0, 1, 4]).value, 2);
  assert.deepEqual(slope([1, 2], [1, 2]), {
    state: "unknown",
    reason: "insufficient_samples",
    n: 2,
  });
  assert.deepEqual(slope([3, 3, 3], [1, 2, 3]), {
    state: "unknown",
    reason: "degenerate_input",
    n: 3,
  });
  assert.throws(() => slope([1, 2, 3], [1, 2]), RangeError);
});

test("share is a percentage of the samples", () => {
  assert.deepEqual(
    share([10, 40, 50, 70], (v) => v > 33.3),
    { state: "measured", value: 75, n: 4 },
  );
  assert.equal(share([1, 2], () => false).value, 0);
  assert.equal(share(range(1, 99), (v) => v > 0, { minN: 100 }).state, "unknown");
});

test("degradation: measured operands, a zero baseline, and propagated reasons", () => {
  const m = (value) => ({ state: "measured", value, n: 100 });
  assert.deepEqual(degradation(m(90), m(30)), { state: "measured", value: 3, n: 2 });
  assert.deepEqual(degradation(m(90), { state: "unknown", reason: "occluded" }), {
    state: "unknown",
    reason: "no_baseline_level",
    n: 1,
  });
  assert.deepEqual(degradation({ state: "unknown", reason: "occluded" }, m(30)), {
    state: "unknown",
    reason: "occluded",
    n: 1,
  });
  assert.deepEqual(degradation(m(90), m(0)), { state: "unknown", reason: "zero_baseline", n: 2 });
  assert.deepEqual(degradation(undefined, undefined), {
    state: "unknown",
    reason: "insufficient_samples",
    n: 0,
  });
});

test("non-finite samples are a defect, not data", () => {
  assert.throws(() => mean([1, Number.NaN]), TypeError);
  assert.throws(() => percentile([1, null], 50), TypeError);
  assert.throws(() => max("12"), TypeError);
  assert.throws(() => percentile([1, 2], 0), RangeError);
  assert.throws(() => percentile([1, 2], 101), RangeError);
});

test("summarize is measured only when every statistic is", () => {
  assert.deepEqual(summarize(range(1, 20), ["mean", "p95"]), {
    state: "measured",
    stats: { mean: 10.5, p95: 19 },
    n: 20,
  });
  assert.deepEqual(summarize(range(1, 19), ["mean", "p95"]), {
    state: "unknown",
    reason: "insufficient_samples",
    n: 19,
  });
  assert.throws(() => summarize([1, 2, 3], ["slope"]), RangeError);
});

test("measureKpi applies the catalog's statistics and min_n", () => {
  assert.deepEqual(measureKpi("output_latency_ms", range(1, 99)), {
    state: "unknown",
    unit: "ms",
    n: 99,
    reason: "insufficient_samples",
  });
  const out = measureKpi("output_latency_ms", shuffled(range(1, 200)));
  assert.deepEqual(out, {
    state: "measured",
    unit: "ms",
    n: 200,
    stats: { p50: 100, p95: 190, p99: 198 },
  });
  assert.deepEqual(measureKpi("hangs", { value: 2, n: 60 }), {
    state: "measured",
    unit: "count",
    n: 60,
    value: 2,
  });
  assert.deepEqual(measureKpi("hangs", { value: 0, n: 59 }), {
    state: "unknown",
    unit: "count",
    n: 59,
    reason: "insufficient_samples",
  });
  assert.throws(() => measureKpi("hangs", { value: 0.5, n: 60 }), /count/);
  assert.deepEqual(measureKpi("memory_mb_per_session", slope([1, 5, 15], [400, 480, 680])), {
    state: "measured",
    unit: "MB/session",
    n: 3,
    value: 20,
  });
  assert.deepEqual(measureKpi("memory_mb_per_session", slope([1, 5], [400, 480])), {
    state: "unknown",
    unit: "MB/session",
    n: 2,
    reason: "insufficient_samples",
  });
  assert.deepEqual(
    measureKpi(
      "degradation_cpu_pct",
      degradation({ state: "measured", value: 50 }, { state: "measured", value: 0 }),
    ),
    {
      state: "unknown",
      unit: "ratio",
      n: 2,
      reason: "zero_baseline",
    },
  );
  assert.deepEqual(
    measureKpi("producer_stall_pct", [1, 3], { coverage: { observed: 2, expected: 15 } }).coverage,
    { observed: 2, expected: 15 },
  );
  assert.throws(() => measureKpi("nope", []), RangeError);
});

test("every value stats.mjs builds is accepted by the result schema", () => {
  const doc = windowsDesktopResults();
  const kpis = { ...doc.rows[1].kpis };
  kpis.input_latency_ms = measureKpi("input_latency_ms", range(1, 40));
  kpis.gpu_pct = notApplicableKpi("gpu_pct", "gpu_counters_unavailable", { detail: "no WDDM GPU" });
  kpis.hangs = unknownKpi("hangs", "operator_activity", { n: 12 });
  kpis.memory_growth_mb_per_min = measureKpi(
    "memory_growth_mb_per_min",
    slope(
      range(1, 30),
      range(1, 30).map((x) => 800 - x),
    ),
  );
  doc.rows = [row({ kpis })];
  const r = validateResults(doc);
  assert.deepEqual(r.errors, []);
  assert.equal(kpis.input_latency_ms.state, "unknown");
  assert.equal(
    kpis.memory_growth_mb_per_min.value,
    -1,
    "memory can shrink: the slope is unbounded below",
  );
});

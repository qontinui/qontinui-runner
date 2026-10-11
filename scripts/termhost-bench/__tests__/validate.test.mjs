/**
 * The JSON Schema subset validator, and the results-file contract it enforces:
 * the fixtures pass; a KPI with no state, a measured KPI below its minimum n and an
 * unmeasured KPI rendered as 0 are rejected (plan Phase 1 "Tests" and "Arming").
 *
 *   node --test scripts/termhost-bench/__tests__/validate.test.mjs
 */

import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import {
  assertSupportedSchema,
  compileSchema,
  deepEqual,
  runCli,
  validateAgainst,
  validateResults,
} from "../validate.mjs";

const FIXTURES = join(dirname(fileURLToPath(import.meta.url)), "fixtures");
const load = (name) => JSON.parse(readFileSync(join(FIXTURES, name), "utf8"));
const desktop = () => load("results-windows-desktop.json");

/** Assert `doc` is rejected and some error matches `pattern` (path + message). */
function rejects(doc, pattern) {
  const r = validateResults(doc);
  assert.equal(r.valid, false, "expected the document to be rejected");
  const lines = r.errors.map((e) => `${e.path} [${e.keyword}] ${e.message}`);
  assert.ok(
    lines.some((l) => pattern.test(l)),
    `no error matched ${pattern}:\n${lines.join("\n")}`,
  );
}

// ---------------------------------------------------------------------------
// The subset validator
// ---------------------------------------------------------------------------

test("type, enum and const", () => {
  assert.equal(validateAgainst({ type: "integer" }, 3).valid, true);
  assert.equal(validateAgainst({ type: "integer" }, 3.5).valid, false);
  assert.equal(validateAgainst({ type: "number" }, 3.5).valid, true);
  assert.equal(validateAgainst({ type: ["string", "null"] }, null).valid, true);
  assert.equal(validateAgainst({ type: "object" }, []).valid, false);
  assert.equal(validateAgainst({ type: "array" }, {}).valid, false);
  assert.equal(validateAgainst({ enum: ["a", 1] }, 1).valid, true);
  assert.equal(validateAgainst({ enum: ["a", 1] }, "b").valid, false);
  assert.equal(validateAgainst({ const: { a: [1] } }, { a: [1] }).valid, true);
  assert.equal(validateAgainst({ const: false }, 0).valid, false);
});

test("numeric and string bounds", () => {
  const s = { minimum: 1, maximum: 5, exclusiveMinimum: 0, exclusiveMaximum: 6 };
  assert.equal(validateAgainst(s, 1).valid, true);
  assert.equal(validateAgainst(s, 0.5).valid, false);
  assert.equal(validateAgainst(s, 5.5).valid, false);
  assert.equal(validateAgainst({ exclusiveMinimum: 0 }, 0).valid, false);
  assert.equal(validateAgainst({ exclusiveMaximum: 6 }, 6).valid, false);
  assert.equal(
    validateAgainst({ minLength: 2, maxLength: 3 }, "✓✓").valid,
    true,
    "length counts code points",
  );
  assert.equal(validateAgainst({ minLength: 2 }, "a").valid, false);
  assert.equal(validateAgainst({ maxLength: 1 }, "ab").valid, false);
  assert.equal(validateAgainst({ pattern: "^sha256:[0-9a-f]{4}$" }, "sha256:abcd").valid, true);
  assert.equal(validateAgainst({ pattern: "^sha256:" }, "md5:ab").valid, false);
});

test("arrays: items, minItems, maxItems, uniqueItems", () => {
  const s = { items: { type: "integer" }, minItems: 1, maxItems: 3, uniqueItems: true };
  assert.equal(validateAgainst(s, [1, 2]).valid, true);
  assert.equal(validateAgainst(s, []).valid, false);
  assert.equal(validateAgainst(s, [1, 2, 3, 4]).valid, false);
  assert.equal(validateAgainst(s, [1, 1]).valid, false);
  assert.equal(validateAgainst(s, [1, "x"]).valid, false);
});

test("objects: required, properties, additionalProperties, propertyNames, min/maxProperties", () => {
  const s = {
    required: ["a"],
    properties: { a: { type: "string" } },
    additionalProperties: { type: "number" },
    propertyNames: { pattern: "^[a-z]+$" },
    minProperties: 1,
    maxProperties: 2,
  };
  assert.equal(validateAgainst(s, { a: "x", b: 1 }).valid, true);
  assert.equal(validateAgainst(s, { b: 1 }).valid, false, "required");
  assert.equal(validateAgainst(s, { a: 1 }).valid, false, "property type");
  assert.equal(validateAgainst(s, { a: "x", b: "y" }).valid, false, "additionalProperties schema");
  assert.equal(validateAgainst(s, { a: "x", B: 1 }).valid, false, "propertyNames");
  assert.equal(validateAgainst(s, { a: "x", b: 1, c: 2 }).valid, false, "maxProperties");
  assert.equal(validateAgainst({ minProperties: 1 }, {}).valid, false, "minProperties");
  assert.equal(
    validateAgainst({ additionalProperties: false, properties: { a: true } }, { a: 1, z: 1 }).valid,
    false,
  );
});

test("allOf, anyOf, oneOf, not and if/then/else", () => {
  assert.equal(validateAgainst({ allOf: [{ minimum: 1 }, { maximum: 2 }] }, 3).valid, false);
  assert.equal(validateAgainst({ anyOf: [{ type: "string" }, { minimum: 10 }] }, 11).valid, true);
  assert.equal(validateAgainst({ anyOf: [{ type: "string" }, { minimum: 10 }] }, 9).valid, false);
  assert.equal(validateAgainst({ oneOf: [{ minimum: 1 }, { minimum: 2 }] }, 1).valid, true);
  assert.equal(
    validateAgainst({ oneOf: [{ minimum: 1 }, { minimum: 2 }] }, 3).valid,
    false,
    "two branches match",
  );
  assert.equal(validateAgainst({ not: { type: "string" } }, "x").valid, false);
  const cond = {
    if: { properties: { s: { const: "m" } } },
    then: { required: ["v"] },
    else: { not: { required: ["v"] } },
  };
  assert.equal(validateAgainst(cond, { s: "m", v: 1 }).valid, true);
  assert.equal(validateAgainst(cond, { s: "m" }).valid, false);
  assert.equal(validateAgainst(cond, { s: "u", v: 1 }).valid, false);
  assert.equal(validateAgainst(cond, { s: "u" }).valid, true);
});

test("$ref resolves local pointers, including escaped segments", () => {
  const s = { $defs: { "a/b": { type: "integer" } }, properties: { x: { $ref: "#/$defs/a~1b" } } };
  assert.equal(validateAgainst(s, { x: 1 }).valid, true);
  assert.equal(validateAgainst(s, { x: "1" }).valid, false);
  assert.equal(validateAgainst(false, 1).valid, false, "a false schema allows nothing");
});

test("FAIL CLOSED: an unsupported keyword, a remote $ref or a dangling $ref is refused at compile time", () => {
  for (const keyword of [
    "patternProperties",
    "dependentRequired",
    "unevaluatedProperties",
    "contains",
    "multipleOf",
    "$dynamicRef",
  ]) {
    assert.throws(
      () => compileSchema({ [keyword]: {} }),
      (e) => e.message.includes(`unsupported schema keyword "${keyword}"`),
      keyword,
    );
  }
  assert.throws(
    () => compileSchema({ properties: { a: { prefixItems: [] } } }),
    /unsupported schema keyword "prefixItems" at #\/properties\/a/,
  );
  assert.throws(() => compileSchema({ $ref: "https://example.com/s.json" }), /only local/);
  assert.throws(() => compileSchema({ $ref: "#/$defs/missing" }), /does not resolve/);
  assert.throws(() => assertSupportedSchema({ type: "decimal" }), /unknown type/);
  assert.doesNotThrow(() =>
    compileSchema({ title: "t", description: "d", format: "date-time", $comment: "c" }),
  );
});

test("deepEqual treats JSON values structurally", () => {
  assert.equal(deepEqual({ a: [1, { b: 2 }] }, { a: [1, { b: 2 }] }), true);
  assert.equal(deepEqual({ a: 1, b: 2 }, { b: 2, a: 1 }), true);
  assert.equal(deepEqual([1], { 0: 1 }), false);
  assert.equal(deepEqual(null, {}), false);
});

// ---------------------------------------------------------------------------
// Results files
// ---------------------------------------------------------------------------

test("the schema accepts the fixtures", () => {
  for (const name of ["results-windows-desktop.json", "results-linux-xvfb.json"]) {
    const r = validateResults(load(name));
    assert.deepEqual(r.errors, [], name);
    assert.equal(r.valid, true, name);
  }
});

test("REJECTS a KPI with no state", () => {
  const doc = desktop();
  delete doc.rows[0].kpis.output_latency_ms.state;
  rejects(doc, /kpis\/output_latency_ms \[required\] missing required property "state"/);
});

test("REJECTS a measured KPI below its minimum n", () => {
  const doc = desktop();
  doc.rows[1].kpis.output_latency_ms.n = 99;
  rejects(doc, /kpis\/output_latency_ms\/n \[minimum\] 99 < minimum 100/);
  const hangs = desktop();
  hangs.rows[1].kpis.hangs.n = 59;
  rejects(hangs, /kpis\/hangs\/n \[minimum\] 59 < minimum 60/);
});

test("REJECTS an unmeasured KPI rendered as 0", () => {
  const unknownZero = desktop();
  unknownZero.rows[1].kpis.hangs = {
    state: "unknown",
    unit: "count",
    reason: "occluded",
    value: 0,
  };
  rejects(unknownZero, /kpis\/hangs \[not\]/);
  const naStats = desktop();
  naStats.rows[1].kpis.gpu_pct = {
    state: "not_applicable",
    unit: "%",
    reason: "gpu_counters_unavailable",
    stats: { mean: 0, p95: 0 },
  };
  rejects(naStats, /kpis\/gpu_pct \[not\]/);
  const zeroSamples = desktop();
  zeroSamples.rows[1].kpis.crashes = { state: "measured", unit: "count", n: 0, value: 0 };
  rejects(zeroSamples, /kpis\/crashes\/n \[minimum\] 0 < minimum 1/);
});

test("REJECTS an unmeasured KPI with no reason, and a measured one with a reason", () => {
  const noReason = desktop();
  noReason.rows[1].kpis.gpu_pct = { state: "unknown", unit: "%" };
  rejects(noReason, /kpis\/gpu_pct \[required\] missing required property "reason"/);
  const measuredReason = desktop();
  measuredReason.rows[1].kpis.hangs.reason = "occluded";
  rejects(measuredReason, /kpis\/hangs \[not\]/);
  const badReason = desktop();
  badReason.rows[1].kpis.gpu_pct = { state: "unknown", unit: "%", reason: "felt_slow" };
  rejects(badReason, /kpis\/gpu_pct\/reason \[enum\]/);
});

test("REJECTS an omitted KPI, an unknown KPI id and a wrong unit", () => {
  const omitted = desktop();
  delete omitted.rows[0].kpis.marker_skipped;
  rejects(omitted, /kpis \[required\] missing required property "marker_skipped"/);
  const extra = desktop();
  extra.rows[0].kpis.vibes_pct = { state: "measured", unit: "%", n: 1, value: 1 };
  rejects(extra, /kpis\/vibes_pct \[additionalProperties\]/);
  const unit = desktop();
  unit.rows[1].kpis.output_latency_ms.unit = "s";
  rejects(unit, /kpis\/output_latency_ms\/unit \[const\]/);
});

test("REJECTS measured statistics that do not match the catalog", () => {
  const missing = desktop();
  delete missing.rows[1].kpis.output_latency_ms.stats.p99;
  rejects(missing, /output_latency_ms\/stats \[required\] missing required property "p99"/);
  const extra = desktop();
  extra.rows[1].kpis.output_latency_ms.stats.p90 = 50;
  rejects(extra, /output_latency_ms\/stats\/p90 \[additionalProperties\]/);
  const valueForStats = desktop();
  valueForStats.rows[1].kpis.output_latency_ms.value = 40;
  rejects(valueForStats, /kpis\/output_latency_ms \[not\]/);
  const negative = desktop();
  negative.rows[1].kpis.output_latency_ms.stats.p50 = -1;
  rejects(negative, /output_latency_ms\/stats\/p50 \[minimum\]/);
});

test("REJECTS out-of-range values: a fractional count, a share over 100, a negative ratio", () => {
  const fractional = desktop();
  fractional.rows[1].kpis.hangs.value = 1.5;
  rejects(fractional, /kpis\/hangs\/value \[type\] expected integer/);
  const share = desktop();
  share.rows[1].kpis.jank_share.value = 101;
  rejects(share, /kpis\/jank_share\/value \[maximum\]/);
  const ratio = desktop();
  ratio.rows[1].kpis.degradation_cpu_pct.value = -0.5;
  rejects(ratio, /degradation_cpu_pct\/value \[minimum\]/);
});

test("ACCEPTS a degradation ratio of 0 (the KPI measured 0 at N)", () => {
  const zero = desktop();
  zero.rows[1].kpis.degradation_cpu_pct.value = 0;
  assert.deepEqual(validateResults(zero).errors, []);
});

test("REJECTS a row without its profile_calibrated flag, and an unknown environment", () => {
  const noFlag = desktop();
  delete noFlag.rows[2].scenario.profile_calibrated;
  rejects(noFlag, /rows\/2\/scenario \[required\] missing required property "profile_calibrated"/);
  const env = desktop();
  env.machine.environment = "macos-desktop";
  rejects(env, /machine\/environment \[enum\]/);
});

test("REJECTS a diagnostic labelled comparable, and an unknown diagnostic carrying data", () => {
  const comparable = desktop();
  comparable.rows[1].diagnostics.webgl_panes.comparable = true;
  rejects(comparable, /diagnostics\/webgl_panes\/comparable \[const\]/);
  const data = desktop();
  data.rows[1].diagnostics.transport_counters.data = { frames: 0 };
  rejects(data, /diagnostics\/transport_counters \[not\]/);
});

test("REJECTS an absent field that is neither a value nor an honest absence", () => {
  const nullGpu = desktop();
  nullGpu.machine.gpu = null;
  rejects(nullGpu, /machine\/gpu \[anyOf\]/);
  const emptyAbsence = desktop();
  emptyAbsence.rows[0].setup.settings_digest = { state: "unknown" };
  rejects(emptyAbsence, /setup\/settings_digest \[anyOf\]/);
});

test("REJECTS cross-field contradictions the schema cannot state", () => {
  const order = desktop();
  const stats = order.rows[1].kpis.output_latency_ms.stats;
  stats.p50 = stats.p95 + 10;
  rejects(order, new RegExp(`output_latency_ms/stats \\[semantic\\] p50 ${stats.p50} > p95`));
  const meanMax = desktop();
  meanMax.rows[1].kpis.producer_stall_pct.stats = { mean: 5, max: 4 };
  rejects(meanMax, /producer_stall_pct\/stats \[semantic\] mean 5 > max 4/);
  const runIndex = desktop();
  runIndex.rows[1].scenario.run_index = 3;
  rejects(runIndex, /scenario\/run_index \[semantic\] run_index 3 >= run_count 3/);
  const vTarget = desktop();
  vTarget.rows[1].scenario.v_target = 16;
  rejects(vTarget, /scenario\/v_target \[semantic\]/);
  const vActual = desktop();
  vActual.rows[1].scenario.v_actual = 16;
  rejects(vActual, /scenario\/v_actual \[semantic\]/);
  const coverage = desktop();
  coverage.rows[2].kpis.producer_stall_pct.coverage = { observed: 16, expected: 15 };
  rejects(coverage, /coverage \[semantic\] observed 16 > expected 15/);
  const duplicate = desktop();
  duplicate.rows.push(structuredClone(duplicate.rows[1]));
  rejects(duplicate, /rows\/5 \[semantic\] duplicate row/);
});

test("REJECTS a measured degradation with no N = 1 row behind it", () => {
  const noBaseline = desktop();
  noBaseline.rows.splice(3, 2); // drop the N = 1 tiled baselines
  rejects(
    noBaseline,
    /rows\/1\/kpis\/degradation_output_latency_ms \[semantic\] measured degradation_output_latency_ms has no N = 1 row/,
  );
  rejects(noBaseline, /rows\/2\/kpis\/degradation_cpu_pct \[semantic\]/);
});

test("REJECTS a baseline that is not the same setup, variant, build role and arm", () => {
  const setup = desktop();
  setup.rows[3].setup.id = "windows-terminal";
  rejects(
    setup,
    /rows\/1\/kpis\/degradation_cpu_pct \[semantic\] measured degradation_cpu_pct has no N = 1 row/,
  );
  const variant = desktop();
  variant.rows[3].setup.variant = "ghostty";
  rejects(
    variant,
    /rows\/1\/kpis\/degradation_cpu_pct \[semantic\] measured degradation_cpu_pct has no N = 1 row/,
  );
  const role = desktop();
  role.rows[3].setup.build.role = "baseline";
  rejects(role, /rows\/1\/kpis\/degradation_cpu_pct \[semantic\]/);
  const arm = desktop();
  arm.rows[4].scenario.arm = "focus";
  rejects(arm, /rows\/2\/kpis\/degradation_cpu_pct \[semantic\]/);
});

test("REJECTS a baseline whose base KPI is not measured, and a measured degradation on an N = 1 row", () => {
  const unmeasuredBase = desktop();
  unmeasuredBase.rows[3].kpis.output_latency_ms = {
    state: "unknown",
    unit: "ms",
    reason: "occluded",
  };
  rejects(
    unmeasuredBase,
    /rows\/1\/kpis\/degradation_output_latency_ms \[semantic\] .*with a measured output_latency_ms/,
  );
  const onBaseline = desktop();
  onBaseline.rows[0].kpis.degradation_cpu_pct = {
    state: "measured",
    unit: "ratio",
    n: 2,
    value: 1,
  };
  rejects(
    onBaseline,
    /rows\/0\/kpis\/degradation_cpu_pct \[semantic\] a measured degradation_cpu_pct on an N = 1 row/,
  );
});

test("$ref checking walks a property merely NAMED enum / const / default / examples", () => {
  const dangling = { properties: { enum: { $ref: "#/$defs/missing" } } };
  assert.throws(() => compileSchema(dangling), /does not resolve/);
  const named = {
    $defs: { n: { type: "integer" } },
    properties: { const: { $ref: "#/$defs/n" }, default: { $ref: "#/$defs/n" } },
  };
  assert.equal(validateAgainst(named, { const: 1, default: 2 }).valid, true);
  assert.equal(validateAgainst(named, { const: "x" }).valid, false);
  const data = { enum: [{ $ref: "#/not/a/schema" }], const: { $ref: "#/nor/this" } };
  assert.doesNotThrow(() => compileSchema(data), "enum and const VALUES are data, never refs");
});

test("the validator CLI exits 0 on valid files, 1 on an invalid one, 2 on a usage or read error", () => {
  const dir = mkdtempSync(join(tmpdir(), "termhost-validate-"));
  try {
    const bad = desktop();
    delete bad.rows[0].kpis.cpu_pct.state;
    writeFileSync(join(dir, "bad.json"), JSON.stringify(bad));
    const out = [];
    const io = { out: (s) => out.push(s), errOut: (s) => out.push(s) };
    assert.equal(
      runCli(
        [join(FIXTURES, "results-windows-desktop.json"), join(FIXTURES, "results-linux-xvfb.json")],
        io,
      ),
      0,
    );
    assert.equal(runCli([join(dir, "bad.json")], io), 1);
    assert.ok(out.some((l) => /INVALID: .*bad\.json/.test(l)));
    assert.equal(runCli([join(dir, "missing.json")], io), 2);
    assert.equal(runCli([], io), 2);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

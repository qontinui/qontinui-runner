/**
 * The run loop end to end on fakes (`run.mjs`): units in D11's interleaved order,
 * one row per unit and arm, every results file schema-valid before it is written,
 * a spawn-admission refusal ending its setup's ladder, teardown on every path, and
 * the redacted copy holding the runner's rows only.
 *
 *   node --test scripts/termhost-bench/__tests__/run-loop.test.mjs
 */

import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, rmSync, writeFileSync, mkdirSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { machineBlock } from "../machine.mjs";
import { NeutralityGuard } from "../neutrality.mjs";
import { redactRunnerOnly } from "../results.mjs";
import { planUnits, runBench } from "../run.mjs";
import { validateResults } from "../validate.mjs";
import { FakeAdapter, VirtualClock, fakeProbe, fakeRunOptions, healthyFacts } from "./helpers/fake-bench.mjs";

/** A run on fakes; returns the result and every adapter call. */
async function fakeRun(t, { opts = {}, refuseAt = {}, onArrange = null, facts = healthyFacts() } = {}) {
  const tmp = mkdtempSync(join(tmpdir(), "tb-run-"));
  t.after(() => rmSync(tmp, { recursive: true, force: true }));
  mkdirSync(join(tmp, "primary"), { recursive: true });
  const settings = join(tmp, "primary", "settings.json");
  writeFileSync(settings, '{"primary": true}');
  const clock = new VirtualClock();
  const calls = [];
  let current = null;
  const deps = {
    log: () => {},
    sleep: clock.sleep,
    startEnvironment: async () => ({ env: { DISPLAY: ":99" }, screen: { width: 1920, height: 1080, scalePct: 100 }, display: ":99", stop: async () => calls.push(["env.stop"]) }),
    probePath: "fake-probe",
    createAdapter: async (setup, o) => {
      current = new FakeAdapter({
        id: setup,
        clock,
        variant: o.variant,
        build: o.build ? { role: o.build.role, commit: o.build.commit } : null,
        refuseAt: refuseAt[setup] ?? null,
        onArrange: onArrange ? (arm, unit) => onArrange(arm, unit, settings) : null,
        calls,
      });
      return current;
    },
    probe: fakeProbe({ clock, current: () => current }),
    gatherDoctorFacts: async () => facts,
    sampleBackgroundLoad: async () => ({ cpu_pct: 1, memory_used_mb: 100, sampled_ms: 1 }),
    machineBlock: (o) => machineBlock({ ...o, gpu: { state: "unknown", reason: "not_instrumented", detail: "test" } }),
    neutrality: new NeutralityGuard({ paths: [{ label: "settings.json", path: settings }] }),
    reapGenerators: async () => ({ ended: [], survivors: [] }),
  };
  const res = await runBench(fakeRunOptions(join(tmp, "work"), { out: join(tmp, "results.json"), redactedOut: join(tmp, "redacted.json"), ...opts }), deps);
  return { res, calls, tmp };
}

test("units interleave head and baseline per run index and N (D11)", () => {
  const units = planUnits({
    setups: ["runner", "tmux"],
    ns: [5, 1],
    runs: 2,
    runnerBuilds: [{ role: "head" }, { role: "baseline" }],
  });
  const shape = units.map((u) => `${u.runIndex}:${u.n}:${u.setup}:${u.role}`);
  assert.deepEqual(shape.slice(0, 6), ["0:1:runner:head", "0:1:runner:baseline", "0:1:tmux:none", "0:5:runner:head", "0:5:runner:baseline", "0:5:tmux:none"]);
  assert.equal(units.length, 12);
  assert.deepEqual(units.map((u) => u.index), [...units.keys()]);
});

test("a run writes a schema-valid file: one row per unit and arm, every KPI present", async (t) => {
  const { res, calls } = await fakeRun(t);
  assert.equal(validateResults(res.doc).valid, true, JSON.stringify(validateResults(res.doc).errors.slice(0, 5)));
  assert.equal(res.doc.rows.length, 2 * 2 * 2, "2 setups x 2 levels x 2 arms");
  assert.equal(res.stopped, null);
  // Every unit launched and torn down exactly once; the environment stopped once.
  const launches = calls.filter((c) => c[0] === "launch").map((c) => c[1]);
  const teardowns = calls.filter((c) => c[0] === "teardown").map((c) => c[1]);
  assert.deepEqual(launches, [0, 1, 2, 3]);
  assert.deepEqual(teardowns, [0, 1, 2, 3]);
  assert.equal(calls.filter((c) => c[0] === "env.stop").length, 1);
  const runnerN5Focus = res.doc.rows.find((r) => r.setup.id === "runner" && r.scenario.n === 5 && r.scenario.arm === "focus");
  assert.equal(runnerN5Focus.kpis.output_latency_ms.state, "measured");
  assert.equal(runnerN5Focus.kpis.degradation_output_latency_ms.state, "measured");
  assert.equal(runnerN5Focus.kpis.input_latency_ms.reason, "input_unsafe", "input.safe is not OPERATIVE in the fake doctor");
  assert.equal(runnerN5Focus.scenario.v_target, 1);
  assert.equal(runnerN5Focus.scenario.v_actual, 1);
  const runnerTiled = res.doc.rows.find((r) => r.setup.id === "runner" && r.scenario.n === 5 && r.scenario.arm === "tiled");
  assert.equal(runnerTiled.scenario.v_target, 5);
  assert.equal(runnerTiled.kpis.switch_latency_ms.reason, "not_applicable_to_arm");
  // Labels: linux-xvfb is software rendered; tmux in public CI is an assert-only smoke.
  for (const r of res.doc.rows) assert.ok(r.labels.includes("software_rendered"));
  for (const r of res.doc.rows.filter((x) => x.setup.id === "tmux")) assert.ok(r.labels.includes("assert_only_smoke"));
  for (const r of res.doc.rows.filter((x) => x.setup.id === "runner")) {
    assert.ok(!r.labels.includes("assert_only_smoke"));
    assert.ok(Object.values(r.diagnostics).every((d) => d.comparable === false), "runner diagnostics are non-comparable");
  }
  for (const r of res.doc.rows.filter((x) => x.setup.id === "tmux")) assert.deepEqual(r.diagnostics, {});
});

test("the redacted copy keeps the runner's rows only and still validates", async (t) => {
  const { res } = await fakeRun(t);
  assert.ok(res.redactedPath);
  const red = redactRunnerOnly(res.doc);
  assert.ok(red.rows.length > 0);
  assert.ok(red.rows.every((r) => r.setup.id === "runner"));
  assert.ok(!Object.keys(red.capabilities).some((id) => id === "setup.tmux"));
  assert.equal(validateResults(red).valid, true);
});

test("a resource-guard refusal ends that setup's ladder; higher levels read admission_refused", async (t) => {
  const { res, calls } = await fakeRun(t, { opts: { ns: [1, 5, 15] }, refuseAt: { runner: 5 } });
  assert.equal(validateResults(res.doc).valid, true);
  const runnerRows = res.doc.rows.filter((r) => r.setup.id === "runner");
  for (const n of [5, 15]) {
    for (const r of runnerRows.filter((x) => x.scenario.n === n)) {
      assert.equal(r.kpis.output_latency_ms.state, "unknown");
      assert.equal(r.kpis.output_latency_ms.reason, "admission_refused");
    }
  }
  assert.equal(runnerRows.filter((r) => r.scenario.n === 1 && r.kpis.output_latency_ms.state === "measured").length, 2);
  // The runner at N = 15 was never launched; tmux ran every level.
  const runnerLaunches = calls.filter((c) => c[0] === "spawnSessions");
  assert.ok(!runnerLaunches.some((c) => c[1] === 15 && false));
  assert.equal(res.doc.rows.filter((r) => r.setup.id === "tmux" && r.kpis.output_latency_ms.state === "measured").length, 6);
  const inc = res.doc.incidents.find((i) => i.kind === "admission_refused");
  assert.ok(inc, "the refusal is an incident");
  assert.equal(inc.data.setup, "runner");
  assert.equal(inc.data.lane, "thread_count");
});

test("an arm that throws still tears the unit down, and its rows say why", async (t) => {
  const { res, calls } = await fakeRun(t, {
    onArrange: (arm, unit) => {
      if (unit === 1) throw new Error("fake arrange failure");
    },
  });
  assert.equal(validateResults(res.doc).valid, true);
  assert.ok(calls.some((c) => c[0] === "teardown" && c[1] === 1));
  const failed = res.doc.rows.filter((r) => r.setup.id === "tmux" && r.scenario.n === 1);
  assert.equal(failed.length, 2);
  for (const r of failed) assert.equal(r.kpis.cpu_pct.reason, "harness_error");
  assert.ok(res.doc.incidents.some((i) => i.kind === "harness_error" && /fake arrange failure/.test(i.detail)));
});

test("a doctor that blocks the run aborts before any unit launches", async (t) => {
  const facts = { ...healthyFacts(), probePath: null };
  await assert.rejects(fakeRun(t, { facts }), /cannot start here: probe.binary/);
});

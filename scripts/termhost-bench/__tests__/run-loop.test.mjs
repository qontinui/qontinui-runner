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
import { EventEmitter } from "node:events";
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync, mkdirSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { machineBlock } from "../machine.mjs";
import { NeutralityGuard } from "../neutrality.mjs";
import { InvalidResults, convertProbeIncident, redactRunnerOnly } from "../results.mjs";
import { RunAbort, RunFailed, planUnits, runBench } from "../run.mjs";
import { validateResults } from "../validate.mjs";
import { FakeAdapter, VirtualClock, fakeProbe, fakeRunOptions, healthyFacts } from "./helpers/fake-bench.mjs";

/** A run on fakes; returns the result and every adapter call. */
async function fakeRun(t, { opts = {}, refuseAt = {}, onArrange = null, facts = healthyFacts(), deps: overrides = {}, describe = null } = {}) {
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
        onArrange: onArrange ? (arm, unit) => onArrange(arm, unit, settings, { tmp, calls }) : null,
        calls,
      });
      if (describe) {
        const base = current.describe.bind(current);
        current.describe = (slots) => describe(base(slots), setup);
      }
      return current;
    },
    probe: fakeProbe({ clock, current: () => current }),
    gatherDoctorFacts: async () => facts,
    sampleBackgroundLoad: async () => ({ cpu_pct: 1, memory_used_mb: 100, sampled_ms: 1 }),
    machineBlock: (o) => machineBlock({ ...o, gpu: { state: "unknown", reason: "not_instrumented", detail: "test" } }),
    neutrality: new NeutralityGuard({ paths: [{ label: "settings.json", path: settings }] }),
    reapGenerators: async () => ({ ended: [], survivors: [] }),
    primaryPresent: async () => false,
  };
  const res = await runBench(fakeRunOptions(join(tmp, "work"), { out: join(tmp, "results.json"), redactedOut: join(tmp, "redacted.json"), ...opts }), { ...deps, ...overrides });
  return { res, calls, tmp, probe: deps.probe };
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
  const { res, calls, probe } = await fakeRun(t);
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
  // One watch at a time per gen dir, each under its own label, so every phase's
  // run file is set aside under a distinct name (records.rs: one run per file).
  assert.equal(probe.labels.size, 4, "one gen dir per unit");
  for (const labels of probe.labels.values()) {
    assert.deepEqual(labels, ["focus-soak", "focus-switch", "focus-spawn", "tiled-soak", "tiled-spawn"].filter((l) => labels.includes(l)));
    assert.equal(new Set(labels).size, labels.length);
    assert.ok(labels.includes("focus-soak") && labels.includes("tiled-soak"));
  }
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
  // The runner at N = 15 (unit 4) was never launched; tmux ran every level.
  assert.deepEqual(calls.filter((c) => c[0] === "launch").map((c) => c[1]), [0, 1, 2, 3, 5]);
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

test("a probe incident keeps its data (data.source) and is placed where the harness met it", () => {
  const rec = { schema: "termhost-probe/v1", type: "incident", kind: "capture_lost", at: "2026-10-12T01:00:00Z", detail: "x11 lost", stops_run: false, data: { source: "x11" } };
  assert.deepEqual(convertProbeIncident(rec, { setup: "tmux", unit: "u001", phase: "focus/soak" }), {
    kind: "capture_lost",
    at: "2026-10-12T01:00:00Z",
    detail: "x11 lost",
    stops_run: false,
    data: { source: "x11", setup: "tmux", unit: "u001", phase: "focus/soak" },
  });
  // A kind the schema does not know is the harness's error, the probe's kind kept.
  const odd = convertProbeIncident({ kind: "cosmic", detail: "?", data: { source: "wgc" } });
  assert.equal(odd.kind, "harness_error");
  assert.equal(odd.data.probe_kind, "cosmic");
  assert.equal(odd.data.source, "wgc");
});

// ---------------------------------------------------------------------------
// Review round 1
// ---------------------------------------------------------------------------

const AB = [
  { role: "head", commit: "a".repeat(40), exe: "fake", source: "artifact" },
  { role: "baseline", commit: "b".repeat(40), exe: "fake", source: "artifact" },
];

test("a refusal ends one ladder — its subject's, in its run, from the refused N up — and the A/B file is still written (review M1)", async (t) => {
  // The review's reproduction (review-p3/ladder-dup.mjs), extended to three levels.
  const { res, calls } = await fakeRun(t, { opts: { ns: [1, 5, 15], runs: 2, runnerBuilds: AB }, refuseAt: { runner: 5 } });
  assert.equal(validateResults(res.doc).valid, true, JSON.stringify(validateResults(res.doc).errors.slice(0, 3)));
  // r0: 0-2 at N=1, 3-5 at N=5 (3, 4 refused), 6-8 at N=15; r1: 9-17 likewise.
  assert.deepEqual(calls.filter((c) => c[0] === "launch").map((c) => c[1]), [0, 1, 2, 3, 4, 5, 8, 9, 10, 11, 12, 13, 14, 17]);
  const skipped = res.doc.rows.filter((r) => r.setup.id === "runner" && r.scenario.n === 15);
  assert.equal(skipped.length, 2 * 2 * 2, "head and baseline, both arms, both runs");
  for (const r of skipped) {
    assert.equal(r.kpis.output_latency_ms.reason, "admission_refused");
    assert.ok(["head", "baseline"].includes(r.setup.build.role), "a skipped row keeps its build's role");
    assert.equal(r.setup.build.commit, r.setup.build.role === "head" ? "a".repeat(40) : "b".repeat(40));
  }
  // Run 1's lower levels were measured again rather than skipped.
  assert.equal(res.doc.rows.filter((r) => r.setup.id === "runner" && r.scenario.run_index === 1 && r.scenario.n === 1 && r.kpis.output_latency_ms.state === "measured").length, 4);
});

test("a skipped row records the launch mode the run used", async (t) => {
  const { res } = await fakeRun(t, { opts: { ns: [1, 5], runnerMode: "supervisor", runnerBuilds: [{ role: "single", commit: "a".repeat(40), gitRef: "a".repeat(40), source: "git_ref" }] }, refuseAt: { runner: 1 }, deps: { primaryPresent: async () => false } });
  const skipped = res.doc.rows.filter((r) => r.setup.id === "runner" && r.scenario.n === 5);
  assert.ok(skipped.length > 0);
  for (const r of skipped) {
    assert.equal(r.setup.launch_mode, "supervisor_spawn_test");
    assert.equal(r.setup.build.source, "git_ref");
  }
});

test("an invalid results document is set aside privately, and the error names paths and keywords only", async (t) => {
  // `ram_mb` below its schema minimum: the validator's own message for that
  // keyword quotes the value ("-31337 < minimum 1"), so this is the case that
  // can leak it.
  const err = await fakeRun(t, {
    deps: {
      machineBlock: (o) => ({ ...machineBlock({ ...o, gpu: { state: "unknown", reason: "not_instrumented", detail: "test" } }), ram_mb: -31337 }),
    },
  }).then(() => null, (e) => e);
  assert.ok(err instanceof InvalidResults, String(err));
  assert.ok(existsSync(err.asidePath), "the data survives, unpublished");
  assert.match(err.asidePath, /results\.invalid\.json$/);
  assert.match(readFileSync(err.asidePath, "utf8"), /-31337/, "the aside copy is the whole document");
  assert.match(err.message, /\/machine\/ram_mb \[minimum\]/);
  assert.doesNotMatch(err.message, /31337/, "a value never reaches the log (D7)");
});

test("one arm's failure is that arm's alone: the measured arm keeps its row (review S1)", async (t) => {
  // The review's reproduction (review-p3/second-arm.mjs).
  const { res } = await fakeRun(t, {
    opts: { ns: [1], setups: ["runner"] },
    onArrange: (arm) => {
      if (arm === "tiled") throw new Error("tiled arrange timed out");
    },
  });
  const focus = res.doc.rows.find((r) => r.scenario.arm === "focus");
  const tiled = res.doc.rows.find((r) => r.scenario.arm === "tiled");
  assert.equal(focus.kpis.output_latency_ms.state, "measured");
  assert.equal(tiled.kpis.output_latency_ms.state, "unknown");
  assert.equal(tiled.kpis.output_latency_ms.reason, "harness_error");
  assert.match(tiled.kpis.output_latency_ms.detail, /tiled arrange timed out/);
  assert.ok(res.doc.incidents.some((i) => /arm tiled: tiled arrange timed out/.test(i.detail)));
});

test("the primary-state guard's baseline is in the results, and a guard watching nothing blocks a supervisor run (review S7)", async (t) => {
  const watched = await fakeRun(t, {});
  assert.equal(watched.res.doc.capabilities["harness.neutrality"].state, "OPERATIVE");
  assert.match(watched.res.doc.capabilities["harness.neutrality"].reason, /watching 1 of 1 primary-state file\(s\): settings\.json sha256:/);
  const nothing = (extra) => new NeutralityGuard({ paths: [{ label: "settings.json", path: join(tmpdir(), "no-such-dir-tb", "settings.json") }], ...extra });
  const blocked = await fakeRun(t, { opts: { runnerMode: "supervisor" }, deps: { neutrality: nothing(), primaryPresent: async () => true } }).then(() => null, (e) => e);
  assert.ok(blocked instanceof RunAbort, String(blocked));
  assert.match(blocked.message, /would watch nothing/);
  const unknownPrimary = await fakeRun(t, { opts: { runnerMode: "supervisor" }, deps: { neutrality: nothing(), primaryPresent: async () => null } }).then(() => null, (e) => e);
  assert.ok(unknownPrimary instanceof RunAbort, "an unanswered :9876 probe counts as 'may be present'");
  const noPrimary = await fakeRun(t, { deps: { neutrality: nothing(), primaryPresent: async () => false } });
  assert.equal(noPrimary.res.doc.capabilities["harness.neutrality"].state, "OPERATIVE");
  assert.match(noPrimary.res.doc.capabilities["harness.neutrality"].reason, /nothing to protect/);
});

test("a primary-state file the guard cannot read is UNKNOWN at baseline and an incident at each check, not a silent pass (review S7)", async (t) => {
  const unreadable = () =>
    new NeutralityGuard({
      paths: [{ label: "settings.json", path: join(tmpdir(), "tb-unreadable", "settings.json") }],
      read: () => {
        throw Object.assign(new Error("denied"), { code: "EACCES" });
      },
    });
  // Subject mode records it and carries on: the incident says what was not seen.
  const { res } = await fakeRun(t, { deps: { neutrality: unreadable(), primaryPresent: async () => false } });
  assert.equal(res.doc.capabilities["harness.neutrality"].state, "UNKNOWN", "an unreadable file is state the guard cannot see");
  assert.equal(res.doc.incidents.filter((i) => i.kind === "harness_error" && /could not read settings\.json/.test(i.detail)).length, 1, "reported once");
  // A supervisor run on such a box is refused before any unit.
  const blocked = await fakeRun(t, { opts: { runnerMode: "supervisor" }, deps: { neutrality: unreadable(), primaryPresent: async () => false } }).then(() => null, (e) => e);
  assert.ok(blocked instanceof RunAbort, String(blocked));
});

test("SIGINT ends the unit in progress and writes the completed units as a partial file, at once and at the end (review S9)", async (t) => {
  const signals = new EventEmitter();
  let atSignal = null;
  const { res, calls, tmp } = await fakeRun(t, {
    opts: { ns: [1, 5] },
    deps: { signals },
    onArrange: (arm, unit, settings, ctx) => {
      if (unit === 1 && arm === "focus") {
        signals.emit("SIGINT", "SIGINT");
        const out = join(ctx.tmp, "results.json");
        atSignal = existsSync(out) ? JSON.parse(readFileSync(out, "utf8")) : null;
      }
    },
  });
  // Written synchronously at the signal: unit 0 only, marked partial.
  assert.ok(atSignal, "the completed units were written the moment the signal arrived");
  assert.deepEqual([...new Set(atSignal.rows.map((r) => r.setup.id))], ["runner"]);
  assert.ok(atSignal.incidents.some((i) => i.stops_run && /interrupted by SIGINT after 1 of 4 unit\(s\)/.test(i.detail)));
  // Then the unit in progress tore down, nothing later launched, and the final file says partial.
  assert.deepEqual(calls.filter((c) => c[0] === "launch").map((c) => c[1]), [0, 1]);
  assert.deepEqual(calls.filter((c) => c[0] === "teardown").map((c) => c[1]), [0, 1]);
  assert.ok(res.stopped && /interrupted by SIGINT/.test(res.stopped.detail));
  assert.equal(validateResults(res.doc).valid, true);
  // The interrupted unit's arms were cut short: unknown, never a short window passed off as the soak.
  const cut = res.doc.rows.filter((r) => r.setup.id === "tmux");
  assert.equal(cut.length, 2);
  for (const r of cut) {
    assert.equal(r.kpis.output_latency_ms.state, "unknown");
    assert.match(r.kpis.output_latency_ms.detail, /interrupted/);
  }
  assert.equal(signals.listenerCount("SIGINT"), 0, "the handlers are removed when the run ends");
  void tmp;
});

test("a run that stops before any row exists fails (exit 1), it is not a doctor refusal (review N4)", async (t) => {
  const changedAtOnce = {
    baseline: () => [{ label: "settings.json", state: "present", sha256: `sha256:${"0".repeat(64)}` }],
    check: () => ({ changed: true, incident: { kind: "primary_state_changed", at: new Date().toISOString(), detail: "changed", stops_run: true } }),
  };
  const err = await fakeRun(t, { deps: { neutrality: changedAtOnce } }).then(() => null, (e) => e);
  assert.ok(err instanceof RunFailed, String(err));
  assert.ok(!(err instanceof RunAbort));
});

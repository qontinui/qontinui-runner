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
import { existsSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync, mkdirSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { machineBlock } from "../machine.mjs";
import { NeutralityGuard } from "../neutrality.mjs";
import { InvalidResults, convertProbeIncident, redactRunnerOnly } from "../results.mjs";
import { AdapterRefusal } from "../adapters/index.mjs";
import { RunnerAdapter } from "../adapters/runner.mjs";
import { RunAbort, RunFailed, abortAwareSleep, measureOrder, planUnits, runBench } from "../run.mjs";
import { stallSegment } from "./helpers/bench-fixtures.mjs";
import { validateResults } from "../validate.mjs";
import { FakeAdapter, VirtualClock, fakeProbe, fakeRunOptions, healthyFacts } from "./helpers/fake-bench.mjs";

/** A run on fakes; returns the result and every adapter call. */
async function fakeRun(t, { opts = {}, refuseAt = {}, onArrange = null, facts = healthyFacts(), deps: overrides = {}, describe = null, adapterHook = null, wrapProbe = null } = {}) {
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
      adapterHook?.(current, setup);
      return current;
    },
    probe: (wrapProbe ?? ((x) => x))(fakeProbe({ clock, current: () => current })),
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
  // The spawn pass, run after every arm measured, still lands in its arm's row.
  for (const arm of ["focus", "tiled"]) {
    const sp = res.doc.rows.find((r) => r.setup.id === "runner" && r.scenario.n === 5 && r.scenario.arm === arm).kpis.spawn_latency_ms;
    assert.equal(sp.n, 2, `${arm}: both spawn samples reached the row (${JSON.stringify(sp)})`);
  }
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
    // Every arm measures before ANY arm spawns: a spawn changes the runner's layout,
    // and the tiled soak run after the focus arm's spawns saw no session at all
    // (CI run 38345475088). And tiled measures before focus: after the focus arm's
    // maximize + restore the restored pane stopped repainting (CI run 38351621760).
    assert.deepEqual(labels, ["tiled-soak", "focus-soak", "focus-switch", "focus-spawn", "tiled-spawn"].filter((l) => labels.includes(l)));
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
    // Nothing ran that could prove the build: the requested commit, never a proven one (D12).
    assert.equal(r.setup.build.state, "unknown");
    assert.equal(r.setup.build.commit, undefined);
    assert.equal(r.setup.build.requested_commit, r.setup.build.role === "head" ? "a".repeat(40) : "b".repeat(40));
    assert.ok(r.labels.includes("unknown_build"));
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
    assert.equal(r.setup.build.requested_source, "git_ref");
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
      if (unit === 1 && arm === "tiled") { // the first arm measured
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

test("both A/B builds failing to boot still write a valid file: each row keeps its role, unproven and labelled (review round 2 M1)", async (t) => {
  // The review's reproduction (review-p3/describe-role.mjs), inside a run: the REAL
  // runner adapter's describe(), with launch failing the way waitUiBridgeReady does.
  const { res } = await fakeRun(t, {
    opts: { ns: [1], setups: ["runner"], runnerBuilds: AB },
    deps: {
      createAdapter: async (setup, o) => {
        const a = new RunnerAdapter({ ...o, mode: "subject", platform: "linux", repoRoot: ".", list: () => new Map(), log: () => {} });
        a.launch = async () => {
          throw new AdapterRefusal("harness_error", "runner did not report uiBridgeIpcObserved:true within 120s");
        };
        a.teardown = async () => ({ closed: [], survivors: [], incidents: [], notes: [] });
        a.coordReadback = undefined;
        return a;
      },
    },
  });
  assert.equal(validateResults(res.doc).valid, true, JSON.stringify(validateResults(res.doc).errors.slice(0, 3)));
  const rows = res.doc.rows.filter((r) => r.setup.id === "runner");
  assert.equal(rows.length, 4, "head and baseline, both arms");
  for (const r of rows) {
    assert.equal(r.setup.build.state, "unknown");
    assert.equal(r.setup.build.commit, undefined, "never a proven commit");
    assert.equal(r.setup.build.requested_commit, r.setup.build.role === "head" ? "a".repeat(40) : "b".repeat(40));
    assert.ok(r.labels.includes("unknown_build"));
    assert.equal(r.kpis.output_latency_ms.reason, "harness_error");
  }
  assert.deepEqual([...new Set(rows.map((r) => r.setup.build.role))].sort(), ["baseline", "head"]);
});

test("a signal during the switch phase stops the switch loop at once, and the arm reads interrupted (review round 2 S1)", async (t) => {
  // The review's reproduction (review-p3/sigint-switch.mjs).
  const signals = new EventEmitter();
  let shows = 0;
  const shown = [];
  const { res } = await fakeRun(t, {
    opts: { ns: [5], setups: ["runner"], switchSamples: 20 },
    deps: { signals },
    adapterHook: (a) => {
      const show = a.show.bind(a);
      a.show = async (k) => {
        shown.push(k);
        if (++shows === 3) signals.emit("SIGINT", "SIGINT");
        return show(k);
      };
    },
  });
  // Three samples, then only the restore of the arm's focus session (the fake's
  // lowest slot): no switch after the signal, whose settle time is gone.
  assert.equal(shows, 4, `shown: ${JSON.stringify(shown)}`);
  assert.equal(shown[3], Math.min(...shown), "the fourth show() is the focus restore");
  const focus = res.doc.rows.find((r) => r.scenario.arm === "focus");
  assert.equal(focus.kpis.switch_latency_ms.state, "unknown");
  assert.equal(focus.kpis.output_latency_ms.state, "unknown");
  assert.match(focus.kpis.output_latency_ms.detail, /interrupted/);
});

test("a signal during the spawn phase stops the spawn loop, and the arm reads interrupted (review round 2 S1)", async (t) => {
  const signals = new EventEmitter();
  let spawns = 0;
  const { res } = await fakeRun(t, {
    opts: { ns: [1], setups: ["runner"], spawnSamples: 10 },
    deps: { signals },
    adapterHook: (a) => {
      const one = a.spawnOne.bind(a);
      a.spawnOne = async () => {
        if (++spawns === 2) signals.emit("SIGINT", "SIGINT");
        return one();
      };
    },
  });
  assert.equal(spawns, 2, "no spawnOne() after the signal");
  const focus = res.doc.rows.find((r) => r.scenario.arm === "focus");
  assert.equal(focus.kpis.spawn_latency_ms.state, "unknown");
  assert.match(focus.kpis.output_latency_ms.detail, /interrupted/);
});

test("after the abort the run's sleep still yields a turn of the event loop, on both clocks (review round 2 S1)", async () => {
  // The virtual clock's timers are unref'd, so a sleep the abort ended leaves
  // nothing holding the test process open for its nominal 60 s.
  for (const inner of [null, (ms) => new Promise((r) => setTimeout(r, ms).unref())]) {
    const ac = new AbortController();
    const sleep = abortAwareSleep(ac.signal, inner);
    const t0 = Date.now();
    const pending = sleep(60_000);
    ac.abort();
    await pending;
    assert.ok(Date.now() - t0 < 5000, "the abort ends a sleep in progress");
    let turned = false;
    setImmediate(() => (turned = true));
    await sleep(60_000);
    assert.equal(turned, true, `${inner ? "virtual" : "real"} clock: a sleep after the abort lets the event loop turn`);
  }
});

test("a second signal still stops the temp runners already known, then exits 130 (review round 2 nit 5)", async (t) => {
  const signals = new EventEmitter();
  const stopped = [];
  const exits = [];
  const launchSignals = [];
  await fakeRun(t, {
    opts: { ns: [1], setups: ["runner"] },
    deps: { signals, exit: (code) => exits.push(code), hardExitGraceMs: 2000 },
    adapterHook: (a) => {
      const launch = a.launch.bind(a);
      a.launch = async (ctx) => {
        launchSignals.push(ctx.signal);
        return launch(ctx);
      };
      a.emergencyStop = async () => {
        stopped.push("temp runner test-9883");
        return "stopped temp runner test-9883";
      };
    },
    onArrange: (arm) => {
      if (arm === "focus") {
        signals.emit("SIGINT", "SIGINT");
        signals.emit("SIGTERM", "SIGTERM");
      }
    },
  });
  await new Promise((r) => setImmediate(r));
  assert.deepEqual(stopped, ["temp runner test-9883"]);
  assert.deepEqual(exits, [130]);
  // launch was handed the run's abort signal, so its own long waits end at it.
  assert.ok(launchSignals.length > 0 && launchSignals.every((x) => x instanceof AbortSignal && x.aborted));
});

test("a signal while waiting for a spawned session's first marker ends that wait at once (review round 2 S1)", async (t) => {
  const signals = new EventEmitter();
  let clock = null;
  let armed = false;
  let signalAt = null;
  let closedAt = null;
  await fakeRun(t, {
    opts: { ns: [1], setups: ["runner"], spawnSamples: 3, spawnTimeoutMs: 4000 },
    deps: {
      signals,
      // The poll's own 100 ms sleep is where the signal lands.
      sleep: async (ms) => {
        if (ms === 100 && armed) {
          armed = false;
          signalAt = Date.now();
          signals.emit("SIGINT", "SIGINT");
        }
        return clock.sleep(ms);
      },
    },
    adapterHook: (a) => {
      clock = a.clock;
      a.spawnOne = async () => {
        // A session that claims its slot but never paints a marker.
        const k = readdirSync(a.genDir).filter((f) => /^slot-\d+$/.test(f)).length;
        const hostId = `quiet${k}`;
        writeFileSync(join(a.genDir, `slot-${k}`), `${JSON.stringify({ schema: "termhost-bench/slot/v1", slot: k, run_id: "x", pid: 4_100_000 + k, ppid: 3_100_000 + k, host_terminal_id: { TERMHOST_BENCH_TERMINAL_ID: hostId }, claimed_at_ns: String(a.clock.now()), argv: a.argv })}\n`);
        writeFileSync(join(a.genDir, `gen-${k}.jsonl`), "");
        armed = true;
        return { ok: true, hostId, tCmdNs: a.clock.now() };
      };
      const close = a.closeOne.bind(a);
      a.closeOne = async (h) => {
        closedAt ??= Date.now();
        return close(h);
      };
    },
  });
  assert.ok(signalAt !== null && closedAt !== null);
  assert.ok(closedAt - signalAt < 1000, `the marker wait ended ${closedAt - signalAt} ms after the signal, not at its 4 s deadline`);
});

test("tiled measures before focus whatever order the arms are given in; rows keep the given order (ci run 38351621760)", () => {
  assert.deepEqual(measureOrder(["focus", "tiled"]), ["tiled", "focus"]);
  assert.deepEqual(measureOrder(["tiled", "focus"]), ["tiled", "focus"]);
  assert.deepEqual(measureOrder(["focus"]), ["focus"]);
});

test("a stalled pane in a soak becomes a pane_stalled incident on its row and names itself in the KPI (ci run 38351621760)", async (t) => {
  const { res } = await fakeRun(t, {
    opts: { ns: [1], setups: ["runner"] },
    // The tiled soak's records as the stalled pane in CI recorded them.
    wrapProbe: (inner) => ({
      ...inner,
      startWatch(o) {
        const w = inner.startWatch(o);
        if (o.label !== "tiled-soak") return w;
        return { ...w, stop: async () => {
          const seg = await w.stop();
          return { ...seg, records: stallSegment(seg.records) };
        } };
      },
    }),
  });
  const tiled = res.doc.rows.find((r) => r.scenario.arm === "tiled");
  assert.equal(tiled.kpis.output_latency_ms.reason, "insufficient_samples");
  assert.match(tiled.kpis.output_latency_ms.detail, /the pane stopped repainting: session 0 stayed at seq/);
  assert.ok(tiled.incidents.some((i) => i.kind === "pane_stalled" && /session 0's output marker stayed at seq \d+/.test(i.detail)), JSON.stringify(tiled.incidents));
  const focus = res.doc.rows.find((r) => r.scenario.arm === "focus");
  assert.ok(!focus.incidents.some((i) => i.kind === "pane_stalled"));
  assert.equal(validateResults(res.doc).valid, true);
});

test("the spawn pass measures new sessions even when its arrangement shows no earlier one (ci run 38351621760)", async (t) => {
  const seen = new Map();
  const { res } = await fakeRun(t, {
    opts: { ns: [5], setups: ["runner"] },
    adapterHook: (a) => {
      const arrange = a.arrange.bind(a);
      a.arrange = async (arm, actx) => {
        const r = await arrange(arm, actx);
        seen.set(arm, (seen.get(arm) ?? 0) + 1);
        // The spawn pass's tiled arrangement shows no earlier session.
        if (arm === "tiled" && seen.get(arm) === 2) return { ...r, ok: false, failure: { reason: "sessions_not_rendered", detail: "after restore-zone the probe saw no session marker" } };
        return r;
      };
    },
  });
  assert.equal(seen.get("tiled"), 2, "measured once, arranged again for its spawn pass");
  const tiled = res.doc.rows.find((r) => r.scenario.arm === "tiled");
  assert.equal(tiled.kpis.spawn_latency_ms.n, 2, JSON.stringify(tiled.kpis.spawn_latency_ms));
  assert.notEqual(tiled.kpis.spawn_latency_ms.reason, "sessions_not_rendered");
  assert.equal(tiled.kpis.output_latency_ms.state, "measured", "the measure pass is untouched");
});

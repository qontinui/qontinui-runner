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
import { createHash } from "node:crypto";
import { existsSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync, mkdirSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { machineBlock } from "../machine.mjs";
import { NeutralityGuard } from "../neutrality.mjs";
import { InvalidResults, convertProbeIncident, redactRunnerOnly } from "../results.mjs";
import { AdapterRefusal } from "../adapters/index.mjs";
import { RunnerAdapter } from "../adapters/runner.mjs";
import { RESTORE_CHECK_MIN_FRAMES, RunAbort, RunFailed, abortAwareSleep, measureOrder, outputMarkersEmitted, planUnits, restoreCoverageGap, runBench } from "../run.mjs";
import { stallSegment } from "./helpers/bench-fixtures.mjs";
import { paneStalls, stallCoverage } from "../kpis.mjs";
import { validateResults } from "../validate.mjs";
import { FakeAdapter, fakeProbe, fakeProbeBinary, fakeRunOptions, healthyFacts, VirtualClock } from "./helpers/fake-bench.mjs";

/** A run on fakes; returns the result and every adapter call. */
async function fakeRun(t, { opts = {}, refuseAt = {}, onArrange = null, facts = healthyFacts(), deps: overrides = {}, describe = null, adapterHook = null, wrapProbe = null, wrapSleep = null, driverPaths = null } = {}) {
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
    sleep: wrapSleep ? wrapSleep(clock.sleep) : clock.sleep,
    startEnvironment: async () => ({ env: { DISPLAY: ":99" }, screen: { width: 1920, height: 1080, scalePct: 100 }, display: ":99", stop: async () => calls.push(["env.stop"]) }),
    probePath: fakeProbeBinary(tmp),
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
  if (driverPaths) {
    // Build the fake through runBench's driver factory, recording the path it was given.
    const fake = deps.probe;
    deps.probe = undefined;
    deps.probeDriver = (args) => {
      driverPaths.push(args.probePath);
      return fake;
    };
  }
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
  // The focus arm's restore check is on (the runner adapter restores its zone),
  // so its watch's place in the order is pinned too.
  const { res, calls, probe } = await fakeRun(t, {
    opts: { restoreCheckMs: 4000 },
    adapterHook: (a, setup) => {
      if (setup === "runner") realZoneView(a);
    },
  });
  assert.equal(validateResults(res.doc).valid, true, JSON.stringify(validateResults(res.doc).errors.slice(0, 5)));
  assert.equal(res.doc.rows.length, 2 * 2 * 2, "2 setups x 2 levels x 2 arms");
  assert.equal(res.stopped, null);
  // Which probe binary decoded the frames (M1): the injected one, its codec version from the doctor, no invented build commit.
  assert.match(res.doc.run.probe.path, /fake-probe$/);
  assert.match(res.doc.run.probe.executed_path, /probe-bin[\\/]fake-probe$/, "the pinned copy in the run directory");
  assert.equal(res.doc.run.probe.source, "explicit");
  assert.equal(res.doc.run.probe.codec_version, 2);
  assert.equal(res.doc.run.probe.build.reason, "not_instrumented");
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
    // (CI run 38345475088). And tiled measures before focus, the layout an operator
    // gets before any harness maximize. The restore check follows the focus arm's
    // measure phases and precedes every spawn.
    assert.deepEqual(labels, ["tiled-soak", "focus-soak", "focus-switch", "focus-restore-check", "focus-spawn", "tiled-spawn"].filter((l) => labels.includes(l)));
    assert.equal(new Set(labels).size, labels.length);
    assert.ok(labels.includes("focus-soak") && labels.includes("tiled-soak"));
  }
  const withCheck = [...probe.labels.values()].filter((l) => l.includes("focus-restore-check"));
  assert.equal(withCheck.length, 2, "one restore check per runner unit");
  for (const labels of withCheck) assert.ok(labels.includes("focus-soak") && labels.includes("focus-spawn"), labels.join(","));
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

test("an arrangement the harness failed is a run-level incident, once per cause; one the host could not realise is not (ci run 38362046020)", async (t) => {
  const arranged = new Map();
  const lines = [];
  const { res } = await fakeRun(t, {
    opts: { ns: [1], setups: ["runner"] },
    deps: { log: (l) => lines.push(l) },
    adapterHook: (a) => {
      const arrange = a.arrange.bind(a);
      a.arrange = async (arm, actx) => {
        const r = await arrange(arm, actx);
        arranged.set(arm, (arranged.get(arm) ?? 0) + 1);
        if (arm === "tiled") return { ...r, ok: false, failure: { reason: "harness_error", detail: "the extension's arrange call timed out" } };
        return { ...r, ok: false, failure: { reason: "arm_not_realisable", detail: "no editor group" } };
      };
    },
  });
  assert.equal(arranged.get("tiled"), 2, "the measure and the spawn pass each arranged the arm");
  const tiled = res.doc.rows.find((r) => r.scenario.arm === "tiled");
  const focus = res.doc.rows.find((r) => r.scenario.arm === "focus");
  assert.equal(tiled.kpis.output_latency_ms.reason, "harness_error");
  const top = res.doc.incidents.filter((i) => /: arrange: /.test(i.detail));
  assert.equal(top.length, 1, `one root cause, one incident (review N5): ${JSON.stringify(top)}`);
  assert.equal(top[0].kind, "harness_error");
  assert.ok(top[0].detail.endsWith("arm tiled: arrange: the extension's arrange call timed out"), "the measure pass's, not the spawn pass's: " + top[0].detail);
  assert.equal(top[0].data.setup, "runner", "redaction filters run-level incidents on data.setup");
  assert.ok(lines.some((l) => l.endsWith("arm tiled: arrange: the extension's arrange call timed out")), "the cause reaches the log");
  assert.ok(!res.doc.rows.flatMap((r) => r.incidents ?? []).some((i) => /: arrange: /.test(i.detail)), "not duplicated on the row");
  assert.equal(focus.kpis.output_latency_ms.reason, "arm_not_realisable");
  assert.ok(!res.doc.incidents.some((i) => /arm focus.*arrange:/.test(i.detail)), "a host that cannot realise the arm is a reading, not a harness error");
  assert.equal(validateResults(res.doc).valid, true);
});

test("a third party's arrange incident stays off the redacted copy, and the public log carries it reading-free (D7, second review S2)", async (t) => {
  const lines = [];
  const { res } = await fakeRun(t, {
    opts: { ns: [1], setups: ["runner", "tmux"] },
    deps: { log: (l) => lines.push(l) },
    adapterHook: (a, setup) => {
      const arrange = a.arrange.bind(a);
      a.arrange = async (arm, actx) => {
        const r = await arrange(arm, actx);
        if (arm !== "tiled") return r;
        return { ...r, ok: false, failure: { reason: "harness_error", detail: `${setup} arrange call timed out after 12034 ms` } };
      };
    },
  });
  const tmux = res.doc.incidents.filter((i) => /: arrange: /.test(i.detail) && i.data?.setup === "tmux");
  assert.equal(tmux.length, 1);
  assert.match(tmux[0].detail, /after 12034 ms$/, "the incident record keeps the verbatim text (it never leaves the VM)");
  assert.equal(redactRunnerOnly(res.doc).incidents.filter((i) => /: arrange: /.test(i.detail) && i.data?.setup === "tmux").length, 0);
  const logged = lines.filter((l) => /: arrange: /.test(l));
  assert.ok(logged.some((l) => l.endsWith("arrange: tmux arrange call timed out after # ms")), JSON.stringify(logged));
  assert.ok(logged.some((l) => l.endsWith("arrange: runner arrange call timed out after 12034 ms")), "the runner's own detail stays verbatim");
  assert.ok(!logged.some((l) => /tmux.*12034/.test(l)), "no third-party reading in the public log");
});

test("one arrange incident per cause: a third party's keyed reading-free, the runner's verbatim, never truncated (second review N2, third review N3)", async (t) => {
  const run = (setups, spawnDetail, measureDetail = "arrange call timed out after 12034 ms") => {
    let pass = 0;
    return fakeRun(t, {
      opts: { ns: [1], setups },
      adapterHook: (a, setup) => {
        const arrange = a.arrange.bind(a);
        a.arrange = async (arm, actx) => {
          const r = await arrange(arm, actx);
          if (arm !== "tiled" || setup !== setups.at(-1)) return r;
          pass += 1;
          return { ...r, ok: false, failure: { reason: "harness_error", detail: pass === 1 ? measureDetail : spawnDetail } };
        };
      },
    });
  };
  const arrangeIncidents = (res, setup) => res.doc.incidents.filter((i) => /: arrange: /.test(i.detail) && i.data?.setup === setup);
  // A third party: the same cause with other numbers is one incident...
  const tmuxSame = await run(["runner", "tmux"], "arrange call timed out after 12999 ms");
  assert.equal(arrangeIncidents(tmuxSame.res, "tmux").length, 1);
  // ...a different cause is a second.
  const tmuxOther = await run(["runner", "tmux"], "the extension refused: no editor group");
  assert.equal(arrangeIncidents(tmuxOther.res, "tmux").length, 2);
  // The runner's details are keyed verbatim: other numbers are another incident.
  const runnerNumbers = await run(["runner"], "arrange call timed out after 12999 ms");
  assert.equal(arrangeIncidents(runnerNumbers.res, "runner").length, 2);
  const runnerSame = await run(["runner"], "arrange call timed out after 12034 ms");
  assert.equal(arrangeIncidents(runnerSame.res, "runner").length, 1);
  // The key is never truncated: two causes that differ only past the gate's cut are two.
  const long = "x".repeat(300);
  const tmuxLong = await run(["runner", "tmux"], `${long} B`, `${long} A`);
  assert.equal(arrangeIncidents(tmuxLong.res, "tmux").length, 2);
  assert.ok(arrangeIncidents(tmuxOther.res, "tmux").some((i) => /\(spawn pass\): arrange: the extension refused/.test(i.detail)));
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
  assert.equal(tiled.kpis.output_latency_ms.reason, "pane_stalled");
  assert.match(tiled.kpis.output_latency_ms.detail, /^pane stalled: session 0 stayed at seq \d+ for \d+ ms .*, and did not advance in any later frame \(a wedge on what was seen: the still marker alone meets the bar\)$/);
  assert.ok(
    tiled.incidents.some(
      (i) => i.kind === "pane_stalled" && /arm tiled: soak: session 0 stayed at seq \d+ for \d+ ms .*, and did not advance in any later frame \(a wedge on what was seen: the still marker alone meets the bar\): the pane did not repaint after that; its marker KPIs read output_latency_ms unknown\(pane_stalled\), marker_loss unknown\(pane_stalled\), marker_skipped unknown\(pane_stalled\)$/.test(i.detail),
    ),
    JSON.stringify(tiled.incidents),
  );
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


/**
 * Give a fake runner adapter the zone view TerminalPage.tsx's handlers have:
 * restore-zone ALWAYS answers `maximizedZone: null` (buildMaximizeResult), with
 * `changed: false` when nothing was maximized; get-zone-view-state reads the
 * state back. The focus arm leaves zone 0 maximized unless `maximized` says not.
 */
function realZoneView(a, { maximized = 0, viewAfter, events = null, throwOnRestore = null, visibleAll = false } = {}) {
  let current = maximized;
  a.restoreZone = async () => {
    events?.push("restore");
    if (throwOnRestore) throw new Error(throwOnRestore);
    const previous = current;
    current = null;
    // The tiled layout is back: the probe sees every session again.
    const all = [...(a.slots?.keys() ?? [])].sort((x, y) => x - y);
    a.visibleSet = new Set(visibleAll ? all : all.slice(0, 9));
    return { maximizedZone: null, previousMaximizedZone: previous, zoneCount: 4, changed: previous !== null };
  };
  a.zoneViewState = async () => {
    events?.push("view");
    return { maximizedZone: viewAfter === undefined ? current : viewAfter, zoneCount: 4, layoutId: "quad" };
  };
}

/** The restore-check watch's records, rewritten by `edit`. */
const editRestoreWatch = (edit) => (inner) => ({
  ...inner,
  startWatch(o) {
    const w = inner.startWatch(o);
    if (o.label !== "focus-restore-check") return w;
    return { ...w, stop: async () => {
      const seg = await w.stop();
      return { ...seg, records: edit(seg.records) };
    } };
  },
});

/** One runner unit at N = 1 with a restore check; returns the focus row and the logs. */
async function restoreRun(t, { zone = {}, wrapProbe = null, facts, deps = {}, adapterHook = null, wrapSleep = null, n = 1, restoreCheckMs = 4000 } = {}) {
  const logs = [];
  const { res } = await fakeRun(t, {
    opts: { ns: [n], setups: ["runner"], restoreCheckMs },
    adapterHook: (a, setup) => {
      realZoneView(a, zone);
      adapterHook?.(a, setup);
    },
    wrapProbe,
    facts,
    wrapSleep,
    deps: { log: (m) => logs.push(String(m)), ...deps },
  });
  assert.equal(validateResults(res.doc).valid, true, JSON.stringify(validateResults(res.doc).errors?.slice(0, 3)));
  return { res, focus: res.doc.rows.find((r) => r.scenario.arm === "focus"), logs };
}
const unknownIncident = (row, re) =>
  row.incidents.some((i) => i.kind === "harness_error" && /restore check UNKNOWN: /.test(i.detail) && re.test(i.detail) && /this is not a clean result$/.test(i.detail));
const check = (row) => row.diagnostics["focus.restore_check"];

test("after the focus arm, a restore-zone check records a frozen restored pane as an incident, gating nothing (review r9 nit 2)", async (t) => {
  const frozen = await restoreRun(t, { wrapProbe: editRestoreWatch((r) => stallSegment(r)) });
  const inc = frozen.focus.incidents.filter((i) => i.kind === "pane_stalled");
  assert.equal(inc.length, 1, JSON.stringify(frozen.focus.incidents));
  assert.match(inc[0].detail, /arm focus: after restore-zone: session 0 stayed at seq \d+ for \d+ ms .*, and did not advance in any later frame \(a wedge on what was seen: the still marker alone meets the bar\): the restored pane did not repaint$/);
  assert.equal(frozen.focus.kpis.output_latency_ms.state, "measured", "it gates nothing: the focus soak is unaffected");
  assert.equal(check(frozen.focus).state, "measured", JSON.stringify(check(frozen.focus)));
  assert.equal(check(frozen.focus).data.outcome, "not repainting");
  // A clean check says so, positively.
  const clean = await restoreRun(t);
  assert.ok(!clean.res.doc.rows.some((r) => r.incidents.some((i) => i.kind === "pane_stalled" || /restore check/.test(i.detail))));
  assert.equal(check(clean.focus).state, "measured");
  assert.match(check(clean.focus).notes[0], /^restore check: observed, clean \(\d+ primary frame\(s\); 0: \d+ kept frame\(s\) at \+\d+\.\.\+\d+ ms, \d+ seq\(s\) \(judged\)\)$/);
  assert.ok(clean.logs.some((l) => /restore check: observed, clean/.test(l)));
});

test("a hitch the restored pane recovers from is reported as recovered (review r11 SF4)", async (t) => {
  const { focus } = await restoreRun(t, { wrapProbe: editRestoreWatch((r) => freezeWindow(r, 500, 3000)) });
  const inc = focus.incidents.filter((i) => i.kind === "pane_stalled");
  assert.equal(inc.length, 1, JSON.stringify(focus.incidents));
  assert.match(inc[0].detail, /after restore-zone: .*, then advanced again: a hitch the restored pane recovered from$/);
  assert.equal(inc[0].data.recovered, true);
  assert.equal(check(focus).data.outcome, "recovered from a hitch");
});

test("the restore check settles BEFORE its watch, after reading the restore back (review r11 SF4)", async (t) => {
  const events = [];
  await restoreRun(t, {
    zone: { events },
    wrapSleep: (inner) => async (ms) => {
      events.push(`sleep:${ms}`);
      return inner(ms);
    },
    wrapProbe: (inner) => ({
      ...inner,
      startWatch(o) {
        events.push(`watch:${o.label}`);
        return inner.startWatch(o);
      },
    }),
  });
  const from = events.indexOf("restore");
  assert.ok(from >= 0, events.join(","));
  assert.deepEqual(events.slice(from, from + 5), ["restore", "sleep:500", "view", "watch:focus-restore-check", "sleep:4000"]);
});

test("every restore-check path that could not look is UNKNOWN, never clean (review r10 SF6/SF7, r11 SF1/SF2/SF4)", async (t) => {
  const cases = [
    ["nothing was maximized (changed: false)", { zone: { maximized: null } }, /changed: false \(nothing was maximized\)/],
    ["no changed field", { adapterHook: (a) => { a.restoreZone = async () => ({ maximizedZone: null }); } }, /carries no changed field/],
    ["the view still reads maximized", { zone: { viewAfter: 2 } }, /zone view state reads maximized zone 2/],
    ["the view cannot be read back", { adapterHook: (a) => { delete a.zoneViewState; } }, /cannot read the zone view state back/],
    ["no summary", { wrapProbe: editRestoreWatch((rs) => rs.filter((r) => r.type !== "summary")) }, /its watch wrote no summary/],
    ["no window", { wrapProbe: editRestoreWatch((rs) => rs.map((r) => (r.type === "summary" ? { ...r, t_end_ns: r.t_start_ns } : r))) }, /no measurement window/],
    ["no primary source", { wrapProbe: editRestoreWatch((rs) => rs.map((r) => (r.type === "capture" ? { ...r, state: "failed" } : r))) }, /no primary capture source opened/],
    [
      "frames only in the watch's last second (too short a span to judge)",
      { wrapProbe: editRestoreWatch((rs) => { const end = BigInt(rs.find((r) => r.type === "summary").t_end_ns); return rs.filter((r) => r.type !== "frame" || BigInt(r.t_ns) > end - 1_000_000_000n); }) },
      /0 of the 1 session\(s\) the restored layout should show could be judged/,
    ],
    [
      "every frame inside an operator_activity interval (none kept)",
      {
        wrapProbe: editRestoreWatch((rs) => {
          const sum = rs.find((r) => r.type === "summary");
          return [...rs, { type: "interval", index: 99, t_start_ns: rs.find((r) => r.type === "start").t0_ns, t_end_ns: String(BigInt(sum.t_end_ns) + 1n), valid: false, exclusion_class: "operator_activity" }];
        }),
      },
      /0 of the 1 session\(s\) .*0: no kept frame/,
    ],
    ["the restore threw", { zone: { throwOnRestore: "UI Bridge action failed: boom" } }, /UNKNOWN: UI Bridge action failed: boom/],
    [
      "the focus arm failed",
      {
        adapterHook: (a) => {
          const base = a.arrange.bind(a);
          a.arrange = async (arm, actx) => (arm === "focus" ? { ok: false, failure: { reason: "zone_unresolved", detail: "fake" } } : base(arm, actx));
        },
      },
      /the focus arm failed \(zone_unresolved\)/,
    ],
  ];
  for (const [what, o, re] of cases) {
    const { focus } = await restoreRun(t, o);
    assert.ok(unknownIncident(focus, re), `${what}: ${JSON.stringify(focus.incidents)}`);
    assert.equal(check(focus).state, "unknown", what);
    assert.match(check(focus).detail, re, what);
    assert.ok(!focus.incidents.some((i) => i.kind === "pane_stalled"), what);
  }
  // An unaligned clock: emission times cannot be set against frame times.
  const facts = healthyFacts();
  facts.probeDoctor.record.mechanisms = facts.probeDoctor.record.mechanisms.map((m) => (m.id === "clock.aligned" ? { ...m, state: "UNKNOWN", reason: "node not found" } : m));
  const unaligned = await restoreRun(t, { facts, wrapProbe: editRestoreWatch((r) => stallSegment(r)) });
  assert.ok(unknownIncident(unaligned.focus, /clock\.aligned is UNKNOWN/), JSON.stringify(unaligned.focus.incidents));
  assert.ok(!unaligned.focus.incidents.some((i) => i.kind === "pane_stalled"));
});

test("a signal during the restore check's watch makes it UNKNOWN, not clean (review r11 SF2)", async (t) => {
  const signals = new EventEmitter();
  const logs = [];
  await fakeRun(t, {
    opts: { ns: [1], setups: ["runner"], restoreCheckMs: 4000 },
    adapterHook: (a) => realZoneView(a),
    deps: { signals, log: (m) => logs.push(String(m)) },
    wrapProbe: (inner) => ({
      ...inner,
      startWatch(o) {
        const w = inner.startWatch(o);
        if (o.label === "focus-restore-check") signals.emit("SIGINT", "SIGINT");
        return w;
      },
    }),
  });
  assert.ok(logs.some((l) => /restore check UNKNOWN: the run was interrupted during its watch; this is not a clean result/.test(l)), logs.filter((l) => /restore/.test(l)).join("\n"));
  assert.ok(!logs.some((l) => /restore check: observed/.test(l)));
});

test("a recovered hitch in a soak is a pane_stalled incident that says so, and voids nothing (review r10 MF1)", async (t) => {
  const { res } = await fakeRun(t, {
    opts: { ns: [1], setups: ["runner"] },
    wrapProbe: (inner) => ({
      ...inner,
      startWatch(o) {
        const w = inner.startWatch(o);
        if (o.label !== "tiled-soak") return w;
        return { ...w, stop: async () => {
          const seg = await w.stop();
          return { ...seg, records: freezeWindow(seg.records, 10_000, 12_500) };
        } };
      },
    }),
  });
  const tiled = res.doc.rows.find((r) => r.scenario.arm === "tiled");
  assert.equal(tiled.kpis.output_latency_ms.state, "measured", JSON.stringify(tiled.kpis.output_latency_ms));
  const inc = tiled.incidents.filter((i) => i.kind === "pane_stalled");
  assert.equal(inc.length, 1, JSON.stringify(tiled.incidents));
  assert.match(inc[0].detail, /soak: session 0 stayed at seq \d+ for \d+ ms .*, then advanced again: a hitch the pane recovered from \(not voided\); its marker KPIs read output_latency_ms measured, marker_loss measured, marker_skipped measured$/);
  assert.equal(inc[0].data.recovered, true);
  assert.equal(validateResults(res.doc).valid, true);
});

test("a recovered hitch on a capture-disagreement row: the incident reads the KPIs as the row carries them, never 'stay measured' (head confirmation of 802e8f3d4)", async (t) => {
  const { res } = await fakeRun(t, {
    opts: { ns: [1], setups: ["runner"] },
    wrapProbe: (inner) => ({
      ...inner,
      startWatch(o) {
        const w = inner.startWatch(o);
        if (o.label !== "tiled-soak") return w;
        return { ...w, stop: async () => {
          const seg = await w.stop();
          const records = freezeWindow(seg.records, 10_000, 12_500).map((r) =>
            r.type === "summary"
              ? { ...r, crosscheck: { ...(r.crosscheck ?? {}), verdict: "disagree", detail: "3 of 40 markers disagreed" }, output_latency_ms: { state: "unknown", unit: "ms", n: 40, reason: "capture_disagreement", detail: "3 of 40 markers disagreed" } }
              : r,
          );
          return { ...seg, records };
        } };
      },
    }),
  });
  const tiled = res.doc.rows.find((r) => r.scenario.arm === "tiled");
  assert.equal(tiled.kpis.output_latency_ms.reason, "capture_disagreement", "a recovered hitch voids nothing: the capture-health reason stays");
  const inc = tiled.incidents.filter((i) => i.kind === "pane_stalled");
  assert.equal(inc.length, 1, JSON.stringify(tiled.incidents));
  assert.match(inc[0].detail, /a hitch the pane recovered from \(not voided\); its marker KPIs read output_latency_ms unknown\(capture_disagreement\), /);
  assert.doesNotMatch(inc[0].detail, /stay measured/);
  // The row's own note on the hitch says the same, with the KPI's actual state.
  const notes = tiled.kpis.output_latency_ms.notes ?? [];
  assert.ok(notes.some((n) => /^pane hitch, recovered \(not voided by the stall; the marker KPIs read output_latency_ms unknown\(capture_disagreement\), /.test(n)), JSON.stringify(notes));
  for (const n of notes) assert.doesNotMatch(n, /stay measured/);
  assert.equal(validateResults(res.doc).valid, true);
});

/** Freeze every output marker shown between `fromMs` and `toMs` into the watch at the sequence shown then. */
function freezeWindow(records, fromMs, toMs) {
  const start = BigInt(records.find((r) => r.type === "start").t0_ns) + 5_000_000n;
  const a = start + BigInt(fromMs) * 1_000_000n;
  const b = start + BigInt(toMs) * 1_000_000n;
  const frozen = new Map();
  return records.map((r) => {
    if (r.type !== "frame" || BigInt(r.t_ns) < a || BigInt(r.t_ns) > b) return r;
    return { ...r, markers: r.markers.map((m) => {
      if (m.kind !== "output") return m;
      if (!frozen.has(m.session)) frozen.set(m.session, m.seq);
      return { ...m, seq: frozen.get(m.session) };
    }) };
  });
}

test("a restored pane that came back blank is judged, not skipped: N = 2 (review r12 MF2)", async (t) => {
  const clean = await restoreRun(t, { n: 2 });
  assert.equal(check(clean.focus).state, "measured", JSON.stringify(check(clean.focus)));
  assert.deepEqual(check(clean.focus).data.judged, [0, 1]);
  assert.match(check(clean.focus).notes[0], /^restore check: observed, clean \(\d+ primary frame\(s\); 0: \d+ kept frame\(s\) at .* \(judged\), 1: \d+ kept frame\(s\) at .* \(judged\)\)$/);
  // Session 1 absent from every frame and from sessions_seen.
  const blank = await restoreRun(t, {
    n: 2,
    wrapProbe: editRestoreWatch((rs) =>
      rs
        .filter((r) => !(r.type === "first_seen" && r.session === 1))
        .map((r) =>
          r.type === "frame"
            ? { ...r, markers: (r.markers ?? []).filter((m) => m.session !== 1) }
            : r.type === "summary"
              ? { ...r, sessions_seen: (r.sessions_seen ?? []).filter((k) => k !== 1) }
              : r,
        ),
    ),
  });
  assert.ok(unknownIncident(blank.focus, /1 of the 2 session\(s\) the restored layout should show could be judged .*0: .* \(judged\), 1: no kept frame \(not judged: no kept frame\)/), JSON.stringify(blank.focus.incidents));
  assert.equal(check(blank.focus).state, "unknown");
});

test("a restored pane that froze and then the screen sent no frame is not clean (review r12 MF1, r13 MF1)", async (t) => {
  const { focus } = await restoreRun(t, {
    wrapProbe: editRestoreWatch((rs) => {
      const s = stallSegment(rs);
      const t0 = BigInt(s.find((r) => r.type === "start").t0_ns);
      return s.filter((r) => r.type !== "frame" || BigInt(r.t_ns) <= t0 + 600_000_000n);
    }),
  });
  // Seen 0.6 s, then 3.4 s with no frame: tail-dependent, and a 3.4 s tail is under
  // the 10 s tail bar, so it is no wedge (review r13 MF1) — but it is not clean either.
  assert.equal(check(focus).state, "unknown", JSON.stringify(check(focus)));
  assert.ok(unknownIncident(focus, /0: \d+ kept frame\(s\) .*\(not judged: kept frames span under 2000 ms\)/), JSON.stringify(focus.incidents));
  const inc = focus.incidents.filter((i) => i.kind === "pane_stalled");
  assert.equal(inc.length, 1);
  assert.match(inc[0].detail, /then no frame for \d+ ms\) .*did not advance in any later frame \(tail-dependent, not a wedge: .*\): the restored pane sent no later frame, on too short a tail to call a wedge$/);
  assert.equal(inc[0].data.voids, false);
});

test("a signal before the restore check makes it UNKNOWN (review r12 SF7)", async (t) => {
  const signals = new EventEmitter();
  const logs = [];
  const restores = [];
  await fakeRun(t, {
    opts: { ns: [1], setups: ["runner"], restoreCheckMs: 4000 },
    adapterHook: (a) => {
      realZoneView(a, { events: restores });
      // Diagnostics run after each arm's measure phases; tiled measures first,
      // so the second call is the focus arm's, just before its restore check.
      let calls = 0;
      a.diagnostics = async () => {
        if (calls++ === 1) signals.emit("SIGINT", "SIGINT");
        return {};
      };
    },
    deps: { signals, log: (m) => logs.push(String(m)) },
  });
  assert.ok(logs.some((l) => /restore check UNKNOWN: the run was interrupted before it; this is not a clean result/.test(l)), logs.slice(-15).join("\n"));
  assert.deepEqual(restores, [], "nothing was restored");
});

test("a clean restore verdict needs ten kept frames per session, reaching near the watch's end (review r12 MF1)", async (t) => {
  // Seen 0-2.5 s of a 6 s watch, then frames without the marker: wide enough, but not near the end.
  const early = await restoreRun(t, {
    restoreCheckMs: 6000,
    wrapProbe: editRestoreWatch((rs) => {
      const t0 = BigInt(rs.find((r) => r.type === "start").t0_ns);
      return rs.map((r) => (r.type === "frame" && BigInt(r.t_ns) > t0 + 2_500_000_000n ? { ...r, markers: [] } : r));
    }),
  });
  assert.ok(unknownIncident(early.focus, /0 of the 1 session\(s\) the restored layout should show could be judged .*0: \d+ kept frame\(s\) at \+\d+\.\.\+2\d{3} ms, \d+ seq\(s\) \(not judged: last kept frame over 2000 ms before the watch's end\)/), JSON.stringify(early.focus.incidents));
  assert.equal(check(early.focus).state, "unknown");
  // One frame per half second over the whole watch: span and end are fine, eight frames are too few.
  const sparse = await restoreRun(t, {
    wrapProbe: editRestoreWatch((rs) => {
      let k = 0;
      return rs.filter((r) => r.type !== "frame" || k++ % 30 === 0);
    }),
  });
  assert.ok(unknownIncident(sparse.focus, /0 of the 1 session\(s\) .*0: [1-9] kept frame\(s\) .*\(not judged: fewer than 10 kept frames\)/), JSON.stringify(sparse.focus.incidents));
});

// --- review r13 ---------------------------------------------------------------

/** Drop session `k` from every frame, first_seen and sessions_seen of the restore watch. */
const blankRestored = (k) =>
  editRestoreWatch((rs) =>
    rs
      .filter((r) => !(r.type === "first_seen" && r.session === k))
      .map((r) =>
        r.type === "frame"
          ? { ...r, markers: (r.markers ?? []).filter((m) => m.session !== k) }
          : r.type === "summary"
            ? { ...r, sessions_seen: (r.sessions_seen ?? []).filter((x) => x !== k) }
            : r,
      ),
  );

test("SF1: at N = 12 the restored (focus) session must be among the judged, else UNKNOWN (review r13 SF1)", async (t) => {
  const clean = await restoreRun(t, { n: 12, zone: { visibleAll: true } });
  assert.equal(check(clean.focus).state, "measured", JSON.stringify(check(clean.focus)));
  assert.equal(check(clean.focus).data.restored, 0);
  assert.ok(check(clean.focus).data.judged.includes(0));
  // Session 0 — the one restore-zone restored — came back blank; the other 11 suffice for the count of 9.
  const blank = await restoreRun(t, { n: 12, zone: { visibleAll: true }, wrapProbe: blankRestored(0) });
  assert.equal(check(blank.focus).state, "unknown", JSON.stringify(check(blank.focus)));
  assert.ok(unknownIncident(blank.focus, /the restored session 0 could not be judged \(no kept frame\); 0: no kept frame \(not judged: no kept frame\), 1: .* \(judged\)/), JSON.stringify(blank.focus.incidents));
  // A different blank session leaves the restored one judged: clean, and the note says who was not judged.
  const other = await restoreRun(t, { n: 12, zone: { visibleAll: true }, wrapProbe: blankRestored(11) });
  assert.equal(check(other.focus).data?.outcome, "clean", JSON.stringify(check(other.focus)));
  assert.match(check(other.focus).notes[0], /0: \d+ kept frame\(s\) .* \(judged\), .*11: no kept frame \(not judged: no kept frame\)\)$/);
});

test("nit: the restore check's judging boundaries — frames 9/10, span >= 2000, end <= 2000, final still run < 2000 (review r13 nit, r14 MF2, r15 nit 7)", () => {
  const ok = { frames: RESTORE_CHECK_MIN_FRAMES, seqs: 2, firstMs: 0, lastMs: 3000, finalSeq: 9, finalFromMs: 1001, finalHeldMs: 1999 };
  assert.equal(RESTORE_CHECK_MIN_FRAMES, 10);
  assert.equal(restoreCoverageGap(ok, 5000), null, "exactly 10 frames, end exactly 2000 ms before the watch's end, final run 1999 ms");
  assert.equal(restoreCoverageGap({ ...ok, frames: 9 }, 5000), "fewer than 10 kept frames");
  assert.equal(restoreCoverageGap({ ...ok, lastMs: 2000 }, 4000), null, "span exactly 2000 ms");
  assert.equal(restoreCoverageGap({ ...ok, lastMs: 1999 }, 3999), "kept frames span under 2000 ms");
  assert.equal(restoreCoverageGap(ok, 5001), "last kept frame over 2000 ms before the watch's end");
  assert.equal(
    restoreCoverageGap({ ...ok, finalFromMs: 1000, finalHeldMs: 2000 }, 5000, 12),
    "marker did not advance in the final 2000 ms of its kept frames (seq 9 from +1000 ms; its generator wrote 12 marker(s) meanwhile)",
    "a final still run of exactly 2000 ms",
  );
  // The held time is the run's time outside excluded spans, not its wall span.
  assert.equal(restoreCoverageGap({ ...ok, finalFromMs: 0, finalHeldMs: 1999 }, 5000), null, "a 3000 ms span, 1999 ms of it seen");
  assert.match(restoreCoverageGap({ ...ok, seqs: 1, finalFromMs: 0, finalHeldMs: 3000 }, 5000), /final 3000 ms .*no generator log, so how many markers it wrote is unknown\)$/);
  assert.equal(restoreCoverageGap({ frames: 0, seqs: 0, firstMs: null, lastMs: null, finalSeq: null, finalFromMs: null, finalHeldMs: null }, 5000), "no kept frame");
});

test("nit: markers emitted in (a, b] — not at a, at b (review r15 nit 5)", () => {
  const gen = { markers: [10n, 20n, 30n].map((t) => ({ kind: "output", tEmit: t })).concat([{ kind: "echo", tEmit: 25n }]) };
  assert.equal(outputMarkersEmitted(gen, 10n, 30n), 2);
  assert.equal(outputMarkersEmitted(gen, 9n, 30n), 3);
  assert.equal(outputMarkersEmitted(undefined, 0n, 30n), null);
});

test("MF1: a 2.4 s whole-screen freeze at the soak's end is a pane_stalled incident, not a void (review r13 MF1)", async (t) => {
  const { res } = await fakeRun(t, {
    opts: { ns: [1], setups: ["tmux"] },
    wrapProbe: (inner) => ({
      ...inner,
      startWatch(o) {
        const w = inner.startWatch(o);
        if (o.label !== "tiled-soak") return w;
        return { ...w, stop: async () => {
          const seg = await w.stop();
          const frames = seg.records.filter((r) => r.type === "frame").map((r) => BigInt(r.t_ns));
          const cut = frames.reduce((m, x) => (x > m ? x : m), 0n) - 2_400_000_000n;
          return { ...seg, records: seg.records.filter((r) => r.type !== "frame" || BigInt(r.t_ns) <= cut) };
        } };
      },
    }),
  });
  const tiled = res.doc.rows.find((r) => r.scenario.arm === "tiled");
  assert.equal(tiled.kpis.output_latency_ms.state, "measured", JSON.stringify(tiled.kpis.output_latency_ms));
  const inc = tiled.incidents.filter((i) => i.kind === "pane_stalled");
  assert.equal(inc.length, 1, JSON.stringify(tiled.incidents));
  assert.match(inc[0].detail, /\(tail-dependent, not a wedge: .*\): still at the soak's end on too short a frameless tail to call a wedge \(not voided\); its marker KPIs read output_latency_ms measured, marker_loss measured, marker_skipped measured$/);
  assert.deepEqual([inc[0].data.recovered, inc[0].data.voids], [false, false]);
});

/** Session `k`'s marker frozen over [fromMs, toMs] of the restore watch, and no frame at all after `toMs`. */
const freezeThenNoFrame = (k, fromMs, toMs) =>
  editRestoreWatch((rs) => {
    const start = BigInt(rs.find((r) => r.type === "start").t0_ns) + 5_000_000n;
    const a = start + BigInt(fromMs) * 1_000_000n;
    const b = start + BigInt(toMs) * 1_000_000n;
    let seq = null;
    return rs
      .filter((r) => r.type !== "frame" || BigInt(r.t_ns) <= b)
      .map((r) => {
        if (r.type !== "frame" || BigInt(r.t_ns) < a) return r;
        return { ...r, markers: r.markers.map((m) => {
          if (m.kind !== "output" || m.session !== k) return m;
          seq ??= m.seq;
          return { ...m, seq };
        }) };
      });
  });

test("a restored session still at the watch's end on a short frameless tail is not judged (review r13 MF1)", async (t) => {
  const { focus } = await restoreRun(t, { wrapProbe: freezeThenNoFrame(0, 1500, 2900) });
  const inc = focus.incidents.filter((i) => i.kind === "pane_stalled");
  assert.equal(inc.length, 1, JSON.stringify(focus.incidents));
  assert.equal(inc[0].data.voids, false);
  assert.equal(check(focus).state, "unknown", JSON.stringify(check(focus)));
  assert.ok(unknownIncident(focus, /0: \d+ kept frame\(s\) .*\(not judged: still at the watch's end on a short frameless tail\)/), JSON.stringify(focus.incidents));
});

test("at N = 12 a tail-dependent stall on an unjudged, non-restored session leaves the outcome clean, not a hitch (review r13 MF1)", async (t) => {
  const { focus } = await restoreRun(t, { n: 12, zone: { visibleAll: true }, wrapProbe: freezeThenNoFrame(11, 1500, 2900) });
  assert.equal(focus.incidents.filter((i) => i.kind === "pane_stalled").length, 1, JSON.stringify(focus.incidents));
  assert.equal(check(focus).data?.outcome, "clean", JSON.stringify(check(focus)));
  assert.deepEqual(check(focus).data.unresolved, [11]);
  assert.match(check(focus).notes[0], /^restore check: observed, clean \(1 session\(s\) unresolved: 11\) \(/);
  assert.match(check(focus).notes[0], /11: \d+ kept frame\(s\) .*\(not judged: still at the watch's end on a short frameless tail\)\)$/);
});

// --- review r14 (scenarios D-F of review5/r1.mjs, r2.mjs) ------------------------

const T0R = 1_000_000_000_000n;
const atR = (ms) => T0R + BigInt(ms) * 1_000_000n;
const frR = (ms, seq) => ({ source: "x11", t_ns: String(atR(ms)), markers: [{ kind: "output", session: 0, seq }] });
const everyR = (a, b, st) => Array.from({ length: Math.floor((b - a) / st) + 1 }, (_, i) => a + i * st);
const segR = (frames) => ({ captures: [{ type: "capture", source: "x11", state: "started" }], summary: { primary: "x11" }, frames, intervals: [], firstSeen: [], incidents: [] });
const genR = (ms) => new Map([[0, { markers: everyR(0, ms, 100).map((t, i) => ({ kind: "output", seq: i, tEmit: atR(t) })) }]]);
const win5 = { t0: atR(0), t1: atR(5000) };
const judgeR = (frames, gens) => {
  const c = stallCoverage(segR(frames), win5, [0]).get(0);
  const g = gens.get(0);
  const emitted = g ? g.markers.filter((m) => m.tEmit > atR(c.finalFromMs) && m.tEmit <= atR(c.lastMs)).length : null;
  return restoreCoverageGap(c, 5000, emitted);
};

test("r14 F: a restored pane that advanced once and then froze, its generator blocked, is not judged (review r14 MF2)", () => {
  const frames = [frR(0, 641), ...everyR(100, 5000, 50).map((t) => frR(t, 642))];
  const blocked = new Map([[0, { markers: Array.from({ length: 15 }, (_, i) => ({ kind: "output", seq: 642 + i, tEmit: atR(i * 100) })) }]]);
  assert.deepEqual(paneStalls(segR(frames), blocked, win5, new Set([0])), [], "15 markers: no stall");
  assert.equal(stallCoverage(segR(frames), win5, [0]).get(0).seqs, 2, "two sequences seen");
  assert.equal(judgeR(frames, blocked), "marker did not advance in the final 4900 ms of its kept frames (seq 642 from +100 ms; its generator wrote 13 marker(s) meanwhile)");
  // Advancing up to 3.001 s, still after: a final run of 1999 ms is judged; from 3.0 s, 2000 ms is not.
  const until = (ms) => [...everyR(0, ms, 50).map((t) => frR(t, Math.floor(t / 50))), ...everyR(ms + 50, 5000, 50).map((t) => frR(t, 9999))];
  assert.equal(judgeR([...everyR(0, 2950, 50).map((t) => frR(t, t / 50)), frR(3001, 9999), ...everyR(3050, 5000, 50).map((t) => frR(t, 9999))], genR(5000)), null);
  assert.match(judgeR(until(2950), genR(5000)), /^marker did not advance in the final 2000 ms /);
});

test("r14 D/E: in a 5 s restore watch a whole-screen freeze never voids, and a seen wedge's report counts real markers (review r14 nit 6, SF3)", () => {
  // D: frames advance to 1 s, then nothing: tail-dependent, under the 10 s tail floor.
  const d = paneStalls(segR(everyR(0, 1000, 100).map((t) => frR(t, t / 100))), genR(5000), win5, new Set([0]));
  assert.deepEqual(d.map((x) => [x.basis, x.voids]), [["tail", false]]);
  // E: one seq 0.5-4.5 s while the generator writes 10 Hz: a seen wedge, and the gap text gives the count.
  const e = everyR(500, 4500, 100).map((t) => frR(t, 7));
  assert.deepEqual(paneStalls(segR(e), genR(5000), win5, new Set([0])).map((x) => [x.emitted, x.basis, x.voids]), [[45, "seen", true]]);
  assert.equal(judgeR(e, genR(5000)), "marker did not advance in the final 4000 ms of its kept frames (seq 7 from +500 ms; its generator wrote 40 marker(s) meanwhile)");
  assert.match(judgeR(e, new Map()), /no generator log, so how many markers it wrote is unknown\)$/);
});

test("r14 SF3: a restored session not judged for a long final still run reports how many markers its generator really wrote (review r14 SF3)", async (t) => {
  // Session 0 frozen from 2.0 s to the end of a 5 s watch, its marker hidden 2.5-3.6 s (over the
  // 900 ms gap): seen ~1.9 s in two pieces, so no stall; its final still run spans ~3 s.
  const { focus } = await restoreRun(t, {
    restoreCheckMs: 5000,
    wrapProbe: editRestoreWatch((rs) => {
      const start = BigInt(rs.find((r) => r.type === "start").t0_ns) + 5_000_000n;
      const ms = (x) => start + BigInt(x) * 1_000_000n;
      let seq = null;
      return rs.map((r) => {
        if (r.type !== "frame" || BigInt(r.t_ns) < ms(2000)) return r;
        const hidden = BigInt(r.t_ns) >= ms(2500) && BigInt(r.t_ns) < ms(3600);
        const markers = r.markers
          .filter((m) => !(hidden && m.kind === "output" && m.session === 0))
          .map((m) => (m.kind === "output" && m.session === 0 ? { ...m, seq: (seq ??= m.seq) } : m));
        return { ...r, markers };
      });
    }),
  });
  assert.equal(focus.incidents.filter((i) => i.kind === "pane_stalled").length, 0, JSON.stringify(focus.incidents));
  assert.equal(check(focus).state, "unknown", JSON.stringify(check(focus)));
  const inc = focus.incidents.find((i) => /restore check UNKNOWN/.test(i.detail));
  const m = /not judged: marker did not advance in the final \d+ ms of its kept frames \(seq \d+ from \+\d+ ms; its generator wrote (\d+) marker\(s\) meanwhile\)/.exec(inc?.detail ?? "");
  assert.ok(m, inc?.detail);
  assert.ok(Number(m[1]) >= 20 && Number(m[1]) <= 35, m[1]);
});

test("r15 MF1: a primary capture loss after a restored session's last kept frame leaves it unjudged, and the check names the loss (review r15 MF1, r3.mjs)", async (t) => {
  const withLoss = (kind) =>
    editRestoreWatch((rs) => {
      const start = BigInt(rs.find((r) => r.type === "start").t0_ns) + 5_000_000n;
      const ms = (x) => start + BigInt(x) * 1_000_000n;
      const source = rs.find((r) => r.type === "capture" && r.state === "started").source;
      const kept = rs.filter((r) => r.type !== "frame" || BigInt(r.t_ns) <= ms(3500));
      if (!kind) return kept;
      const detail = kind === "ended" ? `${source} capture ended: gone` : `${source} capture lost (re-establishing): read failed`;
      return [...kept, { type: "incident", kind: "capture_lost", at: "2026-10-16T00:00:00.000Z", t_ns: String(ms(3550)), detail, stops_run: kind === "ended", data: { source } }];
    });
  // Without a loss: 1.5 s with no frame after +3.5 s of a 5 s watch is within the end term, and judged clean.
  const none = await restoreRun(t, { restoreCheckMs: 5000, wrapProbe: withLoss(null) });
  assert.equal(check(none.focus).data?.outcome, "clean", JSON.stringify(check(none.focus)));
  assert.deepEqual(check(none.focus).data.losses, { count: 0, groups: [] });
  for (const kind of ["lost", "ended"]) {
    const { focus } = await restoreRun(t, { restoreCheckMs: 5000, wrapProbe: withLoss(kind) });
    assert.equal(check(focus).state, "unknown", JSON.stringify(check(focus)));
    assert.ok(
      unknownIncident(focus, /0: \d+ kept frame\(s\) .*\(not judged: the primary capture was lost after its last kept frame \(.* capture (lost \(re-establishing\)|ended)[^)]*\)\); 1 primary capture loss\(es\) \(\w+; first \+\d+ ms, last \+\d+ ms\): 1 x /),
      `${kind}: ${JSON.stringify(focus.incidents)}`,
    );
  }
});

test("r15 MF1: the restore check lists the window's losses in a clean note too, and a mapped loss counts within the slack (review r15 MF1)", async (t) => {
  const edit = (incs) =>
    editRestoreWatch((rs) => {
      const startRec = rs.find((r) => r.type === "start");
      const start = BigInt(startRec.t0_ns) + 5_000_000n;
      const ms = (x) => start + BigInt(x) * 1_000_000n;
      const source = rs.find((r) => r.type === "capture" && r.state === "started").source;
      const lastKept = incs.cut === undefined ? rs : rs.filter((r) => r.type !== "frame" || BigInt(r.t_ns) <= ms(incs.cut));
      const at0 = "2026-10-16T00:00:00.000Z";
      const out = lastKept.map((r) => (r.type === "start" ? { ...r, at: at0 } : r));
      for (const x of incs.list) {
        const rec = { type: "incident", kind: "capture_lost", at: at0, detail: `${source} capture lost (re-establishing): read failed`, stops_run: false, data: { source } };
        if (x.exactMs !== undefined) rec.t_ns = String(x.exactMs === "before" ? BigInt(startRec.t0_ns) - 1n : ms(x.exactMs));
        else if (x.unplaced) rec.at = "nope";
        else rec.at = new Date(Date.parse(at0) + Number((ms(x.wallMs) - BigInt(startRec.t0_ns)) / 1_000_000n)).toISOString();
        out.push(rec);
      }
      return out;
    });
  // A failed read at +1 s, frames after it, and one before the window: clean, and the note names only the first.
  const early = await restoreRun(t, { restoreCheckMs: 5000, wrapProbe: edit({ list: [{ exactMs: 1000 }, { exactMs: "before" }] }) });
  assert.equal(check(early.focus).data?.outcome, "clean", JSON.stringify(check(early.focus)));
  assert.deepEqual(check(early.focus).data.losses, { count: 1, groups: [{ effect: "later", count: 1, firstMs: 1000, lastMs: 1000 }] });
  assert.match(check(early.focus).notes[0], /; 1 primary capture loss\(es\) \(\w+; first \+1000 ms, last \+1000 ms\): 1 x re-established: later frames were read at \+1000 ms; frameless tail read \+\d+ ms\.\.\+5000 ms \(e\.g\. ".*"\)\)$/);
  // Frames to +3.5 s, a loss placed by wall clock 0.5 s BEFORE the last kept frame: within the slack, so not judged.
  const mapped = await restoreRun(t, { restoreCheckMs: 5000, wrapProbe: edit({ cut: 3500, list: [{ wallMs: 3000 }] }) });
  assert.equal(check(mapped.focus).state, "unknown", JSON.stringify(check(mapped.focus)));
  assert.ok(unknownIncident(mapped.focus, /not judged: the primary capture was lost after its last kept frame/), JSON.stringify(mapped.focus.incidents));
  // The same 0.5 s before, on the probe clock: no slack, frames followed it, so judged.
  const exact = await restoreRun(t, { restoreCheckMs: 5000, wrapProbe: edit({ cut: 3500, list: [{ exactMs: 3000 }] }) });
  assert.equal(check(exact.focus).data?.outcome, "clean", JSON.stringify(check(exact.focus)));
  // A loss that cannot be placed in time is kept by the window filter and leaves the session unjudged.
  const unplaced = await restoreRun(t, { restoreCheckMs: 5000, wrapProbe: edit({ list: [{ wallMs: 1000, unplaced: true }] }) });
  assert.equal(check(unplaced.focus).state, "unknown", JSON.stringify(check(unplaced.focus)));
  assert.ok(unknownIncident(unplaced.focus, /not judged: the primary capture was lost after its last kept frame .*none placed in time/), JSON.stringify(unplaced.focus.incidents));
});

test("every setup is wired everywhere: an adapter, a fallback launch mode and an environment (a new setup cannot fall through to a guess)", async () => {
  const { SETUP_IDS, LAUNCH_MODES } = await import("../kpi-catalog.mjs");
  const { createAdapter } = await import("../adapters/index.mjs");
  const { ENVIRONMENT_SPECS } = await import("../envs/index.mjs");
  const { fallbackLaunchMode } = await import("../run.mjs");
  const src = readFileSync(new URL("../adapters/index.mjs", import.meta.url), "utf8");
  const body = src.slice(src.indexOf("export async function createAdapter"));
  const cases = [...body.slice(0, body.indexOf("default:")).matchAll(/case "([^"]+)":/g)].map((m) => m[1]);
  assert.deepEqual([...cases].sort(), [...SETUP_IDS].sort(), "createAdapter has a case for exactly the SETUP_IDS");
  await assert.rejects(createAdapter("no-such-setup", {}), /unknown setup "no-such-setup"/);
  const inSomeEnv = new Set(Object.values(ENVIRONMENT_SPECS).flatMap((e) => e.setups));
  for (const id of SETUP_IDS) {
    for (const o of [{}, { runnerMode: "supervisor", vscodeAppPath: "/x" }]) assert.ok(LAUNCH_MODES.includes(fallbackLaunchMode(id, o)), `${id}: ${fallbackLaunchMode(id, o)}`);
    assert.ok(inSomeEnv.has(id), `${id} is in no environment's setups`);
  }
  assert.throws(() => fallbackLaunchMode("no-such-setup", {}), /no launch mode for setup "no-such-setup"/);
});

test("a probe rewritten mid-run changes neither what runs nor what is recorded: the doctor and the driver run the pinned copy (S1)", async (t) => {
  const dir = mkdtempSync(join(tmpdir(), "tb-pin-run-"));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  const original = fakeProbeBinary(join(dir, "primary", "target-agent", "release"), "termhost-probe", "build A");
  const driverPaths = [];
  const doctorPaths = [];
  let rewrites = 0;
  const { res } = await fakeRun(t, {
    driverPaths,
    onArrange: () => {
      writeFileSync(original, `build B ${(rewrites += 1)}`);
    },
    deps: {
      probePath: original,
      probeSource: "primary_shared",
      gatherDoctorFacts: async (a) => {
        doctorPaths.push(a.probePath);
        return { ...healthyFacts(), probePath: a.probePath, probeTried: a.probeTried, probeSource: a.probeSource };
      },
    },
  });
  assert.ok(rewrites > 0, "the original was rewritten during the run");
  assert.equal(readFileSync(original, "utf8").startsWith("build B"), true);
  const copy = res.doc.run.probe.executed_path;
  assert.notEqual(copy, original);
  assert.deepEqual(doctorPaths, [copy], "the doctor ran the copy");
  assert.deepEqual(driverPaths, [copy], "every probe call goes through the copy");
  assert.equal(readFileSync(copy, "utf8"), "build A", "the copy is what the doctor checked");
  assert.equal(res.doc.run.probe.sha256, `sha256:${createHash("sha256").update("build A").digest("hex")}`);
  assert.equal(res.doc.run.probe.path, original);
  assert.equal(res.doc.run.probe.source, "primary_shared");
});

test("a probe found in the primary's shared target-agent runs DEGRADED, recorded as such (S2: the source reaches the doctor)", async (t) => {
  const dir = mkdtempSync(join(tmpdir(), "tb-shared-run-"));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  const shared = fakeProbeBinary(join(dir, "primary", "target-agent", "release"), "termhost-probe");
  const { res } = await fakeRun(t, {
    deps: {
      probePath: undefined,
      locateProbe: () => ({ path: shared, source: "primary_shared", tried: [shared] }),
      gatherDoctorFacts: async (a) => ({ ...healthyFacts(), probePath: a.probePath, probeTried: a.probeTried, probeSource: a.probeSource }),
    },
  });
  assert.equal(res.doc.capabilities["probe.binary"].state, "DEGRADED");
  assert.match(res.doc.capabilities["probe.binary"].reason, /provenance unproven/);
  assert.ok(res.doc.rows.length > 0, "the run proceeded");
  assert.equal(res.doc.run.probe.source, "primary_shared");
  assert.equal(res.doc.run.probe.path, shared);
  assert.equal(validateResults(res.doc).valid, true);
});

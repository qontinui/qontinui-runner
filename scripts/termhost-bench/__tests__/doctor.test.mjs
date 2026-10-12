/**
 * The doctor's verdict mapping (`doctor.mjs` `mapDoctor`): facts in, the
 * capability-doctor vocabulary out. An absence is INOPERATIVE, an unanswered probe
 * is UNKNOWN, a missing window manager is DEGRADED, and nothing is "fine" by default.
 *
 *   node --test scripts/termhost-bench/__tests__/doctor.test.mjs
 */

import { test } from "node:test";
import assert from "node:assert/strict";

import { DOCTOR_STATES } from "../kpi-catalog.mjs";
import {
  DEGRADED,
  INOPERATIVE,
  OPERATIVE,
  PROBE_MECHANISMS,
  UNKNOWN,
  blockingMechanisms,
  doctorReport,
  mapDoctor,
  renderDoctor,
  toCapabilities,
} from "../doctor.mjs";
import { healthyFacts } from "./helpers/fake-bench.mjs";

test("a healthy linux-xvfb box maps to OPERATIVE where it should, and carries the probe's verdicts through", () => {
  const m = mapDoctor(healthyFacts({ inputSafe: true }));
  for (const id of ["env.platform", "env.harness", "env.xvfb", "env.window_manager", "probe.binary", "node.version", "capture.x11", "input.safe", "clock.aligned", "session.interactive"]) {
    assert.equal(m[id].state, OPERATIVE, `${id}: ${m[id].reason}`);
  }
  assert.equal(m["capture.wgc"].state, INOPERATIVE);
  assert.equal(m["counters.gpu"].state, INOPERATIVE);
  assert.deepEqual(blockingMechanisms(m), []);
});

test("the wrong platform is INOPERATIVE-ON-THIS-MACHINE, and blocks the run", () => {
  const m = mapDoctor({ ...healthyFacts(), platform: "win32", probeDoctor: null, display: { started: false, detail: "starts only on Linux" } });
  assert.equal(m["env.platform"].state, INOPERATIVE);
  assert.match(m["env.platform"].reason, /runs on linux; this machine is win32/);
  assert.equal(m["env.xvfb"].state, INOPERATIVE);
  assert.ok(blockingMechanisms(m).some((b) => b.id === "env.platform"));
});

test("a missing probe is INOPERATIVE and its mechanisms are UNKNOWN, never assumed", () => {
  const m = mapDoctor({ ...healthyFacts(), probePath: null, probeDoctor: null });
  assert.equal(m["probe.binary"].state, INOPERATIVE);
  for (const id of PROBE_MECHANISMS) {
    assert.equal(m[id].state, UNKNOWN, id);
    assert.match(m[id].reason, /not probed/);
  }
  assert.ok(blockingMechanisms(m).some((b) => b.id === "probe.binary"));
});

test("a probe whose doctor did not answer is UNKNOWN, not OPERATIVE", () => {
  const m = mapDoctor({ ...healthyFacts(), probeDoctor: { record: null, error: "termhost-probe doctor printed no doctor record (exit 1)" } });
  assert.equal(m["probe.binary"].state, UNKNOWN);
  assert.match(m["probe.binary"].reason, /no doctor record/);
});

test("no display to probe on makes the probe's mechanisms UNKNOWN with that reason", () => {
  const m = mapDoctor({ ...healthyFacts(), probeDoctor: null, display: { started: false, detail: "Xvfb exited" } });
  assert.match(m["capture.x11"].reason, /no X display to probe on \(Xvfb exited\)/);
  assert.equal(m["capture.x11"].state, UNKNOWN);
});

test("a missing window manager is DEGRADED; old node and a missing Xvfb are INOPERATIVE", () => {
  const m = mapDoctor({ ...healthyFacts(), windowManagerPath: null, xvfbPath: null, nodeVersion: "v18.19.0" });
  assert.equal(m["env.window_manager"].state, DEGRADED);
  assert.equal(m["env.xvfb"].state, INOPERATIVE);
  assert.equal(m["node.version"].state, INOPERATIVE);
});

test("an environment not built yet is INOPERATIVE with its reason", () => {
  const m = mapDoctor({ ...healthyFacts(), environment: "windows-desktop", envPlatform: "win32", platform: "win32", envNotBuilt: "arrives with Phase 5" });
  assert.equal(m["env.harness"].state, INOPERATIVE);
  assert.equal(m["env.harness"].reason, "arrives with Phase 5");
  assert.equal(m["env.xvfb"], undefined, "no Xvfb check outside linux-xvfb");
});

test("setup verdicts pass through; a malformed one becomes UNKNOWN", () => {
  const m = mapDoctor({
    ...healthyFacts(),
    setupMechanisms: { "setup.tmux": { state: OPERATIVE, reason: "tmux 3.2a" }, "setup.runner": { state: "FINE", reason: "?" } },
  });
  assert.equal(m["setup.tmux"].state, OPERATIVE);
  assert.equal(m["setup.runner"].state, UNKNOWN);
});

test("the report counts every verdict and every id fits the results schema's capability pattern", () => {
  const m = mapDoctor(healthyFacts());
  const r = doctorReport(m, { environment: "linux-xvfb", checkedAt: "2026-10-12T00:00:00Z" });
  const total = r.summary.operative + r.summary.degraded + r.summary.inoperative + r.summary.unknown;
  assert.equal(total, r.mechanisms.length);
  for (const x of r.mechanisms) assert.ok(DOCTOR_STATES.includes(x.state));
  const caps = toCapabilities(m, "2026-10-12T00:00:00Z");
  for (const id of Object.keys(caps)) assert.match(id, /^[a-z][a-z0-9_]*(\.[a-z0-9_]+)+$/);
  assert.match(renderDoctor(r), /SUMMARY: \d+ INOPERATIVE-ON-THIS-MACHINE/);
});

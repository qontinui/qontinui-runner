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
import { drainMechanism, readDeviceDrain } from "../drain.mjs";
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
  const m = mapDoctor({ ...healthyFacts(), windowManagerPath: null });
  const r = doctorReport(m, { environment: "linux-xvfb", checkedAt: "2026-10-12T00:00:00Z" });
  const total = r.summary.operative + r.summary.degraded + r.summary.inoperative + r.summary.unknown;
  assert.equal(total, r.mechanisms.length);
  for (const [key, state] of [["operative", OPERATIVE], ["degraded", DEGRADED], ["inoperative", INOPERATIVE], ["unknown", UNKNOWN]]) {
    assert.equal(r.summary[key], r.mechanisms.filter((x) => x.state === state).length, key);
  }
  assert.equal(r.summary.degraded, 1, "the missing window manager");
  for (const x of r.mechanisms) assert.ok(DOCTOR_STATES.includes(x.state));
  const caps = toCapabilities(m, "2026-10-12T00:00:00Z");
  for (const id of Object.keys(caps)) assert.match(id, /^[a-z][a-z0-9_]*(\.[a-z0-9_]+)+$/);
  assert.match(renderDoctor(r), /SUMMARY: \d+ INOPERATIVE-ON-THIS-MACHINE/);
});

// ---------------------------------------------------------------------------
// Review round 1 (S3): the device drain, read before the first unit
// ---------------------------------------------------------------------------

/** A fake home: machine.json and the device JWT file as given (null = absent). */
function fakeHome({ machine = null, tokenFile = null }) {
  return (path) => {
    if (path.endsWith("machine.json")) {
      if (machine === null) throw Object.assign(new Error("ENOENT"), { code: "ENOENT" });
      if (machine === "EACCES") throw Object.assign(new Error("EACCES"), { code: "EACCES" });
      return typeof machine === "string" ? machine : JSON.stringify(machine);
    }
    if (path.endsWith("coord-device-jwt")) {
      if (tokenFile === null) throw Object.assign(new Error("ENOENT"), { code: "ENOENT" });
      return tokenFile;
    }
    throw new Error(`unexpected read ${path}`);
  };
}
const jwt = (exp) => `h.${Buffer.from(JSON.stringify({ exp })).toString("base64url")}.s`;
const DEVICE = { device_id: "0b7a3b1e-1c2d-4e5f-8a9b-0c1d2e3f4a5b" };
const NOW = 1_800_000_000_000;

test("the device drain: not enrolled and clear let a run start; drained and every unknown block it", async () => {
  const answer = (status, body) => async (url, init) => {
    assert.match(url, /\/coord\/devices\/me\/drain$/);
    assert.match(init.headers.Authorization, /^Bearer h\./);
    return { ok: status < 300, status, json: async () => body };
  };
  const live = jwt(NOW / 1000 + 3600);
  let unexpected = 0;
  const read = (o) =>
    readDeviceDrain({
      read: fakeHome(o),
      env: {},
      now: () => NOW,
      fetch: o.fetch ?? (async () => {
        unexpected += 1;
        throw new Error("no network call expected");
      }),
    });
  assert.equal((await read({ machine: null })).state, "not_enrolled", "every CI runner");
  assert.equal((await read({ machine: {} })).state, "not_enrolled");
  assert.equal((await read({ machine: "EACCES" })).state, "unknown", "an unreadable machine.json may name a drained device");
  assert.equal((await read({ machine: "{not json" })).state, "unknown");
  assert.equal((await read({ machine: { device_id: "not-a-uuid" } })).state, "unknown");
  assert.equal((await read({ machine: DEVICE })).state, "unknown", "a device with no token");
  const expired = await read({ machine: DEVICE, tokenFile: jwt(NOW / 1000 - 10) });
  assert.equal(expired.state, "unknown", "an expired token is no token");
  assert.match(expired.detail, /no live device JWT/);
  assert.equal(unexpected, 0, "no read is attempted without a live credential");
  assert.equal((await read({ machine: DEVICE, tokenFile: live, fetch: answer(200, { state: "clear" }) })).state, "clear");
  const drained = await read({ machine: DEVICE, tokenFile: live, fetch: answer(200, { state: "drained", reason: "rebuild" }) });
  assert.deepEqual([drained.state, /rebuild/.test(drained.detail)], ["drained", true]);
  assert.equal((await read({ machine: DEVICE, tokenFile: live, fetch: answer(200, { state: "unreadable" }) })).state, "unknown");
  assert.equal((await read({ machine: DEVICE, tokenFile: live, fetch: answer(503, {}) })).state, "unknown");
  const viaEnv = await readDeviceDrain({ read: fakeHome({ machine: DEVICE }), env: { COORD_DEVICE_JWT: live }, now: () => NOW, fetch: answer(200, { state: "clear" }) });
  assert.equal(viaEnv.state, "clear");
});

test("the doctor maps the drain read, and a drained or unknown drain blocks the run before any unit", () => {
  for (const [drain, state] of [
    [{ state: "not_enrolled", detail: "x" }, OPERATIVE],
    [{ state: "clear", detail: "x" }, OPERATIVE],
    [{ state: "drained", detail: "x" }, INOPERATIVE],
    [{ state: "unknown", detail: "x" }, UNKNOWN],
    [undefined, UNKNOWN],
  ]) {
    const m = mapDoctor({ ...healthyFacts(), drain });
    assert.equal(m["device.drain"].state, state, JSON.stringify(drain));
    assert.equal(blockingMechanisms(m).some((b) => b.id === "device.drain"), state !== OPERATIVE);
  }
  assert.equal(drainMechanism({ state: "drained", detail: "coord reports this device drained (rebuild)" }).state, INOPERATIVE);
});

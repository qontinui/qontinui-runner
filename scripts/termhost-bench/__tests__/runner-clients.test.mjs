/**
 * The shared supervisor and runner clients (`scripts/lib/runner-clients.mjs`):
 * the primary is never driven, every request is the one the method names, errors
 * keep perf-harness's messages while carrying status and body, the async
 * spawn-test path resolves or explains, the UI Bridge readiness wait pokes — and
 * the module stays a superset of perf-harness's private copies until the switch
 * sequenced behind runner#1856.
 *
 *   node --test scripts/termhost-bench/__tests__/runner-clients.test.mjs
 */

import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import {
  HttpError,
  PRIMARY_RUNNER_PORT,
  RunnerClient,
  SupervisorClient,
  httpJson,
  unwrap,
} from "../../lib/runner-clients.mjs";
import { classSource, functionSource, normalize, parityReport } from "./helpers/client-parity.mjs";

const HERE = dirname(fileURLToPath(import.meta.url));
const SCRIPTS = join(HERE, "..", "..");
const PERF_HARNESS = join(SCRIPTS, "perf-harness.mjs");
const CLIENTS = join(SCRIPTS, "lib", "runner-clients.mjs");

/** A recording fetch that answers from a script of responses (or a function). */
function fakeFetch(answers) {
  const calls = [];
  let i = 0;
  const fetch = async (url, init = {}) => {
    calls.push({ url, method: init.method, headers: init.headers, body: init.body === undefined ? undefined : JSON.parse(init.body) });
    const a = typeof answers === "function" ? answers(url, init, calls.length - 1) : answers[Math.min(i++, answers.length - 1)];
    if (a instanceof Error) throw a;
    const status = a.status ?? 200;
    const text = a.text ?? (a.json === undefined ? "" : JSON.stringify(a.json));
    return { ok: status >= 200 && status < 300, status, text: async () => text };
  };
  return { fetch, calls };
}

const quiet = () => {};

// ---------------------------------------------------------------------------
// httpJson / unwrap
// ---------------------------------------------------------------------------

test("httpJson sends JSON only when there is a body, and returns the parsed answer", async () => {
  const { fetch, calls } = fakeFetch([{ json: { ok: 1 } }, { text: "" }]);
  assert.deepEqual(await httpJson("http://127.0.0.1:9880/a", { fetch }), { ok: 1 });
  assert.equal(calls[0].method, "GET");
  assert.equal(calls[0].headers, undefined);
  assert.equal(await httpJson("http://127.0.0.1:9880/b", { method: "POST", body: { x: 1 }, fetch }), null, "an empty body is null");
  assert.deepEqual(calls[1].headers, { "Content-Type": "application/json" });
  assert.deepEqual(calls[1].body, { x: 1 });
});

test("a non-2xx answer is an HttpError with perf-harness's message, plus status and body", async () => {
  const { fetch } = fakeFetch([{ status: 409, json: { error: "conflict", detail: "held" } }]);
  const err = await httpJson("http://127.0.0.1:9880/x", { method: "POST", body: {}, fetch }).catch((e) => e);
  assert.ok(err instanceof HttpError && err instanceof Error);
  assert.equal(err.message, 'POST http://127.0.0.1:9880/x -> 409 {"error":"conflict","detail":"held"}');
  assert.equal(err.status, 409);
  assert.deepEqual(err.body, { error: "conflict", detail: "held" });
  const html = fakeFetch([{ status: 502, text: "<html>bad gateway</html>" }]);
  const e2 = await httpJson("http://127.0.0.1:9880/y", { fetch: html.fetch }).catch((e) => e);
  assert.equal(e2.message, "GET http://127.0.0.1:9880/y -> 502, non-JSON body: <html>bad gateway</html>");
  assert.equal(e2.body, "<html>bad gateway</html>");
});

test("httpJson aborts a request that outlives its timeout", async () => {
  const fetch = (url, init) =>
    new Promise((_, reject) => init.signal.addEventListener("abort", () => reject(Object.assign(new Error("aborted"), { name: "AbortError" }))));
  await assert.rejects(httpJson("http://127.0.0.1:9880/slow", { fetch, timeoutMs: 20 }), { name: "AbortError" });
});

test("unwrap opens the runner envelope, refuses a failed one and passes a bare body through", () => {
  assert.deepEqual(unwrap({ success: true, data: { a: 1 } }), { a: 1 });
  assert.throws(() => unwrap({ success: false, data: null, error: "nope" }), /runner API error: .*nope/);
  assert.deepEqual(unwrap({ a: 1 }), { a: 1 });
  assert.equal(unwrap(null), null);
});

// ---------------------------------------------------------------------------
// The primary is never driven
// ---------------------------------------------------------------------------

test("RunnerClient refuses the primary's port, with perf-harness's words", () => {
  assert.equal(PRIMARY_RUNNER_PORT, 9876);
  assert.throws(() => new RunnerClient("http://127.0.0.1:9876", quiet), /refusing to drive port 9876: that is the primary runner/);
  assert.throws(() => new RunnerClient("http://localhost:9876/", quiet), /primary runner/);
  assert.doesNotThrow(() => new RunnerClient("http://127.0.0.1:9877", quiet));
});

// ---------------------------------------------------------------------------
// SupervisorClient: the async spawn-test path
// ---------------------------------------------------------------------------

test("spawnTestAsync forces async and insists on a submission id", async () => {
  const { fetch, calls } = fakeFetch([{ status: 202, json: { submission_id: "s-1", id: "test-9881", port: 9881 } }, { status: 202, json: { id: "x" } }]);
  const sup = new SupervisorClient("http://127.0.0.1:9875/", quiet, { fetch });
  const res = await sup.spawnTestAsync({ git_ref: "abc", async: false, rebuild: true });
  assert.equal(res.submission_id, "s-1");
  assert.equal(calls[0].url, "http://127.0.0.1:9875/runners/spawn-test");
  assert.equal(calls[0].method, "POST");
  assert.equal(calls[0].body.async, true);
  assert.equal(calls[0].body.git_ref, "abc");
  await assert.rejects(sup.spawnTestAsync({ git_ref: "abc" }), /without a submission_id/);
});

test("awaitSpawn polls the submission until its spawn outcome, and returns it unjudged", async () => {
  const { fetch, calls } = fakeFetch([
    { json: { status: { state: "queued" } } },
    { status: 503, json: { error: "busy" } },
    { json: { status: { state: "running" } } },
    { json: { status: { state: "succeeded" }, spawn: { http_status: 503, port: 9881, body: { error: "frontend_stale" } } } },
  ]);
  const sup = new SupervisorClient("http://127.0.0.1:9875", quiet, { fetch });
  let t = 0;
  const res = await sup.awaitSpawn("s-1", { timeoutMs: 60_000, pollMs: 5000, sleepFn: async (ms) => { t += ms; }, now: () => t });
  assert.deepEqual(res, { httpStatus: 503, port: 9881, body: { error: "frontend_stale" }, status: { state: "succeeded" } });
  assert.equal(calls.length, 4);
  assert.ok(calls.every((c) => c.url === "http://127.0.0.1:9875/build/s-1/status"));
});

test("awaitSpawn: a vanished submission names the in-flight recovery; a failed build is a 500; silence times out", async () => {
  const sup404 = new SupervisorClient("http://127.0.0.1:9875", quiet, { fetch: fakeFetch([{ status: 404, json: { error: "unknown" } }]).fetch });
  await assert.rejects(sup404.awaitSpawn("s-2", { sleepFn: async () => {} }), /GET \/runners\/spawn-test\/in-flight/);
  const failed = new SupervisorClient("http://127.0.0.1:9875", quiet, { fetch: fakeFetch([{ json: { status: { state: "failed", error: "cargo exit 101" } } }]).fetch });
  const r = await failed.awaitSpawn("s-3", { sleepFn: async () => {} });
  assert.equal(r.httpStatus, 500);
  assert.deepEqual(r.body, { error: "build_failed", detail: "cargo exit 101" });
  let t = 0;
  const slow = new SupervisorClient("http://127.0.0.1:9875", quiet, { fetch: fakeFetch([{ json: { status: { state: "running" } } }]).fetch });
  await assert.rejects(
    slow.awaitSpawn("s-4", { timeoutMs: 20_000, pollMs: 5000, sleepFn: async (ms) => { t += ms; }, now: () => t }),
    /no spawn outcome within 20s \(last state running\)/,
  );
});

test("spawnInFlight filters by requester; stopRunner addresses the temp id", async () => {
  const { fetch, calls } = fakeFetch([{ json: { in_flight: [] } }, { json: {} }]);
  const sup = new SupervisorClient("http://127.0.0.1:9875", quiet, { fetch });
  await sup.spawnInFlight({ requesterId: "termhost bench" });
  assert.equal(calls[0].url, "http://127.0.0.1:9875/runners/spawn-test/in-flight?requester_id=termhost%20bench");
  await sup.stopRunner("test-9881");
  assert.equal(calls[1].url, "http://127.0.0.1:9875/runners/test-9881/stop");
  assert.equal(calls[1].method, "POST");
});

// ---------------------------------------------------------------------------
// RunnerClient: the UI Bridge doors and the #1856 counters
// ---------------------------------------------------------------------------

test("component and element actions post to the UI Bridge control doors", async () => {
  const { fetch, calls } = fakeFetch(() => ({ json: { success: true, data: { ok: true } } }));
  const r = new RunnerClient("http://127.0.0.1:9881", quiet, { fetch });
  assert.deepEqual(await r.componentAction("terminal-launch-menu", "create-with-command", { count: 2, command: "x" }), { ok: true });
  assert.equal(calls[0].url, "http://127.0.0.1:9881/ui-bridge/control/component/terminal-launch-menu/action/create-with-command");
  assert.deepEqual(calls[0].body, { params: { count: 2, command: "x" } });
  await r.elementAction("terminal-input-t 1", "focus");
  assert.equal(calls[1].url, "http://127.0.0.1:9881/ui-bridge/control/element/terminal-input-t%201/action");
  assert.deepEqual(calls[1].body, { action: "focus" }, "no params key when none are given");
  await r.elementAction("e", "type", { text: "a" });
  assert.deepEqual(calls[2].body, { action: "type", params: { text: "a" } });
  await r.getElement("terminal-zone-header-0");
  assert.equal(calls[3].url, "http://127.0.0.1:9881/ui-bridge/control/element/terminal-zone-header-0");
  await r.discover();
  assert.deepEqual(calls[4].body, { force: true });
  await r.coordSession("t1");
  assert.equal(calls[5].url, "http://127.0.0.1:9881/terminals/t1/coord-session");
  await r.terminalText("t1");
  assert.equal(calls[6].url, "http://127.0.0.1:9881/ui-bridge/sdk/terminal/sessions/t1/text");
  await r.restartReadiness();
  assert.equal(calls[7].url, "http://127.0.0.1:9881/restart-readiness");
  assert.ok(!calls.some((c) => /\/buffer\b/.test(c.url)), "no client method reads /terminals/{id}/buffer (it resets flow control)");
});

test("a UI Bridge refusal surfaces as an HttpError whose body carries the frontend's error", async () => {
  const { fetch } = fakeFetch([{ status: 400, json: { success: false, error: "TERMINAL_NO_MOUNTED_VIEW: focus needs a mounted view" } }]);
  const r = new RunnerClient("http://127.0.0.1:9881", quiet, { fetch });
  const err = await r.elementAction("terminal-input-t1", "focus").catch((e) => e);
  assert.equal(err.status, 400);
  assert.match(err.body.error, /^TERMINAL_NO_MOUNTED_VIEW/);
});

test("the #1856 transport counters: reset is a POST, the read is raw, alt_screen is boolean or null", async () => {
  const { fetch, calls } = fakeFetch([
    { json: { success: true, data: {} } },
    { json: { success: true, data: { frames: 3 } } },
    { json: { success: true, data: { alt_screen: true } } },
    { json: { success: true, data: { rows: 24 } } },
  ]);
  const r = new RunnerClient("http://127.0.0.1:9881", quiet, { fetch });
  await r.resetTransportStats();
  assert.deepEqual([calls[0].method, calls[0].url, calls[0].body], ["POST", "http://127.0.0.1:9881/terminals/transport-stats", {}]);
  assert.deepEqual(await r.transportStats(), { success: true, data: { frames: 3 } }, "raw: the caller handles envelope or bare");
  assert.equal(await r.gridAltScreen("t1"), true);
  assert.equal(calls[2].url, "http://127.0.0.1:9881/ui-bridge/sdk/terminal/sessions/t1/grid");
  assert.equal(await r.gridAltScreen("t1"), null, "a grid without the flag is unknown, not false");
});

test("waitUiBridgeReady pokes the elements door once the shell is responsive, until the IPC round trip is observed", async () => {
  const health = [
    { success: true, data: { responsive: false, uiBridgeIpcObserved: false } },
    { success: true, data: { responsive: true, uiBridgeIpcObserved: false } },
    { success: true, data: { responsive: true, uiBridgeIpcObserved: true, version: "x" } },
  ];
  let h = 0;
  const { fetch, calls } = fakeFetch((url) => (url.endsWith("/health") ? { json: health[Math.min(h++, health.length - 1)] } : { status: 503, json: { error: "warming" } }));
  const r = new RunnerClient("http://127.0.0.1:9881", quiet, { fetch });
  let t = 0;
  const data = await r.waitUiBridgeReady(60_000, { sleepFn: async (ms) => { t += ms; }, now: () => t });
  assert.equal(data.version, "x");
  const pokes = calls.filter((c) => c.url.endsWith("/ui-bridge/control/elements"));
  assert.equal(pokes.length, 1, "poked only while responsive, and a 503 poke is tolerated");
});

test("waitUiBridgeReady fails fast when the process exits, and times out with the last reading", async () => {
  const { fetch } = fakeFetch([{ json: { success: true, data: { responsive: false, uiBridgeIpcObserved: false } } }]);
  const r = new RunnerClient("http://127.0.0.1:9881", quiet, { fetch });
  await assert.rejects(r.waitUiBridgeReady(60_000, { alive: () => false, sleepFn: async () => {} }), /exited before becoming ready/);
  let t = 0;
  await assert.rejects(
    r.waitUiBridgeReady(3000, { sleepFn: async (ms) => { t += ms; }, now: () => t }),
    /did not report uiBridgeIpcObserved:true within 3s \(uiBridgeIpcObserved=false, responsive=false\)/,
  );
});

// ---------------------------------------------------------------------------
// Parity with perf-harness's private copies (until the switch behind #1856)
// ---------------------------------------------------------------------------

test("runner-clients.mjs is a superset of perf-harness's private clients (or perf-harness imports it)", () => {
  const report = parityReport(readFileSync(PERF_HARNESS, "utf8"), readFileSync(CLIENTS, "utf8"));
  assert.deepEqual(
    report.problems,
    [],
    "perf-harness.mjs's private client drifted from scripts/lib/runner-clients.mjs. Mirror the change in " +
      "runner-clients.mjs (same name, arguments, request and return), or finish the switch: delete perf-harness's " +
      "private SupervisorClient/RunnerClient/httpJson/unwrap and import them from ./lib/runner-clients.mjs.",
  );
  if (report.mode === "private") {
    for (const m of ["SupervisorClient.spawnTest", "RunnerClient.evaluate", "RunnerClient.createTerminal", "httpJson", "unwrap"]) {
      assert.ok(report.checked.includes(m), `${m} was compared`);
    }
  }
});

test("the parity check can fail: a drifted method body and a missing method are both caught", () => {
  const harness = readFileSync(PERF_HARNESS, "utf8");
  const clients = readFileSync(CLIENTS, "utf8");
  if (classSource(harness, "RunnerClient") === null) return; // after the switch there is nothing to drift
  const drifted = clients.replace("/log-sources/runner-log-sink`", "/log-sources/runner-log-sink-v2`");
  assert.match(parityReport(harness, drifted).problems.join("\n"), /RunnerClient\.logSink differs/);
  const missing = clients.replace(/\n {2}async activateTab\(tabId\) \{[\s\S]*?\n {2}\}\n/, "\n");
  assert.match(parityReport(harness, missing).problems.join("\n"), /RunnerClient\.activateTab is in perf-harness but missing/);
  // Formatting alone is not drift.
  assert.equal(normalize("return httpJson(`${a}/x`, {\n  timeoutMs: 1,\n},\n);"), normalize("return this.http(`${a}/x`, { timeoutMs: 1 });"));
});

test("httpJson and unwrap throw perf-harness's exact messages (run side by side)", async () => {
  const harness = readFileSync(PERF_HARNESS, "utf8");
  const theirsSrc = functionSource(harness, "httpJson");
  const theirUnwrapSrc = functionSource(harness, "unwrap");
  if (theirsSrc === null || theirUnwrapSrc === null) return; // switched: perf-harness uses these very functions
  const answers = [{ status: 404, json: { error: "no such terminal" } }, { status: 500, text: "oops <b>" }, { json: { a: 1 } }];
  for (const a of answers) {
    const { fetch } = fakeFetch([a]);
    // perf-harness's function with a local `fetch` shadowing the global.
    const theirs = new Function("fetch", `${theirsSrc}\nreturn httpJson;`)(fetch);
    const t = await theirs("http://127.0.0.1:9881/x", { method: "POST", body: { q: 1 } }).then((v) => ({ v }), (e) => ({ e: e.message }));
    const o = await httpJson("http://127.0.0.1:9881/x", { method: "POST", body: { q: 1 }, fetch }).then((v) => ({ v }), (e) => ({ e: e.message }));
    assert.deepEqual(o, t);
  }
  const theirUnwrap = new Function(`${theirUnwrapSrc}\nreturn unwrap;`)();
  const env = { success: false, data: null, error: "x" };
  assert.throws(() => unwrap(env), { message: (() => { try { theirUnwrap(env); } catch (e) { return e.message; } })() });
});

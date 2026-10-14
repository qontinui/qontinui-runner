/**
 * perf-harness's Phase 3 fixes (plan
 * `2026-10-11-terminal-page-performance-kpis-across-setups`): it drives the
 * runner through the shared `scripts/lib/runner-clients.mjs` rather than private
 * copies, creates its terminals on the page the Terminal tab is showing, closes
 * every terminal it created (an attached `--runner-url` run included), and no
 * longer fabricates — in the dry-run fake — the two spawn spans the
 * `POST /terminals` path never opens.
 *
 * It lives beside termhost-bench's tests because that is the suite CI runs
 * (`ci.yml` "Run termhost-bench unit tests"); `scripts/__tests__/` is not wired.
 *
 *   node --test scripts/termhost-bench/__tests__/perf-harness.test.mjs
 *
 * No runner is started: an attached run is driven against a recording `fetch`
 * on a temp-runner port (never 9876).
 */

import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, readdirSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import {
  DEFAULT_TERMINAL_PAGE,
  PERF_PROBE_ACTIONS,
  PERF_PROBE_COMPONENT,
  TERMINAL_PAGE_COMPONENT,
  VIEW_STATE_ACTION,
  closeCreated,
  probeAction,
  resolveTerminalPage,
  runHarness,
} from "../../perf-harness.mjs";
import { HTTP_PATH_UNOPENED_SPANS, renderSingleRun } from "../../perf-harness-lib.mjs";

const HERE = dirname(fileURLToPath(import.meta.url));
const REPO = join(HERE, "..", "..", "..");
const PERF_HARNESS = join(REPO, "scripts", "perf-harness.mjs");
const RUNNER_URL = "http://127.0.0.1:9881";

const quiet = () => {};

/**
 * An attached run against the fake answers in milliseconds. A harness that
 * bypassed the shared client's fetch injection would dial 127.0.0.1:9881 for
 * real and sit in `waitHealthy` for two minutes, so bound it: fail, not hang.
 */
const ATTACHED = { timeout: 15000 };

/** A config shaped like the one `main()` builds, for a small attached run. */
function attachedConfig(over = {}) {
  return {
    dryRun: false,
    supervisor: "http://127.0.0.1:9875",
    runnerUrl: RUNNER_URL,
    rebuild: false,
    gitRef: null,
    worktree: null,
    useLkg: false,
    keepRunner: false,
    skipFrontend: false,
    sessions: [2, 3],
    generator: "echo hi",
    generatorPreset: null,
    soakMs: 5,
    settleMs: 1,
    createTimeoutMs: 5000,
    spawnTimeoutSecs: 10,
    rustLog: "info",
    label: "unit",
    ...over,
  };
}

/**
 * A fake temp runner behind `fetch`: the routes an attached perf-harness run
 * calls, a terminal table with one pre-existing terminal, and a record of every
 * request. `activePage` is the `pageId` the Terminal page's view-state action
 * reports (`null` = a build whose action predates the field). `perfProbe: false`
 * is a build whose webview has no `perf-probe` component (the door's own 404).
 */
function fakeRunner({ activePage = "page-7", failDelete = new Set(), perfProbe = true, sampleError = null } = {}) {
  const dir = mkdtempSync(join(tmpdir(), "perf-harness-test-"));
  const logFile = join(dir, "qontinui-runner.log");
  writeFileSync(logFile, "");
  const terminals = new Map([["pre-1", { id: "pre-1", isAlive: true, totalBytesProduced: 0 }]]);
  const calls = [];
  let seq = 0;
  const ok = (data) => ({ status: 200, json: { success: true, data } });
  const route = (method, path, body) => {
    if (method === "GET" && path === "/health") return ok({ gitSha: "abc", buildId: "unit" });
    if (method === "GET" && path === "/log-sources/runner-log-sink") return ok({ current_file: logFile, exists: true });
    if (method === "POST" && path === "/ui-bridge/control/tab/activate") return ok(null);
    if (method === "POST" && path === `/ui-bridge/control/component/${TERMINAL_PAGE_COMPONENT}/action/${VIEW_STATE_ACTION}`) {
      const result = { maximizedZone: null, zoneCount: 1, layoutId: "single" };
      if (activePage !== null) result.pageId = activePage;
      return ok({ success: true, result, durationMs: 1 });
    }
    const probe = /^\/ui-bridge\/control\/component\/perf-probe\/action\/([^/]+)$/.exec(path);
    if (method === "POST" && probe) {
      if (!perfProbe) {
        return { status: 404, json: { success: false, error: "Component not found: perf-probe", code: "ELEMENT_NOT_FOUND" } };
      }
      if (sampleError && decodeURIComponent(probe[1]) === "sample") return { status: 500, json: { success: false, error: sampleError } };
      const result = PROBE_ANSWERS[decodeURIComponent(probe[1])];
      if (!result) return { status: 400, json: { success: false, error: `Action ${probe[1]} not found on component perf-probe` } };
      return ok({ success: true, result, durationMs: 1 });
    }
    if (method === "POST" && path === "/ui-bridge/control/page/evaluate") {
      // The webview CSP refuses every evaluated string (no 'unsafe-eval'), as on a real build.
      return { status: 400, json: { code: "INVALID_JSON", error: "JS evaluation error: Refused to evaluate a string as JavaScript" } };
    }
    if (method === "GET" && path === "/terminals") return ok([...terminals.values()]);
    if (method === "POST" && path === "/terminals") {
      const id = `t-${seq++}`;
      terminals.set(id, { id, isAlive: true, totalBytesProduced: 0 });
      return ok({ id, isAlive: true });
    }
    let m = /^\/terminals\/([^/]+)\/write$/.exec(path);
    if (method === "POST" && m) return ok(null);
    m = /^\/terminals\/([^/]+)$/.exec(path);
    if (method === "DELETE" && m) {
      const id = decodeURIComponent(m[1]);
      if (failDelete.has(id)) return { status: 500, json: { error: "close failed" } };
      terminals.delete(id);
      return ok(null);
    }
    if (/\/grid$/.test(path)) return ok({ alt_screen: false });
    return { status: 404, json: { error: `no route ${method} ${path}` } };
  };
  // A supervisor on :9875 whose spawn-test hands back the fake runner on :9881.
  const supervisorRoute = (method, path) => {
    if (method === "GET" && path === "/health") return { status: 200, json: { status: "ok", build: { available_slots: 1 } } };
    if (method === "POST" && path === "/runners/spawn-test") return { status: 200, json: { id: "test-9881", port: 9881, api_url: RUNNER_URL } };
    if (method === "POST" && path === "/runners/test-9881/stop") return { status: 200, json: { stopped: true } };
    return { status: 404, json: { error: `no supervisor route ${method} ${path}` } };
  };
  const fetch = async (url, init = {}) => {
    const u = new URL(url);
    assert.notEqual(u.port, "9876", "the primary runner is never addressed");
    const method = init.method ?? "GET";
    const body = init.body === undefined ? undefined : JSON.parse(init.body);
    calls.push({ method, path: u.pathname, body, port: u.port });
    const a = u.port === "9875" ? supervisorRoute(method, u.pathname, body) : route(method, u.pathname, body);
    const text = JSON.stringify(a.json);
    return { ok: a.status >= 200 && a.status < 300, status: a.status, text: async () => text };
  };
  return { fetch, calls, terminals };
}

/** The `perf-probe` answers, in the shapes `src/lib/perfProbe.ts` returns. */
const PROBE_ANSWERS = {
  install: { installed: true, longTaskObserver: true, errors: [] },
  reset: { reset: true },
  sample: {
    elapsedMs: 1000,
    frameGapsMs: [16, 17, 18, 40],
    longTasks: [{ start: 1, duration: 60 }],
    webviewOutputEvents: 12,
    webviewHooked: true,
    errors: [],
    ipc: { fallbackWarned: false, postMessageHooked: true, postMessageCalls: 0 },
  },
  stop: { stopped: true },
  "transport-reset": { available: true },
  "transport-sample": {
    available: true,
    stats: {
      enabled: true,
      sinceResetMs: 1000,
      decodeNs: { pane: 1, tap: 2 },
      decodeCalls: { pane: 1, tap: 1 },
      decodeBytes: { pane: 10, tap: 10 },
      writeToRenderNs: 5,
      writeToRenderCount: 1,
      writeToRenderBytes: 10,
      eventsDelivered: 12,
      eventsForeign: 0,
      ringReplay: { fetches: 0, bytesFetched: 0, bytesWritten: 0 },
      rawIpc: null,
    },
  },
};

// ---------------------------------------------------------------------------
// The shared clients
// ---------------------------------------------------------------------------

test("perf-harness imports the shared clients and keeps no private copy of them", () => {
  const src = readFileSync(PERF_HARNESS, "utf8");
  assert.match(src, /from "\.\/lib\/runner-clients\.mjs"/);
  for (const name of ["class SupervisorClient", "class RunnerClient", "function httpJson(", "function unwrap("]) {
    assert.ok(!src.includes(name), `perf-harness.mjs defines ${name} privately again`);
  }
});

test("an attached run goes through the shared RunnerClient (its fetch injection reaches every request)", ATTACHED, async () => {
  const r = fakeRunner();
  await runHarness(attachedConfig(), quiet, { fetch: r.fetch });
  // The private client had no injection: it would have dialled 127.0.0.1:9881 for real.
  assert.ok(r.calls.some((c) => c.method === "GET" && c.path === "/health"));
  assert.ok(r.calls.filter((c) => c.method === "POST" && c.path === "/terminals").length === 2);
});

test("--runner-url on the primary's port is still refused", async () => {
  await assert.rejects(runHarness(attachedConfig({ runnerUrl: "http://127.0.0.1:9876" }), quiet), /refusing to drive port 9876/);
});

// ---------------------------------------------------------------------------
// Fix 1 — terminals go on the active page
// ---------------------------------------------------------------------------

test("terminals are created on the page the Terminal tab is showing, never on a page of their own", ATTACHED, async () => {
  const r = fakeRunner({ activePage: "page-7" });
  const run = await runHarness(attachedConfig(), quiet, { fetch: r.fetch });
  const creates = r.calls.filter((c) => c.method === "POST" && c.path === "/terminals");
  assert.equal(creates.length, 2, "S=2 then S=3 on top of one pre-existing terminal");
  for (const c of creates) assert.equal(c.body.page_id, "page-7");
  assert.equal(run.terminalPage.pageId, "page-7");
  assert.match(run.terminalPage.source, /^frontend active page/);
  // The read happens after the tab is activated: that is when the page is the one shown.
  const activate = r.calls.findIndex((c) => c.path === "/ui-bridge/control/tab/activate");
  const read = r.calls.findIndex((c) => c.path.endsWith(`/action/${VIEW_STATE_ACTION}`));
  assert.ok(activate >= 0 && read > activate);
});

test("a view state without pageId falls back to default and says so; --skip-frontend never asks", ATTACHED, async () => {
  const r = fakeRunner({ activePage: null });
  const run = await runHarness(attachedConfig({ sessions: [2] }), quiet, { fetch: r.fetch });
  assert.equal(run.terminalPage.pageId, DEFAULT_TERMINAL_PAGE);
  assert.equal(r.calls.find((c) => c.method === "POST" && c.path === "/terminals").body.page_id, "default");
  assert.match(run.terminalPage.source, /reported no pageId/);

  const s = fakeRunner();
  const skipped = await runHarness(attachedConfig({ sessions: [2], skipFrontend: true }), quiet, { fetch: s.fetch });
  assert.equal(skipped.terminalPage.pageId, DEFAULT_TERMINAL_PAGE);
  assert.match(skipped.terminalPage.source, /--skip-frontend/);
  assert.equal(s.calls.filter((c) => c.path.startsWith("/ui-bridge/control/")).length, 0, "no frontend door is asked");
});

test("a failed view-state read falls back to default and says so", async () => {
  const thrown = { componentAction: async () => { throw new Error("UI Bridge request timed out"); } };
  assert.deepEqual(await resolveTerminalPage(thrown), {
    pageId: "default",
    source: `fallback: ${VIEW_STATE_ACTION} failed (UI Bridge request timed out)`,
  });
  const refused = { componentAction: async () => ({ success: false, error: "component not registered" }) };
  assert.match((await resolveTerminalPage(refused)).source, /failed \(component not registered\)/);
});

test("the page is read through the Terminal page's own view-state action, never page/evaluate", () => {
  const page = readFileSync(join(REPO, "src", "components", "terminal", "TerminalPage.tsx"), "utf8");
  const action = page.slice(page.indexOf(`id: "${VIEW_STATE_ACTION}"`));
  assert.ok(page.includes(`id: "${TERMINAL_PAGE_COMPONENT}"`), "the component id the harness addresses");
  assert.match(action.slice(0, action.indexOf("}),")), /pageId: zoneMaximizeRef\.current\.pageId/, "the action reports the page it shows");
  // page/evaluate is refused by the CSP on every build: no evaluated string can read anything.
  const conf = JSON.parse(readFileSync(join(REPO, "src-tauri", "tauri.conf.json"), "utf8"));
  assert.doesNotMatch(JSON.stringify(conf), /'unsafe-eval'/);
  assert.ok(!readFileSync(PERF_HARNESS, "utf8").includes("qontinui-terminal-active-page"), "no evaluated localStorage read");
  const manager = readFileSync(join(REPO, "src-tauri", "src", "terminal", "manager.rs"), "utf8");
  assert.match(manager, /page_id\.unwrap_or_else\(\|\| "default"\.to_string\(\)\)/, "a page-less create lands on default");
});

// ---------------------------------------------------------------------------
// Fix 2 — close what the run created
// ---------------------------------------------------------------------------

test("an attached run closes every terminal it created and leaves the pre-existing one alone", ATTACHED, async () => {
  const r = fakeRunner();
  const run = await runHarness(attachedConfig(), quiet, { fetch: r.fetch });
  const deletes = r.calls.filter((c) => c.method === "DELETE").map((c) => c.path);
  assert.deepEqual(deletes.sort(), ["/terminals/t-0", "/terminals/t-1"]);
  assert.deepEqual([...r.terminals.keys()], ["pre-1"]);
  assert.equal(run.cleanup.created, 2);
  assert.deepEqual(run.cleanup.closed.sort(), ["t-0", "t-1"]);
  assert.deepEqual(run.cleanup.failed, []);
});

test("a close that fails is recorded in the run, and the other closes still happen", ATTACHED, async () => {
  const r = fakeRunner({ failDelete: new Set(["t-0"]) });
  const run = await runHarness(attachedConfig(), quiet, { fetch: r.fetch });
  assert.deepEqual(run.cleanup.closed, ["t-1"]);
  assert.equal(run.cleanup.failed.length, 1);
  assert.equal(run.cleanup.failed[0].id, "t-0");
  assert.ok(run.notes.some((n) => n.includes("could not be closed") && n.includes("t-0")));
});

test("--keep-runner keeps the harness's terminals too, and lists them", async () => {
  const closed = [];
  const out = await closeCreated({ closeTerminal: async (id) => closed.push(id) }, [{ id: "a" }, { id: "b" }], { keep: true });
  assert.deepEqual(closed, []);
  assert.deepEqual(out, { created: 2, closed: [], kept: ["a", "b"], failed: [] });
});

test("a dry run ends with no harness terminal left in the fake", async () => {
  const run = await runHarness({ ...attachedConfig({ runnerUrl: null }), dryRun: true, sessions: [2, 4] }, quiet);
  assert.equal(run.cleanup.created, 4);
  assert.equal(run.cleanup.closed.length, 4);
});

// ---------------------------------------------------------------------------
// Fix 3 — spans the HTTP path never opens
// ---------------------------------------------------------------------------

test("the dry-run fake no longer fabricates spans POST /terminals never opens; each level marks them not applicable", async () => {
  const run = await runHarness({ ...attachedConfig({ runnerUrl: null }), dryRun: true, sessions: [2, 3] }, quiet);
  for (const level of run.levels) {
    assert.ok(level.spawnSpanSampleCount > 0, "the fake still emits the spans the path does open");
    for (const span of Object.keys(HTTP_PATH_UNOPENED_SPANS)) {
      assert.ok(!(span in level.spawnSpans), `${span} was fabricated at S=${level.sessions}`);
      assert.match(level.spawnSpansNotApplicable[span], /terminal_create/);
    }
  }
  assert.match(renderSingleRun(run), /Not applicable on the `POST \/terminals` path[^\n]*terminal_spawn\.manager_create/);
});

test("the not-applicable list is exactly the spans only the Tauri terminal_create command opens", () => {
  const spanNames = (text) => new Set([...text.matchAll(/"(terminal_spawn\.[a-z_]+)"/g)].map((m) => m[1]));
  const src = join(REPO, "src-tauri", "src");
  const command = spanNames(readFileSync(join(src, "commands", "terminal.rs"), "utf8"));
  // The HTTP door and everything it calls into: the terminal module and mcp/terminals.rs.
  const httpPath = new Set();
  for (const f of readdirSync(join(src, "terminal"), { recursive: true })) {
    if (String(f).endsWith(".rs")) for (const s of spanNames(readFileSync(join(src, "terminal", String(f)), "utf8"))) httpPath.add(s);
  }
  for (const s of spanNames(readFileSync(join(src, "mcp", "terminals.rs"), "utf8"))) httpPath.add(s);
  const onlyCommand = [...command].filter((s) => !httpPath.has(s)).sort();
  assert.deepEqual(Object.keys(HTTP_PATH_UNOPENED_SPANS).sort(), onlyCommand);
  assert.ok(httpPath.has("terminal_spawn.identity_seam"), "the scan sees the HTTP path's own spans");
});

test("closes are issued together, so one hung DELETE does not hold the others (or the runner stop)", async () => {
  let inFlight = 0;
  let peak = 0;
  const runner = {
    closeTerminal: async () => {
      inFlight += 1;
      peak = Math.max(peak, inFlight);
      await new Promise((r) => setTimeout(r, 20));
      inFlight -= 1;
    },
  };
  const out = await closeCreated(runner, [{ id: "a" }, { id: "b" }, { id: "c" }]);
  assert.equal(peak, 3);
  assert.deepEqual(out.closed, ["a", "b", "c"], "the order of the created list is kept");
});

// ---------------------------------------------------------------------------
// The frontend probe is the bundled perf-probe component, never page/evaluate
// (plan 2026-10-14-perf-harness-frontend-probe-evaluates-strings-the-csp-refuses)
// ---------------------------------------------------------------------------

const probePath = (action) => `/ui-bridge/control/component/${PERF_PROBE_COMPONENT}/action/${action}`;

test("an attached run never posts to page/evaluate: the CSP refuses every evaluated string", ATTACHED, async () => {
  const r = fakeRunner();
  await runHarness(attachedConfig(), quiet, { fetch: r.fetch });
  assert.deepEqual(r.calls.filter((c) => c.path === "/ui-bridge/control/page/evaluate"), []);
  // And no evaluated string survives in the harness to be sent later.
  const src = readFileSync(PERF_HARNESS, "utf8");
  assert.ok(!/runner\.evaluate\(/.test(src), "perf-harness still calls runner.evaluate");
  for (const gone of ["PROBE_INSTALL", "PROBE_SAMPLE", "TRANSPORT_SAMPLE", "__qontinuiPerfProbe"]) {
    assert.ok(!src.includes(gone), `perf-harness still carries ${gone}`);
  }
});

test("an attached run drives every perf-probe action and measures the frontend from them", ATTACHED, async () => {
  const r = fakeRunner();
  const run = await runHarness(attachedConfig(), quiet, { fetch: r.fetch });
  const driven = r.calls.filter((c) => c.method === "POST" && c.path.startsWith(probePath("")));
  const actions = new Set(driven.map((c) => c.path.slice(probePath("").length)));
  assert.deepEqual([...actions].sort(), Object.values(PERF_PROBE_ACTIONS).sort());
  // install first, stop last, and the soak's reset/sample once per level.
  assert.equal(driven[0].path, probePath("install"));
  assert.equal(driven.at(-1).path, probePath("stop"));
  for (const a of ["reset", "sample", "transport-reset", "transport-sample"]) {
    assert.equal(driven.filter((c) => c.path === probePath(a)).length, 2, `${a} once per level`);
  }
  for (const level of run.levels) {
    assert.equal(level.frontendError, null);
    assert.equal(level.frontend.frameCount, 4);
    assert.equal(level.frontend.longTasks.count, 1);
    assert.equal(level.frontend.webviewOutputEvents, 12);
    assert.equal(level.transport.frontendError, null);
  }
});

test("an absent perf-probe component reads unknown with the reason, never zero", ATTACHED, async () => {
  const r = fakeRunner({ perfProbe: false });
  const run = await runHarness(attachedConfig(), quiet, { fetch: r.fetch });
  for (const level of run.levels) {
    assert.equal(level.frontend, null, "no zero-filled frontend sample");
    assert.match(level.frontendError, /perf-probe UI Bridge component is not registered/);
    assert.match(level.frontendError, /Component not found: perf-probe/);
    assert.match(level.transport.frontendError, /perf-probe UI Bridge component is not registered/);
    assert.equal(level.chunks.webviewEventsPerSec, null);
    assert.equal(level.webviewWedged, false);
  }
  assert.ok(run.notes.some((n) => /probe install FAILED/.test(n) && /UNKNOWN, not zero/.test(n)));
  // A probe that never installed is not asked for a window it does not have.
  assert.deepEqual(r.calls.filter((c) => c.path === probePath("reset") || c.path === probePath("sample")), []);
});

test("the harness's perf-probe actions are the ones the bundle registers, from the app shell", () => {
  const lib = readFileSync(join(REPO, "src", "lib", "perfProbe.ts"), "utf8");
  const factory = lib.slice(lib.indexOf("export function buildPerfProbeActions"));
  const registered = [...factory.matchAll(/^\s{6}id: "([a-z-]+)",$/gm)].map((m) => m[1]);
  assert.deepEqual(registered.sort(), Object.values(PERF_PROBE_ACTIONS).sort());
  const hook = readFileSync(join(REPO, "src", "components", "app", "usePerfProbeComponent.ts"), "utf8");
  assert.ok(hook.includes(`id: "${PERF_PROBE_COMPONENT}"`), "the component id the harness addresses");
  assert.match(hook, /actions: buildPerfProbeActions\(perfProbe\)/);
  const app = readFileSync(join(REPO, "src", "App.tsx"), "utf8");
  const content = app.slice(app.indexOf("function AppContent()"));
  const call = content.indexOf("usePerfProbeComponent();");
  assert.ok(call > 0, "AppContent registers the probe");
  assert.ok(call < content.indexOf('presentation.mode === "early-return"'), "before any early return");
});

test("probeAction unwraps the {success, result} envelope and refuses an inner failure", async () => {
  const runner = { componentAction: async (id, action) => ({ success: true, result: { id, action } }) };
  assert.deepEqual(await probeAction(runner, "sample"), { id: PERF_PROBE_COMPONENT, action: "sample" });
  const refused = { componentAction: async () => ({ success: false, error: "boom" }) };
  await assert.rejects(probeAction(refused, "sample"), /perf-probe\.sample failed: boom/);
});

test("--dry-run measures the frontend through the fake's perf-probe component", async () => {
  const run = await runHarness({ ...attachedConfig({ runnerUrl: null }), dryRun: true, sessions: [2] }, quiet);
  const [level] = run.levels;
  assert.equal(level.frontendError, null);
  assert.ok(level.frontend.frameCount > 0);
  assert.ok(level.transport.frontend, "the webview transport half is measured too");
});

// ---------------------------------------------------------------------------
// The perf-probe route waits the runner's UI Bridge IPC budget
// ---------------------------------------------------------------------------

test("a runner the harness spawns gets the probe's IPC budget, beside RUST_LOG", ATTACHED, async () => {
  const r = fakeRunner();
  const run = await runHarness(attachedConfig({ runnerUrl: null, sessions: [2] }), quiet, { fetch: r.fetch });
  const spawn = r.calls.find((c) => c.port === "9875" && c.path === "/runners/spawn-test");
  assert.ok(spawn, "the harness spawned through the supervisor");
  assert.equal(spawn.body.extra_env.QONTINUI_TIMEOUT_UI_BRIDGE_IPC, "60");
  assert.equal(spawn.body.extra_env.RUST_LOG, "info");
  assert.ok(r.calls.some((c) => c.port === "9875" && c.path === "/runners/test-9881/stop"), "and stopped it");
  assert.equal(run.levels[0].created, 1, "topped up to 2 over the one pre-existing terminal");
});

test("an attached run says its IPC budget is the runner's own", ATTACHED, async () => {
  const r = fakeRunner();
  const run = await runHarness(attachedConfig({ sessions: [2] }), quiet, { fetch: r.fetch });
  assert.ok(run.notes.some((n) => n.includes("QONTINUI_TIMEOUT_UI_BRIDGE_IPC")));
});

test("a timed-out sample and an open UI Bridge circuit breaker both read as a wedged webview", ATTACHED, async () => {
  for (const error of [
    "UI Bridge request timed out after 10000ms",
    "UI Bridge temporarily unavailable (circuit breaker open)",
  ]) {
    const r = fakeRunner({ sampleError: error });
    const run = await runHarness(attachedConfig({ sessions: [2] }), quiet, { fetch: r.fetch });
    assert.equal(run.levels[0].frontend, null, error);
    assert.equal(run.levels[0].webviewWedged, true, error);
  }
  const ok = await runHarness(attachedConfig({ sessions: [2] }), quiet, { fetch: fakeRunner({ sampleError: "something else" }).fetch });
  assert.equal(ok.levels[0].webviewWedged, false, "any other failure is not a wedge");
});

/**
 * Adapter command construction (`adapters/*.mjs`): the exact argv each adapter
 * hands to tmux, xterm, the runner, the supervisor and the UI Bridge — every one
 * an argv array scoped to what the run owns — plus the runner's spawn admission
 * (decline, never "Start anyway"), its 24-per-call chunking, zone resolution and
 * the input arm's pane_not_mounted path.
 *
 *   node --test scripts/termhost-bench/__tests__/adapters.test.mjs
 */

import { test } from "node:test";
import assert from "node:assert/strict";
import { EventEmitter } from "node:events";
import { appendFileSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { HttpError } from "../../lib/runner-clients.mjs";
import { OPTIONAL_METHODS, REQUIRED_METHODS, assertAdapter, createAdapter, rowMajorOrder } from "../adapters/index.mjs";
import {
  DECLINE_LABELS,
  MAX_SPAWN_COUNT,
  NEVER_CLICK,
  RunnerAdapter,
  checkFixCommits,
  judgeSpawnOutcome,
  jwtPayload,
  parseSpawnSpans,
  refusalLane,
  spawnTestBody,
  subjectEnv,
} from "../adapters/runner.mjs";
import { TILED_MAX_PANES, TmuxAdapter, cleanEnv, sessionShellArgv, tmuxConf, xtermArgv } from "../adapters/tmux.mjs";
import { generatorArgv } from "../rundir.mjs";

const ARGV = generatorArgv({ node: "/usr/bin/node", genDir: "/tmp/tb/run/units/u000/gen", profile: "claude-tui", seed: 1, genRunId: "run-1-u0" });

function dirs(t) {
  const runDir = mkdtempSync(join(tmpdir(), "tb-adapter-"));
  t.after(() => rmSync(runDir, { recursive: true, force: true }));
  const unitDir = join(runDir, "units", "u000");
  mkdirSync(join(unitDir, "gen"), { recursive: true });
  return { runDir, unitDir, genDir: join(unitDir, "gen") };
}

// ---------------------------------------------------------------------------
// The interface
// ---------------------------------------------------------------------------

test("both Phase 3 adapters implement every interface method, and the optional extensions", async () => {
  const tmux = await createAdapter("tmux", { platform: "linux" });
  const runner = await createAdapter("runner", { mode: "subject", build: { role: "single", commit: null, exe: null }, repoRoot: "." });
  for (const a of [tmux, runner]) {
    for (const m of [...REQUIRED_METHODS, ...OPTIONAL_METHODS]) assert.equal(typeof a[m], "function", `${a.id}.${m}`);
    assert.equal(typeof a.hostIdKey, "string");
  }
  assert.throws(() => assertAdapter({ id: "x", check() {} }), /lacks launch/);
  await assert.rejects(createAdapter("vscode", {}), /Phase 4/);
  await assert.rejects(createAdapter("windows-terminal", {}), /Phase 5/);
});

test("row-major order clusters markers by row, then column", () => {
  const sessions = new Map([
    [7, { x: 700, y: 41, h: 16 }],
    [3, { x: 10, y: 40, h: 16 }],
    [9, { x: 10, y: 400, h: 16 }],
    [1, { x: 1400, y: 39, h: 16 }],
  ]);
  const { order, rows } = rowMajorOrder(sessions);
  assert.deepEqual(order, [3, 7, 1, 9]);
  assert.deepEqual(rows, [[3, 7, 1], [9]]);
});

// ---------------------------------------------------------------------------
// tmux
// ---------------------------------------------------------------------------

/** A scripted tmux: panes in their own windows, ids handed out in order. */
function tmuxHarness() {
  const calls = [];
  const spawns = [];
  let pane = 0;
  const panes = [];
  const run = (cmd, args) => {
    calls.push([cmd, ...args]);
    const out = (stdout) => ({ status: 0, stdout, stderr: "" });
    if (args[0] === "-V") return out("tmux 3.2a\n");
    if (args[0] === "-version") return out("XTerm(372)\n");
    const sub = args.find((a) => ["new-session", "new-window", "split-window", "display-message", "list-clients", "list-panes"].includes(a));
    if (sub === "new-session" || sub === "new-window" || sub === "split-window") {
      const id = `%${pane}`;
      panes.push({ id, window: `@${pane}` });
      pane += 1;
      return out(`${id}\n`);
    }
    if (sub === "display-message") {
      const fmt = args[args.length - 1];
      if (fmt === "#{pid}") return out("4242\n");
      const target = args[args.indexOf("-t") + 1];
      return out(`${panes.find((p) => p.id === target)?.window ?? "@0"}\n`);
    }
    if (sub === "list-clients") return out("7777\n");
    if (sub === "list-panes") return out(panes.map((p, i) => `${p.id}\t${p.window}\t${i}\t80\t24\t${i === 0 ? 1 : 0}`).join("\n"));
    return out("");
  };
  const spawnFn = (cmd, args) => {
    spawns.push([cmd, ...args]);
    const child = new EventEmitter();
    child.pid = 9999;
    child.kill = () => child.emit("exit", 0, null);
    return child;
  };
  return { calls, spawns, run, spawnFn };
}

test("the private tmux config: no status line, generous history, 256 colours and RGB", () => {
  const conf = tmuxConf({ version: "3.2a" });
  assert.match(conf, /^set -g status off$/m);
  assert.match(conf, /^set -g history-limit 50000$/m);
  assert.match(conf, /^set -g default-terminal "screen-256color"$/m);
  assert.match(conf, /terminal-features ",xterm\*:RGB:256"/);
  assert.match(conf, /terminal-overrides ",xterm\*:Tc"/);
  assert.doesNotMatch(tmuxConf({ version: "3.0" }), /terminal-features/, "3.0 has no terminal-features");
});

test("sessions are the interactive shell with the generator as its child, as an argv", () => {
  const argv = sessionShellArgv("/bin/bash", ["node", "/a b/gen.mjs", "--seed", "1"]);
  assert.deepEqual(argv.slice(0, 3), ["/bin/bash", "-i", "-c"]);
  assert.equal(argv[3], "node '/a b/gen.mjs' --seed 1; exec /bin/bash -i");
  assert.deepEqual(cleanEnv({ TMUX: "/tmp/x", TMUX_PANE: "%1", PATH: "/bin" }, { DISPLAY: ":99" }), { PATH: "/bin", DISPLAY: ":99" });
});

test("xterm hosts the private server full-screen, attached by argv (no shell)", () => {
  const argv = xtermArgv({ socket: "termhost-r-u0", title: "termhost-r-u0", cols: 236, rows: 62 });
  assert.equal(argv[0], "xterm");
  assert.deepEqual(argv.slice(-7), ["-e", "tmux", "-L", "termhost-r-u0", "attach-session", "-t", "bench"]);
  assert.ok(argv.includes("+sb"));
  assert.equal(argv[argv.indexOf("-geometry") + 1], "236x62+0+0");
  assert.equal(argv[argv.indexOf("-tn") + 1], "xterm-256color");
});

test("tmux launch, spawn, arrange, show, spawnOne and closeOne name the private socket every time", async (t) => {
  const { runDir, unitDir, genDir } = dirs(t);
  const h = tmuxHarness();
  const a = new TmuxAdapter({ platform: "linux", run: h.run, spawnFn: h.spawnFn, shell: "/bin/bash", env: { PATH: "/bin", TMUX: "/tmp/tmux-0/default,1,0" } });
  await a.launch({ runDir, unitDir, genDir, genRunId: "run-1-u0", env: { env: { DISPLAY: ":99" }, screen: { width: 1920, height: 1080 } } });
  const conf = join(unitDir, "tmux.conf");
  assert.ok(existsSync(conf), "the config is written inside the unit directory");
  assert.match(readFileSync(conf, "utf8"), /status off/);
  const spawned = await a.spawnSessions(3, ARGV);
  assert.deepEqual(spawned.sessions.map((s) => s.hostId), ["%0", "%1", "%2"]);
  const first = h.calls.find((c) => c.includes("new-session"));
  assert.deepEqual(first.slice(0, 5), ["tmux", "-L", "termhost-run-1-u0", "-f", conf]);
  const dd = first.indexOf("--");
  assert.deepEqual(first.slice(dd + 1, dd + 4), ["/bin/bash", "-i", "-c"]);
  assert.match(first[dd + 4], /--termhost-bench-generator run-1-u0; exec \/bin\/bash -i$/);
  assert.equal(h.calls.filter((c) => c.includes("new-window")).length, 2);
  assert.equal(h.spawns.length, 1, "one xterm");
  assert.deepEqual(h.spawns[0].slice(-7), ["-e", "tmux", "-L", "termhost-run-1-u0", "attach-session", "-t", "bench"]);
  assert.deepEqual(a.processRoots(), [4242, 9999], "the server and the xterm");

  const slots = new Map([[0, "%0"], [1, "%1"], [2, "%2"]]);
  const tiled = await a.arrange("tiled", { slots });
  assert.equal(tiled.vTarget, 3);
  assert.ok(h.calls.some((c) => c.includes("join-pane") && c.includes("%1") && c.includes("%0")));
  assert.ok(h.calls.some((c) => c.includes("select-layout") && c.includes("tiled")));
  const focus = await a.arrange("focus", { slots });
  assert.equal(focus.vTarget, 1);
  assert.equal(focus.focusSession, 0);
  const sw = await a.show(2);
  assert.equal(sw.ok, true);
  assert.equal(typeof sw.tCmdNs, "bigint");
  assert.deepEqual(h.calls.at(-1), ["tmux", "-L", "termhost-run-1-u0", "select-window", "-t", "@2"]);
  const one = await a.spawnOne();
  assert.ok(h.calls.at(-1).includes("new-window") && !h.calls.at(-1).includes("-d"), "focus: the new window becomes current");
  await a.closeOne(one.hostId);
  assert.deepEqual(h.calls.at(-1), ["tmux", "-L", "termhost-run-1-u0", "kill-pane", "-t", one.hostId]);
  // Every tmux call except the version query names the private socket.
  for (const c of h.calls.filter((x) => x[0] === "tmux" && x[1] !== "-V")) assert.deepEqual(c.slice(1, 3), ["-L", "termhost-run-1-u0"], c.join(" "));
  // TMUX from the harness's own environment never reaches the private server.
  const d = a.describe();
  assert.equal(d.launch_mode, "private_server");
  assert.equal(d.pty_host_impl, "unix_pty");
  assert.match(d.app_version, /tmux 3\.2a in XTerm\(372\)/);
  assert.match(d.settings_digest, /^sha256:[0-9a-f]{64}$/);
});

test("tmux tiled arm puts at most nine panes in a window", async (t) => {
  const { runDir, unitDir, genDir } = dirs(t);
  const h = tmuxHarness();
  const a = new TmuxAdapter({ platform: "linux", run: h.run, spawnFn: h.spawnFn, env: { PATH: "/bin" } });
  await a.launch({ runDir, unitDir, genDir, genRunId: "run-1-u1", env: { env: {}, screen: { width: 1920, height: 1080 } } });
  await a.spawnSessions(15, ARGV);
  const slots = new Map([...Array(15).keys()].map((k) => [k, `%${k}`]));
  const r = await a.arrange("tiled", { slots });
  assert.equal(r.vTarget, TILED_MAX_PANES);
  const joins = h.calls.filter((c) => c.includes("join-pane"));
  assert.equal(joins.length, 8 + 5, "nine panes in window one (8 joins), six in window two (5 joins)");
  assert.ok(joins.every((c) => c.at(-1) === "%0" || c.at(-1) === "%9"));
});

test("tmux refuses to launch off Linux (the Windows variant is tmux-wsl)", async (t) => {
  const { runDir, unitDir, genDir } = dirs(t);
  const a = new TmuxAdapter({ platform: "win32" });
  await assert.rejects(a.launch({ runDir, unitDir, genDir, genRunId: "x", env: { env: {} } }), /runs on Linux/);
});

// ---------------------------------------------------------------------------
// runner: launch construction
// ---------------------------------------------------------------------------

test("the subject environment is the D12 contract and nothing that points outside the root", () => {
  const env = subjectEnv({
    base: {
      PATH: "/bin",
      QONTINUI_CONFIG_DIR: "/home/u/.config/x",
      QONTINUI_EMBEDDED_PG_DIR: "/shared/pg",
      QONTINUI_HOME: "/home/u/.qontinui",
      WEBVIEW2_USER_DATA_FOLDER: "C:/x",
      QONTINUI_TERMINAL_ID: "parent-terminal",
      CLAUDECODE: "1",
      QONTINUI_PORT: "9876",
    },
    root: "/tmp/tb/run/units/u0/runner-root",
    port: 9881,
    instanceName: "termhost-run-1-u0",
    platform: "linux",
    display: ":99",
    extra: { WEBKIT_DISABLE_DMABUF_RENDERER: "1" },
  });
  assert.equal(env.QONTINUI_INSTANCE_ROOT, "/tmp/tb/run/units/u0/runner-root");
  assert.equal(env.QONTINUI_PORT, "9881");
  assert.equal(env.QONTINUI_INSTANCE_NAME, "termhost-run-1-u0");
  assert.equal(env.QONTINUI_ALLOW_NO_DB, "1");
  assert.equal(env.QONTINUI_SETUP_WIZARD_BYPASS, "1");
  assert.equal(env.DISPLAY, ":99");
  assert.match(env.XDG_DATA_HOME.replace(/\\/g, "/"), /runner-root\/xdg\/data$/);
  assert.equal(env.WEBKIT_DISABLE_DMABUF_RENDERER, "1");
  for (const k of ["QONTINUI_CONFIG_DIR", "QONTINUI_EMBEDDED_PG_DIR", "QONTINUI_HOME", "WEBVIEW2_USER_DATA_FOLDER", "QONTINUI_TERMINAL_ID", "CLAUDECODE"]) {
    assert.equal(env[k], undefined, `${k} must not leak into the subject`);
  }
});

test("the spawn-test body: rebuild, strict frontend, async, exactly one selector, DISPLAY on Linux", () => {
  const b = spawnTestBody({ gitRef: "b".repeat(40), requesterId: "termhost-bench-r", display: ":99", platform: "linux" });
  assert.deepEqual(b, { rebuild: true, frontend_strict: true, async: true, wait: false, requester_id: "termhost-bench-r", git_ref: "b".repeat(40), extra_env: { DISPLAY: ":99" } });
  const w = spawnTestBody({ worktreePath: "/w/qontinui-runner", requesterId: "r", platform: "win32" });
  assert.equal(w.worktree_path, "/w/qontinui-runner");
  assert.equal(w.extra_env, undefined);
  assert.equal(w.from_working_tree, undefined, "never the supervisor's live tree");
  assert.throws(() => spawnTestBody({ requesterId: "r" }), /exactly one/);
  assert.throws(() => spawnTestBody({ gitRef: "x", worktreePath: "/w", requesterId: "r" }), /exactly one/);
});

test("a spawn-test outcome is judged by source and sha (D12)", () => {
  const sha = "c".repeat(40);
  const ok = judgeSpawnOutcome({ httpStatus: 200, body: { source: "worktree", build_sha: sha, port: 9880 } }, { gitRef: sha, expectedCommit: sha });
  assert.equal(ok.ok, true);
  assert.equal(ok.buildSha, sha);
  assert.equal(ok.port, 9880);
  assert.equal(judgeSpawnOutcome({ httpStatus: 503, body: { error: "frontend_stale" } }, { gitRef: sha }).ok, false);
  assert.match(judgeSpawnOutcome({ httpStatus: 200, body: { source: "live_tree", build_sha: sha } }, { gitRef: sha }).detail, /expected "worktree"/);
  assert.match(judgeSpawnOutcome({ httpStatus: 200, body: { source: "worktree", build_sha: "d".repeat(40) } }, { gitRef: sha, expectedCommit: sha }).detail, /not the requested/);
  const unknown = judgeSpawnOutcome({ httpStatus: 200, body: { source: "worktree_path" } }, { worktreePath: "/w" });
  assert.equal(unknown.ok, true);
  assert.equal(unknown.buildSha, null, "an absent sha is an unknown build, never a baseline");
});

test("the #1756 fix check: ancestor, not ancestor, unknown", () => {
  const run = (status) => () => ({ status, stdout: "", stderr: "" });
  assert.equal(checkFixCommits("e".repeat(40), { repoRoot: ".", run: run(0) }).state, "OPERATIVE");
  assert.equal(checkFixCommits("e".repeat(40), { repoRoot: ".", run: run(1) }).state, "INOPERATIVE-ON-THIS-MACHINE");
  assert.equal(checkFixCommits("e".repeat(40), { repoRoot: ".", run: run(128) }).state, "UNKNOWN");
  assert.equal(checkFixCommits(null, { repoRoot: "." }).state, "UNKNOWN");
});

test("runner variants that change a setting run only in subject mode", () => {
  assert.throws(() => new RunnerAdapter({ mode: "supervisor", variant: "ghostty", build: { role: "single" }, repoRoot: "." }), /only in subject mode/);
  assert.throws(() => new RunnerAdapter({ mode: "primary", build: {}, repoRoot: "." }), /subject or supervisor/);
});

test("log parsing: spawn spans and refusal lanes", () => {
  const spans = parseSpawnSpans(
    "2026-10-12 01:00:00.000  INFO terminal_spawn.identity_seam{terminal_id=a}: q: close time.busy=12.5ms time.idle=3µs\n" +
      "x terminal_spawn.manager_create{}: close time.busy=800µs time.idle=1ms\nunrelated line\n",
  );
  assert.deepEqual(spans.get("identity_seam"), [12.5]);
  assert.deepEqual(spans.get("manager_create"), [0.8]);
  assert.equal(refusalLane('WARN resource_guard: refusing to spawn past the session critical limit lane="x" metric=thread_count observed=401'), "thread_count");
  assert.equal(refusalLane("resource_guard:critical:free_commit_bytes: Not starting"), "free_commit_bytes");
  assert.equal(refusalLane("something else"), "unknown");
  const payload = Buffer.from(JSON.stringify({ exp: 1, device_id: "d" })).toString("base64url");
  assert.deepEqual(jwtPayload(`h.${payload}.s`), { exp: 1, device_id: "d" });
  assert.equal(jwtPayload("garbage"), null);
});

// ---------------------------------------------------------------------------
// runner: driving the Terminal page (fake UI Bridge client)
// ---------------------------------------------------------------------------

/** A fake RunnerClient over an in-memory terminal list. */
function fakeClient(over = {}) {
  const terminals = [];
  const calls = [];
  let created = 0;
  const client = {
    calls,
    terminals,
    async listTerminals() {
      return terminals.map((id, i) => ({ id, createdAt: i, cols: 80, rows: 24 }));
    },
    async componentAction(component, action, params) {
      calls.push(["component", component, action, params]);
      if (over.componentAction) {
        const r = await over.componentAction(component, action, params, { terminals, create: () => terminals.push(`t${created++}`) });
        if (r !== undefined) return r;
      }
      if (action === "create-with-command") {
        for (let i = 0; i < params.count; i += 1) terminals.push(`t${created++}`);
        return { success: true, tab_ids: terminals.slice(-params.count) };
      }
      if (action === "get-zone-view-state") return { maximizedZone: null, zoneCount: terminals.length, layoutId: "quad" };
      return {};
    },
    async elementAction(id, action) {
      calls.push(["element", id, action]);
      if (over.elementAction) return over.elementAction(id, action);
      return {};
    },
    async getElement(id) {
      if (over.getElement) return over.getElement(id);
      throw new HttpError("404", { status: 404, body: {}, method: "GET", url: id });
    },
    async discover() {
      return over.discover ? over.discover() : { elements: [] };
    },
    async health() {
      return { threadCeilings: { warn: 256, critical: 400 } };
    },
    async coordSession(id) {
      return { terminal_id: id, coord_session_id: null };
    },
  };
  return client;
}

function subjectAdapter(t, client, over = {}) {
  const a = new RunnerAdapter({ mode: "subject", build: { role: "single", commit: "f".repeat(40), exe: "x" }, repoRoot: ".", platform: "linux", ...over });
  a.client = client;
  return a;
}

test("spawnSessions creates sessions through create-with-command, at most 24 per call (40 = 24 + 16)", async (t) => {
  const client = fakeClient();
  const a = subjectAdapter(t, client);
  const res = await a.spawnSessions(40, ARGV);
  const counts = client.calls.filter((c) => c[2] === "create-with-command").map((c) => c[3].count);
  assert.deepEqual(counts, [MAX_SPAWN_COUNT, 16]);
  assert.equal(res.sessions.length, 40);
  assert.equal(res.admission, null);
  const cmd = client.calls.find((c) => c[2] === "create-with-command")[3].command;
  // The script path is quoted when it needs it (a Windows checkout path does; a CI path does not).
  assert.match(cmd, /^\/usr\/bin\/node '?[^ ]*ai-session\.mjs'? --run-dir \/tmp\/tb\/run\/units\/u000\/gen --profile claude-tui --seed 1 --echo --termhost-bench-generator run-1-u0$/);
  assert.equal(client.calls[0][1], "terminal-launch-menu", "the launch menu's own action — never POST /terminals");
});

test("a resource-guard refusal is declined — never 'Start anyway' — and recorded as the admission limit", async (t) => {
  const dir = mkdtempSync(join(tmpdir(), "tb-guard-"));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  const log = join(dir, "qontinui-runner.log.2026-10-12");
  writeFileSync(log, "boot\n");
  let decline;
  const declined = new Promise((r) => {
    decline = r;
  });
  const client = fakeClient({
    componentAction: async (component, action, params, ctx) => {
      if (action !== "create-with-command") return undefined;
      ctx.create();
      ctx.create();
      // The third spawn is refused: the runner logs it and its dialog holds the action open.
      appendFileSync(log, 'WARN resource_guard: refusing to spawn past the session critical limit metric=thread_count observed=401 limit=400\n');
      // A runner whose dialog is never declined would hold the action open for good:
      // bound the wait so a regression fails here instead of hanging the suite.
      await Promise.race([declined, new Promise((_, reject) => setTimeout(() => reject(new Error("the guard dialog was never declined")), 5000))]);
      throw new HttpError("POST create-with-command -> 400", { status: 400, body: { error: "spawned 2 of 5 terminals" }, method: "POST", url: "x" });
    },
    discover: () => ({
      elements: [
        { id: "btn-confirm", label: "Start anyway" },
        { id: "btn-decline", label: DECLINE_LABELS[0] },
        { id: "msg", text: "Not starting a new terminal: thread count 401 is past 400." },
      ],
    }),
    elementAction: (id) => {
      if (NEVER_CLICK.test(id === "btn-confirm" ? "Start anyway" : "")) throw new Error("clicked Start anyway");
      if (id === "btn-decline") decline();
      return {};
    },
  });
  const a = subjectAdapter(t, client);
  a.logFile = log;
  const res = await a.spawnSessions(5, ARGV);
  const clicks = client.calls.filter((c) => c[0] === "element").map((c) => c[1]);
  assert.deepEqual(clicks, ["btn-decline"]);
  assert.equal(res.sessions.length, 2);
  assert.equal(res.admission.refused, true);
  assert.equal(res.admission.lane, "thread_count");
  assert.equal(res.admission.n_reached, 2);
  assert.deepEqual(res.admission.threadCeilings, { warn: 256, critical: 400 });
  assert.match(res.admission.detail, /Not starting a new terminal/);
});

test("focus resolves the session's zone from the probe's markers and verifies the maximize", async (t) => {
  const client = fakeClient();
  const a = subjectAdapter(t, client);
  client.terminals.push("t0", "t1", "t2");
  const looks = [
    new Map([[2, { x: 10, y: 40, w: 160, h: 16 }], [0, { x: 700, y: 40, w: 160, h: 16 }], [1, { x: 10, y: 500, w: 160, h: 16 }]]),
    new Map([[2, { x: 10, y: 40, w: 160, h: 16 }]]),
  ];
  let i = 0;
  const look = async () => ({ ok: true, sessions: looks[Math.min(i++, looks.length - 1)], detail: "fake" });
  const r = await a.arrange("focus", { slots: new Map([[0, "t0"], [1, "t1"], [2, "t2"]]), n: 3, look });
  assert.equal(r.ok, true);
  assert.equal(r.focusSession, 2, "the top-left session");
  const max = client.calls.find((c) => c[2] === "maximize-zone");
  assert.deepEqual(max[3], { zoneIndex: 0 });
  assert.deepEqual(r.switchable, [2, 0, 1]);
  assert.equal(r.layout.zoneMapping, "row_major_rank");
  // show(k): restore, then maximize k's zone.
  await a.show(1);
  const last = client.calls.slice(-2).map((c) => [c[2], c[3]]);
  assert.deepEqual(last, [["restore-zone", {}], ["maximize-zone", { zoneIndex: 2 }]]);
});

test("a maximize that leaves two sessions visible is unknown(zone_unresolved)", async (t) => {
  const client = fakeClient();
  const a = subjectAdapter(t, client);
  const two = new Map([[0, { x: 10, y: 40, w: 160, h: 16 }], [1, { x: 700, y: 40, w: 160, h: 16 }]]);
  const r = await a.arrange("focus", { slots: new Map([[0, "t0"], [1, "t1"]]), n: 2, look: async () => ({ ok: true, sessions: two, detail: "fake" }) });
  assert.equal(r.ok, false);
  assert.equal(r.failure.reason, "zone_unresolved");
});

test("zone headers that line up with the markers map sessions to their real zone indices", async (t) => {
  const boxes = new Map([[4, { x: 0, y: 30 }], [5, { x: 640, y: 30 }]]);
  const client = fakeClient({
    getElement: (id) => {
      const i = Number(id.split("-").pop());
      if (!boxes.has(i)) throw new HttpError("404", { status: 404, body: {}, method: "GET", url: id });
      return { visible: true, bbox: { ...boxes.get(i), width: 600, height: 22 } };
    },
  });
  client.terminals.push("t0", "t1", "t2", "t3", "t4", "t5");
  const a = subjectAdapter(t, client);
  const seen = new Map([[3, { x: 10, y: 60, w: 160, h: 16 }], [8, { x: 650, y: 60, w: 160, h: 16 }]]);
  const z = await a.resolveZones(seen, 6);
  assert.equal(z.method, "header_boxes");
  assert.equal(z.zoneOf.get(3), 4);
  assert.equal(z.zoneOf.get(8), 5);
});

test("an unmounted pane makes the input arm unknown(pane_not_mounted)", async (t) => {
  const client = fakeClient({
    elementAction: () => {
      throw new HttpError("POST /ui-bridge/control/element/terminal-input-t0/action -> 400", {
        status: 400,
        body: { success: false, error: "TERMINAL_NO_MOUNTED_VIEW: 'focus' needs a mounted terminal view" },
        method: "POST",
        url: "x",
      });
    },
  });
  const a = subjectAdapter(t, client);
  a.slots = new Map([[0, "t0"]]);
  a.focusSession = 0;
  const r = await a.focusObserved();
  assert.equal(r.ok, false);
  assert.equal(r.failure.reason, "pane_not_mounted");
  assert.deepEqual(client.calls.at(-1), ["element", "terminal-input-t0", "focus"]);
});

test("closeOne deletes exactly the terminal it is given", async (t) => {
  const deleted = [];
  const client = { ...fakeClient(), closeTerminal: async (id) => deleted.push(id) };
  const a = subjectAdapter(t, client);
  a.spawned = ["t9"];
  a.terminals = ["t1", "t9"];
  await a.closeOne();
  assert.deepEqual(deleted, ["t9"]);
  assert.deepEqual(a.terminals, ["t1"]);
});

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
import net from "node:net";
import { appendFileSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { HttpError, RunnerClient, SupervisorClient, WaitAborted } from "../../lib/runner-clients.mjs";
import { waitForGenerators, waitForNewSlot } from "../sessions.mjs";
import { OPTIONAL_METHODS, REQUIRED_METHODS, assertAdapter, createAdapter, rowMajorOrder } from "../adapters/index.mjs";
import {
  DECLINE_LABELS,
  isPortListening,
  parseGuardWarnings,
  shaMatches,
  subjectLaunchVerdict,
  summarizeGuardWarnings,
  supervisorAnswers,
  MAX_SPAWN_COUNT,
  NEVER_CLICK,
  RunnerAdapter,
  checkFixCommits,
  judgeSpawnOutcome,
  jwtPayload,
  parseSpawnSpans,
  refusalLane,
  spawnTestBody,
  subjectContractProblems,
  actionResult,
  subjectEnv,
  ROOT_CONFINED_ENV_KEYS,
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
  // Beside the root, never inside it: the runner refuses a root that contains its XDG dirs.
  assert.equal(env.XDG_DATA_HOME.replace(/\\/g, "/"), "/tmp/tb/run/units/u0/runner-xdg/data");
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

test("a spawn-test outcome is judged by the RUNNING child's sha, not the tree it was asked to build (D12)", () => {
  const sha = "c".repeat(40);
  // git_sha is what the supervisor probed from the child's own /health (12 chars).
  const ok = judgeSpawnOutcome({ httpStatus: 200, body: { source: "worktree", git_sha: sha.slice(0, 12), port: 9880 } }, { gitRef: sha, expectedCommit: sha });
  assert.equal(ok.ok, true);
  assert.equal(ok.buildSha, sha, "a matching probed sha proves the requested commit");
  assert.equal(ok.port, 9880);
  assert.equal(judgeSpawnOutcome({ httpStatus: 503, body: { error: "frontend_stale" } }, { gitRef: sha }).ok, false);
  assert.match(judgeSpawnOutcome({ httpStatus: 200, body: { source: "live_tree", git_sha: sha } }, { gitRef: sha }).detail, /expected "worktree"/);
  const other = judgeSpawnOutcome({ httpStatus: 200, body: { source: "worktree", git_sha: "d".repeat(12) } }, { gitRef: sha, expectedCommit: sha });
  assert.equal(other.ok, false);
  assert.match(other.detail, /reports git_sha d{12}, not the requested/);
  const lkg = judgeSpawnOutcome({ httpStatus: 200, body: { source: "worktree", git_sha: sha.slice(0, 12), used_lkg: true } }, { gitRef: sha, expectedCommit: sha });
  assert.equal(lkg.ok, false, "a last-known-good fallback is not the requested build");
  assert.match(lkg.detail, /used_lkg/);
  // The tree's HEAD is not the binary: git_ref_resolved_sha alone proves nothing.
  const treeOnly = judgeSpawnOutcome({ httpStatus: 200, body: { source: "worktree", git_ref_resolved_sha: sha, build_sha: sha } }, { gitRef: sha, expectedCommit: sha });
  assert.equal(treeOnly.ok, true);
  assert.equal(treeOnly.buildSha, null, "no probed sha: an unknown build, never a baseline");
  assert.equal(shaMatches("cccccccccccc", sha), true);
  assert.equal(shaMatches("unknown", sha), false);
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
      // The real route's shape: the SDK's action envelope, the handler's value in `result`.
      if (action === "get-zone-view-state") return { success: true, result: { maximizedZone: null, zoneCount: terminals.length, layoutId: "quad" }, durationMs: 1 };
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
  // show(k): ONE maximize-zone on k's zone (no restore that would repaint the grid first).
  const before = client.calls.length;
  await a.show(1);
  assert.deepEqual(client.calls.slice(before).map((c) => [c[2], c[3]]), [["maximize-zone", { zoneIndex: 2 }]]);
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

// ---------------------------------------------------------------------------
// Review round 1
// ---------------------------------------------------------------------------

test("a spawn is timed from immediately before the action, after the harness's own GET /terminals", async (t) => {
  const marks = [];
  const client = fakeClient();
  const list = client.listTerminals;
  client.listTerminals = async () => {
    marks.push(["list", process.hrtime.bigint()]);
    return list();
  };
  const action = client.componentAction;
  client.componentAction = async (...args) => {
    marks.push(["action", process.hrtime.bigint()]);
    return action(...args);
  };
  const a = subjectAdapter(t, client);
  a.command = "gen";
  const r = await a.spawnOne();
  const firstList = marks.find((m) => m[0] === "list")[1];
  const act = marks.find((m) => m[0] === "action")[1];
  assert.ok(r.tCmdNs > firstList, "the harness's GET /terminals is not part of the spawn");
  assert.ok(r.tCmdNs <= act, "the start is taken before the action is sent");
});

test("a subject launch needs a definite 'nothing there' from both probes; an unknown is never absent (D12)", () => {
  assert.equal(subjectLaunchVerdict(false, false).state, "OPERATIVE");
  assert.equal(subjectLaunchVerdict(true, false).state, "INOPERATIVE-ON-THIS-MACHINE");
  assert.equal(subjectLaunchVerdict(false, true).state, "INOPERATIVE-ON-THIS-MACHINE");
  assert.equal(subjectLaunchVerdict(null, false).state, "UNKNOWN");
  assert.equal(subjectLaunchVerdict(false, null).state, "UNKNOWN");
  assert.match(subjectLaunchVerdict(null, false).reason, /could not be told in time/);
});

test("isPortListening answers accepted, refused, or unknown — a timeout is not 'not listening'", async () => {
  const srv = net.createServer((c) => c.destroy());
  await new Promise((r) => srv.listen(0, "127.0.0.1", r));
  const port = srv.address().port;
  assert.equal(await isPortListening(port), true);
  await new Promise((r) => srv.close(r));
  assert.equal(await isPortListening(port), false, "a closed port refuses");
  // A connection that neither connects nor errors before the timeout.
  const hanging = () => {
    const s = new EventEmitter();
    s.setTimeout = (ms, cb) => setTimeout(cb, ms);
    s.destroy = () => {};
    return s;
  };
  assert.equal(await isPortListening(1, { timeoutMs: 20, connect: hanging }), null);
  const odd = () => {
    const s = new EventEmitter();
    s.setTimeout = () => {};
    s.destroy = () => {};
    setImmediate(() => s.emit("error", Object.assign(new Error("unreachable"), { code: "EHOSTUNREACH" })));
    return s;
  };
  assert.equal(await isPortListening(1, { connect: odd }), null, "only ECONNREFUSED means nothing listens");
});

test("supervisorAnswers: any HTTP answer is a supervisor, a refusal is none, a timeout is unknown", async () => {
  const answer = (status) => async () => ({ ok: status < 300, status, text: async () => "{}" });
  assert.equal(await supervisorAnswers("http://127.0.0.1:9875", { fetch: answer(200) }), true);
  assert.equal(await supervisorAnswers("http://127.0.0.1:9875", { fetch: answer(503) }), true);
  const refused = async () => {
    throw Object.assign(new TypeError("fetch failed"), { cause: { code: "ECONNREFUSED" } });
  };
  assert.equal(await supervisorAnswers("http://127.0.0.1:9875", { fetch: refused }), false);
  const slow = async (url, init) =>
    new Promise((_, reject) => init.signal.addEventListener("abort", () => reject(Object.assign(new Error("aborted"), { name: "AbortError" }))));
  assert.equal(await supervisorAnswers("http://127.0.0.1:9875", { fetch: slow, timeoutMs: 20 }), null);
});

test("the per-launch drain check passes clear and not_enrolled, and refuses drained, unknown and a boot read still pending", async (t) => {
  const run = async (sequence, waitMs = 5000) => {
    let i = 0;
    let clock = 0;
    const client = { restartReadiness: async () => ({ success: true, data: { drain: { coordDrain: sequence[Math.min(i++, sequence.length - 1)] } } }) };
    const a = subjectAdapter(t, client);
    try {
      await a.checkDrain({ waitMs, sleepFn: async (ms) => { clock += ms; }, now: () => clock });
      return { ok: true, reads: i };
    } catch (e) {
      return { ok: false, reason: e.reason, detail: e.detail, reads: i };
    }
  };
  assert.equal((await run([{ state: "clear" }])).ok, true);
  assert.equal((await run([{ state: "not_enrolled", cause: "no machine.json" }])).ok, true);
  const drained = await run([{ state: "drained", reason: "rebuild" }]);
  assert.deepEqual([drained.ok, drained.reason], [false, "device_drained"]);
  const unknown = await run([{ state: "unknown", cause: "three missed heartbeats" }]);
  assert.deepEqual([unknown.ok, unknown.reason], [false, "device_drained"]);
  const settles = await run([{ state: "unknown", bootReadPending: true }, { state: "unknown", bootReadPending: true }, { state: "clear", bootReadPending: false }]);
  assert.equal(settles.ok, true, "a boot read is waited for");
  assert.equal(settles.reads, 3);
  const pending = await run([{ state: "unknown", bootReadPending: true }], 3000);
  assert.deepEqual([pending.ok, pending.reason], [false, "device_drained"]);
  assert.match(pending.detail, /still pending/);
});

/** A subject adapter whose launch reaches the runner through a fake HTTP layer. */
function subjectLaunch(t, { gitSha, drain = { state: "not_enrolled" } } = {}) {
  const fetch = async (url, init = {}) => {
    const u = new URL(url);
    const json = (body) => ({ ok: true, status: 200, text: async () => JSON.stringify(body) });
    if (u.pathname === "/health") return json({ success: true, data: { uiBridgeIpcObserved: true, gitSha, version: "0.1.0" } });
    if (u.pathname === "/restart-readiness") return json({ success: true, data: { drain: { coordDrain: drain } } });
    return json({ success: true, data: {} });
  };
  const a = new RunnerAdapter({
    mode: "subject",
    build: { role: "single", commit: "f".repeat(40), exe: "x", source: "artifact" },
    repoRoot: ".",
    platform: "linux",
    fetch,
    run: () => ({ status: 0, stdout: "", stderr: "" }),
    list: () => new Map([[4242, { pid: 4242, ppid: 1, name: "qontinui-runner", argv: ["qontinui-runner"], cmdline: "qontinui-runner", startKey: "77" }]]),
  });
  a.launchSubject = async () => {
    a.port = 9881;
    a.pid = 4242;
  };
  return a;
}

test("a subject whose running binary reports another commit is refused; describe() proves a build only by that binary", async (t) => {
  const wrong = subjectLaunch(t, { gitSha: "0123456789ab" });
  await assert.rejects(wrong.launch({ runDir: "/r", unitDir: "/r/u", genDir: "/r/u/gen", genRunId: "r-u0", env: { env: {} } }), (e) => /reports gitSha 0123456789ab, not the build's f{40}/.test(e.detail));
  const right = subjectLaunch(t, { gitSha: "ffffffffffff" });
  await right.launch({ runDir: "/r", unitDir: "/r/u", genDir: "/r/u/gen", genRunId: "r-u1", env: { env: {} } });
  assert.deepEqual(right.describe().build, { commit: "f".repeat(40), source: "artifact", role: "single" });
  assert.equal(right.startKey, "77", "the start key is taken again once the binary answers");
  const blind = subjectLaunch(t, { gitSha: "unknown" });
  await blind.launch({ runDir: "/r", unitDir: "/r/u", genDir: "/r/u/gen", genRunId: "r-u2", env: { env: {} } });
  assert.equal(blind.describe().build.state, "unknown", "a binary that names no sha is an unknown build");
  // Supervisor mode: the requested commit is never taken on trust.
  const sup = new RunnerAdapter({ mode: "supervisor", build: { role: "head", commit: "a".repeat(40), gitRef: "a".repeat(40) }, repoRoot: "." });
  assert.equal(sup.describe().build.state, "unknown");
  sup.buildSha = "a".repeat(40);
  assert.equal(sup.describe().build.commit, "a".repeat(40));
});

/** A fake supervisor HTTP layer; `lost` makes the spawn-test POST lose its answer. */
function fakeSupervisor({ lost = true, inflight = [], runners = [] } = {}) {
  const calls = [];
  const fetch = async (url, init = {}) => {
    const u = new URL(url);
    calls.push(`${init.method ?? "GET"} ${u.pathname}${u.search}`);
    const json = (body, status = 200) => ({ ok: status < 300, status, text: async () => JSON.stringify(body) });
    if (u.pathname === "/health") return json({ ok: true });
    if (u.pathname === "/runners/spawn-test" && init.method === "POST") {
      if (lost) throw new TypeError("fetch failed: socket hang up");
      return json({ submission_id: "s-9", id: "test-9882", port: 9882 }, 202);
    }
    if (u.pathname === "/runners/spawn-test/in-flight") return json({ count: inflight.length, spawns: inflight });
    if (u.pathname.startsWith("/build/")) {
      return json({ status: { state: "succeeded" }, spawn: { http_status: 200, port: 9881, body: { source: "worktree", git_sha: "aaaaaaaaaaaa", port: 9881 } } });
    }
    if (u.pathname === "/runners" && (init.method ?? "GET") === "GET") return json(runners);
    if (/\/runners\/[^/]+\/stop$/.test(u.pathname)) return json({ stopped: true });
    return json({});
  };
  return { fetch, calls };
}

test("a lost spawn-test answer is recovered from the in-flight index; once the build is over, /runners finds the runner for teardown", async () => {
  const requester = "termhost-bench-r-u0";
  const sup = fakeSupervisor({ inflight: [{ requester_id: requester, id: "test-9881", port: 9881, submission_id: "s-1" }], runners: [{ id: "test-9881", pid: 5151, requester_id: requester }] });
  const a = new RunnerAdapter({ mode: "supervisor", build: { role: "single", commit: "a".repeat(40), gitRef: "a".repeat(40) }, repoRoot: ".", platform: "linux", fetch: sup.fetch, list: () => new Map() });
  await a.launchSupervisor({ genRunId: "r-u0", env: { env: {} } });
  assert.equal(a.supervisorRunnerId, "test-9881");
  assert.equal(a.buildSha, "a".repeat(40));
  assert.ok(sup.calls.includes(`GET /runners/spawn-test/in-flight?requester_id=${requester}`));
  assert.ok(sup.calls.includes("GET /build/s-1/status"), "the recovered submission is awaited");
  assert.ok(a.notes.some((n) => /recovered submission s-1/.test(n)));

  // The build already ended: the in-flight key is released, /runners still names it.
  const gone = fakeSupervisor({ inflight: [], runners: [{ id: "test-9890", requester_id: requester, protected: false }] });
  const b = new RunnerAdapter({ mode: "supervisor", build: { role: "single", commit: "a".repeat(40), gitRef: "a".repeat(40) }, repoRoot: ".", platform: "linux", fetch: gone.fetch, list: () => new Map() });
  await assert.rejects(b.launchSupervisor({ genRunId: "r-u0", env: { env: {} } }), (e) => /found by requester id and will be stopped/.test(e.detail));
  assert.equal(b.supervisorRunnerId, "test-9890");
  const td = await b.teardown();
  assert.ok(gone.calls.includes("POST /runners/test-9890/stop"), "teardown stops the runner the lost answer left behind");
  assert.ok(td.closed.includes("temp runner test-9890"));
});

test("teardown stops a supervisor runner only after its row reads back unprotected and ours", async () => {
  const requester = "termhost-bench-r-u3";
  for (const [row, stopped] of [
    [{ id: "test-9883", requester_id: requester }, true],
    [{ id: "test-9883", requester_id: requester, protected: true }, false],
    [{ id: "test-9883", requester_id: "someone-else" }, false],
  ]) {
    const sup = fakeSupervisor({ lost: false, runners: [row] });
    const a = new RunnerAdapter({ mode: "supervisor", build: { role: "single", commit: "a".repeat(40), gitRef: "a".repeat(40) }, repoRoot: ".", fetch: sup.fetch });
    a.supervisor = new (await import("../../lib/runner-clients.mjs")).SupervisorClient("http://127.0.0.1:9875", () => {}, { fetch: sup.fetch });
    a.supervisorRunnerId = "test-9883";
    a.requesterId = requester;
    const td = await a.teardown();
    assert.equal(sup.calls.includes("POST /runners/test-9883/stop"), stopped, JSON.stringify(row));
    if (!stopped) assert.ok(td.survivors.some((x) => /not stopped/.test(x.detail)));
  }
});

test("teardown never walks a tree whose root start key was not captured, nor the PID of a child that already exited", async (t) => {
  const signalled = [];
  const table = new Map([
    [4242, { pid: 4242, ppid: 1, name: "qontinui-runner", argv: ["x"], cmdline: "x", startKey: "9" }],
    [4243, { pid: 4243, ppid: 4242, name: "WebKitWebProcess", argv: ["w"], cmdline: "w", startKey: "10" }],
  ]);
  const noKey = subjectAdapter(t, {}, { list: () => table, kill: (pid, sig) => signalled.push([pid, sig]) });
  noKey.client = null;
  noKey.pid = 4242;
  noKey.startKey = null;
  const r1 = await noKey.teardown();
  assert.deepEqual(signalled, [], "an unproven identity is never signalled");
  assert.ok(r1.survivors.some((x) => /no start key was captured/.test(x.detail)));
  const exited = subjectAdapter(t, {}, { list: () => table, kill: (pid, sig) => signalled.push([pid, sig]) });
  exited.client = null;
  exited.pid = 4242;
  exited.startKey = "9";
  exited.childExit = { code: 78, signal: null };
  const r2 = await exited.teardown();
  assert.deepEqual(signalled, [], "a PID whose child already exited may belong to another process now");
  assert.ok(r2.closed.some((c) => /had already exited/.test(c)));
});

test("pty_holder is what the runner ran: its fallback log lines and this instance's holder processes", (t) => {
  const a = subjectAdapter(t, {});
  a.root = "/tmp/tb/run/units/u0/runner-root";
  const holder = (pid) => ({ pid, ppid: 1, name: "qontinui-pty-holder", argv: ["/x/qontinui-pty-holder"], cmdline: `/x/qontinui-pty-holder --pane-dir ${a.root}/scope/pty-panes/p${pid}`, startKey: "5" });
  const foreign = { pid: 90, ppid: 1, name: "qontinui-pty-holder", argv: ["/x/qontinui-pty-holder"], cmdline: "/x/qontinui-pty-holder --pane-dir /home/u/.local/share/runner/pty-panes/p1", startKey: "5" };
  const fallback = "WARN pty holder: terminal.pty_holder is on but the holder could not be started — spawn refused\n";
  a.holderSetting = false;
  assert.equal(a.ptyHolderAsRun({ sessions: 2, logText: "", table: new Map() }), false);
  a.holderSetting = true;
  assert.equal(a.ptyHolderAsRun({ sessions: 2, logText: "", table: new Map([[1, holder(1)], [2, holder(2)], [90, foreign]]) }), true);
  assert.deepEqual(a.holderPids(new Map([[1, holder(1)], [90, foreign]])), [1], "another instance's holder is not ours");
  // A holder binary run from inside the root for something else (no pane directory) is not a pane holder.
  const selftest = { pid: 91, ppid: 1, name: "qontinui-pty-holder", argv: [`${a.root}/bin/qontinui-pty-holder`], cmdline: `${a.root}/bin/qontinui-pty-holder --selftest`, startKey: "5" };
  assert.deepEqual(a.holderPids(new Map([[1, holder(1)], [91, selftest]])), [1]);
  assert.equal(a.ptyHolderAsRun({ sessions: 2, logText: fallback.repeat(2), table: new Map() }), false, "every pane fell back");
  const mixed = a.ptyHolderAsRun({ sessions: 2, logText: fallback, table: new Map([[1, holder(1)]]) });
  assert.equal(mixed.state, "unknown");
  assert.match(mixed.detail, /1 of 2 pane\(s\) logged a fallback/);
});

test("the resource guard's WARN notices are parsed and kept as a diagnostic, never as a refusal", async (t) => {
  const lines = [
    '2026-10-12T01:00:00Z  WARN qontinui_runner::resource_guard: resource_guard: spawning past the session warn limit lane=session metric="thread_count" observed=270 limit=256 what=terminal',
    "2026-10-12T01:00:01Z  WARN qontinui_runner::resource_guard: resource_guard: spawning past the session warn limit lane=session metric=thread_count observed=290 limit=256 what=terminal",
    "unrelated line",
  ].join("\n");
  const w = parseGuardWarnings(lines);
  assert.deepEqual(w, [
    { metric: "thread_count", lane: "session", observed: 270, limit: 256 },
    { metric: "thread_count", lane: "session", observed: 290, limit: 256 },
  ]);
  assert.deepEqual(summarizeGuardWarnings(w), { count: 2, by_metric: { thread_count: { count: 2, max_observed: 290, limit: 256 } } });
  const dir = mkdtempSync(join(tmpdir(), "tb-warn-"));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  const log = join(dir, "runner.log");
  writeFileSync(log, "boot\n");
  const client = fakeClient({
    componentAction: async (component, action) => {
      if (action === "create-with-command") appendFileSync(log, `${lines}\n`);
      return undefined;
    },
  });
  const a = subjectAdapter(t, client);
  a.logFile = log;
  const res = await a.spawnSessions(2, ARGV);
  assert.equal(res.admission, null, "a WARN notice is not an admission refusal");
  const d = await a.diagnostics();
  assert.equal(d["spawn.admission_warnings"].state, "measured");
  assert.equal(d["spawn.admission_warnings"].data.count, 2);
});

test("the coord read-back waits out a grace period before calling a session left open", async (t) => {
  const payload = Buffer.from(JSON.stringify({ exp: 4102444800 })).toString("base64url");
  const env = { COORD_DEVICE_JWT: `h.${payload}.s` };
  const run = async (answers) => {
    let i = 0;
    let clock = 0;
    const fetch = async () => ({ ok: true, status: 200, json: async () => answers[Math.min(i++, answers.length - 1)] });
    const a = subjectAdapter(t, {}, { env, fetch, sleepFn: async (ms) => { clock += ms; }, now: () => clock });
    a.coordSessions = new Map([["t1", "sess-1"]]);
    return { r: await a.coordReadback({ graceMs: 30000, pollMs: 5000 }), reads: i };
  };
  const closes = await run([{ sessions: [{ sessionId: "sess-1" }] }, { sessions: [] }]);
  assert.deepEqual(closes.r.incidents, [], "closed on the second read: not left open");
  assert.equal(closes.r.diagnostic.data.reads, 2);
  const stays = await run([{ sessions: [{ sessionId: "sess-1" }] }]);
  assert.equal(stays.r.incidents.length, 1);
  assert.equal(stays.r.incidents[0].kind, "coord_session_left_open");
  assert.ok(stays.reads >= 7, `read until the grace ran out (${stays.reads} reads)`);
});

test("an unproven build keeps its role and requested commit, never a proven commit (review round 2 M1)", () => {
  // The review's reproduction (review-p3/describe-role.mjs).
  for (const [role, c] of [["head", "a"], ["baseline", "b"]]) {
    const a = new RunnerAdapter({ mode: "subject", build: { role, commit: c.repeat(40), exe: "x", source: "artifact" }, repoRoot: ".", platform: "linux", list: () => new Map() });
    const b = a.describe().build;
    assert.equal(b.state, "unknown");
    assert.equal(b.role, role);
    assert.equal(b.requested_commit, c.repeat(40));
    assert.equal(b.requested_source, "artifact");
    assert.equal(b.commit, undefined);
  }
  const sup = new RunnerAdapter({ mode: "supervisor", build: { role: "head", gitRef: "main" }, repoRoot: ".", platform: "linux", list: () => new Map() });
  assert.deepEqual(
    [sup.describe().build.role, sup.describe().build.requested_source, sup.describe().build.requested_commit],
    ["head", "git_ref", undefined],
    "a ref that is not a commit is not quoted as one",
  );
  const short = new RunnerAdapter({ mode: "subject", build: { role: "head", commit: "deadbeef", exe: "x", source: "artifact" }, repoRoot: ".", platform: "linux", list: () => new Map() });
  assert.equal(short.describe().build.requested_commit, undefined, "an abbreviated sha is not a 40-hex commit, so it is not carried");
});

test("teardown counts a temp runner the supervisor no longer lists as closed, not a survivor (review round 2 nit 1)", async () => {
  const sup = fakeSupervisor({ lost: false, runners: [] });
  const a = new RunnerAdapter({ mode: "supervisor", build: { role: "single", commit: "a".repeat(40), gitRef: "a".repeat(40) }, repoRoot: ".", fetch: sup.fetch });
  a.supervisor = new SupervisorClient("http://127.0.0.1:9875", () => {}, { fetch: sup.fetch });
  a.supervisorRunnerId = "test-9883";
  a.requesterId = "termhost-bench-r-n1";
  const td = await a.teardown();
  assert.equal(sup.calls.includes("POST /runners/test-9883/stop"), false, "nothing to stop");
  assert.deepEqual(td.survivors, []);
  assert.ok(td.closed.some((c) => /test-9883 \(already gone: not listed\)/.test(c)), JSON.stringify(td.closed));
});

test("emergencyStop stops a known temp runner by its checked stop, and does nothing without one (review round 2 nit 5)", async () => {
  const requester = "termhost-bench-r-n5";
  const sup = fakeSupervisor({ lost: false, runners: [{ id: "test-9883", requester_id: requester }] });
  const a = new RunnerAdapter({ mode: "supervisor", build: { role: "single", commit: "a".repeat(40), gitRef: "a".repeat(40) }, repoRoot: ".", fetch: sup.fetch });
  a.supervisor = new SupervisorClient("http://127.0.0.1:9875", () => {}, { fetch: sup.fetch });
  assert.equal(await a.emergencyStop(), null, "no id known yet: nothing to stop");
  a.supervisorRunnerId = "test-9883";
  a.requesterId = requester;
  assert.match(await a.emergencyStop(), /stopped temp runner test-9883/);
  assert.ok(sup.calls.includes("POST /runners/test-9883/stop"));
});

test("the launch waits end at the run's abort signal, not at their timeouts (review round 2 nit 5)", async (t) => {
  const health = async () => ({ ok: true, status: 200, text: async () => JSON.stringify({ uiBridgeIpcObserved: false, responsive: false }) });
  const ac = new AbortController();
  setTimeout(() => ac.abort(), 50);
  const t0 = Date.now();
  await assert.rejects(new RunnerClient("http://127.0.0.1:1", () => {}, { fetch: health }).waitUiBridgeReady(600_000, { signal: ac.signal }), WaitAborted);
  const building = async () => ({ ok: true, status: 200, text: async () => JSON.stringify({ status: { state: "building" } }) });
  const ac2 = new AbortController();
  setTimeout(() => ac2.abort(), 50);
  await assert.rejects(new SupervisorClient("http://127.0.0.1:9875", () => {}, { fetch: building }).awaitSpawn("s-1", { signal: ac2.signal }), WaitAborted);
  const dir = mkdtempSync(join(tmpdir(), "tb-wait-"));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  const ac3 = new AbortController();
  setTimeout(() => ac3.abort(), 50);
  const gens = await waitForGenerators(dir, 2, { timeoutMs: 600_000, signal: ac3.signal });
  assert.equal(gens.ok, false);
  assert.match(gens.detail, /interrupted/);
  assert.equal(await waitForNewSlot(dir, new Set(), { timeoutMs: 600_000, signal: ac3.signal }), null);
  assert.ok(Date.now() - t0 < 10_000, `every wait ended at the abort (${Date.now() - t0} ms)`);
});


test("the subject env passes the runner's instance-root contract; XDG dirs inside the root are what CI's exit 78 was (ci run 38341619507)", () => {
  const root = "/home/runner/work/_temp/tb/termhost-bench/ci-1/units/u000-runner-default-single-n1-r0/runner-root";
  const base = { PATH: "/usr/bin", HOME: "/home/runner", QONTINUI_HOME: "/home/runner/.qontinui", QONTINUI_SESSION_NAMES_DIR: "/home/runner/.qontinui/session-names" };
  const env = subjectEnv({ base, root, port: 9877, instanceName: "termhost-ci-1", platform: "linux", display: ":90" });
  assert.deepEqual(subjectContractProblems(env, { platform: "linux" }), []);
  // The reproduction: the env the first CI run passed (XDG dirs inside the root).
  const ci = { ...env, XDG_CONFIG_HOME: `${root}/xdg/config`, XDG_DATA_HOME: `${root}/xdg/data` };
  assert.deepEqual(subjectContractProblems(ci, { platform: "linux" }), [`QONTINUI_INSTANCE_ROOT ${JSON.stringify(root)} contains the machine-global directory ${JSON.stringify(`${root}/xdg/data`)}`]);
  // Every other refusal path of validate_instance_root / root_problem.
  const bad = (patch) => subjectContractProblems({ ...env, ...patch }, { platform: "linux" });
  assert.match(bad({ QONTINUI_INSTANCE_ROOT: "relative/root" })[0], /must be an absolute path/);
  assert.match(bad({ QONTINUI_INSTANCE_ROOT: "/a/../b" })[0], /must not contain `\.\.`/);
  assert.match(bad({ QONTINUI_INSTANCE_ROOT: "/" })[0], /is a filesystem root/);
  assert.match(bad({ QONTINUI_INSTANCE_ROOT: "/home" })[0], /contains the machine-global directory "\/home\/runner/);
  assert.deepEqual(bad({ QONTINUI_PORT: "" }), ["QONTINUI_PORT is unset"]);
  assert.deepEqual(bad({ QONTINUI_PORT: " 9877" }), ['QONTINUI_PORT=" 9877" is not a valid port']);
  assert.deepEqual(bad({ QONTINUI_PORT: "0" }), ["QONTINUI_PORT=0 is not a fixed port"]);
  assert.deepEqual(bad({ QONTINUI_PORT: "9876" }), ["QONTINUI_PORT=9876 is the primary runner's port"]);
  assert.deepEqual(bad({ QONTINUI_HOME: "/home/runner/.qontinui" }), ['QONTINUI_HOME="/home/runner/.qontinui" is outside the instance root']);
  assert.deepEqual(bad({ QONTINUI_CONFIG_DIR: `${root}/../x` }), [`QONTINUI_CONFIG_DIR=${JSON.stringify(`${root}/../x`)} is outside the instance root`]);
  assert.deepEqual(bad({ QONTINUI_CONFIG_DIR: `${root}/config`, QONTINUI_HOME: "  " }), [], "inside the root, and a blank value is no override");
  // Windows folds case, as PLATFORM_PATH_CASE does there.
  const w = { QONTINUI_INSTANCE_ROOT: "C:\\tb\\u0\\runner-root", QONTINUI_PORT: "9877", USERPROFILE: "C:\\Users\\ci", QONTINUI_HOME: "c:\\TB\\u0\\RUNNER-ROOT\\home" };
  assert.deepEqual(subjectContractProblems(w, { platform: "win32" }), []);
});

test("the contract mirror reads the runner's own lists: ROOT_CONFINED_ENV_KEYS and machine_global_dirs, from instance_env.rs", () => {
  const src = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "..", "..", "..", "src-tauri", "src", "instance_env.rs"), "utf8");
  const keys = /pub const ROOT_CONFINED_ENV_KEYS: &\[&str\] = &\[([\s\S]*?)\];/.exec(src);
  assert.ok(keys, "ROOT_CONFINED_ENV_KEYS is defined");
  assert.deepEqual([...keys[1].matchAll(/"([A-Z0-9_]+)"/g)].map((m) => m[1]), [...ROOT_CONFINED_ENV_KEYS]);
  const dirsBody = /pub fn machine_global_dirs\(\) -> Vec<PathBuf> \{([\s\S]*?)\r?\n\}/.exec(src);
  assert.ok(dirsBody, "machine_global_dirs is defined");
  // One entry per line (an entry may itself contain a comma).
  const entries = /\[([\s\S]*?)\]\s*\.into_iter\(\)/.exec(dirsBody[1])[1].split(/\r?\n/).map((x) => x.trim().replace(/,$/, "")).filter(Boolean);
  assert.deepEqual(entries, ["dirs::data_local_dir()", "dirs::config_dir()", "crate::ambient::qontinui_dir_from(None, home.clone())", "home"], "the JS mirror checks exactly these four");
});

test("a subject launch the contract refuses is never spawned, and a subject that exits names its stderr tail (ci run 38341619507)", async (t) => {
  const dir = mkdtempSync(join(tmpdir(), "tb-contract-"));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  const unitDir = join(dir, "units", "u0");
  mkdirSync(unitDir, { recursive: true });
  const spawned = [];
  const a = new RunnerAdapter({
    mode: "subject",
    build: { role: "single", commit: "f".repeat(40), exe: process.execPath },
    repoRoot: ".",
    platform: process.platform,
    env: { PATH: process.env.PATH, HOME: process.env.HOME ?? dir, USERPROFILE: process.env.USERPROFILE ?? dir },
    extraEnv: { QONTINUI_HOME: join(tmpdir(), "somewhere-else", ".qontinui") },
    fetch: async () => {
      throw Object.assign(new TypeError("fetch failed"), { cause: { code: "ECONNREFUSED" } });
    },
    portListening: async () => false,
    spawnFn: (...x) => {
      spawned.push(x);
      throw new Error("must not spawn");
    },
    list: () => new Map(),
  });
  await assert.rejects(a.launchSubject({ runDir: dir, unitDir, genRunId: "r1", env: { env: {} } }), (e) => /instance-root contract: QONTINUI_HOME=.* is outside the instance root/.test(e.detail ?? e.message));
  assert.deepEqual(spawned, []);
  // A subject that did start and then refused: the exit AND its stderr tail.
  a.childExit = { code: 78, signal: null };
  a.stderrPath = join(unitDir, "runner-stderr.log");
  writeFileSync(a.stderrPath, "qontinui-runner: refusing to start under QONTINUI_INSTANCE_ROOT=/r: it contains the machine-global directory /r/xdg/data\n");
  const why = a.refusalLogText();
  assert.match(why, / — the runner exited \{"code":78,"signal":null\}/);
  assert.match(why, / — stderr: qontinui-runner: refusing to start under QONTINUI_INSTANCE_ROOT=\/r: it contains the machine-global directory \/r\/xdg\/data$/);
});

test("a component action's value is its envelope's `result`: the zone view state is read, not lost (ci run 38345475088)", async (t) => {
  assert.deepEqual(actionResult({ success: true, result: { zoneCount: 4, layoutId: "quad" }, durationMs: 2 }), { zoneCount: 4, layoutId: "quad" });
  assert.deepEqual(actionResult({ zoneCount: 1 }), { zoneCount: 1 }, "an already-flat value passes through");
  assert.throws(() => actionResult({ success: false, result: null, error: "Action not found" }), /Action not found/);
  const client = fakeClient();
  const a = subjectAdapter(t, client);
  client.terminals.push("t0");
  const look = async () => ({ ok: true, sessions: new Map([[0, { x: 10, y: 40, w: 160, h: 16 }]]), detail: "fake" });
  const r = await a.arrange("tiled", { slots: new Map([[0, "t0"]]), n: 1, look });
  assert.equal(r.ok, true);
  assert.equal(r.layout.layoutId, "quad");
  assert.equal(r.layout.zoneCount, 1);
});

test("a tiled arrange that shows no session fails as sessions_not_rendered, naming the layout, instead of soaking an empty page (ci run 38345475088)", async (t) => {
  const client = fakeClient();
  const a = subjectAdapter(t, client);
  client.terminals.push("t0");
  const look = async () => ({ ok: true, sessions: new Map(), detail: "fake: no marker" });
  const r = await a.arrange("tiled", { slots: new Map([[0, "t0"]]), n: 1, look });
  assert.equal(r.ok, false);
  assert.equal(r.failure.reason, "sessions_not_rendered");
  assert.match(r.failure.detail, /after restore-zone the probe saw no session marker \(layout quad, 1 zone\(s\), maximized zone none; fake: no marker\)/);
});

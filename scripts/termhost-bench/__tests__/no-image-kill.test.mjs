/**
 * Teardown never terminates a process by image name (served policy
 * `production-and-cost` `runner-lifecycle`; plan D5/D12): it acts only on PIDs
 * this run proved it started. FAILS when any adapter or teardown path gains a
 * name-based kill (static scan), and when a PID-based path signals a process the
 * run did not start — an unrelated process with the same image name, a recycled
 * PID, another run's generator (dynamic, against fake process tables).
 *
 *   node --test scripts/termhost-bench/__tests__/no-image-kill.test.mjs
 */

import { test } from "node:test";
import assert from "node:assert/strict";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, statSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, relative } from "node:path";
import { fileURLToPath } from "node:url";

import { TmuxAdapter } from "../adapters/tmux.mjs";
import { VscodeAdapter } from "../adapters/vscode.mjs";
import { terminatePid, terminateTree } from "../proc.mjs";
import { reapGenerators } from "../run.mjs";

const HERE = dirname(fileURLToPath(import.meta.url));
const BENCH = join(HERE, "..");
const SHARED_CLIENTS = join(BENCH, "..", "lib", "runner-clients.mjs");

/**
 * Every harness source file: the bench (minus tests) and the shared clients —
 * ES modules and the CommonJS of the VS Code harness extension alike.
 */
function harnessSources() {
  const out = [SHARED_CLIENTS];
  const walk = (dir) => {
    for (const name of readdirSync(dir)) {
      const p = join(dir, name);
      if (statSync(p).isDirectory()) {
        if (name !== "__tests__" && name !== "profiles") walk(p);
      } else if (/\.(mjs|cjs|js)$/.test(name)) {
        out.push(p);
      }
    }
  };
  walk(BENCH);
  return out;
}

test("the scan reaches the VS Code harness extension and the VS Code adapter", () => {
  const files = harnessSources().map((f) => relative(BENCH, f).replace(/\\/g, "/"));
  for (const f of ["vscode-harness-extension/harness.js", "vscode-harness-extension/extension.js", "vscode-harness-extension/protocol.js", "adapters/vscode.mjs", "adapters/vscode-install.mjs"]) {
    assert.ok(files.includes(f), `${f} is scanned`);
  }
});

/** Strip comments so documentation that NAMES a forbidden command does not trip the scan. */
function code(text) {
  return text.replace(/\/\*[\s\S]*?\*\//g, "").replace(/(^|[^:"'`\\])\/\/.*$/gm, "$1");
}

const FORBIDDEN = [
  [/\btaskkill\b/i, "taskkill (Windows name/tree kill)"],
  [/Stop-Process/i, "Stop-Process"],
  [/\bpkill\b/, "pkill"],
  [/\bkillall\b/, "killall"],
  [/\bpgrep\b/, "pgrep (a name-based selection feeding a kill)"],
  [/\bpidof\b/, "pidof"],
  [/\bwmic\b[^\n]*\bdelete\b/i, "wmic ... delete"],
  [/["'`]kill["'`]\s*,\s*\[/, 'spawning the external "kill" command'],
  [/\/IM\b/, "/IM (image-name selector)"],
];

test("no harness source contains a name-based kill", () => {
  const hits = [];
  for (const file of harnessSources()) {
    const text = code(readFileSync(file, "utf8"));
    for (const [re, what] of FORBIDDEN) {
      if (re.test(text)) hits.push(`${relative(BENCH, file)}: ${what}`);
    }
  }
  assert.deepEqual(hits, []);
});

test("process.kill is called only by proc.mjs, the one PID-scoped terminating door", () => {
  const offenders = harnessSources()
    .filter((f) => !f.endsWith(`${join("termhost-bench", "proc.mjs")}`))
    .filter((f) => /process\.kill\s*\(/.test(code(readFileSync(f, "utf8"))))
    .map((f) => relative(BENCH, f));
  assert.deepEqual(offenders, []);
});

test("the scan itself can fail: a planted name-based kill is caught", (t) => {
  const dir = mkdtempSync(join(tmpdir(), "tb-scan-"));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  const planted = join(dir, "bad.mjs");
  writeFileSync(planted, 'import { spawnSync } from "node:child_process";\nspawnSync("taskkill", ["/F", "/IM", "node.exe"]);\n');
  const text = code(readFileSync(planted, "utf8"));
  assert.ok(FORBIDDEN.some(([re]) => re.test(text)));
});

test("terminatePid refuses process groups, PIDs 0-1 and non-integers", () => {
  const kill = () => assert.fail("must not signal");
  for (const pid of [0, 1, -42, 3.5, Number.NaN, "123"]) {
    assert.throws(() => terminatePid(pid, { kill }), /single positive PID/);
  }
});

test("terminatePid leaves a PID alone when it no longer is the process the run started", () => {
  const signalled = [];
  const kill = (pid, sig) => signalled.push([pid, sig]);
  const table = new Map([[500, { pid: 500, ppid: 1, name: "bash", argv: ["bash"], cmdline: "bash", startKey: "9" }]]);
  const r = terminatePid(500, { table, expect: (p) => p.startKey === "1", kill });
  assert.equal(r.outcome, "identity_mismatch");
  assert.deepEqual(signalled, []);
  assert.equal(terminatePid(501, { table, expect: () => true, kill }).outcome, "gone");
  assert.deepEqual(signalled, []);
});

test("terminateTree signals the run's process and its descendants only — never a same-named stranger or a recycled PID", async () => {
  const proc = (pid, ppid, name, startKey) => ({ pid, ppid, name, argv: [name], cmdline: name, startKey });
  let table = new Map([
    [100, proc(100, 1, "qontinui-runner", "1000")], // ours (the root we spawned)
    [101, proc(101, 100, "WebKitWebProcess", "1001")],
    [102, proc(102, 101, "bash", "1002")],
    [103, proc(103, 100, "bash", "0500")], // a stale parent link: started BEFORE the root
    [200, proc(200, 1, "qontinui-runner", "0900")], // the operator's runner: same image, not ours
    [201, proc(201, 200, "bash", "0901")],
  ]);
  const signalled = [];
  const kill = (pid, sig) => {
    signalled.push([pid, sig]);
    table = new Map([...table].filter(([p]) => p !== pid));
  };
  const res = await terminateTree({ pid: 100, startKey: "1000" }, { list: () => table, kill, sleepFn: async () => {}, graceMs: 0 });
  const pids = signalled.map(([p]) => p);
  assert.deepEqual(pids, [102, 101, 100], "children first, then the root, nothing else");
  assert.ok(!pids.includes(200) && !pids.includes(201) && !pids.includes(103));
  assert.deepEqual(res.survivors, []);
});

test("generators are reaped by slot-file PID with their run id re-checked; others are never touched", async (t) => {
  const dir = mkdtempSync(join(tmpdir(), "tb-reap-"));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  const slot = (k, pid) => writeFileSync(join(dir, `slot-${k}`), `${JSON.stringify({ schema: "termhost-bench/slot/v1", slot: k, pid, ppid: 1 })}\n`);
  slot(0, 300); // our live generator
  slot(1, 302); // our slot, but PID 302 now belongs to an unrelated process
  const gen = (pid, id) => ({ pid, ppid: 1, name: "node", argv: ["node", "ai-session.mjs", "--termhost-bench-generator", id], cmdline: `node ai-session.mjs --termhost-bench-generator ${id}`, startKey: "5" });
  let table = new Map([
    [300, gen(300, "run-A-u0")],
    [301, gen(301, "run-B-u0")], // another run's generator: same image, never ours
    [302, { pid: 302, ppid: 1, name: "node", argv: ["node", "server.js"], cmdline: "node server.js", startKey: "6" }],
  ]);
  const signalled = [];
  const kill = (pid, sig) => {
    signalled.push([pid, sig]);
    table = new Map([...table].filter(([p]) => p !== pid));
  };
  const res = await reapGenerators(dir, "run-A-u0", { list: () => table, kill, waitMs: 0, sleepFn: async () => {} });
  assert.deepEqual(signalled.map(([p]) => p), [300]);
  assert.deepEqual(res.survivors, []);
  assert.ok(table.has(301) && table.has(302));
});

test("a generator that survives every signal is reported, not hidden", async (t) => {
  const dir = mkdtempSync(join(tmpdir(), "tb-reap-"));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  writeFileSync(join(dir, "slot-0"), `${JSON.stringify({ slot: 0, pid: 400, ppid: 1 })}\n`);
  const table = new Map([[400, { pid: 400, ppid: 1, name: "node", argv: ["node", "--termhost-bench-generator", "r"], cmdline: "node --termhost-bench-generator r", startKey: "1" }]]);
  const res = await reapGenerators(dir, "r", { list: () => table, kill: () => {}, waitMs: 0, sleepFn: async () => {} });
  assert.deepEqual(res.survivors.map((s) => s.pid), [400]);
});

test("VS Code teardown ends only the instance it launched — by PID and start key — never the operator's Code.exe or a recycled PID", async () => {
  const P = (pid, ppid, name, startKey) => ({ pid, ppid, name, argv: null, cmdline: name, startKey: String(startKey) });
  let table = new Map([
    [100, P(100, 1, "Code.exe", 1000)], // ours: the main process the adapter spawned
    [101, P(101, 100, "Code.exe", 1001)], // its GPU process
    [102, P(102, 100, "Code.exe", 1002)], // its pty host
    [103, P(103, 102, "OpenConsole.exe", 1003)],
    [104, P(104, 102, "powershell.exe", 1004)],
    [200, P(200, 1, "Code.exe", 500)], // the operator's VS Code: same image, never ours
    [201, P(201, 200, "Code.exe", 501)],
  ]);
  let reads = 0;
  const signalled = [];
  const adapter = new VscodeAdapter({
    platform: "win32",
    list: () => {
      reads += 1;
      // After teardown recorded what it owns, PID 104 is recycled by an unrelated process.
      if (reads === 2) table = new Map([...table].map(([pid, p]) => [pid, pid === 104 ? P(104, 7, "notepad.exe", 9999) : p]));
      return new Map(table);
    },
    kill: (pid, sig) => {
      signalled.push([pid, sig]);
      table = new Map([...table].filter(([p]) => p !== pid));
    },
    sleep: async () => {},
  });
  adapter.pid = 100;
  adapter.startKey = "1000";
  const res = await adapter.teardown();
  assert.deepEqual(signalled.map(([p]) => p), [103, 102, 101, 100], "children first, then the main process, nothing else");
  assert.ok(table.has(200) && table.has(201), "the operator's Code.exe is untouched");
  assert.ok(table.has(104) && table.get(104).name === "notepad.exe", "a recycled PID is untouched");
  assert.deepEqual(res.survivors, []);
});

test("a VS Code process that survives every signal is reported, not hidden; a main process recycled before teardown is not ours", async () => {
  const P = (pid, ppid, name, startKey) => ({ pid, ppid, name, argv: null, cmdline: name, startKey: String(startKey) });
  const stubborn = new VscodeAdapter({ platform: "win32", list: () => new Map([[100, P(100, 1, "Code.exe", 1000)]]), kill: () => {}, sleep: async () => {} });
  stubborn.pid = 100;
  stubborn.startKey = "1000";
  assert.deepEqual((await stubborn.teardown()).survivors.map((s) => s.pid), [100]);
  const signalled = [];
  const recycled = new VscodeAdapter({ platform: "win32", list: () => new Map([[100, P(100, 1, "Code.exe", 7777)]]), kill: (pid) => signalled.push(pid), sleep: async () => {} });
  recycled.pid = 100;
  recycled.startKey = "1000";
  await recycled.teardown();
  assert.deepEqual(signalled, [], "PID 100 now belongs to a process started after ours: not a target");
});

test("a launch whose VS Code exited claims nothing at teardown, even when a stranger on its PID shares its start key", async (t) => {
  const { EventEmitter } = await import("node:events");
  const P = (pid, ppid, name, startKey) => ({ pid, ppid, name, argv: null, cmdline: name, startKey: String(startKey) });
  const dir = mkdtempSync(join(tmpdir(), "tb-vsc-exit-"));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  const unitDir = join(dir, "units", "u0");
  mkdirSync(join(unitDir, "gen"), { recursive: true });
  const app = join(dir, "app");
  mkdirSync(app);
  writeFileSync(join(app, "code"), "x");
  // A possible ordering: the launch reads OUR live process's start key (the read
  // is synchronous, so no exit lands during it); our child exits afterwards; and
  // a stranger takes the PID. On Linux a start key is a clock tick, so a PID
  // reused within the same tick even carries the same key — only teardown's
  // "our child exited" guard tells them apart.
  let table = new Map([[600, P(600, 1, "code", 5000)]]);
  let child = null;
  const signalled = [];
  const a = new VscodeAdapter({
    platform: "linux",
    appPath: app,
    cacheDir: dir,
    shell: "/bin/bash",
    sandbox: () => ({ state: "OPERATIVE", reason: "fake", noSandbox: false }),
    exists: (p) => p === "/bin/bash" || existsSync(p),
    spawnFn: () => {
      child = new EventEmitter();
      child.pid = 600;
      setTimeout(() => {
        child.emit("exit", 1, null);
        table = new Map([[600, P(600, 1, "code", 5000)], [601, P(601, 600, "bash", 5001)]]);
      }, 5);
      return child;
    },
    list: () => new Map(table),
    kill: (pid, sig) => signalled.push([pid, sig]),
    sleep: (ms) => new Promise((r) => setTimeout(r, Math.min(ms, 10))),
    launchTimeoutMs: 2000,
    quitTimeoutMs: 10,
  });
  await assert.rejects(a.launch({ runDir: dir, unitDir, genDir: join(unitDir, "gen"), genRunId: "r-u0", env: { env: {} } }), /exited/);
  assert.equal(a.startKey, "5000", "the key was read while our process held the PID");
  const res = await a.teardown();
  assert.deepEqual(signalled, [], "neither the stranger nor its child is signalled");
  assert.ok(res.notes.some((n) => /cannot be proven this run's/.test(n)), res.notes.join("; "));
});

test("a recorded key never licenses teardown after the child exited, whatever now holds the PID", async () => {
  const P = (pid, ppid, name, startKey) => ({ pid, ppid, name, argv: null, cmdline: name, startKey: String(startKey) });
  const stranger = new Map([[600, P(600, 1, "Code.exe", 7000)], [601, P(601, 600, "Code.exe", 7001)]]);
  const signalled = [];
  const b = new VscodeAdapter({ platform: "win32", list: () => new Map(stranger), kill: (pid) => signalled.push([pid]), sleep: async () => {} });
  b.pid = 600;
  b.startKey = "7000";
  b.childExit = { code: 0, signal: null, afterMs: 5 };
  await b.teardown();
  assert.deepEqual(signalled, [], "an exited child's PID is never adopted");
});

test("a live VS Code whose start key was never read is not signalled, and is reported rather than hidden", async () => {
  const { EventEmitter } = await import("node:events");
  const signalled = [];
  const a = new VscodeAdapter({
    platform: "win32",
    list: () => new Map([[700, { pid: 700, ppid: 1, name: "Code.exe", argv: null, cmdline: "Code.exe", startKey: "1" }]]),
    kill: (pid) => signalled.push(pid),
    sleep: async () => {},
    quitTimeoutMs: 10,
  });
  a.child = new EventEmitter(); // ours, still running; the launch's table read failed
  a.pid = 700;
  a.startKey = null;
  const res = await a.teardown();
  assert.deepEqual(signalled, []);
  assert.deepEqual(res.survivors.map((s) => [s.pid, /cannot be proven/.test(s.detail)]), [[700, true]]);
});

test("VS Code teardown adopts a descendant only through parent links each started before its child", async () => {
  const P = (pid, ppid, name, startKey) => ({ pid, ppid, name, argv: null, cmdline: name, startKey: String(startKey) });
  let table = new Map([
    [100, P(100, 1, "Code.exe", 1000)], // ours
    [250, P(250, 100, "Code.exe", 2500)], // ours, but its PID was reused: an earlier 250 died
    [300, P(300, 250, "cmd.exe", 2000)], // the dead 250's child — a stranger whose parent link now points at ours
  ]);
  const signalled = [];
  const a = new VscodeAdapter({
    platform: "win32",
    list: () => new Map(table),
    kill: (pid) => {
      signalled.push(pid);
      table = new Map([...table].filter(([p]) => p !== pid));
    },
    sleep: async () => {},
  });
  a.pid = 100;
  a.startKey = "1000";
  await a.teardown();
  assert.deepEqual(signalled, [250, 100], "the stranger started before the PID it names as its parent: not ours");
  assert.ok(table.has(300));
});

test("tmux teardown is kill-server on the PRIVATE socket, and a lingering server is signalled only if it is ours", async (t) => {
  const dir = mkdtempSync(join(tmpdir(), "tb-tmux-"));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  const calls = [];
  const signalled = [];
  const adapter = new TmuxAdapter({
    platform: "linux",
    run: (cmd, args) => {
      calls.push([cmd, ...args]);
      return { status: 0, stdout: "", stderr: "" };
    },
    alive: () => true,
    // The server PID was recycled by an unrelated tmux (the operator's): no -L <our socket>.
    list: () => new Map([[4242, { pid: 4242, ppid: 1, name: "tmux", argv: ["tmux", "new-session"], cmdline: "tmux new-session", startKey: "1" }]]),
    kill: (pid, sig) => signalled.push([pid, sig]),
  });
  adapter.socket = "termhost-run-1-u0";
  adapter.serverPid = 4242;
  const res = await adapter.teardown();
  const killServer = calls.find((c) => c.includes("kill-server"));
  assert.deepEqual(killServer, ["tmux", "-L", "termhost-run-1-u0", "kill-server"]);
  assert.deepEqual(signalled, [], "the recycled PID is not our server");
  assert.deepEqual(res.survivors, []);
});

test("a tree walk needs the root's start key: without one nothing of the tree is signalled, but proven extras still are (review S8)", async () => {
  const proc = (pid, ppid, name, startKey) => ({ pid, ppid, name, argv: [name], cmdline: name, startKey });
  let table = new Map([
    [100, proc(100, 1, "qontinui-runner", "1000")],
    [101, proc(101, 100, "WebKitWebProcess", "1001")],
    [300, proc(300, 1, "postgres", "1100")],
  ]);
  const signalled = [];
  const kill = (pid, sig) => {
    signalled.push([pid, sig]);
    table = new Map([...table].filter(([p]) => p !== pid));
  };
  const res = await terminateTree(
    { pid: 100, startKey: null },
    { list: () => table, kill, sleepFn: async () => {}, graceMs: 0, alsoPids: [{ pid: 300, startKey: "1100" }] },
  );
  assert.deepEqual(signalled.map(([p]) => p), [300], "only the extra proven ours by its own start key");
  assert.match(res.refused, /no start key was captured/);
  assert.ok(table.has(100) && table.has(101));
  // A root that is already gone does not stop the proven extras either.
  const t2 = new Map([[301, proc(301, 1, "postgres", "1200")]]);
  const s2 = [];
  const r2 = await terminateTree({ pid: 100, startKey: "1000" }, { list: () => t2, kill: (pid, sig) => s2.push([pid, sig]), sleepFn: async () => {}, graceMs: 0, alsoPids: [{ pid: 301, startKey: "1200" }] });
  assert.deepEqual(s2.map(([p]) => p).slice(0, 1), [301]);
  assert.match(r2.note, /already gone/);
  // No root at all (our child already exited): only the extras.
  const r3 = await terminateTree(null, { list: () => new Map(), kill: () => assert.fail("nothing to signal"), sleepFn: async () => {}, graceMs: 0 });
  assert.deepEqual(r3, { signalled: [], survivors: [] });
});

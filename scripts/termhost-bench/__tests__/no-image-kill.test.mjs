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
import { mkdtempSync, readFileSync, readdirSync, rmSync, statSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, relative } from "node:path";
import { fileURLToPath } from "node:url";

import { TmuxAdapter } from "../adapters/tmux.mjs";
import { terminatePid, terminateTree } from "../proc.mjs";
import { reapGenerators } from "../run.mjs";

const HERE = dirname(fileURLToPath(import.meta.url));
const BENCH = join(HERE, "..");
const SHARED_CLIENTS = join(BENCH, "..", "lib", "runner-clients.mjs");

/** Every harness source file: the bench (minus tests) and the shared clients. */
function harnessSources() {
  const out = [SHARED_CLIENTS];
  const walk = (dir) => {
    for (const name of readdirSync(dir)) {
      const p = join(dir, name);
      if (statSync(p).isDirectory()) {
        if (name !== "__tests__" && name !== "profiles") walk(p);
      } else if (name.endsWith(".mjs")) {
        out.push(p);
      }
    }
  };
  walk(BENCH);
  return out;
}

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

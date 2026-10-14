/**
 * Settling perf-harness's inactive-page inference (`settle-perf-harness.mjs`, plan
 * `2026-10-11-terminal-page-performance-kpis-across-setups` Phase 3): the verdict
 * mapping (rendered / not_rendered / unknown for every failed precondition), the
 * perf-harness argv it builds, the count of terminals the harness left behind, the
 * teardown it always runs, and the CI step that runs it before and after the fix.
 * Everything runs on fakes: no display, no runner, no probe.
 *
 *   node --test scripts/termhost-bench/__tests__/settle-perf-harness.test.mjs
 */

import { test } from "node:test";
import assert from "node:assert/strict";
import { EventEmitter } from "node:events";
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { UsageError, main, normaliseSettleOptions } from "../cli.mjs";
import { SEAT_COMMAND, SETTLE_SCHEMA, harnessArgv, runSettle, seatTerminal, settleVerdict, summarizeHarnessRun } from "../settle-perf-harness.mjs";

const HERE = dirname(fileURLToPath(import.meta.url));
const REPO = join(HERE, "..", "..", "..");
const WORKFLOW = join(REPO, ".github", "workflows", "termhost-bench.yml");

// ---------------------------------------------------------------------------
// the verdict
// ---------------------------------------------------------------------------

const look = (sessions = [], o = {}) => ({ ok: true, sessions, harnessRunning: true, detail: "fake look", ...o });
const healthy = (o = {}) => ({ sessions: 10, launched: true, seated: true, harnessStarted: true, generatorsStarted: 10, looks: [look(), look(), look()], ...o });

test("verdict: a marker seen while the generators ran is `rendered`", () => {
  const v = settleVerdict(healthy({ looks: [look([]), look([3, 1]), look([1, 4])] }));
  assert.equal(v.verdict, "rendered");
  assert.deepEqual(v.sessionsSeen, [1, 3, 4]);
});

test("verdict: S of S generators, healthy looks, no marker is `not_rendered` (the inference confirmed)", () => {
  const v = settleVerdict(healthy());
  assert.equal(v.verdict, "not_rendered");
  assert.deepEqual(v.sessionsSeen, []);
  assert.match(v.reason, /confirmed/);
});

test("verdict: every failed precondition is `unknown` — never either answer", () => {
  const cases = {
    interrupted: healthy({ interrupted: true, looks: [look([1])] }),
    "not launched": healthy({ launched: false, launchDetail: "exit 78", looks: [look([1])] }),
    "no seat on the active page": healthy({ seated: false, seatDetail: "refused", looks: [look([1])] }),
    "harness not started": healthy({ harnessStarted: false, harnessDetail: "ENOENT" }),
    "generators short": healthy({ generatorsStarted: 9, looks: [look([1])] }),
    "no generators": healthy({ generatorsStarted: 0, looks: [] }),
    "no look": healthy({ looks: [] }),
    "every look after the harness exited": healthy({ looks: [look([2], { harnessRunning: false }), look([], { harnessRunning: false })] }),
    "a look failed": healthy({ looks: [look(), look([], { ok: false, detail: "no capture source opened" })] }),
  };
  for (const [name, f] of Object.entries(cases)) {
    const v = settleVerdict(f);
    assert.equal(v.verdict, "unknown", `${name}: ${v.reason}`);
    assert.ok(v.reason.length > 0, name);
  }
  // A failed look's markers never count, and a look after the harness exited is not one.
  assert.equal(settleVerdict(healthy({ looks: [look([5], { ok: false }), look()] })).verdict, "unknown");
  assert.equal(settleVerdict(healthy({ looks: [look([5], { harnessRunning: false }), look()] })).verdict, "not_rendered");
});

// ---------------------------------------------------------------------------
// the perf-harness argv
// ---------------------------------------------------------------------------

const argvBase = { harness: "/r/scripts/perf-harness.mjs", runnerUrl: "http://127.0.0.1:9911", sessions: 10, generator: "node gen --termhost-bench-generator x", soakMs: 45000, settleMs: 3000, label: "l", out: "/o.json" };

test("harness argv: an array naming the subject's URL, S, the generator and the soak", () => {
  const a = harnessArgv({ node: "/usr/bin/node", ...argvBase });
  assert.ok(Array.isArray(a));
  assert.deepEqual(a.slice(0, 2), ["/usr/bin/node", "/r/scripts/perf-harness.mjs"]);
  const val = (f) => a[a.indexOf(f) + 1];
  assert.equal(val("--runner-url"), "http://127.0.0.1:9911");
  assert.equal(val("--sessions"), "10");
  assert.equal(val("--generator"), argvBase.generator, "the generator is ONE argv element, never re-split");
  assert.equal(val("--soak-ms"), "45000");
  assert.equal(val("--out"), "/o.json");
  assert.ok(a.includes("--quiet"));
});

test("harness argv: the primary runner's port, a portless URL and S < 1 are refused", () => {
  assert.throws(() => harnessArgv({ ...argvBase, runnerUrl: "http://127.0.0.1:9876" }), /9876/);
  assert.throws(() => harnessArgv({ ...argvBase, runnerUrl: "http://localhost" }), /no port/);
  assert.throws(() => harnessArgv({ ...argvBase, sessions: 0 }), /sessions/);
  assert.throws(() => harnessArgv({ ...argvBase, generator: "" }), /generator/);
});

test("summarizeHarnessRun reads the levels' frontend, and an absent file is null", (t) => {
  const dir = mkdtempSync(join(tmpdir(), "tb-settle-sum-"));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  const p = join(dir, "run.json");
  writeFileSync(p, JSON.stringify({ label: "x", notes: ["n"], levels: [{ sessions: 10, created: 10, createFailures: [], frontend: { frameTimeMs: { p95: 17 } }, frontendError: null, webviewWedged: false }] }));
  assert.deepEqual(summarizeHarnessRun(p).levels[0], { sessions: 10, created: 10, createFailures: 0, frontend: { frameTimeMs: { p95: 17 } }, frontendError: null, webviewWedged: false });
  assert.equal(summarizeHarnessRun(join(dir, "absent.json")), null);
});

// ---------------------------------------------------------------------------
// runSettle end to end on fakes
// ---------------------------------------------------------------------------

/**
 * One settle on fakes. `slots`: how many generators the fake harness starts;
 * `seen`: what each look sees; `leftover`: terminals the harness leaves behind;
 * `exitAtSpawn`: the harness exits before any look.
 */
async function fakeSettle(t, { slots = 10, seen = [[], [], []], lookOk = true, leftover = 0, exitAtSpawn = false, launchFails = false, preExisting = ["default-0"], seatFails = false, listFailsAfterHarness = false } = {}) {
  const tmp = mkdtempSync(join(tmpdir(), "tb-settle-"));
  t.after(() => rmSync(tmp, { recursive: true, force: true }));
  const harness = join(tmp, "perf-harness.mjs");
  writeFileSync(harness, "// a fake harness\n");
  const calls = [];
  let child = null;
  let looks = 0;
  let harnessIds = [];
  let seatIds = [];
  const adapter = {
    port: 9911,
    platform: "linux",
    terminals: [],
    client: {
      listTerminals: async () => {
        calls.push(["listTerminals"]);
        if (listFailsAfterHarness && child && !child.running()) throw new Error("fake listing failure");
        return [...preExisting, ...seatIds, ...harnessIds].map((id) => ({ id }));
      },
    },
    async createWithCommand(count) {
      calls.push(["createWithCommand", count, this.command]);
      if (seatFails) return { tabIds: [], refusals: [{ lane: "thread_count", message: "fake refusal" }], error: null };
      seatIds = ["seat-0"];
      return { tabIds: ["seat-0"], refusals: [], error: null };
    },
    async launch(ctx) {
      calls.push(["launch", ctx.genRunId]);
      if (launchFails) throw Object.assign(new Error("refused"), { detail: "the subject exited 78" });
    },
    windowTarget: () => ({ pid: 4242 }),
    async teardown() {
      calls.push(["teardown", [...this.terminals]]);
      return { closed: [], survivors: [] };
    },
  };
  const deps = {
    log: () => {},
    sleep: async () => {},
    signals: new EventEmitter(),
    startEnvironment: async () => ({ env: { DISPLAY: ":99" }, stop: async () => calls.push(["env.stop"]) }),
    probePath: "fake-probe",
    createAdapter: async (setup, o) => {
      calls.push(["createAdapter", setup, o.mode, o.build.commit]);
      return adapter;
    },
    probe: {
      activate: async () => ({ ok: true, detail: "fake" }),
      look: async (o) => {
        const i = looks++;
        calls.push(["look", o.target.pid]);
        if (looks === 3) setImmediate(() => child.finish());
        return { ok: lookOk, sessions: new Map((seen[i] ?? []).map((k) => [k, {}])), frames: 4, detail: lookOk ? "fake look" : "no capture source opened" };
      },
    },
    spawnHarness: (argv, o) => {
      calls.push(["spawnHarness", argv]);
      const gen = argv[argv.indexOf("--generator") + 1];
      const genDir = /--run-dir (\S+)/.exec(gen)[1];
      for (let k = 0; k < slots; k += 1) writeFileSync(join(genDir, `slot-${k}`), `${JSON.stringify({ slot: k, pid: 1000 + k })}\n`);
      harnessIds = Array.from({ length: leftover }, (_, i) => `h${i}`);
      let done = false;
      let resolveExit;
      const exited = new Promise((r) => (resolveExit = r));
      child = {
        pid: 777,
        exited,
        running: () => !done,
        finish: () => {
          done = true;
          resolveExit({ code: 0, signal: null });
        },
        stop: async () => {
          child.finish();
          return "exited";
        },
      };
      if (exitAtSpawn || slots < 10) child.finish();
      assert.ok(o.stdoutPath && o.stderrPath);
      return child;
    },
    reapGenerators: async (genDir, genRunId) => {
      calls.push(["reap", genRunId, existsSync(join(genDir, "stop"))]);
      return { ended: [], survivors: [] };
    },
  };
  const out = join(tmp, "settle.json");
  const res = await runSettle(
    { env: "linux-xvfb", runnerExe: "/x/qontinui-runner", headCommit: "a".repeat(40), harness, sessions: 10, workDir: tmp, out, runId: "settle-test", repoRoot: REPO },
    deps,
  );
  return { res, calls, out, written: existsSync(out) ? JSON.parse(readFileSync(out, "utf8")) : null };
}

test("runSettle: the harness child is an argv array against the subject's port, its generator the D13 command line", async (t) => {
  const { res, calls } = await fakeSettle(t, { seen: [[0], [0, 2], []] });
  const argv = calls.find((c) => c[0] === "spawnHarness")[1];
  assert.ok(Array.isArray(argv));
  assert.equal(argv[0], process.execPath);
  const url = argv[argv.indexOf("--runner-url") + 1];
  assert.equal(url, "http://127.0.0.1:9911");
  assert.notEqual(new URL(url).port, "9876");
  assert.equal(argv[argv.indexOf("--sessions") + 1], "10");
  const gen = argv[argv.indexOf("--generator") + 1];
  assert.match(gen, /generator\/ai-session\.mjs/);
  assert.match(gen, /--termhost-bench-generator settle-test-u0(\s|$)/);
  assert.match(gen, / --echo /);
  assert.ok(Number(argv[argv.indexOf("--soak-ms") + 1]) >= 30000, "the soak is long enough to hold the looks");
  assert.deepEqual(calls.find((c) => c[0] === "createAdapter").slice(1), ["runner", "subject", "a".repeat(40)]);
  assert.equal(res.verdict, "rendered");
  assert.deepEqual(res.sessionsSeen, [0, 2]);
});

test("runSettle: `rendered` and `not_rendered` written with the schema; terminals left come from the listing", async (t) => {
  const r1 = await fakeSettle(t, { seen: [[], [4], []], leftover: 0 });
  assert.equal(r1.written.schema, SETTLE_SCHEMA);
  assert.equal(r1.written.verdict, "rendered");
  assert.equal(r1.written.generatorsStarted, 10);
  assert.equal(r1.written.terminalsLeftAfterHarness, 0, "neither the pre-existing nor the seated terminal is the harness's");
  assert.equal(r1.written.seatedTerminal, "seat-0");
  // The seat is made through the launch menu, count 1, a marker-free command — before the harness.
  const seatAt = r1.calls.findIndex((c) => c[0] === "createWithCommand");
  assert.deepEqual(r1.calls[seatAt], ["createWithCommand", 1, SEAT_COMMAND]);
  assert.ok(seatAt < r1.calls.findIndex((c) => c[0] === "spawnHarness"), "seated before the harness spawns");
  assert.ok(r1.calls.find((c) => c[0] === "teardown")[1].includes("seat-0"), "the seat is closed at teardown");
  assert.equal(r1.written.perfHarnessExit.code, 0);
  assert.match(r1.written.harnessSha256, /^[0-9a-f]{64}$/);

  const r2 = await fakeSettle(t, { seen: [[], [], []], leftover: 10 });
  assert.equal(r2.written.verdict, "not_rendered");
  assert.equal(r2.written.terminalsLeftAfterHarness, 10);
  // Teardown closes every terminal the subject still holds, the harness's included.
  const td = r2.calls.find((c) => c[0] === "teardown")[1];
  for (const id of ["default-0", ...Array.from({ length: 10 }, (_, i) => `h${i}`)]) assert.ok(td.includes(id), id);
  // The stop file is written before the generators are reaped; the environment stops last.
  assert.deepEqual(r2.calls.find((c) => c[0] === "reap"), ["reap", "settle-test-u0", true]);
  assert.equal(r2.calls.at(-1)[0], "env.stop");
});

test("runSettle: a failed precondition is `unknown`, and teardown still runs", async (t) => {
  const short = await fakeSettle(t, { slots: 7 });
  assert.equal(short.res.verdict, "unknown");
  assert.equal(short.res.generatorsStarted, 7);
  assert.equal(short.calls.filter((c) => c[0] === "look").length, 0, "no look is taken before every generator runs");
  assert.ok(short.calls.some((c) => c[0] === "teardown"));

  const early = await fakeSettle(t, { exitAtSpawn: true });
  assert.equal(early.res.verdict, "unknown");
  assert.match(early.res.verdictReason, /perf-harness|ended/);

  const blind = await fakeSettle(t, { lookOk: false, seen: [[1], [1], [1]] });
  assert.equal(blind.res.verdict, "unknown");

  const unseated = await fakeSettle(t, { seatFails: true });
  assert.equal(unseated.res.verdict, "unknown");
  assert.match(unseated.res.verdictReason, /seated/);
  assert.ok(!unseated.calls.some((c) => c[0] === "spawnHarness"), "no harness beside an unoccupied active page");
  assert.ok(unseated.calls.some((c) => c[0] === "teardown"));

  // The seat is closed at teardown even when the post-harness listing fails.
  const blindList = await fakeSettle(t, { listFailsAfterHarness: true, leftover: 2 });
  assert.equal(blindList.res.terminalsLeftAfterHarness, null, "an unread listing is UNKNOWN, never 0");
  assert.ok(blindList.calls.find((c) => c[0] === "teardown")[1].includes("seat-0"));

  const refused = await fakeSettle(t, { launchFails: true });
  assert.equal(refused.res.verdict, "unknown");
  assert.match(refused.res.verdictReason, /exited 78/);
  assert.ok(!refused.calls.some((c) => c[0] === "spawnHarness"), "no harness against a runner that did not launch");
  assert.ok(refused.calls.some((c) => c[0] === "teardown"));
  assert.ok(refused.written, "an unknown is written too");
});

test("seatTerminal: one terminal, listed; a refusal, a wrong count or an unlisted id is a failure", async () => {
  let clock = 0;
  const deps = { timeoutMs: 1000, sleep: async (ms) => (clock += ms), now: () => clock };
  const adapter = (r, listed) => ({ command: null, createWithCommand: async (n) => (assert.equal(n, 1), r), client: { listTerminals: async () => listed.map((id) => ({ id })) } });
  const a = adapter({ tabIds: ["s1"], refusals: [] }, ["s1"]);
  assert.deepEqual(await seatTerminal(a, deps), { ok: true, id: "s1", detail: "seated terminal s1 on the active page" });
  assert.equal(a.command, SEAT_COMMAND);
  assert.doesNotMatch(SEAT_COMMAND, /termhost-bench-generator|ai-session/, "the seat paints no marker");
  // A refusal fails the seat even when a terminal was created and listed.
  const refused = await seatTerminal(adapter({ tabIds: ["s3"], refusals: [{ lane: "x", message: "m" }] }, ["s3"]), deps);
  assert.equal(refused.ok, false);
  assert.match(refused.detail, /refused/);
  assert.equal((await seatTerminal(adapter({ tabIds: ["a", "b"], refusals: [] }, ["a", "b"]), deps)).ok, false);
  clock = 0;
  const unlisted = await seatTerminal(adapter({ tabIds: ["s2"], refusals: [] }, []), deps);
  assert.equal(unlisted.ok, false);
  assert.match(unlisted.detail, /not listed/);
});

// ---------------------------------------------------------------------------
// the CLI
// ---------------------------------------------------------------------------

test("cli: settle-perf-harness requires its options and dispatches to runSettle", async () => {
  const base = { env: "linux-xvfb", "runner-exe": "/x/runner", "head-commit": "b".repeat(40), harness: "/x/h.mjs", out: "/x/o.json", "runner-env": ["A=1"] };
  for (const drop of ["env", "runner-exe", "head-commit", "harness", "out"]) {
    const f = { ...base };
    delete f[drop];
    assert.throws(() => normaliseSettleOptions(f), UsageError, drop);
  }
  const o = normaliseSettleOptions({ ...base, sessions: "10" });
  assert.equal(o.sessions, 10);
  assert.deepEqual(o.runnerEnv, { A: "1" });
  assert.equal(o.headCommit, "b".repeat(40));
  let got = null;
  const out = [];
  const code = await main(
    ["settle-perf-harness", "--env", "linux-xvfb", "--runner-exe", "/x/runner", "--head-commit", "b".repeat(40), "--harness", "/x/h.mjs", "--out", "/x/o.json", "--sessions", "10"],
    { out: (s) => out.push(s), err: () => {} },
    {
      runSettle: async (opts) => {
        got = opts;
        return { verdict: "rendered", verdictReason: "r", generatorsStarted: 10, sessions: 10, sessionsSeen: [1], perfHarnessExit: { code: 0 }, terminalsLeftAfterHarness: 0 };
      },
    },
  );
  assert.equal(code, 0);
  assert.equal(got.sessions, 10);
  assert.match(out.join(""), /settle-perf-harness: rendered/);
});

// ---------------------------------------------------------------------------
// the CI step
// ---------------------------------------------------------------------------

function jobText(yml, key) {
  const lines = yml.split(/\r?\n/);
  const start = lines.findIndex((l) => l === `  ${key}:`);
  assert.ok(start > 0, `the workflow has no job ${key}`);
  let end = lines.length;
  for (let i = start + 1; i < lines.length; i += 1) {
    if (/^ {2}[A-Za-z0-9_-]+:\s*$/.test(lines[i])) {
      end = i;
      break;
    }
  }
  return lines.slice(start, end).join("\n");
}

test("ci: the settle step runs before and after, asserts on the head run only, and perf-harness changes trigger it", () => {
  const yml = readFileSync(WORKFLOW, "utf8");
  const job = jobText(yml, "run-linux-xvfb");
  const at = job.indexOf("- name: Settle perf-harness's inactive-page inference");
  assert.ok(at > 0, "the step exists");
  const step = job.slice(at, job.indexOf("\n      - name:", at + 1) < 0 ? undefined : job.indexOf("\n      - name:", at + 1));
  assert.match(step, /timeout-minutes: \d+/);
  assert.match(step, /git show origin\/main:scripts\/perf-harness\.mjs > scripts\/perf-harness\.settle-before\.mjs/);
  assert.match(step, /dbus-run-session --/);
  assert.match(step, /WEBKIT_DISABLE_DMABUF_RENDERER=1/);
  assert.equal((step.match(/cli\.mjs settle-perf-harness/g) ?? []).length, 1, "one invocation, run in a loop over before and head");
  assert.match(step, /for which in before head/);
  // The assertion reads the HEAD result only, and asks for `rendered` and zero left.
  assert.match(step, /head_verdict.*rendered|"\$head_verdict" != rendered/s);
  assert.match(step, /head_left.*0|"\$head_left" != 0/s);
  assert.match(step, /head="\$out\/head\.json"/, "the asserted file is the head run's");
  assert.match(step, /head_verdict="\$\(jq -r \.verdict "\$head"\)"/);
  assert.doesNotMatch(step, /before_verdict" != /, "the before run is evidence, never asserted");
  const scope = /grep -qE '([^']+)'/.exec(jobText(yml, "bench-scope"));
  assert.ok(new RegExp(scope[1]).test("scripts/perf-harness.mjs"), "bench-scope runs it when perf-harness changes");
  assert.ok(new RegExp(scope[1]).test("scripts/perf-harness-lib.mjs"), "bench-scope runs it when perf-harness's lib changes");
  assert.ok(!new RegExp(scope[1]).test("scripts/perf-harness-lib.mjs.bak"));
  const prPaths = yml.slice(yml.indexOf("  pull_request:"), yml.indexOf("  push:"));
  assert.match(prPaths, /- "scripts\/perf-harness\.mjs"/);
  assert.match(prPaths, /- "scripts\/perf-harness-lib\.mjs"/);
  // The failure-only log upload carries the settle's work dir and results.
  const upload = job.slice(job.indexOf("- name: Upload runner unit logs (on failure)"));
  assert.match(upload, /^ {12}\$\{\{ runner\.temp \}\}\/tb-settle\/work\/\*\*$/m);
  assert.match(upload, /^ {12}\$\{\{ runner\.temp \}\}\/tb-settle\/\*\.json$/m);
});

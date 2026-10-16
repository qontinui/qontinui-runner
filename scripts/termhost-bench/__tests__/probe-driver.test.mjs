/**
 * The probe driver (`probe.mjs`): one run per file. `watch` refuses a
 * `probe.jsonl` that already holds records and `inject` an `inject.jsonl`
 * (`crates/termhost-probe/src/records.rs` `FileMode::New`), while a unit's phases
 * share the generators' directory — so each finished run's file is set aside under
 * its phase's label before the next starts. Also: activation failures read back
 * from inject's records, and the argv the probe is given.
 *
 *   node --test scripts/termhost-bench/__tests__/probe-driver.test.mjs
 */

import { test } from "node:test";
import assert from "node:assert/strict";
import { EventEmitter } from "node:events";
import { spawnSync } from "node:child_process";
import { appendFileSync, existsSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, statSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve as resolvePath } from "node:path";

import { WatchProcess, activate, codecMismatch, endChild, freshTag, goldenCodec, injectArgs, locateProbeBinary, pinProbe, primaryCheckoutRoot, probeCandidateList, probeCandidates, probeIdentity, raceTimeout, resolveProbeBinary, runProbe, setAside, takeRunFile, watchArgs } from "../probe.mjs";
import { blockingMechanisms, mapDoctor, probeWhere } from "../doctor.mjs";
import { createHash } from "node:crypto";
import { validateResults } from "../validate.mjs";
import { linuxXvfbResults } from "./helpers/results.mjs";

function dir(t) {
  const d = mkdtempSync(join(tmpdir(), "tb-probe-"));
  t.after(() => rmSync(d, { recursive: true, force: true }));
  return d;
}

const line = (r) => `${JSON.stringify({ schema: "termhost-probe/v1", ...r })}\n`;

/** A fake `watch`: writes its start record when spawned, its summary and end when stdin closes. */
function fakeWatchSpawn(runDir) {
  const spawned = [];
  const spawnFn = (cmd, args) => {
    spawned.push([cmd, ...args]);
    const child = new EventEmitter();
    child.pid = 4321;
    const file = join(runDir, "probe.jsonl");
    appendFileSync(file, line({ type: "start", subcommand: "watch", t0_ns: "1" }));
    child.stdin = {
      end: () => {
        appendFileSync(file, line({ type: "summary", t_start_ns: "1", t_end_ns: "2" }) + line({ type: "end", t_ns: "3", exit_code: 0, reason: "stdin closed" }));
        setImmediate(() => child.emit("exit", 0, null));
      },
    };
    child.kill = () => child.emit("exit", null, "SIGTERM");
    return child;
  };
  return { spawnFn, spawned };
}

test("a finished run's file is set aside under its label, with a counter on a collision", (t) => {
  const d = dir(t);
  assert.equal(setAside(d, "probe", "focus-soak"), null, "no file, nothing to set aside");
  writeFileSync(join(d, "probe.jsonl"), "x\n");
  assert.equal(setAside(d, "probe", "focus-soak"), join(d, "probe-focus-soak.jsonl"));
  assert.equal(existsSync(join(d, "probe.jsonl")), false);
  writeFileSync(join(d, "probe.jsonl"), "y\n");
  assert.equal(setAside(d, "probe", "focus-soak"), join(d, "probe-focus-soak-1.jsonl"));
  assert.equal(readFileSync(join(d, "probe-focus-soak.jsonl"), "utf8"), "x\n", "an earlier run's file is never overwritten");
  assert.throws(() => setAside(d, "probe", "../escape"), /bad run-file label/);
  writeFileSync(join(d, "inject.jsonl"), line({ type: "inject", index: 0, key: "a" }));
  const taken = takeRunFile(d, "inject", "focus-input");
  assert.deepEqual(taken.records.map((r) => r.type), ["inject"]);
  assert.equal(taken.path, join(d, "inject-focus-input.jsonl"));
});

test("a watch reads its own run, sets the file aside when it stops, and the next watch starts on no file", async (t) => {
  const d = dir(t);
  const { spawnFn, spawned } = fakeWatchSpawn(d);
  const args = watchArgs({ runDir: d, target: { pid: 7 }, hostPids: [7], runId: "r-u0", stdinStop: true, tag: "0x0000abcd" });
  const w = new WatchProcess({ probe: "termhost-probe", args, runDir: d, label: "focus-soak", spawnFn });
  assert.equal(await w.ready({ sleepFn: async () => {} }), true);
  const seg = await w.stop();
  assert.deepEqual(seg.records.map((r) => r.type), ["start", "summary", "end"]);
  assert.equal(seg.path, join(d, "probe-focus-soak.jsonl"));
  assert.equal(existsSync(join(d, "probe.jsonl")), false);
  const w2 = new WatchProcess({ probe: "termhost-probe", args, runDir: d, label: "focus-input", spawnFn });
  const seg2 = await w2.stop();
  assert.equal(seg2.records.filter((r) => r.type === "start").length, 1, "the second run holds only its own records");
  assert.equal(spawned.length, 2);
  assert.ok(spawned[0].includes("--tag") && spawned[0].includes("0x0000abcd"));
});

test("a watch refuses to start over a file that still holds a run (it would be refused by the probe)", (t) => {
  const d = dir(t);
  writeFileSync(join(d, "probe.jsonl"), line({ type: "start" }));
  const { spawnFn, spawned } = fakeWatchSpawn(d);
  assert.throws(
    () => new WatchProcess({ probe: "p", args: ["watch", "--stdin-stop"], runDir: d, label: "tiled-soak", spawnFn }),
    /already holds a run's records/,
  );
  assert.equal(spawned.length, 0);
  assert.throws(() => new WatchProcess({ probe: "p", args: ["watch"], runDir: d, label: "x", spawnFn }), /--stdin-stop/);
});

test("activation reads inject's own records: an incident or a refusal is a failed activation", async (t) => {
  const d = dir(t);
  const run = (records, code) => async ({ args }) => {
    const runDir = args[args.indexOf("--run-dir") + 1];
    writeFileSync(join(runDir, "inject.jsonl"), records.map(line).join(""));
    return { code, stdout: "", stderr: "" };
  };
  const ok = await activate({ probe: "p", runDir: join(d, "a1"), target: { pid: 9 }, run: run([{ type: "start" }, { type: "end", exit_code: 0 }], 0) });
  assert.deepEqual(ok, { ok: true, detail: "target activated" });
  const failed = await activate({
    probe: "p",
    runDir: join(d, "a2"),
    target: { pid: 9 },
    run: run([{ type: "incident", kind: "harness_error", detail: "--activate could not make the target foreground: no WM" }], 0),
  });
  assert.equal(failed.ok, false);
  assert.match(failed.detail, /could not make the target foreground/);
  const refused = await activate({
    probe: "p",
    runDir: join(d, "a3"),
    target: { pid: 9 },
    run: run([{ type: "inject_abort", index: 0, reason: "operator_input", detail: "a key", keys_sent: 0 }], 3),
  });
  assert.match(refused.detail, /refused \(operator_input\)/);
  assert.equal((await activate({ probe: "p", runDir: join(d, "a4"), target: { title: "x" } })).ok, false, "a title is not unique enough");
});

test("inject and watch argv: count, tag, quiet period, host roots, run id", () => {
  const a = injectArgs({ runDir: "/r", target: { hwnd: 0x1234 }, count: 120, intervalMs: 100, startDelayMs: 300, settleMs: 800, tag: "0x00000001", quietMs: 2500 });
  assert.deepEqual(a, ["inject", "--run-dir", "/r", "--hwnd", "4660", "--count", "120", "--interval-ms", "100", "--start-delay-ms", "300", "--settle-ms", "800", "--tag", "0x00000001", "--quiet-ms", "2500"]);
  assert.throws(() => injectArgs({ runDir: "/r", target: { pid: 1 }, count: -1 }), /count/);
  const w = watchArgs({ runDir: "/r", target: { pid: 5 }, hostPids: [5, 6], runId: "r-u1", stdinStop: true });
  assert.deepEqual(w, ["watch", "--run-dir", "/r", "--pid", "5", "--host-pid", "5", "--host-pid", "6", "--run-id", "r-u1", "--frames", "all", "--stdin-stop"]);
});

test("an injection tag is 0x-hex and never 0 (what an untagged injection carries)", () => {
  assert.match(freshTag(), /^0x[0-9a-f]{8}$/);
  const seq = [0, 0, 5];
  assert.equal(freshTag(() => seq.shift()), "0x00000005");
});

test("a finished race clears its timer, so nothing waits out the rest of a timeout (review N6)", async () => {
  const set = [];
  const cleared = [];
  const fakeTimers = { setTimer: (fn, ms) => { set.push(ms); return { id: set.length }; }, clearTimer: (t) => cleared.push(t.id) };
  assert.equal(await raceTimeout(Promise.resolve("done"), 20000, fakeTimers), "done");
  assert.deepEqual(set, [20000]);
  assert.deepEqual(cleared, [1], "the 20 s timer is cleared as soon as the race is won");
  assert.equal(await raceTimeout(new Promise(() => {}), 5), "timeout");
});

/** A child that ignores SIGTERM and dies only on SIGKILL. */
function stubbornChild() {
  const child = new EventEmitter();
  child.pid = 777;
  child.signals = [];
  child.stdin = { end: () => {} };
  child.kill = (sig) => {
    child.signals.push(sig);
    if (sig === "SIGKILL") setImmediate(() => child.emit("exit", null, "SIGKILL"));
    return true;
  };
  return child;
}

test("a child that ignores SIGTERM is killed, never waited on forever (review N6)", async (t) => {
  const child = stubbornChild();
  const exited = new Promise((r) => child.once("exit", () => r()));
  assert.equal(await endChild(child, exited, { graceMs: 20 }), "killed");
  assert.deepEqual(child.signals, ["SIGTERM", "SIGKILL"]);
  // runProbe past its timeout: SIGTERM, then SIGKILL, then it answers.
  const c2 = stubbornChild();
  const res = await runProbe({ probe: "p", args: ["inject"], timeoutMs: 10, killGraceMs: 20, spawnFn: () => c2 });
  assert.deepEqual(c2.signals, ["SIGTERM", "SIGKILL"]);
  assert.equal(res.code, null);
  assert.match(res.stderr, /exceeded 10 ms/);
  // A watch that does not exit when its stdin closes.
  const d = dir(t);
  const c3 = stubbornChild();
  const w = new WatchProcess({ probe: "p", args: ["watch", "--stdin-stop"], runDir: d, label: "focus-soak", spawnFn: () => c3, killGraceMs: 20 });
  const seg = await w.stop({ timeoutMs: 10 });
  assert.equal(seg.timedOut, true);
  assert.deepEqual(c3.signals, ["SIGTERM", "SIGKILL"]);
});

// ---------------------------------------------------------------------------
// Finding the probe binary (KB runbook review): cargo-guard.sh builds a linked
// worktree's crates into the PRIMARY checkout's shared target-agent/.
// ---------------------------------------------------------------------------

test("the probe is looked for at every candidate, in order, and the first that exists wins", () => {
  const repo = join("/r", "wt");
  const primary = join("/r", "primary");
  const list = probeCandidateList({ env: {}, platform: "linux", repoRoot: repo, primaryRoot: primary });
  assert.deepEqual(list, [
    { path: join(repo, "target", "debug", "termhost-probe"), source: "checkout" },
    { path: join(repo, "target", "release", "termhost-probe"), source: "checkout" },
    { path: join(repo, "target-agent", "debug", "termhost-probe"), source: "checkout" },
    { path: join(repo, "target-agent", "release", "termhost-probe"), source: "checkout" },
    { path: join(primary, "target-agent", "debug", "termhost-probe"), source: "primary_shared" },
    { path: join(primary, "target-agent", "release", "termhost-probe"), source: "primary_shared" },
  ]);
  const tried = list.map((c) => c.path);
  assert.deepEqual(probeCandidates({ env: {}, platform: "linux", repoRoot: repo, primaryRoot: primary }), tried);
  // Each candidate alone is found, with its source.
  for (const c of list) {
    const got = locateProbeBinary({ env: {}, platform: "linux", repoRoot: repo, primaryRoot: primary, exists: (p) => p === c.path });
    assert.deepEqual([got.path, got.source, got.tried], [resolvePath(c.path), c.source, tried], c.path);
  }
  assert.equal(resolveProbeBinary({ env: {}, platform: "linux", repoRoot: repo, primaryRoot: primary, exists: (p) => p === tried[5] }), resolvePath(tried[5]));
  // In the primary itself (no primary root): four; .exe on Windows.
  assert.equal(probeCandidates({ env: {}, platform: "linux", repoRoot: repo, primaryRoot: null }).length, 4);
  assert.ok(probeCandidates({ env: {}, platform: "win32", repoRoot: repo, primaryRoot: null }).every((p) => p.endsWith("termhost-probe.exe")));
  const none = locateProbeBinary({ env: {}, platform: "linux", repoRoot: repo, primaryRoot: primary, exists: () => false });
  assert.deepEqual([none.path, none.source, none.tried.length], [null, null, 6]);
});

test("--probe or $TERMHOST_PROBE is the ONLY path tried: a missing named probe is never replaced by another build (S1)", () => {
  const repo = join("/r", "wt");
  const everything = () => true;
  assert.deepEqual(probeCandidateList({ explicit: "/x/probe", env: { TERMHOST_PROBE: "/e/probe" }, repoRoot: repo, primaryRoot: "/r/p" }), [{ path: "/x/probe", source: "explicit" }]);
  assert.deepEqual(probeCandidateList({ env: { TERMHOST_PROBE: "/e/probe" }, repoRoot: repo, primaryRoot: "/r/p" }), [{ path: "/e/probe", source: "env" }]);
  const missing = locateProbeBinary({ explicit: "/x/probe", env: {}, repoRoot: repo, primaryRoot: "/r/p", exists: (p) => p !== "/x/probe" });
  assert.deepEqual(missing, { path: null, source: "explicit", tried: ["/x/probe"] });
  const envMissing = locateProbeBinary({ env: { TERMHOST_PROBE: "/e/probe" }, repoRoot: repo, exists: (p) => p !== "/e/probe", gitCommonDir: () => { throw new Error("a named probe never asks git"); } });
  assert.deepEqual(envMissing, { path: null, source: "env", tried: ["/e/probe"] });
  assert.equal(locateProbeBinary({ explicit: "/x/probe", env: {}, repoRoot: repo, primaryRoot: null, exists: everything }).source, "explicit");
  const m = mapDoctor({ environment: "linux-xvfb", envPlatform: "linux", platform: "linux", nodeVersion: process.version, probePath: null, probeTried: missing.tried, probeSource: "explicit", display: null, setupMechanisms: {}, drain: null });
  assert.equal(m["probe.binary"].state, "INOPERATIVE-ON-THIS-MACHINE");
  assert.equal(m["probe.binary"].reason, "--probe names /x/probe, which does not exist; no other build is tried in its place");
  const e = mapDoctor({ environment: "linux-xvfb", envPlatform: "linux", platform: "linux", nodeVersion: process.version, probePath: null, probeTried: envMissing.tried, probeSource: "env", display: null, setupMechanisms: {}, drain: null });
  assert.match(e["probe.binary"].reason, /^\$TERMHOST_PROBE names \/e\/probe, which does not exist/);
});

const doctorFacts = (over) => ({ environment: "linux-xvfb", envPlatform: "linux", platform: "linux", nodeVersion: process.version, display: null, setupMechanisms: {}, drain: null, ...over });

test("the doctor holds the probe's codec to marker-golden.json: a drifted codec is INOPERATIVE and blocks the run (M1)", () => {
  const golden = goldenCodec();
  assert.deepEqual(codecMismatch(golden), []);
  const ok = mapDoctor(doctorFacts({ probePath: "/p/termhost-probe", probeSource: "checkout", probeDoctor: { record: { codec: golden, mechanisms: [] }, error: null } }));
  assert.equal(ok["probe.binary"].state, "OPERATIVE");
  const cases = [
    [{ ...golden, version: golden.version + 1 }, /version: probe 3, golden 2/],
    [{ ...golden, pixel: { ...golden.pixel, channel_low_max: 70 } }, /pixel\.channel_low_max: probe 70, golden 63/],
    [{ ...golden, decode_order: [...golden.decode_order].reverse() }, /decode_order: probe /],
    [{ ...golden, row_reasons: ["wrong_length"] }, /row_reasons: probe \["wrong_length"\]/],
    [undefined, /carries no codec/],
  ];
  for (const [codec, re] of cases) {
    const m = mapDoctor(doctorFacts({ probePath: "/p/termhost-probe", probeSource: "checkout", probeDoctor: { record: { codec, mechanisms: [] }, error: null } }));
    assert.equal(m["probe.binary"].state, "INOPERATIVE-ON-THIS-MACHINE", String(re));
    assert.match(m["probe.binary"].reason, /^termhost-probe at \/p\/termhost-probe decodes a different codec than this harness's marker-golden\.json \(/);
    assert.match(m["probe.binary"].reason, re);
    assert.ok(blockingMechanisms(m).some((b) => b.id === "probe.binary"));
  }
});

test("a probe from the primary checkout's shared target-agent is DEGRADED (provenance unproven), and does not block (M1)", () => {
  const m = mapDoctor(doctorFacts({ probePath: "/r/primary/target-agent/release/termhost-probe", probeSource: "primary_shared", probeDoctor: { record: { codec: goldenCodec(), mechanisms: [] }, error: null } }));
  assert.equal(m["probe.binary"].state, "DEGRADED");
  assert.match(m["probe.binary"].reason, /shared build \(target-agent\).+provenance unproven/);
  assert.ok(!blockingMechanisms(m).some((b) => b.id === "probe.binary"), "DEGRADED runs, and is recorded");
  // A drifted shared build is still INOPERATIVE.
  const drifted = mapDoctor(doctorFacts({ probePath: "/r/primary/target-agent/release/termhost-probe", probeSource: "primary_shared", probeDoctor: { record: { codec: { ...goldenCodec(), version: 9 }, mechanisms: [] }, error: null } }));
  assert.equal(drifted["probe.binary"].state, "INOPERATIVE-ON-THIS-MACHINE");
});

test("the run pins its probe: a copy in the run directory, its sha256, the original's path and source; a later rewrite of the original changes neither (M1, S1)", () => {
  const dir = mkdtempSync(join(tmpdir(), "tb-probe-pin-"));
  try {
    const exe = process.platform === "win32" ? "termhost-probe.exe" : "termhost-probe";
    const bin = join(dir, "shared", exe);
    mkdirSync(dirname(bin), { recursive: true });
    writeFileSync(bin, "probe-bytes", { mode: 0o755 });
    const runDir = join(dir, "run");
    const pinned = pinProbe(bin, runDir);
    assert.equal(pinned.path, join(runDir, "probe-bin", exe), "the name (and a Windows .exe) is kept");
    assert.equal(readFileSync(pinned.path, "utf8"), "probe-bytes");
    if (process.platform !== "win32") assert.equal(statSync(pinned.path).mode & 0o111, 0o111, "the exec bit survives");
    const want = `sha256:${createHash("sha256").update("probe-bytes").digest("hex")}`;
    assert.equal(pinned.sha256, want);
    assert.equal(pinned.size, 11);
    // The shared build is rewritten after pinning: the copy and its record stand.
    writeFileSync(bin, "another commit's probe");
    assert.equal(readFileSync(pinned.path, "utf8"), "probe-bytes");
    const id = probeIdentity({ path: bin, source: "primary_shared" }, { pinned, codecVersion: 2 });
    assert.deepEqual([id.path, id.source, id.executed_path, id.sha256, id.size_bytes, id.codec_version], [bin, "primary_shared", pinned.path, want, 11, 2]);
    assert.match(id.mtime, /^\d{4}-\d{2}-\d{2}T/);
    assert.deepEqual([id.build.state, id.build.reason], ["unknown", "not_instrumented"]);
    const unpinned = probeIdentity({ path: bin, source: "checkout" });
    assert.deepEqual([unpinned.sha256.state, unpinned.executed_path.state, unpinned.codec_version.state], ["unknown", "unknown", "unknown"]);
    const none = probeIdentity({ path: null, source: null });
    assert.deepEqual([none.path.state, none.source.state], ["unknown", "unknown"]);
    assert.throws(() => pinProbe(join(dir, "missing"), runDir), /ENOENT/);
    // The identity is valid where the schema puts it, and required.
    const doc = linuxXvfbResults();
    doc.run.probe = id;
    assert.equal(validateResults(doc).valid, true, JSON.stringify(validateResults(doc).errors.slice(0, 3)));
    delete doc.run.probe;
    assert.equal(validateResults(doc).valid, false, "run.probe is required");
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("a probe rewritten WHILE it is copied is refused, not pinned half-written; size and sha256 are the copy's, mtime the original's after the copy", () => {
  const dir = mkdtempSync(join(tmpdir(), "tb-probe-torn-"));
  try {
    const bin = join(dir, "termhost-probe");
    writeFileSync(bin, "probe-bytes", { mode: 0o755 });
    let calls = 0;
    const moving = (p) => {
      const st = statSync(p);
      calls += 1;
      return calls === 1 ? st : { ...st, mode: st.mode, size: st.size, mtimeMs: st.mtimeMs + 1000, mtime: new Date(st.mtimeMs + 1000) };
    };
    assert.throws(() => pinProbe(bin, join(dir, "run"), { stat: moving }), /termhost-probe changed while it was being copied/);
    const pinned = pinProbe(bin, join(dir, "run2"));
    assert.equal(pinned.size, statSync(pinned.path).size);
    assert.equal(pinned.source_mtime, statSync(bin).mtime.toISOString());
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("doctor messages name the ORIGINAL build, with the pinned copy beside it, so 'rebuild it' points at the real build (S3)", () => {
  const orig = "/r/primary/target-agent/release/termhost-probe";
  const copy = "/runs/r1/probe-bin/termhost-probe";
  assert.equal(probeWhere({ probePath: copy, probeOriginalPath: orig }), `${orig} (pinned copy ${copy})`);
  assert.equal(probeWhere({ probePath: orig, probeOriginalPath: null }), orig, "unpinned (the doctor command): the build itself");
  const drift = mapDoctor(doctorFacts({ probePath: copy, probeOriginalPath: orig, probeSource: "checkout", probeDoctor: { record: { codec: { ...goldenCodec(), version: 9 }, mechanisms: [] }, error: null } }));
  assert.match(drift["probe.binary"].reason, new RegExp(`^termhost-probe at ${orig.replace(/[/.]/g, "\\$&")} \\(pinned copy ${copy.replace(/[/.]/g, "\\$&")}\\) decodes a different codec .+: rebuild it from this checkout$`));
  const shared = mapDoctor(doctorFacts({ probePath: copy, probeOriginalPath: orig, probeSource: "primary_shared", probeDoctor: { record: { codec: goldenCodec(), mechanisms: [] }, error: null } }));
  assert.ok(shared["probe.binary"].reason.startsWith(`termhost-probe at ${orig} (pinned copy ${copy}) is the primary checkout's shared build`), shared["probe.binary"].reason);
  const silent = mapDoctor(doctorFacts({ probePath: copy, probeOriginalPath: orig, probeSource: "checkout", probeDoctor: null }));
  assert.equal(silent["probe.binary"].reason, `termhost-probe at ${orig} (pinned copy ${copy}) did not answer doctor`);
});

test("a linked worktree resolves its primary checkout through git rev-parse --git-common-dir", () => {
  // Pure: what git prints in each case.
  assert.equal(primaryCheckoutRoot("/p/wt", { gitCommonDir: () => "/p/primary/.git" }), resolvePath("/p/primary"));
  assert.equal(primaryCheckoutRoot("/p/primary", { gitCommonDir: () => ".git" }), null, "the primary itself");
  assert.equal(primaryCheckoutRoot("/p/bare-wt", { gitCommonDir: () => "/p/repo.git" }), null, "a bare repository has no checkout");
  assert.equal(primaryCheckoutRoot("/p/x", { gitCommonDir: () => null }), null, "git could not answer");
  // Real git: a primary and a linked worktree.
  const git = spawnSync("git", ["--version"]);
  if (git.status !== 0) return;
  const dir = realpathSync(mkdtempSync(join(tmpdir(), "tb-probe-wt-")));
  try {
    const primary = join(dir, "primary");
    const wt = join(dir, "wt");
    const g = (cwd, ...args) => {
      const r = spawnSync("git", ["-C", cwd, "-c", "core.hooksPath=/dev/null", "-c", "user.name=t", "-c", "user.email=t@t", ...args], { encoding: "utf8" });
      assert.equal(r.status, 0, r.stderr);
    };
    mkdirSync(primary);
    g(primary, "init", "-q");
    g(primary, "commit", "-q", "--allow-empty", "-m", "init");
    g(primary, "worktree", "add", "-q", "--detach", wt);
    assert.equal(realpathSync(primaryCheckoutRoot(wt)), realpathSync(primary), "realpath'd both sides (Windows 8.3 names and case)");
    assert.equal(primaryCheckoutRoot(primary), null);
    const release = join(primary, "target-agent", "release", "termhost-probe");
    mkdirSync(dirname(release), { recursive: true });
    writeFileSync(release, "");
    const found = locateProbeBinary({ env: {}, platform: "linux", repoRoot: wt });
    assert.equal(realpathSync(found.path), realpathSync(release));
    assert.equal(found.source, "primary_shared");
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("the doctor's INOPERATIVE probe.binary names every path it tried", () => {
  const tried = probeCandidates({ env: {}, platform: "linux", repoRoot: "/r/wt", primaryRoot: "/r/primary" });
  const m = mapDoctor({ environment: "linux-xvfb", envPlatform: "linux", platform: "linux", nodeVersion: process.version, probePath: null, probeTried: tried, display: null, setupMechanisms: {}, drain: null });
  assert.equal(m["probe.binary"].state, "INOPERATIVE-ON-THIS-MACHINE");
  for (const p of tried) assert.ok(m["probe.binary"].reason.includes(p), p);
  assert.match(m["probe.binary"].reason, /; tried /);
});

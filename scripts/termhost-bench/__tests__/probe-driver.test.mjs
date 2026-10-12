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
import { appendFileSync, existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { WatchProcess, activate, freshTag, injectArgs, setAside, takeRunFile, watchArgs } from "../probe.mjs";

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

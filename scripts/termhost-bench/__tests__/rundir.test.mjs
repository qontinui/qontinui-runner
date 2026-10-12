/**
 * Run-directory isolation and the D13 command line (`rundir.mjs`): every run gets
 * its own directory under the OS temp dir, created exclusively; units are
 * exclusive inside it; nothing may be written outside it; the generator argv is
 * Phase 1's, `--echo` included; and the shell renderings round-trip.
 *
 *   node --test scripts/termhost-bench/__tests__/rundir.test.mjs
 */

import { test } from "node:test";
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { existsSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, relative, resolve } from "node:path";

import {
  GENERATOR_SCRIPT,
  RUN_ID_PATTERN,
  assertInside,
  benchRoot,
  commandLine,
  createRunDir,
  createUnitDir,
  generatorArgv,
  generatorRunId,
  makeRunId,
  posixQuote,
  powershellQuote,
} from "../rundir.mjs";
import { parseArgs } from "../generator/ai-session.mjs";

function base(t) {
  const dir = mkdtempSync(join(tmpdir(), "tb-rundir-"));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  return dir;
}

test("run ids are timestamped, random and generator-safe", () => {
  const id = makeRunId(new Date("2026-10-12T01:02:03.456Z"), "a1b2c3");
  assert.equal(id, "20261012T010203Z-a1b2c3");
  assert.match(makeRunId(), RUN_ID_PATTERN);
  assert.notEqual(makeRunId(), makeRunId());
});

test("each run gets its own directory, created exclusively under the bench root", (t) => {
  const b = base(t);
  const a = createRunDir({ runId: "run-a", base: b });
  const c = createRunDir({ runId: "run-b", base: b });
  assert.equal(dirname(a), benchRoot(b));
  assert.notEqual(a, c);
  assert.throws(() => createRunDir({ runId: "run-a", base: b }), /EEXIST/);
  assert.throws(() => createRunDir({ runId: "../escape", base: b }), /bad run id/);
  assert.throws(() => createRunDir({ runId: "x".repeat(60), base: b }), /bad run id/);
  // The default base is the OS temp dir.
  assert.equal(benchRoot(), join(resolve(tmpdir()), "termhost-bench"));
});

test("units are exclusive directories inside their run, each with its own gen dir", (t) => {
  const run = createRunDir({ runId: "run-u", base: base(t) });
  const unit = { index: 3, setup: "runner", variant: "default", role: "head", n: 15, runIndex: 1 };
  const u = createUnitDir(run, unit);
  assert.equal(u.name, "u003-runner-default-head-n15-r1");
  assert.ok(existsSync(u.genDir));
  assert.ok(!relative(run, u.unitDir).startsWith(".."));
  assert.throws(() => createUnitDir(run, unit), /EEXIST/);
  const v = createUnitDir(run, { ...unit, index: 4 });
  assert.notEqual(v.genDir, u.genDir);
});

test("nothing may be written outside the run directory", (t) => {
  const run = createRunDir({ runId: "run-w", base: base(t) });
  assert.equal(assertInside(run, join(run, "units", "x", "tmux.conf")), resolve(run, "units", "x", "tmux.conf"));
  assert.throws(() => assertInside(run, join(run, "..", "elsewhere")), /not inside the run directory/);
  assert.throws(() => assertInside(run, run), /not inside the run directory/);
  assert.throws(() => assertInside(run, resolve(tmpdir(), "outside.json")), /not inside the run directory/);
});

test("the D13 argv is Phase 1's command line, --echo included, and the generator parses it", () => {
  const argv = generatorArgv({ node: "node", genDir: "/tmp/run/gen", profile: "claude-tui", seed: 7, genRunId: generatorRunId("run-1", 12) });
  assert.deepEqual(argv, [
    "node",
    GENERATOR_SCRIPT,
    "--run-dir",
    "/tmp/run/gen",
    "--profile",
    "claude-tui",
    "--seed",
    "7",
    "--echo",
    "--termhost-bench-generator",
    "run-1-u12",
  ]);
  const parsed = parseArgs(argv.slice(2));
  assert.equal(parsed.echo, true);
  assert.equal(parsed.runId, "run-1-u12");
  assert.equal(parsed.seed, 7);
  assert.throws(() => generatorArgv({ genDir: "g", profile: "p", seed: -1, genRunId: "r" }), /seed/);
  assert.throws(() => generatorRunId("bad id with spaces", 1), /malformed/);
});

test("command lines quote safely for POSIX shells and PowerShell", () => {
  assert.equal(posixQuote("plain-arg_1.mjs"), "plain-arg_1.mjs");
  assert.equal(posixQuote("it's here"), "'it'\\''s here'");
  assert.equal(posixQuote(""), "''");
  assert.equal(powershellQuote("C:\\Program Files\\node.exe"), "'C:\\Program Files\\node.exe'");
  assert.equal(powershellQuote("it's"), "'it''s'");
  assert.equal(commandLine(["a b", "c"], "powershell"), "& 'a b' 'c'");
  assert.throws(() => commandLine([], "posix"), /non-empty/);
  assert.throws(() => commandLine(["a"], "cmd"), /unknown shell flavor/);
});

test("a POSIX command line round-trips through a real shell to the same argv", (t) => {
  const sh = spawnSync("sh", ["-c", "true"], { encoding: "utf8" });
  if (sh.error || sh.status !== 0) {
    t.skip("no POSIX sh on this machine");
    return;
  }
  const argv = ["printf", "%s\\n", "two words", "it's", "$HOME", "semi;colon", "back\\slash"];
  const res = spawnSync("sh", ["-c", commandLine(argv, "posix")], { encoding: "utf8" });
  assert.equal(res.status, 0, res.stderr);
  assert.deepEqual(res.stdout.split("\n").slice(0, 5), ["two words", "it's", "$HOME", "semi;colon", "back\\slash"]);
});

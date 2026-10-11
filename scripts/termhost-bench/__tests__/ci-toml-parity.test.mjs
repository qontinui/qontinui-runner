/**
 * The termhost-bench tests run in both CI lanes: `ci.yml`'s `test` job (bash
 * expands the glob on both matrix legs) and its runner-as-CI-node mirror in
 * `.qontinui/ci.toml`, whose argv has no shell and so lists the files. The ci.toml
 * header calls a `test`-job step without a line there drift; this test is what
 * catches a new test file added here but not there.
 *
 *   node --test scripts/termhost-bench/__tests__/ci-toml-parity.test.mjs
 */

import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync, readdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));
const REPO = join(HERE, "..", "..", "..");
const TEST_DIR = "scripts/termhost-bench/__tests__";

/** The quoted argv strings of the `[[steps]]` block named `name`. */
function stepCommand(toml, name) {
  const blocks = toml.split(/^\[\[steps\]\]\s*$/m).slice(1);
  const block = blocks.find((b) => new RegExp(`^name\\s*=\\s*"${name}"\\s*$`, "m").test(b));
  assert.ok(block, `.qontinui/ci.toml has no step named ${name}`);
  const command = /^command\s*=\s*\[([\s\S]*?)\]/m.exec(block);
  assert.ok(command, `${name} has no command array`);
  return [...command[1].matchAll(/"([^"]*)"/g)].map((m) => m[1]);
}

test("the ci.toml mirror runs exactly the termhost-bench test files", () => {
  const argv = stepCommand(
    readFileSync(join(REPO, ".qontinui", "ci.toml"), "utf8"),
    "termhost-bench-node-tests",
  );
  assert.deepEqual(argv.slice(0, 2), ["node", "--test"]);
  const onDisk = readdirSync(join(REPO, TEST_DIR))
    .filter((f) => f.endsWith(".test.mjs"))
    .map((f) => `${TEST_DIR}/${f}`)
    .sort();
  assert.deepEqual([...argv.slice(2)].sort(), onDisk);
});

test("ci.yml's test job runs the same files through a bash-expanded glob", () => {
  const yml = readFileSync(join(REPO, ".github", "workflows", "ci.yml"), "utf8");
  const at = yml.indexOf("- name: Run termhost-bench unit tests");
  assert.ok(at > 0, "the ci.yml step is missing");
  const step = yml.slice(at, yml.indexOf("\n\n", at));
  assert.match(step, /^\s+shell: bash$/m);
  assert.match(step, /^\s+run: node --test scripts\/termhost-bench\/__tests__\/\*\.test\.mjs$/m);
  const jobs = yml.slice(yml.indexOf("\njobs:\n"));
  const testJob = jobs.indexOf("\n  test:\n");
  const nextJob = jobs.slice(testJob + 1).search(/\n  [a-z][a-z0-9_-]*:\n/);
  assert.ok(
    testJob >= 0 && jobs.indexOf("- name: Run termhost-bench unit tests") < testJob + 1 + nextJob,
    "the step lives in the test job",
  );
});

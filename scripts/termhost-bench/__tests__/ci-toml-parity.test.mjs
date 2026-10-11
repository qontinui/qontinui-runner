/**
 * The termhost-bench tests run in both CI lanes: `ci.yml`'s `test` job (bash expands
 * the glob on both matrix legs) and its runner-as-CI-node mirror in
 * `.qontinui/ci.toml`, whose argv has no shell and so lists the files. Both also run
 * runner#1856's frame-builder tests, which no other step runs. The ci.toml header
 * calls a `test`-job step without a line there drift; this test catches a new test
 * file added here but not there, and a step moved, gated or narrowed.
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
/** The one file outside the directory both lanes run: runner#1856's frame-builder tests. */
const CARRIED_TEST = "scripts/__tests__/tui-repaint-generator.test.mjs";
const STEP_NAME = "Run termhost-bench unit tests";

/** The `[[steps]]` block named `name`, as text. */
function tomlStep(toml, name) {
  const blocks = toml.split(/^\[\[steps\]\]\s*$/m).slice(1);
  const block = blocks.find((b) => new RegExp(`^name\\s*=\\s*"${name}"\\s*$`, "m").test(b));
  assert.ok(block, `.qontinui/ci.toml has no step named ${name}`);
  return block;
}

/** The step's lines and the key of the job holding it, by indentation. */
function ymlStep(yml, name) {
  const lines = yml.split(/\r?\n/);
  const at = lines.findIndex((l) => l === `      - name: ${name}`);
  assert.ok(at > 0, `ci.yml has no step "${name}" at step indentation`);
  let job = null;
  for (let i = at; i >= 0 && job === null; i -= 1) {
    const m = /^ {2}([A-Za-z0-9_-]+):\s*$/.exec(lines[i]);
    if (m) job = m[1];
  }
  const step = [lines[at]];
  for (let i = at + 1; i < lines.length; i += 1) {
    if (/^ {0,6}\S/.test(lines[i]) || lines[i].trim() === "") break;
    step.push(lines[i]);
  }
  return { job, step };
}

test("the ci.toml mirror runs exactly the termhost-bench test files plus the carried one", () => {
  const block = tomlStep(
    readFileSync(join(REPO, ".qontinui", "ci.toml"), "utf8"),
    "termhost-bench-node-tests",
  );
  const command = /^command\s*=\s*\[([\s\S]*?)\]/m.exec(block);
  assert.ok(command, "the step has no command array");
  const argv = [...command[1].matchAll(/"([^"]*)"/g)].map((m) => m[1]);
  assert.deepEqual(argv.slice(0, 2), ["node", "--test"]);
  const onDisk = readdirSync(join(REPO, TEST_DIR))
    .filter((f) => f.endsWith(".test.mjs"))
    .map((f) => `${TEST_DIR}/${f}`);
  assert.deepEqual([...argv.slice(2)].sort(), [...onDisk, CARRIED_TEST].sort());
  assert.match(block, /^timeout_secs\s*=\s*600\s*$/m, "the mirror is bounded like the ci.yml step");
});

test("ci.yml runs them in the test job, bash-shelled, unconditionally", () => {
  const { job, step } = ymlStep(
    readFileSync(join(REPO, ".github", "workflows", "ci.yml"), "utf8"),
    STEP_NAME,
  );
  assert.equal(job, "test", "the step lives in the test job, so both matrix legs run it");
  const text = step.join("\n");
  assert.match(text, /^\s+shell: bash$/m);
  assert.match(text, /^\s+timeout-minutes: \d+$/m);
  assert.equal(
    step.find((l) => /^\s+run:/.test(l))?.trim(),
    `run: node --test ${TEST_DIR}/*.test.mjs ${CARRIED_TEST}`,
  );
  assert.ok(!step.some((l) => /^\s+if:/.test(l)), "the step carries no if: (it runs on every leg)");
  assert.ok(!step.some((l) => /^\s+continue-on-error:/.test(l)), "a red step must fail the job");
});

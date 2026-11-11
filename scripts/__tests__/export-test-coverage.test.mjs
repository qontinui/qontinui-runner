// Regression test for the coverage producer's per-file scratch dir
// (plan 2026-11-10-devops-coverage-producer-leaves-a-covprod-dir-per-test-file-in-ram-tmp):
// every `covprod-*` dir must be gone once its file is processed, also when
// processing throws. Run: node --test scripts/__tests__/export-test-coverage.test.mjs
import { test, before, after } from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, readdirSync, rmSync, writeFileSync, existsSync } from "node:fs";
import { join, dirname } from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath, pathToFileURL } from "node:url";
import { tmpdir } from "node:os";

const scriptPath = join(dirname(fileURLToPath(import.meta.url)), "..", "export-test-coverage.mjs");
const sandbox = mkdtempSync(join(tmpdir(), "covprod-test-sandbox-"));
const savedTmp = { TMPDIR: process.env.TMPDIR, TMP: process.env.TMP, TEMP: process.env.TEMP };
let withScratchDir;
/** @type {import("node:child_process").SpawnSyncReturns<string>} */
let guardProbe;

before(async () => {
  // Probe the direct-execution guard in a CHILD first. An unguarded main()
  // calls process.exit(0) at import, which would end THIS process with a pass
  // before any test ran. If the child shows the producer running, throw here,
  // so the in-process import below never happens and the file fails.
  const env = { ...process.env };
  delete env.GITHUB_SHA;
  const importer = `await import(${JSON.stringify(pathToFileURL(scriptPath).href)}); console.log("IMPORTED");`;
  guardProbe = spawnSync(process.execPath, ["--input-type=module", "-e", importer], {
    encoding: "utf-8",
    env,
  });
  const output = `${guardProbe.stdout}${guardProbe.stderr}`;
  if (output.includes("[export-test-coverage]") || !guardProbe.stdout.includes("IMPORTED")) {
    throw new Error(`importing the script ran the producer or failed:\n${output}`);
  }

  // os.tmpdir() reads these on every call, so the helper's dirs land here.
  process.env.TMPDIR = process.env.TMP = process.env.TEMP = sandbox;
  ({ withScratchDir } = await import("../export-test-coverage.mjs"));
});

after(() => {
  for (const [k, v] of Object.entries(savedTmp)) {
    if (v === undefined) delete process.env[k];
    else process.env[k] = v;
  }
  rmSync(sandbox, { recursive: true, force: true });
});

const leftovers = () => readdirSync(sandbox).filter((n) => n.startsWith("covprod-"));

test("importing the script does not run the producer", () => {
  assert.equal(guardProbe.status, 0, guardProbe.stderr);
  assert.equal(typeof withScratchDir, "function");
  assert.deepEqual(leftovers(), []);
});

test("the scratch dir is removed after a successful run, and the result is returned", () => {
  let seen;
  const out = withScratchDir((dir) => {
    seen = dir;
    assert.ok(dir.startsWith(join(sandbox, "covprod-")), dir);
    writeFileSync(join(dir, "coverage-final.json"), "{}");
    return { filesTouched: ["a"], junitXml: null };
  });
  assert.deepEqual(out, { filesTouched: ["a"], junitXml: null });
  assert.equal(existsSync(seen), false);
  assert.deepEqual(leftovers(), []);
});

test("the scratch dir is removed when the run throws, and the error propagates", () => {
  assert.throws(
    () =>
      withScratchDir((dir) => {
        writeFileSync(join(dir, "junit.xml"), "<x/>");
        throw new Error("boom");
      }),
    /boom/,
  );
  assert.deepEqual(leftovers(), []);
});

test("many runs leave nothing behind", () => {
  for (let i = 0; i < 25; i++) withScratchDir((dir) => writeFileSync(join(dir, "f"), "x"));
  assert.deepEqual(leftovers(), []);
});

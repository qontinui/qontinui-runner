#!/usr/bin/env node
// Smoke-test for coverage-diff.mjs (Section 11, Phase C3).
//
// Uses Node's built-in `node:test` runner so this file requires zero config
// in `vitest.config.ts` (which only globs `src/**`). The script under test is
// invoked as a child process so we exercise the real CLI surface — argv
// parsing, exit codes, stdout shape — exactly as CI would.
//
// Run with:
//   node --test scripts/__tests__/coverage-diff.test.mjs
//
// On Node 22+ (the runner's pinned Node) `node:test` and `node:assert` are
// stable and need no flags. We deliberately avoid vitest here so the script
// stays runnable in plain CI containers without the runner's full toolchain.

import test from "node:test";
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtempSync, writeFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const __filename = fileURLToPath(import.meta.url);
const __dirname = dirname(__filename);
const SCRIPT = resolve(__dirname, "..", "coverage-diff.mjs");
const REPO = resolve(__dirname, "..", "..");

// ---------------------------------------------------------------------------
// Fixture builders — minimal RegressionSuite shape that satisfies
// `deserializeSuite`'s shallow validation. Each case has 3 assertions; the
// total is 6 assertions across 2 cases.
// ---------------------------------------------------------------------------

function makeSuite() {
  return {
    id: "doc@suite",
    ir: { id: "doc", version: "1.0" },
    cases: [
      {
        id: "t-a-to-b",
        transitionId: "t-a-to-b",
        fromStates: ["a"],
        activateStates: ["b"],
        exitStates: [],
        assertions: [
          {
            kind: "state-active",
            phase: "pre",
            stateId: "a",
            requiredElementIds: [0, 1],
          },
          {
            kind: "state-active",
            phase: "post",
            stateId: "b",
            requiredElementIds: [0],
          },
          {
            kind: "action-target-resolves",
            transitionId: "t-a-to-b",
            actionIndex: 0,
            targetCriteria: { id: "btn-go" },
          },
        ],
      },
      {
        id: "t-b-to-c",
        transitionId: "t-b-to-c",
        fromStates: ["b"],
        activateStates: ["c"],
        exitStates: [],
        assertions: [
          {
            kind: "state-active",
            phase: "pre",
            stateId: "b",
            requiredElementIds: [0],
          },
          {
            kind: "state-active",
            phase: "post",
            stateId: "c",
            requiredElementIds: [],
          },
          {
            kind: "action-target-resolves",
            transitionId: "t-b-to-c",
            actionIndex: 0,
            targetCriteria: { id: "btn-next" },
          },
        ],
      },
    ],
  };
}

function makeFullLog() {
  return [
    {
      caseId: "t-a-to-b",
      assertionId: "state-active:pre:a",
      executedAt: "2026-05-02T00:00:00.000Z",
    },
    {
      caseId: "t-a-to-b",
      assertionId: "state-active:post:b",
      executedAt: "2026-05-02T00:00:00.000Z",
    },
    {
      caseId: "t-a-to-b",
      assertionId: "action-target-resolves:t-a-to-b#0",
      executedAt: "2026-05-02T00:00:00.000Z",
    },
    {
      caseId: "t-b-to-c",
      assertionId: "state-active:pre:b",
      executedAt: "2026-05-02T00:00:00.000Z",
    },
    {
      caseId: "t-b-to-c",
      assertionId: "state-active:post:c",
      executedAt: "2026-05-02T00:00:00.000Z",
    },
    {
      caseId: "t-b-to-c",
      assertionId: "action-target-resolves:t-b-to-c#0",
      executedAt: "2026-05-02T00:00:00.000Z",
    },
  ];
}

// ---------------------------------------------------------------------------
// Per-test scratch dir + child-process runner
// ---------------------------------------------------------------------------

function withFixtureDir(fn) {
  const dir = mkdtempSync(join(tmpdir(), "cov-diff-"));
  try {
    return fn(dir);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

function writeJson(dir, name, value) {
  const path = join(dir, name);
  writeFileSync(path, JSON.stringify(value, null, 2));
  return path;
}

function runCli(args) {
  // `inherit: false` keeps stderr off the test runner's tty; we capture both
  // streams as utf8 so assertions can match on substrings.
  const res = spawnSync(process.execPath, [SCRIPT, ...args], {
    cwd: REPO,
    encoding: "utf8",
  });
  return {
    code: res.status,
    stdout: res.stdout ?? "",
    stderr: res.stderr ?? "",
  };
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

test("exits 0 when coverage is full", () => {
  withFixtureDir((dir) => {
    const suitePath = writeJson(dir, "suite.json", makeSuite());
    const logPath = writeJson(dir, "log.json", makeFullLog());

    const r = runCli([
      "--suite",
      suitePath,
      "--exercise-log",
      logPath,
      "--threshold",
      "0.10",
    ]);
    assert.equal(r.code, 0, `expected exit 0, got ${r.code}\nstderr: ${r.stderr}`);
    assert.match(r.stdout, /Status: PASS/);
    assert.match(r.stdout, /6\/6 assertions/);
  });
});

test("exits 1 when log is empty (uncoveredRatio = 1.0)", () => {
  withFixtureDir((dir) => {
    const suitePath = writeJson(dir, "suite.json", makeSuite());
    const logPath = writeJson(dir, "log.json", []);

    const r = runCli(["--suite", suitePath, "--exercise-log", logPath]);
    assert.equal(r.code, 1, `expected exit 1, got ${r.code}\nstderr: ${r.stderr}`);
    assert.match(r.stdout, /Status: FAIL/);
    assert.match(r.stdout, /uncoveredRatio:\s+1\.0000/);
  });
});

test("exits 0 with --format json and stdout parses as CoverageDiffReport", () => {
  withFixtureDir((dir) => {
    const suitePath = writeJson(dir, "suite.json", makeSuite());
    const logPath = writeJson(dir, "log.json", makeFullLog());

    const r = runCli([
      "--suite",
      suitePath,
      "--exercise-log",
      logPath,
      "--format",
      "json",
    ]);
    assert.equal(r.code, 0, `expected exit 0, got ${r.code}\nstderr: ${r.stderr}`);

    const report = JSON.parse(r.stdout);
    // Shape check — every documented field must exist.
    assert.ok(Array.isArray(report.unexercisedAssertions));
    assert.ok(Array.isArray(report.uncoveredTransitions));
    assert.equal(typeof report.stats, "object");
    assert.equal(typeof report.stats.totalAssertions, "number");
    assert.equal(typeof report.stats.exercisedAssertions, "number");
    assert.equal(typeof report.stats.totalTransitions, "number");
    assert.equal(typeof report.stats.coveredTransitions, "number");
    assert.equal(typeof report.stats.uncoveredRatio, "number");
    // Value check for the full-log fixture.
    assert.equal(report.stats.totalAssertions, 6);
    assert.equal(report.stats.exercisedAssertions, 6);
    assert.equal(report.stats.uncoveredRatio, 0);
  });
});

test("exits 2 on invalid --threshold (out of [0,1])", () => {
  withFixtureDir((dir) => {
    const suitePath = writeJson(dir, "suite.json", makeSuite());
    const logPath = writeJson(dir, "log.json", makeFullLog());

    const r = runCli([
      "--suite",
      suitePath,
      "--exercise-log",
      logPath,
      "--threshold",
      "1.5",
    ]);
    assert.equal(r.code, 2, `expected exit 2, got ${r.code}\nstdout: ${r.stdout}`);
    assert.match(r.stderr, /--threshold must be a number in \[0, 1\]/);
  });
});

test("exits 2 when required flags are missing", () => {
  const r = runCli(["--threshold", "0.5"]);
  assert.equal(r.code, 2, `expected exit 2, got ${r.code}\nstdout: ${r.stdout}`);
  assert.match(r.stderr, /--suite and --exercise-log are both required/);
});

test("exits 1 when uncoveredRatio strictly exceeds threshold", () => {
  withFixtureDir((dir) => {
    // Only 1 of the 6 assertions is exercised → uncoveredRatio = 5/6 ≈ 0.833
    const suitePath = writeJson(dir, "suite.json", makeSuite());
    const partialLog = [
      {
        caseId: "t-a-to-b",
        assertionId: "state-active:pre:a",
        executedAt: "2026-05-02T00:00:00.000Z",
      },
    ];
    const logPath = writeJson(dir, "log.json", partialLog);

    const r = runCli([
      "--suite",
      suitePath,
      "--exercise-log",
      logPath,
      "--threshold",
      "0.50",
    ]);
    assert.equal(r.code, 1, `expected exit 1, got ${r.code}\nstderr: ${r.stderr}`);
    assert.match(r.stdout, /Status: FAIL/);
  });
});

test("exits 0 when uncoveredRatio exactly equals threshold (boundary)", () => {
  withFixtureDir((dir) => {
    // 1/2 transitions covered with 3 assertions exercised out of 6 →
    // uncoveredRatio = 0.5. Threshold = 0.5 must NOT fail (spec: strict >).
    const suitePath = writeJson(dir, "suite.json", makeSuite());
    const halfLog = [
      {
        caseId: "t-a-to-b",
        assertionId: "state-active:pre:a",
        executedAt: "2026-05-02T00:00:00.000Z",
      },
      {
        caseId: "t-a-to-b",
        assertionId: "state-active:post:b",
        executedAt: "2026-05-02T00:00:00.000Z",
      },
      {
        caseId: "t-a-to-b",
        assertionId: "action-target-resolves:t-a-to-b#0",
        executedAt: "2026-05-02T00:00:00.000Z",
      },
    ];
    const logPath = writeJson(dir, "log.json", halfLog);

    const r = runCli([
      "--suite",
      suitePath,
      "--exercise-log",
      logPath,
      "--threshold",
      "0.50",
    ]);
    assert.equal(r.code, 0, `expected exit 0, got ${r.code}\nstderr: ${r.stderr}`);
    assert.match(r.stdout, /Status: PASS/);
  });
});

test("exits 1 when --suite path does not exist", () => {
  withFixtureDir((dir) => {
    const logPath = writeJson(dir, "log.json", makeFullLog());

    const r = runCli([
      "--suite",
      join(dir, "missing.json"),
      "--exercise-log",
      logPath,
    ]);
    assert.equal(r.code, 1, `expected exit 1, got ${r.code}\nstdout: ${r.stdout}`);
    assert.match(r.stderr, /cannot read --suite/);
  });
});

test("--help prints usage and exits 0", () => {
  const r = runCli(["--help"]);
  assert.equal(r.code, 0, `expected exit 0, got ${r.code}\nstderr: ${r.stderr}`);
  assert.match(r.stdout, /Usage: coverage-diff\.mjs/);
  assert.match(r.stdout, /Exit codes:/);
});

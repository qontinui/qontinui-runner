#!/usr/bin/env node
// Unit tests for the PURE half of ci-flake-analyze.mjs (Phase 0 of plan
// `2026-08-30-runner-ci-has-no-flake-detection-so-one-flaky-test-freezes-the-train`).
//
// Uses Node's built-in `node:test` runner so this file needs no config in
// `vitest.config.ts` (which only globs `src/**`), mirroring
// `scripts/__tests__/coverage-diff.test.mjs`.
//
// Run with:
//   node --test scripts/__tests__/ci-flake-analyze.test.mjs
//
// Every expectation below is a LITERAL. None is derived from a constant the
// module under test exports — a test written against its own constant pins
// nothing.

import test from "node:test";
import assert from "node:assert/strict";

import {
  normalizeLogLine,
  hasCargoTestOutput,
  parseFailingTests,
  groupAttemptConclusions,
  groupJobConclusions,
  classifyRunsBySha,
  platformOfJobName,
  isRustTestJob,
  tallyFailingTests,
  evaluateProceedCriterion,
} from "../ci-flake-analyze.mjs";

// ---------------------------------------------------------------------------
// Fixtures — copied in shape from a real Actions log for
// `test (ubuntu-22.04)` on run 33021749396 attempt 2. GitHub prefixes every
// line with an ISO timestamp, which the parser must strip WITHOUT eating the
// indentation cargo's `failures:` block depends on.
// ---------------------------------------------------------------------------

const TS = "2026-08-27T07:26:07.5955615Z ";

/** A `failures:` summary block naming two tests. */
const FAILURES_BLOCK_LOG = [
  `${TS}running 34 tests`,
  `${TS}test tests::rpc_error_envelope_shape ... ok`,
  `${TS}`,
  `${TS}failures:`,
  `${TS}`,
  `${TS}---- tests::read_spill_says_when_retention_is_the_reason_a_locator_died stdout ----`,
  `${TS}thread 'tests::read_spill_says_when_retention_is_the_reason_a_locator_died' panicked at src-tauri/src/bin/wrappers_mcp.rs:1918:9:`,
  `${TS}assertion \`left == right\` failed`,
  `${TS}`,
  `${TS}`,
  `${TS}failures:`,
  `${TS}    tests::read_spill_says_when_retention_is_the_reason_a_locator_died`,
  `${TS}    tests::spill_eviction_prefers_the_older_record`,
  `${TS}`,
  `${TS}test result: FAILED. 32 passed; 2 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.08s`,
].join("\n");

/** Only the inline `... FAILED` shape — no summary block at all. */
const INLINE_FAILED_LOG = [
  `${TS}running 3 tests`,
  `${TS}test foo::bar ... FAILED`,
  `${TS}test foo::baz ... ok`,
  `${TS}test qux::quux ... FAILED`,
  `${TS}test result: FAILED. 1 passed; 2 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s`,
].join("\n");

/** A job that died before cargo ever produced test output (link OOM). */
const NO_CARGO_OUTPUT_LOG = [
  `${TS}##[group]Run cd src-tauri`,
  `${TS}   Compiling qontinui-runner v1.0.10`,
  `${TS}collect2: fatal error: ld terminated with signal 9 [Killed]`,
  `${TS}##[error]Process completed with exit code 101.`,
].join("\n");

// ---------------------------------------------------------------------------
// normalizeLogLine / hasCargoTestOutput
// ---------------------------------------------------------------------------

test("normalizeLogLine strips the ISO timestamp but keeps indentation", () => {
  assert.equal(
    normalizeLogLine(`${TS}    tests::alpha`),
    "    tests::alpha",
  );
  assert.equal(normalizeLogLine(`${TS}failures:`), "failures:");
});

test("hasCargoTestOutput is true for a cargo run and false for a link failure", () => {
  assert.equal(
    hasCargoTestOutput(FAILURES_BLOCK_LOG.split("\n").map(normalizeLogLine)),
    true,
  );
  assert.equal(
    hasCargoTestOutput(NO_CARGO_OUTPUT_LOG.split("\n").map(normalizeLogLine)),
    false,
  );
});

// ---------------------------------------------------------------------------
// parseFailingTests
// ---------------------------------------------------------------------------

test("a `failures:` block with 2 tests yields both names", () => {
  const r = parseFailingTests(FAILURES_BLOCK_LOG);
  assert.deepEqual(r.tests, [
    "tests::read_spill_says_when_retention_is_the_reason_a_locator_died",
    "tests::spill_eviction_prefers_the_older_record",
  ]);
  assert.equal(r.recognized, true);
  assert.equal(r.unparsed, false);
});

test("the stdout-dump `failures:` heading contributes no names", () => {
  // The first `failures:` in FAILURES_BLOCK_LOG heads `---- … stdout ----`,
  // which is NOT an indented bare test path. If the scanner mistook it for the
  // list, `----` would show up as a test name.
  const r = parseFailingTests(FAILURES_BLOCK_LOG);
  assert.equal(r.tests.length, 2);
  assert.equal(
    r.tests.some((t) => t.startsWith("-")),
    false,
  );
});

test("inline `test <name> ... FAILED` lines are extracted", () => {
  const r = parseFailingTests(INLINE_FAILED_LOG);
  assert.deepEqual(r.tests, ["foo::bar", "qux::quux"]);
  assert.equal(r.recognized, true);
  assert.equal(r.unparsed, false);
});

test("a log with no recognisable cargo output is UNPARSED, not 'no failures'", () => {
  const r = parseFailingTests(NO_CARGO_OUTPUT_LOG);
  assert.deepEqual(r.tests, []);
  // The unparsed signal is the point of this case — an empty list alone would
  // be indistinguishable from a clean run.
  assert.equal(r.unparsed, true);
  assert.equal(r.recognized, false);
  assert.equal(r.reason, "no recognisable cargo test output");
});

test("an empty log is UNPARSED", () => {
  const r = parseFailingTests("");
  assert.deepEqual(r.tests, []);
  assert.equal(r.unparsed, true);
  assert.equal(r.reason, "empty log");
});

test("the same test named by both shapes is counted once", () => {
  const both = [
    `${TS}running 1 test`,
    `${TS}test foo::bar ... FAILED`,
    `${TS}`,
    `${TS}failures:`,
    `${TS}    foo::bar`,
    `${TS}`,
    `${TS}test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s`,
  ].join("\n");
  assert.deepEqual(parseFailingTests(both).tests, ["foo::bar"]);
});

// ---------------------------------------------------------------------------
// groupAttemptConclusions — the class-1 verdict
// ---------------------------------------------------------------------------

test("attempts [success, failure, success] at one SHA is a class-1 disagreement", () => {
  // This is the shape of run 33021749396 at head_sha a3c3307d84.
  const r = groupAttemptConclusions([
    { attempt: 1, conclusion: "success", status: "completed" },
    { attempt: 2, conclusion: "failure", status: "completed" },
    { attempt: 3, conclusion: "success", status: "completed" },
  ]);
  assert.equal(r.disagreement, true);
  assert.deepEqual(r.distinct, ["failure", "success"]);
  assert.deepEqual(r.conclusions, ["success", "failure", "success"]);
  assert.equal(r.unparsed, false);
});

test("attempts [failure, failure] is NOT a disagreement — a deterministic break is not a flake", () => {
  const r = groupAttemptConclusions([
    { attempt: 1, conclusion: "failure", status: "completed" },
    { attempt: 2, conclusion: "failure", status: "completed" },
  ]);
  assert.equal(r.disagreement, false);
  assert.deepEqual(r.distinct, ["failure"]);
});

test("a single successful attempt is neither a disagreement nor unparsed", () => {
  const r = groupAttemptConclusions([
    { attempt: 1, conclusion: "success", status: "completed" },
  ]);
  assert.equal(r.disagreement, false);
  assert.equal(r.unparsed, false);
});

test("an attempt with a null conclusion is UNPARSED, never counted as agreement", () => {
  const r = groupAttemptConclusions([
    { attempt: 1, conclusion: "success", status: "completed" },
    { attempt: 2, conclusion: null, status: "in_progress" },
  ]);
  assert.equal(r.unparsed, true);
  assert.equal(r.disagreement, false);
  assert.equal(r.reason, "1 attempt(s) with no conclusion yet");
});

test("[cancelled, success] is NOT a disagreement — a cancellation is a non-answer", () => {
  // Counting `cancelled` as a second conclusion would inflate the strongest
  // evidence class with events that carry no evidence at all.
  const r = groupAttemptConclusions([
    { attempt: 1, conclusion: "cancelled", status: "completed" },
    { attempt: 2, conclusion: "success", status: "completed" },
  ]);
  assert.equal(r.disagreement, false);
  assert.deepEqual(r.distinct, ["success"]);
  assert.equal(r.unparsed, true);
  assert.equal(r.nonTerminal, 1);
  assert.equal(r.reason, "1 attempt(s) cancelled/skipped (no verdict)");
});

test("[cancelled, failure, success] IS a disagreement on the two real verdicts", () => {
  const r = groupAttemptConclusions([
    { attempt: 1, conclusion: "cancelled", status: "completed" },
    { attempt: 2, conclusion: "failure", status: "completed" },
    { attempt: 3, conclusion: "success", status: "completed" },
  ]);
  assert.equal(r.disagreement, true);
  assert.deepEqual(r.distinct, ["failure", "success"]);
  assert.equal(r.unparsed, true);
  assert.equal(r.nonTerminal, 1);
});

test("[skipped, success] is NOT a disagreement", () => {
  const r = groupAttemptConclusions([
    { attempt: 1, conclusion: "skipped", status: "completed" },
    { attempt: 2, conclusion: "success", status: "completed" },
  ]);
  assert.equal(r.disagreement, false);
  assert.equal(r.nonTerminal, 1);
});

test("no attempts at all is UNPARSED", () => {
  const r = groupAttemptConclusions([]);
  assert.equal(r.unparsed, true);
  assert.equal(r.disagreement, false);
  assert.equal(r.reason, "no attempts enumerated");
});

// ---------------------------------------------------------------------------
// Platform helpers
// ---------------------------------------------------------------------------

test("platformOfJobName reads the matrix leg out of the job name", () => {
  assert.equal(platformOfJobName("test (ubuntu-22.04)"), "ubuntu-22.04");
  assert.equal(platformOfJobName("test (windows-latest)"), "windows-latest");
  assert.equal(platformOfJobName("security"), "unknown");
});

test("isRustTestJob matches only the Rust test matrix jobs", () => {
  assert.equal(isRustTestJob("test (ubuntu-22.04)"), true);
  assert.equal(isRustTestJob("test (windows-latest)"), true);
  assert.equal(isRustTestJob("Frontend unit tests (vitest)"), false);
  assert.equal(isRustTestJob("Clippy (windows)"), false);
});

// ---------------------------------------------------------------------------
// groupJobConclusions — the per-platform half of class 1
// ---------------------------------------------------------------------------

test("groupJobConclusions ignores a cancelled job conclusion", () => {
  const split = groupJobConclusions([
    { attempt: 1, jobs: [{ name: "test (ubuntu-22.04)", conclusion: "cancelled" }] },
    { attempt: 2, jobs: [{ name: "test (ubuntu-22.04)", conclusion: "success" }] },
  ]);
  assert.deepEqual(split, [
    {
      name: "test (ubuntu-22.04)",
      platform: "ubuntu-22.04",
      conclusions: ["cancelled", "success"],
      distinct: ["success"],
      disagreement: false,
    },
  ]);
});

test("groupJobConclusions isolates the leg that actually disagreed", () => {
  const split = groupJobConclusions([
    {
      attempt: 1,
      jobs: [
        { name: "test (ubuntu-22.04)", conclusion: "success" },
        { name: "test (windows-latest)", conclusion: "success" },
      ],
    },
    {
      attempt: 2,
      jobs: [
        { name: "test (ubuntu-22.04)", conclusion: "failure" },
        { name: "test (windows-latest)", conclusion: "success" },
      ],
    },
  ]);
  assert.deepEqual(
    split.map((j) => [j.name, j.platform, j.disagreement]),
    [
      ["test (ubuntu-22.04)", "ubuntu-22.04", true],
      ["test (windows-latest)", "windows-latest", false],
    ],
  );
});

// ---------------------------------------------------------------------------
// classifyRunsBySha
// ---------------------------------------------------------------------------

test("classifyRunsBySha finds a within-run disagreement and leaves clean SHAs alone", () => {
  const verdicts = classifyRunsBySha([
    {
      id: 33021749396,
      head_sha: "a3c3307d8466666c02df9f1fad09f5a8dab8d547",
      head_branch: "main",
      event: "push",
      attempts: [
        { attempt: 1, conclusion: "success" },
        { attempt: 2, conclusion: "failure" },
        { attempt: 3, conclusion: "success" },
      ],
    },
    {
      id: 1,
      head_sha: "deadbeef",
      head_branch: "main",
      event: "push",
      attempts: [{ attempt: 1, conclusion: "success" }],
    },
  ]);
  const flagged = verdicts.filter((v) => v.disagreement);
  assert.equal(flagged.length, 1);
  assert.equal(flagged[0].headSha, "a3c3307d8466666c02df9f1fad09f5a8dab8d547");
  assert.deepEqual(flagged[0].runIds, [33021749396]);
  assert.equal(flagged[0].withinRun, true);
  assert.deepEqual(flagged[0].distinct, ["failure", "success"]);
});

test("classifyRunsBySha also catches disagreement ACROSS two runs at one SHA", () => {
  const verdicts = classifyRunsBySha([
    {
      id: 10,
      head_sha: "cafe",
      head_branch: "main",
      event: "push",
      attempts: [{ attempt: 1, conclusion: "success" }],
    },
    {
      id: 11,
      head_sha: "cafe",
      head_branch: "main",
      event: "push",
      attempts: [{ attempt: 1, conclusion: "failure" }],
    },
  ]);
  assert.equal(verdicts.length, 1);
  assert.equal(verdicts[0].disagreement, true);
  assert.equal(verdicts[0].withinRun, false);
  assert.deepEqual(verdicts[0].runIds, [10, 11]);
});

// ---------------------------------------------------------------------------
// tallyFailingTests
// ---------------------------------------------------------------------------

test("tallyFailingTests counts per test and splits by platform", () => {
  const tally = tallyFailingTests([
    { platform: "ubuntu-22.04", tests: ["a::x", "b::y"] },
    { platform: "ubuntu-22.04", tests: ["a::x"] },
    { platform: "windows-latest", tests: ["a::x"] },
  ]);
  assert.deepEqual(tally, [
    { test: "a::x", total: 3, byPlatform: { "ubuntu-22.04": 2, "windows-latest": 1 } },
    { test: "b::y", total: 1, byPlatform: { "ubuntu-22.04": 1 } },
  ]);
});

// ---------------------------------------------------------------------------
// evaluateProceedCriterion
// ---------------------------------------------------------------------------

test("arm A alone (2 distinct disagreeing tests) meets the criterion", () => {
  const r = evaluateProceedCriterion({
    distinctTestsInDisagreement: 2,
    maxOccurrences: 1,
    unparsed: 0,
  });
  assert.equal(r.met, true);
  assert.equal(r.verdict, "MET");
});

test("arm B alone (one test seen 3 times) meets the criterion", () => {
  const r = evaluateProceedCriterion({
    distinctTestsInDisagreement: 0,
    maxOccurrences: 3,
    unparsed: 0,
  });
  assert.equal(r.met, true);
  assert.equal(r.verdict, "MET");
});

test("a clean window with zero unparsed buckets is NOT MET", () => {
  const r = evaluateProceedCriterion({
    distinctTestsInDisagreement: 1,
    maxOccurrences: 2,
    unparsed: 0,
  });
  assert.equal(r.met, false);
  assert.equal(r.ambiguous, false);
  assert.equal(r.verdict, "NOT MET");
});

test("unparsed buckets downgrade a negative to ambiguous, never to a clean NOT MET", () => {
  const r = evaluateProceedCriterion({
    distinctTestsInDisagreement: 1,
    maxOccurrences: 2,
    unparsed: 4,
  });
  assert.equal(r.met, false);
  assert.equal(r.ambiguous, true);
  assert.equal(r.verdict, "MET-AMBIGUOUSLY (not met over what could be read)");
});

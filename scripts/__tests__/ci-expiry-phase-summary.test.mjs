#!/usr/bin/env node
// Unit tests for the PURE half of ci-expiry-phase-summary.mjs (Phase 2 of plan
// `2026-09-17-the-windows-test-gate-is-a-90-minute-build-wearing-a-test-shaped-bound`).
//
// The properties pinned here are the ones a wrong summariser gets wrong, which
// is the same discipline `scripts/tests/test_ci_timeout_marker.py` applies to
// this repo's job-level marker: every arm must route to the right phase, and NO
// arm may turn a missing measurement into a verdict.
//
// Uses Node's built-in `node:test` runner, mirroring
// `scripts/__tests__/ci-test-results-ingest.test.mjs`.
//
// Run with:
//   node --test scripts/__tests__/ci-expiry-phase-summary.test.mjs

import test from "node:test";
import assert from "node:assert/strict";

import { mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import {
  classifyExpiry,
  describeLog,
  lastRustcCrate,
  renderSummary,
  tallyTestResults,
} from "../ci-expiry-phase-summary.mjs";

const CLI = join(dirname(fileURLToPath(import.meta.url)), "..", "ci-expiry-phase-summary.mjs");

// ---------------------------------------------------------------------------
// Fixtures — the three real shapes, written as the tee'd file actually looks.
//
// NOTE the deliberate ABSENCE of ISO timestamps. GitHub's Actions log service
// adds those at SERVE time; `tee` never sees them. A fixture that carried them
// would be testing a file this script never reads. (`normalizeLogLine` strips
// them anyway, so one is included in the build fixture to prove that.)
// ---------------------------------------------------------------------------

/// Expired mid-compile: cargo --verbose rustc invocations, no `error:` line,
/// and no run-phase output at all. This is run 35043646051 attempt 1's shape.
const BUILD_EXPIRED_LOG = [
  "2026-09-16T01:33:20.1Z    Compiling qontinui-types v0.1.0",
  "     Running `/usr/bin/rustc --crate-name qontinui_types --edition=2021 --emit=dep-info,metadata,link`",
  "     Running `/usr/bin/rustc --crate-name qontinui_runner --edition=2021 --emit=dep-info,link --test`",
].join("\n");

/// A genuine compile error — same phase, opposite remedy.
const BUILD_COMPILE_ERROR_LOG = [
  "     Running `/usr/bin/rustc --crate-name qontinui_runner --edition=2021 --test`",
  "error[E0425]: cannot find value `nope` in this scope",
  "error: could not compile `qontinui-runner` (lib test) due to 1 previous error",
].join("\n");

/// The OOM signature the CARGO_BUILD_JOBS throttle exists for.
const BUILD_OOM_LOG = [
  "     Running `/usr/bin/rustc --crate-name qontinui_runner --edition=2021 --test`",
  "rustc-LLVM ERROR: out of memory",
].join("\n");

/// The suite reported and everything passed — #1545's own run log, in miniature.
const RUN_ALL_GREEN_LOG = [
  "     Running `target/debug/deps/qontinui_runner_lib-a9341426b1692ff6`",
  "running 1354 tests",
  "test some::path ... ok",
  "test result: ok. 1354 passed; 0 failed; 6 ignored; 0 measured; 0 filtered out; finished in 117.72s",
].join("\n");

/// A real test failure.
const RUN_RED_LOG = [
  "running 2 tests",
  "test some::path ... FAILED",
  "test result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.2s",
].join("\n");

/// Truncated / killed before the harness reported anything. THE arm that must
/// come back UNKNOWN rather than being collapsed into either neighbour.
const RUN_TRUNCATED_LOG = ["     Running `target/debug/deps/foo-0123456789abcdef`"].join("\n");

// ---------------------------------------------------------------------------
// The primitives
// ---------------------------------------------------------------------------

test("lastRustcCrate returns the LAST crate rustc was invoked on", () => {
  const lines = BUILD_EXPIRED_LOG.split("\n");
  assert.equal(lastRustcCrate(lines), "qontinui_runner");
});

test("lastRustcCrate returns null when no rustc invocation is present", () => {
  assert.equal(lastRustcCrate(RUN_ALL_GREEN_LOG.split("\n")), null);
});

test("tallyTestResults counts binaries and notices a FAILED one", () => {
  assert.deepEqual(tallyTestResults(RUN_ALL_GREEN_LOG.split("\n")), {
    binaries: 1,
    anyFailed: false,
  });
  assert.deepEqual(tallyTestResults(RUN_RED_LOG.split("\n")), {
    binaries: 1,
    anyFailed: true,
  });
});

test("describeLog distinguishes UNREADABLE from empty-but-read", () => {
  const unreadable = describeLog(null);
  assert.equal(unreadable.readable, false);
  const empty = describeLog("");
  assert.equal(empty.readable, true);
  assert.equal(empty.recognisedAsRun, false);
});

// ---------------------------------------------------------------------------
// The arms — one per phase, plus the two that must abstain
// ---------------------------------------------------------------------------

/// Build the `classifyExpiry` input from two raw log bodies. Split out from
/// `classify` so a mutation proof can feed the SAME input to the real
/// classifier and to a mutant.
function describeAll(buildText, runText, buildOutcome, runOutcome) {
  return {
    buildLog: describeLog(buildText),
    runLog: describeLog(runText),
    buildOutcome,
    runOutcome,
  };
}

function classify(buildText, runText, buildOutcome, runOutcome) {
  return classifyExpiry(describeAll(buildText, runText, buildOutcome, runOutcome));
}

test("a build expiry names the BUILD phase and the crate in flight", () => {
  const v = classify(BUILD_EXPIRED_LOG, "", "failure", "skipped");
  assert.equal(v.phase, "build");
  const text = renderSummary(v);
  assert.match(text, /qontinui_runner/);
  // No compiler error line ⇒ say TIMEOUT-shaped, and say why the job log is
  // where the literal timeout message lives.
  assert.match(text, /timeout-minutes` expiring mid-compile/);
  assert.doesNotMatch(text, /COMPILE ERROR/);
});

test("a build COMPILE ERROR is not reported as a bound expiry", () => {
  const v = classify(BUILD_COMPILE_ERROR_LOG, "", "failure", "skipped");
  assert.equal(v.phase, "build");
  const text = renderSummary(v);
  assert.match(text, /COMPILE ERROR/);
  assert.match(text, /the timeout is not implicated/);
});

test("a build OOM names the throttle's own revert ladder", () => {
  const v = classify(BUILD_OOM_LOG, "", "failure", "skipped");
  assert.equal(v.phase, "build");
  assert.match(renderSummary(v), /out-of-memory/);
  assert.match(renderSummary(v), /32 GB pagefile/);
});

test("the two build fixtures produce DIFFERENT text — the phase is not hard-coded", () => {
  const expired = renderSummary(classify(BUILD_EXPIRED_LOG, "", "failure", "skipped"));
  const broken = renderSummary(classify(BUILD_COMPILE_ERROR_LOG, "", "failure", "skipped"));
  assert.notEqual(expired, broken);
});

test("a run failure with an all-green log is reported as the post-report kill it is", () => {
  const v = classify(BUILD_EXPIRED_LOG, RUN_ALL_GREEN_LOG, "success", "failure");
  assert.equal(v.phase, "run");
  const text = renderSummary(v);
  assert.match(text, /1 binary result line\(s\), all `ok`, and the step still failed/);
  // The #1545 shape: never call a red step's all-green log a passing suite.
  assert.doesNotMatch(text, /the tests passed/);
});

test("a run failure with a FAILED test says the bound is not implicated", () => {
  const v = classify(BUILD_EXPIRED_LOG, RUN_RED_LOG, "success", "failure");
  assert.equal(v.phase, "run");
  assert.match(renderSummary(v), /A real test failure/);
});

test("a TRUNCATED run log is UNKNOWN, not 'passed' and not 'failed'", () => {
  const v = classify(BUILD_EXPIRED_LOG, RUN_TRUNCATED_LOG, "success", "failure");
  assert.equal(v.phase, "run");
  const text = renderSummary(v);
  assert.match(text, /\*\*UNKNOWN\*\*/);
  assert.match(text, /not evidence that the tests passed/);
});

test("an UNREADABLE step outcome is UNKNOWN — never read as success", () => {
  for (const [b, r] of [
    ["", "failure"],
    ["${{ steps.build_rust_tests.outcome }}", "failure"],
    ["success", undefined],
  ]) {
    const v = classify(BUILD_EXPIRED_LOG, RUN_ALL_GREEN_LOG, b, r);
    assert.equal(v.phase, "unknown", `outcomes build=${b} run=${r} should be UNKNOWN`);
    assert.match(renderSummary(v), /statement of UNKNOWN/);
  }
});

test("both steps green means this job failed somewhere else, and says so", () => {
  const v = classify(BUILD_EXPIRED_LOG, RUN_ALL_GREEN_LOG, "success", "success");
  assert.equal(v.phase, "elsewhere");
  assert.match(renderSummary(v), /Nothing here is a verdict on the Rust suite/);
});

// ---------------------------------------------------------------------------
// MUTATION PROOFS — each one runs the REAL classifier against a MUTATED input
// or a mutated wrapper and asserts the suite's own assertion would catch it.
//
// The two tests that stood here until 2026-09-19 were tautologies: they built a
// local stub, asserted the stub did what it was written to do, and then
// re-asserted something an earlier test already covered. A mutation proof that
// never runs the real implementation proves nothing and inflates the test
// count. These replace them.
// ---------------------------------------------------------------------------

/// The mutation: "hard-code the phase". Modelled as a wrapper that discards the
/// inputs and always answers BUILD-expired — then the suite's own
/// differing-fixture assertion is RE-RUN against it and must fail.
function hardCodedClassifier() {
  return { phase: "build", title: "Rust test phase: BUILD", lines: ["expired mid-compile"] };
}

/// The mutation: "collapse UNKNOWN into the compile-expiry arm".
function unknownCollapsingClassifier(input) {
  const real = classifyExpiry(input);
  return {
    ...real,
    lines: real.lines.map((l) =>
      l.includes("UNKNOWN") ? "the step's `timeout-minutes` expiring mid-compile" : l,
    ),
  };
}

/// Run one assertion and report whether it threw. This is what lets a mutation
/// proof assert "the suite CATCHES this", rather than asserting a stub.
function caught(fn) {
  try {
    fn();
    return false;
  } catch {
    return true;
  }
}

test("MUTATION hard-coded phase: the differing-fixture assertion catches it", () => {
  const assertion = (classifier) => {
    assert.notEqual(
      renderSummary(classifier(describeAll(BUILD_EXPIRED_LOG, "", "failure", "skipped"))),
      renderSummary(classifier(describeAll(BUILD_COMPILE_ERROR_LOG, "", "failure", "skipped"))),
    );
  };
  // The real implementation passes it…
  assert.equal(caught(() => assertion(classifyExpiry)), false);
  // …and the mutant is caught by that same assertion.
  assert.equal(caught(() => assertion(hardCodedClassifier)), true);
});

test("MUTATION UNKNOWN-collapse: the truncated-run assertion catches it", () => {
  const assertion = (classifier) => {
    const text = renderSummary(
      classifier(describeAll(BUILD_EXPIRED_LOG, RUN_TRUNCATED_LOG, "success", "failure")),
    );
    assert.match(text, /\*\*UNKNOWN\*\*/);
    assert.doesNotMatch(text, /timeout-minutes` expiring mid-compile/);
  };
  assert.equal(caught(() => assertion(classifyExpiry)), false);
  assert.equal(caught(() => assertion(unknownCollapsingClassifier)), true);
});

// ---------------------------------------------------------------------------
// The two abstention arms a review found the first cut got wrong.
// ---------------------------------------------------------------------------

test("a READABLE build log with no rustc invocation at all ABSTAINS", () => {
  // Empty is at least as consistent with the step dying before cargo ran (a
  // failed `cd`, a missing cargo, a full disk, an immediate kill) as with a
  // mid-compile expiry. The first cut called it "a SLOW BUILD, not a broken
  // one" with full confidence.
  const v = classify("", "", "failure", "skipped");
  const text = renderSummary(v);
  assert.match(text, /no rustc invocation at all, so \*\*UNKNOWN\*\*/);
  assert.doesNotMatch(text, /Treat this as a SLOW BUILD/);
});

test("an UNREADABLE run log is not reported as 'no test ever executed'", () => {
  // `recognisedAsRun: false` covers BOTH "read, no test output" and "could not
  // be read"; describeLog's own contract says never to collapse them.
  const v = classify(BUILD_EXPIRED_LOG, null, "failure", "skipped");
  const text = renderSummary(v);
  assert.match(text, /run log could not be read, so whether anything executed is \*\*UNKNOWN\*\*/);
  assert.doesNotMatch(text, /No test ever executed/);
});

test("a READ-but-silent run log still says no test executed", () => {
  const v = classify(BUILD_EXPIRED_LOG, "", "failure", "skipped");
  assert.match(renderSummary(v), /No test ever executed/);
});

// ---------------------------------------------------------------------------
// The $GITHUB_STEP_SUMMARY append path, which no test covered.
// ---------------------------------------------------------------------------

test("the CLI appends its block to $GITHUB_STEP_SUMMARY and still exits 0", () => {
  const dir = mkdtempSync(join(tmpdir(), "expiry-summary-"));
  const summary = join(dir, "summary.md");
  writeFileSync(summary, "PRE-EXISTING\n", "utf8");
  const buildLog = join(dir, "build.log");
  writeFileSync(buildLog, BUILD_EXPIRED_LOG, "utf8");

  const r = spawnSync(
    process.execPath,
    [CLI, "--build-log", buildLog, "--run-log", join(dir, "absent.log"),
     "--build-outcome", "failure", "--run-outcome", "skipped"],
    { env: { ...process.env, GITHUB_STEP_SUMMARY: summary }, encoding: "utf8" },
  );

  assert.equal(r.status, 0, "the summariser must always exit 0");
  const written = readFileSync(summary, "utf8");
  assert.match(written, /^PRE-EXISTING$/m, "it must APPEND, never truncate");
  assert.match(written, /Rust test phase: BUILD/);
  assert.match(r.stdout, /Rust test phase: BUILD/, "and always print to stdout too");
});

test("an UNWRITABLE $GITHUB_STEP_SUMMARY degrades to a warning, never a failure", () => {
  const r = spawnSync(
    process.execPath,
    [CLI, "--build-outcome", "failure", "--run-outcome", "skipped"],
    {
      env: { ...process.env, GITHUB_STEP_SUMMARY: "/nonexistent-dir/summary.md" },
      encoding: "utf8",
    },
  );
  assert.equal(r.status, 0);
  assert.match(r.stdout, /::warning title=ci-expiry-phase-summary::/);
  assert.match(r.stdout, /Rust test phase: BUILD/);
});

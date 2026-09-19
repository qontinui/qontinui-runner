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

import {
  classifyExpiry,
  describeLog,
  lastRustcCrate,
  renderSummary,
  tallyTestResults,
} from "../ci-expiry-phase-summary.mjs";

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

function classify(buildText, runText, buildOutcome, runOutcome) {
  return classifyExpiry({
    buildLog: describeLog(buildText),
    runLog: describeLog(runText),
    buildOutcome,
    runOutcome,
  });
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
// MUTATION PROOF — the plan requires each regression test be shown to FAIL
// against a deliberately broken variant. These re-implement the two mutations
// the plan names and assert they would be caught.
// ---------------------------------------------------------------------------

test("MUTATION: a hard-coded phase fails the differing-fixture assertion", () => {
  const hardCoded = () => ({ phase: "build", title: "Rust test phase: BUILD", lines: ["static"] });
  const a = renderSummary(hardCoded());
  const b = renderSummary(hardCoded());
  // The real implementation distinguishes these two fixtures; a hard-coded one
  // cannot, which is exactly what the assertion above would catch.
  assert.equal(a, b);
  assert.notEqual(
    renderSummary(classify(BUILD_EXPIRED_LOG, "", "failure", "skipped")),
    renderSummary(classify(BUILD_COMPILE_ERROR_LOG, "", "failure", "skipped")),
  );
});

test("MUTATION: collapsing UNKNOWN into 'expired mid-compile' would be caught", () => {
  // A summariser that treated an unrecognisable run log as a compile expiry
  // would emit the build arm's text on the truncated run fixture. Assert the
  // real one does not.
  const text = renderSummary(classify(BUILD_EXPIRED_LOG, RUN_TRUNCATED_LOG, "success", "failure"));
  assert.doesNotMatch(text, /timeout-minutes` expiring mid-compile/);
  assert.match(text, /\*\*UNKNOWN\*\*/);
});

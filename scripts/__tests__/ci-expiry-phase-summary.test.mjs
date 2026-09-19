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

/// ONE binary reported and its own tests all passed. Deliberately NOT "the
/// suite passed": on #1545 this exact shape sat two minutes before the kill
/// while the 9905-test binary was still running, and reading it as a passing
/// suite is the misreading this whole change exists to correct.
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

/// The mutation: "collapse UNKNOWN into the compile-expiry arm", as an OUTPUT
/// rewrite. Proves the assertion is sensitive to the token, not that the
/// decision logic is right — see `unknownBranchDeletedClassifier` for that.
function unknownCollapsingClassifier(input) {
  const real = classifyExpiry(input);
  return {
    ...real,
    lines: real.lines.map((l) =>
      l.includes("UNKNOWN") ? "the step's `timeout-minutes` expiring mid-compile" : l,
    ),
  };
}

/// The mutation that matters: DELETE the `!runLog.recognisedAsRun` abstention
/// branch, i.e. treat an unrecognisable run log as an all-green one. Modelled
/// at the DECISION level by lying to the real classifier about the one input
/// that branch reads — `recognisedAsRun` — which is what a deleted branch
/// amounts to. The classifier is pure, so this is exact rather than an
/// approximation.
function unknownBranchDeletedClassifier(input) {
  return classifyExpiry({
    ...input,
    runLog: { ...input.runLog, recognisedAsRun: true, binaries: 1, anyFailed: false },
  });
}

/// Run one assertion and report whether it failed AS AN ASSERTION.
///
/// Narrowed to `AssertionError` deliberately: a bare `catch` would score an
/// unrelated `TypeError` in a future mutant as "caught", so a mutation proof
/// could pass because the mutant was broken rather than because the suite
/// detected it. Anything that is not an assertion failure is re-thrown.
function caught(fn) {
  try {
    fn();
    return false;
  } catch (err) {
    if (err instanceof assert.AssertionError) return true;
    throw err;
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

test("MUTATION delete-the-abstention-branch: the truncated-run assertion catches it", () => {
  // The decision-level mutant, which the output-rewrite one above does not
  // cover: if the classifier stopped distinguishing an unrecognisable run log
  // from a reported one, the truncated fixture would be summarised as an
  // all-green run that failed anyway.
  const assertion = (classifier) => {
    const text = renderSummary(
      classifier(describeAll(BUILD_EXPIRED_LOG, RUN_TRUNCATED_LOG, "success", "failure")),
    );
    assert.match(text, /\*\*UNKNOWN\*\*/);
    assert.match(text, /not evidence that the tests passed/);
  };
  assert.equal(caught(() => assertion(classifyExpiry)), false);
  assert.equal(caught(() => assertion(unknownBranchDeletedClassifier)), true);
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
  assert.match(text, /carries no rustc invocation, no compiler `error:` and no OOM signature, so \*\*UNKNOWN\*\*/);
  assert.doesNotMatch(text, /Treat this as a SLOW BUILD/);
});

test("a SKIPPED run step is reported as skipped, NOT as UNKNOWN", () => {
  // The commonest build-failure path in CI: GitHub skips the run step, so
  // `cargo-test-output.log` is never written and the log read comes back
  // unreadable — while `runOutcome` says `skipped` in so many words. One
  // revision of this file answered UNKNOWN here, throwing away the fact it had
  // been handed. `skipped` is checked before readability for that reason.
  const v = classify(BUILD_EXPIRED_LOG, null, "failure", "skipped");
  const text = renderSummary(v);
  assert.match(text, /The run step was SKIPPED, so no test executed/);
  assert.doesNotMatch(text, /whether anything executed is \*\*UNKNOWN\*\*/);
});

test("an UNREADABLE run log whose step did NOT skip is genuinely UNKNOWN", () => {
  const v = classify(BUILD_EXPIRED_LOG, null, "failure", "cancelled");
  const text = renderSummary(v);
  assert.match(text, /could not be read and the run step did not report `skipped`/);
  assert.match(text, /\*\*UNKNOWN\*\*/);
  assert.doesNotMatch(text, /No test ever executed/);
});

test("a READ-but-silent run log still says no test executed", () => {
  const v = classify(BUILD_EXPIRED_LOG, "", "failure", "cancelled");
  assert.match(renderSummary(v), /No test ever executed/);
});

test("OOM and a cargo-level compile error BEAT the no-rustc abstention", () => {
  // The regression a review caught: ordering the abstain arm above these two
  // told a log containing `rustc-LLVM ERROR` that it "contains no rustc
  // invocation at all" — false, and it suppressed the single most actionable
  // sentence this script emits.
  const oomNoCrate = renderSummary(
    classify("rustc-LLVM ERROR: out of memory", "", "failure", "skipped"),
  );
  assert.match(oomNoCrate, /out-of-memory/);
  assert.match(oomNoCrate, /32 GB pagefile/);
  assert.doesNotMatch(oomNoCrate, /carries no rustc invocation/);

  const cargoError = renderSummary(
    classify("error: could not compile workspace", "", "failure", "skipped"),
  );
  assert.match(cargoError, /COMPILE ERROR/);
  assert.doesNotMatch(cargoError, /carries no rustc invocation/);
});

test("the abstention distinguishes an EMPTY build log from a one-line one", () => {
  // `"".split("\n")` is `[""]`, so lineCount alone reads 1 for both — which is
  // the exact distinction the abstain arm quotes it for. `describeLog.empty`
  // carries it instead.
  assert.match(
    renderSummary(classify("", "", "failure", "skipped")),
    /readable \(empty\)/,
  );
  assert.match(
    renderSummary(classify("some unrelated line", "", "failure", "skipped")),
    /readable \(1 line\(s\)\)/,
  );
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

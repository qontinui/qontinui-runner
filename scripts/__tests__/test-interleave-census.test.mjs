#!/usr/bin/env node
// Unit tests for the PURE half of test-interleave-census.mjs (Phase 0 of plan
// `2026-09-17-runner-tests-share-in-process-mutable-state`).
//
// Uses Node's built-in `node:test` runner, mirroring
// `scripts/__tests__/ci-flake-analyze.test.mjs`. Nothing here spawns cargo or
// a test binary: the runner layer takes an injectable `spawn`, and every
// spawn below is a stub that answers from a fixture.
//
// Run with:
//   node --test scripts/__tests__/test-interleave-census.test.mjs
//
// Every expectation below is a LITERAL. None is derived from a constant the
// module under test exports — a test written against its own constant pins
// nothing.

import test from "node:test";
import assert from "node:assert/strict";

import {
  binaryIdFromAnnouncementLine,
  binaryIdFromExecutablePath,
} from "../ci-flake-analyze.mjs";
import {
  CLASSIFICATION_TOKEN,
  LABEL,
  buildExecutableIndex,
  classificationTokenFor,
  classificationsFromReport,
  classifyFromLog,
  defaultSpawn,
  isByDesignNonAnswer,
  killTree,
  nonAnswersFor,
  diffFailureSets,
  escapeAnnotationMessage,
  exactNameFor,
  exitCodeFor,
  extractPanicText,
  foldRunResult,
  formatAnnotation,
  formatPretty,
  isDoctestName,
  labelFor,
  parseCargoBuildMessages,
  parseCliArgs,
  parseExeList,
  parseExecutableOutput,
  resolveTestId,
  runCensus,
  snapshotExecutables,
  splitTestId,
  synthesiseAnnouncement,
} from "../test-interleave-census.mjs";

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

const LIB_EXE =
  "/home/box/qontinui-runner/target-agent/debug/deps/qontinui_runner_lib-a9341426b1692ff6";
const BIN_EXE =
  "/home/box/qontinui-runner/target-agent/debug/deps/qontinui_runner-0123456789abcdef";
const WIN_EXE =
  "D:\\a\\qontinui-runner\\target\\debug\\deps\\qontinui_runner_lib-deadbeefcafef00d.exe";

/** What libtest prints on stdout when a binary is invoked directly. */
function libtestOutput(results, { panicFor = {} } = {}) {
  const lines = [`running ${results.length} tests`];
  for (const [name, outcome] of results) lines.push(`test ${name} ... ${outcome}`);
  const failed = results.filter(([, o]) => o === "FAILED");
  if (failed.length > 0) {
    lines.push("", "failures:", "");
    for (const [name] of failed) {
      lines.push(`---- ${name} stdout ----`);
      lines.push(
        panicFor[name] ??
          `thread '${name}' panicked at src-tauri/src/outbound_trace.rs:200:9:\nassertion \`left == right\` failed\n  left: 2\n right: 1`,
      );
      lines.push("note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace", "");
    }
    lines.push("", "failures:");
    for (const [name] of failed) lines.push(`    ${name}`);
    lines.push("");
  }
  const passed = results.filter(([, o]) => o === "ok").length;
  lines.push(
    `test result: ${failed.length ? "FAILED" : "ok"}. ${passed} passed; ${failed.length} failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.05s`,
  );
  return lines.join("\n");
}

function result(stdout, { code = 0, stderr = "", signal = null, timedOut = false } = {}) {
  return { stdout, stderr, code, signal, timedOut, spawnError: null };
}

const CARGO_JSON = [
  `{"reason":"compiler-artifact","package_id":"x","manifest_path":"/home/box/qontinui-runner/src-tauri/Cargo.toml","target":{"kind":["lib"],"name":"qontinui_runner_lib"},"profile":{"test":true},"executable":"${LIB_EXE}"}`,
  `{"reason":"compiler-artifact","package_id":"x","manifest_path":"/home/box/qontinui-runner/src-tauri/Cargo.toml","target":{"kind":["bin"],"name":"qontinui-runner"},"profile":{"test":true},"executable":"${BIN_EXE}"}`,
  // A non-test artifact (a build dependency) — must be ignored.
  `{"reason":"compiler-artifact","package_id":"y","manifest_path":"/home/box/dep/Cargo.toml","target":{"kind":["lib"],"name":"serde"},"profile":{"test":false},"executable":null}`,
  // A test-profile artifact with no executable (an rlib) — must be ignored.
  `{"reason":"compiler-artifact","package_id":"z","manifest_path":"/home/box/qontinui-runner/crates/runner-stats/Cargo.toml","target":{"kind":["lib"],"name":"runner_stats"},"profile":{"test":true},"executable":null}`,
  `{"reason":"build-finished","success":true}`,
  // A wrapper's chatter on stdout — must be skipped, not choke the parse.
  "[cargo-guard] Still waiting... (60s/2700s)",
].join("\n");

// ---------------------------------------------------------------------------
// The one normaliser — Linux path AND Windows .exe path, both doors
// ---------------------------------------------------------------------------

test("binaryIdFromExecutablePath strips the hash on a Linux path", () => {
  assert.equal(binaryIdFromExecutablePath(LIB_EXE), "qontinui_runner_lib");
  assert.equal(binaryIdFromExecutablePath(BIN_EXE), "qontinui_runner");
});

test("binaryIdFromExecutablePath strips .exe and the hash on a Windows path", () => {
  assert.equal(binaryIdFromExecutablePath(WIN_EXE), "qontinui_runner_lib");
});

test("binaryIdFromExecutablePath refuses a basename with no cargo hash", () => {
  assert.equal(binaryIdFromExecutablePath("/usr/local/bin/rustc"), undefined);
  assert.equal(binaryIdFromExecutablePath("/tmp/qontinui_runner_lib"), undefined);
});

test("binaryIdFromAnnouncementLine and binaryIdFromExecutablePath agree", () => {
  assert.equal(binaryIdFromAnnouncementLine(`Running \`${LIB_EXE}\``), "qontinui_runner_lib");
  assert.equal(binaryIdFromAnnouncementLine(`Running \`${WIN_EXE}\``), "qontinui_runner_lib");
  assert.equal(binaryIdFromAnnouncementLine("Doc-tests qontinui-runner"), "qontinui-runner");
  assert.equal(binaryIdFromAnnouncementLine("Running `/usr/bin/rustc --crate-name x`"), undefined);
});

// ---------------------------------------------------------------------------
// Parser reuse — a direct run's output gets the <binary>:: prefix
// ---------------------------------------------------------------------------

test("synthesiseAnnouncement is the line cargo would have printed", () => {
  assert.equal(synthesiseAnnouncement(LIB_EXE), `     Running \`${LIB_EXE}\``);
});

test("parseExecutableOutput yields <binary>::<path> ids through the shared parser", () => {
  const out = libtestOutput([
    ["outbound_trace::tests::ring_is_bounded", "ok"],
    ["outbound_trace::tests::drain_since_returns_only_traces_after_the_snapshot", "FAILED"],
    ["settings::tests::slow_one", "ignored"],
  ]);
  const parsed = parseExecutableOutput(LIB_EXE, out);
  assert.equal(parsed.unparsed, false);
  assert.deepEqual(parsed.tests, [
    {
      testId: "qontinui_runner_lib::outbound_trace::tests::drain_since_returns_only_traces_after_the_snapshot",
      outcome: "fail",
    },
    { testId: "qontinui_runner_lib::outbound_trace::tests::ring_is_bounded", outcome: "pass" },
    { testId: "qontinui_runner_lib::settings::tests::slow_one", outcome: "skip" },
  ]);
});

test("parseExecutableOutput on a Windows executable path prefixes the same id", () => {
  const parsed = parseExecutableOutput(WIN_EXE, libtestOutput([["a::b", "ok"]]));
  assert.deepEqual(parsed.tests, [{ testId: "qontinui_runner_lib::a::b", outcome: "pass" }]);
});

test("parseExecutableOutput is fail-closed on output with no libtest shape", () => {
  const parsed = parseExecutableOutput(LIB_EXE, "Segmentation fault (core dumped)\n");
  assert.equal(parsed.unparsed, true);
  assert.deepEqual(parsed.tests, []);
});

// ---------------------------------------------------------------------------
// Executable resolution
// ---------------------------------------------------------------------------

test("parseCargoBuildMessages keeps test-profile artifacts with an executable, cwd = manifest dir", () => {
  const { executables, skipped } = parseCargoBuildMessages(CARGO_JSON);
  assert.deepEqual(
    executables.map((e) => [e.binaryId, e.executable, e.cwd]),
    [
      ["qontinui_runner_lib", LIB_EXE, "/home/box/qontinui-runner/src-tauri"],
      ["qontinui_runner", BIN_EXE, "/home/box/qontinui-runner/src-tauri"],
    ],
  );
  assert.deepEqual(skipped, []);
});

test("parseCargoBuildMessages reports (not drops) an executable with no hash", () => {
  const { executables, skipped } = parseCargoBuildMessages(
    `{"reason":"compiler-artifact","manifest_path":"/m/Cargo.toml","target":{"kind":["bin"],"name":"x"},"profile":{"test":true},"executable":"/m/target/debug/x"}`,
  );
  assert.deepEqual(executables, []);
  assert.equal(skipped.length, 1);
  assert.equal(skipped[0].executable, "/m/target/debug/x");
  assert.match(skipped[0].reason, /no cargo metadata hash/);
});

test("parseExeList reads path[<TAB>cwd] lines, ignoring blanks and comments", () => {
  const { executables, skipped } = parseExeList(
    `# pre-resolved\n${LIB_EXE}\t/home/box/qontinui-runner/src-tauri\n\n${WIN_EXE}\n/tmp/unhashed\n`,
    { defaultCwd: "/ws" },
  );
  assert.deepEqual(
    executables.map((e) => [e.binaryId, e.cwd]),
    [
      ["qontinui_runner_lib", "/home/box/qontinui-runner/src-tauri"],
      ["qontinui_runner_lib", "/ws"],
    ],
  );
  assert.deepEqual(skipped.map((s) => s.executable), ["/tmp/unhashed"]);
});

test("buildExecutableIndex keeps the first executable per id and records collisions", () => {
  const { byId, collisions } = buildExecutableIndex([
    { executable: LIB_EXE, binaryId: "qontinui_runner_lib", cwd: null },
    { executable: WIN_EXE, binaryId: "qontinui_runner_lib", cwd: null },
    { executable: BIN_EXE, binaryId: "qontinui_runner", cwd: null },
  ]);
  assert.equal(byId.get("qontinui_runner_lib").executable, LIB_EXE);
  assert.deepEqual(collisions, [{ binaryId: "qontinui_runner_lib", executables: [LIB_EXE, WIN_EXE] }]);
});

test("splitTestId splits at the FIRST :: only", () => {
  assert.deepEqual(splitTestId("qontinui_runner_lib::a::b::c"), { binary: "qontinui_runner_lib", name: "a::b::c" });
  assert.deepEqual(splitTestId("a_bare_name"), { binary: null, name: "a_bare_name" });
  assert.deepEqual(splitTestId("::x"), { binary: null, name: "::x" });
});

test("isDoctestName recognises rustdoc's `<path> - <item> (line N)` shape", () => {
  assert.equal(isDoctestName("src-tauri/src/foo.rs - foo::Bar (line 12)"), true);
  assert.equal(isDoctestName("foo::tests::bar"), false);
  assert.equal(isDoctestName("foo::tests::line_counts_to_12"), false);
});

test("resolveTestId maps an id to its executable by the shared rule", () => {
  const index = buildExecutableIndex(parseCargoBuildMessages(CARGO_JSON).executables);
  const r = resolveTestId("qontinui_runner_lib::outbound_trace::tests::ring_is_bounded", index);
  assert.equal(r.executable, LIB_EXE);
  assert.equal(r.cwd, "/home/box/qontinui-runner/src-tauri");
  assert.equal(r.name, "outbound_trace::tests::ring_is_bounded");
  assert.equal(r.unresolvedReason, null);
});

test("resolveTestId labels a doctest UNRESOLVED with the reason, never an executable", () => {
  const index = buildExecutableIndex(parseCargoBuildMessages(CARGO_JSON).executables);
  const r = resolveTestId("qontinui-runner::src-tauri/src/foo.rs - foo::Bar (line 12)", index);
  assert.equal(r.executable, null);
  assert.match(r.unresolvedReason, /doctest/);
  assert.match(r.unresolvedReason, /Doc-tests qontinui-runner/);
});

test("resolveTestId hands --exact the bare name of a should_panic test", () => {
  const index = buildExecutableIndex(parseCargoBuildMessages(CARGO_JSON).executables);
  const r = resolveTestId("qontinui_runner_lib::a::it_panics - should panic", index);
  assert.equal(r.executable, LIB_EXE);
  assert.equal(r.name, "a::it_panics");
  assert.equal(exactNameFor("a::plain"), "a::plain");
});

test("resolveTestId says why an unknown binary or a bare id cannot be re-run", () => {
  const index = buildExecutableIndex(parseCargoBuildMessages(CARGO_JSON).executables);
  assert.match(resolveTestId("nope::a::b", index).unresolvedReason, /no built test executable normalises to `nope`/);
  assert.match(resolveTestId("bare_name", index).unresolvedReason, /no `<binary>::` prefix/);
});

// ---------------------------------------------------------------------------
// Panic text
// ---------------------------------------------------------------------------

test("extractPanicText returns the captured stdout block, timestamps stripped, backtrace hint dropped", () => {
  const TS = "2026-09-17T07:26:07.5955615Z ";
  const log = [
    `${TS}---- a::b stdout ----`,
    `${TS}thread 'a::b' panicked at src/a.rs:1:1:`,
    `${TS}assertion \`left == right\` failed`,
    `${TS}  left: 2`,
    `${TS} right: 1`,
    `${TS}note: run with \`RUST_BACKTRACE=1\` environment variable to display a backtrace`,
    `${TS}`,
    `${TS}---- c::d stdout ----`,
    `${TS}other`,
  ].join("\n");
  assert.equal(
    extractPanicText(log, "a::b"),
    "thread 'a::b' panicked at src/a.rs:1:1:\nassertion `left == right` failed\n  left: 2\n right: 1",
  );
  assert.equal(extractPanicText(log, "c::d"), "other");
  assert.equal(extractPanicText(log, "zz::zz"), null);
});

test("extractPanicText caps a very long block", () => {
  const long = `---- t stdout ----\n${"x".repeat(5000)}\n`;
  const got = extractPanicText(long, "t");
  assert.equal(got.length, 801);
  assert.ok(got.endsWith("…"));
});

// ---------------------------------------------------------------------------
// Fold, diff, label, exit
// ---------------------------------------------------------------------------

test("foldRunResult: a normal red run parses; an abort with no parsed failure is UNPARSED", () => {
  const ok = foldRunResult(LIB_EXE, result(libtestOutput([["a::b", "FAILED"]]), { code: 101 }));
  assert.deepEqual([...ok.outcomes], [["qontinui_runner_lib::a::b", "fail"]]);
  assert.equal(ok.reason, null);

  const abort = foldRunResult(LIB_EXE, result("running 3 tests\ntest a::b ... ok\n", { code: null, signal: "SIGSEGV" }));
  assert.equal(abort.outcomes, null);
  assert.match(abort.reason, /signal SIGSEGV/);
  assert.match(abort.reason, /died mid-way/);

  // A run that printed a FAILED and then died: it has "cargo test output" and a
  // parsed failure, and until 2026-09-21 it folded as a complete red run. The
  // tests after the abort were never judged, so it is a non-answer.
  const partial = foldRunResult(
    LIB_EXE,
    result("running 3 tests\ntest a::b ... FAILED\ntest a::c ... ok\n", { code: null, signal: "SIGABRT" }),
  );
  assert.equal(partial.outcomes, null, "a red run with no summary line must NOT count as a complete run");
  assert.match(partial.reason, /no test-result summary — the run died mid-way \(signal SIGABRT\)/);

  // …while the same lines WITH the summary are a complete run (exit 101 is libtest's own red).
  const complete = foldRunResult(
    LIB_EXE,
    result("running 2 tests\ntest a::b ... FAILED\ntest a::c ... ok\ntest result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s\n", { code: 101 }),
  );
  assert.equal(complete.reason, null);
  assert.equal(complete.outcomes.get("qontinui_runner_lib::a::b"), "fail");

  // The summary present, a non-zero non-libtest exit and no failure: still the
  // original "aborted" arm (a process that failed AFTER judging everything green).
  const weird = foldRunResult(
    LIB_EXE,
    result("running 1 test\ntest a::b ... ok\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s\n", { code: 3 }),
  );
  assert.equal(weird.outcomes, null);
  assert.match(weird.reason, /exit 3.*aborted mid-run/);

  const timeout = foldRunResult(LIB_EXE, result("", { code: null, timedOut: true }));
  assert.equal(timeout.outcomes, null);
  assert.match(timeout.reason, /timed out/);

  const nothing = foldRunResult(LIB_EXE, result("", { code: 0 }));
  assert.equal(nothing.outcomes, null);
  assert.match(nothing.reason, /no recognisable cargo test output/);
});

test("diffFailureSets separates intermittent from always-red and skips unparsed runs", () => {
  const runs = [
    new Map([["x::a", "pass"], ["x::b", "fail"], ["x::c", "pass"]]),
    new Map([["x::a", "fail"], ["x::b", "fail"], ["x::c", "pass"]]),
    null,
    new Map([["x::a", "pass"], ["x::b", "fail"], ["x::c", "pass"]]),
  ];
  const d = diffFailureSets(runs);
  assert.deepEqual(d.intermittent, ["x::a"]);
  assert.deepEqual(d.always, ["x::b"]);
  assert.deepEqual(d.everRed, ["x::a", "x::b"]);
  assert.deepEqual(d.perTest.get("x::a"), { seen: 3, failures: 1, failedInRuns: [2] });
  assert.deepEqual(d.perTest.get("x::b"), { seen: 3, failures: 3, failedInRuns: [1, 2, 4] });
  assert.deepEqual(d.perTest.get("x::c"), { seen: 3, failures: 0, failedInRuns: [] });
});

test("labelFor: the matrix", () => {
  const L = (o) => labelFor({ suiteRuns: 10, suiteFailures: 3, soloRuns: 3, soloFailures: 0, ...o }).label;
  assert.equal(L({}), "SUITE-ONLY");
  assert.equal(L({ soloFailures: 3 }), "SOLO-RED");
  assert.equal(L({ suiteFailures: 10, soloFailures: 3 }), "SOLO-RED");
  assert.equal(L({ soloFailures: 1 }), "BOTH-FLAKY");
  assert.equal(L({ soloFailures: 2 }), "BOTH-FLAKY");
  assert.equal(L({ suiteFailures: 0 }), "GREEN");
  assert.equal(L({ unresolvedReason: "doctest" }), "UNRESOLVED");
  assert.equal(L({ soloRuns: 0, soloFailures: 0 }), "UNRESOLVED");
  assert.equal(L({ soloRuns: 0, soloFailures: 0, budgetExhausted: true }), "UNRESOLVED (budget)");
  // UNPARSED wins over a would-be SUITE-ONLY: fail-closed.
  assert.equal(L({ soloUnparsed: 1, soloFailures: 0 }), "UNPARSED");
  // An unresolved id is never SOLO-RED, whatever the counts say.
  assert.equal(L({ unresolvedReason: "doctest", soloFailures: 3 }), "UNRESOLVED");
});

test("labelFor: every reason names the counts it was derived from", () => {
  const r = labelFor({ suiteRuns: 10, suiteFailures: 2, soloRuns: 3, soloFailures: 0 });
  assert.equal(r.reason, "red 2/10 in the suite, green 3/3 alone");
  const u = labelFor({ suiteRuns: 10, suiteFailures: 2, soloRuns: 3, soloFailures: 0, soloUnparsed: 2 });
  assert.match(u.reason, /2 of 3 solo re-run\(s\) produced no recognisable libtest output/);
});

const DOCTEST_REASON = "doctest — rustdoc compiles it per run; `cargo test --no-run` builds no executable for `Doc-tests x`";
const NO_PREFIX_REASON = "id carries no `<binary>::` prefix (the run's announcement line was missing), so no executable can be named for it";
const RESOLUTION_FAILED_REASON = "no built test executable normalises to `zzz` (resolution failed)";

test("exitCodeFor: 0 no reds, 1 any SUITE-ONLY, 2 unparsed (precedence)", () => {
  const rep = (labels, unparsed = []) => ({
    header: { unparsed },
    tests: Object.fromEntries(labels.map((l, i) => [`t${i}`, typeof l === "string" ? { label: l } : l])),
  });
  assert.equal(exitCodeFor(rep([])), 0);
  assert.equal(
    exitCodeFor(rep(["SOLO-RED", "BOTH-FLAKY", { label: "UNRESOLVED", reason: DOCTEST_REASON }])),
    0,
    "a by-design non-answer does not gate",
  );
  assert.equal(exitCodeFor(rep(["SOLO-RED", "SUITE-ONLY"])), 1);
  assert.equal(exitCodeFor(rep(["SUITE-ONLY"], [{ run: 1, executable: LIB_EXE, reason: "x" }])), 2);
  assert.equal(exitCodeFor(rep(["UNPARSED"])), 2);
  assert.equal(exitCodeFor(rep([], [{ run: 2, executable: LIB_EXE, reason: "timed out" }])), 2);
});

test("exitCodeFor: FAIL-CLOSED on every non-answer — budget, budget_exhausted, resolution failure — and 2 beats 1", () => {
  const rep = (tests, header = {}) => ({ header: { unparsed: [], ...header }, tests });
  // Until 2026-09-21 all three of these were exit 0 and the nightly printed "clean".
  assert.equal(exitCodeFor(rep({ a: { label: "UNRESOLVED (budget)", reason: "…" } })), 2, "budget");
  assert.equal(exitCodeFor(rep({}, { budget_exhausted: true })), 2, "header.budget_exhausted with nothing labelled");
  assert.equal(
    exitCodeFor(rep({ a: { label: "UNRESOLVED", reason: RESOLUTION_FAILED_REASON } })),
    2,
    "a resolution failure is a non-answer",
  );
  assert.equal(
    exitCodeFor(rep({ a: { label: "UNRESOLVED", reason: "solo re-run of `x` executed no test of that name (0 tests ran) in /e — …" } })),
    2,
    "an executable that ran no test of the name is a non-answer",
  );
  assert.equal(exitCodeFor(rep({ a: { label: "UNRESOLVED" } })), 2, "an UNRESOLVED with no reason is not assumed by-design");
  // By-design non-answers stay 0 (or 1 beside a SUITE-ONLY).
  assert.equal(exitCodeFor(rep({ a: { label: "UNRESOLVED", reason: NO_PREFIX_REASON } })), 0);
  assert.equal(
    exitCodeFor(rep({ a: { label: "UNRESOLVED", reason: DOCTEST_REASON }, b: { label: "SUITE-ONLY", reason: "…" } })),
    1,
  );
  // Precedence: a non-answer beside a SUITE-ONLY is 2 — the class may be larger than what was measured.
  assert.equal(
    exitCodeFor(rep({ a: { label: "SUITE-ONLY", reason: "…" }, b: { label: "UNRESOLVED (budget)", reason: "…" } })),
    2,
  );
  assert.equal(isByDesignNonAnswer(DOCTEST_REASON), true);
  assert.equal(isByDesignNonAnswer(NO_PREFIX_REASON), true);
  assert.equal(isByDesignNonAnswer(RESOLUTION_FAILED_REASON), false);
  assert.equal(isByDesignNonAnswer(null), false);
});

test("nonAnswersFor names each class that moved the verdict to UNKNOWN, and the pretty output prints it", () => {
  const report = {
    header: { mode: "census", unparsed: [{ run: 1, executable: LIB_EXE, reason: "budget passed before this run" }], budget_exhausted: true, executables: [], skipped_executables: [], collisions: [], runs: 1, solo_runs: 3, hostname: "h", started_at: "s", finished_at: "f", budget_seconds: 1 },
    tests: {
      a: { label: "UNRESOLVED (budget)", reason: "…", suite_runs: 1, suite_failures: 1, solo_runs: 0, solo_failures: 0 },
      b: { label: "UNRESOLVED", reason: RESOLUTION_FAILED_REASON, suite_runs: 1, suite_failures: 1, solo_runs: 0, solo_failures: 0 },
      c: { label: "UNRESOLVED", reason: DOCTEST_REASON, suite_runs: 1, suite_failures: 1, solo_runs: 0, solo_failures: 0 },
      d: { label: "UNPARSED", reason: "…", suite_runs: 1, suite_failures: 1, solo_runs: 1, solo_failures: 0 },
    },
    summary: { "SUITE-ONLY": 0, "SOLO-RED": 0, "BOTH-FLAKY": 0, UNRESOLVED: 2, "UNRESOLVED (budget)": 1, UNPARSED: 1 },
    exit_code: 2,
  };
  assert.deepEqual(nonAnswersFor(report), [
    "1 unparsed suite run(s)",
    "1 test(s) with an unparsed solo re-run",
    "1 test(s) never re-run — budget passed",
    "1 test(s) whose executable could not be resolved",
  ]);
  const pretty = formatPretty(report);
  assert.match(pretty, /NON-ANSWERS \(fail-closed — the verdict is UNKNOWN, not clean\): 1 unparsed suite run\(s\); .*could not be resolved/);
  assert.match(pretty, /budget 1s — EXHAUSTED/);
});

// ---------------------------------------------------------------------------
// Annotations
// ---------------------------------------------------------------------------

test("escapeAnnotationMessage encodes %, CR and LF", () => {
  assert.equal(escapeAnnotationMessage("a%b\r\nc"), "a%25b%0D%0Ac");
});

test("formatAnnotation: SUITE-ONLY is an ::error with the dossier and the panic", () => {
  const a = formatAnnotation("qontinui_runner_lib::x::y", {
    label: "SUITE-ONLY",
    solo_runs: 3,
    solo_failures: 0,
    reason: "red 1/1 in the suite, green 3/3 alone",
    sample_panic: "assertion `left == right` failed\n  left: 2",
  });
  assert.equal(
    a,
    "::error title=SUITE-ONLY::qontinui_runner_lib::x::y shares process state with a concurrent test — passes alone 3/3; see dossier runner-tests-share-in-process-mutable-state; panic: assertion `left == right` failed%0A  left: 2",
  );
});

test("formatAnnotation: SOLO-RED and BOTH-FLAKY are ::warning; non-answers are ::notice", () => {
  assert.match(
    formatAnnotation("x::y", { label: "SOLO-RED", solo_runs: 3, solo_failures: 3, reason: "r", sample_panic: "boom" }),
    /^::warning title=SOLO-RED::x::y fails alone 3\/3 — a real defect or an ambient read, not the shared-state class; panic: boom$/,
  );
  assert.match(
    formatAnnotation("x::y", { label: "BOTH-FLAKY", solo_runs: 3, solo_failures: 1, reason: "r", sample_panic: null }),
    /^::warning title=BOTH-FLAKY::x::y fails in both arms \(alone 1\/3\) — timing, not the shared-state class$/,
  );
  assert.match(
    formatAnnotation("x::y", { label: "UNRESOLVED (budget)", solo_runs: 0, solo_failures: 0, reason: "the budget passed", sample_panic: null }),
    /^::notice title=UNRESOLVED \(budget\)::x::y UNRESOLVED \(budget\): the budget passed$/,
  );
});

// ---------------------------------------------------------------------------
// The runner layer with a stubbed spawn
// ---------------------------------------------------------------------------

const EXES = parseCargoBuildMessages(CARGO_JSON).executables;
/** A stable fingerprint — the fixture paths do not exist on disk. */
const FP = (path) => `sha-of-${path}`;
const RING = "outbound_trace::tests::drain_since_returns_only_traces_after_the_snapshot";
const RING_ID = `qontinui_runner_lib::${RING}`;

/** A spawn stub: suite runs answer from `suiteByRun[exe][run]`, solo re-runs from `solo(exe, name, k)`. */
function stubSpawn({ suiteByRun, solo }) {
  const calls = [];
  const suiteCount = {};
  const soloCount = {};
  const spawn = async (exe, args, opts) => {
    calls.push({ exe, args, opts });
    if (args.length === 0) {
      suiteCount[exe] = (suiteCount[exe] ?? 0) + 1;
      const r = suiteByRun[exe]?.[suiteCount[exe] - 1];
      if (!r) throw new Error(`stub: no suite fixture for ${exe} run ${suiteCount[exe]}`);
      return r;
    }
    assert.deepEqual(args.slice(1), ["--exact", "--test-threads=1"]);
    const name = args[0];
    const key = `${exe}#${name}`;
    soloCount[key] = (soloCount[key] ?? 0) + 1;
    return solo(exe, name, soloCount[key]);
  };
  return { spawn, calls };
}

const GREEN_LIB = () => result(libtestOutput([["a::one", "ok"], [RING, "ok"]]));
const RED_LIB = () => result(libtestOutput([["a::one", "ok"], [RING, "FAILED"]]), { code: 101 });
const GREEN_BIN = () => result(libtestOutput([["commands::x", "ok"]]));
const soloPass = (exe, name) =>
  result(libtestOutput([[name, "ok"]]));
const soloFail = (exe, name) =>
  result(libtestOutput([[name, "FAILED"]]), { code: 101 });

test("runCensus: a name red in run 2 only, green alone, is SUITE-ONLY and exit 1", async () => {
  const { spawn, calls } = stubSpawn({
    suiteByRun: { [LIB_EXE]: [GREEN_LIB(), RED_LIB(), GREEN_LIB()], [BIN_EXE]: [GREEN_BIN(), GREEN_BIN(), GREEN_BIN()] },
    solo: soloPass,
  });
  const report = await runCensus({
    fingerprint: FP,
    executables: EXES,
    runs: 3,
    soloRuns: 3,
    spawn,
    now: () => 1_000_000,
    treeSha: "e60a8944f",
    hostname: "merytshost",
  });
  assert.equal(report.exit_code, 1);
  assert.deepEqual(Object.keys(report.tests), [RING_ID]);
  const rec = report.tests[RING_ID];
  assert.equal(rec.label, "SUITE-ONLY");
  assert.equal(rec.suite_runs, 3);
  assert.equal(rec.suite_failures, 1);
  assert.deepEqual(rec.failed_in_runs, [2]);
  assert.equal(rec.solo_runs, 3);
  assert.equal(rec.solo_failures, 0);
  assert.equal(rec.executable, LIB_EXE);
  assert.match(rec.sample_panic, /panicked at src-tauri\/src\/outbound_trace.rs:200:9/);
  assert.deepEqual(report.header.intermittent, [RING_ID]);
  assert.deepEqual(report.header.always_red, []);
  assert.equal(report.header.tree_sha, "e60a8944f");
  assert.equal(report.header.hostname, "merytshost");
  assert.equal(report.header.runs, 3);
  assert.equal(report.header.solo_runs, 3);
  assert.equal(report.header.executables.length, 2);
  assert.deepEqual(report.header.unparsed, []);
  assert.deepEqual(report.summary, {
    "SUITE-ONLY": 1,
    "SOLO-RED": 0,
    "BOTH-FLAKY": 0,
    UNRESOLVED: 0,
    "UNRESOLVED (budget)": 0,
    UNPARSED: 0,
  });
  // 3 runs x 2 executables suite spawns, then 3 solo spawns with cargo's cwd.
  const soloCalls = calls.filter((c) => c.args.length > 0);
  assert.equal(calls.length - soloCalls.length, 6);
  assert.equal(soloCalls.length, 3);
  assert.equal(soloCalls[0].exe, LIB_EXE);
  assert.equal(soloCalls[0].args[0], RING);
  assert.equal(soloCalls[0].opts.cwd, "/home/box/qontinui-runner/src-tauri");
  assert.equal(soloCalls[0].opts.env.CARGO_MANIFEST_DIR, "/home/box/qontinui-runner/src-tauri");
  // The pretty renderer carries the block and the exit.
  const pretty = formatPretty(report);
  assert.match(pretty, /SUITE-ONLY — shares process state with a concurrent test/);
  assert.match(pretty, /suite {7}red 1\/3 \(runs 2\)/);
  assert.match(pretty, /exit 1$/);
});

test("runCensus: no reds at all is exit 0 with an empty inventory and no solo spawn", async () => {
  const { spawn, calls } = stubSpawn({
    suiteByRun: { [LIB_EXE]: [GREEN_LIB(), GREEN_LIB()], [BIN_EXE]: [GREEN_BIN(), GREEN_BIN()] },
    solo: () => {
      throw new Error("must not be called");
    },
  });
  const report = await runCensus({ fingerprint: FP, executables: EXES, runs: 2, soloRuns: 3, spawn, now: () => 0, hostname: "h" });
  assert.equal(report.exit_code, 0);
  assert.deepEqual(report.tests, {});
  assert.equal(calls.length, 4);
  assert.match(formatPretty(report), /No test was red in any suite run\./);
});

test("runCensus: fails alone every time is SOLO-RED and does not move the exit code", async () => {
  const { spawn } = stubSpawn({
    suiteByRun: { [LIB_EXE]: [RED_LIB(), RED_LIB()], [BIN_EXE]: [GREEN_BIN(), GREEN_BIN()] },
    solo: soloFail,
  });
  const report = await runCensus({ fingerprint: FP, executables: EXES, runs: 2, soloRuns: 3, spawn, now: () => 0, hostname: "h" });
  assert.equal(report.tests[RING_ID].label, "SOLO-RED");
  assert.equal(report.tests[RING_ID].solo_failures, 3);
  assert.deepEqual(report.header.always_red, [RING_ID]);
  assert.equal(report.exit_code, 0);
});

test("runCensus: fails alone sometimes is BOTH-FLAKY", async () => {
  const { spawn } = stubSpawn({
    suiteByRun: { [LIB_EXE]: [RED_LIB(), GREEN_LIB()], [BIN_EXE]: [GREEN_BIN(), GREEN_BIN()] },
    solo: (exe, name, k) => (k === 2 ? soloFail(exe, name) : soloPass(exe, name)),
  });
  const report = await runCensus({ fingerprint: FP, executables: EXES, runs: 2, soloRuns: 3, spawn, now: () => 0, hostname: "h" });
  assert.equal(report.tests[RING_ID].label, "BOTH-FLAKY");
  assert.equal(report.tests[RING_ID].solo_failures, 1);
  assert.equal(report.exit_code, 0);
});

test("runCensus: an unparsed suite run is exit 2 and is never read as green", async () => {
  const { spawn } = stubSpawn({
    suiteByRun: {
      [LIB_EXE]: [GREEN_LIB(), result("Segmentation fault\n", { code: null, signal: "SIGSEGV" })],
      [BIN_EXE]: [GREEN_BIN(), GREEN_BIN()],
    },
    solo: soloPass,
  });
  const report = await runCensus({ fingerprint: FP, executables: EXES, runs: 2, soloRuns: 3, spawn, now: () => 0, hostname: "h" });
  assert.equal(report.exit_code, 2);
  assert.deepEqual(report.tests, {});
  assert.equal(report.header.unparsed.length, 1);
  assert.equal(report.header.unparsed[0].run, 2);
  assert.equal(report.header.unparsed[0].executable, LIB_EXE);
  assert.match(formatPretty(report), /UNPARSED suite runs \(fail-closed — never read as green\): 1/);
});

test("runCensus: a solo re-run with no libtest output labels the test UNPARSED, exit 2", async () => {
  const { spawn } = stubSpawn({
    suiteByRun: { [LIB_EXE]: [RED_LIB()], [BIN_EXE]: [GREEN_BIN()] },
    solo: (exe, name, k) => (k === 1 ? result("", { code: 0 }) : soloPass(exe, name)),
  });
  const report = await runCensus({ fingerprint: FP, executables: EXES, runs: 1, soloRuns: 3, spawn, now: () => 0, hostname: "h" });
  assert.equal(report.tests[RING_ID].label, "UNPARSED");
  assert.equal(report.exit_code, 2);
});

const NO_SUCH_TEST = () =>
  result("running 0 tests\n\ntest result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 1 filtered out; finished in 0.00s\n");

test("runCensus: a solo re-run that runs 0 tests is UNRESOLVED (the id is not this executable's)", async () => {
  const { spawn } = stubSpawn({
    suiteByRun: { [LIB_EXE]: [RED_LIB()], [BIN_EXE]: [GREEN_BIN()] },
    solo: NO_SUCH_TEST,
  });
  const report = await runCensus({ fingerprint: FP, executables: EXES, runs: 1, soloRuns: 3, spawn, now: () => 0, hostname: "h" });
  assert.equal(report.tests[RING_ID].label, "UNRESOLVED");
  assert.match(report.tests[RING_ID].reason, /executed no test of that name/);
  assert.equal(report.exit_code, 2, "a name no executable ran is a resolution failure — a non-answer, fail-closed");
});

test("runCensus: two executables sharing one id — the re-run asks each until one runs the test", async () => {
  // This workspace really has two `qontinui_specs` (an integration test and a
  // bin). The suite red came from the SECOND one; the first knows no such test.
  const SPECS_TEST = "/t/deps/qontinui_specs-aaaaaaaaaaaaaaaa";
  const SPECS_BIN = "/t/deps/qontinui_specs-bbbbbbbbbbbbbbbb";
  const exes = [
    { executable: SPECS_TEST, binaryId: "qontinui_specs", cwd: "/t" },
    { executable: SPECS_BIN, binaryId: "qontinui_specs", cwd: "/t" },
  ];
  const { spawn, calls } = stubSpawn({
    suiteByRun: {
      [SPECS_TEST]: [result(libtestOutput([["a::x", "ok"]]))],
      [SPECS_BIN]: [result(libtestOutput([["b::y", "FAILED"]]), { code: 101 })],
    },
    solo: (exe, name) => (exe === SPECS_BIN ? soloPass(exe, name) : NO_SUCH_TEST()),
  });
  const report = await runCensus({ fingerprint: FP, executables: exes, runs: 1, soloRuns: 3, spawn, now: () => 0, hostname: "h" });
  const rec = report.tests["qontinui_specs::b::y"];
  assert.equal(rec.label, "SUITE-ONLY");
  assert.equal(rec.solo_runs, 3);
  assert.equal(rec.executable, SPECS_BIN);
  assert.deepEqual(report.header.collisions, [{ binaryId: "qontinui_specs", executables: [SPECS_TEST, SPECS_BIN] }]);
  const solo = calls.filter((c) => c.args.length > 0);
  assert.deepEqual(solo.map((c) => c.exe), [SPECS_TEST, SPECS_BIN, SPECS_BIN, SPECS_BIN]);
  assert.equal(report.exit_code, 1);
});

test("runCensus: an id no colliding executable knows is UNRESOLVED naming every one tried", async () => {
  const A = "/t/deps/qontinui_specs-aaaaaaaaaaaaaaaa";
  const B = "/t/deps/qontinui_specs-bbbbbbbbbbbbbbbb";
  const exes = [
    { executable: A, binaryId: "qontinui_specs", cwd: "/t" },
    { executable: B, binaryId: "qontinui_specs", cwd: "/t" },
  ];
  const { spawn } = stubSpawn({
    suiteByRun: { [A]: [result(libtestOutput([["a::x", "FAILED"]]), { code: 101 })], [B]: [result(libtestOutput([["b::y", "ok"]]))] },
    solo: NO_SUCH_TEST,
  });
  const report = await runCensus({ fingerprint: FP, executables: exes, runs: 1, soloRuns: 3, spawn, now: () => 0, hostname: "h" });
  const rec = report.tests["qontinui_specs::a::x"];
  assert.equal(rec.label, "UNRESOLVED");
  assert.match(rec.reason, new RegExp(`in ${A}, ${B} — the id belongs to none`));
  assert.equal(rec.solo_runs, 0);
});

test("runCensus: --budget-seconds — the deadline passing leaves the rest UNRESOLVED (budget), no solo spawn", async () => {
  // Two reds; the clock is advanced by each spawn so the budget ends after the
  // first test's first solo re-run.
  let t = 0;
  const twoRed = () =>
    result(libtestOutput([["a::first", "FAILED"], [RING, "FAILED"]]), { code: 101 });
  const { spawn: inner, calls } = stubSpawn({
    suiteByRun: { [LIB_EXE]: [twoRed()], [BIN_EXE]: [GREEN_BIN()] },
    solo: soloPass,
  });
  const spawn = async (...a) => {
    t += 1000;
    return inner(...a);
  };
  const report = await runCensus({
    fingerprint: FP,
    executables: EXES,
    runs: 1,
    soloRuns: 3,
    spawn,
    now: () => t,
    budgetSeconds: 3, // 2 suite spawns + 1 solo spawn = 3000 ms, then the deadline
    hostname: "h",
  });
  const first = report.tests["qontinui_runner_lib::a::first"];
  const ring = report.tests[RING_ID];
  assert.equal(first.solo_runs, 1);
  assert.equal(first.label, "SUITE-ONLY");
  assert.match(first.reason, /only 1\/3 solo re-runs fit the budget/);
  assert.equal(ring.solo_runs, 0);
  assert.equal(ring.label, "UNRESOLVED (budget)");
  assert.equal(report.header.budget_exhausted, true);
  assert.equal(report.header.budget_seconds, 3);
  assert.equal(calls.filter((c) => c.args.length > 0).length, 1);
  assert.equal(report.exit_code, 2, "a budget that cut the solo phase short is UNKNOWN, never clean");
});

test("runCensus: the deadline is checked in the SUITE loop too — a run not started by then is a non-answer, exit 2, no spawn", async () => {
  // 3 runs × 2 executables; the clock advances 1000 ms per spawn and the
  // budget is 2 s, so run 1 completes (2 spawns → t=2000 ≥ deadline) and every
  // later executable is recorded, not run — and never left to hang a bound.
  let t = 0;
  const { spawn: inner, calls } = stubSpawn({
    suiteByRun: { [LIB_EXE]: [GREEN_LIB(), GREEN_LIB(), GREEN_LIB()], [BIN_EXE]: [GREEN_BIN(), GREEN_BIN(), GREEN_BIN()] },
    solo: soloPass,
  });
  const spawn = async (...a) => {
    t += 1000;
    return inner(...a);
  };
  const report = await runCensus({ fingerprint: FP, executables: EXES, runs: 3, soloRuns: 3, spawn, now: () => t, budgetSeconds: 2, hostname: "h" });
  assert.equal(calls.length, 2, "only run 1's two executables were spawned");
  assert.equal(report.header.unparsed.length, 4, "runs 2 and 3, both executables");
  for (const u of report.header.unparsed) assert.equal(u.reason, "budget passed before this run");
  assert.deepEqual(report.header.unparsed.map((u) => u.run), [2, 2, 3, 3]);
  assert.equal(report.exit_code, 2);
  assert.match(formatPretty(report), /NON-ANSWERS .*4 unparsed suite run\(s\)/);
});

test("killTree: POSIX kills the process GROUP, Windows walks the tree with taskkill, and both fall back to the child", () => {
  const calls = [];
  const child = { pid: 4242, kill: (sig) => { calls.push(["child.kill", sig]); return true; } };
  assert.equal(killTree(child, { platform: "linux", kill: (pid, sig) => calls.push(["kill", pid, sig]) }), "group");
  assert.deepEqual(calls, [["kill", -4242, "SIGKILL"]], "negative pid = the group the detached child leads");

  calls.length = 0;
  assert.equal(
    killTree(child, { platform: "win32", taskkill: (cmd, args) => { calls.push([cmd, ...args]); return { status: 0 }; } }),
    "tree",
  );
  assert.deepEqual(calls, [["taskkill", "/PID", "4242", "/T", "/F"]]);

  // `spawnSync` throws only on ENOENT; access denied / a recycled PID is a
  // NON-ZERO STATUS with no throw, and must fall through to the direct kill.
  calls.length = 0;
  assert.equal(
    killTree(child, { platform: "win32", taskkill: (cmd, args) => { calls.push([cmd, ...args]); return { status: 128 }; } }),
    "child",
  );
  assert.deepEqual(calls, [["taskkill", "/PID", "4242", "/T", "/F"], ["child.kill", "SIGKILL"]]);
  calls.length = 0;
  assert.equal(killTree(child, { platform: "win32", taskkill: () => undefined }), "child", "no result object at all is not success either");

  calls.length = 0;
  const refuse = () => { throw new Error("ESRCH"); };
  assert.equal(killTree(child, { platform: "linux", kill: refuse }), "child", "group refused → direct kill");
  assert.deepEqual(calls, [["child.kill", "SIGKILL"]]);
  calls.length = 0;
  assert.equal(killTree(child, { platform: "win32", taskkill: refuse }), "child");
  assert.deepEqual(calls, [["child.kill", "SIGKILL"]]);

  assert.equal(killTree({ pid: undefined, kill: () => true }, { platform: "linux" }), "none", "no pid: nothing to kill");
  assert.equal(killTree({ pid: 1, kill: refuse }, { platform: "linux", kill: refuse }), "none");
});

test("runCensus: a timed-out solo re-run records the executable it ran against", async () => {
  const { spawn } = stubSpawn({
    suiteByRun: { [LIB_EXE]: [RED_LIB()], [BIN_EXE]: [GREEN_BIN()] },
    solo: () => result("", { code: null, timedOut: true }),
  });
  const report = await runCensus({ fingerprint: FP, executables: EXES, runs: 1, soloRuns: 2, spawn, now: () => 0, hostname: "h" });
  const rec = report.tests[RING_ID];
  assert.equal(rec.executable, LIB_EXE, "the timed-out branch must still record which executable was run");
  assert.equal(rec.solo_failures, 2);
  assert.equal(rec.label, "SOLO-RED");
  // The suite run's own panic was captured first and is kept — the timeout
  // note only fills an EMPTY sample_panic.
  assert.match(rec.sample_panic, /panicked at/);
});

test("extractPanicText redacts secret-shaped text at capture, and the annotation carries the redacted form", () => {
  const jwt = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.abc";
  const out = libtestOutput([["a::b", "FAILED"]], {
    panicFor: { "a::b": `thread 'a::b' panicked:\nAuthorization: Bearer ${jwt}\nurl=http://x/?token=s3cr3t&y=1 password=hunter2 SECRET=z` },
  });
  const panic = extractPanicText(out, "a::b");
  assert.doesNotMatch(panic, /eyJ/);
  assert.doesNotMatch(panic, /hunter2|s3cr3t/);
  assert.match(panic, /Bearer \[redacted\]/);
  assert.match(panic, /token=\[redacted\]/);
  assert.match(panic, /password=\[redacted\]/);
  assert.match(panic, /SECRET=\[redacted\]/);
  const ann = formatAnnotation("x::a::b", { label: "SUITE-ONLY", solo_runs: 3, solo_failures: 0, reason: "r", sample_panic: `Bearer ${jwt}` });
  assert.doesNotMatch(ann, /eyJ/);
  assert.match(ann, /Bearer \[redacted\]/);

  // The shapes a Rust panic actually prints: a Debug struct and a JSON body.
  const shaped = libtestOutput([["a::b", "FAILED"]], {
    panicFor: {
      "a::b": [
        "thread 'a::b' panicked at src/x.rs:1:1:",
        `assertion failed: Settings { runner_token: "qr_live_abcdef123456", api_key: "sk-live-1", retries: 3 }`,
        `body: {"token": "abc123", "n": 1} GITHUB_TOKEN=ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ0123`,
      ].join("\n"),
    },
  });
  const shapedPanic = extractPanicText(shaped, "a::b");
  for (const leak of ["qr_live", "sk-live", "abc123", "ghp_"]) assert.doesNotMatch(shapedPanic, new RegExp(leak), `leaked ${leak}`);
  assert.match(shapedPanic, /runner_token=\[redacted\], api_key=\[redacted\], retries: 3/);
  assert.match(shapedPanic, /"token"=\[redacted\], "n": 1\} GITHUB_TOKEN=\[redacted\]/);
});

test("defaultSpawn: a timeout settles even when `close` never fires — exit is honoured, streams destroyed, after the grace", async () => {
  // A fake child: `exit` fires after the kill, `close` never does (a
  // grandchild holds the inherited stdout pipe).
  const { EventEmitter } = await import("node:events");
  const stream = () => {
    const st = new EventEmitter();
    st.destroyed = false;
    st.setEncoding = () => {};
    st.destroy = () => { st.destroyed = true; };
    return st;
  };
  const child = new EventEmitter();
  child.pid = 777;
  child.stdout = stream();
  child.stderr = stream();
  child.kill = () => true;
  const killed = [];
  const kill = (c) => { killed.push(c.pid); setTimeout(() => c.emit("exit", null, "SIGKILL"), 1); return "group"; };
  const started = Date.now();
  const r = await defaultSpawn("/bin/hang", [], { timeoutMs: 5, graceMs: 20, spawnImpl: () => child, kill });
  assert.equal(r.timedOut, true);
  assert.equal(r.signal, "SIGKILL", "the `exit` that did fire is what the result carries");
  assert.equal(r.code, null);
  assert.deepEqual(killed, [777]);
  assert.equal(child.stdout.destroyed, true);
  assert.equal(child.stderr.destroyed, true);
  assert.ok(Date.now() - started < 2000, "settled by the grace, not by a `close` that never comes");

  // …and with no `exit` either, it still settles with nulls.
  const child2 = new EventEmitter();
  child2.pid = 778;
  child2.stdout = stream();
  child2.stderr = stream();
  child2.kill = () => true;
  const r2 = await defaultSpawn("/bin/hang", [], { timeoutMs: 5, graceMs: 20, spawnImpl: () => child2, kill: () => "group" });
  assert.deepEqual([r2.timedOut, r2.code, r2.signal, r2.spawnError], [true, null, null, null]);

  // …while an ordinary `close` still wins and the grace never fires.
  const child3 = new EventEmitter();
  child3.pid = 779;
  child3.stdout = stream();
  child3.stderr = stream();
  child3.kill = () => true;
  setTimeout(() => { child3.stdout.emit("data", "running 1 test\n"); child3.emit("exit", 0, null); child3.emit("close", 0, null); }, 1);
  const r3 = await defaultSpawn("/bin/ok", [], { timeoutMs: 5000, graceMs: 20, spawnImpl: () => child3, kill: () => { throw new Error("must not be called"); } });
  assert.deepEqual([r3.timedOut, r3.code, r3.stdout], [false, 0, "running 1 test\n"]);
  assert.equal(child3.stdout.destroyed, false);
});

test("runCensus: a should_panic test's panic block is found under its bare name", async () => {
  const sp = "a::boom - should panic";
  // libtest prints the id WITH the suffix on the result line and the BARE
  // name on the `---- <name> stdout ----` header.
  const red = result(
    libtestOutput([[sp, "FAILED"]], { panicFor: { [sp]: "thread 'a::boom' panicked: did not panic as expected" } }).replace(
      `---- ${sp} stdout ----`,
      "---- a::boom stdout ----",
    ),
    { code: 101 },
  );
  const { spawn } = stubSpawn({ suiteByRun: { [LIB_EXE]: [red], [BIN_EXE]: [GREEN_BIN()] }, solo: soloPass });
  const report = await runCensus({ fingerprint: FP, executables: EXES, runs: 1, soloRuns: 1, spawn, now: () => 0, hostname: "h" });
  const rec = report.tests[`qontinui_runner_lib::${sp}`];
  assert.ok(rec, "the id keeps the suffix");
  assert.match(rec.sample_panic ?? "", /did not panic as expected/, "the block is keyed by the bare name libtest prints");
});

test("runCensus: a doctest id is UNRESOLVED, never SOLO-RED, and is not spawned", async () => {
  // A libtest-shaped log where the announcement is a Doc-tests header (as
  // cargo prints for rustdoc's run) — fed through the exe-list route by
  // giving the stub an executable whose output carries that header.
  const doctestOutput = [
    "   Doc-tests qontinui-runner",
    "running 1 test",
    "test src-tauri/src/foo.rs - foo::Bar (line 12) ... FAILED",
    "test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.10s",
  ].join("\n");
  const { spawn, calls } = stubSpawn({
    suiteByRun: { [LIB_EXE]: [result(doctestOutput, { code: 101 })], [BIN_EXE]: [GREEN_BIN()] },
    solo: () => {
      throw new Error("a doctest must never be re-run through an executable");
    },
  });
  const report = await runCensus({ fingerprint: FP, executables: EXES, runs: 1, soloRuns: 3, spawn, now: () => 0, hostname: "h" });
  const id = "qontinui-runner::src-tauri/src/foo.rs - foo::Bar (line 12)";
  assert.deepEqual(Object.keys(report.tests), [id]);
  assert.equal(report.tests[id].label, "UNRESOLVED");
  assert.match(report.tests[id].reason, /doctest/);
  assert.equal(report.tests[id].executable, null);
  assert.equal(calls.filter((c) => c.args.length > 0).length, 0);
  assert.equal(report.exit_code, 0);
});

test("runCensus: an executable replaced on disk mid-census makes that run UNPARSED, never a verdict", async () => {
  let generation = 0;
  const fp = (path) => `${path}@${path === LIB_EXE ? generation : 0}`;
  const { spawn: inner, calls } = stubSpawn({
    suiteByRun: { [LIB_EXE]: [GREEN_LIB(), GREEN_LIB(), GREEN_LIB()], [BIN_EXE]: [GREEN_BIN(), GREEN_BIN(), GREEN_BIN()] },
    solo: soloPass,
  });
  const spawn = async (exe, args, opts) => {
    const r = await inner(exe, args, opts);
    // A peer's build lands after run 1's lib spawn.
    if (exe === LIB_EXE && args.length === 0) generation += 1;
    return r;
  };
  const report = await runCensus({ fingerprint: fp, executables: EXES, runs: 3, soloRuns: 3, spawn, now: () => 0, hostname: "h" });
  assert.equal(report.exit_code, 2);
  assert.deepEqual(report.header.unparsed.map((u) => [u.run, u.executable]), [[2, LIB_EXE], [3, LIB_EXE]]);
  assert.match(report.header.unparsed[0].reason, /executable changed on disk since resolution/);
  // The lib binary was spawned once (run 1); the bin binary all three runs.
  assert.equal(calls.filter((c) => c.exe === LIB_EXE).length, 1);
  assert.equal(calls.filter((c) => c.exe === BIN_EXE).length, 3);
  assert.equal(report.header.executables[0].sha256, `${LIB_EXE}@0`);
});

test("runCensus: a solo re-run against a replaced executable is UNRESOLVED, not a verdict", async () => {
  let generation = 0;
  const fp = (path) => `${path}@${path === LIB_EXE ? generation : 0}`;
  const { spawn: inner, calls } = stubSpawn({
    suiteByRun: { [LIB_EXE]: [RED_LIB()], [BIN_EXE]: [GREEN_BIN()] },
    solo: soloPass,
  });
  const spawn = async (exe, args, opts) => {
    const r = await inner(exe, args, opts);
    if (exe === BIN_EXE) generation += 1; // lands after the suite, before the solo phase
    return r;
  };
  const report = await runCensus({ fingerprint: fp, executables: EXES, runs: 1, soloRuns: 3, spawn, now: () => 0, hostname: "h" });
  assert.equal(report.tests[RING_ID].label, "UNRESOLVED");
  assert.match(report.tests[RING_ID].reason, /a solo re-run would measure a different tree/);
  assert.equal(calls.filter((c) => c.args.length > 0).length, 0);
});

test("snapshotExecutables copies each binary under <dir>/deps/<basename> and keeps the source", () => {
  const copied = [];
  const made = [];
  const out = snapshotExecutables(EXES, "/snap", { copy: (a, b) => copied.push([a, b]), mkdir: (d) => made.push(d) });
  // `deps` is the leaf the runner's ambient canary keys on; a flat copy
  // silently disarms it (measured 2026-09-21, four false SOLO-REDs).
  assert.deepEqual(made, ["/snap/deps"]);
  assert.deepEqual(copied, [
    [LIB_EXE, "/snap/deps/qontinui_runner_lib-a9341426b1692ff6"],
    [BIN_EXE, "/snap/deps/qontinui_runner-0123456789abcdef"],
  ]);
  assert.equal(out[0].executable, "/snap/deps/qontinui_runner_lib-a9341426b1692ff6");
  assert.equal(out[0].source, LIB_EXE);
  assert.equal(out[0].binaryId, "qontinui_runner_lib");
  assert.equal(out[0].cwd, "/home/box/qontinui-runner/src-tauri");
  // The copy still resolves to the same id through the shared normaliser.
  assert.equal(buildExecutableIndex(out).byId.get("qontinui_runner_lib").executable, "/snap/deps/qontinui_runner_lib-a9341426b1692ff6");
});

// ---------------------------------------------------------------------------
// --classify-from-log
// ---------------------------------------------------------------------------

const TS = "2026-09-17T07:26:07.5955615Z ";
const CI_LOG = [
  `${TS}     Running \`${LIB_EXE}\``,
  `${TS}running 3 tests`,
  `${TS}test a::one ... ok`,
  `${TS}test ${RING} ... FAILED`,
  `${TS}test settings::tests::persist ... FAILED`,
  `${TS}`,
  `${TS}failures:`,
  `${TS}`,
  `${TS}---- ${RING} stdout ----`,
  `${TS}thread '${RING}' panicked at src-tauri/src/outbound_trace.rs:200:9:`,
  `${TS}assertion \`left == right\` failed`,
  `${TS}`,
  `${TS}---- settings::tests::persist stdout ----`,
  `${TS}thread 'settings::tests::persist' panicked at src-tauri/src/settings.rs:4306:5:`,
  `${TS}settings.json appeared in the fixture dir`,
  `${TS}`,
  `${TS}failures:`,
  `${TS}    ${RING}`,
  `${TS}    settings::tests::persist`,
  `${TS}`,
  `${TS}test result: FAILED. 1 passed; 2 failed; 0 ignored; 0 measured; 0 filtered out; finished in 76.00s`,
  `${TS}   Doc-tests qontinui-runner`,
  `${TS}running 1 test`,
  `${TS}test src-tauri/src/foo.rs - foo::Bar (line 12) ... FAILED`,
  `${TS}test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.10s`,
].join("\n");

test("classifyFromLog: takes the reds from the gating log, re-runs alone, annotates, exit 0", async () => {
  const { spawn, calls } = stubSpawn({
    suiteByRun: {},
    solo: (exe, name) => (name === RING ? soloPass(exe, name) : soloFail(exe, name)),
  });
  const { report, annotations } = await classifyFromLog({
    logText: CI_LOG,
    executables: EXES,
    soloRuns: 3,
    spawn,
    now: () => 0,
    hostname: "gh-runner",
  });
  assert.equal(report.exit_code, 0);
  assert.equal(report.header.mode, "classify-from-log");
  assert.deepEqual(Object.keys(report.tests).sort(), [
    "qontinui-runner::src-tauri/src/foo.rs - foo::Bar (line 12)",
    RING_ID,
    "qontinui_runner_lib::settings::tests::persist",
  ]);
  assert.equal(report.tests[RING_ID].label, "SUITE-ONLY");
  assert.equal(report.tests[RING_ID].suite_runs, 1);
  assert.equal(report.tests[RING_ID].suite_failures, 1);
  assert.equal(report.tests[RING_ID].sample_panic, `thread '${RING}' panicked at src-tauri/src/outbound_trace.rs:200:9:\nassertion \`left == right\` failed`);
  assert.equal(report.tests["qontinui_runner_lib::settings::tests::persist"].label, "SOLO-RED");
  assert.equal(report.tests["qontinui-runner::src-tauri/src/foo.rs - foo::Bar (line 12)"].label, "UNRESOLVED");
  // Only the two resolvable ids were spawned, 3x each.
  assert.equal(calls.length, 6);
  assert.deepEqual(annotations, [
    "::notice title=UNRESOLVED::qontinui-runner::src-tauri/src/foo.rs - foo::Bar (line 12) UNRESOLVED: doctest — rustdoc compiles it per run; `cargo test --no-run` builds no executable for `Doc-tests qontinui-runner`",
    `::error title=SUITE-ONLY::${RING_ID} shares process state with a concurrent test — passes alone 3/3; see dossier runner-tests-share-in-process-mutable-state; panic: thread '${RING}' panicked at src-tauri/src/outbound_trace.rs:200:9:%0Aassertion \`left == right\` failed`,
    "::warning title=SOLO-RED::qontinui_runner_lib::settings::tests::persist fails alone 3/3 — a real defect or an ambient read, not the shared-state class; panic: thread 'settings::tests::persist' panicked at src-tauri/src/settings.rs:4306:5:%0Asettings.json appeared in the fixture dir",
  ]);
});

test("classifyFromLog: an unparsed log is a warning annotation and STILL exit 0", async () => {
  const { spawn, calls } = stubSpawn({ suiteByRun: {}, solo: soloPass });
  const { report, annotations } = await classifyFromLog({
    logText: "collect2: fatal error: ld terminated with signal 9 [Killed]\n",
    executables: EXES,
    soloRuns: 3,
    spawn,
    now: () => 0,
    hostname: "gh-runner",
  });
  assert.equal(report.exit_code, 0);
  assert.equal(report.header.unparsed.length, 1);
  assert.equal(calls.length, 0);
  assert.equal(annotations.length, 1);
  assert.match(annotations[0], /^::warning title=UNPARSED::the gating log carried no recognisable cargo test output/);
});

test("classifyFromLog: a green log classifies nothing and exits 0", async () => {
  const { spawn, calls } = stubSpawn({ suiteByRun: {}, solo: soloPass });
  const green = [`     Running \`${LIB_EXE}\``, "running 1 test", "test a::one ... ok", "test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.00s"].join("\n");
  const { report, annotations } = await classifyFromLog({ logText: green, executables: EXES, soloRuns: 3, spawn, now: () => 0, hostname: "h" });
  assert.equal(report.exit_code, 0);
  assert.deepEqual(report.tests, {});
  assert.deepEqual(annotations, []);
  assert.equal(calls.length, 0);
});

test("classifyFromLog: the budget bounds the solo re-runs and the exit stays 0", async () => {
  let t = 0;
  const { spawn: inner } = stubSpawn({ suiteByRun: {}, solo: soloPass });
  const spawn = async (...a) => {
    t += 1000;
    return inner(...a);
  };
  const { report, annotations } = await classifyFromLog({
    logText: CI_LOG,
    executables: EXES,
    soloRuns: 3,
    spawn,
    now: () => t,
    budgetSeconds: 1,
    hostname: "h",
  });
  assert.equal(report.exit_code, 0);
  assert.equal(report.header.budget_exhausted, true);
  const labels = Object.values(report.tests).map((r) => r.label).sort();
  assert.deepEqual(labels, ["SUITE-ONLY", "UNRESOLVED", "UNRESOLVED (budget)"]);
  assert.ok(annotations.some((a) => a.startsWith("::notice title=UNRESOLVED (budget)::")));
});

// ---------------------------------------------------------------------------
// CLI parsing
// ---------------------------------------------------------------------------

test("parseCliArgs: defaults and the documented flags", () => {
  const d = parseCliArgs([]);
  assert.equal(d.runs, 5);
  assert.equal(d.soloRuns, 3);
  assert.equal(d.cargo, "cargo");
  assert.equal(d.format, "pretty");
  assert.equal(d.budgetSeconds, null);
  assert.equal(d.classifyFromLog, null);
  const c = parseCliArgs([
    "--runs", "10", "--solo-runs", "4", "--cargo", "bash /x/cargo-guard.sh", "--exe-list", "/tmp/e",
    "--budget-seconds", "600", "--format", "json", "--out", "/tmp/o.json", "--classify-from-log", "/tmp/l",
  ]);
  assert.equal(c.runs, 10);
  assert.equal(c.soloRuns, 4);
  assert.equal(c.cargo, "bash /x/cargo-guard.sh");
  assert.equal(c.exeList, "/tmp/e");
  assert.equal(c.budgetSeconds, 600);
  assert.equal(c.format, "json");
  assert.equal(c.out, "/tmp/o.json");
  assert.equal(c.classifyFromLog, "/tmp/l");
  assert.equal(parseCliArgs(["--snapshot-dir", "/tmp/snap"]).snapshotDir, "/tmp/snap");
  assert.throws(() => parseCliArgs(["--runs", "0"]), /--runs must be a positive integer/);
  assert.throws(() => parseCliArgs(["--format", "xml"]), /--format must be pretty or json/);
});

// ---------------------------------------------------------------------------
// The wire token (Phase 1): what the ingest carries and the escalator keys on
// ---------------------------------------------------------------------------

test("classificationTokenFor: exactly the three verdicts have a token; every non-answer is null", () => {
  assert.equal(classificationTokenFor(LABEL.SUITE_ONLY), "suite_only");
  assert.equal(classificationTokenFor(LABEL.SOLO_RED), "solo_red");
  assert.equal(classificationTokenFor(LABEL.BOTH_FLAKY), "both_flaky");
  for (const l of [LABEL.UNRESOLVED, LABEL.UNRESOLVED_BUDGET, LABEL.UNPARSED, LABEL.GREEN, "NEW", "", null, undefined]) {
    assert.equal(classificationTokenFor(l), null, `label ${JSON.stringify(l)}`);
  }
  assert.deepEqual(Object.keys(CLASSIFICATION_TOKEN).sort(), [LABEL.BOTH_FLAKY, LABEL.SOLO_RED, LABEL.SUITE_ONLY].sort());
});

test("classificationsFromReport: id -> token out of a report of either mode, non-answers omitted, any shape tolerated", () => {
  const report = {
    header: { mode: "classify-from-log" },
    tests: {
      "b::x": { label: LABEL.SUITE_ONLY },
      "b::y": { label: LABEL.SOLO_RED },
      "b::z": { label: LABEL.BOTH_FLAKY },
      "b::u": { label: LABEL.UNRESOLVED },
      "b::p": { label: LABEL.UNPARSED },
      "b::nolabel": {},
      "b::null": null,
    },
  };
  assert.deepEqual(
    [...classificationsFromReport(report).entries()].sort(),
    [["b::x", "suite_only"], ["b::y", "solo_red"], ["b::z", "both_flaky"]],
  );
  for (const bad of [null, undefined, 3, "s", [], {}, { tests: null }, { tests: [] }, { tests: "x" }]) {
    assert.equal(classificationsFromReport(bad).size, 0, `shape ${JSON.stringify(bad)}`);
  }
});

test("classificationsFromReport reads what classifyFromLog wrote (the two ends agree by construction)", async () => {
  const { spawn } = stubSpawn({
    suiteByRun: {},
    solo: (exe, name) => (name === RING ? soloPass(exe, name) : soloFail(exe, name)),
  });
  const { report } = await classifyFromLog({
    logText: CI_LOG,
    executables: EXES,
    soloRuns: 3,
    spawn,
    now: () => 0,
  });
  // Through JSON, as the ingest and the escalator read it off disk.
  const byId = classificationsFromReport(JSON.parse(JSON.stringify(report)));
  assert.deepEqual(
    [...byId.entries()].sort(),
    [
      [RING_ID, "suite_only"],
      ["qontinui_runner_lib::settings::tests::persist", "solo_red"],
    ],
    "the doctest's UNRESOLVED is omitted, not mapped",
  );
});

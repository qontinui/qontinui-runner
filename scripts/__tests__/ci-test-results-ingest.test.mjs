#!/usr/bin/env node
// Unit tests for the PURE half of ci-test-results-ingest.mjs (Phase 1,
// redesigned, of plan
// `2026-08-30-runner-ci-has-no-flake-detection-so-one-flaky-test-freezes-the-train`).
//
// Uses Node's built-in `node:test` runner, mirroring
// `scripts/__tests__/ci-flake-analyze.test.mjs`.
//
// Run with:
//   node --test scripts/__tests__/ci-test-results-ingest.test.mjs

import test from "node:test";
import assert from "node:assert/strict";

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

import { buildIngestBody, chunkResults, readClassification, toWireRow } from "../ci-test-results-ingest.mjs";

const TS = "2026-09-02T07:26:07.5955615Z ";

const GREEN_LOG = [
  `${TS}running 2 tests`,
  `${TS}test foo::bar ... ok`,
  `${TS}test foo::baz ... ok`,
  `${TS}test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s`,
].join("\n");

const RED_LOG = [
  `${TS}running 2 tests`,
  `${TS}test foo::bar ... FAILED`,
  `${TS}test foo::baz ... ok`,
  `${TS}test result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s`,
].join("\n");

const NO_CARGO_OUTPUT_LOG = [
  `${TS}##[group]Run cd src-tauri`,
  `${TS}   Compiling qontinui-runner v1.0.10`,
  `${TS}collect2: fatal error: ld terminated with signal 9 [Killed]`,
  `${TS}##[error]Process completed with exit code 101.`,
].join("\n");

test("a green log builds a results body with pass outcomes", () => {
  const { body, warning } = buildIngestBody({
    logText: GREEN_LOG,
    repo: "qontinui/qontinui-runner",
    headSha: "abc123",
  });
  assert.equal(warning, null);
  assert.deepEqual(body, {
    repo: "qontinui/qontinui-runner",
    head_sha: "abc123",
    source: "ci",
    results: [
      { test_id: "foo::bar", outcome: "pass", classification: null },
      { test_id: "foo::baz", outcome: "pass", classification: null },
    ],
  });
});

test("a red log carries the failing test's outcome through, not a suppressed pass", () => {
  const { body, warning } = buildIngestBody({
    logText: RED_LOG,
    repo: "qontinui/qontinui-runner",
    headSha: "def456",
  });
  assert.equal(warning, null);
  assert.deepEqual(body.results, [
    { test_id: "foo::bar", outcome: "fail", classification: null },
    { test_id: "foo::baz", outcome: "pass", classification: null },
  ]);
});

test("shard is attached to every row when supplied", () => {
  const { body } = buildIngestBody({
    logText: GREEN_LOG,
    repo: "qontinui/qontinui-runner",
    headSha: "abc123",
    shard: "windows-latest",
  });
  assert.deepEqual(body.results, [
    { test_id: "foo::bar", outcome: "pass", shard: "windows-latest", classification: null },
    { test_id: "foo::baz", outcome: "pass", shard: "windows-latest", classification: null },
  ]);
});

test("shard is omitted entirely (not sent as null/undefined) when not supplied", () => {
  const { body } = buildIngestBody({
    logText: GREEN_LOG,
    repo: "qontinui/qontinui-runner",
    headSha: "abc123",
  });
  for (const r of body.results) {
    assert.equal(Object.hasOwn(r, "shard"), false);
  }
});

test("an unparsed log (compile failure) yields no body and a named warning, never an empty-results POST", () => {
  const { body, warning } = buildIngestBody({
    logText: NO_CARGO_OUTPUT_LOG,
    repo: "qontinui/qontinui-runner",
    headSha: "abc123",
  });
  assert.equal(body, null);
  assert.match(warning, /not recognised as cargo test output/);
});

test("recognised output with zero named tests yields no body, not a zero-row POST", () => {
  const log = [`${TS}running 0 tests`, `${TS}test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s`].join(
    "\n",
  );
  const { body, warning } = buildIngestBody({
    logText: log,
    repo: "qontinui/qontinui-runner",
    headSha: "abc123",
  });
  assert.equal(body, null);
  assert.match(warning, /zero named tests/);
});

// ---------------------------------------------------------------------------
// chunkResults — the fix for the silent half-loss on run 34105940854, where a
// single 10,369-row POST got HTTP 200 on windows and was aborted by its own
// 60 s client timeout on ubuntu. Every expectation below is a LITERAL: a test
// written against the module's own CHUNK_SIZE would still pass if that constant
// were changed to something broken, so the constant is deliberately not
// imported here.
// ---------------------------------------------------------------------------

const mk = (n) =>
  Array.from({ length: n }, (_, i) => ({ test_id: `t${i}`, outcome: "pass" }));

test("chunkResults splits on the boundary with a short final chunk", () => {
  const chunks = chunkResults(mk(2500), 1000);
  assert.equal(chunks.length, 3);
  assert.deepEqual(
    chunks.map((c) => c.length),
    [1000, 1000, 500],
  );
});

test("chunkResults preserves order and loses no row", () => {
  const rows = mk(2500);
  const flat = chunkResults(rows, 1000).flat();
  assert.equal(flat.length, 2500);
  assert.equal(flat[0].test_id, "t0");
  assert.equal(flat[1000].test_id, "t1000");
  assert.equal(flat[2499].test_id, "t2499");
});

test("an exact multiple yields no trailing empty chunk", () => {
  const chunks = chunkResults(mk(2000), 1000);
  assert.equal(chunks.length, 2);
  assert.deepEqual(
    chunks.map((c) => c.length),
    [1000, 1000],
  );
});

test("fewer rows than the chunk size is a single chunk", () => {
  const chunks = chunkResults(mk(7), 1000);
  assert.equal(chunks.length, 1);
  assert.equal(chunks[0].length, 7);
});

test("the observed 10369-row payload becomes 11 bounded chunks, not one", () => {
  // The exact size that was lost on ubuntu. The point of the assertion is that
  // NO chunk is the whole payload.
  const chunks = chunkResults(mk(10369), 1000);
  assert.equal(chunks.length, 11);
  assert.deepEqual(chunks.at(-1).length, 369);
  assert.ok(
    chunks.every((c) => c.length <= 1000),
    "no chunk may carry the whole payload",
  );
});

test("an empty result set yields no chunks, so no empty POST is made", () => {
  assert.deepEqual(chunkResults([], 1000), []);
  assert.deepEqual(chunkResults(undefined, 1000), []);
});

test("a non-positive size degrades to one chunk, never an infinite loop", () => {
  // A misconfigured constant must fall back to today's single-request
  // behaviour rather than hanging CI.
  assert.equal(chunkResults(mk(5), 0).length, 1);
  assert.equal(chunkResults(mk(5), -1).length, 1);
  assert.equal(chunkResults(mk(5), Number.NaN).length, 1);
});

// ---------------------------------------------------------------------------
// Regression: the ingest step must pin its own COORD_HTTP_URL.
//
// `Poison ambient state` exports COORD_HTTP_URL=http://poison.invalid to
// $GITHUB_ENV so the SUITE cannot reach coord. `Unpoison ambient state` blanks
// it again -- but it is ordered AFTER the ingest step, so on 2026-09-09 the
// ingest POSTed to the poison host and recorded `0/10603 row(s)` (job
// 102391645194) while still reporting green, because the step is
// `continue-on-error`.
//
// Reordering was not the fix: `Unpoison` then had no `if: always()`, so a red
// suite skipped it, and a red suite is precisely what this ingest exists to
// record. It runs `if: always()` now, but the step still carries its own
// `COORD_HTTP_URL`, which beats $GITHUB_ENV for that step alone and keeps it
// independent of where the unpoison sits.
//
// ===========================================================================
// WHAT THIS GUARD DOES NOT DO -- read before adding to it
// ===========================================================================
// It asserts ONE textual fact: the named step's `env:` mapping pins
// COORD_HTTP_URL to an empty string. That is exactly the line this fix adds,
// and deleting or weakening that line is the regression worth catching.
//
// It does NOT verify that the ingest reaches coord, and NO static assertion
// over this file can. Four review rounds established that empirically: three
// successive attempts to also guard "the upstream export still exists" were
// each a FALSE PASS, satisfied by text that was not the export --
//   1. a whole-file grep, satisfied by this comment block quoting the string;
//   2. two independent existence checks, satisfied by the diagnostic echo and
//      by the three OTHER exported variables;
//   3. a forward non-greedy brace match, which began at `rand_uuid() {` and so
//      accepted a comment 66 lines away;
//   4. a backward walk to the nearest lone `{`, defeated by a `{ cmd`
//      same-line brace, by nested braces, and by an `echo` inside a quoted
//      heredoc -- dead text that never executes.
// Closing those needs heredoc stripping, brace-depth matching, top-level-depth
// checks and `if:` evaluation -- a shell parser in a test file that may import
// only Node built-ins (ci.yml:224). It would STILL miss `if: false` on the
// step, a `GITHUB_ENV=` reassignment, a later line blanking the value, or an
// `export COORD_HTTP_URL=...` in the ingest step's own `run:` body.
//
// That guard was therefore REMOVED rather than patched a fifth time. A guard
// that cannot fail is worse than no guard, because a commit message cites it
// as coverage. The property it was reaching for -- "the ingest actually
// recorded rows" -- is observable at RUN time and nowhere else: the step
// already prints `N/M row(s) recorded across K chunk(s)`, and asserting on
// that line is the change that would have caught the original defect. Tracked
// as follow-up; deliberately not faked here.
// ---------------------------------------------------------------------------

const CI_YML = join(
  dirname(fileURLToPath(import.meta.url)),
  "..",
  "..",
  ".github",
  "workflows",
  "ci.yml",
);

/** Indent width of a line, or Infinity for a blank one (blanks never bound). */
const indentOf = (l) => (l.trim() === "" ? Infinity : l.search(/\S/));

const escapeRe = (s) => s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");

/**
 * The lines of ONE `- name: <stepName>` step.
 *
 * The name match is ANCHORED and must be unique: an unanchored `includes`
 * matches a prose comment that mentions the step, or a longer step name that
 * merely contains this one, and then reports a confusing failure against the
 * wrong block. The block ends at the first line indented at or above the
 * step's own `-`, which is every one of its keys and nothing else.
 */
function stepLines(yml, stepName) {
  const lines = yml.split("\n");
  const re = new RegExp(
    `^\\s*-\\s+name:\\s*(['"]?)${escapeRe(stepName)}\\1\\s*(#.*)?$`,
  );
  const hits = lines.map((l, i) => (re.test(l) ? i : -1)).filter((i) => i !== -1);
  assert.equal(
    hits.length,
    1,
    `expected exactly one \`- name: ${stepName}\` step in ci.yml, found ${hits.length}`,
  );
  const start = hits[0];
  const stepIndent = indentOf(lines[start]);
  const out = [lines[start]];
  for (let i = start + 1; i < lines.length; i++) {
    if (indentOf(lines[i]) <= stepIndent) break;
    out.push(lines[i]);
  }
  return out;
}

/** The `env:` mapping lines of one step, bounded to that step. */
function stepEnvLines(yml, stepName) {
  const lines = stepLines(yml, stepName);
  const envAt = lines.findIndex((l) => /^\s+env:\s*$/.test(l));
  assert.ok(envAt !== -1, `no env: block on step: ${stepName}`);
  const indent = indentOf(lines[envAt]);
  const out = [];
  for (let i = envAt + 1; i < lines.length; i++) {
    if (lines[i].trim() === "") continue;
    if (indentOf(lines[i]) <= indent) break;
    out.push(lines[i]);
  }
  return out;
}

test("the coord-report step pins its own COORD_HTTP_URL to empty, defeating the ambient poison", () => {
  const yml = readFileSync(CI_YML, "utf8");
  const env = stepEnvLines(yml, "Report test results to coord (best-effort)");
  const assigned = env.filter((l) => /^\s*COORD_HTTP_URL\s*:/.test(l));

  assert.equal(
    assigned.length,
    1,
    "the ingest step must set COORD_HTTP_URL itself; without it the step " +
      "inherits the poisoned value from `Poison ambient state` and silently " +
      "records nothing (continue-on-error hides the failure)",
  );

  // Assert the VALUE, not merely the key. `COORD_HTTP_URL: ${{ env.COORD_HTTP_URL }}`
  // would re-import the poisoned ambient value verbatim while satisfying any
  // shape-only check -- a silent regression straight back to this defect.
  // Empty is intended: the script reads
  // `process.env.COORD_HTTP_URL || DEFAULT_COORD_URL`, so "" falls through to
  // its documented default on every matrix leg (Windows may present an
  // empty-valued entry as absent; both are falsy, so both resolve the same).
  // A trailing comment is allowed -- this file's house style is comment-heavy.
  assert.match(
    assigned[0],
    /^\s*COORD_HTTP_URL\s*:\s*(""|'')\s*(#.*)?$/,
    `the ingest step must pin COORD_HTTP_URL to an empty string, got: ${assigned[0].trim()}`,
  );
});

// ---------------------------------------------------------------------------
// Phase 4a of plan
// `2026-09-17-the-windows-test-gate-is-a-90-minute-build-wearing-a-test-shaped-bound`:
// `--gating-outcome` makes an UNQUALIFIED ingest LOUD, and changes nothing else.
//
// The second half of that sentence is the load-bearing one and is asserted
// against a real POST rather than by inspection: coord's `ResultIngestRequest`
// carries no `#[serde(deny_unknown_fields)]`, so a body key added here would be
// SILENTLY DROPPED — a producer that looked correct and wrote nothing. Payload
// identity is therefore pinned byte-for-byte, so the day someone "finishes the
// job" by adding a field, this fails instead of shipping a void.
// ---------------------------------------------------------------------------

import { createServer } from "node:http";
import { execFile } from "node:child_process";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { gatingQualification } from "../ci-test-results-ingest.mjs";

const INGEST_CLI = join(dirname(fileURLToPath(import.meta.url)), "..", "ci-test-results-ingest.mjs");

test("gatingQualification: an ABSENT flag is not a warning", () => {
  const q = gatingQualification(undefined, { repo: "o/r", headSha: "abc", rows: 3 });
  assert.equal(q.qualified, true);
  assert.equal(q.message, null);
});

test("gatingQualification: `success` is qualified", () => {
  assert.equal(
    gatingQualification("success", { repo: "o/r", headSha: "abc", rows: 3 }).qualified,
    true,
  );
});

test("gatingQualification: a non-success outcome is announced with the head and the count", () => {
  const q = gatingQualification("failure", { repo: "o/r", headSha: "deadbee", rows: 7112 });
  assert.equal(q.qualified, false);
  assert.match(q.message, /7112 row\(s\)/);
  assert.match(q.message, /o\/r@deadbee/);
  assert.match(q.message, /UNQUALIFIED/);
  assert.match(q.message, /pr_check_runs/);
});

test("gatingQualification: a non-success gate whose OWN rows carry a failure is NOT announced", () => {
  // The ordinary red PR. coord's rows are correct and complete — `FAILED`
  // entries included — so there is no disagreement, and firing the alarm on the
  // commonest non-success path is how an alarm stops being read.
  const q = gatingQualification("failure", {
    repo: "o/r",
    headSha: "abc",
    rows: 2,
    anyFailed: true,
  });
  assert.equal(q.qualified, true);
  assert.equal(q.message, null);
});

test("gatingQualification: an UNRECOGNISED outcome is announced EVEN on a red suite", () => {
  // The regression a review caught: the `anyFailed` short-circuit was ordered
  // ABOVE the recognition test, so an empty or unexpanded `${{ … }}` — i.e.
  // "this script could not read the gate at all" — went unreported on every
  // red suite. That is the silent-empty-is-unknown failure this flag exists to
  // end, and it is the exact breakage the step-`id` pin in
  // src-tauri/tests/ci_rust_test_steps_split.rs guards upstream.
  for (const unreadable of ["", "${{ steps.run_rust_tests.outcome }}", "TIMED_OUT"]) {
    const q = gatingQualification(unreadable, {
      repo: "o/r",
      headSha: "abc",
      rows: 2,
      anyFailed: true,
    });
    assert.equal(q.qualified, false, `an unreadable outcome ${JSON.stringify(unreadable)} must still announce`);
    assert.match(q.message, /UNKNOWN, not success/);
  }

  // …while every RECOGNISED non-success outcome still stays quiet on a red suite.
  for (const known of ["failure", "cancelled", "skipped"]) {
    assert.equal(
      gatingQualification(known, { repo: "o/r", headSha: "abc", rows: 2, anyFailed: true })
        .qualified,
      true,
      `a recognised ${known} beside failing rows is the ordinary red PR`,
    );
  }
});

test("gatingQualification: an UNRECOGNISED outcome is UNKNOWN, never read as success", () => {
  const q = gatingQualification("${{ steps.run_rust_tests.outcome }}", {
    repo: "o/r",
    headSha: "abc",
    rows: 1,
  });
  assert.equal(q.qualified, false);
  assert.match(q.message, /UNKNOWN, not success/);
});

/// Run the CLI against a throwaway HTTP server and return {bodies, stdout}.
function runIngest(args, logText, extraEnv = {}) {
  return new Promise((resolvePromise, rejectPromise) => {
    const bodies = [];
    const server = createServer((req, res) => {
      let buf = "";
      req.on("data", (c) => {
        buf += c;
      });
      req.on("end", () => {
        bodies.push(buf);
        res.writeHead(200, { "Content-Type": "application/json" });
        res.end(JSON.stringify({ parsed: 1, persisted: 1, failed: 0 }));
      });
    });
    server.listen(0, "127.0.0.1", () => {
      const port = server.address().port;
      const dir = mkdtempSync(join(tmpdir(), "ingest-gating-"));
      const logPath = join(dir, "cargo-test-output.log");
      writeFileSync(logPath, logText, "utf8");
      execFile(
        process.execPath,
        [INGEST_CLI, "--log", logPath, "--repo", "o/r", "--head-sha", "abc123", ...args],
        {
          env: {
            ...process.env,
            COORD_INGEST_TOKEN: "test-token",
            COORD_HTTP_URL: `http://127.0.0.1:${port}`,
            GITHUB_STEP_SUMMARY: "",
            ...extraEnv,
          },
        },
        (err, stdout) => {
          server.close();
          if (err) return rejectPromise(err);
          resolvePromise({ bodies, stdout });
        },
      );
    });
  });
}

test("the POST body is BYTE-IDENTICAL with and without --gating-outcome", async () => {
  const plain = await runIngest([], GREEN_LOG);
  const flagged = await runIngest(["--gating-outcome", "failure"], GREEN_LOG);

  assert.equal(plain.bodies.length, 1, "the plain run must have POSTed exactly once");
  assert.equal(flagged.bodies.length, 1, "the flagged run must have POSTed exactly once");
  assert.equal(
    flagged.bodies[0],
    plain.bodies[0],
    "--gating-outcome must not change one byte of the payload: coord has no field " +
      "for it and would silently drop an added key",
  );

  // …and the announcement DOES fire, loudly, on exactly the flagged run.
  assert.match(flagged.stdout, /::error title=test-results-ingest::.*UNQUALIFIED/s);
  assert.doesNotMatch(plain.stdout, /UNQUALIFIED/);
});

test("--gating-outcome success stays silent", async () => {
  const ok = await runIngest(["--gating-outcome", "success"], GREEN_LOG);
  assert.equal(ok.bodies.length, 1);
  assert.doesNotMatch(ok.stdout, /UNQUALIFIED/);
});

test("a RED suite under a failed gate stays quiet end to end", async () => {
  // The same narrowing, exercised through the real CLI rather than the pure
  // function, because the conjunct is computed in `main` from the parsed body.
  const red = await runIngest(["--gating-outcome", "failure"], RED_LOG);
  assert.equal(red.bodies.length, 1, "it must still ingest — the rows are real data");
  assert.doesNotMatch(red.stdout, /UNQUALIFIED/);
});

test("the UNQUALIFIED notice reaches $GITHUB_STEP_SUMMARY, appended", async () => {
  // Neither `stepSummary()` nor the summary branch was covered by any test —
  // the byte-identity test above deliberately blanks GITHUB_STEP_SUMMARY.
  const dir = mkdtempSync(join(tmpdir(), "ingest-summary-"));
  const summary = join(dir, "summary.md");
  writeFileSync(summary, "PRE-EXISTING\n", "utf8");

  const r = await runIngest(["--gating-outcome", "failure"], GREEN_LOG, {
    GITHUB_STEP_SUMMARY: summary,
  });
  assert.equal(r.bodies.length, 1);

  const written = readFileSync(summary, "utf8");
  assert.match(written, /^PRE-EXISTING$/m, "it must APPEND, never truncate");
  assert.match(written, /Test results recorded for a NON-SUCCESS gating step/);
  assert.match(written, /UNQUALIFIED/);
});

// ---------------------------------------------------------------------------
// MUTATION PROOF — runs the REAL function under a mutated wrapper and asserts
// that an assertion in THIS suite catches it. The test that stood here until
// 2026-09-19 built a local stub and asserted the stub behaved as written, which
// is a tautology that inflates the count and covers nothing.
// ---------------------------------------------------------------------------

test("MUTATION: dropping the gating plumbing is caught by this suite's own assertions", () => {
  // The mutation: `main` never consults the flag, so everything looks qualified.
  const mutated = () => ({ qualified: true, message: null });

  // The assertion this suite already makes about a non-success, all-green gate:
  const assertion = (fn) => {
    const q = fn("failure", { repo: "o/r", headSha: "deadbee", rows: 7112, anyFailed: false });
    assert.equal(q.qualified, false);
    assert.match(q.message, /UNQUALIFIED/);
  };

  let realThrew = false;
  try {
    assertion(gatingQualification);
  } catch {
    realThrew = true;
  }
  assert.equal(realThrew, false, "the real implementation must satisfy it");

  let mutantThrew = false;
  try {
    assertion(mutated);
  } catch {
    mutantThrew = true;
  }
  assert.equal(mutantThrew, true, "and the mutant must be caught by that same assertion");
});

// ---------------------------------------------------------------------------
// --classification (Phase 1 of plan
// 2026-09-17-runner-tests-share-in-process-mutable-state): the classifier's
// json rides onto the pre-parsed rows; its absence is null everywhere, never
// a failure; and NOTHING of it reaches the wire — coord has no column, and a
// dropped key would read as a successful write of nothing (the rule the
// `--gating-outcome` section above establishes).
// ---------------------------------------------------------------------------

/** A `test-interleave-census.mjs` report (either mode carries `tests[id].label`). */
const CLASSIFICATION_REPORT = JSON.stringify({
  header: { mode: "classify-from-log", solo_runs: 3 },
  tests: {
    "foo::bar": { label: "SUITE-ONLY", solo_runs: 3, solo_failures: 0 },
    "foo::other": { label: "SOLO-RED", solo_runs: 3, solo_failures: 3 },
    "foo::timing": { label: "BOTH-FLAKY", solo_runs: 3, solo_failures: 1 },
    "foo::doc - x (line 1)": { label: "UNRESOLVED", reason: "doctest" },
    "foo::late": { label: "UNRESOLVED (budget)" },
    "foo::garbled": { label: "UNPARSED" },
  },
  summary: {},
  exit_code: 0,
});

const fakeFs = (files) => ({
  read: (p) => {
    if (!Object.hasOwn(files, p)) {
      const err = new Error(`ENOENT: no such file or directory, open '${p}'`);
      err.code = "ENOENT";
      throw err;
    }
    return files[p];
  },
});

test("classification present: the three verdicts map to tokens by test id; every non-answer is null", () => {
  const { byId, note } = readClassification(
    "/w/test-classification.json",
    fakeFs({ "/w/test-classification.json": CLASSIFICATION_REPORT }),
  );
  assert.equal(note, null);
  assert.deepEqual(
    [...byId.entries()].sort(),
    [
      ["foo::bar", "suite_only"],
      ["foo::other", "solo_red"],
      ["foo::timing", "both_flaky"],
    ],
    "UNRESOLVED / UNRESOLVED (budget) / UNPARSED never become a class",
  );
  const { body } = buildIngestBody({
    logText: RED_LOG,
    repo: "qontinui/qontinui-runner",
    headSha: "def456",
    shard: "ubuntu-22.04",
    classification: byId,
  });
  assert.deepEqual(body.results, [
    { test_id: "foo::bar", outcome: "fail", shard: "ubuntu-22.04", classification: "suite_only" },
    { test_id: "foo::baz", outcome: "pass", shard: "ubuntu-22.04", classification: null },
  ]);
});

test("classification file absent (the green-run case): null on every row and a note, never a throw", () => {
  const { byId, note } = readClassification("/w/test-classification.json", fakeFs({}));
  assert.equal(byId, null);
  assert.match(note, /not readable \(ENOENT\)/);
  assert.match(note, /classification=null on every row/);
  const { body, warning } = buildIngestBody({
    logText: RED_LOG,
    repo: "qontinui/qontinui-runner",
    headSha: "def456",
    classification: byId,
  });
  assert.equal(warning, null, "an absent classification never blocks the ingest");
  for (const r of body.results) assert.equal(r.classification, null);
});

test("no --classification flag at all: no note, null on every row", () => {
  const { byId, note } = readClassification(undefined, fakeFs({}));
  assert.equal(byId, null);
  assert.equal(note, null);
  const { body } = buildIngestBody({ logText: GREEN_LOG, repo: "o/r", headSha: "x" });
  for (const r of body.results) {
    assert.equal(Object.hasOwn(r, "classification"), true, "the key is always present on the pre-parsed row");
    assert.equal(r.classification, null);
  }
});

test("classification file unparseable or the wrong shape: null on every row and a note naming the file", () => {
  for (const [content, expect] of [
    ["{not json", /is not JSON/],
    ['"a string"', /carries no `tests` object/],
    ["[1,2,3]", /carries no `tests` object/],
    ['{"header":{}}', /carries no `tests` object/],
    ['{"tests":"nope"}', /carries no `tests` object/],
  ]) {
    const { byId, note } = readClassification("/w/c.json", fakeFs({ "/w/c.json": content }));
    assert.equal(byId, null, `content ${content}`);
    assert.match(note, expect, `content ${content}`);
    assert.match(note, /\/w\/c\.json/);
  }
});

test("classification report with malformed records: tolerated per record, never a throw", () => {
  const { byId, note } = readClassification(
    "/w/c.json",
    fakeFs({
      "/w/c.json": JSON.stringify({
        tests: {
          "a::ok": { label: "SUITE-ONLY" },
          "a::nolabel": {},
          "a::null": null,
          "a::weird": { label: "SOMETHING-NEW" },
          "a::num": 7,
        },
      }),
    }),
  );
  assert.equal(note, null);
  assert.deepEqual([...byId.entries()], [["a::ok", "suite_only"]]);
});

test("toWireRow strips exactly the local classification key and nothing else", () => {
  assert.deepEqual(
    toWireRow({ test_id: "a::b", outcome: "fail", shard: "x", classification: "suite_only" }),
    { test_id: "a::b", outcome: "fail", shard: "x" },
  );
  assert.deepEqual(toWireRow({ test_id: "a::b", outcome: "pass", classification: null }), {
    test_id: "a::b",
    outcome: "pass",
  });
  assert.deepEqual(toWireRow({ test_id: "a::b", outcome: "pass" }), { test_id: "a::b", outcome: "pass" });
});

test("the POST body is BYTE-IDENTICAL with and without --classification (coord has no column; a dropped key is a void)", async () => {
  const dir = mkdtempSync(join(tmpdir(), "ingest-classification-"));
  const cls = join(dir, "test-classification.json");
  writeFileSync(cls, CLASSIFICATION_REPORT, "utf8");

  const plain = await runIngest([], RED_LOG);
  const flagged = await runIngest(["--classification", cls], RED_LOG);
  const absent = await runIngest(["--classification", join(dir, "does-not-exist.json")], RED_LOG);

  assert.equal(plain.bodies.length, 1);
  assert.equal(flagged.bodies.length, 1);
  assert.equal(absent.bodies.length, 1);
  assert.equal(flagged.bodies[0], plain.bodies[0], "--classification must not change one byte of the payload");
  assert.equal(absent.bodies[0], plain.bodies[0], "an absent file changes nothing either");
  assert.doesNotMatch(plain.bodies[0], /classification/);

  // …while the classification IS visible where it is meant to be: the log.
  assert.match(flagged.stdout, /classification from .*test-classification\.json: 3 classified id\(s\) in the file, suite_only=1 on this leg's rows/);
  assert.match(absent.stdout, /::notice title=test-results-ingest::classification file .*does-not-exist\.json not readable \(ENOENT\); classification=null on every row/);
  assert.doesNotMatch(absent.stdout, /::error/, "an absent classification is routine, never an error");
});


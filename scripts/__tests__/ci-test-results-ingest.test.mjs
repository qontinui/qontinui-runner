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

import { buildIngestBody, chunkResults } from "../ci-test-results-ingest.mjs";

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
      { test_id: "foo::bar", outcome: "pass" },
      { test_id: "foo::baz", outcome: "pass" },
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
    { test_id: "foo::bar", outcome: "fail" },
    { test_id: "foo::baz", outcome: "pass" },
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
    { test_id: "foo::bar", outcome: "pass", shard: "windows-latest" },
    { test_id: "foo::baz", outcome: "pass", shard: "windows-latest" },
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
// Regression: the ingest step must not inherit the poisoned COORD_HTTP_URL.
//
// `Poison ambient state` exports COORD_HTTP_URL to $GITHUB_ENV so the SUITE
// cannot reach coord. `Unpoison ambient state` blanks it again -- but it is
// ordered AFTER the ingest step, so on 2026-09-09 the ingest POSTed to the
// poison host and recorded `0/10603 row(s)` (job 102391645194) while still
// reporting green, because the step is `continue-on-error`.
//
// Reordering is not the fix: `Unpoison` has no `if: always()`, so a red suite
// skips it, and a red suite is precisely what this ingest exists to record.
// The step therefore carries its own `COORD_HTTP_URL`, which beats $GITHUB_ENV
// for that step alone.
//
// HOW THESE ASSERT, and why each narrowing is load-bearing. Two earlier
// revisions of these guards passed while the thing they guard was deleted:
//   * a whole-FILE substring search was satisfied by the comments above, which
//     quote the poisoned assignment verbatim -> scan a bounded STEP instead;
//   * within the step, two INDEPENDENT existence checks ("the poison appears"
//     and "something is exported") were satisfied by two DIFFERENT lines -- the
//     plain diagnostic echo, and the three other variables still being
//     exported -> assert the poison is inside the REDIRECTED BLOCK itself.
// A guard that cannot fail is worse than no guard, because the commit message
// cites it as coverage.
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
 * wrong block.
 *
 * The block ends at the first line indented at or above the step's own `-`,
 * which is every one of its keys and nothing else. Ending at "the next
 * `- name:`" instead would run a final step's scan on into the FOLLOWING JOB's
 * keys, where a job-level `env:` would be read as the step's own.
 */
function stepLines(yml, stepName) {
  const lines = yml.split("\n");
  const re = new RegExp(`^\\s*-\\s+name:\\s*(['"]?)${escapeRe(stepName)}\\1\\s*$`);
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

test("the ambient poison the step overrides is still actually EXPORTED upstream", () => {
  // Guards the other direction: if `Poison ambient state` ever stops exporting
  // COORD_HTTP_URL, the override above becomes dead weight and this test says
  // so, rather than leaving a comment describing a mechanism that is gone.
  //
  // The assignment must sit INSIDE the `{ ... } >> "$GITHUB_ENV"` group. Two
  // weaker spellings both pass while the export is gone: the step also echoes
  // the same string as a plain diagnostic, and the group exports three other
  // variables, so "poison appears somewhere" AND "something is exported" can be
  // satisfied by two unrelated lines.
  const yml = readFileSync(CI_YML, "utf8");
  const step = stepLines(yml, "Poison ambient state").join("\n");

  const exported = step.match(/\{([\s\S]*?)\}\s*>>\s*"\$GITHUB_ENV"/);
  assert.ok(
    exported,
    "expected `Poison ambient state` to export a `{ ... } >> \"$GITHUB_ENV\"` group; " +
      "without an export there is no ambient value for the ingest step to override",
  );
  assert.match(
    exported[1],
    /COORD_HTTP_URL=http:\/\/poison\.invalid/,
    "expected COORD_HTTP_URL=http://poison.invalid INSIDE the $GITHUB_ENV export " +
      "group — a diagnostic echo of the same string is not an export",
  );
});

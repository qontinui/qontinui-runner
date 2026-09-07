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

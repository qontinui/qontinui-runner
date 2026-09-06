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

import { buildIngestBody } from "../ci-test-results-ingest.mjs";

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

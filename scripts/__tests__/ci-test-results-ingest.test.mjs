#!/usr/bin/env node
// Tests for ci-test-results-ingest.mjs (Phase 1, redesigned, of plan
// `2026-08-30-runner-ci-has-no-flake-detection-so-one-flaky-test-freezes-the-train`):
// the PURE half directly, the shape of the ci.yml step that invokes it, and —
// for the invocation budget — the real CLI as a child process against a local
// coord stand-in.
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

import { execFile } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { createServer } from "node:http";
import { tmpdir } from "node:os";

import {
  buildIngestBody,
  chunkResults,
  chunkTimeoutMs,
  resolveBudgetMs,
} from "../ci-test-results-ingest.mjs";

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
// The whole-invocation budget. This step runs INSIDE the gating `test` job and
// coord's merge predicate waits for every check run on the head, so the step's
// wall time is the merge train's wait: run 34660689077 (2026-09-12) held the
// job 11m39s on ubuntu / 6m13s on windows AFTER `cargo test` had passed, and
// twelve chunks at the 120 s per-request cap is 24 min with no bound at all.
// Every expectation is a LITERAL for the same reason as the chunk tests above.
// ---------------------------------------------------------------------------

test("chunkTimeoutMs caps the next request at the smaller of the two bounds", () => {
  // Plenty of budget left: the per-request cap stands on its own.
  assert.equal(chunkTimeoutMs(1_000_000, 120_000), 120_000);
  // Less budget than the per-request cap: the budget wins.
  assert.equal(chunkTimeoutMs(30_000, 120_000), 30_000);
  // Fractions round UP to whole milliseconds (see the function for why).
  assert.equal(chunkTimeoutMs(30_000.9, 120_000), 30_001);
  assert.equal(chunkTimeoutMs(4_999.02, 120_000), 5_000);
});

test("chunkTimeoutMs refuses to start a chunk it could only abort", () => {
  // Below the 5 s floor a request would be aborted mid-flight — it still costs
  // the round trip and records nothing, so the honest answer is "do not start".
  assert.equal(chunkTimeoutMs(4_999, 120_000), null);
  assert.equal(chunkTimeoutMs(0, 120_000), null);
  assert.equal(chunkTimeoutMs(-1, 120_000), null);
  assert.equal(chunkTimeoutMs(Number.NaN, 120_000), null);
  // Exactly at the floor is still worth starting.
  assert.equal(chunkTimeoutMs(5_000, 120_000), 5_000);
});

test("chunkTimeoutMs honours a caller-lowered floor, so a small budget still sends its first chunk", () => {
  // `postResults` clamps the floor to the budget: with a 1 s budget the first
  // chunk gets 1 s rather than the invocation sending nothing at all.
  assert.equal(chunkTimeoutMs(1_000, 120_000, 1_000), 1_000);
  assert.equal(chunkTimeoutMs(999, 120_000, 1_000), null);
  // The first iteration's reading: the budget less a fraction of a millisecond.
  assert.equal(chunkTimeoutMs(999.98, 120_000, 1_000), 1_000);
  assert.equal(chunkTimeoutMs(0, 120_000, 0), null);
});

test("resolveBudgetMs reads the knob and falls back to the 3-minute default, never to unbounded", () => {
  assert.deepEqual(resolveBudgetMs("60000"), { budgetMs: 60_000, warning: null });
  assert.deepEqual(resolveBudgetMs(" 60000 "), { budgetMs: 60_000, warning: null });
  assert.deepEqual(resolveBudgetMs("60000.7"), { budgetMs: 60_000, warning: null });
  // Unset (or blank) is the ordinary case and is silent.
  for (const unset of [undefined, "", "  "]) {
    assert.deepEqual(resolveBudgetMs(unset), { budgetMs: 180_000, warning: null });
  }
  // Set but unusable falls back to the default AND says so — the one degraded
  // input this script would otherwise swallow silently.
  for (const bad of ["0", "-1", "abc", "Infinity", "3m"]) {
    const r = resolveBudgetMs(bad);
    assert.equal(r.budgetMs, 180_000, `expected the default for ${JSON.stringify(bad)}`);
    assert.match(r.warning, /is not a positive number of milliseconds; using the default 180000/);
    assert.ok(r.warning.includes(JSON.stringify(bad)), r.warning);
  }
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
// Reordering is not the fix: `Unpoison` has no `if: always()`, so a red suite
// skips it, and a red suite is precisely what this ingest exists to record. The
// step therefore carries its own `COORD_HTTP_URL`, which beats $GITHUB_ENV for
// that step alone.
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

test("the coord-report step spells its invocation budget, bounded to minutes not tens of minutes", () => {
  // The step runs inside the gating job and coord waits for it, so the bound
  // must be visible where the step is — and must stay a bound: 12 chunks at
  // the 120 s per-request cap is 24 minutes, which is the state this closes.
  const yml = readFileSync(CI_YML, "utf8");
  const env = stepEnvLines(yml, "Report test results to coord (best-effort)");
  const assigned = env.filter((l) => /^\s*COORD_INGEST_BUDGET_MS\s*:/.test(l));
  assert.equal(assigned.length, 1, "the ingest step must set COORD_INGEST_BUDGET_MS itself");
  const m = /^\s*COORD_INGEST_BUDGET_MS\s*:\s*["']?(\d+)["']?\s*(#.*)?$/.exec(assigned[0]);
  assert.ok(m, `COORD_INGEST_BUDGET_MS must be a literal number of milliseconds, got: ${assigned[0].trim()}`);
  const ms = Number(m[1]);
  assert.ok(ms >= 60_000 && ms <= 600_000, `budget ${ms}ms is outside the 1–10 minute band this step is sized for`);
});

// ---------------------------------------------------------------------------
// The budget, end to end: the REAL CLI (a child process, so this file's own
// stdout is never touched — `node --test` parses it) against a local coord
// stand-in that answers each chunk after a fixed delay. CHUNK_SIZE is 1000 and
// is deliberately not imported, so the log below carries 4,500 tests = 5
// chunks by the module's own arithmetic.
// ---------------------------------------------------------------------------

const SCRIPT = join(dirname(fileURLToPath(import.meta.url)), "..", "ci-test-results-ingest.mjs");

/** A cargo-test log with `n` passing tests, in the real timestamped shape. */
function syntheticLog(n) {
  const lines = [`${TS}running ${n} tests`];
  for (let i = 0; i < n; i++) lines.push(`${TS}test m::t${i} ... ok`);
  lines.push(
    `${TS}test result: ok. ${n} passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.00s`,
  );
  return lines.join("\n");
}

/** Serve `POST /coord/test-results/ingest`, answering after `delayMs`. */
async function slowCoord(delayMs) {
  let hits = 0;
  const server = createServer((req, res) => {
    hits += 1;
    let bytes = 0;
    req.on("data", (c) => (bytes += c.length));
    req.on("end", () => {
      setTimeout(() => {
        res.writeHead(200, { "Content-Type": "application/json" });
        res.end(JSON.stringify({ parsed: 1, persisted: 1, failed: 0, bytes }));
      }, delayMs);
    });
  });
  await new Promise((r) => server.listen(0, "127.0.0.1", r));
  return {
    url: `http://127.0.0.1:${server.address().port}`,
    hits: () => hits,
    close: () => new Promise((r) => server.close(r)),
  };
}

/** Run the CLI with `env` merged over a minimal base; resolve `{code, out}`. */
function runCli(logPath, env) {
  return new Promise((resolve) => {
    execFile(
      process.execPath,
      [SCRIPT, "--log", logPath, "--repo", "o/r", "--head-sha", "h", "--shard", "s"],
      { env: { PATH: process.env.PATH, ...env }, timeout: 30_000 },
      (err, stdout, stderr) => resolve({ code: err ? err.code : 0, out: `${stdout}${stderr}` }),
    );
  });
}

/**
 * One CLI run against a stand-in coord answering after `delayMs`, with a
 * 4,500-test log (5 chunks) and the given budget; the server and temp dir are
 * torn down whatever happens. Resolves `{code, out, hits, elapsed}`.
 */
async function runBudgetScenario({ delayMs, budgetMs }) {
  const coord = await slowCoord(delayMs);
  const dir = mkdtempSync(join(tmpdir(), "ci-ingest-budget-"));
  const logPath = join(dir, "cargo-test-output.log");
  writeFileSync(logPath, syntheticLog(4500));
  const started = Date.now();
  try {
    const r = await runCli(logPath, {
      COORD_INGEST_TOKEN: "tok",
      COORD_HTTP_URL: coord.url,
      COORD_INGEST_BUDGET_MS: String(budgetMs),
    });
    return { ...r, hits: coord.hits(), elapsed: Date.now() - started };
  } finally {
    await coord.close();
    rmSync(dir, { recursive: true, force: true });
  }
}

test("CLI: a slow coord cannot hold the invocation past COORD_INGEST_BUDGET_MS; later chunks are skipped and named", async () => {
  // 1 s per chunk against a 7,999 ms budget with the 5 s floor: chunks start
  // at ~0 / 1 / 2 s with ~7999 / ~6999 / ~5999 ms left and are sent; the
  // fourth would start at ~3 s with ~4999 ms left, under the floor, so it and
  // the fifth are skipped. Per-request overhead only ever pushes a start
  // LATER, which can only turn a send into a skip — so the boundary sits
  // ~1 s (a whole round trip) past the third start rather than midway, and a
  // loaded 2-vCPU runner cannot flip the count. (Process start is not on the
  // clock at all: the budget starts inside `postResults`.)
  const r = await runBudgetScenario({ delayMs: 1_000, budgetMs: 7_999 });

  assert.equal(r.code, 0, `best-effort: exit 0 even over budget\n${r.out}`);
  assert.equal(r.hits, 3, `expected exactly 3 requests, got ${r.hits}:\n${r.out}`);
  // The skips cost nothing: the invocation ends when the third chunk does,
  // not at the budget.
  assert.ok(r.elapsed < 7_000, `must not wait out its budget, took ${r.elapsed}ms\n${r.out}`);
  assert.match(r.out, /3000\/4500 row\(s\) recorded across 5 chunk\(s\)/);
  assert.match(r.out, /::error title=test-results-ingest::invocation budget of 7999ms exhausted/);
  // The two skipped chunks are the fourth (1000 rows) and the short fifth (500).
  assert.match(r.out, /2 of 5 chunk\(s\) \(1500 row\(s\)\) were NOT sent for o\/r@h \(shard s\)/);
  // A budget skip is not a coord failure and must not be reported as one.
  assert.doesNotMatch(r.out, /chunk\(s\) failed/);
});

test("CLI: the per-request timeout is the remaining budget, so one slow chunk is aborted at the budget and the rest skipped", async () => {
  // A 1 s budget against a 3 s coord: the FIRST chunk is started (the floor is
  // clamped to the budget) with the whole budget as its timeout, aborted at
  // ~1 s, and the remaining four are skipped. This is the wiring test: it
  // fails if the per-request timeout is not actually handed to the request,
  // if the floor clamp is dropped (nothing would be sent), or if the
  // failed-chunk arithmetic double-counts the skipped rows.
  const r = await runBudgetScenario({ delayMs: 3_000, budgetMs: 1_000 });

  assert.equal(r.code, 0, `best-effort: exit 0 even on abort\n${r.out}`);
  assert.equal(r.hits, 1, `expected exactly 1 request, got ${r.hits}:\n${r.out}`);
  assert.ok(r.elapsed < 2_500, `must abort at the budget, not wait for coord, took ${r.elapsed}ms\n${r.out}`);
  assert.match(r.out, /ABORTED by the client after \d+ms \(timeout 1000ms\)/);
  assert.match(r.out, /0\/4500 row\(s\) recorded across 5 chunk\(s\)/);
  assert.match(r.out, /1 of 5 chunk\(s\) failed — 1000 of 4500 row\(s\) were NOT recorded/);
  assert.match(r.out, /4 of 5 chunk\(s\) \(3500 row\(s\)\) were NOT sent/);
});

test("CLI: within budget every chunk is sent and no budget error is emitted", async () => {
  const r = await runBudgetScenario({ delayMs: 0, budgetMs: 60_000 });
  assert.equal(r.code, 0, r.out);
  assert.equal(r.hits, 5, r.out);
  assert.match(r.out, /4500\/4500 row\(s\) recorded across 5 chunk\(s\)/);
  assert.doesNotMatch(r.out, /::error/);
});

test("CLI: a set-but-unusable COORD_INGEST_BUDGET_MS is warned about and falls back to the default", async () => {
  const coord = await slowCoord(0);
  const dir = mkdtempSync(join(tmpdir(), "ci-ingest-budget-"));
  const logPath = join(dir, "cargo-test-output.log");
  writeFileSync(logPath, syntheticLog(1));
  let r;
  try {
    r = await runCli(logPath, {
      COORD_INGEST_TOKEN: "tok",
      COORD_HTTP_URL: coord.url,
      COORD_INGEST_BUDGET_MS: "3m",
    });
  } finally {
    await coord.close();
    rmSync(dir, { recursive: true, force: true });
  }
  assert.equal(r.code, 0, r.out);
  assert.match(r.out, /::warning title=test-results-ingest::COORD_INGEST_BUDGET_MS="3m" is not a positive number/);
  assert.match(r.out, /using the default 180000/);
  assert.match(r.out, /1\/1 row\(s\) recorded/);
});


#!/usr/bin/env node
// Unit tests for the PURE half of ci-flake-escalate.mjs (Phase 2 of plan
// `2026-08-30-runner-ci-has-no-flake-detection-so-one-flaky-test-freezes-the-train`).
//
// Uses Node's built-in `node:test` runner, mirroring
// `scripts/__tests__/ci-test-results-ingest.test.mjs`.
//
// Run with:
//   node --test scripts/__tests__/ci-flake-escalate.test.mjs

import test from "node:test";
import assert from "node:assert/strict";

import {
  DEFAULT_MIN_OCCURRENCES,
  classifyFlakinessResponse,
  issueTitle,
  planIssueActions,
  renderIssueBody,
  selectEscalations,
} from "../ci-flake-escalate.mjs";

// The test #1444 fixed, at the numbers coord served on 2026-09-10.
const WEDGE =
  "qontinui_runner_lib::wedge_diagnostics::tests::a_spinning_child_reports_meaningful_cpu";
const MANIFEST =
  "qontinui_runner_lib::mcp::ui_bridge::manifest_drift_tests::sdk_manifest_routes_are_exposed_by_runner";

function prior(flakeRate, sampleSize = 20, modal = "pass") {
  return { flake_rate: flakeRate, sample_size: sampleSize, modal_outcome: modal };
}

const LIVE_PRIORS = {
  [WEDGE]: prior(0.4),
  [MANIFEST]: prior(0.1),
  "qontinui_runner_lib::stable::always_green": prior(0.0),
};

// ---------------------------------------------------------------------------
// classifyFlakinessResponse — the three-way discriminator, read not guessed
// ---------------------------------------------------------------------------

test("history_read=ok with priors is an observation", () => {
  const v = classifyFlakinessResponse({
    history_read: "ok",
    min_k: 20,
    window: 20,
    priors: LIVE_PRIORS,
    repo: "qontinui/qontinui-runner",
  });
  assert.equal(v.kind, "ok");
  assert.equal(v.minK, 20);
  assert.equal(v.window, 20);
  assert.equal(Object.keys(v.priors).length, 3);
});

test("history_read=thin is not an error and not a clean bill of health", () => {
  const v = classifyFlakinessResponse({ history_read: "thin", min_k: 20, window: 20, priors: {} });
  assert.equal(v.kind, "thin");
  assert.deepEqual(v.priors, {});
});

test("history_read=failed is UNKNOWN even when priors is present", () => {
  // Measured live 2026-09-12: exactly this body, after ~60 s.
  const v = classifyFlakinessResponse({
    history_read: "failed",
    min_k: 20,
    priors: {},
    repo: "qontinui/qontinui-runner",
    window: 20,
  });
  assert.equal(v.kind, "failed");
});

test("a pre-#2075 coord (no history_read) with EMPTY priors is UNKNOWN, never thin", () => {
  const v = classifyFlakinessResponse({ min_k: 20, window: 20, priors: {} });
  assert.equal(v.kind, "failed");
  assert.match(v.reason, /omits `history_read`/);
});

test("a pre-#2075 coord (no history_read) with non-empty priors is still an observation", () => {
  const v = classifyFlakinessResponse({ min_k: 20, window: 20, priors: LIVE_PRIORS });
  assert.equal(v.kind, "ok");
});

test("an unrecognised history_read token is UNKNOWN", () => {
  const v = classifyFlakinessResponse({ history_read: "partial", priors: LIVE_PRIORS });
  assert.equal(v.kind, "failed");
  assert.match(v.reason, /unrecognised/);
});

test("non-object bodies and bodies without priors are UNKNOWN", () => {
  for (const body of [null, undefined, "ok", 42, [], { history_read: "ok" }, { priors: [] }]) {
    assert.equal(classifyFlakinessResponse(body).kind, "failed", JSON.stringify(body));
  }
});

// ---------------------------------------------------------------------------
// selectEscalations — the plan's "3 occurrences" rule, in coord's units
// ---------------------------------------------------------------------------

test("the default threshold is the plan's 3 occurrences", () => {
  assert.equal(DEFAULT_MIN_OCCURRENCES, 3);
});

test("0.400 over 20 escalates (8 occurrences); 0.100 over 20 does not (2)", () => {
  const out = selectEscalations(LIVE_PRIORS);
  assert.deepEqual(
    out.map((e) => [e.testId, e.occurrences]),
    [[WEDGE, 8]],
  );
  assert.equal(out[0].flakeRate, 0.4);
  assert.equal(out[0].sampleSize, 20);
  assert.equal(out[0].modalOutcome, "pass");
});

test("exactly 3 occurrences is at threshold and escalates; 2 is below", () => {
  const out = selectEscalations({ three: prior(0.15), two: prior(0.1) });
  assert.deepEqual(
    out.map((e) => e.testId),
    ["three"],
  );
});

test("occurrences are computed from the sample size, not assumed to be 20", () => {
  // 0.1 over 40 runs is 4 disagreeing runs — over threshold.
  const out = selectEscalations({ t: prior(0.1, 40) });
  assert.equal(out.length, 1);
  assert.equal(out[0].occurrences, 4);
});

test("--min-occurrences is honoured", () => {
  assert.equal(selectEscalations(LIVE_PRIORS, { minOccurrences: 9 }).length, 0);
  assert.equal(selectEscalations(LIVE_PRIORS, { minOccurrences: 2 }).length, 2);
});

test("ordering is most-flaky first, then by occurrences, then by id", () => {
  const out = selectEscalations({
    b: prior(0.5),
    a: prior(0.5),
    c: prior(0.9),
    d: prior(0.5, 40), // same rate as a/b, more occurrences
  });
  assert.deepEqual(
    out.map((e) => e.testId),
    ["c", "d", "a", "b"],
  );
});

test("malformed priors are skipped, never escalated on a guess", () => {
  const out = selectEscalations({
    ok: prior(0.5),
    nan: { flake_rate: "lots", sample_size: 20 },
    missing: { sample_size: 20 },
    nul: null,
    str: "0.9",
    zeroSample: { flake_rate: 1, sample_size: 0 },
  });
  assert.deepEqual(
    out.map((e) => e.testId),
    ["ok"],
  );
});

test("a consistently failing test with a few passes still escalates (it flips)", () => {
  const out = selectEscalations({ t: prior(0.15, 20, "fail") });
  assert.equal(out.length, 1);
  assert.equal(out[0].modalOutcome, "fail");
});

// ---------------------------------------------------------------------------
// issueTitle — the idempotency key
// ---------------------------------------------------------------------------

test("the title is `flaky test: <id>` and stable", () => {
  assert.equal(issueTitle(WEDGE), `flaky test: ${WEDGE}`);
  assert.equal(issueTitle(WEDGE), issueTitle(WEDGE));
});

test("a title over GitHub's limit is truncated deterministically and stays unique", () => {
  const longA = "a::" + "x".repeat(400) + "::one";
  const longB = "a::" + "x".repeat(400) + "::two";
  const ta = issueTitle(longA);
  const tb = issueTitle(longB);
  assert.ok(ta.length <= 256, `too long: ${ta.length}`);
  assert.ok(ta.startsWith("flaky test: a::xxx"));
  assert.notEqual(ta, tb, "two long ids sharing a prefix must not collide");
  assert.equal(ta, issueTitle(longA), "truncation must be deterministic");
});

// ---------------------------------------------------------------------------
// planIssueActions — one test owns one issue forever
// ---------------------------------------------------------------------------

const ESC = selectEscalations(LIVE_PRIORS);

test("no existing issue -> create", () => {
  const actions = planIssueActions(ESC, []);
  assert.equal(actions.length, 1);
  assert.equal(actions[0].action, "create");
  assert.equal(actions[0].title, issueTitle(WEDGE));
  assert.deepEqual(actions[0].duplicates, []);
});

test("an OPEN issue with the exact title -> update, never a second issue", () => {
  const actions = planIssueActions(ESC, [{ number: 7, title: issueTitle(WEDGE), state: "OPEN" }]);
  assert.deepEqual(
    actions.map((a) => [a.action, a.number]),
    [["update", 7]],
  );
});

test("a CLOSED issue with the exact title -> reopen (the test flaked again)", () => {
  const actions = planIssueActions(ESC, [{ number: 7, title: issueTitle(WEDGE), state: "CLOSED" }]);
  assert.deepEqual(
    actions.map((a) => [a.action, a.number]),
    [["reopen", 7]],
  );
});

test("matching is on the EXACT title — a near-miss does not adopt", () => {
  const actions = planIssueActions(ESC, [
    { number: 7, title: issueTitle(WEDGE) + " (old)", state: "OPEN" },
    { number: 8, title: "flaky test: something_else", state: "OPEN" },
  ]);
  assert.equal(actions[0].action, "create");
});

test("duplicate titles: the lowest number owns, the rest are reported", () => {
  const actions = planIssueActions(ESC, [
    { number: 12, title: issueTitle(WEDGE), state: "OPEN" },
    { number: 9, title: issueTitle(WEDGE), state: "CLOSED" },
    { number: 15, title: issueTitle(WEDGE), state: "OPEN" },
  ]);
  assert.equal(actions[0].number, 9);
  assert.equal(actions[0].action, "reopen");
  assert.deepEqual(actions[0].duplicates, [12, 15]);
});

test("issues with no title are ignored rather than crashing the plan", () => {
  const actions = planIssueActions(ESC, [null, {}, { number: 3 }]);
  assert.equal(actions[0].action, "create");
});

// ---------------------------------------------------------------------------
// renderIssueBody — every number is coord's, and the run is linked
// ---------------------------------------------------------------------------

test("the body carries the rate, the occurrence count, the window and the run link", () => {
  const body = renderIssueBody(ESC[0], {
    repo: "qontinui/qontinui-runner",
    minK: 20,
    window: 20,
    runUrl: "https://github.com/qontinui/qontinui-runner/actions/runs/1",
    readAt: "2026-09-12T06:00:00.000Z",
  });
  assert.match(
    body,
    /`qontinui_runner_lib::wedge_diagnostics::tests::a_spinning_child_reports_meaningful_cpu`/,
  );
  assert.match(body, /\*\*0\.400\*\* \(40\.0%\)/);
  assert.match(body, /\*\*8\*\* of 20 runs disagreed/);
  assert.match(body, /last 20 runs per test \(min_k 20\)/);
  assert.match(
    body,
    /\[this run\]\(https:\/\/github\.com\/qontinui\/qontinui-runner\/actions\/runs\/1\)/,
  );
  assert.match(body, /2026-09-12T06:00:00\.000Z/);
  assert.match(body, /test or production/);
});

test("the body degrades honestly when the optional context is absent", () => {
  const body = renderIssueBody(ESC[0]);
  assert.match(body, /a manual run/);
  assert.match(body, /last \? runs per test \(min_k \?\)/);
});

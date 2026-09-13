#!/usr/bin/env node
// Unit tests for ci-flake-escalate.mjs — the pure half directly, the IO half through a recording `gh` runner (Phase 2 of plan
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
  FLAKE_LABEL,
  MAIN_RED_CAP,
  MAX_TITLE_LEN,
  TITLE_PREFIX,
  applyAction,
  classifyFlakinessResponse,
  ensureLabel,
  fetchRunJobs,
  issueTitle,
  listFlakeIssues,
  mergeEscalations,
  planIssueActions,
  renderIssueBody,
  renderMainRedComment,
  selectEscalations,
  selectMainRedEscalations,
  sliceGatingStep,
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

test("history_read=ok with EMPTY priors is trusted as an observation (nothing to escalate)", () => {
  // Deliberate policy, pinned so a later "make it UNKNOWN" edit is a choice
  // and not a drift: coord's `ok` means the read succeeded and at least one
  // test reached the window, so an empty map here is coord's own claim and
  // the script has no better information to overrule it with.
  const v = classifyFlakinessResponse({ history_read: "ok", min_k: 20, window: 20, priors: {} });
  assert.equal(v.kind, "ok");
  assert.deepEqual(selectEscalations(v.priors), []);
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
  assert.equal(MAX_TITLE_LEN, 250, "headroom under GitHub's 256-character cap");
  assert.ok(ta.length <= MAX_TITLE_LEN, `too long: ${ta.length}`);
  assert.ok(ta.startsWith(`${TITLE_PREFIX}a::xxx`));
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

test("duplicate titles: the lowest OPEN issue owns; a closed one is never reopened beside it", () => {
  // Reopening #9 here would leave THREE open issues for one test while the
  // run claims to dedupe — the review of this script caught exactly that.
  const actions = planIssueActions(ESC, [
    { number: 12, title: issueTitle(WEDGE), state: "OPEN" },
    { number: 9, title: issueTitle(WEDGE), state: "CLOSED" },
    { number: 15, title: issueTitle(WEDGE), state: "OPEN" },
  ]);
  assert.equal(actions[0].number, 12);
  assert.equal(actions[0].action, "update");
  assert.deepEqual(actions[0].duplicates, [15, 9]);
});

test("duplicate titles, all CLOSED: the lowest number is reopened", () => {
  const actions = planIssueActions(ESC, [
    { number: 12, title: issueTitle(WEDGE), state: "CLOSED" },
    { number: 9, title: issueTitle(WEDGE), state: "CLOSED" },
  ]);
  assert.equal(actions[0].number, 9);
  assert.equal(actions[0].action, "reopen");
  assert.deepEqual(actions[0].duplicates, [12]);
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
  assert.match(body, /rate rule/);
  assert.match(body, /last 20 observations per test \(min_k 20\)/);
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
  assert.match(body, /last \? observations per test \(min_k \?\)/);
});

// ---------------------------------------------------------------------------
// The tally rule — coord's per-outcome `outcomes` beats the rate when served
// ---------------------------------------------------------------------------

/** A coord prior WITH the per-outcome tally (qontinui-coord since the 2026-09-12 read rewrite). */
function tallied(outcomes, extra = {}) {
  const total = Object.values(outcomes).reduce((a, b) => a + b, 0);
  const modal = Object.entries(outcomes).sort((a, b) => b[1] - a[1])[0][0];
  return {
    sample_size: total,
    modal_outcome: modal,
    flake_rate: 1 - outcomes[modal] / total,
    outcomes,
    recent_failures: [],
    ...extra,
  };
}

test("tally: fail+error >= threshold with at least one pass escalates, and records the rule", () => {
  const out = selectEscalations({ t: tallied({ pass: 10, fail: 2, error: 1 }) });
  assert.equal(out.length, 1);
  assert.equal(out[0].occurrences, 3);
  assert.equal(out[0].rule, "tally");
  assert.deepEqual(out[0].outcomes, { pass: 10, fail: 2, error: 1 });
});

test("tally: a test that ONLY fails is broken, not flaky — no escalation even at rate 0", () => {
  assert.deepEqual(selectEscalations({ t: tallied({ fail: 20 }) }), []);
  // fail+error only, non-zero rate (modal fail, 5 errors disagree): the
  // early `flake_rate <= 0` guard does NOT catch this one — only the tally
  // rule's "passed at least once" does. Delete that line and this fails.
  assert.deepEqual(selectEscalations({ t: tallied({ fail: 15, error: 5 }) }), []);
  assert.deepEqual(selectEscalations({ t: tallied({ fail: 17, unknown: 3 }) }), []);
  // …and modal=fail with a few passes is a flip (the rate arm's own case) —
  // the tally says so too, on its failures.
  const out = selectEscalations({ t: tallied({ fail: 17, pass: 3 }) });
  assert.equal(out.length, 1);
  assert.equal(out[0].occurrences, 17);
});

test("tally: a skip/pass split (cfg_attr(windows, ignore)) is NOT a flake, whatever the rate says", () => {
  const p = tallied({ pass: 10, skip: 10 });
  assert.ok(p.flake_rate >= 0.5, "the rate alone would escalate this");
  assert.equal(
    selectEscalations({ t: { ...p, outcomes: undefined } }).length,
    1,
    "rate rule: escalates",
  );
  assert.deepEqual(selectEscalations({ t: p }), [], "tally rule: does not");
});

test("tally: below threshold does not escalate; --min-occurrences applies to failures", () => {
  assert.deepEqual(selectEscalations({ t: tallied({ pass: 18, fail: 2 }) }), []);
  assert.equal(
    selectEscalations({ t: tallied({ pass: 18, fail: 2 }) }, { minOccurrences: 2 }).length,
    1,
  );
});

test("tally: recent_failures ride along into the escalation", () => {
  const rf = [
    {
      outcome: "fail",
      head_sha: "abc",
      shard: "ubuntu-22.04",
      observed_at: "2026-09-12T01:00:00Z",
    },
  ];
  const out = selectEscalations({ t: tallied({ pass: 17, fail: 3 }, { recent_failures: rf }) });
  assert.deepEqual(out[0].recentFailures, rf);
});

test("tally: a body carries the tally, the SHA table and the iterated-PR caveat", () => {
  const [e] = selectEscalations({
    t: tallied(
      { pass: 17, fail: 3 },
      {
        recent_failures: [
          {
            outcome: "fail",
            head_sha: "abcdef0123456789",
            shard: "ubuntu-22.04",
            observed_at: "2026-09-12T01:00:00Z",
          },
        ],
      },
    ),
  });
  const body = renderIssueBody(e, { repo: "o/r", minK: 20, window: 200, readAt: "x" });
  assert.match(body, /tally rule/);
  assert.match(body, /fail 3 · pass 17/);
  assert.match(body, /commit\/abcdef0123456789/);
  assert.match(body, /broken PR that was iterated/);
  assert.equal(
    body,
    renderIssueBody(e, { repo: "o/r", minK: 20, window: 200, readAt: "x" }),
    "deterministic",
  );
});

// ---------------------------------------------------------------------------
// The main-red arm — failing tests of a `failure` run on main
// ---------------------------------------------------------------------------

const TS = "2026-09-12T07:26:07.5955615Z ";

const RED_UBUNTU_LOG = [
  `${TS}     Running \`/home/runner/work/x/src-tauri/target/debug/deps/qontinui_runner_lib-a9341426b1692ff6\``,
  `${TS}running 3 tests`,
  `${TS}test spill::tests::evicts_oldest ... FAILED`,
  `${TS}test spill::tests::keeps_newest ... ok`,
  `${TS}test spill::tests::ignored_one ... ignored`,
  `${TS}test result: FAILED. 1 passed; 1 failed; 1 ignored; 0 measured; 0 filtered out; finished in 0.01s`,
].join("\n");

const RED_WINDOWS_LOG = RED_UBUNTU_LOG.replace(
  "/home/runner/work/x/src-tauri/target/debug/deps/qontinui_runner_lib-a9341426b1692ff6",
  "D:\\a\\x\\src-tauri\\target\\debug\\deps\\qontinui_runner_lib-b1b2b3b4b5b6b7b8.exe",
);

const COMPILE_ERROR_LOG = [
  `${TS}   Compiling qontinui-runner v1.0.10`,
  `${TS}error[E0425]: cannot find value \`x\` in this scope`,
  `${TS}error: could not compile \`qontinui-runner\``,
].join("\n");

test("main-red: failing tests come from failed test jobs only, binary-prefixed, with their shard", () => {
  const r = selectMainRedEscalations([
    { name: "test (ubuntu-22.04)", conclusion: "failure", logText: RED_UBUNTU_LOG },
    { name: "test (windows-latest)", conclusion: "success", logText: RED_UBUNTU_LOG },
    { name: "clippy-windows", conclusion: "failure", logText: RED_UBUNTU_LOG },
  ]);
  assert.deepEqual(r.escalations, [
    {
      testId: "qontinui_runner_lib::spill::tests::evicts_oldest",
      mainRed: { shards: ["ubuntu-22.04"] },
    },
  ]);
  assert.equal(r.failingCount, 1);
  assert.equal(r.capped, false);
  assert.deepEqual(r.unparsed, []);
});

test("main-red: the same test failing on both legs is ONE escalation carrying both shards", () => {
  const r = selectMainRedEscalations([
    { name: "test (ubuntu-22.04)", conclusion: "failure", logText: RED_UBUNTU_LOG },
    { name: "test (windows-latest)", conclusion: "failure", logText: RED_WINDOWS_LOG },
  ]);
  assert.equal(r.escalations.length, 1);
  assert.deepEqual(r.escalations[0].mainRed.shards, ["ubuntu-22.04", "windows-latest"]);
});

test("main-red: a failed job with no cargo output is reported unparsed, never as zero flakes", () => {
  const r = selectMainRedEscalations([
    { name: "test (ubuntu-22.04)", conclusion: "failure", logText: COMPILE_ERROR_LOG },
    { name: "test (windows-latest)", conclusion: "failure", logText: null },
  ]);
  assert.deepEqual(r.escalations, []);
  assert.deepEqual(r.unparsed, ["test (ubuntu-22.04)", "test (windows-latest)"]);
});

test("main-red: a mass failure is capped to nothing — a broken commit is not N flakes", () => {
  const lines = [`${TS}running 12 tests`];
  for (let i = 0; i <= MAIN_RED_CAP; i += 1) lines.push(`${TS}test m::t${i} ... FAILED`);
  lines.push(
    `${TS}test result: FAILED. 0 passed; 11 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s`,
  );
  const r = selectMainRedEscalations([
    { name: "test (ubuntu-22.04)", conclusion: "failure", logText: lines.join("\n") },
  ]);
  assert.equal(r.failingCount, MAIN_RED_CAP + 1);
  assert.equal(r.capped, true);
  assert.deepEqual(r.escalations, []);
});

test("merge: both arms land on one escalation per test id; rate order first, then main-red-only by id", () => {
  const rate = selectEscalations({ "b::t": tallied({ pass: 17, fail: 3 }) });
  const merged = mergeEscalations(rate, [
    { testId: "b::u", mainRed: { shards: ["windows-latest"] } },
    { testId: "b::t", mainRed: { shards: ["ubuntu-22.04"] } },
  ]);
  assert.deepEqual(
    merged.map((e) => [e.testId, e.rule ?? null, e.mainRed?.shards ?? null]),
    [
      ["b::t", "tally", ["ubuntu-22.04"]],
      ["b::u", null, ["windows-latest"]],
    ],
  );
});

test("main-red-only body: no rate rows, no coord paragraphs, the push-to-main row linking the CI run", () => {
  const body = renderIssueBody(
    { testId: "b::u", mainRed: { shards: ["windows-latest"] } },
    {
      repo: "o/r",
      runUrl: "https://github.com/o/r/actions/runs/9",
      ciRunUrl: "https://github.com/o/r/actions/runs/5",
      readAt: "x",
    },
  );
  assert.doesNotMatch(body, /flake_rate/);
  assert.doesNotMatch(body, /POST \/coord\/test-flakiness/, "no coord read happened");
  assert.doesNotMatch(body, /Read the number before/);
  assert.match(body, /^a push to `main` failed this test/);
  assert.match(
    body,
    /failed on a push to `main` \| `windows-latest` — \[the run\]\(https:\/\/github\.com\/o\/r\/actions\/runs\/5\)/,
  );
  assert.match(
    body,
    /escalated by \| \[this run\]\(https:\/\/github\.com\/o\/r\/actions\/runs\/9\)/,
  );
  assert.match(body, /test or production/);
});

test("main-red comment: an event naming the shards and the CI run, never the body", () => {
  const c = renderMainRedComment(
    { testId: "b::u", mainRed: { shards: ["ubuntu-22.04", "windows-latest"] } },
    { ciRunUrl: "https://github.com/o/r/actions/runs/5", readAt: "2026-09-12T01:00:00.000Z" },
  );
  assert.match(
    c,
    /^Failed again on a push to `main` — `ubuntu-22.04`, `windows-latest` — \[the run\]/,
  );
  assert.match(c, /body above is coord's/);
});

test("applyAction: a main-red-only escalation on an existing issue comments, never edits the body", () => {
  const { calls, gh } = recorder();
  applyAction(
    { action: "update", number: 7, escalation: { testId: "b::u", mainRed: { shards: ["x"] } } },
    "BODY",
    "o/r",
    gh,
    { comment: "EVENT" },
  );
  assert.deepEqual(
    calls.map((c) => c.args.slice(0, 2)),
    [["issue", "comment"]],
  );
  assert.equal(calls[0].opts.input, "EVENT");
});

test("applyAction: a closed issue hit by the main-red arm is reopened, then commented", () => {
  const { calls, gh } = recorder();
  applyAction(
    { action: "reopen", number: 7, escalation: { testId: "b::u", mainRed: { shards: ["x"] } } },
    "BODY",
    "o/r",
    gh,
    { comment: "EVENT" },
  );
  assert.deepEqual(
    calls.map((c) => c.args.slice(0, 2)),
    [
      ["issue", "reopen"],
      ["issue", "comment"],
    ],
  );
});

test("applyAction: an escalation with a coord reading AND a push failure edits the body and comments", () => {
  const { calls, gh } = recorder();
  applyAction(
    {
      action: "update",
      number: 7,
      escalation: { testId: "t", rule: "tally", mainRed: { shards: ["x"] } },
    },
    "BODY",
    "o/r",
    gh,
    { comment: "EVENT" },
  );
  assert.deepEqual(
    calls.map((c) => c.args.slice(0, 2)),
    [
      ["issue", "edit"],
      ["issue", "comment"],
    ],
  );
  assert.equal(calls[0].opts.input, "BODY");
  assert.equal(calls[1].opts.input, "EVENT");
});

test("applyAction: create writes the body whichever arm produced it", () => {
  const { calls, gh } = recorder({ "issue create": "u" });
  applyAction(
    {
      action: "create",
      title: "flaky test: b::u",
      escalation: { testId: "b::u", mainRed: { shards: ["x"] } },
    },
    "BODY",
    "o/r",
    gh,
    { comment: "EVENT" },
  );
  assert.deepEqual(
    calls.map((c) => c.args.slice(0, 2)),
    [["issue", "create"]],
  );
  assert.equal(calls[0].opts.input, "BODY");
});

test("fetchRunJobs: lists the run's jobs and reads the log of failed test jobs only", () => {
  const { calls, gh } = recorder({
    "api repos/o/r/actions/runs/5/jobs?per_page=100": JSON.stringify({
      jobs: [
        { id: 1, name: "test (ubuntu-22.04)", conclusion: "failure" },
        { id: 2, name: "test (windows-latest)", conclusion: "success" },
        { id: 3, name: "clippy-windows", conclusion: "failure" },
      ],
    }),
    "api repos/o/r/actions/jobs/1/logs": RED_UBUNTU_LOG,
  });
  const jobs = fetchRunJobs("o/r", "5", { gh });
  assert.deepEqual(
    jobs.map((j) => [j.name, j.logText === null ? null : "log"]),
    [
      ["test (ubuntu-22.04)", "log"],
      ["test (windows-latest)", null],
      ["clippy-windows", null],
    ],
  );
  assert.equal(
    calls.length,
    2,
    "one list call and one log read — never the green or non-test jobs",
  );
});

test("fetchRunJobs: pins the attempt when given, so a re-run cannot hide the triggering failure", () => {
  const { calls, gh } = recorder({
    "api repos/o/r/actions/runs/5/attempts/2/jobs?per_page=100": JSON.stringify({ jobs: [] }),
  });
  fetchRunJobs("o/r", "5", { attempt: "2", gh });
  assert.equal(calls[0].args[1], "repos/o/r/actions/runs/5/attempts/2/jobs?per_page=100");
});

test("fetchRunJobs: a log that cannot be read is marked unreadable, and the arm goes dark on it", () => {
  const { gh } = recorder({
    "api repos/o/r/actions/runs/5/jobs?per_page=100": JSON.stringify({
      jobs: [{ id: 1, name: "test (ubuntu-22.04)", conclusion: "failure" }],
    }),
    "api repos/o/r/actions/jobs/1/logs": () => {
      const e = new Error("HTTP 403");
      e.stderr = "Resource not accessible by integration (HTTP 403)";
      throw e;
    },
  });
  const jobs = fetchRunJobs("o/r", "5", { gh });
  assert.equal(jobs[0].unreadable, true);
  const r = selectMainRedEscalations(jobs);
  assert.deepEqual(r.unreadable, ["test (ubuntu-22.04)"]);
  assert.deepEqual(
    r.unparsed,
    [],
    "unreadable is NOT unparsed — one is a permissions failure, the other a broken build",
  );
  assert.deepEqual(r.escalations, []);
});

// ---------------------------------------------------------------------------
// sliceGatingStep — only the gate's output is parsed, never the smoke step's
// ---------------------------------------------------------------------------

/** A job log with the shape ci.yml's `test (…)` job really has: gate, ingest, smoke, build. */
const FULL_JOB_LOG = [
  `${TS}##[group]Run pnpm test`,
  `${TS}[36;1mpnpm test[0m`,
  `${TS}##[endgroup]`,
  `${TS}> vitest run`,
  `${TS}##[group]Run if [ "$RUNNER_OS" = "Linux" ]; then`,
  `${TS}[36;1mif [ "$RUNNER_OS" = "Linux" ]; then[0m`,
  `${TS}[36;1mcargo test --verbose 2>&1 | tee "$GITHUB_WORKSPACE/cargo-test-output.log"[0m`,
  `${TS}##[endgroup]`,
  `${TS}     Running \`/home/runner/work/x/src-tauri/target/debug/deps/qontinui_runner-a9341426b1692ff6\``,
  `${TS}running 2 tests`,
  `${TS}test spill::tests::evicts_oldest ... FAILED`,
  `${TS}test util::path_extraction::tests::test_extract_paths_via_ai_real_call ... ignored`,
  `${TS}test result: FAILED. 0 passed; 1 failed; 1 ignored; 0 measured; 0 filtered out; finished in 0.01s`,
  `${TS}##[group]Run node scripts/ci-test-results-ingest.mjs \\`,
  `${TS}##[endgroup]`,
  `${TS}[ci-test-results-ingest] chunk 1/1 (2 rows) -> HTTP 200 in 10ms`,
  `${TS}##[group]Run if [ -z "$ANTHROPIC_API_KEY" ]; then`,
  `${TS}[36;1mcargo test --bin qontinui-runner -- --ignored --exact \\[0m`,
  `${TS}##[endgroup]`,
  `${TS}     Running \`/home/runner/work/x/src-tauri/target/debug/deps/qontinui_runner-a9341426b1692ff6\``,
  `${TS}running 1 test`,
  `${TS}test util::path_extraction::tests::test_extract_paths_via_ai_real_call ... FAILED`,
  `${TS}test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 9.01s`,
  `${TS}##[group]Run pnpm run tauri build --debug --no-bundle`,
  `${TS}##[endgroup]`,
  `${TS}error: build failed`,
].join("\n");

test("sliceGatingStep: keeps the gate's output and drops the smoke step's FAILED line", () => {
  const sliced = sliceGatingStep(FULL_JOB_LOG);
  assert.match(sliced, /evicts_oldest \.\.\. FAILED/);
  assert.match(sliced, /test_extract_paths_via_ai_real_call \.\.\. ignored/);
  assert.doesNotMatch(sliced, /test_extract_paths_via_ai_real_call \.\.\. FAILED/);
  assert.doesNotMatch(sliced, /ci-test-results-ingest/);
  const r = selectMainRedEscalations([
    { name: "test (ubuntu-22.04)", conclusion: "failure", logText: FULL_JOB_LOG },
  ]);
  assert.deepEqual(
    r.escalations.map((e) => e.testId),
    ["qontinui_runner::spill::tests::evicts_oldest"],
    "the smoke test's failure never reds the job and must not be filed as a flake",
  );
});

test("sliceGatingStep: a log with steps but no gating step is EMPTY (unparsed), never the whole log", () => {
  const noGate = FULL_JOB_LOG.replace("cargo test --verbose", "cargo build --verbose");
  assert.equal(sliceGatingStep(noGate), "");
  const r = selectMainRedEscalations([
    { name: "test (ubuntu-22.04)", conclusion: "failure", logText: noGate },
  ]);
  assert.deepEqual(r.unparsed, ["test (ubuntu-22.04)"]);
});

test("sliceGatingStep: a log with no step markers at all is returned whole", () => {
  assert.equal(sliceGatingStep(RED_UBUNTU_LOG), RED_UBUNTU_LOG);
});

test("main-red: exactly MAIN_RED_CAP failing tests is NOT capped; one more is", () => {
  const mk = (n) => {
    const lines = [`${TS}running ${n} tests`];
    for (let i = 0; i < n; i += 1) lines.push(`${TS}test m::t${i} ... FAILED`);
    lines.push(
      `${TS}test result: FAILED. 0 passed; ${n} failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s`,
    );
    return [{ name: "test (ubuntu-22.04)", conclusion: "failure", logText: lines.join("\n") }];
  };
  assert.equal(selectMainRedEscalations(mk(MAIN_RED_CAP)).escalations.length, MAIN_RED_CAP);
  assert.equal(selectMainRedEscalations(mk(MAIN_RED_CAP + 1)).capped, true);
});

// ---------------------------------------------------------------------------
// IO half — the exact `gh` argv each action issues, through a recording runner
// (flag drift is where a CLI wrapper breaks, and the pure tests cannot see it)
// ---------------------------------------------------------------------------

function recorder(responses = {}) {
  const calls = [];
  const gh = (args, opts) => {
    calls.push({ args, opts });
    const key = args.slice(0, 2).join(" ");
    const r = responses[key];
    return typeof r === "function" ? r(args, opts) : (r ?? "");
  };
  return { calls, gh };
}

test("ensureLabel is a single --force create (idempotent under case and race)", () => {
  const { calls, gh } = recorder();
  ensureLabel("o/r", gh);
  assert.equal(calls.length, 1);
  assert.deepEqual(calls[0].args.slice(0, 4), ["label", "create", FLAKE_LABEL, "--force"]);
  assert.equal(calls[0].opts.repo, "o/r");
});

test("listFlakeIssues unions the labelled list with the title search, by number", () => {
  const { calls, gh } = recorder({
    "issue list": (args) =>
      args.includes("--label")
        ? JSON.stringify([{ number: 1, title: "flaky test: a", state: "OPEN" }])
        : JSON.stringify([
            { number: 1, title: "flaky test: a", state: "OPEN" },
            { number: 2, title: "flaky test: b (hand-filed, no label)", state: "OPEN" },
          ]),
  });
  const issues = listFlakeIssues("o/r", gh);
  assert.deepEqual(issues.map((i) => i.number).sort(), [1, 2]);
  assert.equal(calls.length, 2);
  assert.ok(calls[0].args.includes("--label") && calls[0].args.includes(FLAKE_LABEL));
  assert.ok(calls[1].args.includes("--search"));
  assert.ok(calls[1].args.includes(`"${TITLE_PREFIX.trim()}" in:title`));
  for (const c of calls) {
    assert.ok(
      c.args.includes("--state") && c.args.includes("all"),
      "closed issues must be visible",
    );
    assert.ok(c.args.includes("number,title,state"));
  }
});

test("applyAction create: title + label + body via stdin, never on argv", () => {
  const { calls, gh } = recorder({ "issue create": "https://github.com/o/r/issues/42\n" });
  const ref = applyAction({ action: "create", title: "flaky test: x" }, "BODY", "o/r", gh);
  assert.equal(ref, "https://github.com/o/r/issues/42");
  assert.deepEqual(calls[0].args, [
    "issue",
    "create",
    "--title",
    "flaky test: x",
    "--label",
    FLAKE_LABEL,
    "--body-file",
    "-",
  ]);
  assert.equal(calls[0].opts.input, "BODY");
});

test("applyAction update: edit re-adds the label and refreshes the body", () => {
  const { calls, gh } = recorder();
  const ref = applyAction({ action: "update", number: 7 }, "BODY", "o/r", gh);
  assert.equal(ref, "#7");
  assert.equal(calls.length, 1);
  assert.deepEqual(calls[0].args, [
    "issue",
    "edit",
    "7",
    "--add-label",
    FLAKE_LABEL,
    "--body-file",
    "-",
  ]);
  assert.equal(calls[0].opts.input, "BODY");
});

test("applyAction reopen: reopens THEN edits (the fall-through is load-bearing)", () => {
  const { calls, gh } = recorder();
  const ref = applyAction({ action: "reopen", number: 7 }, "BODY", "o/r", gh);
  assert.equal(ref, "#7");
  assert.deepEqual(
    calls.map((c) => c.args.slice(0, 2)),
    [
      ["issue", "reopen"],
      ["issue", "edit"],
    ],
  );
  assert.equal(calls[1].opts.input, "BODY");
});

test("applyAction refuses an unknown action rather than guessing", () => {
  const { gh } = recorder();
  assert.throws(
    () => applyAction({ action: "close", number: 7 }, "BODY", "o/r", gh),
    /unknown action/,
  );
});

test("a test id with shell metacharacters reaches gh as one argv entry, unchanged", () => {
  const id = 'crate::mod::test $(rm -rf /) `x` ; & | > "q"';
  const { calls, gh } = recorder({ "issue create": "u" });
  applyAction({ action: "create", title: issueTitle(id) }, "BODY", "o/r", gh);
  assert.equal(calls[0].args[3], `${TITLE_PREFIX}${id}`);
});

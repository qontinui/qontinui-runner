#!/usr/bin/env node
/**
 * ci-flake-escalate.mjs — Phase 2 of plan
 * `2026-08-30-runner-ci-has-no-flake-detection-so-one-flaky-test-freezes-the-train`:
 * turn coord's per-test flake history into a human-facing escalation.
 *
 * THE COUNTER IS COORD, NOT THIS SCRIPT. Phase 1 (`ci-test-results-ingest.mjs`)
 * fills `coord.test_results` with every per-test outcome the gating
 * `cargo test` step prints; coord's `flakiness_priors` scores that history
 * into a per-test `flake_rate ∈ [0,1]` over the last `min_k` (default 20)
 * runs, readable at `POST /coord/test-flakiness`. This script READS that
 * verdict and files the GitHub issue the plan demotes to "a human-facing
 * escalation artifact, not the counter". Nothing here recomputes a rate.
 *
 * ESCALATION RULE (plan, Phase 2), two arms:
 *
 *   rate arm — a test escalates at ≥ 3 occurrences inside coord's window.
 *     When coord serves the per-outcome tally (`outcomes`, qontinui-coord
 *     since the 2026-09-12 read rewrite) an occurrence is a `fail` or
 *     `error`, and the test must ALSO have passed at least once in the
 *     window: a test that only fails is broken, not flaky, and a
 *     `#[cfg_attr(windows, ignore)]` test that passes on one shard and is
 *     skipped on the other reads as a 50 % "flake" by rate but has zero
 *     failures by tally. Against an older coord that serves no tally the rule
 *     degrades to `occurrences = round(flake_rate × sample_size)` — runs whose
 *     outcome disagreed with the modal one — which is the rule this script
 *     shipped with, and carries exactly the two false positives above.
 *
 *   main-red arm — "any occurrence on a push to `refs/heads/main`". Given a
 *     CI run (`--run-id`, `--run-conclusion failure`), the failing tests are
 *     parsed from the failed `test (<platform>)` jobs' logs with the SAME
 *     parser Phase 1 ingests with, so the issue title matches the coord
 *     identity (`<binary>::<path>`) and both arms land on one issue. Capped
 *     at `MAIN_RED_CAP` tests per run: a commit that reds 200 tests is a
 *     broken commit, not 200 flakes, and coord's red-main machinery owns it.
 *     The rate arm can be switched off for that trigger (`--no-rate`) so a
 *     per-push run does not repeat the nightly read. Only the GATING step's
 *     output is parsed — the log is sliced to the `cargo test --verbose`
 *     step, because the same job also runs the non-gating `Live AI extractor
 *     smoke` (`cargo test … --ignored --exact …`) whose failure never reds the
 *     job and must not be filed as a flake.
 *
 * TWO ARMS, TWO KINDS OF WRITE. The nightly's body is coord's snapshot and is
 * REPLACED on every run. A push-to-main occurrence is an EVENT: on an issue
 * that already exists it is appended as a comment (and the issue reopened if
 * closed), never written into the body — otherwise a 09:00 push would erase
 * the nightly's tally and SHA table and the next nightly would erase the
 * push. Only a test with no issue yet gets its body written by the main-red
 * arm, and that body says so.
 *
 * THE BODY CARRIES THE EVIDENCE, NOT JUST THE NUMBER. With the tally, coord
 * also serves the most recent failures' SHAs and shards. Three failures on
 * three commits of one PR is a broken PR that was iterated, not a flake; the
 * rate cannot tell those apart and a reader with the SHA list can.
 *
 * ONE TEST OWNS ONE ISSUE FOREVER. The issue title is `flaky test: <test id>`
 * — stable, so the upsert is idempotent: an existing open issue gets its
 * body refreshed, a closed one is reopened (the test flaked again after
 * someone closed it), and only a test with no issue at all gets a new one.
 * The candidate set is every `flake`-labelled issue PLUS every issue whose
 * title carries the prefix, so a hand-filed or de-labelled issue is adopted
 * (and relabelled) rather than duplicated. The label is created on first use.
 *
 * THE FIRST ESCALATION WAS DONE BY HAND. Before this reader existed, a session
 * read the endpoint, found `wedge_diagnostics::tests::a_spinning_child_reports_meaningful_cpu`
 * at flake_rate 0.400 (8 of 20), root-caused it as a test defect and fixed it
 * in qontinui-runner#1444 — the plan's rule applied manually. This script is
 * that read, made recurring.
 *
 * A FAILED READ IS UNKNOWN, NEVER "NO FLAKY TESTS". coord's response carries
 * `history_read` (qontinui-coord#2075): `ok` means `priors` is a real
 * observation; `thin` means no test has reached `min_k` samples yet; `failed`
 * means the read itself failed and `priors` says nothing. Measured against
 * this repo on 2026-09-12 the read reported `failed` after ~60 s on three
 * consecutive probes, so the honest thing for this script to do on that arm
 * is exit non-zero with a loud `::error` and file NOTHING — a red scheduled
 * run says "the rail is dark", where a green one with zero issues would say
 * "no flakes", which is the false clean bill of health the plan exists to
 * abolish. An older coord that omits `history_read` entirely is treated the
 * same way whenever its `priors` is empty, since that shape cannot be told
 * apart from a failure.
 *
 * USAGE
 *   node scripts/ci-flake-escalate.mjs --repo <owner/repo>
 *        [--min-occurrences 3] [--window N] [--run-url <url>] [--dry-run]
 *        [--run-id <id> --run-conclusion <success|failure|…>] [--no-rate]
 *
 * ENV
 *   COORD_HTTP_URL   coord base URL. Default https://coord.qontinui.io
 *                    (mirrors ci-test-results-ingest.mjs).
 *   GH_TOKEN         what `gh` authenticates with (needs `issues: write`).
 *
 * EXIT CODES
 *   0  read was `ok` (issues upserted, or `--dry-run` printed the plan) or
 *      `thin` (nothing scorable yet — a notice, not an error), or the rate
 *      arm was switched off and the main-red arm ran to completion. A
 *      failed test job whose log carried no cargo output (compile error,
 *      starved runner) is warned about, not a failure — that is a broken
 *      build, which has its own owner.
 *   1  … or a failed test job's LOG COULD NOT BE READ (a 403 from a dropped
 *      `actions: read`, a 5xx): that is the main-red arm going dark, and it
 *      is loud for the same reason the rate arm is.
 *   1  the read could not be taken as an observation (network failure,
 *      non-2xx, non-JSON, `history_read: failed`, or an unrecognised body),
 *      or any `gh` call — label, list, create, edit, reopen — failed. Loud on
 *      purpose; nothing is filed on that path.
 *   2  CLI usage error.
 */

import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { parseArgs as nodeParseArgs } from "node:util";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { isRustTestJob, parseTestOutcomes, platformOfJobName } from "./ci-flake-analyze.mjs";

const DEFAULT_COORD_URL = "https://coord.qontinui.io";
const FLAKINESS_PATH = "/coord/test-flakiness";

/// The plan's threshold: "at 3 occurrences of one test … it stops being a
/// counter and becomes a plan".
export const DEFAULT_MIN_OCCURRENCES = 3;

/// The main-red arm files nothing when a single run reds MORE than this many
/// tests: that is a broken commit (a bad merge, a poisoned build), not a
/// crowd of flakes, and it already has an owner — coord's red-main baseline.
export const MAIN_RED_CAP = 10;

/// Every escalation issue carries this label; it is also the search key the
/// upsert uses to find existing issues without scanning the whole tracker.
export const FLAKE_LABEL = "flake";
const FLAKE_LABEL_COLOR = "D93F0B";
const FLAKE_LABEL_DESCRIPTION =
  "A test coord's flake history escalated (plan 2026-08-30-runner-ci-has-no-flake-detection)";

/// GitHub caps issue titles at 256 characters. Titles are the upsert key, so
/// a long test id is truncated DETERMINISTICALLY (with a digest suffix that
/// keeps two long ids from colliding) rather than rejected.
export const TITLE_PREFIX = "flaky test: ";
export const MAX_TITLE_LEN = 250;

/// Bound the coord read. The read itself has been measured to fail
/// server-side at ~60 s; a client bound above that lets coord report its own
/// verdict instead of this script guessing at one.
const REQUEST_TIMEOUT_MS = 120_000;

// ---------------------------------------------------------------------------
// Pure half — unit-tested in scripts/__tests__/ci-flake-escalate.test.mjs
// ---------------------------------------------------------------------------

/**
 * Classify a `POST /coord/test-flakiness` response body.
 *
 * Returns `{ kind, priors, minK, window, reason }` where `kind` is one of
 * `ok` | `thin` | `failed`. `failed` covers coord's own `failed` verdict AND
 * any response this script cannot read as an observation (non-object,
 * missing `priors`, an unknown `history_read` token, or a pre-#2075 coord that
 * omits `history_read` while serving empty `priors`) — every one of those is
 * UNKNOWN, and UNKNOWN must not render as "no flaky tests".
 */
export function classifyFlakinessResponse(body) {
  if (!body || typeof body !== "object" || Array.isArray(body)) {
    return { kind: "failed", priors: {}, reason: "response is not a JSON object" };
  }
  const priors =
    body.priors && typeof body.priors === "object" && !Array.isArray(body.priors)
      ? body.priors
      : null;
  if (!priors) {
    return { kind: "failed", priors: {}, reason: "response carries no `priors` object" };
  }
  const minK = typeof body.min_k === "number" ? body.min_k : undefined;
  const window = typeof body.window === "number" ? body.window : undefined;
  const base = { priors, minK, window };

  const read = body.history_read;
  if (read === undefined) {
    // A coord predating qontinui-coord#2075 serves no discriminator. A
    // non-empty map is still an observation; an empty one is ambiguous
    // between "thin" and "failed", and ambiguity resolves to UNKNOWN.
    if (Object.keys(priors).length > 0) {
      return { ...base, kind: "ok", reason: "no `history_read` field; priors non-empty" };
    }
    return {
      ...base,
      kind: "failed",
      reason:
        "response omits `history_read` and `priors` is empty — this coord predates " +
        "qontinui-coord#2075, and an empty map there cannot be told apart from a failed read",
    };
  }
  switch (read) {
    case "ok":
      return { ...base, kind: "ok", reason: "history_read=ok" };
    case "thin":
      return { ...base, kind: "thin", reason: "history_read=thin" };
    case "failed":
      return {
        ...base,
        kind: "failed",
        reason: "history_read=failed (coord could not load the history)",
      };
    default:
      return {
        ...base,
        kind: "failed",
        reason: `unrecognised history_read token ${JSON.stringify(read)}`,
      };
  }
}

/**
 * The tests that clear the escalation threshold, most-flaky first.
 *
 * With a per-outcome tally (`prior.outcomes`, an object keyed by coord's
 * outcome token), `occurrences` is `fail + error` and the test must have
 * passed at least once in the window — see the header for the two false
 * positives that rules out. Without a tally, `occurrences` is the number of
 * runs whose outcome disagreed with the modal outcome — `flake_rate ×
 * sample_size`, rounded, because coord's rate is exactly that ratio. Each
 * escalation records which arm scored it (`rule`). A prior with a malformed
 * shape is skipped rather than escalated: this function files nothing on a
 * guess.
 */
export function selectEscalations(priors, { minOccurrences = DEFAULT_MIN_OCCURRENCES } = {}) {
  const out = [];
  for (const [testId, prior] of Object.entries(priors ?? {})) {
    if (!prior || typeof prior !== "object") continue;
    const flakeRate = Number(prior.flake_rate);
    const sampleSize = Number(prior.sample_size);
    if (!Number.isFinite(flakeRate) || !Number.isFinite(sampleSize)) continue;
    if (flakeRate <= 0 || sampleSize <= 0) continue;
    const tally =
      prior.outcomes && typeof prior.outcomes === "object" && !Array.isArray(prior.outcomes)
        ? prior.outcomes
        : null;
    let occurrences;
    let rule;
    if (tally) {
      occurrences = Number(tally.fail ?? 0) + Number(tally.error ?? 0);
      if (!Number.isFinite(occurrences)) continue;
      if (Number(tally.pass ?? 0) < 1) continue;
      rule = "tally";
    } else {
      occurrences = Math.round(flakeRate * sampleSize);
      rule = "rate";
    }
    if (occurrences < minOccurrences) continue;
    out.push({
      testId,
      flakeRate,
      sampleSize,
      occurrences,
      rule,
      modalOutcome: typeof prior.modal_outcome === "string" ? prior.modal_outcome : "unknown",
      outcomes: tally,
      recentFailures: Array.isArray(prior.recent_failures) ? prior.recent_failures : [],
    });
  }
  out.sort(
    (a, b) =>
      b.flakeRate - a.flakeRate ||
      b.occurrences - a.occurrences ||
      a.testId.localeCompare(b.testId),
  );
  return out;
}

/**
 * The main-red arm over one CI run's jobs.
 *
 * @param {Array<{name: string, conclusion: string|null, logText: string|null}>} jobs
 *   every job of the run; only failed `test (…)` jobs with a log are read
 * @returns {{escalations: Array<{testId: string, mainRed: {shards: string[]}}>, unparsed: string[], capped: boolean, failingCount: number}}
 *   `unparsed` names failed test jobs whose log carried no recognisable cargo
 *   output — a compile error, a starved runner — which are NOT flakes and are
 *   reported, never folded into an empty list.
 */
export function selectMainRedEscalations(jobs, { cap = MAIN_RED_CAP } = {}) {
  const shardsByTest = new Map();
  const unparsed = [];
  const unreadable = [];
  for (const job of jobs ?? []) {
    if (!isRustTestJob(job?.name) || job.conclusion !== "failure") continue;
    if (job.unreadable) {
      unreadable.push(job.name);
      continue;
    }
    const parsed = parseTestOutcomes(sliceGatingStep(job.logText ?? ""));
    if (parsed.unparsed) {
      unparsed.push(job.name);
      continue;
    }
    const shard = platformOfJobName(job.name);
    for (const t of parsed.tests) {
      if (t.outcome !== "fail") continue;
      if (!shardsByTest.has(t.testId)) shardsByTest.set(t.testId, new Set());
      shardsByTest.get(t.testId).add(shard);
    }
  }
  const failingCount = shardsByTest.size;
  const capped = failingCount > cap;
  const escalations = capped
    ? []
    : [...shardsByTest.entries()]
        .map(([testId, shards]) => ({ testId, mainRed: { shards: [...shards].sort() } }))
        .sort((a, b) => a.testId.localeCompare(b.testId));
  return { escalations, unparsed, unreadable, capped, failingCount };
}

/// The echoed script of the gating `Run Rust tests` step in ci.yml carries
/// this invocation; the non-gating smoke step's is `cargo test --bin … --ignored`.
const GATING_STEP_MARKER = "cargo test --verbose";
const STEP_START_RE = /^(?:\S+ )?##\[group\]Run /;
const STEP_ECHO_END_RE = /^(?:\S+ )?##\[endgroup\]/;

/**
 * Cut a job log down to the gating `cargo test --verbose` step's output.
 *
 * An Actions job log is every step in sequence; each step opens with a
 * `##[group]Run <script>` … `##[endgroup]` block echoing its script, then
 * its output runs until the next step's `##[group]Run`. The `test (…)` job
 * runs cargo test TWICE — the gate, and the `Live AI extractor smoke`
 * (`cargo test --bin … --ignored --exact …`, `continue-on-error`) — so a
 * parse of the whole log would attribute the smoke's `… FAILED` line to a
 * job the smoke never reds. Slice from the step whose echoed script names the
 * gating invocation to the next step.
 *
 * Fail CLOSED: a log with no such step returns "" (→ `unparsed`), never the
 * whole log — an unsliced parse is precisely the misattribution above. A
 * log with no step markers at all (a synthetic excerpt, a hand-saved
 * `cargo-test-output.log`) is returned whole: there is nothing to slice.
 *
 * @param {string} logText
 * @returns {string}
 */
export function sliceGatingStep(logText) {
  const lines = String(logText ?? "").split("\n");
  const starts = [];
  for (let i = 0; i < lines.length; i += 1) {
    if (STEP_START_RE.test(lines[i])) starts.push(i);
  }
  if (starts.length === 0) return lines.join("\n");
  for (const [k, start] of starts.entries()) {
    const end = k + 1 < starts.length ? starts[k + 1] : lines.length;
    let echoEnd = start;
    while (echoEnd < end && !STEP_ECHO_END_RE.test(lines[echoEnd])) echoEnd += 1;
    const echoed = lines.slice(start, echoEnd + 1).join("\n");
    if (echoed.includes(GATING_STEP_MARKER)) return lines.slice(start, end).join("\n");
  }
  return "";
}

/**
 * Join the two arms on test id, so a test both arms name gets ONE issue
 * carrying both pieces of evidence. Rate-arm order is kept; main-red-only
 * tests follow, by id.
 */
export function mergeEscalations(rate, mainRed) {
  const byId = new Map();
  for (const r of rate ?? []) byId.set(r.testId, { ...r });
  for (const m of mainRed ?? []) {
    const existing = byId.get(m.testId);
    if (existing) existing.mainRed = m.mainRed;
    else byId.set(m.testId, { testId: m.testId, mainRed: m.mainRed });
  }
  return [...byId.values()];
}

/** The stable, idempotent issue title for a test id. */
export function issueTitle(testId) {
  const full = TITLE_PREFIX + testId;
  if (full.length <= MAX_TITLE_LEN) return full;
  const digest = createHash("sha256").update(testId).digest("hex").slice(0, 8);
  const room = MAX_TITLE_LEN - TITLE_PREFIX.length - digest.length - 2; // "…" + " "
  return `${TITLE_PREFIX}${testId.slice(0, room)}… ${digest}`;
}

/** Markdown body for an escalation issue — every number read back from coord. */
export function renderIssueBody(escalation, { repo, minK, window, runUrl, ciRunUrl, readAt } = {}) {
  const hasRate = Number.isFinite(escalation.flakeRate);
  const pct = hasRate ? (escalation.flakeRate * 100).toFixed(1) : null;
  const tally = escalation.outcomes
    ? Object.entries(escalation.outcomes)
        .sort(([a], [b]) => a.localeCompare(b))
        .map(([k, v]) => `${k} ${v}`)
        .join(" · ")
    : null;
  const lines = [
    hasRate
      ? `coord's per-test flake history escalated this test (plan`
      : `a push to \`main\` failed this test (plan`,
    `\`2026-08-30-runner-ci-has-no-flake-detection-so-one-flaky-test-freezes-the-train\`, Phase 2).`,
    ``,
    `| | |`,
    `|---|---|`,
    `| test | \`${escalation.testId}\` |`,
  ];
  if (hasRate) {
    lines.push(
      `| flake_rate | **${escalation.flakeRate.toFixed(3)}** (${pct}%) |`,
      escalation.rule === "tally"
        ? `| occurrences | **${escalation.occurrences}** failure(s) in ${escalation.sampleSize} observations, with at least one pass (tally rule) |`
        : `| occurrences | **${escalation.occurrences}** of ${escalation.sampleSize} runs disagreed with the modal outcome (rate rule — coord served no tally) |`,
      `| modal outcome | \`${escalation.modalOutcome}\` |`,
    );
    if (tally) lines.push(`| outcome tally | ${tally} |`);
    lines.push(`| window | last ${window ?? "?"} observations per test (min_k ${minK ?? "?"}) |`);
  }
  if (escalation.mainRed) {
    lines.push(
      `| failed on a push to \`main\` | ${renderMainRedCell(escalation.mainRed, ciRunUrl)} |`,
    );
  }
  lines.push(
    `| repo | \`${repo ?? "?"}\` |`,
    `| read at | ${readAt ?? "?"} |`,
    `| escalated by | ${runUrl ? `[this run](${runUrl})` : "a manual run"} |`,
    ``,
  );
  const recent = escalation.recentFailures ?? [];
  if (recent.length > 0) {
    lines.push(
      `**Recent failures, newest first** (read back from \`coord.test_results\`):`,
      ``,
      `| observed_at | head_sha | shard | outcome |`,
      `|---|---|---|---|`,
    );
    for (const f of recent) {
      const sha = f.head_sha
        ? `[\`${String(f.head_sha).slice(0, 9)}\`](https://github.com/${repo ?? ""}/commit/${f.head_sha})`
        : "—";
      lines.push(`| ${f.observed_at ?? "—"} | ${sha} | ${f.shard ?? "—"} | ${f.outcome ?? "—"} |`);
    }
    lines.push(
      ``,
      `Read the SHA column before believing the word "flaky": three failures on three`,
      `commits of one PR is a broken PR that was iterated, not a flake. \`coord.test_results\``,
      `carries no ref, so the rate cannot tell those apart; the list can.`,
      ``,
    );
  }
  lines.push(
    ...(hasRate ? coordParagraphs(repo, window) : []),
    `**Root-cause it before touching the threshold.** Ask *test or production*`,
    `*defect?* first, and expect the answer to sometimes be production — that is`,
    `what it was for \`#1178\`. The first escalation this rail produced,`,
    `\`wedge_diagnostics::tests::a_spinning_child_reports_meaningful_cpu\` at`,
    `0.400, was a test defect (qontinui-runner#1444).`,
    ``,
    `This issue's body is coord's snapshot, refreshed on every scheduled run`,
    `while the test stays above threshold; each failing push to \`main\` that`,
    `names this test is appended as a comment. It is reopened if closed while`,
    `either keeps happening. Close it once the fix has landed; a later`,
    `re-escalation reopens it rather than filing a duplicate.`,
  );
  return lines.join("\n");
}

function renderMainRedCell(mainRed, ciRunUrl) {
  const shards = mainRed.shards.map((x) => `\`${x}\``).join(", ");
  return `${shards}${ciRunUrl ? ` — [the run](${ciRunUrl})` : ""}`;
}

/** The coord-sourced caveats — only meaningful when a coord read happened. */
function coordParagraphs(repo, window) {
  return [
    `**Read the number before reading the word "flaky".** Every CI run ingests`,
    `one row per platform leg under the SAME test id (coord's \`test_id\` carries`,
    `no platform component), so a window of ${window ?? "N"} observations is ~${window ? Math.round(window / 2) : "N/2"} CI runs × 2`,
    `legs. A rate near **0.500** with modal \`pass\` is therefore usually a test`,
    `that fails DETERMINISTICALLY on one platform and passes on the other — the`,
    `plan's own Phase 0 found 10 of 12 same-SHA disagreements were windows-only —`,
    `not nondeterminism. Check the platform split (the \`shard\` column of the`,
    `recent-failures table above when coord served one) before treating it as a`,
    `flake.`,
    ``,
    `Source: \`POST /coord/test-flakiness\` \`{"repo":"${repo ?? ""}"}\` — the rate is`,
    `coord's \`flakiness_priors\` over \`coord.test_results\`, filled by the`,
    `\`Report test results to coord\` step of \`ci.yml\` (see the plan's "Known`,
    `limitation" for why the platform is not in the key).`,
    ``,
  ];
}

/**
 * The comment a push-to-main occurrence appends to an EXISTING issue — an
 * event, not a snapshot, so it never overwrites the nightly's evidence.
 */
export function renderMainRedComment(escalation, { ciRunUrl, readAt } = {}) {
  return [
    `Failed again on a push to \`main\` — ${renderMainRedCell(escalation.mainRed, ciRunUrl)}` +
      ` (${readAt ?? "?"}).`,
    ``,
    `Appended by \`scripts/ci-flake-escalate.mjs\` (main-red arm); the body above is coord's`,
    `nightly snapshot and is not touched by this arm.`,
  ].join("\n");
}

/**
 * Decide what to do for each escalation against the issues that already
 * exist. `existingIssues` is `[{ number, title, state }]` (any state).
 *
 * Match is on the EXACT title — that is the whole idempotency contract. An
 * open match is refreshed, a closed match is reopened and refreshed, and no
 * match creates. When two issues somehow share a title (a hand-filed
 * duplicate), the owner is the lowest-numbered OPEN one — never a closed one
 * while an open one exists, or the run would reopen a second issue for the
 * same test while claiming to dedupe — and only when every match is closed
 * is the lowest-numbered closed one reopened. The rest are named in
 * `duplicates` so the run can warn rather than pick silently.
 */
export function planIssueActions(escalations, existingIssues) {
  const byTitle = new Map();
  for (const issue of existingIssues ?? []) {
    if (!issue || typeof issue.title !== "string") continue;
    const list = byTitle.get(issue.title) ?? [];
    list.push(issue);
    byTitle.set(issue.title, list);
  }
  const isClosed = (issue) => String(issue.state ?? "").toUpperCase() === "CLOSED";
  const actions = [];
  for (const escalation of escalations) {
    const title = issueTitle(escalation.testId);
    const matches = (byTitle.get(title) ?? [])
      .slice()
      .sort(
        (a, b) => Number(isClosed(a)) - Number(isClosed(b)) || Number(a.number) - Number(b.number),
      );
    if (matches.length === 0) {
      actions.push({ action: "create", title, escalation, duplicates: [] });
      continue;
    }
    const [owner, ...duplicates] = matches;
    actions.push({
      action: isClosed(owner) ? "reopen" : "update",
      title,
      number: owner.number,
      escalation,
      duplicates: duplicates.map((d) => d.number),
    });
  }
  return actions;
}

// ---------------------------------------------------------------------------
// IO half
// ---------------------------------------------------------------------------

function info(msg) {
  process.stdout.write(`[ci-flake-escalate] ${msg}\n`);
}
function notice(msg) {
  process.stdout.write(`::notice title=flake-escalate::${msg}\n`);
}
function warn(msg) {
  process.stdout.write(`::warning title=flake-escalate::${msg}\n`);
}
function error(msg) {
  process.stdout.write(`::error title=flake-escalate::${msg}\n`);
}

async function readFlakiness(base, repo, window) {
  const url = base + FLAKINESS_PATH;
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), REQUEST_TIMEOUT_MS);
  const started = Date.now();
  try {
    const res = await fetch(url, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(window ? { repo, window } : { repo }),
      signal: controller.signal,
    });
    const text = await res.text().catch(() => "");
    const ms = Date.now() - started;
    if (!res.ok) {
      return {
        error: `non-2xx from ${url} after ${ms}ms: HTTP ${res.status} ${text.slice(0, 300)}`,
      };
    }
    try {
      return { body: JSON.parse(text), ms };
    } catch {
      return { error: `non-JSON body from ${url} after ${ms}ms: ${text.slice(0, 300)}` };
    }
  } catch (err) {
    const ms = Date.now() - started;
    const aborted = err?.name === "AbortError" || /abort/i.test(err?.message ?? "");
    return {
      error: aborted
        ? `request to ${url} ABORTED by the client after ${ms}ms (REQUEST_TIMEOUT_MS=${REQUEST_TIMEOUT_MS})`
        : `request to ${url} failed after ${ms}ms: ${err.message}`,
    };
  } finally {
    clearTimeout(timer);
  }
}

/**
 * Run `gh` with the repo pinned (`GH_REPO`, which `gh` honours as an env
 * override); throws with stderr on failure. `execFileSync` with an argv, never
 * a shell — test ids flow into these arguments and must not be interpreted.
 */
function execGh(args, { repo, input } = {}) {
  return execFileSync("gh", args, {
    encoding: "utf8",
    input,
    env: { ...process.env, GH_REPO: repo },
    stdio: ["pipe", "pipe", "pipe"],
    // A failed `test (…)` job log is tens of MB (every `cargo test --verbose`
    // line); 16 MiB truncated one and the parse read it as "unparsed".
    maxBuffer: 256 * 1024 * 1024,
  });
}

/**
 * The triggering run's jobs, with the log of every FAILED Rust test job
 * attached (`actions: read`). A job whose log cannot be read is returned with
 * `logText: null` and a warning; `selectMainRedEscalations` then reports it as
 * unparsed rather than as "no failing tests".
 */
export function fetchRunJobs(repo, runId, { attempt, gh = execGh } = {}) {
  // Pin the attempt: `/runs/{id}/jobs` defaults to the LATEST attempt, so a
  // re-run started before this read lists in-progress jobs with a null
  // conclusion and the failure that triggered us reads as "nothing failed".
  const path =
    attempt !== undefined
      ? `repos/${repo}/actions/runs/${runId}/attempts/${attempt}/jobs?per_page=100`
      : `repos/${repo}/actions/runs/${runId}/jobs?per_page=100`;
  const listed = JSON.parse(gh(["api", path], { repo }) || "{}");
  return (listed.jobs ?? []).map((j) => {
    if (!isRustTestJob(j.name) || j.conclusion !== "failure") {
      return { name: j.name, conclusion: j.conclusion, logText: null };
    }
    try {
      const logText = gh(["api", `repos/${repo}/actions/jobs/${j.id}/logs`], { repo });
      return { name: j.name, conclusion: j.conclusion, logText };
    } catch (err) {
      error(
        `could not read the log of failed job "${j.name}" (#${j.id}): ${String(err.stderr || err.message).trim()}`,
      );
      return { name: j.name, conclusion: j.conclusion, logText: null, unreadable: true };
    }
  });
}

/**
 * Create-or-update the `flake` label. `--force` is what makes this idempotent
 * under a case-different pre-existing `Flake` (label names are unique
 * case-insensitively, so a list-then-create would 422) and under two runs
 * racing the same first use.
 */
export function ensureLabel(repo, gh = execGh) {
  gh(
    [
      "label",
      "create",
      FLAKE_LABEL,
      "--force",
      "--color",
      FLAKE_LABEL_COLOR,
      "--description",
      FLAKE_LABEL_DESCRIPTION,
    ],
    { repo },
  );
}

/**
 * The issues the upsert matches against. Two lists, unioned by number: every
 * `flake`-labelled issue, plus every issue whose title carries the prefix —
 * so a hand-filed `flaky test: …` issue nobody labelled, or one whose label
 * was removed, is still found and adopted (and relabelled by the edit)
 * rather than duplicated. The exact-title match is `planIssueActions`'s.
 */
export function listFlakeIssues(repo, gh = execGh) {
  const common = ["--state", "all", "--limit", "1000", "--json", "number,title,state"];
  const labelled = JSON.parse(
    gh(["issue", "list", "--label", FLAKE_LABEL, ...common], { repo }) || "[]",
  );
  const titled = JSON.parse(
    gh(["issue", "list", "--search", `"${TITLE_PREFIX.trim()}" in:title`, ...common], { repo }) ||
      "[]",
  );
  const byNumber = new Map();
  for (const issue of [...labelled, ...titled]) {
    if (issue && issue.number !== undefined) byNumber.set(issue.number, issue);
  }
  return [...byNumber.values()];
}

/**
 * Apply one planned action; returns the created URL or `#<number>`.
 *
 * `body` is written on `create`, and on `update`/`reopen` only when the
 * escalation carries a coord reading (`rule` set) — the body is coord's
 * snapshot. `comment`, when given, is appended on `update`/`reopen` — the
 * main-red arm's event. A main-red-only escalation on an existing issue
 * therefore reopens (if closed) and comments, and leaves the body alone.
 */
export function applyAction(action, body, repo, gh = execGh, { comment } = {}) {
  switch (action.action) {
    case "create": {
      const url = gh(
        ["issue", "create", "--title", action.title, "--label", FLAKE_LABEL, "--body-file", "-"],
        { repo, input: body },
      ).trim();
      return url;
    }
    case "reopen":
      gh(["issue", "reopen", String(action.number)], { repo });
    // fall through — a reopened issue gets the fresh body / the comment too
    case "update": {
      const writesBody = action.escalation?.rule !== undefined || !comment;
      if (writesBody) {
        gh(
          ["issue", "edit", String(action.number), "--add-label", FLAKE_LABEL, "--body-file", "-"],
          { repo, input: body },
        );
      }
      if (comment) {
        gh(["issue", "comment", String(action.number), "--body-file", "-"], {
          repo,
          input: comment,
        });
      }
      return `#${action.number}`;
    }
    default:
      throw new Error(`unknown action ${action.action}`);
  }
}

function printUsage(stream) {
  stream.write(
    [
      "usage: node scripts/ci-flake-escalate.mjs --repo <owner/repo>",
      "         [--min-occurrences N] [--window N] [--run-url <url>] [--dry-run]",
      "         [--run-id <id> --run-conclusion <c> [--run-attempt N]] [--no-rate]",
      "",
      "Reads POST /coord/test-flakiness for <repo> and upserts one",
      "`flaky test: <test id>` GitHub issue (label `flake`) per test at or above",
      `N failures (with at least one pass) in coord's window (default ${DEFAULT_MIN_OCCURRENCES}).`,
      "With --run-id/--run-conclusion, ALSO escalates every test that failed in",
      "that run's `test (…)` jobs when the run concluded `failure` (the plan's",
      "push-to-main arm); --no-rate skips the coord read for such a per-push run.",
      "",
    ].join("\n"),
  );
}

async function main(argv) {
  let parsed;
  try {
    parsed = nodeParseArgs({
      args: argv,
      options: {
        repo: { type: "string" },
        "min-occurrences": { type: "string" },
        window: { type: "string" },
        "run-url": { type: "string" },
        "run-id": { type: "string" },
        "run-conclusion": { type: "string" },
        "run-attempt": { type: "string" },
        "no-rate": { type: "boolean" },
        "dry-run": { type: "boolean" },
        help: { type: "boolean", short: "h" },
      },
      allowPositionals: false,
    });
  } catch (err) {
    process.stderr.write(`ci-flake-escalate: ${err.message}\n`);
    printUsage(process.stderr);
    return 2;
  }
  if (parsed.values.help) {
    printUsage(process.stdout);
    return 0;
  }
  const { repo } = parsed.values;
  if (!repo) {
    process.stderr.write("ci-flake-escalate: --repo is required\n");
    printUsage(process.stderr);
    return 2;
  }
  const rawMin = parsed.values["min-occurrences"];
  const minOccurrences = rawMin === undefined ? DEFAULT_MIN_OCCURRENCES : Number(rawMin);
  if (!/^\d+$/.test(rawMin ?? "1") || !Number.isInteger(minOccurrences) || minOccurrences < 1) {
    process.stderr.write("ci-flake-escalate: --min-occurrences must be a positive integer\n");
    return 2;
  }
  const rawWindow = parsed.values.window;
  const window = rawWindow === undefined ? undefined : Number(rawWindow);
  if (rawWindow !== undefined && (!/^\d+$/.test(rawWindow) || window < 1)) {
    process.stderr.write("ci-flake-escalate: --window must be a positive integer\n");
    return 2;
  }
  const dryRun = Boolean(parsed.values["dry-run"]);
  const runUrl = parsed.values["run-url"];
  const runId = parsed.values["run-id"];
  const runConclusion = parsed.values["run-conclusion"];
  const runAttempt = parsed.values["run-attempt"];
  if (runAttempt !== undefined && !/^\d+$/.test(runAttempt)) {
    process.stderr.write("ci-flake-escalate: --run-attempt must be a positive integer\n");
    return 2;
  }
  // The CI run the evidence points at — distinct from `--run-url`, which is
  // the escalator's own run (where the cap / unreadable warnings live).
  const ciRunUrl =
    runId !== undefined && runUrl
      ? `${runUrl.replace(/\/actions\/runs\/.*$/, "")}/actions/runs/${runId}`
      : undefined;
  const rateArm = !parsed.values["no-rate"];
  if ((runId === undefined) !== (runConclusion === undefined)) {
    process.stderr.write("ci-flake-escalate: --run-id and --run-conclusion go together\n");
    return 2;
  }
  if (!rateArm && runId === undefined) {
    process.stderr.write(
      "ci-flake-escalate: --no-rate needs --run-id/--run-conclusion, else nothing runs\n",
    );
    return 2;
  }

  const readAt = new Date().toISOString();
  let verdict = { kind: "skipped", priors: {}, minK: undefined, window: undefined };
  let rateEscalations = [];
  if (rateArm) {
    const base = (process.env.COORD_HTTP_URL || DEFAULT_COORD_URL).replace(/\/+$/, "");
    const read = await readFlakiness(base, repo, window);
    if (read.error) {
      error(
        `flakiness read UNKNOWN — ${read.error}. Filing nothing: an unreadable rail is not a clean one.`,
      );
      return 1;
    }
    verdict = classifyFlakinessResponse(read.body);
    info(
      `POST ${base}${FLAKINESS_PATH} (repo=${repo}) -> ${verdict.kind} in ${read.ms}ms ` +
        `(${verdict.reason}; ${Object.keys(verdict.priors).length} scored test(s), ` +
        `min_k=${verdict.minK ?? "?"}, window=${verdict.window ?? "?"})`,
    );
    if (verdict.kind === "failed") {
      error(
        `coord could not serve a flake history for ${repo} after ${read.ms}ms — ${verdict.reason}. ` +
          `Filing nothing: this is UNKNOWN, not "no flaky tests". A read that fails server-side ` +
          `is a coord-side condition (see the header of scripts/ci-flake-escalate.mjs and coord ` +
          `finding e5311ec9-1592-430e-a7f8-9cc6eb84032e, topic test-flakiness), not this repo's.`,
      );
      return 1;
    }
    if (verdict.kind === "thin") {
      notice(
        `no test in ${repo} has reached coord's min_k=${verdict.minK ?? "?"} samples yet — ` +
          `nothing to score. Not a clean bill of health; the corpus is still filling.`,
      );
    } else {
      rateEscalations = selectEscalations(verdict.priors, { minOccurrences });
      const flakyCount = Object.values(verdict.priors).filter(
        (p) => Number(p?.flake_rate) > 0,
      ).length;
      info(
        `${flakyCount} test(s) with a non-zero flake_rate; ${rateEscalations.length} at or above ` +
          `${minOccurrences} occurrence(s) in the window`,
      );
      for (const e of rateEscalations) {
        info(
          `  ${e.flakeRate.toFixed(3)}  ${e.occurrences}/${e.sampleSize}  modal=${e.modalOutcome}  rule=${e.rule}  ${e.testId}`,
        );
      }
    }
  }

  // The main-red arm: every test that failed in a `failure` run's test jobs.
  let mainRed = { escalations: [], unparsed: [], unreadable: [], capped: false, failingCount: 0 };
  if (runId !== undefined) {
    if (runConclusion === "failure") {
      let jobs;
      try {
        jobs = fetchRunJobs(repo, runId, { attempt: runAttempt });
      } catch (err) {
        error(
          `could not list the jobs of run ${runId}: ${String(err.stderr || err.message).trim()}`,
        );
        return 1;
      }
      mainRed = selectMainRedEscalations(jobs);
      info(
        `main-red arm: run ${runId} concluded failure; ${mainRed.failingCount} failing test(s) in its ` +
          `test jobs, ${mainRed.escalations.length} escalated` +
          (mainRed.capped
            ? ` (CAPPED at ${MAIN_RED_CAP}: a mass failure is a broken commit, not ${mainRed.failingCount} flakes)`
            : ""),
      );
      for (const name of mainRed.unparsed) {
        warn(
          `main-red arm: failed job "${name}" carried no recognisable cargo test output in its ` +
            `gating step (compile error / starved runner?) — not a flake, not escalated`,
        );
      }
      if (mainRed.unreadable.length > 0) {
        error(
          `main-red arm is DARK for run ${runId}: the log of ${mainRed.unreadable.length} failed test ` +
            `job(s) could not be read (${mainRed.unreadable.join(", ")}) — a dropped \`actions: read\`? ` +
            `Filing nothing for them: UNKNOWN is not "no failing tests".`,
        );
        return 1;
      }
      if (mainRed.capped) {
        notice(
          `main-red arm: ${mainRed.failingCount} tests failed in run ${runId} — above the cap of ` +
            `${MAIN_RED_CAP}, so this is treated as a broken commit and nothing is filed.`,
        );
      }
    } else {
      info(`main-red arm: run ${runId} concluded ${runConclusion}; nothing to read`);
    }
  }

  const escalations = mergeEscalations(rateEscalations, mainRed.escalations);
  if (escalations.length === 0) {
    notice(`nothing in ${repo} to escalate.`);
    return 0;
  }

  const bodyOpts = { repo, minK: verdict.minK, window: verdict.window, runUrl, ciRunUrl, readAt };
  const commentFor = (e) => (e.mainRed ? renderMainRedComment(e, { ciRunUrl, readAt }) : undefined);
  if (dryRun) {
    for (const e of escalations) {
      info(`DRY RUN — would upsert "${issueTitle(e.testId)}":`);
      process.stdout.write(renderIssueBody(e, bodyOpts) + "\n\n");
      const c = commentFor(e);
      if (c) process.stdout.write(`DRY RUN — and on an existing issue, comment:\n${c}\n\n`);
    }
    return 0;
  }

  let failures = 0;
  try {
    ensureLabel(repo);
  } catch (err) {
    error(`could not ensure label "${FLAKE_LABEL}": ${String(err.stderr || err.message).trim()}`);
    return 1;
  }
  let existing;
  try {
    existing = listFlakeIssues(repo);
  } catch (err) {
    error(
      `could not list existing "${FLAKE_LABEL}" issues: ${String(err.stderr || err.message).trim()}`,
    );
    return 1;
  }
  const actions = planIssueActions(escalations, existing);
  for (const action of actions) {
    if (action.duplicates.length > 0) {
      warn(
        `"${action.title}" has duplicate issue(s) #${action.duplicates.join(", #")}; ` +
          `#${action.number} owns it — close the others by hand`,
      );
    }
    try {
      const ref = applyAction(action, renderIssueBody(action.escalation, bodyOpts), repo, execGh, {
        comment: commentFor(action.escalation),
      });
      info(`${action.action} ${ref}  "${action.title}"`);
    } catch (err) {
      failures += 1;
      error(
        `${action.action} failed for "${action.title}": ${String(err.stderr || err.message).trim()}`,
      );
    }
  }
  if (failures > 0) {
    error(`${failures} of ${actions.length} issue write(s) failed`);
    return 1;
  }
  return 0;
}

const invokedDirectly =
  process.argv[1] && resolve(process.argv[1]) === resolve(fileURLToPath(import.meta.url));
if (invokedDirectly) {
  main(process.argv.slice(2))
    .then((code) => {
      process.exitCode = code;
    })
    .catch((e) => {
      error(`unexpected failure: ${e?.stack || e}`);
      process.exitCode = 1;
    });
}

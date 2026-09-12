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
 * ESCALATION RULE (plan, Phase 2): a test escalates at ≥ 3 occurrences —
 * runs whose outcome DISAGREED with the test's modal outcome — inside coord's
 * window. `occurrences = round(flake_rate × sample_size)`, so with the
 * default window of 20 that is `flake_rate ≥ 0.15`. The rule's second arm,
 * "any occurrence on a push to `refs/heads/main`", needs per-run outcomes the
 * flakiness endpoint does not serve; it is NOT implemented here and is named
 * as such in the plan.
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
 *        [--min-occurrences 3] [--run-url <url>] [--dry-run]
 *
 * ENV
 *   COORD_HTTP_URL   coord base URL. Default https://coord.qontinui.io
 *                    (mirrors ci-test-results-ingest.mjs).
 *   GH_TOKEN         what `gh` authenticates with (needs `issues: write`).
 *
 * EXIT CODES
 *   0  read was `ok` (issues upserted, or `--dry-run` printed the plan) or
 *      `thin` (nothing scorable yet — a notice, not an error).
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

const DEFAULT_COORD_URL = "https://coord.qontinui.io";
const FLAKINESS_PATH = "/coord/test-flakiness";

/// The plan's threshold: "at 3 occurrences of one test … it stops being a
/// counter and becomes a plan".
export const DEFAULT_MIN_OCCURRENCES = 3;

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
 * `occurrences` is the number of runs in the window whose outcome disagreed
 * with the modal outcome — `flake_rate × sample_size`, rounded, because
 * coord's rate is exactly that ratio and the plan's rule counts occurrences.
 * A prior with a malformed shape is skipped rather than escalated: this
 * function files nothing on a guess.
 */
export function selectEscalations(priors, { minOccurrences = DEFAULT_MIN_OCCURRENCES } = {}) {
  const out = [];
  for (const [testId, prior] of Object.entries(priors ?? {})) {
    if (!prior || typeof prior !== "object") continue;
    const flakeRate = Number(prior.flake_rate);
    const sampleSize = Number(prior.sample_size);
    if (!Number.isFinite(flakeRate) || !Number.isFinite(sampleSize)) continue;
    if (flakeRate <= 0 || sampleSize <= 0) continue;
    const occurrences = Math.round(flakeRate * sampleSize);
    if (occurrences < minOccurrences) continue;
    out.push({
      testId,
      flakeRate,
      sampleSize,
      occurrences,
      modalOutcome: typeof prior.modal_outcome === "string" ? prior.modal_outcome : "unknown",
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

/** The stable, idempotent issue title for a test id. */
export function issueTitle(testId) {
  const full = TITLE_PREFIX + testId;
  if (full.length <= MAX_TITLE_LEN) return full;
  const digest = createHash("sha256").update(testId).digest("hex").slice(0, 8);
  const room = MAX_TITLE_LEN - TITLE_PREFIX.length - digest.length - 2; // "…" + " "
  return `${TITLE_PREFIX}${testId.slice(0, room)}… ${digest}`;
}

/** Markdown body for an escalation issue — every number read back from coord. */
export function renderIssueBody(escalation, { repo, minK, window, runUrl, readAt } = {}) {
  const pct = (escalation.flakeRate * 100).toFixed(1);
  const lines = [
    `coord's per-test flake history escalated this test (plan`,
    `\`2026-08-30-runner-ci-has-no-flake-detection-so-one-flaky-test-freezes-the-train\`, Phase 2).`,
    ``,
    `| | |`,
    `|---|---|`,
    `| test | \`${escalation.testId}\` |`,
    `| flake_rate | **${escalation.flakeRate.toFixed(3)}** (${pct}%) |`,
    `| occurrences | **${escalation.occurrences}** of ${escalation.sampleSize} runs disagreed with the modal outcome |`,
    `| modal outcome | \`${escalation.modalOutcome}\` |`,
    `| window | last ${window ?? "?"} runs per test (min_k ${minK ?? "?"}) |`,
    `| repo | \`${repo ?? "?"}\` |`,
    `| read at | ${readAt ?? "?"} |`,
    `| escalated by | ${runUrl ? `[this run](${runUrl})` : "a manual run"} |`,
    ``,
    `**Read the number before reading the word "flaky".** Every CI run ingests`,
    `one row per platform leg under the SAME test id (coord's \`test_id\` carries`,
    `no platform component), so a window of ${window ?? "N"} runs is ~${window ? Math.round(window / 2) : "N/2"} CI runs × 2`,
    `legs. A rate near **0.500** with modal \`pass\` is therefore usually a test`,
    `that fails DETERMINISTICALLY on one platform and passes on the other — the`,
    `plan's own Phase 0 found 10 of 12 same-SHA disagreements were windows-only —`,
    `not nondeterminism. Check the platform split (the \`shard\` column in`,
    `\`coord.test_results\`, which this endpoint does not serve) before treating`,
    `it as a flake.`,
    ``,
    `Source: \`POST /coord/test-flakiness\` \`{"repo":"${repo ?? ""}"}\` — the rate is`,
    `coord's \`flakiness_priors\` over \`coord.test_results\`, filled by the`,
    `\`Report test results to coord\` step of \`ci.yml\` (see the plan's "Known`,
    `limitation" for why the platform is not in the key).`,
    ``,
    `**Root-cause it before touching the threshold.** Ask *test or production*`,
    `*defect?* first, and expect the answer to sometimes be production — that is`,
    `what it was for \`#1178\`. The first escalation this rail produced,`,
    `\`wedge_diagnostics::tests::a_spinning_child_reports_meaningful_cpu\` at`,
    `0.400, was a test defect (qontinui-runner#1444).`,
    ``,
    `This issue is refreshed on every scheduled run while the test stays above`,
    `threshold, and reopened if it is closed while still above it. Close it once`,
    `the fix has landed; a later re-escalation reopens it rather than filing a`,
    `duplicate.`,
  ];
  return lines.join("\n");
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

async function readFlakiness(base, repo) {
  const url = base + FLAKINESS_PATH;
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), REQUEST_TIMEOUT_MS);
  const started = Date.now();
  try {
    const res = await fetch(url, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ repo }),
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
    maxBuffer: 16 * 1024 * 1024,
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

/** Apply one planned action; returns the created URL or `#<number>`. */
export function applyAction(action, body, repo, gh = execGh) {
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
    // fall through — a reopened issue gets the fresh body too
    case "update":
      gh(["issue", "edit", String(action.number), "--add-label", FLAKE_LABEL, "--body-file", "-"], {
        repo,
        input: body,
      });
      return `#${action.number}`;
    default:
      throw new Error(`unknown action ${action.action}`);
  }
}

function printUsage(stream) {
  stream.write(
    [
      "usage: node scripts/ci-flake-escalate.mjs --repo <owner/repo>",
      "         [--min-occurrences N] [--run-url <url>] [--dry-run]",
      "",
      "Reads POST /coord/test-flakiness for <repo> and upserts one",
      "`flaky test: <test id>` GitHub issue (label `flake`) per test at or above",
      `N disagreeing runs in coord's window (default ${DEFAULT_MIN_OCCURRENCES}).`,
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
        "run-url": { type: "string" },
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
  const dryRun = Boolean(parsed.values["dry-run"]);
  const runUrl = parsed.values["run-url"];

  const base = (process.env.COORD_HTTP_URL || DEFAULT_COORD_URL).replace(/\/+$/, "");
  const readAt = new Date().toISOString();
  const read = await readFlakiness(base, repo);
  if (read.error) {
    error(
      `flakiness read UNKNOWN — ${read.error}. Filing nothing: an unreadable rail is not a clean one.`,
    );
    return 1;
  }
  const verdict = classifyFlakinessResponse(read.body);
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
    return 0;
  }

  const escalations = selectEscalations(verdict.priors, { minOccurrences });
  const flakyCount = Object.values(verdict.priors).filter((p) => Number(p?.flake_rate) > 0).length;
  info(
    `${flakyCount} test(s) with a non-zero flake_rate; ${escalations.length} at or above ` +
      `${minOccurrences} occurrence(s) in the window`,
  );
  for (const e of escalations) {
    info(
      `  ${e.flakeRate.toFixed(3)}  ${e.occurrences}/${e.sampleSize}  modal=${e.modalOutcome}  ${e.testId}`,
    );
  }
  if (escalations.length === 0) {
    notice(
      `no test in ${repo} is at or above ${minOccurrences} disagreeing runs — nothing to escalate.`,
    );
    return 0;
  }

  const bodyOpts = { repo, minK: verdict.minK, window: verdict.window, runUrl, readAt };
  if (dryRun) {
    for (const e of escalations) {
      info(`DRY RUN — would upsert "${issueTitle(e.testId)}":`);
      process.stdout.write(renderIssueBody(e, bodyOpts) + "\n\n");
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
      const ref = applyAction(action, renderIssueBody(action.escalation, bodyOpts), repo);
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

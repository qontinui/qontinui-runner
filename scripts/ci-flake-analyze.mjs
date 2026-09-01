#!/usr/bin/env node
// ci-flake-analyze.mjs — read-only CI flake measurement over the GitHub API.
//
// Phase 0 of plan
// `2026-08-30-runner-ci-has-no-flake-detection-so-one-flaky-test-freezes-the-train`.
// It MEASURES and writes nothing: no workflow edit, no coord write, no issue.
//
// This repo emits no structured per-test record, so its results never reach
// `coord.test_results` and coord's `flakiness_priors` has nothing to score for
// `qontinui/qontinui-runner`. Until that rail is connected the only evidence
// available is the GitHub API itself, which is what this script reads.
//
// TWO CLASSES OF EVIDENCE, REPORTED SEPARATELY AND NEVER BLENDED
//
//   Class 1 — same-SHA disagreement. Two terminal conclusions at one
//     `head_sha` (across a run's attempts, or across two runs of the same
//     workflow at that SHA). This is the strongest class, and it is a
//     LOWER BOUND: GitHub only creates a second attempt when a human or coord
//     re-ran it, so a flake that reds `main` and is never re-run appears
//     exactly once and contributes ZERO here.
//
//   Class 2 — failing test names, counted per test across the window, split by
//     platform. A count here is not by itself evidence of flakiness (a
//     deterministically broken test also accumulates), which is precisely why
//     it is reported apart from class 1.
//
// THE `unparsed` BUCKET IS NOT "NO FLAKE"
//
//   Any run whose attempts could not be enumerated, and any failed test job
//   whose log could not be fetched, was truncated, or produced no recognisable
//   cargo output, is counted as `unparsed` and is NEVER averaged into any rate.
//   The sibling harness (`qontinui-web` `frontend/tools/spec-ci-flake/analyze.ts`)
//   had to be amended for exactly this defect — it silently dropped
//   artifact-less runs, so its criterion could never fire on that failure
//   class. Served policy `verification-and-evidence` `silent-empty-is-unknown`,
//   applied to a measurement tool.
//
// USAGE
//   node scripts/ci-flake-analyze.mjs [--runs 60] [--max-logs 40]
//                                     [--repo qontinui/qontinui-runner]
//                                     [--workflow ci.yml]
//                                     [--since 2026-08-20]
//                                     [--format pretty|json]
//
// FLAGS
//   --runs <N>      Window size: the N most recent workflow runs. Default 60.
//   --max-logs <N>  Hard cap on job-log fetches (logs are megabytes each).
//                   Default 40. The cap and how much of it was used are
//                   ALWAYS printed, so a truncated scan can never be mistaken
//                   for a complete one.
//   --repo <o/r>    Default `qontinui/qontinui-runner`.
//   --workflow <f>  Workflow file name. Default `ci.yml`.
//   --since <date>  Optional ISO date/datetime. Widens the window to every run
//                   created at or after it, ignoring --runs. Use this to reach
//                   back past a busy week — `--runs 60` on this repo covers
//                   roughly half a day.
//   --format        `pretty` (default) or `json`.
//
// EXIT CODES
//   0 — analysis completed (whatever it found; a flake is a finding, not an
//       error).
//   1 — runtime error (gh unavailable, API refused, etc).
//   2 — invalid CLI arguments.
//
// EXAMPLES
//   npm run ci-flake -- --runs 60
//   npm run ci-flake -- --since 2026-08-26 --max-logs 25 --format json

import { execFileSync } from "node:child_process";
import { parseArgs as nodeParseArgs } from "node:util";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";

const DEFAULT_REPO = "qontinui/qontinui-runner";
const DEFAULT_WORKFLOW = "ci.yml";
const DEFAULT_RUNS = 60;
const DEFAULT_MAX_LOGS = 40;

// ===========================================================================
// PURE PARSING — no I/O, no network. Everything below this banner and above
// the "LIVE HALF" banner is exercised directly by
// `scripts/__tests__/ci-flake-analyze.test.mjs` with literal fixtures.
// ===========================================================================

const ANSI_RE = /\[[0-9;]*[A-Za-z]/g;
const TIMESTAMP_RE = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d+Z ?/;

/**
 * Normalise one raw Actions log line: strip the ISO timestamp GitHub prefixes
 * onto every line, then strip ANSI colour escapes. Leading indentation AFTER
 * the timestamp is preserved — cargo's `failures:` block is identified by that
 * indentation, so eating it would break the parse.
 *
 * @param {string} line
 * @returns {string}
 */
export function normalizeLogLine(line) {
  return line.replace(/\r$/, "").replace(TIMESTAMP_RE, "").replace(ANSI_RE, "");
}

/**
 * Does this log contain output we recognise as a cargo test run at all?
 *
 * This is the gate for the `unparsed` bucket. A job that failed to compile, was
 * OOM-killed during link, hung on `apt`, or whose log we truncated will not
 * match — and that must read as UNKNOWN, never as "no failing tests".
 *
 * @param {string[]} lines normalised lines
 * @returns {boolean}
 */
export function hasCargoTestOutput(lines) {
  for (const line of lines) {
    const t = line.trim();
    if (t.startsWith("test result:")) return true;
    if (/^running \d+ tests?$/.test(t)) return true;
    if (/^test .+ \.\.\. (ok|FAILED|ignored)\b/.test(t)) return true;
  }
  return false;
}

/**
 * Extract failing test names from a cargo job log.
 *
 * Handles BOTH shapes cargo emits, because a log routinely contains only one:
 *
 *   1. the inline result line — `test some::path ... FAILED`
 *   2. the summary block — a `failures:` line followed by indented bare test
 *      paths. Note cargo prints `failures:` TWICE: once heading the captured
 *      stdout (`---- name stdout ----`, which is not indented and terminates
 *      the scan) and once heading the actual list.
 *
 * @param {string} logText raw job log (timestamps and ANSI still present)
 * @returns {{tests: string[], recognized: boolean, unparsed: boolean, reason: string|null}}
 *   `tests` is de-duplicated and sorted. `unparsed` is true — and MUST be
 *   surfaced by the caller as its own bucket — whenever the log carried no
 *   recognisable cargo output, regardless of `tests` being empty.
 */
export function parseFailingTests(logText) {
  if (typeof logText !== "string" || logText.length === 0) {
    return { tests: [], recognized: false, unparsed: true, reason: "empty log" };
  }

  const lines = logText.split("\n").map(normalizeLogLine);
  const recognized = hasCargoTestOutput(lines);
  const found = new Set();

  for (let i = 0; i < lines.length; i += 1) {
    const line = lines[i];

    // Shape 1: inline result line.
    const inline = /^test (\S+) \.\.\. FAILED\b/.exec(line.trim());
    if (inline) {
      found.add(inline[1]);
      continue;
    }

    // Shape 2: the `failures:` summary block.
    if (line.trim() !== "failures:") continue;
    let collected = 0;
    for (let j = i + 1; j < lines.length; j += 1) {
      const entry = lines[j];
      if (entry.trim() === "") {
        // Blank lines are allowed BEFORE the list starts, but a blank after we
        // have started collecting ends the block.
        if (collected > 0) break;
        continue;
      }
      const m = /^\s{2,}(\S+)\s*$/.exec(entry);
      if (!m || m[1].startsWith("-")) break;
      found.add(m[1]);
      collected += 1;
    }
  }

  return {
    tests: [...found].sort(),
    recognized,
    unparsed: !recognized,
    reason: recognized ? null : "no recognisable cargo test output",
  };
}

/**
 * Conclusions that are NOT a verdict on the commit.
 *
 * A `cancelled` run never finished judging the tree — somebody or something
 * stopped it — so pairing it with a `success` is not a same-SHA disagreement,
 * it is one verdict and one non-answer. Counting it as class 1 would inflate
 * the strongest evidence class with events that carry no evidence at all;
 * coord makes the same call, keeping the last conclusive baseline rather than
 * treating a `cancelled` workflow_run as a verdict. These land in `unparsed`,
 * where an unknown belongs.
 */
const NON_TERMINAL_CONCLUSIONS = new Set(["cancelled", "skipped"]);

/**
 * Reduce a SHA's terminal conclusions to a class-1 verdict.
 *
 * @param {Array<{attempt?: number, conclusion: string|null|undefined, status?: string}>} attempts
 * @returns {{conclusions: Array<string|null>, distinct: string[], disagreement: boolean, unparsed: boolean, nonTerminal: number, missing: number, reason: string|null}}
 *   `disagreement` is true iff two or more DISTINCT terminal conclusions were
 *   observed. `[failure, failure]` is therefore NOT a disagreement — a
 *   deterministic failure is a broken commit, not a flake — and neither is
 *   `[cancelled, success]`. `unparsed` is true when an attempt produced no
 *   verdict (still running, cancelled, skipped, or not enumerable); it is
 *   reported alongside, never folded into, the verdict.
 */
export function groupAttemptConclusions(attempts) {
  if (!Array.isArray(attempts) || attempts.length === 0) {
    return {
      conclusions: [],
      distinct: [],
      disagreement: false,
      unparsed: true,
      nonTerminal: 0,
      missing: 0,
      reason: "no attempts enumerated",
    };
  }

  const conclusions = attempts.map((a) =>
    a && typeof a.conclusion === "string" && a.conclusion.length > 0
      ? a.conclusion
      : null,
  );
  const missing = conclusions.filter((c) => c === null).length;
  const nonTerminal = conclusions.filter(
    (c) => c !== null && NON_TERMINAL_CONCLUSIONS.has(c),
  ).length;
  const distinct = [
    ...new Set(
      conclusions.filter((c) => c !== null && !NON_TERMINAL_CONCLUSIONS.has(c)),
    ),
  ].sort();

  const reasons = [];
  if (missing > 0) reasons.push(`${missing} attempt(s) with no conclusion yet`);
  if (nonTerminal > 0) {
    reasons.push(`${nonTerminal} attempt(s) cancelled/skipped (no verdict)`);
  }

  return {
    conclusions,
    distinct,
    disagreement: distinct.length > 1,
    unparsed: missing + nonTerminal > 0,
    nonTerminal,
    missing,
    reason: reasons.length > 0 ? reasons.join("; ") : null,
  };
}

/**
 * Which matrix leg is this job? The matrix is `[ubuntu-22.04, windows-latest]`
 * and a flake on one platform only is a different defect, so the split is
 * load-bearing rather than cosmetic.
 *
 * @param {string} jobName
 * @returns {string} the matrix value, or `"unknown"`
 */
export function platformOfJobName(jobName) {
  const m = /\(([^)]+)\)/.exec(String(jobName ?? ""));
  if (!m) return "unknown";
  const inner = m[1].trim();
  return inner.length > 0 ? inner : "unknown";
}

/**
 * Is this one of the Rust `test` matrix jobs (`test (ubuntu-22.04)` etc)?
 *
 * @param {string} jobName
 * @returns {boolean}
 */
export function isRustTestJob(jobName) {
  return /^test \(/.test(String(jobName ?? ""));
}

/**
 * Per-JOB conclusion split at one SHA — a flake confined to one matrix leg is
 * a different defect from one that hits both.
 *
 * @param {Array<{attempt: number, jobs: Array<{name: string, conclusion: string|null}>}>} attemptJobs
 * @returns {Array<{name: string, platform: string, conclusions: Array<string|null>, distinct: string[], disagreement: boolean}>}
 */
export function groupJobConclusions(attemptJobs) {
  const byName = new Map();
  for (const entry of attemptJobs ?? []) {
    for (const job of entry?.jobs ?? []) {
      const name = job?.name ?? "unknown";
      if (!byName.has(name)) byName.set(name, []);
      byName.get(name).push(
        typeof job?.conclusion === "string" && job.conclusion.length > 0
          ? job.conclusion
          : null,
      );
    }
  }
  const out = [];
  for (const [name, conclusions] of byName) {
    // Same rule as groupAttemptConclusions: a cancelled/skipped job is a
    // non-answer, not a second verdict.
    const distinct = [
      ...new Set(
        conclusions.filter(
          (c) => c !== null && !NON_TERMINAL_CONCLUSIONS.has(c),
        ),
      ),
    ].sort();
    out.push({
      name,
      platform: platformOfJobName(name),
      conclusions,
      distinct,
      disagreement: distinct.length > 1,
    });
  }
  out.sort((a, b) => a.name.localeCompare(b.name));
  return out;
}

/**
 * Group workflow runs by `head_sha`, folding every run's attempts together, and
 * emit the class-1 verdict per SHA.
 *
 * A SHA can carry disagreement two ways: across the attempts of ONE run
 * (a re-run — the `33021749396` shape), or across two separate runs of the same
 * workflow at that SHA. Both are same-SHA disagreement; the `withinRun` flag
 * distinguishes them so a reader is never misled about which was proven.
 *
 * @param {Array<{id: number, head_sha: string, head_branch?: string, event?: string, attempts: Array<{attempt: number, conclusion: string|null, status?: string}>}>} runs
 * @returns {Array<{headSha: string, runIds: number[], branches: string[], events: string[], conclusions: Array<string|null>, distinct: string[], disagreement: boolean, withinRun: boolean, unparsed: boolean}>}
 */
export function classifyRunsBySha(runs) {
  const bySha = new Map();
  for (const run of runs ?? []) {
    const sha = run?.head_sha ?? "unknown";
    if (!bySha.has(sha)) bySha.set(sha, []);
    bySha.get(sha).push(run);
  }

  const out = [];
  for (const [headSha, group] of bySha) {
    const allAttempts = [];
    let withinRun = false;
    for (const run of group) {
      const verdict = groupAttemptConclusions(run.attempts ?? []);
      if (verdict.disagreement) withinRun = true;
      for (const a of run.attempts ?? []) allAttempts.push(a);
    }
    const verdict = groupAttemptConclusions(allAttempts);
    out.push({
      headSha,
      runIds: group.map((r) => r.id),
      branches: [...new Set(group.map((r) => r.head_branch).filter(Boolean))],
      events: [...new Set(group.map((r) => r.event).filter(Boolean))],
      conclusions: verdict.conclusions,
      distinct: verdict.distinct,
      disagreement: verdict.disagreement,
      withinRun,
      unparsed: verdict.unparsed,
      nonTerminal: verdict.nonTerminal,
      missing: verdict.missing,
    });
  }
  return out;
}

/**
 * Fold per-job failing-test extractions into a per-test occurrence table split
 * by platform.
 *
 * @param {Array<{platform: string, tests: string[]}>} jobResults
 * @returns {Array<{test: string, total: number, byPlatform: Record<string, number>}>}
 *   sorted by total descending, then test name ascending.
 */
export function tallyFailingTests(jobResults) {
  const table = new Map();
  for (const r of jobResults ?? []) {
    for (const test of r?.tests ?? []) {
      if (!table.has(test)) table.set(test, { test, total: 0, byPlatform: {} });
      const row = table.get(test);
      row.total += 1;
      const p = r.platform ?? "unknown";
      row.byPlatform[p] = (row.byPlatform[p] ?? 0) + 1;
    }
  }
  return [...table.values()].sort(
    (a, b) => b.total - a.total || a.test.localeCompare(b.test),
  );
}

/**
 * Evaluate the plan's PROCEED CRITERION, which was written down before any data
 * was seen so it could not be fitted to it:
 *
 *   implement Phases 1-2 if the window shows >= 2 distinct test names with a
 *   same-SHA disagreement, OR >= 1 test name with >= 3 occurrences.
 *
 * The `ambiguous` arm exists because `unparsed` is not zero: when the criterion
 * is not met but the scan left unparsed buckets behind, the honest verdict is
 * "not met over what could be read", not "not met".
 *
 * @param {{distinctTestsInDisagreement: number, maxOccurrences: number, unparsed: number}} counts
 * @returns {{met: boolean, ambiguous: boolean, verdict: string, why: string}}
 */
export function evaluateProceedCriterion(counts) {
  const distinct = counts?.distinctTestsInDisagreement ?? 0;
  const maxOcc = counts?.maxOccurrences ?? 0;
  const unparsed = counts?.unparsed ?? 0;

  const armA = distinct >= 2;
  const armB = maxOcc >= 3;
  const met = armA || armB;

  if (met) {
    return {
      met: true,
      ambiguous: false,
      verdict: "MET",
      why: [
        armA
          ? `arm A satisfied: ${distinct} distinct test names carry a same-SHA disagreement (>= 2)`
          : `arm A not satisfied: ${distinct} distinct test names carry a same-SHA disagreement (< 2)`,
        armB
          ? `arm B satisfied: max per-test occurrence count is ${maxOcc} (>= 3)`
          : `arm B not satisfied: max per-test occurrence count is ${maxOcc} (< 3)`,
      ].join("; "),
    };
  }

  if (unparsed > 0) {
    return {
      met: false,
      ambiguous: true,
      verdict: "MET-AMBIGUOUSLY (not met over what could be read)",
      why: `arm A ${distinct}/2, arm B ${maxOcc}/3 — but ${unparsed} unparsed bucket(s) were not readable, so this is UNKNOWN over that slice rather than a negative`,
    };
  }

  return {
    met: false,
    ambiguous: false,
    verdict: "NOT MET",
    why: `arm A ${distinct}/2 distinct test names with same-SHA disagreement, arm B ${maxOcc}/3 max per-test occurrences, 0 unparsed buckets`,
  };
}

// ===========================================================================
// LIVE HALF — everything below shells out to `gh` and is not unit tested.
// ===========================================================================

/**
 * One `gh api` call, returning parsed JSON. `gh` is expected to be
 * authenticated in this environment; no token is hand-rolled.
 */
function ghJson(path) {
  const out = execFileSync("gh", ["api", path], {
    encoding: "utf8",
    maxBuffer: 128 * 1024 * 1024,
    stdio: ["ignore", "pipe", "pipe"],
  });
  return JSON.parse(out);
}

/** Raw (non-JSON) `gh api` call — used for job logs, which are plain text. */
function ghText(path) {
  return execFileSync("gh", ["api", path], {
    encoding: "utf8",
    maxBuffer: 256 * 1024 * 1024,
    stdio: ["ignore", "pipe", "pipe"],
  });
}

function listRuns({ repo, workflow, runs, since, log }) {
  const collected = [];
  const sinceMs = since ? Date.parse(since) : null;
  for (let page = 1; page <= 50; page += 1) {
    const path = `repos/${repo}/actions/workflows/${workflow}/runs?per_page=100&page=${page}`;
    const body = ghJson(path);
    const batch = body.workflow_runs ?? [];
    if (batch.length === 0) break;
    for (const r of batch) {
      if (sinceMs !== null && Date.parse(r.created_at) < sinceMs) {
        return collected;
      }
      collected.push(r);
      if (sinceMs === null && collected.length >= runs) return collected;
    }
    log(`  … listed ${collected.length} runs`);
  }
  return collected;
}

function fetchAttempts({ repo, run, log }) {
  // The run object IS the final attempt, so only 1..run_attempt-1 need a call.
  const attempts = [];
  const total = Number(run.run_attempt ?? 1);
  for (let n = 1; n < total; n += 1) {
    try {
      const a = ghJson(`repos/${repo}/actions/runs/${run.id}/attempts/${n}`);
      attempts.push({
        attempt: n,
        conclusion: a.conclusion ?? null,
        status: a.status ?? null,
      });
    } catch (err) {
      log(`  ! attempt ${n} of run ${run.id} unreadable: ${err.message}`);
      attempts.push({ attempt: n, conclusion: null, status: "unreadable" });
    }
  }
  attempts.push({
    attempt: total,
    conclusion: run.conclusion ?? null,
    status: run.status ?? null,
  });
  return attempts;
}

function fetchJobs({ repo, runId, attempt }) {
  const path =
    attempt === null
      ? `repos/${repo}/actions/runs/${runId}/jobs?per_page=100`
      : `repos/${repo}/actions/runs/${runId}/attempts/${attempt}/jobs?per_page=100`;
  return ghJson(path).jobs ?? [];
}

async function main(argv) {
  let parsed;
  try {
    parsed = nodeParseArgs({
      args: argv,
      options: {
        repo: { type: "string", default: DEFAULT_REPO },
        workflow: { type: "string", default: DEFAULT_WORKFLOW },
        runs: { type: "string", default: String(DEFAULT_RUNS) },
        "max-logs": { type: "string", default: String(DEFAULT_MAX_LOGS) },
        since: { type: "string" },
        format: { type: "string", default: "pretty" },
        help: { type: "boolean", short: "h", default: false },
      },
      strict: true,
      allowPositionals: false,
    });
  } catch (err) {
    process.stderr.write(`error: ${err.message}\n`);
    printUsage(process.stderr);
    return 2;
  }

  const v = parsed.values;
  if (v.help) {
    printUsage(process.stdout);
    return 0;
  }
  const runsWanted = Number.parseInt(v.runs, 10);
  if (!Number.isInteger(runsWanted) || runsWanted < 1) {
    process.stderr.write(`error: --runs must be a positive integer\n`);
    return 2;
  }
  const maxLogs = Number.parseInt(v["max-logs"], 10);
  if (!Number.isInteger(maxLogs) || maxLogs < 0) {
    process.stderr.write(`error: --max-logs must be a non-negative integer\n`);
    return 2;
  }
  if (v.format !== "pretty" && v.format !== "json") {
    process.stderr.write(`error: --format must be "pretty" or "json"\n`);
    return 2;
  }
  if (v.since && Number.isNaN(Date.parse(v.since))) {
    process.stderr.write(`error: --since must be an ISO date\n`);
    return 2;
  }

  const log = (m) => process.stderr.write(`${m}\n`);
  const startedAt = new Date().toISOString();

  log(`ci-flake-analyze: listing runs for ${v.repo} ${v.workflow} …`);
  const rawRuns = listRuns({
    repo: v.repo,
    workflow: v.workflow,
    runs: runsWanted,
    since: v.since,
    log,
  });
  log(`ci-flake-analyze: ${rawRuns.length} runs in window`);

  // ---- Class 1 --------------------------------------------------------
  const runsWithAttempts = [];
  const attemptFetchFailures = [];
  for (const run of rawRuns) {
    let attempts;
    if (Number(run.run_attempt ?? 1) > 1) {
      log(`  attempts: run ${run.id} (${run.run_attempt} attempts)`);
      attempts = fetchAttempts({ repo: v.repo, run, log });
      if (attempts.some((a) => a.status === "unreadable")) {
        attemptFetchFailures.push(run.id);
      }
    } else {
      attempts = [
        {
          attempt: 1,
          conclusion: run.conclusion ?? null,
          status: run.status ?? null,
        },
      ];
    }
    runsWithAttempts.push({
      id: run.id,
      head_sha: run.head_sha,
      head_branch: run.head_branch,
      event: run.event,
      created_at: run.created_at,
      run_attempt: Number(run.run_attempt ?? 1),
      attempts,
    });
  }

  const shaVerdicts = classifyRunsBySha(runsWithAttempts);
  const class1 = shaVerdicts.filter((s) => s.disagreement);
  const unparsedShas = shaVerdicts.filter((s) => s.unparsed && !s.disagreement);

  // Per-job split for every class-1 SHA.
  for (const s of class1) {
    const attemptJobs = [];
    s.jobSplitUnparsed = false;
    for (const runId of s.runIds) {
      const run = runsWithAttempts.find((r) => r.id === runId);
      for (const a of run.attempts) {
        try {
          const jobs = fetchJobs({
            repo: v.repo,
            runId,
            attempt: run.run_attempt > 1 ? a.attempt : null,
          });
          attemptJobs.push({
            attempt: a.attempt,
            jobs: jobs.map((j) => ({ name: j.name, conclusion: j.conclusion })),
          });
        } catch (err) {
          log(`  ! jobs for run ${runId} attempt ${a.attempt}: ${err.message}`);
          s.jobSplitUnparsed = true;
        }
      }
    }
    s.jobSplit = groupJobConclusions(attemptJobs);
    s.disagreeingJobs = s.jobSplit.filter((j) => j.disagreement);
  }

  // ---- Class 2 --------------------------------------------------------
  // Log fetches are megabytes each, so failed `test (...)` jobs are the only
  // thing fetched, class-1 SHAs first, newest first after that.
  const class1ShaSet = new Set(class1.map((s) => s.headSha));
  const failedRuns = runsWithAttempts.filter((r) =>
    r.attempts.some((a) => a.conclusion === "failure"),
  );
  const candidateJobs = [];
  const jobEnumFailures = [];
  for (const run of failedRuns) {
    for (const a of run.attempts) {
      if (a.conclusion !== "failure") continue;
      try {
        const jobs = fetchJobs({
          repo: v.repo,
          runId: run.id,
          attempt: run.run_attempt > 1 ? a.attempt : null,
        });
        for (const j of jobs) {
          if (!isRustTestJob(j.name)) continue;
          if (j.conclusion !== "failure") continue;
          candidateJobs.push({
            jobId: j.id,
            name: j.name,
            platform: platformOfJobName(j.name),
            runId: run.id,
            attempt: a.attempt,
            headSha: run.head_sha,
            branch: run.head_branch,
            createdAt: run.created_at,
            priority: class1ShaSet.has(run.head_sha) ? 0 : 1,
          });
        }
      } catch (err) {
        log(`  ! jobs for run ${run.id} attempt ${a.attempt}: ${err.message}`);
        jobEnumFailures.push({ runId: run.id, attempt: a.attempt });
      }
    }
  }
  candidateJobs.sort(
    (a, b) =>
      a.priority - b.priority || Date.parse(b.createdAt) - Date.parse(a.createdAt),
  );

  const toFetch = candidateJobs.slice(0, maxLogs);
  const skippedForCap = candidateJobs.length - toFetch.length;
  const jobResults = [];
  const unparsedJobs = [];
  for (const job of toFetch) {
    log(`  log: job ${job.jobId} (${job.name}) of run ${job.runId}`);
    let text = "";
    try {
      text = ghText(`repos/${v.repo}/actions/jobs/${job.jobId}/logs`);
    } catch (err) {
      unparsedJobs.push({ ...job, reason: `log fetch failed: ${err.message}` });
      continue;
    }
    const p = parseFailingTests(text);
    if (p.unparsed) {
      unparsedJobs.push({ ...job, reason: p.reason });
      continue;
    }
    jobResults.push({ ...job, tests: p.tests });
    if (p.tests.length === 0) {
      // Recognisable cargo output but no named failure — the job failed at a
      // step other than a test assertion (compile, clippy, timeout). Recorded
      // so the totals below reconcile.
      job.noNamedFailure = true;
    }
  }

  const tally = tallyFailingTests(jobResults);
  const testsInDisagreement = new Set();
  for (const r of jobResults) {
    if (!class1ShaSet.has(r.headSha)) continue;
    for (const t of r.tests) testsInDisagreement.add(t);
  }

  const unparsedTotal =
    attemptFetchFailures.length +
    unparsedShas.length +
    jobEnumFailures.length +
    unparsedJobs.length;

  const criterion = evaluateProceedCriterion({
    distinctTestsInDisagreement: testsInDisagreement.size,
    maxOccurrences: tally.length > 0 ? tally[0].total : 0,
    unparsed: unparsedTotal,
  });

  const platformTotals = {};
  for (const r of jobResults) {
    platformTotals[r.platform] = (platformTotals[r.platform] ?? 0) + 1;
  }

  const report = {
    generatedAt: startedAt,
    repo: v.repo,
    workflow: v.workflow,
    window: {
      requestedRuns: v.since ? null : runsWanted,
      since: v.since ?? null,
      runsScanned: rawRuns.length,
      oldestRun: rawRuns.length ? rawRuns[rawRuns.length - 1].created_at : null,
      newestRun: rawRuns.length ? rawRuns[0].created_at : null,
      distinctShas: shaVerdicts.length,
      runsWithMultipleAttempts: runsWithAttempts.filter(
        (r) => r.run_attempt > 1,
      ).length,
    },
    logBudget: {
      maxLogs,
      candidateFailedTestJobs: candidateJobs.length,
      logsFetched: toFetch.length,
      skippedForCap,
      complete: skippedForCap === 0,
    },
    class1: {
      note: "LOWER BOUND — GitHub only creates a second attempt when a human or coord re-ran it, so a flake that reds a ref and is never re-run contributes ZERO here. `cancelled`/`skipped` are NOT counted as a second conclusion (a cancellation is a non-answer, not a verdict); those attempts land in `unparsed` instead.",
      shasWithDisagreement: class1.length,
      details: class1.map((s) => ({
        headSha: s.headSha,
        runIds: s.runIds,
        branches: s.branches,
        events: s.events,
        conclusions: s.conclusions,
        distinct: s.distinct,
        withinRun: s.withinRun,
        attemptsWithoutVerdict: (s.nonTerminal ?? 0) + (s.missing ?? 0),
        jobSplitUnparsed: Boolean(s.jobSplitUnparsed),
        disagreeingJobs: (s.disagreeingJobs ?? []).map((j) => ({
          name: j.name,
          platform: j.platform,
          conclusions: j.conclusions,
        })),
      })),
    },
    class2: {
      note: "Occurrence count is NOT by itself evidence of flakiness — a deterministically broken test also accumulates. Reported apart from class 1, never blended.",
      testJobLogsParsed: jobResults.length,
      perTest: tally,
      platformTotalsOfParsedFailedJobs: platformTotals,
      testNamesAlsoInAClass1Sha: [...testsInDisagreement].sort(),
    },
    unparsed: {
      note: "NEVER averaged into any rate. Each entry is UNKNOWN, not 'no flake'.",
      total: unparsedTotal,
      runsWithUnreadableAttempts: attemptFetchFailures,
      shasWithoutTerminalConclusions: unparsedShas.map((s) => ({
        headSha: s.headSha,
        runIds: s.runIds,
        conclusions: s.conclusions,
        cancelledOrSkipped: s.nonTerminal ?? 0,
        stillRunning: s.missing ?? 0,
      })),
      jobEnumerationFailures: jobEnumFailures,
      unparsedTestJobLogs: unparsedJobs.map((j) => ({
        jobId: j.jobId,
        name: j.name,
        runId: j.runId,
        attempt: j.attempt,
        headSha: j.headSha,
        reason: j.reason,
      })),
    },
    proceedCriterion: {
      statedBeforeData:
        ">= 2 distinct test names with a same-SHA disagreement, OR >= 1 test name with >= 3 occurrences",
      distinctTestsInDisagreement: testsInDisagreement.size,
      maxPerTestOccurrences: tally.length > 0 ? tally[0].total : 0,
      ...criterion,
    },
  };

  if (v.format === "json") {
    process.stdout.write(`${JSON.stringify(report, null, 2)}\n`);
  } else {
    process.stdout.write(formatPretty(report));
  }
  return 0;
}

function formatPretty(r) {
  const L = [];
  L.push("=".repeat(78));
  L.push(`CI flake analysis — ${r.repo} / ${r.workflow}`);
  L.push(`generated ${r.generatedAt}`);
  L.push("=".repeat(78));
  L.push("");
  L.push("WINDOW");
  L.push(
    `  runs scanned:            ${r.window.runsScanned}` +
      (r.window.since
        ? ` (--since ${r.window.since})`
        : ` (--runs ${r.window.requestedRuns})`),
  );
  L.push(`  newest run created:      ${r.window.newestRun}`);
  L.push(`  oldest run created:      ${r.window.oldestRun}`);
  L.push(`  distinct head_shas:      ${r.window.distinctShas}`);
  L.push(`  runs with >1 attempt:    ${r.window.runsWithMultipleAttempts}`);
  L.push("");
  L.push("LOG BUDGET (a truncated scan is never a complete one)");
  L.push(`  --max-logs cap:          ${r.logBudget.maxLogs}`);
  L.push(`  candidate failed test jobs: ${r.logBudget.candidateFailedTestJobs}`);
  L.push(`  logs actually fetched:   ${r.logBudget.logsFetched}`);
  L.push(`  skipped for cap:         ${r.logBudget.skippedForCap}`);
  L.push(
    `  scan of class 2 is:      ${r.logBudget.complete ? "COMPLETE over the window" : "TRUNCATED — class 2 counts are lower bounds too"}`,
  );
  L.push("");
  L.push("-".repeat(78));
  L.push(
    `CLASS 1 — same-SHA disagreement: ${r.class1.shasWithDisagreement} SHA(s)  [LOWER BOUND]`,
  );
  L.push("-".repeat(78));
  L.push(`  ${r.class1.note}`);
  if (r.class1.details.length === 0) {
    L.push("  (none in this window)");
  }
  for (const d of r.class1.details) {
    L.push("");
    L.push(`  head_sha ${d.headSha}`);
    L.push(`    runs:        ${d.runIds.join(", ")}`);
    L.push(`    branch(es):  ${d.branches.join(", ") || "?"}`);
    L.push(`    event(s):    ${d.events.join(", ") || "?"}`);
    L.push(`    conclusions: [${d.conclusions.map((c) => c ?? "null").join(", ")}]`);
    L.push(`    distinct:    [${d.distinct.join(", ")}]`);
    L.push(
      `    shape:       ${d.withinRun ? "within one run's attempts (re-run)" : "across separate runs at the same SHA"}`,
    );
    if (d.attemptsWithoutVerdict > 0) {
      L.push(
        `    caveat:      ${d.attemptsWithoutVerdict} attempt(s) here produced NO verdict (cancelled/skipped/running) and were excluded from distinct`,
      );
    }
    if (d.disagreeingJobs.length === 0) {
      L.push(
        `    job split:   ${d.jobSplitUnparsed ? "UNPARSED (job enumeration failed)" : "no single job disagreed (run-level only)"}`,
      );
    } else {
      L.push("    disagreeing jobs:");
      for (const j of d.disagreeingJobs) {
        L.push(
          `      ${j.name}  [${j.platform}]  [${j.conclusions.map((c) => c ?? "null").join(", ")}]`,
        );
      }
    }
  }
  L.push("");
  L.push("-".repeat(78));
  L.push("CLASS 2 — failing test names (occurrence count, split by platform)");
  L.push("-".repeat(78));
  L.push(`  ${r.class2.note}`);
  L.push(`  test-job logs parsed:    ${r.class2.testJobLogsParsed}`);
  if (r.class2.perTest.length === 0) {
    L.push("  (no named failing tests extracted)");
  }
  for (const t of r.class2.perTest) {
    const split = Object.entries(t.byPlatform)
      .sort(([a], [b]) => a.localeCompare(b))
      .map(([p, n]) => `${p}=${n}`)
      .join(" ");
    L.push(`    ${String(t.total).padStart(3)}x  ${t.test}   (${split})`);
  }
  L.push("");
  L.push("  PLATFORM SPLIT of parsed failed test jobs:");
  const pt = Object.entries(r.class2.platformTotalsOfParsedFailedJobs).sort(
    ([a], [b]) => a.localeCompare(b),
  );
  if (pt.length === 0) L.push("    (none)");
  for (const [p, n] of pt) L.push(`    ${p}: ${n}`);
  L.push("");
  L.push(
    `  test names that ALSO sit on a class-1 SHA: ${r.class2.testNamesAlsoInAClass1Sha.length}`,
  );
  for (const t of r.class2.testNamesAlsoInAClass1Sha) L.push(`    - ${t}`);
  L.push("");
  L.push("-".repeat(78));
  L.push(`UNPARSED — ${r.unparsed.total}`);
  L.push("-".repeat(78));
  L.push(`  ${r.unparsed.note}`);
  L.push(
    `  runs whose attempts were unreadable:    ${r.unparsed.runsWithUnreadableAttempts.length}`,
  );
  for (const id of r.unparsed.runsWithUnreadableAttempts) L.push(`    - run ${id}`);
  L.push(
    `  SHAs without a terminal conclusion:     ${r.unparsed.shasWithoutTerminalConclusions.length}`,
  );
  const shaList = r.unparsed.shasWithoutTerminalConclusions;
  for (const s of shaList.slice(0, 25)) {
    L.push(
      `    - ${s.headSha} runs [${s.runIds.join(", ")}] conclusions [${s.conclusions.map((c) => c ?? "null").join(", ")}]`,
    );
  }
  if (shaList.length > 25) {
    L.push(`    … and ${shaList.length - 25} more (use --format json for all)`);
  }
  L.push(
    `  job enumerations that failed:           ${r.unparsed.jobEnumerationFailures.length}`,
  );
  L.push(
    `  test-job logs with no cargo output:     ${r.unparsed.unparsedTestJobLogs.length}`,
  );
  for (const j of r.unparsed.unparsedTestJobLogs) {
    L.push(`    - job ${j.jobId} ${j.name} run ${j.runId} att ${j.attempt}: ${j.reason}`);
  }
  L.push("");
  L.push("=".repeat(78));
  L.push("PROCEED CRITERION (stated before the data was seen)");
  L.push("=".repeat(78));
  L.push(`  ${r.proceedCriterion.statedBeforeData}`);
  L.push("");
  L.push(
    `  arm A — distinct test names on a class-1 SHA: ${r.proceedCriterion.distinctTestsInDisagreement}  (needs >= 2)`,
  );
  L.push(
    `  arm B — max per-test occurrence count:        ${r.proceedCriterion.maxPerTestOccurrences}  (needs >= 3)`,
  );
  L.push(`  unparsed buckets:                             ${r.unparsed.total}`);
  L.push("");
  L.push(`  VERDICT: ${r.proceedCriterion.verdict}`);
  L.push(`  ${r.proceedCriterion.why}`);
  L.push("");
  return L.join("\n");
}

function printUsage(stream) {
  stream.write(
    [
      "Usage: ci-flake-analyze.mjs [--runs 60] [--max-logs 40] [--repo o/r]",
      "                           [--workflow ci.yml] [--since ISO-DATE]",
      "                           [--format pretty|json]",
      "",
      "Read-only. Measures CI flakiness from the GitHub API; writes nothing.",
      "",
      "  --runs <N>       Window: N most recent workflow runs (default 60)",
      "  --max-logs <N>   Cap on job-log fetches (default 40); always reported",
      "  --repo <o/r>     Default qontinui/qontinui-runner",
      "  --workflow <f>   Default ci.yml",
      "  --since <date>   Widen the window to every run at/after this ISO date",
      "  --format <fmt>   pretty (default) | json",
      "  -h, --help       Print this help and exit 0",
      "",
      "Exit codes:",
      "  0  analysis completed",
      "  1  runtime error",
      "  2  invalid arguments",
      "",
    ].join("\n"),
  );
}

// Only run when invoked directly, so the unit tests can import the module.
const invokedDirectly =
  process.argv[1] &&
  resolve(process.argv[1]) === resolve(fileURLToPath(import.meta.url));
if (invokedDirectly) {
  main(process.argv.slice(2))
    .then((code) => {
      process.exitCode = code;
    })
    .catch((e) => {
      process.stderr.write(`ci-flake-analyze: ${e?.stack ?? e}\n`);
      process.exitCode = 1;
    });
}

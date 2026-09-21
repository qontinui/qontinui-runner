#!/usr/bin/env node
/**
 * ci-test-results-ingest.mjs — best-effort push of one CI job's per-test
 * outcomes into `coord.test_results` via `POST /coord/test-results/ingest`.
 *
 * Phase 1 (redesigned) of plan
 * `2026-08-30-runner-ci-has-no-flake-detection-so-one-flaky-test-freezes-the-train`.
 * The original Phase 1 ran `cargo-nextest` a second time to get structured
 * output; measured on CI it cost ~74 added job-minutes/PR and was reverted
 * (commit `7ff875ec4`). This redesign parses the output the GATING
 * `cargo test --verbose` step already produces — zero extra execution — and
 * posts to coord's pre-parsed `results` path (not `raw`), which also carries
 * `shard` so each row can be attributed to its matrix platform.
 *
 * Deliberately reuses `parseTestOutcomes` from `ci-flake-analyze.mjs` rather
 * than re-deriving the parse: that module's docs-mandated invariant is
 * "MEASURES and writes nothing", so the network write lives here instead.
 *
 * NEVER FAILS THE CALLING CI JOB. A coord outage, a missing
 * `COORD_INGEST_TOKEN`, or cargo output this parser no longer recognises are
 * all reported as a `::warning::` annotation and this script still exits 0.
 * The gating verdict is `cargo test`'s own exit code from the EARLIER step;
 * this one is pure best-effort telemetry, run with `if: always()` so a
 * failed suite's results reach coord too. The caller should additionally set
 * `continue-on-error: true` on this step as defence in depth. This script
 * bounds its own network call internally, so no step-level `timeout-minutes`
 * is needed here regardless of the paragraph below.
 *
 * MEASURED, not asserted: Phase 0 of plan
 * 2026-08-31-published-build-parity-check's 2026-09-20 follow-up ran a
 * throwaway `continue-on-error: true` step that tripped its own
 * `timeout-minutes: 1` (github.com/qontinui/qontinui-runner/actions/runs/35730892188,
 * job step "continue-on-error step that trips its own timeout-minutes",
 * 2026-09-22). The step failed at exactly its bound
 * (`##[error]...has timed out after 1 minutes.`); the JOB did NOT cancel;
 * the next step ran immediately; a post-step (Swatinem/rust-cache) ran too;
 * the job concluded `success`. So pairing `continue-on-error` with a
 * step's own `timeout-minutes` did not, in this controlled run, cancel the
 * whole job.
 *
 * This is UNRECONCILED, not a correction, against `7ff875ec4`
 * ("revert(ci): drop the nextest shadow"), which reported the opposite from
 * a real incident on 2026-09-01 (run 33545829387: the nextest shadow step
 * recorded `cancelled`, every later step `skipped`, despite `if: always()`
 * on the ingest step) — that run's raw logs have since aged out of GitHub's
 * retention, so the discrepancy could not be re-diagnosed from source. Do
 * not treat either observation as authoritative over the other; both are
 * measurements of the same claim, on different occasions, with opposite
 * results. What is unaffected either way: this script needs no step-level
 * timeout because it bounds its own network call internally.
 *
 * USAGE
 *   node scripts/ci-test-results-ingest.mjs --log <path> --repo <owner/repo>
 *                                            --head-sha <sha> [--shard <platform>]
 *                                            [--gating-outcome <outcome>]
 *                                            [--classification <path>]
 *
 * CLASSIFICATION (Phase 1 of plan
 * `2026-09-17-runner-tests-share-in-process-mutable-state`). On a red run
 * the `Classify failed tests (solo re-run)` step of ci.yml re-runs each
 * failed test alone against the binaries the build half produced and writes
 * `test-classification.json` (`scripts/test-interleave-census.mjs
 * --classify-from-log`). Given `--classification <path>`, every pre-parsed
 * `results` row this script builds carries
 * `classification: "suite_only" | "solo_red" | "both_flaky" | null` — the
 * three verdicts by test id; null for a test the report did not classify
 * (green, UNRESOLVED, UNRESOLVED (budget), UNPARSED, or absent from it). A
 * file that is absent (every green run), unreadable or not the report's
 * shape is null on EVERY row and a `::notice`, never a failure: the
 * classification is a diagnostic riding on the ingest, not a precondition
 * of it. The token mapping is the census script's own
 * (`classificationsFromReport`), so the two never drift.
 *
 * ⚠️ THE FIELD IS NOT SENT TO COORD, for the reason the `--gating-outcome`
 * paragraph below states at length: `ResultItem` carries no such field and
 * no `deny_unknown_fields`, so a `classification` key in the POST body would
 * be silently dropped — written into a void that reads as success. The wire
 * rows are therefore stripped to coord's `ResultItem` shape
 * (`toWireRow`), byte-identical to what this script sent before the flag
 * existed, and the tests pin that. The classification lives in this step's
 * log (a per-label tally), in the classify step's Checks annotations, and in
 * the json itself, which `ci-flake-escalate.mjs --classification` reads
 * directly. Persisting it is the same three-repo change as the gating
 * outcome (a `qontinui-web` alembic column, a `qontinui-coord` request field
 * + INSERT, then one line here) — the plan's recorded coord gap.
 *
 * `--gating-outcome` is the GATING step's own `outcome` (Phase 4a of plan
 * `2026-09-17-the-windows-test-gate-is-a-90-minute-build-wearing-a-test-shaped-bound`).
 * Its ONLY effect is to make a disagreement VISIBLE: when the gating step did
 * NOT succeed and the parsed rows nevertheless carry no failure, this script
 * emits an `::error` annotation and a `$GITHUB_STEP_SUMMARY` line saying that
 * the rows it just wrote for this head are UNQUALIFIED. **The POST body is
 * byte-identical either way**, deliberately, and the tests pin that.
 *
 * The all-green conjunct is deliberate. A non-success gate whose own rows carry
 * a `FAILED` is the ordinary red PR: coord's rows are correct and complete, and
 * announcing a disagreement there would fire the alarm on the common path until
 * nobody read it.
 *
 * WHAT IT DOES NOT CATCH, stated because the motivating incident is subtler
 * than it first reads. On run 35043646051 attempt 1 this script recorded 7112
 * rows for a job GitHub called `failure`. Those rows are not "a passing suite":
 * the full suite is **11,486** tests across 29 binaries — 11,368 passed + 118
 * ignored, as its own `test result:` lines count it, and the `ignored` half
 * matters because this script emits a `skip` row for each. So 7112 is
 * **61.9%** of it.
 *
 * Note the denominator is the `test result:` ARITHMETIC, not literally the row
 * count this script emits, and the two differ by a hair: `parseTestOutcomes`
 * de-duplicates by `test_id` across binaries (worst outcome wins), and attempt
 * 1's log carries ~100 ids that appear in more than one binary. A review
 * measured the emitted sets at 11,485 and 7111 against the ingest's own
 * `11486/11486` and `7112/7112` log lines. Nothing here turns on ±1 — 61.9%
 * and 4374 are unchanged to the stated precision — but say which quantity is
 * meant rather than eliding the difference. Only **1409** of those rows came
 * from binaries that had reported a `test result:`; the other **5703** came
 * from the one binary still executing when the clock killed it, and 4374 tests
 * never ran. coord therefore received a **silently truncated** row set, in a
 * shape byte-indistinguishable from a complete one.
 *
 * This flag announces that the gate did not succeed; it cannot detect
 * truncation, because nothing here knows how many tests the suite has. That is
 * a separate, UNCLOSED gap, recorded as plan-library follow-up EDGE
 * `eff25606-d3e0-4307-9e9c-3ddbceb2b0aa` rather than implied to be covered.
 * That is an EDGE id on plan artifact `ae8b3f39-1bc6-47ab-adbf-2fe607462b3c`,
 * not an artifact id: read it back at `GET /api/v1/plan-library/followups`
 * (keyed `edge_id`; the feed carries no `to_id` column, consistent with it
 * listing only OPEN follow-ups). `GET /api/v1/plan-library/<edge id>` is
 * the ARTIFACT route and correctly 404s — a review of this change probed it
 * and reported the id unresolvable, so the door is named here.
 *
 * ⚠️ And the feed is PAGINATED AND UNFILTERED: measured 2026-09-19 it serves
 * `limit 50` of `total 134` oldest-first, and this row sits at index 131, so
 * the bare URL above is a 200 carrying neither id. `limit` and `offset` are
 * the ONLY parameters it implements — an unknown one (`?from_id=…`,
 * `?slug=…`) is REFUSED with HTTP 422 `unknown_query_parameter` naming the
 * accepted set, so a guessed filter fails loudly rather than looking like an
 * empty result. Use `?offset=100`, keep paging against `total`, and match
 * `from_id` client-side.
 *
 * WHY IT CANNOT DO MORE, verified at source on `qontinui-coord` `origin/main`
 * 2026-09-19 (`crates/coord/src/test_run_effects.rs`). There is nowhere for a
 * gating outcome to land: `ResultIngestRequest` carries no such field and no
 * `#[serde(deny_unknown_fields)]`, so an added body key is **silently dropped**
 * rather than rejected — a producer that "stamped" one would look correct and
 * write nothing; `ResultItem` is `{test_id, outcome, duration_seconds, shard}`;
 * `persist_test_results` writes a fixed column list whose `provenance` is a
 * server-side constant; `source` is CHECK-constrained to
 * `('ci','local','sandboxed','agent')` and silently coerced to `ci` otherwise,
 * and it is the credibility axis feeding the Tier-7 merge gate, so it is not
 * available to borrow; and `shard` is the platform-attribution axis Phase 0 of
 * the flake plan added. The durable stamp is therefore a three-repo change
 * (a `qontinui-web` alembic column, a `qontinui-coord` request field + INSERT,
 * then a flag here) tracked as that plan's Phase 4b. Do not "finish the job" by
 * adding a body key: it would write into a void that reads as success.
 *
 * ENV
 *   COORD_INGEST_TOKEN   Bearer token for the ingest route. Missing -> warn,
 *                        skip the network call, exit 0 (same posture as a
 *                        coord outage).
 *   COORD_HTTP_URL       coord base URL. Default https://coord.qontinui.io
 *                        (mirrors scripts/export-test-coverage.mjs).
 *
 * EXIT CODES
 *   Always 0, including on a coord/network failure or a missing token —
 *   best-effort by design (see above). Only a CLI usage error (a missing
 *   required flag) exits 2, since that can only mean this script's own
 *   invocation in ci.yml is broken and should be visible while wiring it up.
 */

import { appendFileSync, readFileSync } from "node:fs";
import { parseArgs as nodeParseArgs } from "node:util";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { parseTestOutcomes } from "./ci-flake-analyze.mjs";
import { classificationsFromReport } from "./test-interleave-census.mjs";

const DEFAULT_COORD_URL = "https://coord.qontinui.io";
const INGEST_PATH = "/coord/test-results/ingest";

/// Rows per POST.
///
/// MEASURED, not guessed. On run 34105940854 (head 98afa94af) this script sent
/// all 10,369 results as ONE request on both legs. `test (windows-latest)` got
/// HTTP 200; `test (ubuntu-22.04)` was aborted by its own client timeout at
/// EXACTLY 60 s and its half of the data was never recorded — same commit, same
/// payload, opposite outcome. A single all-or-nothing request at that size is a
/// coin flip, and losing one leg is worse than it sounds here: the flake signal
/// this data feeds is dominated by a platform axis (10 of the 12 same-SHA
/// disagreements Phase 0 found were windows-latest ONLY), so a dropped leg can
/// silently halve the very dimension the ingest exists to measure.
///
/// Chunking is the fix rather than a bigger timeout alone, because it bounds
/// per-request work AND makes progress partial: if chunk 7 fails, chunks 1-6
/// are already durable. coord supports this directly — its own coverage
/// producer POSTs one document per module against the same (repo, head_sha),
/// and `test_run_effects` appends rather than replaces.
const CHUNK_SIZE = 1000;

/// Per-REQUEST budget. Now bounds a ~1000-row chunk instead of the whole
/// suite, so it is far more headroom than the old 60 s was for 10k rows —
/// raised anyway because the failure mode is silent data loss, and the cost of
/// waiting is a best-effort step nobody is blocked on.
const REQUEST_TIMEOUT_MS = 120_000;

/**
 * Read the classifier's json into a test id → token map. NEVER throws: an
 * absent file (the green-run case, and the skipped-step case), an unreadable
 * one, non-JSON, or a JSON value that is not the report's shape all yield
 * `{ byId: null, note }` — and `byId: null` means "no classification",
 * which `buildIngestBody` renders as null on every row.
 *
 * @param {string|undefined|null} path
 * @param {{ read?: (p: string) => string }} [io]
 * @returns {{ byId: Map<string, string>|null, note: string|null }}
 */
export function readClassification(path, { read = (p) => readFileSync(p, "utf8") } = {}) {
  if (!path) return { byId: null, note: null };
  let text;
  try {
    text = read(path);
  } catch (err) {
    return {
      byId: null,
      note: `classification file ${path} not readable (${err?.code ?? err?.message ?? err}); classification=null on every row`,
    };
  }
  let report;
  try {
    report = JSON.parse(text);
  } catch (err) {
    return {
      byId: null,
      note: `classification file ${path} is not JSON (${err?.message ?? err}); classification=null on every row`,
    };
  }
  if (!report || typeof report !== "object" || Array.isArray(report) || !report.tests || typeof report.tests !== "object") {
    return {
      byId: null,
      note: `classification file ${path} carries no \`tests\` object; classification=null on every row`,
    };
  }
  return { byId: classificationsFromReport(report), note: null };
}

/**
 * Build the `POST /coord/test-results/ingest` body from a parsed log. Pure —
 * no I/O — so this is what the unit tests exercise directly.
 *
 * `classification`, when given, is the id → token map `readClassification`
 * produced; every row carries `classification` (the token, or null), so a
 * reader of the rows can never confuse "not classified" with "field absent".
 * The key is LOCAL to these pre-parsed rows: `toWireRow` strips it before
 * the POST (see the header).
 *
 * @returns {{body: object|null, warning: string|null}} `body` is null when
 *   there is nothing worth sending (unparsed log, or zero named tests); the
 *   caller must surface `warning` rather than silently skip.
 */
export function buildIngestBody({ logText, repo, headSha, shard, classification = null }) {
  const parsed = parseTestOutcomes(logText);
  if (parsed.unparsed) {
    return {
      body: null,
      warning: `log not recognised as cargo test output (${parsed.reason}); nothing to ingest`,
    };
  }
  if (parsed.tests.length === 0) {
    return {
      body: null,
      warning: "recognised cargo test output but zero named tests; nothing to ingest",
    };
  }
  return {
    body: {
      repo,
      head_sha: headSha,
      source: "ci",
      results: parsed.tests.map((t) => ({
        test_id: t.testId,
        outcome: t.outcome,
        ...(shard ? { shard } : {}),
        classification: classification?.get?.(t.testId) ?? null,
      })),
    },
    warning: null,
  };
}

/**
 * The row as coord's `ResultItem` reads it — `{test_id, outcome, shard?}` —
 * with the local `classification` key removed. PURE, and pinned by the
 * tests: the POST body must stay byte-identical to what this script sent
 * before `--classification` existed, because a key coord silently drops
 * would read as a successful write of nothing (see the header).
 */
export function toWireRow(row) {
  const { classification: _dropped, ...wire } = row;
  return wire;
}

/// The gating step outcomes GitHub can report. Anything else — an unexpanded
/// `${{ … }}`, an empty string, a typo — is UNKNOWN, and UNKNOWN is announced,
/// never read as `success`. Reading an unreadable outcome as a pass is exactly
/// the silent-empty-is-unknown failure this flag exists to end.
const KNOWN_GATING_OUTCOMES = new Set(["success", "failure", "cancelled", "skipped"]);

/**
 * Should this ingest be announced as UNQUALIFIED, and what should it say?
 * PURE — no I/O — so the tests exercise it directly.
 *
 * `undefined` (the flag was not passed at all) is NOT a warning: every caller
 * that predates the flag is correct as it stands, and turning their silence
 * into an annotation would make the loud case indistinguishable from the
 * ordinary one.
 *
 * @param {string|undefined} gatingOutcome
 * @param {{repo: string, headSha: string, rows: number, anyFailed: boolean}} ctx
 * @returns {{qualified: boolean, message: string|null}}
 */
export function gatingQualification(gatingOutcome, { repo, headSha, rows, anyFailed }) {
  if (gatingOutcome === undefined || gatingOutcome === null) {
    return { qualified: true, message: null };
  }
  if (gatingOutcome === "success") return { qualified: true, message: null };

  const recognised = KNOWN_GATING_OUTCOMES.has(gatingOutcome);

  // NARROWED: a RECOGNISED non-success outcome whose OWN rows carry a failure
  // is the ordinary red PR, and there is no disagreement to announce — the rows
  // coord holds are correct and complete, `FAILED` entries included. Alarming
  // on every red PR is how an alarm stops being read, and this flag exists for
  // the case where coord's rows and GitHub's verdict DISAGREE.
  //
  // ⚠️ GATED ON `recognised`, and that conjunct is load-bearing. An
  // UNRECOGNISED outcome — an empty string, an unexpanded `${{ … }}` — means
  // this script could not read the gate AT ALL, which is not "the ordinary red
  // PR" and must be announced whatever the rows say. An earlier cut put this
  // short-circuit above the recognition test, so a broken `steps.<id>.outcome`
  // reference went unreported on every red suite — the exact
  // silent-empty-is-unknown failure the paragraph below says this flag ends,
  // and the exact failure the step-`id` pin in
  // src-tauri/tests/ci_rust_test_steps_split.rs exists to catch upstream.
  if (recognised && anyFailed === true) return { qualified: true, message: null };

  const named = recognised
    ? `\`${gatingOutcome}\``
    : `an unrecognised value \`${gatingOutcome}\` (UNKNOWN, not success)`;
  return {
    qualified: false,
    message:
      `${rows} row(s) are being recorded in coord.test_results for ${repo}@${headSha} ` +
      `while the GATING step's outcome was ${named}. Those rows are UNQUALIFIED: ` +
      `coord has no column that can carry a gating outcome (verified at source, ` +
      `qontinui-coord crates/coord/src/test_run_effects.rs), so anything reading ` +
      `"did this head's tests pass" from that store alone gets an answer with no ` +
      `mention of the red check. Cross-read coord.pr_check_runs for this head. ` +
      `The durable fix is Phase 4b of plan ` +
      `2026-09-17-the-windows-test-gate-is-a-90-minute-build-wearing-a-test-shaped-bound.`,
  };
}

function printUsage(stream) {
  stream.write(
    [
      "Usage: ci-test-results-ingest.mjs --log <path> --repo <owner/repo> --head-sha <sha>",
      "                                  [--shard <platform>] [--gating-outcome <outcome>]",
      "                                  [--classification <path>]",
      "",
      "Best-effort: never fails the calling CI job. See file header.",
      "",
      "  --log <path>         Path to the captured `cargo test --verbose` output",
      "  --repo <owner/repo>  e.g. qontinui/qontinui-runner (must contain '/' —",
      "                       coord joins on the webhook's owner/name form)",
      "  --head-sha <sha>     Commit the results belong to",
      "  --shard <string>     Matrix leg, e.g. the platform (ubuntu-22.04) —",
      "                       closes the platform-attribution gap Phase 0 found",
      "  --gating-outcome <o> The GATING step's own outcome. Anything other than",
      "                       'success' is announced loudly; the payload is unchanged",
      "  --classification <path>",
      "                       The solo re-run classifier's json (test-interleave-",
      "                       census.mjs --classify-from-log); each pre-parsed row then",
      "                       carries classification: suite_only|solo_red|both_flaky|null,",
      "                       tallied in this step's log. NOT sent to coord (no column).",
      "                       Absent or unreadable -> null on every row, never a failure",
      "  -h, --help           Print this help and exit 0",
      "",
    ].join("\n"),
  );
}

function warn(msg) {
  process.stdout.write(`::warning title=test-results-ingest::${msg}\n`);
}
/// For a routine, expected absence — the classification file on a green run.
function noticeLine(msg) {
  process.stdout.write(`::notice title=test-results-ingest::${msg}\n`);
}
function info(msg) {
  process.stdout.write(`[ci-test-results-ingest] ${msg}\n`);
}
/// Loud, for data actually LOST.
///
/// This step is `continue-on-error`, so nothing here can (or should) fail the
/// job — the ingest must never gate a PR. But the previous version reported a
/// dropped leg with the same `::warning` it uses for routine notes, on a step
/// that then reports success inside a green job. That is indistinguishable from
/// working, which is how run 34105940854 lost half its data without anyone
/// noticing until the log was read by hand. `::error` costs nothing, changes no
/// verdict, and is the difference between a silent loss and a visible one.
function error(msg) {
  process.stdout.write(`::error title=test-results-ingest::${msg}\n`);
}
/// Append one markdown line to the job summary, where a human actually looks.
/// Best-effort and silent on failure — this whole script is a diagnostic on a
/// path that may already be red, and a summary it cannot write is not a reason
/// to add noise.
function stepSummary(markdown) {
  const path = process.env.GITHUB_STEP_SUMMARY;
  if (!path) return;
  try {
    appendFileSync(path, markdown + "\n", "utf8");
  } catch {
    /* nothing to do: `error()` above already carried the same text */
  }
}

/// Split `results` into runs of at most `size`. PURE — no I/O, no clock — so
/// the boundary behaviour is table-testable without a network.
///
/// A non-positive or non-finite `size` yields ONE chunk rather than throwing or
/// looping forever: a misconfigured constant must degrade to today's
/// single-request behaviour, never to an infinite loop in CI.
export function chunkResults(results, size) {
  if (!Array.isArray(results) || results.length === 0) return [];
  if (!Number.isFinite(size) || size <= 0) return [results];
  const out = [];
  for (let i = 0; i < results.length; i += size) {
    out.push(results.slice(i, i + size));
  }
  return out;
}

async function postOneChunk(url, body, token) {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), REQUEST_TIMEOUT_MS);
  const started = Date.now();
  try {
    const res = await fetch(url, {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
        Authorization: `Bearer ${token}`,
      },
      body: JSON.stringify(body),
      signal: controller.signal,
    });
    const text = await res.text().catch(() => "");
    const ms = Date.now() - started;
    if (!res.ok) {
      // Include the elapsed time on EVERY outcome. The 60 s abort was only
      // diagnosable because the timestamps happened to bracket it exactly;
      // printing the duration means the next reader does not need that luck.
      error(
        `non-2xx from ${url} after ${ms}ms: HTTP ${res.status} ${text.slice(0, 300)}`,
      );
      return { ok: false, ms, status: res.status };
    }
    let serverFailed = 0;
    try {
      const json = JSON.parse(text);
      if (typeof json.failed === "number") serverFailed = json.failed;
    } catch {
      // Non-JSON body — nothing further to check.
    }
    return { ok: true, ms, status: res.status, serverFailed };
  } catch (err) {
    const ms = Date.now() - started;
    const aborted = err?.name === "AbortError" || /abort/i.test(err?.message ?? "");
    error(
      aborted
        ? `request to ${url} ABORTED by the client after ${ms}ms ` +
            `(REQUEST_TIMEOUT_MS=${REQUEST_TIMEOUT_MS}); these rows were NOT recorded`
        : `request to ${url} failed after ${ms}ms: ${err.message}`,
    );
    return { ok: false, ms, aborted };
  } finally {
    clearTimeout(timer);
  }
}

async function postResults(url, body, token) {
  const chunks = chunkResults(body.results, CHUNK_SIZE);
  let sent = 0;
  let failedChunks = 0;
  let serverFailed = 0;

  for (const [i, results] of chunks.entries()) {
    // Every chunk repeats repo/head_sha/source — coord keys on those and
    // appends, so N posts for one head are a supported shape (its own coverage
    // producer does exactly this per module).
    const r = await postOneChunk(url, { ...body, results: results.map(toWireRow) }, token);
    if (r.ok) {
      sent += results.length;
      serverFailed += r.serverFailed ?? 0;
    } else {
      failedChunks += 1;
    }
    info(
      `chunk ${i + 1}/${chunks.length} (${results.length} rows) -> ` +
        `${r.ok ? `HTTP ${r.status}` : "FAILED"} in ${r.ms}ms`,
    );
  }

  const total = body.results.length;
  info(
    `POST ${url} (repo=${body.repo} head_sha=${body.head_sha}) — ` +
      `${sent}/${total} row(s) recorded across ${chunks.length} chunk(s)`,
  );
  if (failedChunks > 0) {
    error(
      `${failedChunks} of ${chunks.length} chunk(s) failed — ${total - sent} of ${total} ` +
        `row(s) were NOT recorded for ${body.repo}@${body.head_sha}` +
        (body.results[0]?.shard ? ` (shard ${body.results[0].shard})` : ""),
    );
  }
  if (serverFailed > 0) {
    warn(`${serverFailed} row(s) failed to persist server-side`);
  }
}

async function main(argv) {
  let parsed;
  try {
    parsed = nodeParseArgs({
      args: argv,
      options: {
        log: { type: "string" },
        repo: { type: "string" },
        "head-sha": { type: "string" },
        shard: { type: "string" },
        "gating-outcome": { type: "string" },
        classification: { type: "string" },
        help: { type: "boolean", short: "h" },
      },
      allowPositionals: false,
    });
  } catch (err) {
    process.stderr.write(`ci-test-results-ingest: ${err.message}\n`);
    printUsage(process.stderr);
    return 2;
  }
  if (parsed.values.help) {
    printUsage(process.stdout);
    return 0;
  }

  const { log, repo, shard } = parsed.values;
  const headSha = parsed.values["head-sha"];
  if (!log || !repo || !headSha) {
    process.stderr.write(
      "ci-test-results-ingest: --log, --repo and --head-sha are required\n",
    );
    printUsage(process.stderr);
    return 2;
  }

  let logText;
  try {
    logText = readFileSync(log, "utf8");
  } catch (err) {
    warn(`could not read ${log}: ${err.message}`);
    return 0;
  }

  const { byId: classification, note } = readClassification(parsed.values.classification);
  if (note) noticeLine(note);

  const { body, warning } = buildIngestBody({ logText, repo, headSha, shard, classification });
  if (warning) {
    warn(warning);
    return 0;
  }
  if (classification) {
    const tally = {};
    for (const r of body.results) {
      if (r.classification) tally[r.classification] = (tally[r.classification] ?? 0) + 1;
    }
    const summary = Object.entries(tally)
      .sort(([a], [b]) => a.localeCompare(b))
      .map(([k, v]) => `${k}=${v}`)
      .join(" ");
    info(
      `classification from ${parsed.values.classification}: ${classification.size} classified id(s) in the file, ` +
        `${summary || "none"} on this leg's rows (not sent to coord — no column; see the header)`,
    );
  }

  // Phase 4a: announce an UNQUALIFIED ingest loudly, and change nothing else.
  // Deliberately after `buildIngestBody` so the row count in the message is the
  // real one, and deliberately before the token check so the disagreement is
  // reported even on a box with no COORD_INGEST_TOKEN — the announcement is
  // about what coord will hold, and a skipped POST is its own separate warning.
  const qualification = gatingQualification(parsed.values["gating-outcome"], {
    repo,
    headSha,
    rows: body.results.length,
    anyFailed: body.results.some((r) => r.outcome === "fail"),
  });
  if (!qualification.qualified) {
    error(qualification.message);
    stepSummary(
      `### Test results recorded for a NON-SUCCESS gating step\n\n` +
        `- ${qualification.message}\n`,
    );
  }

  const token = process.env.COORD_INGEST_TOKEN;
  if (!token) {
    warn(
      `COORD_INGEST_TOKEN not set — ${body.results.length} result(s) parsed but not sent`,
    );
    return 0;
  }

  const base = (process.env.COORD_HTTP_URL || DEFAULT_COORD_URL).replace(/\/+$/, "");
  await postResults(base + INGEST_PATH, body, token);
  return 0;
}

const invokedDirectly =
  process.argv[1] &&
  resolve(process.argv[1]) === resolve(fileURLToPath(import.meta.url));
if (invokedDirectly) {
  main(process.argv.slice(2))
    .then((code) => {
      process.exitCode = code;
    })
    .catch((e) => {
      // Top-level guard: never fail the calling job on an unexpected throw.
      warn(`unexpected error: ${e?.stack ?? e}`);
      process.exitCode = 0;
    });
}

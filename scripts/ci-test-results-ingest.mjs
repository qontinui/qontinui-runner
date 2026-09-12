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
 * `continue-on-error: true` on this step as defence in depth — but avoid
 * pairing that with a GitHub Actions `timeout-minutes` on the SAME step: a
 * `continue-on-error` step that hits its OWN `timeout-minutes` cancels the
 * whole job, not just the step (learned the expensive way in this plan's
 * nextest revert). This script bounds its own network calls internally
 * instead — per request (`REQUEST_TIMEOUT_MS`) AND per invocation
 * (`DEFAULT_BUDGET_MS`, see there for why the second bound exists) — so no
 * step-level timeout is needed here.
 *
 * USAGE
 *   node scripts/ci-test-results-ingest.mjs --log <path> --repo <owner/repo>
 *                                            --head-sha <sha> [--shard <platform>]
 *
 * ENV
 *   COORD_INGEST_TOKEN   Bearer token for the ingest route. Missing -> warn,
 *                        skip the network call, exit 0 (same posture as a
 *                        coord outage).
 *   COORD_HTTP_URL       coord base URL. Default https://coord.qontinui.io
 *                        (mirrors scripts/export-test-coverage.mjs).
 *   COORD_INGEST_BUDGET_MS
 *                        Whole-invocation wall-clock budget across every
 *                        chunk. Default 180000 (3 min). Chunks that would
 *                        start past it are NOT sent and are reported as a
 *                        `::error::` naming the row count.
 *
 * EXIT CODES
 *   Always 0, including on a coord/network failure or a missing token —
 *   best-effort by design (see above). Only a CLI usage error (a missing
 *   required flag) exits 2, since that can only mean this script's own
 *   invocation in ci.yml is broken and should be visible while wiring it up.
 */

import { readFileSync } from "node:fs";
import { parseArgs as nodeParseArgs } from "node:util";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { parseTestOutcomes } from "./ci-flake-analyze.mjs";

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

/// Whole-INVOCATION budget, across every chunk. Override with
/// `COORD_INGEST_BUDGET_MS`.
///
/// "Nobody is blocked on" above turned out to be false. This step runs INSIDE
/// the gating `test` job, after `cargo test`, and coord's merge predicate
/// (`ci_outcome_for` in qontinui-coord `merge_scheduler.rs`) waits for every
/// check run on the head to complete — so the job's wall time is the merge
/// train's wait, whatever the verdict was. Measured on run 34660689077 (main,
/// 2026-09-12): the step took 11m39s on ubuntu and 6m13s on windows, because
/// coord persisted each 1,000-row chunk as a thousand autocommitted INSERTs
/// (3–120 s per chunk under load; 3 of 12 chunks hit the per-request budget
/// above). Twelve chunks at the per-request budget is 24 minutes of held job
/// per leg with NO bound at all.
///
/// So: sequential chunks, and a hard stop once this much wall time has been
/// spent, reported as a loud `::error` naming exactly how many rows were not
/// sent. Coord's batched insert (its own follow-up to qontinui-runner#1306)
/// takes a chunk to well under a second, so in the healthy case the whole
/// suite lands in seconds and this budget never binds; it exists for the day
/// coord is slow again, so that day costs the train three minutes, not
/// twenty-four.
const DEFAULT_BUDGET_MS = 180_000;

/// The smallest per-request timeout worth starting a chunk with. A chunk
/// whose remaining budget is below this is skipped outright rather than
/// started and aborted: an abort still costs the round trip and records
/// nothing, so it is strictly worse than the honest skip.
const MIN_CHUNK_TIMEOUT_MS = 5_000;

/// Resolve the whole-invocation budget from `COORD_INGEST_BUDGET_MS`, falling
/// back to [`DEFAULT_BUDGET_MS`] for an unset, non-numeric or non-positive
/// value — a misconfigured knob must degrade to the default bound, never to
/// "no bound", which is the state this constant exists to end.
export function resolveBudgetMs(raw) {
  const n = Number(raw);
  return Number.isFinite(n) && n > 0 ? Math.floor(n) : DEFAULT_BUDGET_MS;
}

/// The per-request timeout the NEXT chunk may use given `remainingMs` of the
/// invocation budget, or `null` when the chunk should not be started at all.
/// PURE — the loop below feeds it the clock, so the boundary is
/// table-testable.
///
/// Never more than [`REQUEST_TIMEOUT_MS`] (the per-request bound stands on its
/// own), never less than `floorMs` (see [`MIN_CHUNK_TIMEOUT_MS`]), `null` once
/// the budget cannot cover even that. The caller clamps `floorMs` to the
/// invocation budget, so a budget smaller than the floor still sends its
/// first chunk rather than nothing at all.
export function chunkTimeoutMs(
  remainingMs,
  requestTimeoutMs = REQUEST_TIMEOUT_MS,
  floorMs = MIN_CHUNK_TIMEOUT_MS,
) {
  if (!Number.isFinite(remainingMs) || remainingMs <= 0 || remainingMs < floorMs) return null;
  return Math.min(requestTimeoutMs, Math.floor(remainingMs));
}

/**
 * Build the `POST /coord/test-results/ingest` body from a parsed log. Pure —
 * no I/O — so this is what the unit tests exercise directly.
 *
 * @returns {{body: object|null, warning: string|null}} `body` is null when
 *   there is nothing worth sending (unparsed log, or zero named tests); the
 *   caller must surface `warning` rather than silently skip.
 */
export function buildIngestBody({ logText, repo, headSha, shard }) {
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
      })),
    },
    warning: null,
  };
}

function printUsage(stream) {
  stream.write(
    [
      "Usage: ci-test-results-ingest.mjs --log <path> --repo <owner/repo> --head-sha <sha>",
      "                                  [--shard <platform>]",
      "",
      "Best-effort: never fails the calling CI job. See file header.",
      "",
      "  --log <path>         Path to the captured `cargo test --verbose` output",
      "  --repo <owner/repo>  e.g. qontinui/qontinui-runner (must contain '/' —",
      "                       coord joins on the webhook's owner/name form)",
      "  --head-sha <sha>     Commit the results belong to",
      "  --shard <string>     Matrix leg, e.g. the platform (ubuntu-22.04) —",
      "                       closes the platform-attribution gap Phase 0 found",
      "  -h, --help           Print this help and exit 0",
      "",
    ].join("\n"),
  );
}

function warn(msg) {
  process.stdout.write(`::warning title=test-results-ingest::${msg}\n`);
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

async function postOneChunk(url, body, token, timeoutMs = REQUEST_TIMEOUT_MS) {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), timeoutMs);
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
            `(timeout ${timeoutMs}ms); these rows were NOT recorded`
        : `request to ${url} failed after ${ms}ms: ${err.message}`,
    );
    return { ok: false, ms, aborted };
  } finally {
    clearTimeout(timer);
  }
}

async function postResults(url, body, token, budgetMs = DEFAULT_BUDGET_MS) {
  const chunks = chunkResults(body.results, CHUNK_SIZE);
  let sent = 0;
  let failedChunks = 0;
  let skippedChunks = 0;
  let skippedRows = 0;
  let serverFailed = 0;
  const startedAt = Date.now();
  // The floor can never exceed the budget, or a small budget would send
  // nothing at all instead of its first chunk.
  const floorMs = Math.min(MIN_CHUNK_TIMEOUT_MS, budgetMs);

  for (const [i, results] of chunks.entries()) {
    const timeoutMs = chunkTimeoutMs(
      budgetMs - (Date.now() - startedAt),
      REQUEST_TIMEOUT_MS,
      floorMs,
    );
    if (timeoutMs === null) {
      skippedChunks += 1;
      skippedRows += results.length;
      continue;
    }
    // Every chunk repeats repo/head_sha/source — coord keys on those and
    // appends, so N posts for one head are a supported shape (its own coverage
    // producer does exactly this per module).
    const r = await postOneChunk(url, { ...body, results }, token, timeoutMs);
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
  const elapsed = Date.now() - startedAt;
  info(
    `POST ${url} (repo=${body.repo} head_sha=${body.head_sha}) — ` +
      `${sent}/${total} row(s) recorded across ${chunks.length} chunk(s) in ${elapsed}ms`,
  );
  const where =
    `${body.repo}@${body.head_sha}` +
    (body.results[0]?.shard ? ` (shard ${body.results[0].shard})` : "");
  if (skippedChunks > 0) {
    // Its own line, distinct from the failed-chunk one below: a chunk that
    // was never sent is a budget decision this script made, not a coord
    // failure, and the two want different fixes.
    error(
      `invocation budget of ${budgetMs}ms exhausted after ${elapsed}ms — ` +
        `${skippedChunks} of ${chunks.length} chunk(s) (${skippedRows} row(s)) were NOT sent ` +
        `for ${where}; raise COORD_INGEST_BUDGET_MS only if coord is known to be fast again`,
    );
  }
  if (failedChunks > 0) {
    error(
      `${failedChunks} of ${chunks.length} chunk(s) failed — ${total - sent - skippedRows} of ${total} ` +
        `row(s) were NOT recorded for ${where}`,
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

  const { body, warning } = buildIngestBody({ logText, repo, headSha, shard });
  if (warning) {
    warn(warning);
    return 0;
  }

  const token = process.env.COORD_INGEST_TOKEN;
  if (!token) {
    warn(
      `COORD_INGEST_TOKEN not set — ${body.results.length} result(s) parsed but not sent`,
    );
    return 0;
  }

  const base = (process.env.COORD_HTTP_URL || DEFAULT_COORD_URL).replace(/\/+$/, "");
  await postResults(
    base + INGEST_PATH,
    body,
    token,
    resolveBudgetMs(process.env.COORD_INGEST_BUDGET_MS),
  );
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

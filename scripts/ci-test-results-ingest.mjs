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
 * nextest revert). This script bounds its own network call internally
 * instead, so no step-level timeout is needed here.
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

async function postResults(url, body, token) {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), 60_000);
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
    info(
      `POST ${url} (repo=${body.repo} head_sha=${body.head_sha} results=${body.results.length}) -> HTTP ${res.status}`,
    );
    if (!res.ok) {
      warn(`non-2xx response from ${url}: ${text.slice(0, 500)}`);
      return;
    }
    try {
      const json = JSON.parse(text);
      if (typeof json.failed === "number" && json.failed > 0) {
        warn(
          `${json.failed} of ${json.parsed ?? body.results.length} row(s) failed to persist server-side`,
        );
      }
    } catch {
      // Non-JSON body — nothing further to check.
    }
  } catch (err) {
    warn(`request to ${url} failed: ${err.message}`);
  } finally {
    clearTimeout(timer);
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

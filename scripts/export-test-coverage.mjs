#!/usr/bin/env node
// @ts-check
/**
 * Per-test-FILE -> touched-file coverage producer for coord (qontinui-runner).
 *
 * Adapted from qontinui-web's proven producer. The only layout differences:
 *   - qontinui-runner's frontend lives at the REPO ROOT (not a frontend/ subdir),
 *     so the repo-relative test_id is "src/...test.tsx" (no "frontend/" prefix)
 *     and source attribution is rooted at "src/".
 *   - runner's vitest.config.ts pins `coverage.include` to
 *     src/lib/step-output-handlers/** for its own local report. That would blind
 *     this producer to every other touched source file, so we OVERRIDE it on the
 *     CLI with `--coverage.include=src/**` to recover v8's full touched-file set.
 *
 * vitest 4 + @vitest/coverage-v8 expose NO per-test-CASE coverage contexts
 * (unlike coverage.py's --cov-context=test on the backend), so the honest,
 * mechanically-exact granularity here is per-test-FILE. vitest's default
 * isolation is already one worker per test file, so running `vitest run
 * --coverage <one-file>` yields a coverage-final.json whose covered statements
 * are exactly the source files that ONE test file exercised. We loop over every
 * test file, attribute its covered source files to it, and emit one observation
 * per test file with `test_id` = the repo-relative test file path.
 *
 * Cadence: this is a main-only, NON-gating job. Per-file vitest startup is slow
 * (~5-15s each) and there are 60+ test files, so a full run is many minutes —
 * unacceptable for a PR gate but fine for a push-to-main producer whose output
 * coord caches per (repo, head_sha). PR lanes consume the map; they never run
 * this.
 *
 * test_id alignment (LOAD-BEARING):
 *   - Coverage observations are per-test-FILE: test_id = "src/...test.tsx".
 *   - coord derives a per-test-CASE test_id from JUnit XML as `{classname}::{name}`.
 *     vitest's JUnit reporter sets classname = the test FILE path (cwd-relative,
 *     e.g. "src/...test.tsx"). Because runner's frontend IS the repo root, that
 *     classname is ALREADY repo-relative — but we still run it through the
 *     normalizer (idempotent) so every per-case result test_id is
 *     `<coverage test_id>::<case name>`. The coverage test_id is therefore an
 *     exact prefix of the result test_ids from the same file — a clean documented
 *     join (results.test_id startsWith coverage.test_id) despite the deliberate
 *     file-vs-case granularity gap.
 *
 * Visibility (lesson from the web lanes): each POST's response body is printed
 * (truncated) and a WARNING is emitted whenever the server reports
 * `persisted < accepted`, so silent partial drops surface in the job log.
 *
 * Best-effort: any POST failure prints a warning and exits 0 — never fails CI.
 * node stdlib only.
 */

import { spawnSync } from "node:child_process";
import { readFileSync, existsSync, mkdtempSync, readdirSync, statSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, sep, posix } from "node:path";

const REPO = "qontinui-runner";
const DEFAULT_COORD_URL = "https://coord.qontinui.io";
const OBSERVE_PATH = "/coord/test-coverage/observe";
const RESULTS_PATH = "/coord/test-results/ingest";

const COORD_URL = (process.env.COORD_HTTP_URL || DEFAULT_COORD_URL).replace(/\/+$/, "");
const HEAD_SHA = process.env.GITHUB_SHA || "";

function warn(msg) {
  process.stderr.write(`[export-test-coverage] WARNING: ${msg}\n`);
}
function info(msg) {
  process.stdout.write(`[export-test-coverage] ${msg}\n`);
}

/** Parse simple CLI flags. */
function parseArgs(argv) {
  const args = { dryRun: false, limit: 0 };
  for (const a of argv) {
    if (a === "--dry-run") args.dryRun = true;
    else if (a.startsWith("--limit=")) args.limit = parseInt(a.slice("--limit=".length), 10) || 0;
  }
  // Env overrides (handy for CI matrix / local probing).
  if (process.env.COVERAGE_PRODUCER_LIMIT) {
    args.limit = parseInt(process.env.COVERAGE_PRODUCER_LIMIT, 10) || args.limit;
  }
  if (process.env.COVERAGE_PRODUCER_DRY_RUN === "1") args.dryRun = true;
  return args;
}

/**
 * Recursively enumerate test files under src/.
 * Mirrors vitest.config.ts include globs: src/**\/*.{test,spec}.{ts,tsx,...}.
 * Returns paths relative to the cwd (repo root), posix-separated, sorted.
 */
function enumerateTestFiles(root) {
  const out = [];
  const isTest = (name) => /\.(test|spec)\.(ts|tsx|js|jsx|mjs|cjs|mts|cts)$/.test(name);
  /** @param {string} dir */
  function walk(dir) {
    for (const entry of readdirSync(dir)) {
      const full = join(dir, entry);
      const st = statSync(full);
      if (st.isDirectory()) {
        if (entry === "node_modules") continue;
        walk(full);
      } else if (isTest(entry)) {
        out.push(full);
      }
    }
  }
  walk(root);
  // Normalize to cwd-relative posix paths (e.g. "src/foo/bar.test.tsx").
  const cwd = process.cwd();
  return out
    .map((p) => (p.startsWith(cwd) ? p.slice(cwd.length + 1) : p))
    .map((p) => p.split(sep).join(posix.sep))
    .sort();
}

/** "./src/foo/bar.test.tsx" or "src/foo/bar.test.tsx" -> "src/foo/bar.test.tsx". */
function toRepoRelTestId(relPath) {
  return relPath.split(sep).join(posix.sep).replace(/^\.\//, "");
}

/**
 * Normalize a coverage-final.json source path to a repo-relative forward-slash
 * path rooted at src/. Drops anything outside src/ (node_modules, configs, etc.)
 * and the test files themselves — only source attribution is signal.
 * @returns {string|null}
 */
function normalizeSourcePath(absOrRel) {
  const p = absOrRel.split("\\").join("/");
  const marker = "/src/";
  let rel;
  const idx = p.indexOf(marker);
  if (p.startsWith("src/")) {
    rel = p;
  } else if (idx !== -1) {
    rel = p.slice(idx + 1); // -> "src/..."
  } else {
    return null;
  }
  // Don't attribute a test file to itself.
  if (/\.(test|spec)\.(ts|tsx|js|jsx|mjs|cjs|mts|cts)$/.test(rel)) return null;
  if (!rel.startsWith("src/")) return null;
  return rel;
}

/**
 * Run vitest once for a single test file, collecting v8 coverage JSON + JUnit.
 * @returns {{ filesTouched: string[], junitXml: string|null }}
 */
function runOneFile(testFileRel, outDir) {
  const junitPath = join(outDir, "junit.xml");
  // vitest 4 CLI: --coverage enables, --coverage.* configure the v8 provider.
  // We OVERRIDE coverage.include (vitest.config.ts pins it to
  // src/lib/step-output-handlers/**) back to the full src tree so every touched
  // source file is attributed, not just the handlers.
  const cliArgs = [
    "vitest",
    "run",
    "--coverage",
    "--coverage.enabled=true",
    "--coverage.provider=v8",
    "--coverage.reporter=json",
    `--coverage.reportsDirectory=${outDir}`,
    "--coverage.include=src/**",
    "--reporter=junit",
    `--outputFile=${junitPath}`,
    testFileRel,
  ];
  const res = spawnSync("npx", cliArgs, {
    encoding: "utf-8",
    shell: process.platform === "win32",
    stdio: ["ignore", "pipe", "pipe"],
  });
  if (res.status !== 0) {
    // A failing/erroring test file still produces coverage+junit for what ran;
    // warn but proceed so one bad file doesn't sink the whole producer.
    warn(`vitest exited ${res.status} for ${testFileRel} (continuing)`);
  }

  /** @type {string[]} */
  let filesTouched = [];
  const covPath = join(outDir, "coverage-final.json");
  if (existsSync(covPath)) {
    try {
      const cov = JSON.parse(readFileSync(covPath, "utf-8"));
      const touched = new Set();
      for (const key of Object.keys(cov)) {
        const entry = cov[key];
        const s = entry.s || {};
        const coveredStmts = Object.values(s).filter((n) => n > 0).length;
        if (coveredStmts <= 0) continue;
        const norm = normalizeSourcePath(entry.path || key);
        if (norm) touched.add(norm);
      }
      filesTouched = [...touched].sort();
    } catch (e) {
      warn(`failed to parse coverage json for ${testFileRel}: ${e}`);
    }
  } else {
    warn(`no coverage-final.json produced for ${testFileRel}`);
  }

  let junitXml = null;
  if (existsSync(junitPath)) {
    junitXml = readFileSync(junitPath, "utf-8");
  }
  return { filesTouched, junitXml };
}

/**
 * Rewrite vitest JUnit classnames to the repo-relative test_id form so coord's
 * derived per-case test_id (`classname::name`) shares the exact prefix the
 * coverage observation uses. For runner the classname is already repo-root
 * relative ("src/...test.tsx"), so this is effectively idempotent — but we run
 * it for parity with the web producer and to strip any "./" prefix.
 */
function rewriteJunitClassnames(xml) {
  return xml.replace(/classname="([^"]*)"/g, (m, cn) => {
    const repoRel = toRepoRelTestId(cn);
    return `classname="${repoRel}"`;
  });
}

/** Truncate a response body for log printing. */
function truncate(s, n = 600) {
  if (s.length <= n) return s;
  return `${s.slice(0, n)}… (${s.length} bytes total)`;
}

/**
 * POST JSON; print the (truncated) response body and warn on persisted<accepted.
 * Throws on non-2xx so the caller's best-effort try/catch can swallow it.
 */
async function postJson(url, payload) {
  const res = await fetch(url, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(payload),
  });
  const body = await res.text().catch(() => "");
  info(`POST ${url} -> ${res.status} ${truncate(body)}`);
  if (!res.ok) {
    throw new Error(`non-2xx ${res.status} from ${url}`);
  }
  // Visibility: coord echoes how many records it accepted vs persisted. A gap
  // means some observations/results were silently dropped server-side.
  try {
    const json = JSON.parse(body);
    const accepted = json.accepted ?? json.received ?? null;
    const persisted = json.persisted ?? json.stored ?? json.inserted ?? null;
    if (accepted != null && persisted != null && persisted < accepted) {
      warn(`${url}: persisted ${persisted} < accepted ${accepted} — some records dropped server-side`);
    }
  } catch {
    // Non-JSON body (e.g. plain "ok") — nothing to compare.
  }
}

async function main() {
  const args = parseArgs(process.argv.slice(2));

  if (!HEAD_SHA && !args.dryRun) {
    warn("GITHUB_SHA not set; cannot key observations — exiting 0 (best-effort)");
    return 0;
  }
  const headSha = HEAD_SHA || "dryrun-sha";

  const srcRoot = join(process.cwd(), "src");
  if (!existsSync(srcRoot)) {
    warn(`src/ not found at ${srcRoot} (run from repo root) — exiting 0`);
    return 0;
  }

  let testFiles = enumerateTestFiles(srcRoot);
  info(`discovered ${testFiles.length} test files`);
  if (args.limit > 0) {
    testFiles = testFiles.slice(0, args.limit);
    info(`limiting to first ${testFiles.length} test files`);
  }

  /** @type {object[]} */
  const observations = [];
  /** @type {string[]} */
  const junitDocs = [];

  for (const testFileRel of testFiles) {
    const outDir = mkdtempSync(join(tmpdir(), "covprod-"));
    info(`running ${testFileRel}`);
    const { filesTouched, junitXml } = runOneFile(testFileRel, outDir);

    const testId = toRepoRelTestId(testFileRel);
    observations.push({
      repo: REPO,
      head_sha: headSha,
      test_id: testId,
      files_touched: filesTouched,
      // v8 reports per-file statement maps but we attribute at file granularity;
      // a single per-test line count isn't meaningful here, so null.
      lines_covered: null,
      coverage_kind: "line",
    });

    if (junitXml) {
      junitDocs.push(rewriteJunitClassnames(junitXml));
    }
  }

  const batch = { observations };

  if (args.dryRun) {
    info("--dry-run: printing batch JSON, skipping POSTs");
    process.stdout.write(JSON.stringify(batch, null, 2) + "\n");
    info(`(would also POST ${junitDocs.length} JUnit document(s) to ${RESULTS_PATH})`);
    return 0;
  }

  // Best-effort POSTs: a failure in one must not skip the other or fail the lane.
  try {
    if (observations.length > 0) {
      await postJson(COORD_URL + OBSERVE_PATH, batch);
    } else {
      info("no observations to post");
    }
  } catch (e) {
    warn(`coverage observe POST failed: ${e} — continuing (best-effort)`);
  }

  // Each per-file vitest run emits its own JUnit doc; POST each (coord parses
  // JUnit XML per call and upserts results keyed by repo/head_sha/test_id).
  for (const xml of junitDocs) {
    try {
      await postJson(COORD_URL + RESULTS_PATH, {
        repo: REPO,
        head_sha: headSha,
        source: "ci",
        format: "junit_xml",
        raw: xml,
      });
    } catch (e) {
      warn(`junit results POST failed: ${e} — continuing (best-effort)`);
    }
  }

  return 0;
}

main()
  .then((code) => process.exit(code))
  .catch((e) => {
    // Top-level guard: never fail the lane.
    warn(`unexpected error: ${e} — exiting 0 (best-effort)`);
    process.exit(0);
  });

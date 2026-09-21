#!/usr/bin/env node
// test-interleave-census.mjs — run the built test binaries N times, diff the
// failure sets, re-run every red ALONE, and label the arm.
//
// Phase 0 of plan `2026-09-17-runner-tests-share-in-process-mutable-state`
// (dossier `runner-tests-share-in-process-mutable-state`).
//
// WHY THIS EXISTS
//
//   `cargo test` runs every unit test of a binary as a thread in ONE process.
//   A test whose assertion depends on a process-global value that a concurrent
//   sibling also writes passes alone, passes under `--test-threads=1`, and
//   reds nondeterministically in the full suite — under an unstable name, with
//   an unstable count, and with a panic that names an assertion rather than the
//   shared resource. The distinguishing signal is a two-command test every
//   session has so far computed by hand: SOLO-PASS + SUITE-FAIL. This script
//   computes it, so the next red arrives labelled with its arm.
//
// THE LABELS
//
//   SUITE-ONLY           red ≥ 1 time in the suite, 0 solo failures — shares
//                        process state with a concurrent test. THE CLASS.
//   SOLO-RED             fails alone every time — a real defect or an ambient
//                        read; not this class.
//   BOTH-FLAKY           fails in both arms, nondeterministically — timing (the
//                        third dossier's class); not this class.
//   UNRESOLVED           no executable for the id (a doctest: rustdoc compiles
//                        one per run and `cargo test --no-run` builds none; an
//                        id with no `<binary>::` prefix; a binary the build did
//                        not list; a name the executable does not know).
//   UNRESOLVED (budget)  `--budget-seconds` ran out before this test's solo
//                        re-run — a non-answer, never a verdict.
//   UNPARSED             a re-run produced no recognisable libtest output.
//                        Fail-closed, exactly as `ci-flake-analyze.mjs` treats
//                        an unparsed log: an unparsed run is NEVER green.
//
// TWO MODES
//
//   Census (default) — for a local box or a scheduled job. Resolves the test
//   executables, runs each one N times with libtest's default thread count,
//   diffs the failure sets, re-runs every red alone K times, labels, and exits
//   0 (no SUITE-ONLY and NO NON-ANSWERS) / 1 (≥ 1 SUITE-ONLY) / 2 (a
//   non-answer — takes precedence, because a non-answer can hide either).
//   A non-answer is: an unparsed suite run or solo re-run; the budget
//   passing before a re-run (`UNRESOLVED (budget)`, or `budget_exhausted` in
//   the header); or an `UNRESOLVED` whose reason is a RESOLUTION FAILURE (no
//   built executable normalises to the id's prefix; no executable ran the
//   named test; the executable changed on disk). The two BY-DESIGN
//   `UNRESOLVED` reasons — a doctest, an id with no `<binary>::` prefix —
//   are not: nothing this script could have done would answer them, and
//   they are listed rather than gated. FAIL-CLOSED on purpose: until
//   2026-09-21 a nightly whose solo phase never ran (budget gone, every red
//   `UNRESOLVED (budget)`) exited 0 and printed "clean".
//   SOLO-RED and BOTH-FLAKY do not move the exit code: they are `cargo test`'s
//   own red, reported here for completeness, not the class this census gates.
//
//   `--classify-from-log <path>` — for the gating step on CI (Phase 1). No
//   repeat suite runs: takes the failed ids out of an existing
//   `cargo test --verbose` log, resolves the executables (a cache hit after the
//   gating step built them), re-runs each alone K times, prints one block per
//   test plus a GitHub annotation, and ALWAYS exits 0 — the gating verdict is
//   the earlier step's own exit code, and this is a post-failure diagnostic
//   (the same exit-0-always contract `scripts/ci-test-results-ingest.mjs`
//   documents). Bound it with `--budget-seconds`, never with a step
//   `timeout-minutes` beside `continue-on-error` — see ci.yml around the
//   ingest step for why that pairing cancels the whole job.
//
// HOW EXECUTABLES ARE RESOLVED
//
//   `cargo test --no-run --message-format=json`, run at the WORKSPACE root
//   (where the gating step runs), `executable` of every `compiler-artifact`
//   whose `profile.test` is true. Never a filtered `cargo test` — a filtered
//   `cargo test --bin` re-links and blew a 2-minute budget on 2026-09-13.
//   An id's `<binary>` prefix maps to an executable by the SAME rule the log
//   parser uses to mint it (`binaryIdFromExecutablePath` in
//   `ci-flake-analyze.mjs`: basename, `.exe` stripped, `-<hash>` stripped).
//   `--cargo "<cmd>"` routes the build through a wrapper (this fleet's
//   `cargo-guard.sh`); `--exe-list <file>` skips the build and reads one
//   executable per line (`<path>[<TAB><cwd>]`) so a caller can pre-resolve.
//
//   THE TREE UNDER TEST MUST STAY THE TREE UNDER TEST. On this fleet the
//   executables live in a target dir SHARED with concurrent sessions, and a
//   peer building a different tree into it replaces `<name>-<hash>` in place
//   — a census that kept running would then measure the peer's tree under
//   this one's sha. So every executable's sha256 is taken at resolution and
//   re-checked before each run; a changed file makes that run UNPARSED
//   ("executable changed on disk") rather than a verdict. `--snapshot-dir
//   <dir>` copies the executables aside first — into `<dir>/deps/`, because
//   the runner's ambient canary arms itself only when the executable's parent
//   directory is named `deps` (see `snapshotExecutables`) — and runs the
//   copies, which removes the race instead of detecting it.
//
// HOW A RUN IS PARSED
//
//   With `parseTestOutcomes` from `ci-flake-analyze.mjs` — the one parser this
//   repo has for libtest output, shared with the ingest and the escalation
//   rail — so ids are in the `<binary>::<test path>` grammar those consume.
//   libtest itself prints no `Running …/deps/<name>-<hash>` announcement when
//   invoked directly (cargo prints that), so this script synthesises exactly
//   that line, with the executable's REAL path, in front of the captured
//   output. Nothing else is added; the parser sees what `cargo test` would
//   have printed.
//
// USAGE
//   node scripts/test-interleave-census.mjs [--runs 5] [--solo-runs 3]
//        [--cargo cargo] [--exe-list <file>] [--budget-seconds N]
//        [--format pretty|json] [--out <path>]
//        [--run-timeout-seconds 1800] [--solo-timeout-seconds 300]
//        [--snapshot-dir <dir>]
//   node scripts/test-interleave-census.mjs --classify-from-log <log> …
//
// The json (`--format json`, or `--out <path>`) is the inventory artifact:
// header {tree_sha, hostname, runs, solo_runs, started_at, finished_at,
// executables[], unparsed[], …} and per test id {suite_runs, suite_failures,
// solo_runs, solo_failures, label, reason, sample_panic, executable}.

import { spawn as nodeSpawn, spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { copyFileSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { hostname as osHostname } from "node:os";
import { basename, dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { parseArgs as nodeParseArgs } from "node:util";

import {
  binaryIdFromExecutablePath,
  lastBinaryHasSummary,
  normalizeLogLine,
  parseTestOutcomes,
  redactSecrets,
} from "./ci-flake-analyze.mjs";

// ===========================================================================
// Labels
// ===========================================================================

export const LABEL = Object.freeze({
  SUITE_ONLY: "SUITE-ONLY",
  SOLO_RED: "SOLO-RED",
  BOTH_FLAKY: "BOTH-FLAKY",
  UNRESOLVED: "UNRESOLVED",
  UNRESOLVED_BUDGET: "UNRESOLVED (budget)",
  UNPARSED: "UNPARSED",
  /** Never red in the suite — not part of the inventory; kept so `labelFor` is total. */
  GREEN: "GREEN",
});

/** The one-line meaning printed beside each label. */
export const LABEL_SENTENCE = Object.freeze({
  [LABEL.SUITE_ONLY]: "shares process state with a concurrent test",
  [LABEL.SOLO_RED]:
    "fails alone — a real defect or an ambient read, not the shared-state class",
  [LABEL.BOTH_FLAKY]: "fails in both arms — timing, not the shared-state class",
  [LABEL.UNRESOLVED]: "no executable could be re-run for this id",
  [LABEL.UNRESOLVED_BUDGET]:
    "the solo re-run budget ran out before this test was re-run",
  [LABEL.UNPARSED]:
    "a re-run produced no recognisable libtest output — fail-closed, never green",
  [LABEL.GREEN]: "never red in the suite",
});

export const DOSSIER_SLUG = "runner-tests-share-in-process-mutable-state";

/**
 * The wire token for a label — what the ingest carries on a
 * `coord.test_results` row as `classification`, and what the escalator keys
 * the `suite-only` title/label on (Phase 1). Only the three VERDICTS have a
 * token; every non-answer (UNRESOLVED, UNRESOLVED (budget), UNPARSED, GREEN,
 * or a label this build does not know) is `null`, so a downstream reader can
 * never mistake "could not classify" for a class.
 */
export const CLASSIFICATION_TOKEN = Object.freeze({
  [LABEL.SUITE_ONLY]: "suite_only",
  [LABEL.SOLO_RED]: "solo_red",
  [LABEL.BOTH_FLAKY]: "both_flaky",
});

/** @returns {"suite_only"|"solo_red"|"both_flaky"|null} */
export function classificationTokenFor(label) {
  return CLASSIFICATION_TOKEN[label] ?? null;
}

/**
 * Test id → classification token out of a report this script wrote (either
 * mode: both carry `tests[<id>].label`). Tolerant of any shape — a missing
 * `tests`, a non-object, a record with no label — yielding an empty map
 * rather than throwing, because every consumer runs under an exit-0-always
 * or best-effort contract. Ids whose label has no token are OMITTED (their
 * classification is null, the same as an id the report never saw).
 *
 * @param {unknown} report
 * @returns {Map<string, "suite_only"|"solo_red"|"both_flaky">}
 */
export function classificationsFromReport(report) {
  const out = new Map();
  const tests = report && typeof report === "object" ? report.tests : null;
  if (!tests || typeof tests !== "object" || Array.isArray(tests)) return out;
  for (const [testId, rec] of Object.entries(tests)) {
    const token = classificationTokenFor(rec && typeof rec === "object" ? rec.label : undefined);
    if (token) out.set(testId, token);
  }
  return out;
}

// ===========================================================================
// Pure: executables
// ===========================================================================

/**
 * Executables out of `cargo test --no-run --message-format=json` output.
 * Non-JSON lines (a wrapper's own chatter) are skipped; only
 * `compiler-artifact` messages with `profile.test === true` and a non-null
 * `executable` count. An executable whose basename carries no cargo hash is
 * reported under `skipped` rather than dropped: the log parser could never
 * have minted an id for it, so nothing could resolve to it either.
 *
 * @param {string} text
 * @returns {{executables: Array<{executable: string, binaryId: string, cwd: string|null, targetName: string|null, kind: string|null}>, skipped: Array<{executable: string, reason: string}>}}
 */
export function parseCargoBuildMessages(text) {
  const executables = [];
  const skipped = [];
  for (const raw of String(text ?? "").split("\n")) {
    const line = raw.trim();
    if (!line.startsWith("{")) continue;
    let msg;
    try {
      msg = JSON.parse(line);
    } catch {
      continue;
    }
    if (msg?.reason !== "compiler-artifact") continue;
    if (msg.profile?.test !== true) continue;
    if (typeof msg.executable !== "string" || msg.executable.length === 0) continue;
    const binaryId = binaryIdFromExecutablePath(msg.executable);
    if (!binaryId) {
      skipped.push({
        executable: msg.executable,
        reason: "basename carries no cargo metadata hash, so no log id can name it",
      });
      continue;
    }
    executables.push({
      executable: msg.executable,
      binaryId,
      // cargo runs a test binary with cwd = the package root; tests that read
      // relative paths depend on it.
      cwd: typeof msg.manifest_path === "string" ? dirname(msg.manifest_path) : null,
      targetName: msg.target?.name ?? null,
      kind: Array.isArray(msg.target?.kind) ? msg.target.kind.join(",") : null,
    });
  }
  return { executables, skipped };
}

/**
 * Executables out of an `--exe-list` file: one per line, `<path>[<TAB><cwd>]`,
 * blank lines and `#` comments ignored.
 *
 * @param {string} text
 * @param {{defaultCwd?: string|null}} [opts]
 * @returns {ReturnType<typeof parseCargoBuildMessages>}
 */
export function parseExeList(text, { defaultCwd = null } = {}) {
  const executables = [];
  const skipped = [];
  for (const raw of String(text ?? "").split("\n")) {
    const line = raw.replace(/\r$/, "");
    if (line.trim() === "" || line.trimStart().startsWith("#")) continue;
    const [pathPart, cwdPart] = line.split("\t");
    const executable = pathPart.trim();
    const binaryId = binaryIdFromExecutablePath(executable);
    if (!binaryId) {
      skipped.push({
        executable,
        reason: "basename carries no cargo metadata hash, so no log id can name it",
      });
      continue;
    }
    executables.push({
      executable,
      binaryId,
      cwd: cwdPart && cwdPart.trim() !== "" ? cwdPart.trim() : defaultCwd,
      targetName: null,
      kind: null,
    });
  }
  return { executables, skipped };
}

/**
 * Index executables by `<binary>` id. First one wins; a second executable
 * normalising to the same id is recorded as a collision so a resolution
 * against it is visibly ambiguous rather than silently wrong.
 *
 * @param {Array<{executable: string, binaryId: string, cwd: string|null}>} executables
 * @returns {{byId: Map<string, {executable: string, binaryId: string, cwd: string|null}>, collisions: Array<{binaryId: string, executables: string[]}>}}
 */
export function buildExecutableIndex(executables) {
  const byId = new Map();
  const allById = new Map();
  const collisionMap = new Map();
  for (const e of executables) {
    if (!allById.has(e.binaryId)) allById.set(e.binaryId, []);
    allById.get(e.binaryId).push(e);
    if (!byId.has(e.binaryId)) {
      byId.set(e.binaryId, e);
      continue;
    }
    if (!collisionMap.has(e.binaryId)) {
      collisionMap.set(e.binaryId, [byId.get(e.binaryId).executable]);
    }
    collisionMap.get(e.binaryId).push(e.executable);
  }
  const collisions = [...collisionMap.entries()].map(([binaryId, executables]) => ({
    binaryId,
    executables,
  }));
  return { byId, allById, collisions };
}

/**
 * Split a `<binary>::<test path>` id at its FIRST `::`. A binary id never
 * contains `::` (it is a file basename), a test path routinely does.
 *
 * @param {string} testId
 * @returns {{binary: string|null, name: string}}
 */
export function splitTestId(testId) {
  const id = String(testId ?? "");
  const at = id.indexOf("::");
  if (at <= 0) return { binary: null, name: id };
  return { binary: id.slice(0, at), name: id.slice(at + 2) };
}

/**
 * A doctest's name has the shape `<path>.rs - <item> (line N)`. rustdoc
 * compiles and runs each one itself; `cargo test --no-run` builds no
 * executable for it, so it can never be re-run alone through this path.
 *
 * @param {string} name the part after `<binary>::`
 * @returns {boolean}
 */
export function isDoctestName(name) {
  return /\(line \d+\)$/.test(String(name ?? "")) && / - /.test(String(name ?? ""));
}

/**
 * libtest prints a `#[should_panic]` test as `test <name> - should panic ... ok`,
 * and the shared parser keeps that suffix in the id (it is part of what the
 * ingest rail stores). `--exact` wants the bare name.
 *
 * @param {string} name
 * @returns {string}
 */
export function exactNameFor(name) {
  return String(name ?? "").replace(/ - should panic$/, "");
}

/**
 * Map a test id to the executable that can re-run it, or say why none can.
 * `name` is what `--exact` takes; the id itself is left as the parser minted it.
 *
 * @param {string} testId
 * @param {ReturnType<typeof buildExecutableIndex>} index
 * `alternates` lists the OTHER executables that normalise to the same
 * `<binary>` id (this workspace has two `qontinui_specs`: an integration test
 * and a bin — the id grammar cannot tell them apart, so the re-run tries each
 * in turn until one actually runs the named test).
 *
 * @returns {{executable: string|null, cwd: string|null, name: string, unresolvedReason: string|null, alternates: Array<{executable: string, cwd: string|null}>}}
 */
export function resolveTestId(testId, index) {
  const { binary, name } = splitTestId(testId);
  if (binary === null) {
    return {
      executable: null,
      cwd: null,
      name,
      alternates: [],
      unresolvedReason:
        "id carries no `<binary>::` prefix (the run's announcement line was missing), so no executable can be named for it",
    };
  }
  if (isDoctestName(name)) {
    return {
      executable: null,
      cwd: null,
      name,
      alternates: [],
      unresolvedReason:
        `doctest — rustdoc compiles it per run; \`cargo test --no-run\` builds no executable for \`Doc-tests ${binary}\``,
    };
  }
  const entry = index.byId.get(binary);
  if (!entry) {
    return {
      executable: null,
      cwd: null,
      name,
      alternates: [],
      unresolvedReason: `no built test executable normalises to \`${binary}\` (resolution failed)`,
    };
  }
  const alternates = (index.allById?.get(binary) ?? [])
    .filter((e) => e.executable !== entry.executable)
    .map((e) => ({ executable: e.executable, cwd: e.cwd ?? null }));
  return { executable: entry.executable, cwd: entry.cwd ?? null, name: exactNameFor(name), unresolvedReason: null, alternates };
}

// ===========================================================================
// Pure: parsing one executable's output
// ===========================================================================

/**
 * What cargo prints before launching a test binary. libtest does not print it
 * itself, so a direct invocation has to supply it for the shared parser to
 * mint the `<binary>::` prefix. The REAL path is used, so the id is minted by
 * the same normaliser cargo's own line would have gone through.
 *
 * @param {string} executable
 * @returns {string}
 */
export function synthesiseAnnouncement(executable) {
  return `     Running \`${executable}\``;
}

/**
 * Parse one direct run of a test executable with the shared parser.
 *
 * @param {string} executable
 * @param {string} outputText stdout + stderr of the run
 * @returns {ReturnType<typeof parseTestOutcomes>}
 */
export function parseExecutableOutput(executable, outputText) {
  return parseTestOutcomes(`${synthesiseAnnouncement(executable)}\n${outputText ?? ""}`);
}

/** Longest `sample_panic` kept, in characters. */
export const PANIC_TEXT_CAP = 800;

/**
 * The captured `---- <name> stdout ----` block of a failed test: the panic
 * line and whatever the assertion printed, up to the next block or the
 * `failures:` list. Timestamps (CI logs) and ANSI are stripped; the
 * `RUST_BACKTRACE` hint is dropped; the result is capped.
 *
 * @param {string} outputText
 * @param {string} name the test path as libtest prints it (after `<binary>::`)
 * @returns {string|null}
 */
export function extractPanicText(outputText, name) {
  const lines = String(outputText ?? "").split("\n").map(normalizeLogLine);
  const header = `---- ${name} stdout ----`;
  const start = lines.findIndex((l) => l.trim() === header);
  if (start < 0) return null;
  const body = [];
  for (let i = start + 1; i < lines.length; i += 1) {
    const t = lines[i].trim();
    if (/^---- .+ ----$/.test(t)) break;
    if (t === "failures:") break;
    if (t.startsWith("note: run with `RUST_BACKTRACE=1`")) continue;
    body.push(lines[i].trimEnd());
  }
  while (body.length > 0 && body[body.length - 1].trim() === "") body.pop();
  while (body.length > 0 && body[0].trim() === "") body.shift();
  if (body.length === 0) return null;
  // Redacted at CAPTURE, so every consumer of `sample_panic` — the json, the
  // pretty block, the Checks annotation, the escalator's issue body — sees the
  // same text and none can leak what the others hid.
  const text = redactSecrets(body.join("\n"));
  return text.length > PANIC_TEXT_CAP ? `${text.slice(0, PANIC_TEXT_CAP)}…` : text;
}

// ===========================================================================
// Pure: diff and label
// ===========================================================================

/**
 * Failure-set diff across runs. Each run is a Map<testId, outcome> (the
 * shared parser's `tests` folded), or null for a run that did not parse.
 *
 * @param {Array<Map<string, "pass"|"fail"|"skip">|null>} runs
 * @returns {{perTest: Map<string, {seen: number, failures: number, failedInRuns: number[]}>, intermittent: string[], always: string[], everRed: string[]}}
 *   `intermittent`: red in some parsed runs and green in others — THE
 *   signature. `always`: red in every parsed run it appeared in. `everRed`:
 *   both, sorted — the solo re-run candidates.
 */
export function diffFailureSets(runs) {
  const perTest = new Map();
  runs.forEach((run, idx) => {
    if (!run) return;
    for (const [testId, outcome] of run) {
      let rec = perTest.get(testId);
      if (!rec) {
        rec = { seen: 0, failures: 0, failedInRuns: [] };
        perTest.set(testId, rec);
      }
      rec.seen += 1;
      if (outcome === "fail") {
        rec.failures += 1;
        rec.failedInRuns.push(idx + 1);
      }
    }
  });
  const intermittent = [];
  const always = [];
  for (const [testId, rec] of perTest) {
    if (rec.failures === 0) continue;
    if (rec.failures < rec.seen) intermittent.push(testId);
    else always.push(testId);
  }
  intermittent.sort();
  always.sort();
  return { perTest, intermittent, always, everRed: [...intermittent, ...always].sort() };
}

/**
 * The label matrix. Total over its inputs; every arm names its reason.
 *
 * @param {{suiteRuns: number, suiteFailures: number, soloRuns: number, soloFailures: number, soloUnparsed?: number, unresolvedReason?: string|null, budgetExhausted?: boolean}} rec
 * @returns {{label: string, reason: string}}
 */
export function labelFor({
  suiteRuns,
  suiteFailures,
  soloRuns,
  soloFailures,
  soloUnparsed = 0,
  unresolvedReason = null,
  budgetExhausted = false,
}) {
  if (suiteFailures === 0) {
    return { label: LABEL.GREEN, reason: `green in ${suiteRuns}/${suiteRuns} suite run(s)` };
  }
  if (unresolvedReason) return { label: LABEL.UNRESOLVED, reason: unresolvedReason };
  if (budgetExhausted) {
    return {
      label: LABEL.UNRESOLVED_BUDGET,
      reason: `red ${suiteFailures}/${suiteRuns} in the suite; the --budget-seconds deadline passed before a solo re-run was made`,
    };
  }
  if (soloUnparsed > 0) {
    return {
      label: LABEL.UNPARSED,
      reason: `${soloUnparsed} of ${soloRuns} solo re-run(s) produced no recognisable libtest output — fail-closed, never read as green`,
    };
  }
  if (soloRuns === 0) {
    return {
      label: LABEL.UNRESOLVED,
      reason: `red ${suiteFailures}/${suiteRuns} in the suite; no solo re-run was made`,
    };
  }
  if (soloFailures === 0) {
    return {
      label: LABEL.SUITE_ONLY,
      reason: `red ${suiteFailures}/${suiteRuns} in the suite, green ${soloRuns}/${soloRuns} alone`,
    };
  }
  if (soloFailures === soloRuns) {
    return {
      label: LABEL.SOLO_RED,
      reason: `red ${suiteFailures}/${suiteRuns} in the suite, red ${soloFailures}/${soloRuns} alone`,
    };
  }
  return {
    label: LABEL.BOTH_FLAKY,
    reason: `red ${suiteFailures}/${suiteRuns} in the suite, red ${soloFailures}/${soloRuns} alone`,
  };
}

/**
 * The two `UNRESOLVED` reasons that are non-answers BY DESIGN — nothing this
 * script could have done would produce a solo re-run for them — as opposed to
 * a resolution FAILURE (no executable normalises to the prefix, no executable
 * ran the name, the executable changed on disk), which means the census did
 * not measure something it was supposed to. `resolveTestId` mints both.
 *
 * @param {string|null|undefined} reason
 * @returns {boolean}
 */
export function isByDesignNonAnswer(reason) {
  const r = String(reason ?? "");
  return r.startsWith("doctest — ") || r.startsWith("id carries no `<binary>::` prefix");
}

/**
 * Census-mode exit code. 2 takes precedence over 1 (SUITE-ONLY), because a
 * non-answer can hide either answer. FAIL-CLOSED: 2 on any unparsed run or
 * re-run, on the budget passing before a re-run (`UNRESOLVED (budget)`, or
 * `header.budget_exhausted` — the header flag covers a budget that ran out
 * with no candidate left to label), and on any `UNRESOLVED` whose reason is
 * a resolution failure. The by-design non-answers (`isByDesignNonAnswer`)
 * are listed, not gated.
 *
 * @param {{header: {unparsed: unknown[], budget_exhausted?: boolean}, tests: Record<string, {label: string, reason?: string}>}} report
 * @returns {0|1|2}
 */
export function exitCodeFor(report) {
  const recs = Object.values(report.tests);
  const labels = recs.map((t) => t.label);
  if (report.header.unparsed.length > 0 || labels.includes(LABEL.UNPARSED)) return 2;
  if (report.header.budget_exhausted === true || labels.includes(LABEL.UNRESOLVED_BUDGET)) return 2;
  if (recs.some((t) => t.label === LABEL.UNRESOLVED && !isByDesignNonAnswer(t.reason))) return 2;
  if (labels.includes(LABEL.SUITE_ONLY)) return 1;
  return 0;
}

/**
 * The non-answers that moved the exit code to 2, named — so the pretty
 * summary and the log say WHY the verdict is UNKNOWN rather than only that.
 * Pure over a built report.
 *
 * @param {ReturnType<typeof buildReport>} report
 * @returns {string[]}
 */
export function nonAnswersFor(report) {
  const out = [];
  const h = report.header;
  if (h.unparsed.length > 0) out.push(`${h.unparsed.length} unparsed suite run(s)`);
  const recs = Object.entries(report.tests);
  const count = (pred) => recs.filter(([, t]) => pred(t)).length;
  const unparsedSolo = count((t) => t.label === LABEL.UNPARSED);
  if (unparsedSolo) out.push(`${unparsedSolo} test(s) with an unparsed solo re-run`);
  const budget = count((t) => t.label === LABEL.UNRESOLVED_BUDGET);
  if (budget) out.push(`${budget} test(s) never re-run — budget passed`);
  else if (h.budget_exhausted === true) out.push("the budget passed before the solo phase finished");
  const failed = count((t) => t.label === LABEL.UNRESOLVED && !isByDesignNonAnswer(t.reason));
  if (failed) out.push(`${failed} test(s) whose executable could not be resolved`);
  return out;
}

// ===========================================================================
// Pure: output
// ===========================================================================

/**
 * GitHub workflow-command escaping for an annotation MESSAGE (`%`, CR, LF).
 * @param {string} s
 * @returns {string}
 */
export function escapeAnnotationMessage(s) {
  return String(s ?? "").replace(/%/g, "%25").replace(/\r/g, "%0D").replace(/\n/g, "%0A");
}

/**
 * One GitHub annotation per classified test. SUITE-ONLY is an `::error` (the
 * class this exists to name); SOLO-RED / BOTH-FLAKY are `::warning`s (the
 * gating step already reported them red); the non-answers are `::notice`s.
 * The sample panic rides in the body so the label never replaces the
 * assertion text a session still has to read.
 *
 * @param {string} testId
 * @param {{label: string, solo_runs: number, solo_failures: number, reason: string, sample_panic: string|null}} rec
 * @returns {string}
 */
export function formatAnnotation(testId, rec) {
  const K = rec.solo_runs;
  const panic = rec.sample_panic ? `; panic: ${redactSecrets(rec.sample_panic)}` : "";
  let level;
  let body;
  switch (rec.label) {
    case LABEL.SUITE_ONLY:
      level = "error";
      body = `${testId} shares process state with a concurrent test — passes alone ${K - rec.solo_failures}/${K}; see dossier ${DOSSIER_SLUG}${panic}`;
      break;
    case LABEL.SOLO_RED:
      level = "warning";
      body = `${testId} fails alone ${rec.solo_failures}/${K} — a real defect or an ambient read, not the shared-state class${panic}`;
      break;
    case LABEL.BOTH_FLAKY:
      level = "warning";
      body = `${testId} fails in both arms (alone ${rec.solo_failures}/${K}) — timing, not the shared-state class${panic}`;
      break;
    default:
      level = "notice";
      body = `${testId} ${rec.label}: ${rec.reason}${panic}`;
  }
  return `::${level} title=${rec.label}::${escapeAnnotationMessage(body)}`;
}

/**
 * One block per test, for the pretty format and for the classify-mode log.
 * @param {string} testId
 * @param {{label: string, reason: string, suite_runs: number, suite_failures: number, solo_runs: number, solo_failures: number, executable: string|null, sample_panic: string|null}} rec
 * @returns {string}
 */
export function formatTestBlock(testId, rec) {
  const lines = [
    `${rec.label} — ${LABEL_SENTENCE[rec.label] ?? ""}`,
    `  test        ${testId}`,
    `  suite       red ${rec.suite_failures}/${rec.suite_runs}${rec.failed_in_runs?.length ? ` (runs ${rec.failed_in_runs.join(",")})` : ""}`,
    `  alone       red ${rec.solo_failures}/${rec.solo_runs}`,
    `  reason      ${rec.reason}`,
    `  executable  ${rec.executable ?? "<none>"}`,
  ];
  if (rec.sample_panic) {
    lines.push("  panic:");
    for (const l of rec.sample_panic.split("\n")) lines.push(`    ${l}`);
  }
  return lines.join("\n");
}

/**
 * @param {ReturnType<typeof buildReport>} report
 * @returns {string}
 */
export function formatPretty(report) {
  const h = report.header;
  const out = [];
  out.push(`test-interleave-census — mode ${h.mode}`);
  out.push(`  tree ${h.tree_sha ?? "<unknown>"}  box ${h.hostname}  runs ${h.runs}  solo-runs ${h.solo_runs}`);
  out.push(`  started ${h.started_at}  finished ${h.finished_at}`);
  out.push(`  executables ${h.executables.length}${h.skipped_executables.length ? `  (skipped ${h.skipped_executables.length}: ${h.skipped_executables.map((s) => s.executable).join(", ")})` : ""}`);
  if (h.collisions.length) {
    out.push(`  COLLISIONS (two executables, one id): ${h.collisions.map((c) => `${c.binaryId} -> ${c.executables.join(" | ")}`).join("; ")}`);
  }
  if (h.budget_seconds != null) out.push(`  budget ${h.budget_seconds}s${h.budget_exhausted ? " — EXHAUSTED" : ""}`);
  if (h.unparsed.length) {
    out.push("");
    out.push(`UNPARSED suite runs (fail-closed — never read as green): ${h.unparsed.length}`);
    for (const u of h.unparsed) out.push(`  run ${u.run}  ${u.executable}  ${u.reason}`);
  }
  out.push("");
  const ids = Object.keys(report.tests).sort();
  if (ids.length === 0) {
    out.push("No test was red in any suite run.");
  } else {
    out.push(`Tests red at least once: ${ids.length}`);
    out.push("");
    for (const id of ids) {
      out.push(formatTestBlock(id, report.tests[id]));
      out.push("");
    }
  }
  out.push("Summary by label:");
  for (const [label, n] of Object.entries(report.summary)) out.push(`  ${label.padEnd(20)} ${n}`);
  const nonAnswers = h.mode === "classify-from-log" ? [] : nonAnswersFor(report);
  if (nonAnswers.length > 0) {
    out.push(`NON-ANSWERS (fail-closed — the verdict is UNKNOWN, not clean): ${nonAnswers.join("; ")}`);
  }
  out.push(`exit ${report.exit_code}`);
  return out.join("\n");
}

// ===========================================================================
// Spawn layer (injectable)
// ===========================================================================

/**
 * Kill a timed-out test binary AND everything it spawned. `child.kill` alone
 * reaches the direct child: the runner's `process_helpers` / terminal tests
 * spawn real grandchildren (a mock CLI, a shell), and one left behind after
 * its parent's SIGKILL pins a hosted runner until the job's own bound trips
 * — which is the very bound the census budget exists to stay under.
 *
 * POSIX: the child was spawned `detached: true`, so it leads its own process
 * group and `kill(-pid, SIGKILL)` takes the whole tree. Windows: no groups —
 * `taskkill /PID <pid> /T /F` walks the tree instead. Either way it is the
 * TEST BINARY's own tree, never a session or the runner: this script only
 * ever spawns the executables it resolved.
 *
 * Injectable (`platform`, `kill`, `taskkill`) so the POSIX and Windows arms
 * are pinned by tests without a real process; falls back to `child.kill`
 * when the group kill is refused (the child already gone, or never detached).
 *
 * @param {{pid?: number, kill: (signal: string) => boolean}} child
 * @param {{platform?: string, kill?: (pid: number, signal: string) => void, taskkill?: (cmd: string, args: string[], opts: object) => unknown}} [io]
 * @returns {"group"|"tree"|"child"|"none"} which arm actually ran
 */
export function killTree(child, { platform = process.platform, kill = process.kill, taskkill = spawnSync } = {}) {
  const pid = child?.pid;
  if (!pid) return "none";
  if (platform === "win32") {
    try {
      taskkill("taskkill", ["/PID", String(pid), "/T", "/F"], { stdio: "ignore", windowsHide: true });
      return "tree";
    } catch {
      /* fall through to the direct kill */
    }
  } else {
    try {
      kill(-pid, "SIGKILL");
      return "group";
    } catch {
      /* not a group leader, or already gone — fall through */
    }
  }
  try {
    child.kill("SIGKILL");
    return "child";
  } catch {
    return "none";
  }
}

/**
 * Run one executable to completion, capturing both streams. The ONLY place
 * this script touches a child process; `runCensus` / `classifyFromLog` take
 * it as `spawn` so the tests substitute a stub.
 *
 * @param {string} executable
 * @param {string[]} args
 * @param {{cwd?: string|null, env?: NodeJS.ProcessEnv, timeoutMs?: number}} [opts]
 * @returns {Promise<{stdout: string, stderr: string, code: number|null, signal: string|null, timedOut: boolean, spawnError: string|null}>}
 */
export function defaultSpawn(executable, args, { cwd = null, env = process.env, timeoutMs = 0 } = {}) {
  return new Promise((resolvePromise) => {
    let child;
    let stdout = "";
    let stderr = "";
    let timedOut = false;
    let settled = false;
    const settle = (r) => {
      if (settled) return;
      settled = true;
      resolvePromise(r);
    };
    try {
      child = nodeSpawn(executable, args, {
        cwd: cwd ?? undefined,
        env,
        stdio: ["ignore", "pipe", "pipe"],
        windowsHide: true,
        // POSIX: own process group, so a timeout can kill the binary's whole
        // tree (`killTree`). Not on Windows, where `detached` means a new
        // console rather than a group and `taskkill /T` does the walking.
        detached: process.platform !== "win32",
      });
    } catch (e) {
      settle({ stdout, stderr, code: null, signal: null, timedOut, spawnError: String(e?.message ?? e) });
      return;
    }
    const timer =
      timeoutMs > 0
        ? setTimeout(() => {
            timedOut = true;
            killTree(child);
          }, timeoutMs)
        : null;
    // `setEncoding` so a multi-byte UTF-8 sequence split across two chunks
    // decodes as one character; `stdout += <Buffer>` stringifies each chunk
    // on its own and turns the split into U+FFFD — inside a panic message
    // that is then compared and filed.
    child.stdout.setEncoding("utf8");
    child.stderr.setEncoding("utf8");
    child.stdout.on("data", (d) => {
      stdout += d;
    });
    child.stderr.on("data", (d) => {
      stderr += d;
    });
    child.on("error", (e) => {
      if (timer) clearTimeout(timer);
      settle({ stdout, stderr, code: null, signal: null, timedOut, spawnError: String(e?.message ?? e) });
    });
    child.on("close", (code, signal) => {
      if (timer) clearTimeout(timer);
      settle({ stdout, stderr, code, signal, timedOut, spawnError: null });
    });
  });
}

/**
 * The env a test binary runs under. cargo sets `CARGO_MANIFEST_DIR` at run
 * time too; a test that reads it at run time (rare, but real) gets the same
 * answer here.
 */
function envForExecutable(entry, baseEnv) {
  return entry.cwd ? { ...baseEnv, CARGO_MANIFEST_DIR: entry.cwd } : { ...baseEnv };
}

/**
 * sha256 of a file — the default `fingerprint` the runner layer takes.
 * @param {string} path
 * @returns {string}
 */
export function sha256File(path) {
  return createHash("sha256").update(readFileSync(path)).digest("hex");
}

/**
 * Copy each executable into `<dir>/deps/` (basename preserved, so the
 * `<binary>` id the parser mints from the announcement is unchanged) and
 * return entries pointing at the copies, with the original kept as `source`.
 * The default `copy` is `copyFileSync`; injectable for the tests.
 *
 * THE `deps/` LEAF IS LOAD-BEARING, NOT COSMETIC. The runner's ambient canary
 * (`ambient.rs` `canary_armed()`) decides at run time whether a process is a
 * cargo test binary by asking whether its executable's parent directory is
 * literally named `deps` — the bin crate is linked against the rlib without
 * `cfg(test)`, so that heuristic is the ONLY thing arming the canary for every
 * test under `main.rs`. Measured 2026-09-21: a snapshot into a flat directory
 * silently disarmed it, four canary self-tests went red 10/10 in both arms
 * (a false SOLO-RED), and every bin-crate ambient read ran against the real
 * machine for the whole census.
 *
 * @param {Array<{executable: string, binaryId: string, cwd: string|null}>} executables
 * @param {string} dir
 * @param {{copy?: (from: string, to: string) => void, mkdir?: (dir: string) => void}} [io]
 * @returns {Array<{executable: string, source: string, binaryId: string, cwd: string|null}>}
 */
export function snapshotExecutables(executables, dir, { copy = copyFileSync, mkdir = (d) => mkdirSync(d, { recursive: true }) } = {}) {
  const depsDir = join(dir, "deps");
  mkdir(depsDir);
  return executables.map((e) => {
    const target = join(depsDir, basename(e.executable));
    copy(e.executable, target);
    return { ...e, executable: target, source: e.executable };
  });
}

/**
 * Fold one direct run of an executable into either a Map of outcomes or an
 * `unparsed` reason. Fail-closed on every non-answer: a timeout, a spawn
 * error, output the shared parser does not recognise, a run whose output
 * ends WITHOUT libtest's `test result:` summary line (the shape of an abort
 * mid-run — with or without failures already printed — whose remaining tests
 * were never judged; `lastBinaryHasSummary`), or a process that died
 * (non-zero exit / a signal) without a single parsed failure.
 *
 * @param {string} executable
 * @param {{stdout: string, stderr: string, code: number|null, signal: string|null, timedOut: boolean, spawnError: string|null}} result
 * @returns {{outcomes: Map<string, "pass"|"fail"|"skip">|null, reason: string|null, text: string}}
 */
export function foldRunResult(executable, result) {
  const text = `${result.stdout ?? ""}\n${result.stderr ?? ""}`;
  if (result.spawnError) return { outcomes: null, reason: `spawn failed: ${result.spawnError}`, text };
  if (result.timedOut) return { outcomes: null, reason: "timed out and was killed", text };
  const parsed = parseExecutableOutput(executable, text);
  if (parsed.unparsed) return { outcomes: null, reason: parsed.reason ?? "unparsed", text };
  if (!lastBinaryHasSummary(text.split("\n").map(normalizeLogLine))) {
    const how = result.signal ? `signal ${result.signal}` : `exit ${result.code}`;
    return {
      outcomes: null,
      reason: `no test-result summary — the run died mid-way (${how}); the tests after the last printed outcome were never judged`,
      text,
    };
  }
  const outcomes = new Map(parsed.tests.map((t) => [t.testId, t.outcome]));
  const failures = [...outcomes.values()].filter((o) => o === "fail").length;
  if (result.code !== 0 && failures === 0) {
    const how = result.signal ? `signal ${result.signal}` : `exit ${result.code}`;
    return {
      outcomes: null,
      reason: `process ended with ${how} and no failing test parsed — aborted mid-run, remaining tests never judged`,
      text,
    };
  }
  return { outcomes, reason: null, text };
}

// ===========================================================================
// Orchestration
// ===========================================================================

function nowIso(now) {
  return new Date(now()).toISOString();
}

/**
 * Build the report object from accumulated state (pure).
 */
export function buildReport({ header, tests }) {
  const summary = {};
  for (const label of Object.values(LABEL)) summary[label] = 0;
  delete summary[LABEL.GREEN];
  for (const rec of Object.values(tests)) summary[rec.label] = (summary[rec.label] ?? 0) + 1;
  const report = { header, tests, summary, exit_code: 0 };
  report.exit_code = header.mode === "classify-from-log" ? 0 : exitCodeFor(report);
  return report;
}

/**
 * Re-run each candidate alone K times, honouring the deadline. Returns the
 * per-test records (label included).
 *
 * @param {object} p
 * @param {Array<{testId: string, suiteRuns: number, suiteFailures: number, failedInRuns: number[], samplePanic: string|null}>} p.candidates
 * @param {ReturnType<typeof buildExecutableIndex>} p.index
 * @param {number} p.soloRuns
 * @param {typeof defaultSpawn} p.spawn
 * @param {number|null} p.deadlineMs epoch ms after which remaining re-runs are UNRESOLVED (budget)
 * @param {() => number} p.now
 * @param {number} p.timeoutMs
 * @param {(msg: string) => void} p.log
 * @param {NodeJS.ProcessEnv} p.env
 * @returns {Promise<{tests: Record<string, object>, budgetExhausted: boolean}>}
 */
export async function soloRerunAll({ candidates, index, soloRuns, spawn, deadlineMs, now, timeoutMs, log, env, digests = null, fingerprint = null }) {
  const tests = {};
  let budgetExhausted = false;
  const driftReason = (executable) => {
    if (!digests || !fingerprint || !digests.has(executable)) return null;
    const expected = digests.get(executable);
    let current;
    try {
      current = fingerprint(executable);
    } catch (err) {
      current = `unreadable: ${err?.message ?? err}`;
    }
    if (expected !== null && current === expected) return null;
    return `executable changed on disk since resolution (sha256 ${expected ?? "unreadable"} -> ${current}) — a concurrent build replaced it; a solo re-run would measure a different tree`;
  };
  for (const c of candidates) {
    const resolved = resolveTestId(c.testId, index);
    const rec = {
      suite_runs: c.suiteRuns,
      suite_failures: c.suiteFailures,
      failed_in_runs: c.failedInRuns,
      solo_runs: 0,
      solo_failures: 0,
      solo_unparsed: 0,
      label: null,
      reason: null,
      sample_panic: c.samplePanic ?? null,
      executable: resolved.executable,
    };
    if (resolved.unresolvedReason) {
      Object.assign(rec, labelFor({ suiteRuns: rec.suite_runs, suiteFailures: rec.suite_failures, soloRuns: 0, soloFailures: 0, unresolvedReason: resolved.unresolvedReason }));
      tests[c.testId] = rec;
      log(`solo  ${c.testId}: ${rec.label} — ${rec.reason}`);
      continue;
    }
    if (deadlineMs !== null && now() >= deadlineMs) {
      budgetExhausted = true;
      Object.assign(rec, labelFor({ suiteRuns: rec.suite_runs, suiteFailures: rec.suite_failures, soloRuns: 0, soloFailures: 0, budgetExhausted: true }));
      tests[c.testId] = rec;
      log(`solo  ${c.testId}: ${rec.label}`);
      continue;
    }
    let unresolvedReason = null;
    // The executable in hand plus the alternates sharing its id; a "0 tests
    // ran" answer moves to the next one, so a colliding id is resolved by
    // asking the binaries rather than guessed.
    const queue = [{ executable: resolved.executable, cwd: resolved.cwd }, ...resolved.alternates];
    let current = queue.shift();
    const triedNoSuchTest = [];
    for (let k = 1; k <= soloRuns; k += 1) {
      if (deadlineMs !== null && now() >= deadlineMs) {
        budgetExhausted = true;
        break;
      }
      const drift = driftReason(current.executable);
      if (drift) {
        unresolvedReason = drift;
        break;
      }
      const entry = { cwd: current.cwd };
      const result = await spawn(current.executable, [resolved.name, "--exact", "--test-threads=1"], {
        cwd: current.cwd,
        env: envForExecutable(entry, env),
        timeoutMs,
      });
      rec.solo_runs += 1;
      const folded = foldRunResult(current.executable, result);
      if (folded.outcomes === null) {
        // A killed/hung solo run of a single test is a failure OF THAT TEST
        // (nothing else was running), not an unparsed run — except when the
        // parser saw nothing at all, which stays fail-closed as UNPARSED.
        if (result.timedOut) {
          rec.executable = current.executable;
          rec.solo_failures += 1;
          if (!rec.sample_panic) rec.sample_panic = `solo re-run ${folded.reason} (${timeoutMs} ms)`;
          continue;
        }
        rec.solo_unparsed += 1;
        continue;
      }
      const outcome = folded.outcomes.get(c.testId);
      if (outcome === undefined) {
        triedNoSuchTest.push(current.executable);
        // Not this binary's test; the attempt is not a solo run of the test.
        rec.solo_runs -= 1;
        if (queue.length > 0) {
          current = queue.shift();
          k -= 1;
          continue;
        }
        unresolvedReason = `solo re-run of \`${resolved.name}\` executed no test of that name (0 tests ran) in ${triedNoSuchTest.join(", ")} — the id belongs to none of the executables normalising to its prefix`;
        break;
      }
      rec.executable = current.executable;
      if (outcome === "fail") {
        rec.solo_failures += 1;
        if (!rec.sample_panic) rec.sample_panic = extractPanicText(folded.text, resolved.name);
      }
    }
    if (unresolvedReason) {
      Object.assign(rec, labelFor({ suiteRuns: rec.suite_runs, suiteFailures: rec.suite_failures, soloRuns: 0, soloFailures: 0, unresolvedReason }));
    } else if (rec.solo_runs < soloRuns && budgetExhausted && rec.solo_runs === 0) {
      Object.assign(rec, labelFor({ suiteRuns: rec.suite_runs, suiteFailures: rec.suite_failures, soloRuns: 0, soloFailures: 0, budgetExhausted: true }));
    } else {
      Object.assign(rec, labelFor({ suiteRuns: rec.suite_runs, suiteFailures: rec.suite_failures, soloRuns: rec.solo_runs, soloFailures: rec.solo_failures, soloUnparsed: rec.solo_unparsed }));
      if (rec.solo_runs < soloRuns && budgetExhausted) {
        rec.reason += ` (only ${rec.solo_runs}/${soloRuns} solo re-runs fit the budget)`;
      }
    }
    tests[c.testId] = rec;
    log(`solo  ${c.testId}: ${rec.label} — ${rec.reason}`);
  }
  return { tests, budgetExhausted };
}

/**
 * Census mode.
 *
 * @param {object} p
 * @param {Array<{executable: string, binaryId: string, cwd: string|null}>} p.executables
 * @param {Array<{executable: string, reason: string}>} [p.skipped]
 * @param {number} p.runs
 * @param {number} p.soloRuns
 * @param {typeof defaultSpawn} [p.spawn]
 * @param {number|null} [p.budgetSeconds]
 * @param {() => number} [p.now]
 * @param {number} [p.runTimeoutMs]
 * @param {number} [p.soloTimeoutMs]
 * @param {(msg: string) => void} [p.log]
 * @param {string|null} [p.treeSha]
 * @param {string} [p.hostname]
 * @param {NodeJS.ProcessEnv} [p.env]
 * @returns {Promise<ReturnType<typeof buildReport>>}
 */
export async function runCensus({
  executables,
  skipped = [],
  runs,
  soloRuns,
  spawn = defaultSpawn,
  budgetSeconds = null,
  now = Date.now,
  runTimeoutMs = 30 * 60 * 1000,
  soloTimeoutMs = 5 * 60 * 1000,
  log = () => {},
  treeSha = null,
  hostname = osHostname(),
  env = process.env,
  fingerprint = sha256File,
}) {
  const startedMs = now();
  const deadlineMs = budgetSeconds != null ? startedMs + budgetSeconds * 1000 : null;
  const index = buildExecutableIndex(executables);
  const unparsed = [];
  const runMaps = [];
  const firstPanic = new Map();
  const digests = new Map();
  for (const e of executables) {
    try {
      digests.set(e.executable, fingerprint(e.executable));
    } catch (err) {
      digests.set(e.executable, null);
      log(`fingerprint ${e.executable}: ${err?.message ?? err}`);
    }
  }

  for (let r = 1; r <= runs; r += 1) {
    const merged = new Map();
    let anyParsed = false;
    for (const e of executables) {
      // The deadline is anchored at census START and covers the suite runs
      // too: a run that has not started when it passes is recorded as a
      // non-answer (→ exit 2 through `exitCodeFor`), never silently skipped
      // — five hung executables at the per-run timeout must not blow the
      // job's bound before the json exists.
      if (deadlineMs !== null && now() >= deadlineMs) {
        const reason = "budget passed before this run";
        unparsed.push({ run: r, executable: e.executable, reason });
        log(`run ${r}/${runs}  ${e.executable}: UNPARSED — ${reason}`);
        continue;
      }
      log(`run ${r}/${runs}  ${e.executable}`);
      const expected = digests.get(e.executable);
      let current = null;
      try {
        current = fingerprint(e.executable);
      } catch (err) {
        current = `unreadable: ${err?.message ?? err}`;
      }
      if (expected === null || current !== expected) {
        const reason = `executable changed on disk since resolution (sha256 ${expected ?? "unreadable"} -> ${current}) — a concurrent build replaced it; this run would have measured a different tree`;
        unparsed.push({ run: r, executable: e.executable, reason });
        log(`run ${r}/${runs}  ${e.executable}: UNPARSED — ${reason}`);
        continue;
      }
      const result = await spawn(e.executable, [], {
        cwd: e.cwd,
        env: envForExecutable(e, env),
        timeoutMs: runTimeoutMs,
      });
      const folded = foldRunResult(e.executable, result);
      if (folded.outcomes === null) {
        unparsed.push({ run: r, executable: e.executable, reason: folded.reason });
        log(`run ${r}/${runs}  ${e.executable}: UNPARSED — ${folded.reason}`);
        continue;
      }
      anyParsed = true;
      let reds = 0;
      for (const [testId, outcome] of folded.outcomes) {
        merged.set(testId, outcome);
        if (outcome === "fail") {
          reds += 1;
          if (!firstPanic.has(testId)) {
            // `exactNameFor`: libtest's `---- <name> stdout ----` header carries
            // the bare name, never the ` - should panic` suffix the id keeps.
            firstPanic.set(testId, extractPanicText(folded.text, exactNameFor(splitTestId(testId).name)));
          }
        }
      }
      log(`run ${r}/${runs}  ${e.executable}: ${folded.outcomes.size} tests, ${reds} red`);
    }
    runMaps.push(anyParsed ? merged : null);
  }

  const diff = diffFailureSets(runMaps);
  const candidates = diff.everRed.map((testId) => {
    const rec = diff.perTest.get(testId);
    return {
      testId,
      suiteRuns: rec.seen,
      suiteFailures: rec.failures,
      failedInRuns: rec.failedInRuns,
      samplePanic: firstPanic.get(testId) ?? null,
    };
  });
  log(`suite: ${candidates.length} test(s) red at least once (${diff.intermittent.length} intermittent, ${diff.always.length} every run); re-running each alone ${soloRuns}x`);

  const { tests, budgetExhausted } = await soloRerunAll({
    candidates,
    index,
    soloRuns,
    spawn,
    deadlineMs,
    now,
    timeoutMs: soloTimeoutMs,
    log,
    env,
    digests,
    fingerprint,
  });

  const header = {
    mode: "census",
    tree_sha: treeSha,
    hostname,
    runs,
    solo_runs: soloRuns,
    started_at: new Date(startedMs).toISOString(),
    finished_at: nowIso(now),
    budget_seconds: budgetSeconds,
    budget_exhausted: budgetExhausted,
    executables: executables.map((e) => ({ executable: e.executable, source: e.source ?? null, sha256: digests.get(e.executable) ?? null, binary_id: e.binaryId, cwd: e.cwd })),
    skipped_executables: skipped,
    collisions: index.collisions,
    unparsed,
    intermittent: diff.intermittent,
    always_red: diff.always,
  };
  return buildReport({ header, tests });
}

/**
 * `--classify-from-log` mode. Always exit 0 (the report's `exit_code` is 0 by
 * construction); the classification is in the blocks and the annotations.
 *
 * @param {object} p
 * @param {string} p.logText the gating step's `cargo test --verbose` log
 * @param {Array<{executable: string, binaryId: string, cwd: string|null}>} p.executables
 * @param {Array<{executable: string, reason: string}>} [p.skipped]
 * @param {number} p.soloRuns
 * @param {typeof defaultSpawn} [p.spawn]
 * @param {number|null} [p.budgetSeconds]
 * @param {() => number} [p.now]
 * @param {number} [p.soloTimeoutMs]
 * @param {(msg: string) => void} [p.log]
 * @param {string|null} [p.treeSha]
 * @param {string} [p.hostname]
 * @param {NodeJS.ProcessEnv} [p.env]
 * @returns {Promise<{report: ReturnType<typeof buildReport>, annotations: string[]}>}
 */
export async function classifyFromLog({
  logText,
  executables,
  skipped = [],
  soloRuns,
  spawn = defaultSpawn,
  budgetSeconds = null,
  now = Date.now,
  soloTimeoutMs = 5 * 60 * 1000,
  log = () => {},
  treeSha = null,
  hostname = osHostname(),
  env = process.env,
}) {
  const startedMs = now();
  const deadlineMs = budgetSeconds != null ? startedMs + budgetSeconds * 1000 : null;
  const index = buildExecutableIndex(executables);
  const parsed = parseTestOutcomes(logText);
  const unparsed = [];
  const annotations = [];
  let candidates = [];
  if (parsed.unparsed) {
    unparsed.push({ run: 1, executable: "<log>", reason: parsed.reason ?? "unparsed" });
    annotations.push(
      `::warning title=UNPARSED::the gating log carried no recognisable cargo test output (${parsed.reason}); no test could be classified — fail-closed, not "no reds"`,
    );
    log(`log: UNPARSED — ${parsed.reason}`);
  } else {
    candidates = parsed.tests
      .filter((t) => t.outcome === "fail")
      .map((t) => ({
        testId: t.testId,
        suiteRuns: 1,
        suiteFailures: 1,
        failedInRuns: [1],
        samplePanic: extractPanicText(logText, exactNameFor(splitTestId(t.testId).name)),
      }));
    log(`log: ${parsed.tests.length} outcomes, ${candidates.length} red; re-running each alone ${soloRuns}x`);
  }

  const { tests, budgetExhausted } = await soloRerunAll({
    candidates,
    index,
    soloRuns,
    spawn,
    deadlineMs,
    now,
    timeoutMs: soloTimeoutMs,
    log,
    env,
  });
  for (const id of Object.keys(tests).sort()) annotations.push(formatAnnotation(id, tests[id]));

  const header = {
    mode: "classify-from-log",
    tree_sha: treeSha,
    hostname,
    runs: 1,
    solo_runs: soloRuns,
    started_at: new Date(startedMs).toISOString(),
    finished_at: nowIso(now),
    budget_seconds: budgetSeconds,
    budget_exhausted: budgetExhausted,
    executables: executables.map((e) => ({ executable: e.executable, source: e.source ?? null, binary_id: e.binaryId, cwd: e.cwd })),
    skipped_executables: skipped,
    collisions: index.collisions,
    unparsed,
    intermittent: [],
    always_red: [],
  };
  return { report: buildReport({ header, tests }), annotations };
}

// ===========================================================================
// CLI
// ===========================================================================

/**
 * `cargo test --no-run --message-format=json` at the workspace root, through
 * `--cargo` (which may be a wrapper with arguments, e.g.
 * `"bash /path/cargo-guard.sh"`). Only stdout lines starting with `{` are
 * read; the wrapper's stderr is passed through so a build-lock wait is visible.
 *
 * @param {{cargo: string, cwd: string}} p
 * @returns {string} the json stream
 */
export function resolveExecutablesViaCargo({ cargo, cwd }) {
  const parts = String(cargo).trim().split(/\s+/).filter(Boolean);
  const [cmd, ...pre] = parts;
  const res = spawnSync(cmd, [...pre, "test", "--no-run", "--message-format=json"], {
    cwd,
    encoding: "utf8",
    maxBuffer: 512 * 1024 * 1024,
    stdio: ["ignore", "pipe", "inherit"],
  });
  if (res.error) throw new Error(`could not run \`${cargo} test --no-run\`: ${res.error.message}`);
  if (res.status !== 0) {
    throw new Error(`\`${cargo} test --no-run --message-format=json\` exited ${res.status ?? res.signal}`);
  }
  return res.stdout ?? "";
}

function gitHead(cwd) {
  const res = spawnSync("git", ["rev-parse", "HEAD"], { cwd, encoding: "utf8" });
  if (res.status !== 0) return null;
  return res.stdout.trim() || null;
}

function printUsage(stream) {
  stream.write(
    [
      "usage: node scripts/test-interleave-census.mjs [--runs N] [--solo-runs K]",
      "         [--cargo <cmd>] [--exe-list <file>] [--budget-seconds N]",
      "         [--format pretty|json] [--out <path>]",
      "         [--run-timeout-seconds N] [--solo-timeout-seconds N] [--snapshot-dir <dir>]",
      "       node scripts/test-interleave-census.mjs --classify-from-log <log> [same flags]",
      "",
      "exit (census): 0 no SUITE-ONLY and no non-answers; 1 any SUITE-ONLY; 2 any non-answer",
      "               (unparsed, budget passed, or an executable that could not be resolved)",
      "exit (--classify-from-log): always 0; usage error 64",
      "",
    ].join("\n"),
  );
}

function positiveInt(v, name) {
  const n = Number.parseInt(String(v), 10);
  if (!Number.isFinite(n) || n < 1) throw new Error(`--${name} must be a positive integer, got ${JSON.stringify(v)}`);
  return n;
}

export function parseCliArgs(argv) {
  const { values } = nodeParseArgs({
    args: argv,
    allowPositionals: false,
    options: {
      runs: { type: "string", default: "5" },
      "solo-runs": { type: "string", default: "3" },
      cargo: { type: "string", default: "cargo" },
      "exe-list": { type: "string" },
      "budget-seconds": { type: "string" },
      format: { type: "string", default: "pretty" },
      out: { type: "string" },
      "classify-from-log": { type: "string" },
      "run-timeout-seconds": { type: "string", default: "1800" },
      "solo-timeout-seconds": { type: "string", default: "300" },
      "workspace-root": { type: "string" },
      "snapshot-dir": { type: "string" },
      help: { type: "boolean", default: false },
    },
  });
  if (values.help) return { help: true };
  if (values.format !== "pretty" && values.format !== "json") {
    throw new Error(`--format must be pretty or json, got ${JSON.stringify(values.format)}`);
  }
  return {
    help: false,
    runs: positiveInt(values.runs, "runs"),
    soloRuns: positiveInt(values["solo-runs"], "solo-runs"),
    cargo: values.cargo,
    exeList: values["exe-list"] ?? null,
    budgetSeconds: values["budget-seconds"] != null ? positiveInt(values["budget-seconds"], "budget-seconds") : null,
    format: values.format,
    out: values.out ?? null,
    classifyFromLog: values["classify-from-log"] ?? null,
    runTimeoutMs: positiveInt(values["run-timeout-seconds"], "run-timeout-seconds") * 1000,
    soloTimeoutMs: positiveInt(values["solo-timeout-seconds"], "solo-timeout-seconds") * 1000,
    workspaceRoot: values["workspace-root"] ?? null,
    snapshotDir: values["snapshot-dir"] ?? null,
  };
}

export async function main(argv) {
  let args;
  try {
    args = parseCliArgs(argv);
  } catch (e) {
    process.stderr.write(`test-interleave-census: ${e.message}\n`);
    printUsage(process.stderr);
    return 64;
  }
  if (args.help) {
    printUsage(process.stdout);
    return 0;
  }
  const scriptDir = dirname(fileURLToPath(import.meta.url));
  const workspaceRoot = args.workspaceRoot ? resolve(args.workspaceRoot) : resolve(scriptDir, "..");
  const log = (m) => process.stderr.write(`[census ${new Date().toISOString()}] ${m}\n`);

  let resolvedExecutables;
  if (args.exeList) {
    resolvedExecutables = parseExeList(readFileSync(args.exeList, "utf8"), { defaultCwd: workspaceRoot });
    log(`executables: ${resolvedExecutables.executables.length} from --exe-list ${args.exeList}`);
  } else {
    log(`executables: resolving via \`${args.cargo} test --no-run --message-format=json\` at ${workspaceRoot}`);
    resolvedExecutables = parseCargoBuildMessages(resolveExecutablesViaCargo({ cargo: args.cargo, cwd: workspaceRoot }));
    log(`executables: ${resolvedExecutables.executables.length} resolved`);
  }
  for (const s of resolvedExecutables.skipped) log(`executables: skipped ${s.executable} — ${s.reason}`);
  let executables = resolvedExecutables.executables;
  if (args.snapshotDir) {
    executables = snapshotExecutables(executables, resolve(args.snapshotDir));
    log(`executables: ${executables.length} copied to ${resolve(args.snapshotDir)} (the census runs the copies)`);
  }

  const common = {
    executables,
    skipped: resolvedExecutables.skipped,
    soloRuns: args.soloRuns,
    budgetSeconds: args.budgetSeconds,
    soloTimeoutMs: args.soloTimeoutMs,
    log,
    treeSha: gitHead(workspaceRoot),
  };

  let report;
  let annotations = [];
  if (args.classifyFromLog) {
    const logText = readFileSync(args.classifyFromLog, "utf8");
    ({ report, annotations } = await classifyFromLog({ ...common, logText }));
  } else {
    if (resolvedExecutables.executables.length === 0) {
      process.stderr.write("test-interleave-census: no test executables resolved — nothing to run (fail-closed: exit 2)\n");
      return 2;
    }
    report = await runCensus({ ...common, runs: args.runs, runTimeoutMs: args.runTimeoutMs });
  }

  const json = JSON.stringify(report, null, 2);
  if (args.out) {
    writeFileSync(args.out, `${json}\n`);
    log(`wrote ${args.out}`);
  }
  if (args.format === "json") process.stdout.write(`${json}\n`);
  else process.stdout.write(`${formatPretty(report)}\n`);
  for (const a of annotations) process.stdout.write(`${a}\n`);
  return report.exit_code;
}

// Only run when invoked directly, so the unit tests can import the module.
const invokedDirectly =
  process.argv[1] && resolve(process.argv[1]) === resolve(fileURLToPath(import.meta.url));
if (invokedDirectly) {
  main(process.argv.slice(2))
    .then((code) => {
      process.exitCode = code;
    })
    .catch((e) => {
      process.stderr.write(`test-interleave-census: ${e?.stack ?? e}\n`);
      // In classify mode the contract is exit-0-always; a crash still must not
      // fail the calling job, but it must be LOUD.
      process.exitCode = process.argv.includes("--classify-from-log") ? 0 : 2;
    });
}

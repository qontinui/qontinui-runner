#!/usr/bin/env node
/**
 * ci-expiry-phase-summary.mjs — say WHICH PHASE of the Rust test steps failed,
 * and name what was in flight when it did.
 *
 * Phase 2 of plan
 * `2026-09-17-the-windows-test-gate-is-a-90-minute-build-wearing-a-test-shaped-bound`.
 *
 * WHY THIS EXISTS. Before the Phase 1 split, `ci.yml`'s job `test` ran one
 * `cargo test` under one `timeout-minutes`, and its expiry printed
 * `The action 'Run Rust tests' has timed out after 90 minutes` — a sentence
 * equally compatible with a slow build, a hung test and a wedged runner, which
 * want three different responses. On run `35043646051` attempt 1 every fact
 * needed to tell them apart was sitting in the job log — a single 80.0-minute
 * silent window whose both edges were `Running rustc` lines, and then a suite
 * killed MID-RUN — and no reader produced any of it; a steward reconstructed
 * it by hand.
 *
 * Be precise about that incident, because the obvious reading is wrong and an
 * earlier version of this header carried it. The `test result: ok. 1354
 * passed` line two minutes before the kill is ONE BINARY OF 29, not the suite:
 * the largest binary (9905 tests) started at 02:58:22, was still emitting `ok`
 * lines at 03:00:25, and never reported at all. The run was ~62% complete
 * (7112 rows recorded against a suite of 11,486 by its `test result:` lines).
 * That is exactly the state this script must describe honestly rather than
 * summarising as a pass — see the all-`ok`-and-still-failed arm below.
 *
 * The split answers "which phase" STRUCTURALLY — whichever step's clock
 * expired. This script says it where a human reads it, names the crate that was
 * compiling, and says UNKNOWN when it cannot tell.
 *
 * WHAT IT DELIBERATELY DOES NOT DO — read this before "restoring" it.
 * The plan's Phase 2 also asked for "the elapsed split between first-output and
 * last-output". **That is not derivable here and asking for it would have
 * produced a confident wrong number.** GitHub's Actions log service adds the ISO
 * timestamp to each line at SERVE time; the file this script reads is the one
 * `tee` wrote, which carries no timestamps at all (the plan's own §4 measured
 * its gaps from `gh api .../logs`, a different artifact, fetched after the
 * fact). The split itself supersedes that measurement anyway: which step's clock
 * expired is exactly the fact the elapsed-gap analysis was reconstructing, and
 * it is now reported by GitHub rather than inferred.
 *
 * NEVER FAILS THE CALLING JOB. Every path exits 0. The caller additionally sets
 * `continue-on-error: true` and, deliberately, NO step-level `timeout-minutes`:
 * pairing one with `continue-on-error` cancels the whole JOB on trip rather than
 * the step (see `ci-test-results-ingest.mjs`'s header for the expensive version
 * of that lesson).
 *
 * USAGE
 *   node scripts/ci-expiry-phase-summary.mjs \
 *     --build-log <path> --run-log <path> \
 *     --build-outcome <success|failure|cancelled|skipped> \
 *     --run-outcome   <success|failure|cancelled|skipped>
 *
 * Output goes to `$GITHUB_STEP_SUMMARY` when that variable names a writable
 * file, and to stdout always, so a local run is readable and a CI run is both.
 *
 * EXIT CODES
 *   Always 0. A CLI usage error prints usage and still exits 0 — this is a
 *   diagnostic on a failure path and must never add a second failure to a job
 *   that is already red.
 */

import { readFileSync, appendFileSync } from "node:fs";
import { parseArgs as nodeParseArgs } from "node:util";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { hasCargoTestOutput, normalizeLogLine } from "./ci-flake-analyze.mjs";

/// A cargo `--verbose` rustc invocation announcement. cargo prints
///     Running `/path/to/rustc --crate-name qontinui_runner --edition=2021 …`
/// so the crate name is the token after `--crate-name`. Matched on the flag
/// rather than on the `Running` prefix, because the prefix's exact spelling
/// (leading spaces, backticks, the interpreter path) is cargo's to change and
/// the flag is rustc's stable CLI.
const CRATE_NAME_RE = /--crate-name\s+(\S+)/;

/// Lines that prove the COMPILER, not the harness, rejected the work. `cargo`
/// prints `error: could not compile …`; `rustc` prints `error[E0425]: …` and
/// bare `error: …`. A build step that failed WITHOUT one of these did not fail
/// to compile — which, on a step carrying a `timeout-minutes`, is the timeout's
/// signature, because GitHub writes its `##[error]The action … has timed out`
/// into the JOB log and never into the file `tee` produced.
const COMPILE_ERROR_RE = /^error(\[[A-Za-z0-9]+\])?:/;

/// rustc's own out-of-memory abort, and the Windows status code the allocator
/// aborting produces. Called out separately because it is the exact condition
/// `CARGO_BUILD_JOBS` is throttled against, and the step comment's revert
/// ladder keys on it.
const OOM_RE = /(rustc-LLVM ERROR: out of memory|0xc000001d|Allocation failed)/i;

/// cargo's per-binary result line: `test result: ok. 1354 passed; 0 failed; …`
const TEST_RESULT_RE = /^test result:\s*(ok|FAILED)\b/;

/**
 * The last crate rustc was invoked on in this log, or `null`.
 *
 * @param {string[]} lines normalised lines
 * @returns {string|null}
 */
export function lastRustcCrate(lines) {
  let last = null;
  for (const line of lines) {
    const m = CRATE_NAME_RE.exec(line);
    if (m) last = m[1];
  }
  return last;
}

/**
 * Tally the per-binary `test result:` lines.
 *
 * @param {string[]} lines normalised lines
 * @returns {{binaries: number, anyFailed: boolean}}
 */
export function tallyTestResults(lines) {
  let binaries = 0;
  let anyFailed = false;
  for (const line of lines) {
    const m = TEST_RESULT_RE.exec(line.trim());
    if (!m) continue;
    binaries += 1;
    if (m[1] === "FAILED") anyFailed = true;
  }
  return { binaries, anyFailed };
}

/**
 * Classify one log body. PURE — no I/O — so the tests drive it directly.
 *
 * `text` is `null` when the file could not be read at all, which is a DIFFERENT
 * fact from an empty or unrecognisable log and is never collapsed into one.
 *
 * @param {string|null} text
 * @returns {{readable: boolean, empty: boolean, recognisedAsRun: boolean,
 *            crate: string|null, compileError: boolean, oom: boolean,
 *            binaries: number, anyFailed: boolean, lineCount: number}}
 */
export function describeLog(text) {
  if (typeof text !== "string") {
    return {
      readable: false,
      empty: false,
      recognisedAsRun: false,
      crate: null,
      compileError: false,
      oom: false,
      binaries: 0,
      anyFailed: false,
      lineCount: 0,
    };
  }
  // Drop the trailing "" that `split` yields for a file ending in a newline,
  // so `lineCount` is the number of LINES rather than of separators: a 2-line
  // file used to report 3. `empty` below still answers the empty case, which
  // `lineCount` alone cannot.
  const raw = text.split("\n");
  if (raw.length > 1 && raw[raw.length - 1] === "") raw.pop();
  const lines = raw.map(normalizeLogLine);
  const { binaries, anyFailed } = tallyTestResults(lines);
  return {
    readable: true,
    // `"".split("\n")` is `[""]`, so `lineCount` alone cannot tell an EMPTY log
    // from a one-line one — the very distinction the abstain arm quotes it for.
    // Carry the emptiness separately rather than pretending the count answers it.
    empty: text.length === 0,
    recognisedAsRun: hasCargoTestOutput(lines),
    crate: lastRustcCrate(lines),
    compileError: lines.some((l) => COMPILE_ERROR_RE.test(l.trim())),
    oom: lines.some((l) => OOM_RE.test(l)),
    binaries,
    anyFailed,
    lineCount: lines.length,
  };
}

/// The step outcomes GitHub can report. Anything else — an unexpanded
/// `${{ … }}`, an empty string, a typo — is UNKNOWN, never silently treated as
/// a pass, because reading an unknown outcome as `success` is what would let
/// this script announce the wrong phase with full confidence.
const KNOWN_OUTCOMES = new Set(["success", "failure", "cancelled", "skipped"]);

/**
 * Decide which phase failed, and what to say about it. PURE.
 *
 * @param {{buildLog: ReturnType<typeof describeLog>,
 *          runLog: ReturnType<typeof describeLog>,
 *          buildOutcome: string|undefined, runOutcome: string|undefined}} input
 * @returns {{phase: "build"|"run"|"elsewhere"|"unknown", title: string, lines: string[]}}
 */
export function classifyExpiry({ buildLog, runLog, buildOutcome, runOutcome }) {
  const bo = KNOWN_OUTCOMES.has(buildOutcome) ? buildOutcome : null;
  const ro = KNOWN_OUTCOMES.has(runOutcome) ? runOutcome : null;
  const lines = [];

  // UNKNOWN first, and stated as UNKNOWN. An outcome this script could not read
  // establishes nothing about which phase failed, and a diagnostic that guesses
  // is worse than one that abstains — the whole class of defect this step
  // exists to remove is a confident sentence nobody can check.
  if (bo === null || ro === null) {
    lines.push(
      `Step outcomes could not be read (build=\`${buildOutcome ?? "<absent>"}\`, ` +
        `run=\`${runOutcome ?? "<absent>"}\`), so which phase failed is **UNKNOWN**.`,
      "This is a statement of UNKNOWN, not a verdict of \"no failure\" — read the step list above.",
    );
    return { phase: "unknown", title: "Rust test phase: UNKNOWN", lines };
  }

  if (bo === "failure") {
    const where = buildLog.crate
      ? `\`${buildLog.crate}\` was the last crate rustc was invoked on.`
      : "No `--crate-name` invocation was found in the build log, so the crate in flight is UNKNOWN.";
    lines.push(`**The BUILD phase failed** (\`Build Rust tests\`). ${where}`);
    if (!buildLog.readable) {
      lines.push(
        "The build log could not be read, so compile-error-vs-expiry is **UNKNOWN**.",
      );
    } else if (buildLog.oom) {
      // OOM FIRST, and ahead of the abstain arm below. This is the single most
      // actionable sentence this script emits, and it is derivable from the log
      // alone — a `rustc-LLVM ERROR: out of memory` does not become less true
      // because cargo never printed a `--crate-name` line. An earlier cut put
      // the abstain arm above this one and told a log containing
      // `rustc-LLVM ERROR` that it "contains no rustc invocation at all",
      // which is both false and the opposite of useful.
      lines.push(
        "The log carries an rustc/LLVM out-of-memory signature. That is the condition " +
          "`CARGO_BUILD_JOBS` is throttled against — follow the revert ladder in the step's " +
          "own `env:` comment (revert the jobs value, keep the 32 GB pagefile).",
      );
    } else if (buildLog.compileError) {
      // Likewise ahead of the abstain arm: `error: failed to select a version`
      // and `error: could not compile workspace` are cargo-level failures that
      // carry no rustc invocation at all, and they are a COMPILE ERROR whatever
      // the abstain arm would otherwise say about them.
      lines.push(
        "The log carries a compiler `error:` line, so this is a COMPILE ERROR, not a bound expiry. " +
          "Fix the code; the timeout is not implicated.",
      );
    } else if (buildLog.crate === null) {
      // ABSTAIN — and ONLY once OOM and compile-error have been ruled out. A
      // readable log carrying no rustc invocation, no `error:` and no OOM is at
      // least as consistent with the step dying before cargo emitted anything
      // (`cd src-tauri` failing, cargo missing, the disk full, an immediate
      // runner kill) as with a mid-compile expiry. The no-`error:`-line arm
      // below would call that a SLOW BUILD with full confidence, which is the
      // class this file promises to abstain on.
      lines.push(
        `The build log is readable (${buildLog.empty ? "empty" : `${buildLog.lineCount} line(s)`}) but ` +
          "carries no rustc invocation, no compiler `error:` and no OOM signature, so **UNKNOWN**: " +
          "an expiry before the first compile and a step that died before cargo ran are " +
          "indistinguishable from here. Read the job log's own step result rather than inferring one.",
      );
    } else {
      lines.push(
        "The log carries **no** compiler `error:` line, which is the signature of the step's " +
          "`timeout-minutes` expiring mid-compile: GitHub writes `The action … has timed out` " +
          "into the job log, never into the tee'd file. Treat this as a SLOW BUILD, not a broken one.",
      );
    }
    // FOUR states, and the ORDER matters. `ro` is checked first because on the
    // commonest build failure GitHub skips the run step, so `cargo-test-output.log`
    // is never written and the log read comes back unreadable — and an earlier
    // cut answered UNKNOWN there while being TOLD `skipped`, which is throwing
    // away the fact that settles it. Only an unreadable log whose step did NOT
    // report `skipped` is genuinely unknown.
    //
    // Below that, `recognisedAsRun: false` is still true BOTH when the log was
    // read and had no test output AND when it could not be read at all, and
    // `describeLog`'s own contract says those must never be collapsed.
    if (ro === "skipped") {
      lines.push(
        "The run step was SKIPPED, so no test executed — the build never produced binaries to run.",
      );
    } else if (!runLog.readable) {
      lines.push(
        "The run log could not be read and the run step did not report `skipped`, so whether " +
          "anything executed is **UNKNOWN**.",
      );
    } else if (runLog.recognisedAsRun) {
      lines.push(
        "The run log nevertheless carries recognisable test output — read it before assuming nothing ran.",
      );
    } else {
      lines.push("No test ever executed: the run step never got the binaries.");
    }
    return { phase: "build", title: "Rust test phase: BUILD", lines };
  }

  if (ro === "failure") {
    lines.push("**The RUN phase failed** (`Run Rust tests`).");
    if (!runLog.readable) {
      lines.push("The run log could not be read, so the cause is **UNKNOWN**.");
    } else if (!runLog.recognisedAsRun) {
      lines.push(
        "The run log carries no recognisable cargo test output at all (no `test result:`, " +
          "no `running N tests`, no `test … ... ok`). The suite did not reach the point of " +
          "reporting, so this is **UNKNOWN** between a pre-test failure and an immediate kill — " +
          "not evidence that the tests passed and not evidence that they failed.",
      );
    } else if (runLog.anyFailed) {
      lines.push(
        `A real test failure: ${runLog.binaries} binary result line(s), at least one \`FAILED\`. ` +
          "The bound is not implicated — fix the test.",
      );
    } else {
      lines.push(
        `${runLog.binaries} binary result line(s), all \`ok\`, and the step still failed. That is ` +
          "the shape of a bound expiring with test binaries still to run, or of a harness-level " +
          "abort after the last reported binary. Compare the reported binaries against the suite's " +
          "full set before re-running.",
      );
    }
    return { phase: "run", title: "Rust test phase: RUN", lines };
  }

  lines.push(
    `Neither Rust test step failed (build=\`${bo}\`, run=\`${ro}\`), so this job's failure is ` +
      "in some other step. Nothing here is a verdict on the Rust suite.",
  );
  return { phase: "elsewhere", title: "Rust test phase: not the Rust test steps", lines };
}

/**
 * Render the markdown block. PURE.
 *
 * @param {ReturnType<typeof classifyExpiry>} verdict
 * @returns {string}
 */
export function renderSummary(verdict) {
  return [`### ${verdict.title}`, "", ...verdict.lines.map((l) => `- ${l}`), ""].join("\n");
}

function readOrNull(path) {
  if (!path) return null;
  try {
    return readFileSync(path, "utf8");
  } catch {
    return null;
  }
}

function emit(text) {
  process.stdout.write(text + "\n");
  const summary = process.env.GITHUB_STEP_SUMMARY;
  if (!summary) return;
  try {
    appendFileSync(summary, text + "\n", "utf8");
  } catch (err) {
    // A summary file we cannot append to is not worth failing a diagnostic
    // over; stdout above already carried the whole message.
    process.stdout.write(
      `::warning title=ci-expiry-phase-summary::could not append to GITHUB_STEP_SUMMARY: ${err.message}\n`,
    );
  }
}

async function main(argv) {
  let parsed;
  try {
    parsed = nodeParseArgs({
      args: argv,
      options: {
        "build-log": { type: "string" },
        "run-log": { type: "string" },
        "build-outcome": { type: "string" },
        "run-outcome": { type: "string" },
        help: { type: "boolean", short: "h" },
      },
      allowPositionals: false,
    });
  } catch (err) {
    process.stdout.write(`ci-expiry-phase-summary: ${err.message}\n`);
    return 0;
  }
  if (parsed.values.help) {
    process.stdout.write(
      "Usage: ci-expiry-phase-summary.mjs --build-log <path> --run-log <path> " +
        "--build-outcome <o> --run-outcome <o>\n",
    );
    return 0;
  }

  const verdict = classifyExpiry({
    buildLog: describeLog(readOrNull(parsed.values["build-log"])),
    runLog: describeLog(readOrNull(parsed.values["run-log"])),
    buildOutcome: parsed.values["build-outcome"],
    runOutcome: parsed.values["run-outcome"],
  });
  emit(renderSummary(verdict));
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
      process.stdout.write(
        `::warning title=ci-expiry-phase-summary::unexpected error: ${e?.stack ?? e}\n`,
      );
      process.exitCode = 0;
    });
}

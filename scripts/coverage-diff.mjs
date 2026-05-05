#!/usr/bin/env node
// coverage-diff.mjs — CLI gate for regression-suite coverage (Section 11, C3).
//
// Wraps the pure `coverageDiff` function from
// `@qontinui/ui-bridge-auto/regression` so CI pipelines can fail a build when
// regression-suite coverage falls below a threshold — without booting the
// runner, a browser, or any UI Bridge connection.
//
// USAGE
//   node scripts/coverage-diff.mjs \
//       --suite        path/to/suite.json        # serialized RegressionSuite
//       --exercise-log path/to/exercise-log.json # AssertionExecution[]
//       [--threshold   0.20]                     # max acceptable uncoveredRatio
//       [--format      pretty|json]              # default: pretty
//
// FLAGS
//   --suite         (required) Path to a JSON file produced by
//                   `serializeSuite(...)` from
//                   `@qontinui/ui-bridge-auto/regression`. The file is read as
//                   text and passed verbatim to `deserializeSuite`.
//   --exercise-log  (required) Path to a JSON file holding an array of
//                   `AssertionExecution` records:
//                   `[{caseId, assertionId, executedAt}]`.
//   --threshold     Optional. Floating-point number in `[0, 1]`. The CLI
//                   exits with code 1 when the report's `stats.uncoveredRatio`
//                   STRICTLY exceeds this value. Default `0.20` (20% slack).
//   --format        Optional. `pretty` (default) prints a human-readable
//                   summary on stdout. `json` prints the full
//                   `CoverageDiffReport` as JSON for piping into other tools.
//
// EXIT CODES
//   0 — coverage acceptable (uncoveredRatio <= threshold).
//   1 — coverage below threshold OR runtime error reading/parsing the inputs.
//   2 — invalid CLI arguments (missing required flag, malformed --threshold,
//       unknown --format value).
//
// EXAMPLES
//   # Pretty mode, 10% threshold
//   node scripts/coverage-diff.mjs \
//       --suite ./suite.json \
//       --exercise-log ./log.json \
//       --threshold 0.10
//
//   # JSON mode (machine-readable, full report on stdout)
//   node scripts/coverage-diff.mjs \
//       --suite ./suite.json \
//       --exercise-log ./log.json \
//       --format json
//
//   # via npm (runner package.json `scripts.coverage-diff`)
//   npm run coverage-diff -- --suite ./suite.json --exercise-log ./log.json

import { readFileSync } from "node:fs";
import { parseArgs } from "node:util";
import {
  coverageDiff,
  deserializeSuite,
} from "@qontinui/ui-bridge-auto/regression";

// ---------------------------------------------------------------------------
// Argument parsing
// ---------------------------------------------------------------------------

let parsed;
try {
  parsed = parseArgs({
    options: {
      suite: { type: "string" },
      "exercise-log": { type: "string" },
      threshold: { type: "string", default: "0.20" },
      format: { type: "string", default: "pretty" },
      help: { type: "boolean", short: "h", default: false },
    },
    strict: true,
    allowPositionals: false,
  });
} catch (err) {
  process.stderr.write(`error: ${err.message}\n`);
  printUsage(process.stderr);
  process.exit(2);
}

const { values } = parsed;

if (values.help) {
  printUsage(process.stdout);
  process.exit(0);
}

if (!values.suite || !values["exercise-log"]) {
  process.stderr.write(
    "error: --suite and --exercise-log are both required\n",
  );
  printUsage(process.stderr);
  process.exit(2);
}

const threshold = Number.parseFloat(values.threshold);
if (!Number.isFinite(threshold) || threshold < 0 || threshold > 1) {
  process.stderr.write(
    `error: --threshold must be a number in [0, 1], got ${JSON.stringify(values.threshold)}\n`,
  );
  process.exit(2);
}

if (values.format !== "pretty" && values.format !== "json") {
  process.stderr.write(
    `error: --format must be "pretty" or "json", got ${JSON.stringify(values.format)}\n`,
  );
  process.exit(2);
}

// ---------------------------------------------------------------------------
// Load inputs
// ---------------------------------------------------------------------------

let suiteText;
try {
  suiteText = readFileSync(values.suite, "utf8");
} catch (err) {
  process.stderr.write(
    `error: cannot read --suite ${JSON.stringify(values.suite)}: ${err.message}\n`,
  );
  process.exit(1);
}

let suite;
try {
  // `deserializeSuite` accepts a JSON STRING (not a parsed object) — see
  // ui-bridge-auto/src/state/regression-generator.ts. Passing JSON.parse
  // output here would throw inside deserializeSuite's own JSON.parse call.
  suite = deserializeSuite(suiteText);
} catch (err) {
  process.stderr.write(
    `error: failed to parse suite at ${JSON.stringify(values.suite)}: ${err.message}\n`,
  );
  process.exit(1);
}

let logText;
try {
  logText = readFileSync(values["exercise-log"], "utf8");
} catch (err) {
  process.stderr.write(
    `error: cannot read --exercise-log ${JSON.stringify(values["exercise-log"])}: ${err.message}\n`,
  );
  process.exit(1);
}

let exerciseLog;
try {
  exerciseLog = JSON.parse(logText);
} catch (err) {
  process.stderr.write(
    `error: failed to parse exercise log at ${JSON.stringify(values["exercise-log"])}: ${err.message}\n`,
  );
  process.exit(1);
}
if (!Array.isArray(exerciseLog)) {
  process.stderr.write(
    `error: exercise log at ${JSON.stringify(values["exercise-log"])} must be a JSON array of AssertionExecution records\n`,
  );
  process.exit(1);
}

// ---------------------------------------------------------------------------
// Compute + emit
// ---------------------------------------------------------------------------

const report = coverageDiff(suite, exerciseLog);

if (values.format === "json") {
  process.stdout.write(JSON.stringify(report, null, 2));
  process.stdout.write("\n");
} else {
  process.stdout.write(formatPretty(report, threshold));
}

if (report.stats.uncoveredRatio > threshold) {
  process.exit(1);
}
process.exit(0);

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function formatPretty(report, threshold) {
  const lines = [];
  const s = report.stats;

  const assertionPct =
    s.totalAssertions === 0
      ? 100
      : (s.exercisedAssertions / s.totalAssertions) * 100;
  const transitionPct =
    s.totalTransitions === 0
      ? 100
      : (s.coveredTransitions / s.totalTransitions) * 100;

  lines.push(
    `Suite coverage: ${formatPct(assertionPct)} (${s.exercisedAssertions}/${s.totalAssertions} assertions, ${s.coveredTransitions}/${s.totalTransitions} transitions)`,
  );
  lines.push(
    `  assertions exercised: ${s.exercisedAssertions}/${s.totalAssertions} (${formatPct(assertionPct)})`,
  );
  lines.push(
    `  transitions covered:  ${s.coveredTransitions}/${s.totalTransitions} (${formatPct(transitionPct)})`,
  );
  lines.push(
    `  uncoveredRatio:       ${formatRatio(s.uncoveredRatio)} (threshold ${formatRatio(threshold)})`,
  );

  if (report.unexercisedAssertions.length > 0) {
    lines.push("");
    lines.push(
      `Un-exercised assertions (${report.unexercisedAssertions.length}):`,
    );
    for (const a of report.unexercisedAssertions) {
      lines.push(`  - ${a.caseId} :: ${a.assertionId}`);
    }
  } else {
    lines.push("");
    lines.push("Un-exercised assertions: none");
  }

  if (report.uncoveredTransitions.length > 0) {
    lines.push("");
    lines.push(
      `Un-covered transitions (${report.uncoveredTransitions.length}):`,
    );
    for (const t of report.uncoveredTransitions) {
      lines.push(`  - ${t.caseId} (transition ${t.transitionId})`);
    }
  } else {
    lines.push("");
    lines.push("Un-covered transitions: none");
  }

  lines.push("");
  const pass = report.stats.uncoveredRatio <= threshold;
  if (pass) {
    lines.push(
      `Status: PASS (uncoveredRatio ${formatRatio(report.stats.uncoveredRatio)} <= threshold ${formatRatio(threshold)})`,
    );
  } else {
    lines.push(
      `Status: FAIL (uncoveredRatio ${formatRatio(report.stats.uncoveredRatio)} > threshold ${formatRatio(threshold)})`,
    );
  }
  lines.push("");
  return lines.join("\n");
}

function formatPct(n) {
  // One decimal place, no scientific notation. `100` rounds to `100.0%`.
  return `${n.toFixed(1)}%`;
}

function formatRatio(n) {
  // Four decimals — preserves enough resolution to compare against typical
  // thresholds like 0.05 / 0.10 / 0.20 without surprising rounding.
  return n.toFixed(4);
}

function printUsage(stream) {
  stream.write(
    [
      "Usage: coverage-diff.mjs --suite <path> --exercise-log <path> [--threshold 0.20] [--format pretty|json]",
      "",
      "Required:",
      "  --suite <path>          Serialized RegressionSuite JSON",
      "  --exercise-log <path>   AssertionExecution[] JSON",
      "",
      "Optional:",
      "  --threshold <float>     Max acceptable uncoveredRatio in [0, 1] (default 0.20)",
      "  --format <pretty|json>  Output format (default pretty)",
      "  -h, --help              Print this help and exit 0",
      "",
      "Exit codes:",
      "  0  coverage acceptable",
      "  1  coverage below threshold OR runtime error",
      "  2  invalid arguments",
      "",
    ].join("\n"),
  );
}

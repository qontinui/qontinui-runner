#!/usr/bin/env node
/**
 * ci-audit-ignore-review.mjs — say when a `cargo audit` ignore needs someone to
 * look at it again. The ignores are `.github/audit-ignores/<RUSTSEC-id>.md`, one
 * file per advisory, and ci.yml's `security` job builds its `--ignore` flags from
 * that directory (scripts/ci/audit-ignore-flags.sh).
 *
 * Two things go wrong with an ignore, and neither reds anything:
 *
 *   1. Its `REVIEW BY <date>` passes. The date is prose in the ignore's file.
 *   2. It goes STALE: the advisory no longer fires on Cargo.lock, so the flag
 *      matches nothing today and would hide the advisory if the crate came
 *      back. Measured 2026-11-02: RUSTSEC-2026-0118/-0119 had been stale since
 *      hickory-proto left the lock, and nothing said so.
 *
 * Dates are opt-in. An ignore with no `REVIEW BY` is only checked for staleness.
 *
 * This reports through ONE issue (ISSUE_TITLE): created, refreshed, closed when
 * nothing is left, and reopened if something comes back. See
 * .github/workflows/audit-ignore-review.yml for why it must not red a PR on a
 * date.
 *
 * Usage:
 *   node scripts/ci-audit-ignore-review.mjs --audit-json <file> --repo <owner/name>
 *        [--ignores-dir .github/audit-ignores] [--today YYYY-MM-DD] [--dry-run]
 *
 * <file> holds the output of `cargo audit --file Cargo.lock --json` with NO
 * --ignore flags. cargo-audit exits non-zero when a vulnerability fires, so the
 * caller must not treat that as failure; this script validates the report.
 *
 * Exit codes:
 *   0  read everything; the issue is up to date (or --dry-run printed the plan)
 *   1  could not read: an ignore file's name or dates are not in the expected form,
 *      the audit report is unusable, or a `gh` call failed. A red run means
 *      "this check is blind". A green run with no issue must keep meaning
 *      "nothing needs review", so every parser here refuses rather than guesses.
 *   2  bad usage
 *
 * Imports Node built-ins only, so CI runs it with no install step.
 */

import { execFileSync } from "node:child_process";
import { readdirSync, readFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { parseArgs } from "node:util";

export const ISSUE_TITLE = "cargo-audit ignores need review";

const ADVISORY_ID = /^RUSTSEC-\d{4}-\d{4}$/;
const IGNORE_FILE = /^(RUSTSEC-\d{4}-\d{4})\.md$/;

/**
 * The advisory id a file in the ignores directory names, or null for the
 * directory's README.md. Anything else is refused: a file this cannot read as an
 * ignore is one the `security` job's flags script also refuses, and skipping it
 * here would review a different list from the one CI applies.
 */
export function ignoreIdOf(fileName) {
  if (fileName === "README.md") return null;
  const m = IGNORE_FILE.exec(fileName);
  if (!m) throw new Error(`${fileName} in the ignores directory is not a <RUSTSEC-YYYY-NNNN>.md file`);
  return m[1];
}

function isRealDate(text) {
  const parsed = new Date(`${text}T00:00:00Z`);
  return !Number.isNaN(parsed.getTime()) && parsed.toISOString().slice(0, 10) === text;
}

/**
 * Every `REVIEW BY <date>` in one ignore file's body, each tied to that file's
 * advisory. A misspelling of the phrase (`Review by:`, `REVIEW-BY`, a wrapped
 * date) is refused, because reading it as no date would make the date disappear
 * and the issue close itself. Other wording (`Re-assess by <date>`) is prose and
 * is not read: a body legitimately holds other dates, so a date alone cannot mean
 * a deadline.
 */
export function parseReviewDates(id, text) {
  if (!ADVISORY_ID.test(id)) throw new Error(`\`${id}\` is not a RUSTSEC id`);
  const lines = text.replace(/\r\n/g, "\n").split("\n");
  const out = [];
  for (let i = 0; i < lines.length; i += 1) {
    const line = lines[i];
    const mentions = [...line.matchAll(/\breview[\s_-]*by\b/gi)].length;
    const dates = [...line.matchAll(/\bREVIEW BY (\d{4}-\d{2}-\d{2})(?![-\d])/g)].map((m) => m[1]);
    const wrapped = /\breview\s*$/i.test(line) && /^\s*by\b/i.test(lines[i + 1] ?? "");
    if (wrapped || mentions !== dates.length || !dates.every(isRealDate)) {
      throw new Error(`${id}.md:${i + 1} mentions a review date but not as \`REVIEW BY YYYY-MM-DD\``);
    }
    for (const date of dates) out.push({ date, ids: [id] });
  }
  return out;
}

/**
 * The ignores and their review dates, from the directory `ci.yml`'s `security`
 * job builds its `--ignore` flags from (`.github/audit-ignores/<id>.md`, one file
 * per advisory). `files` is `[{ name, text }]`, so this stays pure; `main` reads
 * the directory. No ignores at all is a fine answer, and is what the directory
 * looks like once the last one goes.
 */
export function readIgnores(files) {
  const ignores = [];
  const reviewDates = [];
  for (const { name, text } of [...files].sort((a, b) => (a.name < b.name ? -1 : a.name > b.name ? 1 : 0))) {
    const id = ignoreIdOf(name);
    if (id === null) continue;
    ignores.push(id);
    reviewDates.push(...parseReviewDates(id, text));
  }
  return { ignores, reviewDates };
}

/** `[{ name, text }]` for every entry of `dir`; a sub-directory is refused. */
export function readIgnoresDir(dir) {
  return readdirSync(dir, { withFileTypes: true }).map((entry) => {
    if (!entry.isFile()) throw new Error(`${entry.name} in ${dir} is not a file`);
    return { name: entry.name, text: readFileSync(join(dir, entry.name), "utf8") };
  });
}

/**
 * The advisory ids that fire on the lock, warnings included: cargo-audit's
 * --ignore applies to those too, so an ignore of an `unmaintained` advisory is
 * not stale. Refuses a report that could not have seen them, because an empty
 * result would call every ignore stale. A yanked crate's warning legitimately
 * has no advisory, so only the `yanked` group may lack an id.
 */
export function firedAdvisories(report) {
  const list = report?.vulnerabilities?.list;
  if (!Array.isArray(list)) throw new Error("audit report has no vulnerabilities.list");
  if (!(report.database?.["advisory-count"] > 0)) throw new Error("audit report read an empty advisory database");
  if (!(report.lockfile?.["dependency-count"] > 0)) throw new Error("audit report read an empty lockfile");
  if (report.warnings === null || typeof report.warnings !== "object" || Array.isArray(report.warnings)) {
    throw new Error("audit report has no warnings section");
  }
  const settings = report.settings;
  const unfiltered =
    Array.isArray(settings?.ignore) && settings.ignore.length === 0 &&
    (settings.target_os ?? []).length === 0 && (settings.target_arch ?? []).length === 0 &&
    settings.severity == null;
  if (!unfiltered) {
    throw new Error("audit report was filtered by configuration (an audit.toml?), so it cannot show what fires");
  }
  const count = report.vulnerabilities.count;
  if (count !== undefined && count !== list.length) {
    throw new Error(`audit report counts ${count} vulnerabilities but lists ${list.length}`);
  }
  const fired = new Set();
  for (const v of list) {
    if (typeof v?.advisory?.id !== "string") throw new Error("audit report has a vulnerability with no advisory id");
    fired.add(v.advisory.id);
  }
  for (const [kind, group] of Object.entries(report.warnings)) {
    for (const w of group) {
      if (typeof w?.advisory?.id === "string") fired.add(w.advisory.id);
      else if (kind !== "yanked") throw new Error(`audit report has a ${kind} warning with no advisory id`);
    }
  }
  return fired;
}

/** A date compares as a string here, since both sides are YYYY-MM-DD. */
export function findProblems({ ignores, reviewDates, fired, today }) {
  return {
    expired: reviewDates.filter((r) => r.date < today),
    stale: ignores.filter((id) => !fired.has(id)),
  };
}

/** Holds no line numbers or run links, so an unchanged finding causes no edit. */
export function renderBody({ expired, stale }) {
  const parts = [
    "Raised by `scripts/ci-audit-ignore-review.mjs` from `.github/workflows/audit-ignore-review.yml`. " +
      "It closes itself once these are dealt with.",
  ];
  if (expired.length) {
    parts.push(
      "## `REVIEW BY` date passed\n\n" +
        "Re-check the justification in `.github/audit-ignores/<id>.md`. " +
        "Then drop the ignore (delete the file), or set a new date and say what you checked.\n\n" +
        expired.map((r) => `- ${r.date}: ${r.ids.join(", ")}`).join("\n"),
    );
  }
  if (stale.length) {
    parts.push(
      "## Ignored, but no longer firing on `Cargo.lock`\n\n" +
        "Delete its `.github/audit-ignores/<id>.md`. A leftover ignore matches nothing " +
        "today and would hide the advisory if the crate came back.\n\n" +
        stale.map((id) => `- ${id}`).join("\n"),
    );
  }
  return parts.join("\n\n");
}

function sameBody(a, b) {
  const normal = (s) => (s ?? "").replace(/\r\n/g, "\n").trim();
  return normal(a) === normal(b);
}

/**
 * Decide against every issue with the exact title, open or closed: the lowest
 * open one owns, else the lowest closed one. Only listing open ones would file a
 * new issue each day after someone closed it with the problem still there. Only
 * another OPEN issue is a duplicate; a closed one needs nothing done to it.
 */
export function planAction(problems, issues) {
  const matches = issues.filter((i) => i.title === ISSUE_TITLE).sort((a, b) => a.number - b.number);
  const owner = matches.find((i) => i.state === "OPEN") ?? matches[0];
  const duplicates = matches.filter((i) => i !== owner && i.state === "OPEN").map((i) => i.number);
  const open = owner?.state === "OPEN";
  if (!problems.expired.length && !problems.stale.length) {
    return open ? { kind: "close", number: owner.number, duplicates } : { kind: "none", duplicates };
  }
  const body = renderBody(problems);
  if (!owner) return { kind: "create", body, duplicates };
  if (!open) return { kind: "reopen", number: owner.number, body, duplicates };
  if (sameBody(owner.body, body)) return { kind: "none", duplicates };
  return { kind: "edit", number: owner.number, body, duplicates };
}

function execGh(args, { repo, input } = {}) {
  return execFileSync("gh", args, {
    encoding: "utf8",
    env: { ...process.env, GH_REPO: repo },
    input,
    stdio: [input === undefined ? "ignore" : "pipe", "pipe", "inherit"],
  });
}

export function listIssues(repo, gh = execGh) {
  const args = [
    "issue", "list", "--state", "all", "--limit", "100",
    "--search", `"${ISSUE_TITLE}" in:title`, "--json", "number,title,body,state",
  ];
  return JSON.parse(gh(args, { repo }));
}

export function applyAction(action, repo, gh = execGh) {
  const number = String(action.number);
  switch (action.kind) {
    case "create":
      gh(["issue", "create", "--title", ISSUE_TITLE, "--body-file", "-"], { repo, input: action.body });
      break;
    case "reopen":
      gh(["issue", "edit", number, "--body-file", "-"], { repo, input: action.body });
      gh(["issue", "reopen", number], { repo });
      break;
    case "edit":
      gh(["issue", "edit", number, "--body-file", "-"], { repo, input: action.body });
      break;
    case "close":
      gh(["issue", "close", number, "--comment", "Nothing left to review."], { repo });
      break;
    case "none":
      break;
    default:
      throw new Error(`unknown action ${action.kind}`);
  }
}

function readReport(path) {
  let text;
  try {
    text = readFileSync(path, "utf8");
    return JSON.parse(text);
  } catch (e) {
    throw new Error(`audit report unreadable, did \`cargo audit\` fail? (${e.message})`);
  }
}

/** Workflow-command data: a raw newline or % would truncate or corrupt the annotation. */
function escapeData(text) {
  return String(text).replace(/%/g, "%25").replace(/\r/g, "%0D").replace(/\n/g, "%0A");
}

function main(argv) {
  let values;
  try {
    ({ values } = parseArgs({
      args: argv,
      options: {
        "audit-json": { type: "string" },
        "ignores-dir": { type: "string", default: ".github/audit-ignores" },
        repo: { type: "string" },
        today: { type: "string", default: new Date().toISOString().slice(0, 10) },
        "dry-run": { type: "boolean", default: false },
      },
    }));
  } catch (e) {
    process.stderr.write(`${e.message}\n`);
    return 2;
  }
  if (!values["audit-json"] || !values.repo || !isRealDate(values.today)) {
    process.stderr.write(
      "Usage: ci-audit-ignore-review.mjs --audit-json <file> --repo <owner/name> " +
        "[--ignores-dir <dir>] [--today YYYY-MM-DD] [--dry-run]\n",
    );
    return 2;
  }
  try {
    // Everything local is read first, so a blind run never touches GitHub.
    const { ignores, reviewDates } = readIgnores(readIgnoresDir(values["ignores-dir"]));
    const problems = findProblems({
      ignores,
      reviewDates,
      fired: firedAdvisories(readReport(values["audit-json"])),
      today: values.today,
    });
    const action = planAction(problems, listIssues(values.repo));
    process.stdout.write(`${JSON.stringify({ ...problems, action: action.kind, number: action.number })}\n`);
    if (action.duplicates.length) {
      process.stdout.write(
        `::warning title=ci-audit-ignore-review::duplicate issues, close these by hand: ${action.duplicates.join(", ")}\n`,
      );
    }
    if (values["dry-run"]) {
      if (action.body) process.stdout.write(`${action.body}\n`);
      return 0;
    }
    applyAction(action, values.repo);
    return 0;
  } catch (e) {
    process.stdout.write(`::error title=ci-audit-ignore-review::${escapeData(e.message)}\n`);
    return 1;
  }
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  process.exit(main(process.argv.slice(2)));
}

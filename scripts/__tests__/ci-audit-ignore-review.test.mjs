#!/usr/bin/env node
// Unit tests for ci-audit-ignore-review.mjs: the pure half directly, and the
// whole CLI against a stub `gh` on PATH that records argv, GH_REPO and stdin.
//
// Run with:
//   node --test scripts/__tests__/ci-audit-ignore-review.test.mjs

import test from "node:test";
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { chmodSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { delimiter, dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import {
  ISSUE_TITLE,
  findProblems,
  firedAdvisories,
  ignoreIdOf,
  parseReviewDates,
  readIgnores,
  readIgnoresDir,
  planAction,
  renderBody,
} from "../ci-audit-ignore-review.mjs";

const here = dirname(fileURLToPath(import.meta.url));
const script = join(here, "..", "ci-audit-ignore-review.mjs");
const realIgnoresDir = join(here, "..", "..", ".github", "audit-ignores");

// The ignores directory as readIgnores sees it: one file per advisory.
const FILES = [
  { name: "README.md", text: "Not an ignore. REVIEW BY anything goes here? No: the README is skipped whole.\n" },
  { name: "RUSTSEC-2026-0001.md", text: "# RUSTSEC-2026-0001\n\nOne advisory.\nREVIEW BY 2026-11-04 (or sooner).\n\nDrop when x.\n" },
  { name: "RUSTSEC-2026-0003.md", text: "Shared block.\nTracking: #1. REVIEW BY 2026-12-02. Re-assess if it changes.\n\nDrop when y.\n" },
  { name: "RUSTSEC-2026-0002.md", text: "Shared block.\nTracking: #1. REVIEW BY 2026-12-02. Re-assess if it changes.\n\nDrop when y.\n" },
  { name: "RUSTSEC-2026-0005.md", text: "No date on this one.\r\n\r\nDrop when z.\r\n" },
];

function report({ ids = [], warnings = {}, ...over } = {}) {
  return {
    database: { "advisory-count": 1402 },
    lockfile: { "dependency-count": 1110 },
    settings: { ignore: [] },
    vulnerabilities: { list: ids.map((id) => ({ advisory: { id } })) },
    warnings,
    ...over,
  };
}

test("readIgnores takes the ids from the file names, sorted, and skips the README", () => {
  const { ignores, reviewDates } = readIgnores(FILES);
  assert.deepEqual(ignores, ["RUSTSEC-2026-0001", "RUSTSEC-2026-0002", "RUSTSEC-2026-0003", "RUSTSEC-2026-0005"]);
  assert.deepEqual(reviewDates, [
    { date: "2026-11-04", ids: ["RUSTSEC-2026-0001"] },
    { date: "2026-12-02", ids: ["RUSTSEC-2026-0002"] },
    { date: "2026-12-02", ids: ["RUSTSEC-2026-0003"] },
  ]);
});

test("readIgnores accepts a directory with no ignores left", () => {
  assert.deepEqual(readIgnores([]), { ignores: [], reviewDates: [] });
  assert.deepEqual(readIgnores([{ name: "README.md", text: "" }]), { ignores: [], reviewDates: [] });
});

test("ignoreIdOf refuses any file it cannot read as one ignore", () => {
  for (const name of [
    "RUSTSEC-2026-12345.md",
    "RUSTSEC-2026-0001",
    "RUSTSEC-2026-0001.txt",
    "rustsec-2026-0001.md",
    "GHSA-aaaa-bbbb-cccc.md",
    "notes.md",
    "readme.md",
  ]) {
    assert.throws(() => ignoreIdOf(name), /not a <RUSTSEC-YYYY-NNNN>\.md file/, name);
  }
  assert.equal(ignoreIdOf("README.md"), null);
  assert.equal(ignoreIdOf("RUSTSEC-2026-0001.md"), "RUSTSEC-2026-0001");
});

test("parseReviewDates refuses a reworded date instead of reading it as no date", () => {
  for (const text of [
    "Review by 2026-11-04",
    "REVIEW BY: 2026-11-04",
    "REVIEW  BY 2026-11-04",
    "REVIEW BY 2026/11/04",
    "REVIEW-BY 2026-11-04",
    "REVIEW_BY 2026-11-04",
    "REVIEWBY 2026-11-04",
    "REVIEW BY",
    "REVIEW BY\n2026-11-04",
    "text REVIEW\nBY 2026-11-04",
    "REVIEW BY 2026-13-45",
    "REVIEW BY 2026-02-30",
    "REVIEW BY 2026-11-04, or review by then",
    "REVIEW BY 2020-01-01-99.",
  ]) {
    assert.throws(() => parseReviewDates("RUSTSEC-2026-0001", text), /REVIEW BY YYYY-MM-DD/, text);
  }
});

test("parseReviewDates treats other wording as prose, and needs a word boundary", () => {
  const dates = (text) => parseReviewDates("RUSTSEC-2026-0001", text);
  assert.deepEqual(dates("Disclosed 2026-05-13.\nRe-assess by 2020-01-01."), []);
  assert.deepEqual(dates("a preview by the team."), []);
  assert.deepEqual(dates("PREVIEW BY 2020-01-01."), []);
  assert.deepEqual(dates("Cites RUSTSEC-2026-0002. REVIEW BY 2026-11-04."), [{ date: "2026-11-04", ids: ["RUSTSEC-2026-0001"] }]);
  assert.throws(() => parseReviewDates("not-an-id", ""), /not a RUSTSEC id/);
});

test("readIgnores surfaces a bad file or a bad date, never a shorter list", () => {
  assert.throws(() => readIgnores([...FILES, { name: "notes.md", text: "" }]), /notes\.md/);
  assert.throws(
    () => readIgnores([...FILES, { name: "RUSTSEC-2026-0009.md", text: "Review by 2026-11-04\n" }]),
    /RUSTSEC-2026-0009\.md:1/,
  );
});

test("readIgnoresDir refuses a sub-directory", () => {
  const dir = mkdtempSync(join(tmpdir(), "audit-ignores-"));
  mkdirSync(join(dir, "RUSTSEC-2026-0001.md"));
  try {
    assert.throws(() => readIgnoresDir(dir), /is not a file/);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("firedAdvisories counts warnings, and allows a yanked crate with no advisory", () => {
  const fired = firedAdvisories(
    report({
      ids: ["RUSTSEC-2026-0001"],
      warnings: { unmaintained: [{ advisory: { id: "RUSTSEC-2026-0002" } }], yanked: [{ advisory: null }] },
    }),
  );
  assert.deepEqual([...fired].sort(), ["RUSTSEC-2026-0001", "RUSTSEC-2026-0002"]);
});

test("firedAdvisories refuses a report that could not have seen what fires", () => {
  const cases = [
    [null, /vulnerabilities\.list/],
    [{}, /vulnerabilities\.list/],
    [report({ vulnerabilities: {} }), /vulnerabilities\.list/],
    [report({ database: { "advisory-count": 0 } }), /empty advisory database/],
    [report({ lockfile: { "dependency-count": 0 } }), /empty lockfile/],
    [report({ warnings: null }), /no warnings section/],
    [report({ settings: { ignore: ["RUSTSEC-2026-0001"] } }), /filtered by configuration/],
    [report({ settings: {} }), /filtered by configuration/],
    [report({ settings: { ignore: [], severity: "critical" } }), /filtered by configuration/],
    [report({ settings: { ignore: [], target_os: ["linux"] } }), /filtered by configuration/],
    [report({ settings: { ignore: [], target_arch: ["x86_64"] } }), /filtered by configuration/],
    [report({ vulnerabilities: { count: 3, list: [] } }), /counts 3 vulnerabilities but lists 0/],
    [report({ vulnerabilities: { list: [{}] } }), /no advisory id/],
    [report({ warnings: [] }), /no warnings section/],
    [report({ warnings: { unmaintained: [{}] } }), /unmaintained warning with no advisory id/],
  ];
  for (const [bad, want] of cases) assert.throws(() => firedAdvisories(bad), want);
});

test("findProblems: a date is expired only after its day, and non-firing ignores are stale", () => {
  const reviewDates = [{ date: "2026-11-04", ids: ["RUSTSEC-2026-0001"] }];
  const ignores = ["RUSTSEC-2026-0001", "RUSTSEC-2026-0002"];
  const fired = new Set(["RUSTSEC-2026-0001"]);
  const on = (today) => findProblems({ ignores, reviewDates, fired, today });
  assert.deepEqual(on("2026-11-04").expired, []);
  assert.equal(on("2026-11-05").expired.length, 1);
  assert.deepEqual(on("2026-11-04").stale, ["RUSTSEC-2026-0002"]);
});

const NONE = { expired: [], stale: [] };
const SOME = { expired: [], stale: ["RUSTSEC-2026-0002"] };

test("planAction covers each state of the single issue", () => {
  const body = renderBody(SOME);
  const issue = (over = {}) => ({ number: 7, title: ISSUE_TITLE, body, state: "OPEN", ...over });
  const kind = (problems, issues) => {
    const { kind: k, number } = planAction(problems, issues);
    return [k, number];
  };
  assert.deepEqual(kind(NONE, []), ["none", undefined]);
  assert.deepEqual(kind(NONE, [issue()]), ["close", 7]);
  assert.deepEqual(kind(NONE, [issue({ state: "CLOSED" })]), ["none", undefined]);
  assert.deepEqual(kind(SOME, []), ["create", undefined]);
  assert.deepEqual(kind(SOME, [issue()]), ["none", undefined]);
  assert.deepEqual(kind(SOME, [issue({ body: `${body}\r\n` })]), ["none", undefined]);
  assert.deepEqual(kind(SOME, [issue({ body: "old" })]), ["edit", 7]);
  assert.deepEqual(kind(SOME, [issue({ state: "CLOSED" })]), ["reopen", 7]);
});

test("planAction matches the exact title, picks the lowest owner and names duplicates", () => {
  const similar = { number: 1, title: `Re: ${ISSUE_TITLE}`, body: "", state: "OPEN" };
  assert.equal(planAction(NONE, [similar]).kind, "none");
  const dupes = [
    { number: 9, title: ISSUE_TITLE, body: "", state: "OPEN" },
    { number: 3, title: ISSUE_TITLE, body: "", state: "OPEN" },
  ];
  const plan = planAction(NONE, dupes);
  assert.equal(plan.number, 3);
  assert.deepEqual(plan.duplicates, [9]);
});

test("planAction: an open issue owns over a closed one, and a closed one is no duplicate", () => {
  const body = renderBody(SOME);
  const low = (state) => ({ number: 3, title: ISSUE_TITLE, body, state });
  const high = (state) => ({ number: 9, title: ISSUE_TITLE, body, state });
  // Nothing left: the open issue still closes, whatever its number.
  const closes = planAction(NONE, [low("CLOSED"), high("OPEN")]);
  assert.deepEqual([closes.kind, closes.number, closes.duplicates], ["close", 9, []]);
  // Problems, lower one open: nothing to do, and no warning about the closed one.
  const quiet = planAction(SOME, [low("OPEN"), high("CLOSED")]);
  assert.deepEqual([quiet.kind, quiet.duplicates], ["none", []]);
  // Problems, higher one open: it stays the owner, so nothing is reopened.
  const noReopen = planAction(SOME, [low("CLOSED"), high("OPEN")]);
  assert.deepEqual([noReopen.kind, noReopen.duplicates], ["none", []]);
  // All closed: the lowest comes back.
  const reopen = planAction(SOME, [low("CLOSED"), high("CLOSED")]);
  assert.deepEqual([reopen.kind, reopen.number], ["reopen", 3]);
});

test("renderBody names each problem", () => {
  const body = renderBody({
    expired: [{ date: "2026-11-04", ids: ["RUSTSEC-2026-0001", "RUSTSEC-2026-0002"] }],
    stale: ["RUSTSEC-2026-0003"],
  });
  assert.match(body, /- 2026-11-04: RUSTSEC-2026-0001, RUSTSEC-2026-0002/);
  assert.match(body, /- RUSTSEC-2026-0003/);
  assert.doesNotMatch(renderBody({ expired: [], stale: ["RUSTSEC-2026-0003"] }), /date passed/);
  assert.doesNotMatch(renderBody({ expired: [{ date: "2026-11-04", ids: [] }], stale: [] }), /no longer firing/);
});

// The reader has to keep working on the directory it exists to read. The count
// catches a date the parser lost. It cannot assert a minimum, since the last
// dated ignore may well go away one day.
test("the real .github/audit-ignores/ reads: every date is read and names an ignore", () => {
  const files = readIgnoresDir(realIgnoresDir);
  const { ignores, reviewDates } = readIgnores(files);
  const dated = files
    .filter((f) => f.name !== "README.md")
    .flatMap((f) => f.text.split("\n").filter((l) => /\bREVIEW BY\b/.test(l)));
  assert.equal(reviewDates.length, dated.length);
  for (const review of reviewDates) {
    for (const id of review.ids) assert.ok(ignores.includes(id), `${id} is not an ignore`);
  }
});

// ---- the CLI, against a stub gh ----

const STUB_GH = `#!/bin/sh
{
  printf 'ARGV %s\\n' "$*"
  printf 'REPO %s\\n' "$GH_REPO"
  case "$*" in *"--body-file -"*) printf 'STDIN '; cat; printf '\\n' ;; esac
} >> "$STUB_LOG"
if [ "$1 $2" = "issue list" ]; then
  [ -n "$STUB_FAIL_LIST" ] && exit 3
  cat "$STUB_ISSUES"
  exit 0
fi
[ -n "$STUB_FAIL_WRITE" ] && exit 4
exit 0
`;

const CLI_IGNORES = {
  "RUSTSEC-2026-0001.md": "x.\nREVIEW BY 2026-11-04.\n\nDrop when x.\n",
  "RUSTSEC-2026-0002.md": "y.\n\nDrop when y.\n",
};

// RUSTSEC-2026-0002 fires nowhere, so this input always has a stale ignore.
const FINDINGS = { expired: [], stale: ["RUSTSEC-2026-0002"] };

function cli({ issues = [], env = {}, reportText, today = "2026-11-02", extra = [], omit = [] } = {}) {
  const dir = mkdtempSync(join(tmpdir(), "audit-ignore-review-"));
  const bin = join(dir, "bin");
  mkdirSync(bin);
  const gh = join(bin, "gh");
  writeFileSync(gh, STUB_GH);
  chmodSync(gh, 0o755);
  const log = join(dir, "gh.log");
  const issuesFile = join(dir, "issues.json");
  writeFileSync(issuesFile, JSON.stringify(issues));
  const ignoresDir = join(dir, "audit-ignores");
  mkdirSync(ignoresDir);
  for (const [name, text] of Object.entries(CLI_IGNORES)) writeFileSync(join(ignoresDir, name), text);
  const reportFile = join(dir, "report.json");
  writeFileSync(reportFile, reportText ?? JSON.stringify(report({ ids: ["RUSTSEC-2026-0001"] })));
  const args = { "--audit-json": reportFile, "--repo": "o/r", "--ignores-dir": ignoresDir, "--today": today };
  const argv = Object.entries(args).filter(([k]) => !omit.includes(k)).flat();
  const res = spawnSync(process.execPath, [script, ...argv, ...extra], {
    encoding: "utf8",
    env: {
      ...process.env,
      PATH: `${bin}${delimiter}${process.env.PATH}`,
      STUB_LOG: log,
      STUB_ISSUES: issuesFile,
      ...env,
    },
  });
  const logText = existsSync(log) ? readFileSync(log, "utf8") : "";
  rmSync(dir, { recursive: true, force: true });
  return { ...res, log: logText };
}

const argvs = (log) => log.split("\n").filter((l) => l.startsWith("ARGV ")).map((l) => l.slice(5));
const writes = (log) => argvs(log).filter((a) => !a.startsWith("issue list"));

test("cli: lists with state, fields and exact-title search, pinned to the repo", () => {
  const { status, log } = cli({ reportText: JSON.stringify(report({ ids: ["RUSTSEC-2026-0001", "RUSTSEC-2026-0002"] })) });
  assert.equal(status, 0);
  const [list] = argvs(log);
  assert.match(list, /^issue list --state all --limit 100 /);
  assert.ok(list.includes(`--search "${ISSUE_TITLE}" in:title`), list);
  assert.ok(list.endsWith("--json number,title,body,state"), list);
  assert.match(log, /^REPO o\/r$/m);
  assert.deepEqual(writes(log), []);
});

test("cli: creates the issue with the body on stdin", () => {
  const { status, log, stdout } = cli();
  assert.equal(status, 0);
  assert.deepEqual(writes(log), [`issue create --title ${ISSUE_TITLE} --body-file -`]);
  assert.ok(log.includes(`STDIN ${renderBody(FINDINGS)}`), log);
  assert.match(stdout, /"action":"create"/);
});

test("cli: --dry-run prints the plan and writes nothing", () => {
  const { status, log, stdout } = cli({ extra: ["--dry-run"] });
  assert.equal(status, 0);
  assert.deepEqual(writes(log), []);
  assert.ok(stdout.includes(renderBody(FINDINGS)));
});

test("cli: edits a changed issue, and leaves an unchanged one alone", () => {
  const issue = { number: 7, title: ISSUE_TITLE, state: "OPEN" };
  const changed = cli({ issues: [{ ...issue, body: "old" }] });
  assert.equal(changed.status, 0);
  assert.deepEqual(writes(changed.log), ["issue edit 7 --body-file -"]);
  assert.ok(changed.log.includes(`STDIN ${renderBody(FINDINGS)}`), changed.log);
  const same = cli({ issues: [{ ...issue, body: renderBody(FINDINGS) }] });
  assert.equal(same.status, 0);
  assert.deepEqual(writes(same.log), []);
});

test("cli: reopens a closed issue, refreshing its body first", () => {
  const { status, log } = cli({ issues: [{ number: 7, title: ISSUE_TITLE, state: "CLOSED", body: "old" }] });
  assert.equal(status, 0);
  assert.deepEqual(writes(log), ["issue edit 7 --body-file -", "issue reopen 7"]);
  assert.ok(log.includes(`STDIN ${renderBody(FINDINGS)}`), log);
});

test("cli: closes the issue when nothing is left, and warns about a duplicate", () => {
  const issues = [
    { number: 7, title: ISSUE_TITLE, state: "OPEN", body: "old" },
    { number: 9, title: ISSUE_TITLE, state: "OPEN", body: "old" },
  ];
  const { status, log, stdout } = cli({
    issues,
    reportText: JSON.stringify(report({ ids: ["RUSTSEC-2026-0001", "RUSTSEC-2026-0002"] })),
  });
  assert.equal(status, 0);
  assert.deepEqual(writes(log), ["issue close 7 --comment Nothing left to review."]);
  assert.match(stdout, /::warning title=ci-audit-ignore-review::duplicate issues.*9/);
});

test("cli: a passed date reaches the issue", () => {
  const { status, log } = cli({ today: "2026-11-05" });
  assert.equal(status, 0);
  assert.ok(log.includes("- 2026-11-04: RUSTSEC-2026-0001"), log);
});

test("cli: a blind run exits 1 and never touches gh", () => {
  for (const reportText of ["", "not json", JSON.stringify({})]) {
    const { status, log, stdout } = cli({ reportText });
    assert.equal(status, 1, reportText);
    assert.equal(log, "", reportText);
    assert.match(stdout, /^::error title=ci-audit-ignore-review::/m);
  }
  assert.match(cli({ reportText: "" }).stdout, /did `cargo audit` fail\?/);
});

test("cli: the error annotation escapes workflow-command data", () => {
  const { status, stdout } = cli({ extra: ["--ignores-dir", "/nonexistent/100%done"] });
  assert.equal(status, 1);
  assert.match(stdout, /100%25done/);
});

test("cli: a failed gh call exits 1, and a failed list writes nothing", () => {
  const listFails = cli({ env: { STUB_FAIL_LIST: "1" } });
  assert.equal(listFails.status, 1);
  assert.deepEqual(writes(listFails.log), []);
  assert.equal(cli({ env: { STUB_FAIL_WRITE: "1" } }).status, 1);
});

test("cli: bad usage exits 2 without calling gh", () => {
  for (const run of [
    cli({ omit: ["--audit-json"] }),
    cli({ omit: ["--repo"] }),
    cli({ extra: ["--bogus"] }),
    cli({ today: "garbage" }),
    cli({ today: "2026-02-30" }),
  ]) {
    assert.equal(run.status, 2);
    assert.equal(run.log, "");
  }
});

// ---- the CI lint and the flags script, on fixture trees ----
// The lint must refuse what the reader above refuses, or a PR goes green and
// the nightly goes blind. Each case is one fixture tree; each must exit 1.

const lintScript = join(here, "..", "ci", "audit-ignores-test.sh");
const flagsScript = join(here, "..", "ci", "audit-ignore-flags.sh");
const GOOD = "x.\nREVIEW BY 2026-11-04.\n\nDrop when x.\n";

function lint({ ignores = { "RUSTSEC-2026-0001.md": GOOD }, workflows = { "ci.yml": "run: cargo audit $flags\n" } } = {}) {
  const dir = mkdtempSync(join(tmpdir(), "audit-ignores-lint-"));
  const ign = join(dir, "ignores");
  const wf = join(dir, "workflows");
  mkdirSync(ign);
  mkdirSync(wf);
  for (const [name, text] of Object.entries(ignores)) writeFileSync(join(ign, name), text);
  for (const [name, text] of Object.entries(workflows)) writeFileSync(join(wf, name), text);
  const lintRun = spawnSync("bash", [lintScript, ign, wf], { encoding: "utf8" });
  const flagsRun = spawnSync("bash", [flagsScript, ign], { encoding: "utf8" });
  rmSync(dir, { recursive: true, force: true });
  return { status: lintRun.status, stderr: lintRun.stderr, flags: flagsRun };
}

test("lint: a clean tree passes, and the flags script prints one pair per file", () => {
  const run = lint({ ignores: { "README.md": "notes", "RUSTSEC-2026-0001.md": GOOD, "RUSTSEC-2026-0002.md": "y.\n\nDrop when y.\n" } });
  assert.equal(run.status, 0, run.stderr);
  assert.equal(run.flags.status, 0);
  assert.equal(run.flags.stdout.trim(), "--ignore RUSTSEC-2026-0001 --ignore RUSTSEC-2026-0002");
  const empty = lint({ ignores: {} });
  assert.equal(empty.status, 0, empty.stderr);
  assert.equal(empty.flags.stdout.trim(), "");
});

test("lint: refuses each malformed tree the reader refuses", () => {
  const cases = [
    [{ ignores: { "RUSTSEC-26-1.md": GOOD } }, /not a <RUSTSEC-YYYY-NNNN>\.md file/],
    [{ ignores: { ".gitkeep": "" } }, /not a <RUSTSEC-YYYY-NNNN>\.md file/],
    [{ ignores: { "RUSTSEC-2026-0001.md": "x.\n" } }, /Drop when/],
    [{ ignores: { "RUSTSEC-2026-0001.md": "REVIEW BY 2026-02-30\nDrop when x.\n" } }, /not a real date/],
    [{ ignores: { "RUSTSEC-2026-0001.md": "Review by 2026-11-04\nDrop when x.\n" } }, /REVIEW BY YYYY-MM-DD/],
    [{ ignores: { "RUSTSEC-2026-0001.md": "see REVIEW\nBY 2026-01-01\nDrop when x.\n" } }, /on one line/],
    [{ workflows: { "ci.yml": "run: cargo audit $flags --ignore RUSTSEC-2023-0071\n" } }, /inline/],
    [{ workflows: { "other.yaml": "run: cargo audit --ignore=RUSTSEC-2023-0071\n" } }, /inline/],
  ];
  for (const [fixture, want] of cases) {
    const run = lint(fixture);
    assert.equal(run.status, 1, JSON.stringify(fixture));
    assert.match(run.stderr, want, JSON.stringify(fixture));
  }
  assert.equal(lint({ ignores: { ".gitkeep": "" } }).flags.status, 1, "the flags script refuses a dotfile too");
});

test("lint and reader agree on prose that is not a date", () => {
  const text = "a preview by Bob.\nRe-assess by 2020-01-01.\nwrapped preview\nby Bob.\n\nDrop when x.\n";
  assert.equal(lint({ ignores: { "RUSTSEC-2026-0001.md": text } }).status, 0);
  assert.deepEqual(readIgnores([{ name: "RUSTSEC-2026-0001.md", text }]).reviewDates, []);
});

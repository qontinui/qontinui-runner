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
  parseIgnores,
  parseReviewDates,
  planAction,
  renderBody,
} from "../ci-audit-ignore-review.mjs";

const here = dirname(fileURLToPath(import.meta.url));
const script = join(here, "..", "ci-audit-ignore-review.mjs");
const realCiYml = join(here, "..", "..", ".github", "workflows", "ci.yml");

const CI = `
      - name: audit
        # --ignore RUSTSEC-2026-0001: one advisory.
        # REVIEW BY 2026-11-04 (or sooner).
        #
        # --ignore RUSTSEC-2026-0002 + RUSTSEC-2026-0003
        # + RUSTSEC-2026-0004: a block whose heading wraps.
        # Tracking: #1. REVIEW BY 2026-12-02. Re-assess if it changes.
        #
        # --ignore RUSTSEC-2026-0005: no date on this one.
        run: |
          # cargo audit --file Cargo.lock --ignore RUSTSEC-1999-0001
          cargo audit --file Cargo.lock --ignore RUSTSEC-2026-0001 --ignore RUSTSEC-2026-0002 --ignore RUSTSEC-2026-0003
`;

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

test("parseIgnores reads the live command line, not a commented-out one", () => {
  assert.deepEqual(parseIgnores(CI), ["RUSTSEC-2026-0001", "RUSTSEC-2026-0002", "RUSTSEC-2026-0003"]);
});

test("parseIgnores accepts a command with no ignores left", () => {
  assert.deepEqual(parseIgnores("  cargo audit --file Cargo.lock\n"), []);
});

test("parseIgnores takes only the real command", () => {
  const ci = "  cargo audit-x --ignore RUSTSEC-1999-0001\n  cargo audit --ignore RUSTSEC-2026-0001\n";
  assert.deepEqual(parseIgnores(ci), ["RUSTSEC-2026-0001"]);
});

test("parseIgnores refuses anything it cannot read completely", () => {
  const cases = [
    ["run: cargo build\n", /found 0/],
    ["  cargo audit --ignore RUSTSEC-2026-0001\n  cargo audit --ignore RUSTSEC-2026-0002\n", /found 2/],
    ["  cargo audit --ignore RUSTSEC-2026-0001 \\\n    --ignore RUSTSEC-2026-0002\n", /continues onto the next line/],
    ["  cargo audit --ignore RUSTSEC-2026-0001 \\\n    $MORE\n", /cannot read/],
    ["  cargo audit --ignore RUSTSEC-2026-0001 --ignore=RUSTSEC-2026-0002\n", /cannot read/],
    ["  cargo audit --ignore RUSTSEC-2026-0001 $MORE\n", /cannot read/],
    ["  cargo audit --ignore RUSTSEC-2026-0001 # --ignore RUSTSEC-2026-0002\n", /cannot read/],
    ["  cargo audit --ignore RUSTSEC-2026-0001 || true\n", /cannot read/],
    ["  cargo audit --ignore RUSTSEC-2026-0001 --ignore GHSA-aaaa-bbbb-cccc\n", /not a RUSTSEC id/],
    ["  cargo audit --ignore RUSTSEC-2026-12345\n", /not a RUSTSEC id/],
    ["  cargo audit --ignore\n", /not a RUSTSEC id/],
    ["  cargo audit --file Cargo.lock\n  --ignore RUSTSEC-2026-0001\n", /continues onto the next line/],
    ["  cargo audit --file src-tauri/Cargo.lock\n", /cannot read/],
    ["  cargo audit --file --ignore --ignore RUSTSEC-2026-0001\n", /cannot read/],
  ];
  for (const [ci, want] of cases) assert.throws(() => parseIgnores(ci), want, ci);
});

test("parseReviewDates ties each date to its block, across a wrapped heading", () => {
  assert.deepEqual(parseReviewDates(CI), [
    { date: "2026-11-04", ids: ["RUSTSEC-2026-0001"] },
    { date: "2026-12-02", ids: ["RUSTSEC-2026-0002", "RUSTSEC-2026-0003", "RUSTSEC-2026-0004"] },
  ]);
});

test("parseReviewDates refuses a reworded date instead of reading it as no date", () => {
  const wrap = (text) => `# --ignore RUSTSEC-2026-0001: x.\n${text}\n  cargo audit --ignore RUSTSEC-2026-0001\n`;
  for (const text of [
    "# Review by 2026-11-04",
    "# REVIEW BY: 2026-11-04",
    "# REVIEW  BY 2026-11-04",
    "# REVIEW BY 2026/11/04",
    "# REVIEW-BY 2026-11-04",
    "# REVIEW_BY 2026-11-04",
    "# REVIEWBY 2026-11-04",
    "# REVIEW BY",
    "# REVIEW BY\n# 2026-11-04",
    "# text REVIEW\n# BY 2026-11-04",
    "# REVIEW BY 2026-13-45",
    "# REVIEW BY 2026-02-30",
    "# REVIEW BY 2026-11-04, or review by then",
  ]) {
    assert.throws(() => parseReviewDates(wrap(text)), /REVIEW BY YYYY-MM-DD/, text);
  }
});

test("parseReviewDates refuses a date with no heading above it", () => {
  for (const ci of [
    "# REVIEW BY 2026-11-04\n# --ignore RUSTSEC-2026-0001: x.\n  cargo audit --ignore RUSTSEC-2026-0001\n",
    "# Ignoring RUSTSEC-2026-0001.\n# REVIEW BY 2026-11-04\n  cargo audit --ignore RUSTSEC-2026-0001\n",
  ]) {
    assert.throws(() => parseReviewDates(ci), /under a `# --ignore` heading/, ci);
  }
});

test("parseReviewDates takes ids from the heading only, and needs a word boundary", () => {
  const ids = (block) => parseReviewDates(`${block}\n  cargo audit --ignore RUSTSEC-2026-0001\n`).map((r) => r.ids);
  assert.deepEqual(ids("# --ignore RUSTSEC-2026-0001: see RUSTSEC-2026-0002.\n# REVIEW BY 2026-11-04."), [["RUSTSEC-2026-0001"]]);
  assert.deepEqual(
    ids("# --ignore RUSTSEC-2026-0001: x.\n# quoting # --ignore RUSTSEC-2020-0001 here\n# REVIEW BY 2026-11-04."),
    [["RUSTSEC-2026-0001"]],
  );
  assert.deepEqual(ids("# --ignore RUSTSEC-2026-0001: x.\n# a preview by the team."), []);
  assert.deepEqual(ids("# --ignore RUSTSEC-2026-0001: x.\n# PREVIEW BY 2020-01-01."), []);
  assert.throws(() => ids("# --ignore RUSTSEC-2026-0001: x.\n# REVIEW BY 2020-01-01-99."), /REVIEW BY YYYY-MM-DD/);
});

test("parseReviewDates treats other wording as prose", () => {
  const ci = "# --ignore RUSTSEC-2026-0001: x. Disclosed 2026-05-13.\n# Re-assess by 2020-01-01.\n  cargo audit --ignore RUSTSEC-2026-0001\n";
  assert.deepEqual(parseReviewDates(ci), []);
});

test("parseReviewDates stops at the step boundary, however the step starts", () => {
  const ci = "# REVIEW BY 2020-01-01\n- run: echo\n- id: audit\n  cargo audit --ignore RUSTSEC-2026-0001\n";
  assert.deepEqual(parseReviewDates(ci), []);
});

test("parseReviewDates only reads the comments of the step holding the command", () => {
  const ci = [
    "# an earlier job, up for review by a human",
    "- name: audit",
    "# --ignore RUSTSEC-2026-0001: x.",
    '  run: echo "REVIEW BY 2020-01-01"',
    "  cargo audit --ignore RUSTSEC-2026-0001",
    "# a later job, review by anyone, REVIEW BY 2020-01-01",
  ].join("\n");
  assert.deepEqual(parseReviewDates(ci), []);
  assert.throws(() => parseReviewDates("# REVIEW BY 2026-11-04\n"), /no `cargo audit/);
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

// The parser has to keep working on the file it exists to read. The count catches
// a date the parser lost. It cannot assert a minimum, since the last dated ignore
// may well go away one day.
test("the real ci.yml parses: every date is read and names advisories on the command", () => {
  const ci = readFileSync(realCiYml, "utf8");
  const ignores = parseIgnores(ci);
  const reviews = parseReviewDates(ci);
  const dated = ci.split("\n").filter((l) => /^\s*#.*\bREVIEW BY\b/.test(l));
  assert.equal(reviews.length, dated.length);
  for (const review of reviews) {
    for (const id of review.ids) assert.ok(ignores.includes(id), `${id} is not on the command`);
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

const CLI_CI = `# --ignore RUSTSEC-2026-0001: x.
# REVIEW BY 2026-11-04.
  cargo audit --file Cargo.lock --ignore RUSTSEC-2026-0001 --ignore RUSTSEC-2026-0002
`;

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
  const ciYml = join(dir, "ci.yml");
  writeFileSync(ciYml, CLI_CI);
  const reportFile = join(dir, "report.json");
  writeFileSync(reportFile, reportText ?? JSON.stringify(report({ ids: ["RUSTSEC-2026-0001"] })));
  const args = { "--audit-json": reportFile, "--repo": "o/r", "--ci-yml": ciYml, "--today": today };
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
  const { status, stdout } = cli({ extra: ["--ci-yml", "/nonexistent/100%done"] });
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

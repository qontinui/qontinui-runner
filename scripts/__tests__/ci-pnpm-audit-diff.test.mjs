#!/usr/bin/env node
// Unit tests for ci-pnpm-audit-diff.mjs. All I/O is injected, so no test
// touches git or the npm registry.
//
// Run with:
//   node --test scripts/__tests__/ci-pnpm-audit-diff.test.mjs

import test from "node:test";
import assert from "node:assert/strict";

import {
  AuditUnreadable,
  GateUsageError,
  auditConfigDrift,
  diffAdvisories,
  npmrcAuditKeys,
  parseAudit,
  resolveBase,
  runGate,
} from "../ci-pnpm-audit-diff.mjs";

function advisory(id, module, severity, extra = {}) {
  return {
    id,
    module_name: module,
    severity,
    github_advisory_id: `GHSA-${id}`,
    url: `https://github.com/advisories/GHSA-${id}`,
    title: `${module} issue`,
    vulnerable_versions: "<2.0.0",
    patched_versions: ">=2.0.0",
    findings: [{ version: "1.0.0", paths: [`.>${module}`] }],
    ...extra,
  };
}

function report(...advisories) {
  return JSON.stringify({
    actions: [],
    advisories: Object.fromEntries(advisories.map((a) => [String(a.id), a])),
    muted: [],
    metadata: { vulnerabilities: {} },
  });
}

/** A fake repo: rev -> { path: content }, plus an audit stub keyed by lock content. */
function harness({ base, head, audits }) {
  const revs = { BASE: base, HEAD: head };
  const calls = [];
  return {
    calls,
    deps: {
      base: "BASE",
      head: "HEAD",
      resolveRev: (rev) => (revs[rev] ? `sha-${rev}` : null),
      readFile: (rev, path) => revs[rev][path] ?? null,
      audit: (files) => {
        calls.push(files);
        const out = audits[files["pnpm-lock.yaml"]];
        return typeof out === "function" ? out(calls.length) : { stdout: out };
      },
    },
  };
}

const PKG = '{"name":"x"}';

test("unchanged lock and package.json: exit 0 and no audit (no network)", () => {
  const h = harness({
    base: { "package.json": PKG, "pnpm-lock.yaml": "lockA" },
    head: { "package.json": PKG, "pnpm-lock.yaml": "lockA" },
    audits: {},
  });
  const r = runGate(h.deps);
  assert.equal(r.exitCode, 0);
  assert.equal(r.verdict, "unchanged");
  assert.equal(h.calls.length, 0);
});

test("package.json change alone is a change and is audited", () => {
  const h = harness({
    base: { "package.json": PKG, "pnpm-lock.yaml": "lockA" },
    head: { "package.json": '{"name":"y"}', "pnpm-lock.yaml": "lockA" },
    audits: { lockA: report() },
  });
  assert.equal(runGate(h.deps).verdict, "clean");
  assert.equal(h.calls.length, 2);
});

test("new moderate advisory at head fails", () => {
  const h = harness({
    base: { "package.json": PKG, "pnpm-lock.yaml": "lockA" },
    head: { "package.json": PKG, "pnpm-lock.yaml": "lockB" },
    audits: { lockA: report(), lockB: report(advisory(1, "left-pad", "moderate")) },
  });
  const r = runGate(h.deps);
  assert.equal(r.exitCode, 1);
  assert.equal(r.verdict, "introduced");
  assert.deepEqual(r.diff.blocking.map((a) => a.module), ["left-pad"]);
  assert.match(r.lines.join("\n"), /moderate +left-pad +GHSA-1 +vulnerable <2\.0\.0 +patched >=2\.0\.0 +path \.>left-pad/);
});

test("same advisory at base and head passes (not introduced by this change)", () => {
  const same = report(advisory(7, "dompurify", "critical"));
  const h = harness({
    base: { "package.json": PKG, "pnpm-lock.yaml": "lockA" },
    head: { "package.json": PKG, "pnpm-lock.yaml": "lockB" },
    audits: { lockA: same, lockB: same },
  });
  const r = runGate(h.deps);
  assert.equal(r.exitCode, 0);
  assert.equal(r.verdict, "clean");
});

test("advisory fixed by the change passes and is reported", () => {
  const h = harness({
    base: { "package.json": PKG, "pnpm-lock.yaml": "lockA" },
    head: { "package.json": PKG, "pnpm-lock.yaml": "lockB" },
    audits: { lockA: report(advisory(3, "vite", "high")), lockB: report() },
  });
  const r = runGate(h.deps);
  assert.equal(r.exitCode, 0);
  assert.deepEqual(r.diff.fixed.map((a) => a.advisoryId), ["GHSA-3"]);
  assert.match(r.lines.join("\n"), /Fixed by this change: 1\n +high +vite/);
});

test("only a new low advisory passes with a warning", () => {
  const h = harness({
    base: { "package.json": PKG, "pnpm-lock.yaml": "lockA" },
    head: { "package.json": PKG, "pnpm-lock.yaml": "lockB" },
    audits: { lockA: report(), lockB: report(advisory(4, "katex", "low")) },
  });
  const r = runGate(h.deps);
  assert.equal(r.exitCode, 0);
  assert.equal(r.diff.warnings.length, 1);
  assert.ok(r.annotations.some((a) => a.startsWith("::warning") && a.includes("katex")));
});

test("unparseable audit output is UNKNOWN and fails, after one retry", () => {
  const h = harness({
    base: { "package.json": PKG, "pnpm-lock.yaml": "lockA" },
    head: { "package.json": PKG, "pnpm-lock.yaml": "lockB" },
    audits: { lockA: report(), lockB: "ERR_PNPM_AUDIT_BAD_RESPONSE The audit endpoint responded with 503" },
  });
  const r = runGate(h.deps);
  assert.equal(r.exitCode, 1);
  assert.equal(r.verdict, "unknown");
  assert.match(r.lines[0], /could not be performed/);
  assert.equal(h.calls.length, 3); // base once, head twice
});

test("an error-shaped JSON report and a spawn failure are UNKNOWN too", () => {
  const err = harness({
    base: { "package.json": PKG, "pnpm-lock.yaml": "lockA" },
    head: { "package.json": PKG, "pnpm-lock.yaml": "lockB" },
    audits: { lockA: JSON.stringify({ error: { code: "ERR_PNPM_AUDIT_BAD_RESPONSE", message: "503" } }), lockB: report() },
  });
  assert.equal(runGate(err.deps).verdict, "unknown");
  const spawn = harness({
    base: { "package.json": PKG, "pnpm-lock.yaml": "lockA" },
    head: { "package.json": PKG, "pnpm-lock.yaml": "lockB" },
    audits: { lockA: () => ({ stdout: undefined, error: new Error("spawn pnpm ENOENT") }), lockB: report() },
  });
  assert.equal(runGate(spawn.deps).verdict, "unknown");
});

test("a transient failure that recovers on retry is judged normally", () => {
  const h = harness({
    base: { "package.json": PKG, "pnpm-lock.yaml": "lockA" },
    head: { "package.json": PKG, "pnpm-lock.yaml": "lockB" },
    audits: { lockA: (n) => ({ stdout: n === 1 ? "" : report() }), lockB: report() },
  });
  assert.equal(runGate(h.deps).verdict, "clean");
});

test("parseAudit refuses non-reports instead of reading them as empty", () => {
  for (const bad of ["", "null", "[]", '{"metadata":{}}', '{"advisories":{}}']) {
    assert.throws(() => parseAudit(bad), AuditUnreadable, bad);
  }
  assert.equal(parseAudit(report()).size, 0);
});

test("identity is (package, advisory id): same GHSA on another package is new", () => {
  const base = parseAudit(report(advisory(9, "a", "high")));
  const head = parseAudit(report(advisory(9, "b", "high", { id: 10, github_advisory_id: "GHSA-9" })));
  const d = diffAdvisories(base, head);
  assert.equal(d.blocking.length, 1);
  assert.equal(d.fixed.length, 1);
});

// ------------------------------------------------------------ added cases

const changed = (audits) =>
  harness({
    base: { "package.json": PKG, "pnpm-lock.yaml": "lockA" },
    head: { "package.json": PKG, "pnpm-lock.yaml": "lockB" },
    audits,
  });

test("an unresolvable revision is a usage error (exit 2), not a verdict", () => {
  const h = changed({});
  h.deps.base = "NOPE";
  assert.throws(() => runGate(h.deps), GateUsageError);
  assert.equal(h.calls.length, 0);
});

test("pnpm-lock.yaml deleted at head fails", () => {
  const h = harness({
    base: { "package.json": PKG, "pnpm-lock.yaml": "lockA" },
    head: { "package.json": PKG },
    audits: {},
  });
  const r = runGate(h.deps);
  assert.equal(r.exitCode, 1);
  assert.equal(r.verdict, "lockfile-removed");
  assert.equal(h.calls.length, 0);
});

test("no lockfile on either side passes with nothing to audit", () => {
  const h = harness({ base: { "package.json": PKG }, head: { "package.json": '{"name":"y"}' }, audits: {} });
  const r = runGate(h.deps);
  assert.equal(r.exitCode, 0);
  assert.equal(r.verdict, "no-lockfile");
});

test("pnpm-lock.yaml added at head: every head advisory is introduced, base not audited", () => {
  const h = harness({
    base: { "package.json": PKG },
    head: { "package.json": PKG, "pnpm-lock.yaml": "lockB" },
    audits: { lockB: report(advisory(5, "vite", "moderate")) },
  });
  const r = runGate(h.deps);
  assert.equal(r.exitCode, 1);
  assert.equal(h.calls.length, 1);
});

test("an unrecognised or missing severity blocks rather than warns", () => {
  for (const severity of ["severe", undefined, ""]) {
    const h = changed({ lockA: report(), lockB: report(advisory(6, "x", severity)) });
    const r = runGate(h.deps);
    assert.equal(r.exitCode, 1, String(severity));
    assert.equal(r.diff.warnings.length, 0);
  }
});

test("duplicate keys merge their paths and versions", () => {
  const m = parseAudit(
    report(
      advisory(1, "ws", "high", { findings: [{ version: "8.0.0", paths: [".>a>ws"] }] }),
      advisory(2, "ws", "high", { github_advisory_id: "GHSA-1", findings: [{ version: "8.1.0", paths: [".>b>ws"] }] }),
    ),
  );
  assert.equal(m.size, 1);
  const [a] = m.values();
  assert.deepEqual(a.paths, [".>a>ws", ".>b>ws"]);
  assert.deepEqual([...a.versions].sort(), ["8.0.0", "8.1.0"]);
});

test("a new vulnerable version under an existing advisory warns, does not block", () => {
  const h = changed({
    lockA: report(advisory(8, "ws", "high", { findings: [{ version: "8.0.0", paths: [".>ws"] }] })),
    lockB: report(
      advisory(8, "ws", "high", {
        findings: [
          { version: "8.0.0", paths: [".>ws"] },
          { version: "8.2.0", paths: [".>a>ws"] },
        ],
      }),
    ),
  });
  const r = runGate(h.deps);
  assert.equal(r.exitCode, 0);
  assert.deepEqual(r.diff.newVersions.map((n) => n.added), [["8.2.0"]]);
  assert.ok(r.annotations.some((a) => a.startsWith("::warning") && a.includes("8.2.0")));
});

test("pnpm.auditConfig drift fails before any audit", () => {
  const h = harness({
    base: { "package.json": PKG, "pnpm-lock.yaml": "lockA" },
    head: {
      "package.json": JSON.stringify({ name: "x", pnpm: { auditConfig: { ignoreGhsas: ["GHSA-1"] } } }),
      "pnpm-lock.yaml": "lockB",
    },
    audits: {},
  });
  const r = runGate(h.deps);
  assert.equal(r.exitCode, 1);
  assert.equal(r.verdict, "config-drift");
  assert.equal(h.calls.length, 0);
});

test(".npmrc registry drift is a warning, not a failure (the audit never reads .npmrc)", () => {
  const base = { "package.json": PKG, "pnpm-lock.yaml": "lockA", ".npmrc": "legacy-peer-deps=true\n" };
  const evil = harness({ base, head: { ...base, ".npmrc": "legacy-peer-deps=true\nregistry = https://evil.example/\n" }, audits: {} });
  const r = runGate(evil.deps);
  assert.equal(r.exitCode, 0);
  assert.equal(r.verdict, "unchanged");
  assert.ok(r.annotations.some((a) => a.startsWith("::warning") && a.includes("registry (added)")));
  const benign = harness({ base, head: { ...base, ".npmrc": "# comment\nlegacy-peer-deps=true\npublic-hoist-pattern[]=x\n" }, audits: {} });
  const rb = runGate(benign.deps);
  assert.equal(rb.verdict, "unchanged");
  assert.equal(rb.annotations.length, 0);
});

test(".npmrc values never appear in any output; credential keys are masked", () => {
  const SECRET = "npm_SuperSecretToken123";
  const HOST = "private.example.internal";
  const base = { "package.json": PKG, "pnpm-lock.yaml": "lockA", ".npmrc": "registry=https://registry.npmjs.org/\n" };
  const head = {
    "package.json": PKG,
    "pnpm-lock.yaml": "lockB",
    ".npmrc": `registry=https://${HOST}/\n//${HOST}/:_authToken=${SECRET}\n_password=${SECRET}\n`,
  };
  const h = harness({ base, head, audits: { lockA: report(), lockB: report() } });
  const r = runGate(h.deps);
  const all = [...r.lines, ...r.annotations].join("\n");
  assert.ok(!all.includes(SECRET), all);
  assert.ok(!all.includes(HOST), all);
  assert.match(all, /<credential key> \(added\)/);
  assert.match(all, /registry \(changed\)/);
});

test("only package.json + pnpm-lock.yaml reach the audit (never the change's .npmrc)", () => {
  const rc = "registry=https://registry.npmjs.org/\n";
  const h = harness({
    base: { "package.json": PKG, "pnpm-lock.yaml": "lockA", ".npmrc": rc },
    head: { "package.json": PKG, "pnpm-lock.yaml": "lockB", ".npmrc": rc },
    audits: { lockA: report(), lockB: report() },
  });
  runGate(h.deps);
  assert.equal(h.calls.length, 2);
  for (const files of h.calls) assert.deepEqual(Object.keys(files).sort(), ["package.json", "pnpm-lock.yaml"]);
});

test("npmrcAuditKeys and auditConfigDrift normalise whitespace and order", () => {
  assert.deepEqual(
    [...npmrcAuditKeys("b=1\n@s:registry = https://r/\naudit=false\n")],
    [
      ["@s:registry", "https://r/"],
      ["audit", "false"],
    ],
  );
  assert.deepEqual(
    auditConfigDrift({ "package.json": PKG, ".npmrc": "audit=false\nregistry=a" }, { "package.json": PKG, ".npmrc": "registry=a\naudit = false" }),
    { failures: [], warnings: [] },
  );
});

// ------------------------------------------------------------ resolveBase

/**
 * A scripted fake git. HEAD = "H", HEAD^ = "C". `mergeBases` maps the other
 * side of `git merge-base HEAD <x>` to its answer before / after an unshallow;
 * `ancestors` lists shas that are ancestors of HEAD before / after it.
 */
function fakeGit({ shallow = false, mergeBases = {}, ancestors = {}, unfetchable = [] }) {
  const log = [];
  let full = !shallow;
  const git = {
    revParse: (rev) => ({ HEAD: "H", "HEAD^": "C" })[rev] ?? null,
    fetch: (refspecs, { depth }) => {
      log.push(`fetch ${depth} ${refspecs.join(" ")}`);
      if (depth === "unshallow") full = true;
      return !refspecs.some((r) => unfetchable.includes(r));
    },
    isShallow: () => !full,
    mergeBase: (_a, b) => {
      const m = mergeBases[b];
      if (!m) return null;
      return full ? m.full ?? null : m.shallow ?? null;
    },
    isAncestor: (a) => {
      const x = ancestors[a];
      return Boolean(x && (full ? x.full : x.shallow));
    },
  };
  return { git, log };
}

const MAIN = "refs/remotes/origin/main";

test("resolveBase: merge-candidate push with all-zero before uses merge-base with main, not HEAD^", () => {
  const f = fakeGit({ shallow: true, mergeBases: { [MAIN]: { shallow: null, full: "M1" } } });
  const r = resolveBase({ eventName: "push", ref: "refs/heads/merge-candidate/123", before: "0000000000" }, f.git);
  assert.equal(r.base, "M1");
  assert.ok(f.log.some((l) => l.startsWith("fetch unshallow")));
});

test("resolveBase: a non-trunk push ignores even a usable before", () => {
  const f = fakeGit({ mergeBases: { [MAIN]: { full: "M1" } }, ancestors: { C: { full: true } } });
  const r = resolveBase({ eventName: "push", ref: "refs/heads/merge-candidate-batch/9", before: "C" }, f.git);
  assert.equal(r.base, "M1");
});

test("resolveBase: push to main with an ancestor before uses before, unshallowing to prove ancestry", () => {
  const f = fakeGit({ shallow: true, ancestors: { B0: { shallow: false, full: true } } });
  const r = resolveBase({ eventName: "push", ref: "refs/heads/main", before: "B0" }, f.git);
  assert.equal(r.base, "B0");
  assert.ok(f.log.some((l) => l.startsWith("fetch unshallow")));
});

test("resolveBase: push to develop with an ancestor before uses before", () => {
  const f = fakeGit({ ancestors: { B0: { full: true } } });
  assert.equal(resolveBase({ eventName: "push", ref: "refs/heads/develop", before: "B0" }, f.git).base, "B0");
});

test("resolveBase: a force-push before on main (not an ancestor) does not become BASE", () => {
  const f = fakeGit({ mergeBases: { [MAIN]: { full: "M1" } } });
  const r = resolveBase({ eventName: "push", ref: "refs/heads/main", before: "X" }, f.git);
  assert.equal(r.base, "M1");
});

test("resolveBase: HEAD^ only as a loud last resort (merge-base with main is HEAD itself)", () => {
  const f = fakeGit({ mergeBases: { [MAIN]: { full: "H" } } });
  const lines = [];
  const r = resolveBase({ eventName: "push", ref: "refs/heads/main", before: "0000" }, f.git, (l) => lines.push(l));
  assert.equal(r.base, "C");
  assert.match(r.how, /LAST RESORT/);
  assert.ok(lines.some((l) => l.startsWith("::warning") && l.includes("HEAD^")));
});

test("resolveBase: unfetchable before and unfetchable main still reach HEAD^ loudly", () => {
  const f = fakeGit({ unfetchable: ["B0", "+refs/heads/main:refs/remotes/origin/main"] });
  const r = resolveBase({ eventName: "push", ref: "refs/heads/main", before: "B0" }, f.git);
  assert.equal(r.base, "C");
});

test("resolveBase: pull_request uses the merge-base with the PR base, unshallowing when needed", () => {
  const f = fakeGit({ shallow: true, mergeBases: { PB: { shallow: null, full: "M1" } } });
  const r = resolveBase({ eventName: "pull_request", prBaseSha: "PB" }, f.git);
  assert.equal(r.base, "M1");
  assert.ok(f.log.some((l) => l.startsWith("fetch unshallow")));
});

test("resolveBase: pull_request with no merge-base, or no base sha, is a usage error", () => {
  assert.throws(() => resolveBase({ eventName: "pull_request", prBaseSha: "Z" }, fakeGit({}).git), GateUsageError);
  assert.throws(() => resolveBase({ eventName: "pull_request" }, fakeGit({}).git), GateUsageError);
});

test("resolveBase: HEAD^ missing from a shallow clone is fetched by unshallowing, not given up on", () => {
  const f = fakeGit({ shallow: true, mergeBases: { [MAIN]: { shallow: "H", full: "H" } } });
  const revParse = f.git.revParse;
  f.git.revParse = (rev) => (rev === "HEAD^" && f.git.isShallow() ? null : revParse(rev));
  const r = resolveBase({ eventName: "push", ref: "refs/heads/main", before: "0000" }, f.git);
  assert.equal(r.base, "C");
  assert.ok(f.log.some((l) => l.startsWith("fetch unshallow")));
});

test("resolveBase: a non-trunk push never falls back to HEAD^ — unfetchable main is UNKNOWN", () => {
  const f = fakeGit({ unfetchable: ["+refs/heads/main:refs/remotes/origin/main"] });
  assert.throws(
    () => resolveBase({ eventName: "push", ref: "refs/heads/merge-candidate/1", before: "0000" }, f.git),
    (e) => e instanceof GateUsageError && /UNKNOWN BASE/.test(e.message) && /fetch origin\/main/.test(e.message),
  );
});

test("resolveBase: a non-trunk push with no merge-base even after unshallow is UNKNOWN", () => {
  const f = fakeGit({ shallow: true, mergeBases: { [MAIN]: { shallow: null, full: null } } });
  assert.throws(
    () => resolveBase({ eventName: "push", ref: "refs/heads/merge-candidate-spec/2", before: "0000" }, f.git),
    (e) => e instanceof GateUsageError && /no merge-base/.test(e.message),
  );
  assert.ok(f.log.some((l) => l.startsWith("fetch unshallow")));
});

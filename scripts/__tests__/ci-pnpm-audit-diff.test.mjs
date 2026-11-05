#!/usr/bin/env node
// Unit tests for ci-pnpm-audit-diff.mjs. All I/O is injected, so no test
// touches git or the npm registry.
//
// Run with:
//   node --test scripts/__tests__/ci-pnpm-audit-diff.test.mjs

import test from "node:test";
import assert from "node:assert/strict";

import { AuditUnreadable, diffAdvisories, parseAudit, runGate } from "../ci-pnpm-audit-diff.mjs";

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

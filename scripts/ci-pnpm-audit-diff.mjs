#!/usr/bin/env node
/**
 * ci-pnpm-audit-diff.mjs — fail ci.yml's `security` job only on npm advisories
 * that a change INTRODUCES to the production dependency tree.
 *
 * Why base-vs-head and not a plain blocking `pnpm audit`: a plain audit judges
 * the tree against TODAY's advisory database, so the morning a new advisory is
 * published every open PR goes red with no change in its tree — the failure
 * PR #2501 hit for Cargo.lock. Comparing the same audit at BASE and at HEAD
 * isolates what the change itself did. (Before this script the step was
 * `pnpm audit --audit-level=moderate || true`, which could never fail.)
 *
 * What it does:
 *   1. Reads package.json + pnpm-lock.yaml at BASE and at HEAD (`git show`).
 *      If both files are byte-identical, the tree is unchanged: exit 0 with NO
 *      network call.
 *   2. Otherwise materialises each side into a temp dir and runs
 *      `pnpm audit --prod --json` there. pnpm audit needs only the lockfile and
 *      package.json — no install. Dev-only tooling is out of scope (--prod).
 *   3. An advisory is the pair (package name, GHSA id — else URL, else npm id).
 *      A pair at HEAD and not at BASE is INTRODUCED; at BASE and not at HEAD it
 *      is FIXED. Introduced moderate/high/critical fail; introduced low (and
 *      info) are warnings; fixed ones are reported.
 *   4. An audit that cannot run, or whose JSON is not an audit report, is
 *      UNKNOWN — never clean. Each audit is tried twice, then the step fails
 *      saying the audit could not be performed. pnpm audit exits non-zero
 *      whenever vulnerabilities exist, so its exit code is NOT the verdict; the
 *      parsed report is.
 *
 * Plan: 2026-11-02-only-cargo-lock-is-gated-by-a-dependency-audit (Phase 3).
 *
 * Usage:
 *   node scripts/ci-pnpm-audit-diff.mjs --base <rev> [--head <rev>]
 *
 * Exit codes:
 *   0  unchanged, or changed with no introduced moderate+ advisory
 *   1  introduced a moderate+ advisory, OR the audit could not be performed
 *   2  bad usage, or a git read failed
 *
 * Imports Node built-ins only, so CI runs it with no install step.
 */

import { execFileSync, spawnSync } from "node:child_process";
import { appendFileSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { parseArgs } from "node:util";

/** The two files whose identity decides "tree unchanged". */
export const GATED_FILES = ["package.json", "pnpm-lock.yaml"];
/** Copied alongside when present, so each audit sees its side's npm config. */
const CONFIG_FILES = [".npmrc"];
export const BLOCKING_SEVERITIES = new Set(["moderate", "high", "critical"]);
const SEVERITY_ORDER = ["critical", "high", "moderate", "low", "info"];
const MAX_PATHS_SHOWN = 3;

export class AuditUnreadable extends Error {}

/**
 * Parse `pnpm audit --json` output into Map<key, advisory>. Throws
 * AuditUnreadable on anything that is not an audit report — an error object,
 * truncated JSON, an empty string — because reading such output as "no
 * advisories" is exactly the false-clean this gate exists to refuse.
 */
export function parseAudit(text) {
  let report;
  try {
    report = JSON.parse(text);
  } catch (e) {
    throw new AuditUnreadable(`audit output is not JSON (${e.message})`);
  }
  if (report === null || typeof report !== "object" || Array.isArray(report)) {
    throw new AuditUnreadable("audit output is not a JSON object");
  }
  if (report.error) {
    const err = report.error;
    const detail = typeof err === "object" ? `${err.code ?? ""} ${err.message ?? ""}`.trim() : String(err);
    throw new AuditUnreadable(`audit reported an error: ${detail}`);
  }
  const { advisories } = report;
  if (advisories === null || typeof advisories !== "object" || Array.isArray(advisories)) {
    throw new AuditUnreadable("audit output has no `advisories` object");
  }
  if (report.metadata === null || typeof report.metadata !== "object") {
    throw new AuditUnreadable("audit output has no `metadata` object");
  }
  const out = new Map();
  for (const a of Object.values(advisories)) {
    if (!a || typeof a.module_name !== "string" || !a.module_name) {
      throw new AuditUnreadable("an advisory has no module_name");
    }
    const advisoryId = a.github_advisory_id || a.url || (a.id != null ? String(a.id) : "");
    if (!advisoryId) throw new AuditUnreadable(`an advisory for ${a.module_name} has no id`);
    const key = `${a.module_name}\u0000${advisoryId}`;
    const paths = (a.findings ?? []).flatMap((f) => f.paths ?? []);
    const prev = out.get(key);
    if (prev) {
      prev.paths.push(...paths);
      continue;
    }
    out.set(key, {
      key,
      module: a.module_name,
      advisoryId,
      severity: String(a.severity ?? "unknown").toLowerCase(),
      title: a.title ?? "",
      vulnerable: a.vulnerable_versions ?? "?",
      patched: a.patched_versions ?? "?",
      url: a.url ?? "",
      paths,
    });
  }
  return out;
}

function bySeverity(a, b) {
  const ra = SEVERITY_ORDER.indexOf(a.severity);
  const rb = SEVERITY_ORDER.indexOf(b.severity);
  return (ra < 0 ? 99 : ra) - (rb < 0 ? 99 : rb) || a.module.localeCompare(b.module) || a.advisoryId.localeCompare(b.advisoryId);
}

/** Pure: which advisories HEAD introduces and which it fixes, relative to BASE. */
export function diffAdvisories(base, head) {
  const introduced = [...head.values()].filter((a) => !base.has(a.key)).sort(bySeverity);
  const fixed = [...base.values()].filter((a) => !head.has(a.key)).sort(bySeverity);
  return {
    introduced,
    fixed,
    blocking: introduced.filter((a) => BLOCKING_SEVERITIES.has(a.severity)),
    warnings: introduced.filter((a) => !BLOCKING_SEVERITIES.has(a.severity)),
  };
}

export function renderTable(rows) {
  if (!rows.length) return ["  (none)"];
  return rows.map((a) => {
    const shown = a.paths.slice(0, MAX_PATHS_SHOWN).join(", ") || "?";
    const more = a.paths.length > MAX_PATHS_SHOWN ? ` (+${a.paths.length - MAX_PATHS_SHOWN} more)` : "";
    return `  ${a.severity.padEnd(8)} ${a.module}  ${a.advisoryId}  vulnerable ${a.vulnerable}  patched ${a.patched}  path ${shown}${more}`;
  });
}

/** Pure verdict over two parsed reports. */
export function decide(base, head) {
  const d = diffAdvisories(base, head);
  const lines = [
    `pnpm-audit-diff: base ${base.size} advisories, head ${head.size} (prod scope).`,
    `Introduced, moderate and above (BLOCKING): ${d.blocking.length}`,
    ...renderTable(d.blocking),
    `Introduced, low/info (warning only): ${d.warnings.length}`,
    ...renderTable(d.warnings),
    `Fixed by this change: ${d.fixed.length}`,
    ...renderTable(d.fixed),
  ];
  const annotations = [
    ...d.blocking.map((a) => `::error title=pnpm audit::introduced ${a.severity} ${a.advisoryId} in ${a.module} (${a.vulnerable})`),
    ...d.warnings.map((a) => `::warning title=pnpm audit::introduced ${a.severity} ${a.advisoryId} in ${a.module} (${a.vulnerable})`),
  ];
  if (d.blocking.length) {
    lines.push(
      `FAIL: this change introduces ${d.blocking.length} moderate-or-higher advisor${d.blocking.length === 1 ? "y" : "ies"} ` +
        "into the production dependency tree. Upgrade to a patched range, or drop the dependency.",
    );
    return { exitCode: 1, verdict: "introduced", lines, annotations, diff: d };
  }
  lines.push("PASS: no moderate-or-higher advisory introduced.");
  return { exitCode: 0, verdict: "clean", lines, annotations, diff: d };
}

/** Run one side's audit, retried once; returns a Map or throws AuditUnreadable. */
export function auditWithRetry(files, audit, log, label) {
  let lastErr;
  for (let attempt = 1; attempt <= 2; attempt++) {
    try {
      const res = audit(files);
      if (res.error) throw new AuditUnreadable(`could not run pnpm: ${res.error.message ?? res.error}`);
      return parseAudit(res.stdout ?? "");
    } catch (e) {
      if (!(e instanceof AuditUnreadable)) throw e;
      lastErr = e;
      log(`pnpm-audit-diff: ${label} audit attempt ${attempt} unreadable: ${e.message}`);
    }
  }
  throw lastErr;
}

/**
 * The gate, with its I/O injected:
 *   readFile(rev, path) -> string | null   (null = absent at that rev)
 *   audit(files)        -> { stdout, error? }  files = { name: content }
 *   log(line)
 * Returns { exitCode, verdict, lines }.
 */
export function runGate({ base, head, readFile, audit, log = () => {} }) {
  const read = (rev, names) => Object.fromEntries(names.map((n) => [n, readFile(rev, n)]));
  const baseFiles = read(base, GATED_FILES);
  const headFiles = read(head, GATED_FILES);

  if (GATED_FILES.every((n) => baseFiles[n] === headFiles[n])) {
    const lines = [`pnpm-audit-diff: ${GATED_FILES.join(" and ")} are unchanged between ${base} and ${head}; nothing to audit (no network call).`];
    return { exitCode: 0, verdict: "unchanged", lines, annotations: [] };
  }
  if (headFiles["pnpm-lock.yaml"] === null) {
    const lines = [`pnpm-audit-diff: no pnpm-lock.yaml at ${head}; no npm production tree to audit.`];
    return { exitCode: 0, verdict: "no-lockfile", lines, annotations: [] };
  }

  const withConfig = (rev, files) => {
    const out = {};
    for (const [n, c] of Object.entries({ ...files, ...read(rev, CONFIG_FILES) })) if (c !== null) out[n] = c;
    return out;
  };

  try {
    let baseAdvisories;
    if (baseFiles["pnpm-lock.yaml"] === null) {
      log(`pnpm-audit-diff: no pnpm-lock.yaml at ${base}; every head advisory counts as introduced.`);
      baseAdvisories = new Map();
    } else {
      baseAdvisories = auditWithRetry(withConfig(base, baseFiles), audit, log, "base");
    }
    const headAdvisories = auditWithRetry(withConfig(head, headFiles), audit, log, "head");
    return decide(baseAdvisories, headAdvisories);
  } catch (e) {
    if (!(e instanceof AuditUnreadable)) throw e;
    const lines = [
      `pnpm-audit-diff: UNKNOWN — the audit could not be performed (${e.message}).`,
      "This is not a clean result: the dependency tree changed and was not checked. Re-run the job; if the npm registry is down, wait for it.",
    ];
    return { exitCode: 1, verdict: "unknown", lines, annotations: [`::error title=pnpm audit::audit could not be performed: ${e.message}`] };
  }
}

// ---------------------------------------------------------------- real I/O

function gitReadFile(rev, path) {
  try {
    execFileSync("git", ["cat-file", "-e", `${rev}:${path}`], { stdio: "ignore" });
  } catch {
    return null;
  }
  return execFileSync("git", ["show", `${rev}:${path}`], { encoding: "utf8", maxBuffer: 256 * 1024 * 1024 });
}

function pnpmAudit(files) {
  const dir = mkdtempSync(join(tmpdir(), "pnpm-audit-diff-"));
  try {
    for (const [name, content] of Object.entries(files)) writeFileSync(join(dir, name), content);
    const res = spawnSync("pnpm", ["audit", "--prod", "--json"], {
      cwd: dir,
      encoding: "utf8",
      maxBuffer: 256 * 1024 * 1024,
      shell: process.platform === "win32",
    });
    return { stdout: res.stdout, error: res.error };
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

function main(argv) {
  let values;
  try {
    ({ values } = parseArgs({ args: argv, options: { base: { type: "string" }, head: { type: "string", default: "HEAD" } } }));
  } catch (e) {
    process.stderr.write(`${e.message}\n`);
    return 2;
  }
  if (!values.base) {
    process.stderr.write("usage: ci-pnpm-audit-diff.mjs --base <rev> [--head <rev>]\n");
    return 2;
  }
  let result;
  try {
    result = runGate({
      base: values.base,
      head: values.head,
      readFile: gitReadFile,
      audit: pnpmAudit,
      log: (l) => process.stdout.write(`${l}\n`),
    });
  } catch (e) {
    process.stdout.write(`::error title=pnpm audit::${e.message}\n`);
    return 2;
  }
  process.stdout.write(`${result.lines.join("\n")}\n`);
  for (const a of result.annotations) process.stdout.write(`${a}\n`);
  if (process.env.GITHUB_STEP_SUMMARY) {
    try {
      appendFileSync(process.env.GITHUB_STEP_SUMMARY, `### pnpm audit (introduced advisories)\n\n\`\`\`\n${result.lines.join("\n")}\n\`\`\`\n`);
    } catch {
      // The summary is a convenience; the log above carries the verdict.
    }
  }
  return result.exitCode;
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  process.exit(main(process.argv.slice(2)));
}

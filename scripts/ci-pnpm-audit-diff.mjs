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
 *   0. Resolves BASE (resolveBase): a PR's merge-base with its base branch; a
 *      push to main/develop's `before` when it is a real ancestor of HEAD; any
 *      other push (merge-candidate refs, new refs, force pushes) the merge-base
 *      with origin/main. HEAD^ is a loudly announced last resort for a TRUNK
 *      push only; for a non-trunk ref an unreachable main or merge-base is
 *      UNKNOWN (exit 2). A merge-candidate ref is new (before = all zeros) and
 *      may carry several commits, so HEAD^ there would miss a lockfile change
 *      in an earlier one.
 *   1. Refuses audit-config drift: a change to package.json `pnpm.auditConfig`
 *      fails, because pnpm audit honours it (an added ignoreGhsas entry drops
 *      that advisory from the report — verified) and package.json reaches the
 *      audit. A change to a registry/audit key of .npmrc is only a WARNING:
 *      .npmrc never reaches the audit (step 3), and only key names are ever
 *      printed, never values.
 *   2. Reads package.json + pnpm-lock.yaml at BASE and at HEAD. If both are
 *      byte-identical: exit 0 with NO network call.
 *   3. Otherwise audits each side in a temp dir holding ONLY those two files,
 *      with `pnpm audit --prod --json --registry=https://registry.npmjs.org/`
 *      (no install needed; the explicit registry and the absent .npmrc mean the
 *      change under test cannot pick who answers). Dev-only tooling is out of
 *      scope (--prod).
 *   4. An advisory is the pair (package name, GHSA id — else URL, else npm id).
 *      A pair at HEAD and not at BASE is INTRODUCED; at BASE and not at HEAD it
 *      is FIXED. Introduced info/low are warnings; every other severity —
 *      moderate, high, critical, and anything unrecognised — blocks. A new
 *      vulnerable VERSION under a pair already present at BASE is a warning,
 *      not a block (identity is deliberately package+advisory).
 *   5. An audit that cannot run, or whose JSON is not an audit report, is
 *      UNKNOWN — never clean. Each audit is tried twice, then the step fails
 *      saying the audit could not be performed. pnpm audit exits non-zero
 *      whenever vulnerabilities exist, so its exit code is NOT the verdict; the
 *      parsed report is.
 *
 * Plan: 2026-11-02-only-cargo-lock-is-gated-by-a-dependency-audit (Phase 3).
 *
 * Usage:
 *   node scripts/ci-pnpm-audit-diff.mjs --base <rev> [--head <rev>]
 *   node scripts/ci-pnpm-audit-diff.mjs            # in GitHub Actions only:
 *        BASE is resolved from GITHUB_EVENT_NAME, GITHUB_REF, PR_BASE_SHA and
 *        PUSH_BEFORE, fetching (and unshallowing) as needed. Refused outside
 *        Actions, because it fetches into the clone it runs in.
 *
 * Exit codes:
 *   0  unchanged, or changed with nothing blocking introduced
 *   1  something blocking introduced, audit-config drift, pnpm-lock.yaml
 *      deleted, OR the audit could not be performed (UNKNOWN)
 *   2  bad usage, an unresolvable revision, or BASE could not be determined
 *      (UNKNOWN — never a pass)
 *
 * Imports Node built-ins only, so CI runs it with no install step.
 */

import { execFileSync, spawnSync } from "node:child_process";
import { appendFileSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { parseArgs } from "node:util";

/** The two files whose identity decides "tree unchanged", and the only two audited. */
export const GATED_FILES = ["package.json", "pnpm-lock.yaml"];
export const AUDIT_REGISTRY = "https://registry.npmjs.org/";
/** Severities that warn rather than block. Everything else blocks. */
export const WARN_SEVERITIES = new Set(["info", "low"]);
const SEVERITY_ORDER = ["critical", "high", "moderate", "low", "info"];
const MAX_PATHS_SHOWN = 3;
const TRUNK_BRANCHES = new Set(["main", "develop"]);
const MAIN_REFSPEC = "+refs/heads/main:refs/remotes/origin/main";
const MAIN_REF = "refs/remotes/origin/main";

export class AuditUnreadable extends Error {}
/** A usage-class failure: bad rev, unresolvable BASE. Exit 2. */
export class GateUsageError extends Error {}

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
    const findings = a.findings ?? [];
    const paths = findings.flatMap((f) => f.paths ?? []);
    const versions = findings.map((f) => f.version).filter((v) => typeof v === "string");
    const prev = out.get(key);
    if (prev) {
      prev.paths.push(...paths);
      for (const v of versions) prev.versions.add(v);
      continue;
    }
    out.set(key, {
      key,
      module: a.module_name,
      advisoryId,
      // Kept verbatim (lowercased); an unrecognised or missing severity BLOCKS.
      severity: typeof a.severity === "string" && a.severity ? a.severity.toLowerCase() : "unknown",
      title: a.title ?? "",
      vulnerable: a.vulnerable_versions ?? "?",
      patched: a.patched_versions ?? "?",
      url: a.url ?? "",
      paths,
      versions: new Set(versions),
    });
  }
  return out;
}

function bySeverity(a, b) {
  const ra = SEVERITY_ORDER.indexOf(a.severity);
  const rb = SEVERITY_ORDER.indexOf(b.severity);
  // Unrecognised severities sort FIRST: they block and need a human.
  return (ra < 0 ? -1 : ra) - (rb < 0 ? -1 : rb) || a.module.localeCompare(b.module) || a.advisoryId.localeCompare(b.advisoryId);
}

/** Pure: which advisories HEAD introduces and which it fixes, relative to BASE. */
export function diffAdvisories(base, head) {
  const introduced = [...head.values()].filter((a) => !base.has(a.key)).sort(bySeverity);
  const fixed = [...base.values()].filter((a) => !head.has(a.key)).sort(bySeverity);
  const newVersions = [];
  for (const a of head.values()) {
    const b = base.get(a.key);
    if (!b) continue;
    const added = [...a.versions].filter((v) => !b.versions.has(v));
    if (added.length) newVersions.push({ advisory: a, added });
  }
  return {
    introduced,
    fixed,
    newVersions,
    blocking: introduced.filter((a) => !WARN_SEVERITIES.has(a.severity)),
    warnings: introduced.filter((a) => WARN_SEVERITIES.has(a.severity)),
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
    `Introduced, blocking (moderate and above, or unrecognised severity): ${d.blocking.length}`,
    ...renderTable(d.blocking),
    `Introduced, low/info (warning only): ${d.warnings.length}`,
    ...renderTable(d.warnings),
    `New vulnerable versions under an advisory already present at base (warning only): ${d.newVersions.length}`,
    ...(d.newVersions.length
      ? d.newVersions.map(({ advisory: a, added }) => `  ${a.severity.padEnd(8)} ${a.module}  ${a.advisoryId}  new version(s) ${added.join(", ")}`)
      : ["  (none)"]),
    `Fixed by this change: ${d.fixed.length}`,
    ...renderTable(d.fixed),
  ];
  const annotations = [
    ...d.blocking.map((a) => `::error title=pnpm audit::introduced ${a.severity} ${a.advisoryId} in ${a.module} (${a.vulnerable})`),
    ...d.warnings.map((a) => `::warning title=pnpm audit::introduced ${a.severity} ${a.advisoryId} in ${a.module} (${a.vulnerable})`),
    ...d.newVersions.map(
      ({ advisory: a, added }) =>
        `::warning title=pnpm audit::${a.module} ${added.join(", ")} is newly vulnerable to ${a.advisoryId}, already present at base`,
    ),
  ];
  if (d.blocking.length) {
    lines.push(
      `FAIL: this change introduces ${d.blocking.length} blocking advisor${d.blocking.length === 1 ? "y" : "ies"} ` +
        "into the production dependency tree. Upgrade to a patched range, or drop the dependency.",
    );
    return { exitCode: 1, verdict: "introduced", lines, annotations, diff: d };
  }
  lines.push("PASS: no blocking advisory introduced.");
  return { exitCode: 0, verdict: "clean", lines, annotations, diff: d };
}

// ------------------------------------------------------- audit-config drift

/** The registry/audit-related keys of an .npmrc: Map<key, value>. */
export function npmrcAuditKeys(text) {
  const out = new Map();
  if (text === null || text === undefined) return out;
  for (const raw of text.split(/\r?\n/)) {
    const l = raw.trim();
    if (!l || l.startsWith("#") || l.startsWith(";")) continue;
    const eq = l.indexOf("=");
    const key = (eq < 0 ? l : l.slice(0, eq)).trim();
    if (!/registry|audit|_auth|token|password/i.test(key)) continue;
    out.set(key, eq < 0 ? "" : l.slice(eq + 1).trim());
  }
  return out;
}

/** A key as it may be printed: credential-shaped keys are masked entirely. */
export function printableKey(key) {
  return /_auth|token|password/i.test(key) ? "<credential key>" : key;
}

function auditConfigOf(pkgText) {
  if (pkgText === null) return null;
  try {
    return JSON.parse(pkgText)?.pnpm?.auditConfig ?? null;
  } catch {
    return "<unparseable package.json>";
  }
}

/**
 * Pure: how HEAD's audit configuration differs from BASE's.
 *   failures — pnpm.auditConfig changes (package.json reaches the audit)
 *   warnings — .npmrc registry/audit key changes (KEY NAMES only, never values)
 */
export function auditConfigDrift(base, head) {
  const failures = [];
  const warnings = [];
  const b = JSON.stringify(auditConfigOf(base["package.json"]));
  const h = JSON.stringify(auditConfigOf(head["package.json"]));
  if (b !== h) failures.push(`package.json pnpm.auditConfig changed: ${b} -> ${h}`);
  const rb = npmrcAuditKeys(base[".npmrc"]);
  const rh = npmrcAuditKeys(head[".npmrc"]);
  const changed = [...new Set([...rb.keys(), ...rh.keys()])]
    .filter((k) => rb.get(k) !== rh.get(k))
    .map((k) => `${printableKey(k)} (${!rb.has(k) ? "added" : !rh.has(k) ? "removed" : "changed"})`)
    .sort();
  if (changed.length) {
    warnings.push(
      `.npmrc registry/audit keys changed: ${changed.join(", ")}. The audit does not read .npmrc, so this does not affect the verdict; ` +
        "review the change on its own merits.",
    );
  }
  return { failures, warnings };
}

// ------------------------------------------------------------------- gate

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
 *   resolveRev(rev)     -> commit sha | null   (null = unresolvable: exit 2)
 *   readFile(rev, path) -> string | null       (null = absent at that rev)
 *   audit(files)        -> { stdout, error? }  files = { name: content }
 *   log(line)
 * Returns { exitCode, verdict, lines, annotations }. Throws GateUsageError.
 */
export function runGate({ base, head, resolveRev, readFile, audit, log = () => {} }) {
  for (const rev of [base, head]) {
    if (!resolveRev(rev)) throw new GateUsageError(`revision ${rev} does not resolve to a commit`);
  }
  const read = (rev, names) => Object.fromEntries(names.map((n) => [n, readFile(rev, n)]));
  const baseFiles = read(base, [...GATED_FILES, ".npmrc"]);
  const headFiles = read(head, [...GATED_FILES, ".npmrc"]);

  const drift = auditConfigDrift(baseFiles, headFiles);
  const warn = (result) => ({
    ...result,
    lines: [...drift.warnings.map((w) => `pnpm-audit-diff: WARNING — ${w}`), ...result.lines],
    annotations: [...drift.warnings.map((w) => `::warning title=pnpm audit::${w}`), ...result.annotations],
  });
  if (drift.failures.length) {
    const lines = [
      "pnpm-audit-diff: FAIL — this change alters how the audit itself is configured:",
      ...drift.failures.map((d) => `  ${d}`),
      "pnpm audit honours package.json pnpm.auditConfig, so a change to it can make this gate pass falsely " +
        "and is never accepted silently. Land it in its own PR with a stated justification (for an ignore: " +
        "the advisory and why it does not apply).",
    ];
    return warn({ exitCode: 1, verdict: "config-drift", lines, annotations: drift.failures.map((d) => `::error title=pnpm audit::${d}`) });
  }
  return warn(gateAfterDrift({ base, head, baseFiles, headFiles, audit, log }));
}

function gateAfterDrift({ base, head, baseFiles, headFiles, audit, log }) {

  if (GATED_FILES.every((n) => baseFiles[n] === headFiles[n])) {
    const lines = [`pnpm-audit-diff: ${GATED_FILES.join(" and ")} are unchanged between ${base} and ${head}; nothing to audit (no network call).`];
    return { exitCode: 0, verdict: "unchanged", lines, annotations: [] };
  }
  if (headFiles["pnpm-lock.yaml"] === null) {
    if (baseFiles["pnpm-lock.yaml"] !== null) {
      const msg = `pnpm-lock.yaml exists at ${base} but is deleted at ${head}; the production tree can no longer be audited.`;
      return { exitCode: 1, verdict: "lockfile-removed", lines: [`pnpm-audit-diff: FAIL — ${msg}`], annotations: [`::error title=pnpm audit::${msg}`] };
    }
    const lines = [`pnpm-audit-diff: no pnpm-lock.yaml at ${base} or ${head}; no npm production tree to audit.`];
    return { exitCode: 0, verdict: "no-lockfile", lines, annotations: [] };
  }

  const only = (files) => Object.fromEntries(GATED_FILES.filter((n) => files[n] !== null).map((n) => [n, files[n]]));

  try {
    let baseAdvisories;
    if (baseFiles["pnpm-lock.yaml"] === null) {
      log(`pnpm-audit-diff: no pnpm-lock.yaml at ${base}; every head advisory counts as introduced.`);
      baseAdvisories = new Map();
    } else {
      baseAdvisories = auditWithRetry(only(baseFiles), audit, log, "base");
    }
    const headAdvisories = auditWithRetry(only(headFiles), audit, log, "head");
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

// ---------------------------------------------------------------- BASE

const ZERO_SHA = /^0+$/;

/**
 * Decide BASE for a CI run, with git injected:
 *   git.revParse(rev)             -> sha | null
 *   git.fetch(refspecs, {depth, refetch?}) -> boolean  (depth: number | "unshallow")
 *   git.isShallow()               -> boolean
 *   git.mergeBase(a, b)           -> sha | null
 *   git.isAncestor(a, b)          -> boolean
 * ctx = { eventName, ref, prBaseSha, before }. Returns { base, how }; throws
 * GateUsageError when no BASE can be determined.
 */
export function resolveBase(ctx, git, log = () => {}) {
  const head = git.revParse("HEAD");
  if (!head) throw new GateUsageError("HEAD does not resolve");
  // HEAD and its parents: enough for a PR merge commit, whose first parent IS
  // the base tip, and for a single-commit push. `refetch`: HEAD is already
  // present (at depth 1), and this fetch exists to deepen it.
  git.fetch([head], { depth: 2, refetch: true });

  let deepened = false;
  const deepen = (refspecs) => {
    if (deepened || !git.isShallow()) return;
    deepened = true;
    log("pnpm-audit-diff: shallow history does not reach the comparison point; unshallowing.");
    git.fetch([head, ...refspecs], { depth: "unshallow" });
  };
  const mergeBaseDeep = (other, refspecs) => {
    let m = git.mergeBase(head, other);
    if (!m) {
      deepen(refspecs);
      m = git.mergeBase(head, other);
    }
    return m;
  };

  if (ctx.eventName === "pull_request") {
    if (!ctx.prBaseSha) throw new GateUsageError("pull_request event without a base sha");
    git.fetch([ctx.prBaseSha], { depth: 1 });
    const m = mergeBaseDeep(ctx.prBaseSha, [ctx.prBaseSha]);
    if (!m) throw new GateUsageError(`no merge-base between HEAD and PR base ${ctx.prBaseSha}`);
    return { base: m, how: `merge-base of HEAD with PR base ${ctx.prBaseSha}` };
  }

  const branch = (ctx.ref ?? "").replace(/^refs\/heads\//, "");
  const trunk = TRUNK_BRANCHES.has(branch);
  const before = ctx.before ?? "";
  if (ctx.eventName === "push" && trunk && before && !ZERO_SHA.test(before)) {
    if (git.fetch([before], { depth: 1 })) {
      let anc = git.isAncestor(before, head);
      if (!anc) {
        deepen([before]);
        anc = git.isAncestor(before, head);
      }
      if (anc) return { base: before, how: `push 'before' ${before}` };
      log(`pnpm-audit-diff: push 'before' ${before} is not an ancestor of HEAD (force push?); using merge-base with main.`);
    } else {
      log(`pnpm-audit-diff: push 'before' ${before} could not be fetched; using merge-base with main.`);
    }
  }

  const fetchedMain = git.fetch([MAIN_REFSPEC], { depth: 2 });
  const m = fetchedMain ? mergeBaseDeep(MAIN_REF, [MAIN_REFSPEC]) : null;
  if (!trunk) {
    // A non-trunk ref (merge-candidate*) may carry several commits, so HEAD^
    // would compare only the last one: no fallback, the BASE is UNKNOWN.
    if (!fetchedMain) throw new GateUsageError(`UNKNOWN BASE for ${ctx.ref ?? "this ref"}: could not fetch origin/main`);
    if (!m) throw new GateUsageError(`UNKNOWN BASE for ${ctx.ref ?? "this ref"}: no merge-base between HEAD and origin/main (even after unshallowing)`);
    return { base: m, how: "merge-base of HEAD with origin/main" };
  }
  if (m && m !== head) return { base: m, how: "merge-base of HEAD with origin/main" };

  let parent = git.revParse("HEAD^");
  if (!parent) {
    deepen([]);
    parent = git.revParse("HEAD^");
  }
  if (!parent) throw new GateUsageError("UNKNOWN BASE: no usable before, no merge-base with main, no HEAD^");
  log(
    "::warning title=pnpm audit::BASE fell back to HEAD^ (no usable push 'before' and no merge-base with main below HEAD). " +
      "Only the LAST commit's dependency changes are compared.",
  );
  return { base: parent, how: "HEAD^ (LAST RESORT)" };
}

// ---------------------------------------------------------------- real I/O

function git(args, opts = {}) {
  return spawnSync("git", args, { encoding: "utf8", maxBuffer: 256 * 1024 * 1024, ...opts });
}

function gitResolveRev(rev) {
  const r = git(["rev-parse", "--verify", "--quiet", `${rev}^{commit}`]);
  return r.status === 0 ? r.stdout.trim() : null;
}

function gitReadFile(rev, path) {
  if (git(["cat-file", "-e", `${rev}:${path}`]).status !== 0) return null;
  return execFileSync("git", ["show", `${rev}:${path}`], { encoding: "utf8", maxBuffer: 256 * 1024 * 1024 });
}

const realGit = {
  revParse: gitResolveRev,
  isShallow: () => git(["rev-parse", "--is-shallow-repository"]).stdout.trim() === "true",
  fetch(refspecs, { depth, refetch = false }) {
    // A --depth fetch of a commit that is already present RE-TRUNCATES its
    // history (measured: fetching main == HEAD at depth 1 dropped HEAD^), so a
    // sha we already hold is never re-fetched with a depth.
    if (depth !== "unshallow" && !refetch) {
      refspecs = refspecs.filter((r) => !/^[0-9a-f]{40}$/.test(r) || git(["cat-file", "-e", `${r}^{commit}`]).status !== 0);
      if (!refspecs.length) return true;
    }
    const args = ["fetch", "--no-tags", "--quiet"];
    // Depth flags only on an already-shallow clone: --depth on a full clone
    // would MAKE it shallow.
    if (realGit.isShallow()) args.push(depth === "unshallow" ? "--unshallow" : `--depth=${depth}`);
    else if (depth === "unshallow") return true;
    return git([...args, "origin", ...refspecs], { stdio: ["ignore", "inherit", "inherit"] }).status === 0;
  },
  mergeBase(a, b) {
    const r = git(["merge-base", a, b]);
    return r.status === 0 ? r.stdout.trim() : null;
  },
  isAncestor: (a, b) => git(["merge-base", "--is-ancestor", a, b]).status === 0,
};

function pnpmAudit(files) {
  const dir = mkdtempSync(join(tmpdir(), "pnpm-audit-diff-"));
  try {
    for (const [name, content] of Object.entries(files)) writeFileSync(join(dir, name), content);
    const res = spawnSync("pnpm", ["audit", "--prod", "--json", `--registry=${AUDIT_REGISTRY}`], {
      cwd: dir,
      encoding: "utf8",
      maxBuffer: 256 * 1024 * 1024,
      shell: process.platform === "win32",
      // Never let the audited package.json's `packageManager` field swap pnpm.
      env: { ...process.env, npm_config_manage_package_manager_versions: "false" },
    });
    return { stdout: res.stdout, error: res.error };
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

/**
 * Escape the message part of a workflow command (`::cmd props::message`) so a
 * `%`, CR or LF inside it (git stderr, for example) is not cut short or
 * garbled in the annotation. Lines that are not workflow commands pass through.
 */
export function escapeAnnotation(line) {
  const m = /^(::[a-z-]+(?: [^:]*)?::)([\s\S]*)$/.exec(line);
  if (!m) return line;
  return m[1] + m[2].replace(/%/g, "%25").replace(/\r/g, "%0D").replace(/\n/g, "%0A");
}

function main(argv) {
  const say = (l) => process.stdout.write(`${l}\n`);
  let values;
  try {
    ({ values } = parseArgs({ args: argv, options: { base: { type: "string" }, head: { type: "string", default: "HEAD" } } }));
  } catch (e) {
    process.stderr.write(`${e.message}\n`);
    return 2;
  }
  let result;
  try {
    let base = values.base;
    if (!base) {
      if (process.env.GITHUB_ACTIONS !== "true") {
        process.stderr.write("usage: ci-pnpm-audit-diff.mjs --base <rev> [--head <rev>]  (BASE auto-resolution runs in GitHub Actions only)\n");
        return 2;
      }
      const r = resolveBase(
        {
          eventName: process.env.GITHUB_EVENT_NAME,
          ref: process.env.GITHUB_REF,
          prBaseSha: process.env.PR_BASE_SHA,
          before: process.env.PUSH_BEFORE,
        },
        realGit,
        say,
      );
      base = r.base;
      say(`pnpm-audit-diff: BASE=${r.base} (${r.how})`);
    }
    result = runGate({ base, head: values.head, resolveRev: gitResolveRev, readFile: gitReadFile, audit: pnpmAudit, log: say });
  } catch (e) {
    say(escapeAnnotation(`::error title=pnpm audit::${e.message}`));
    return 2;
  }
  say(result.lines.join("\n"));
  for (const a of result.annotations) say(escapeAnnotation(a));
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

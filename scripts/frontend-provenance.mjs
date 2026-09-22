#!/usr/bin/env node
// Build provenance for the runner's two build halves — the ONE implementation of
// the fold (plan 2026-08-23-build-provenance-assertion, Phase 1 + D4).
//
// The runner is built in two independent steps: Vite builds the frontend into
// `dist/`, cargo embeds `dist/` into the exe. Nothing asserted the two halves
// describe the same tree, and a leftover `dist/` from an older checkout made a
// "fresh" exe serve a stale UI. This module records WHAT TREE STATE a build was
// made from, as content hashes rather than commit SHAs — agents build from
// dirty worktrees as normal practice, and a SHA says nothing about uncommitted
// edits.
//
// Consumers:
//   * `vite.config.ts` imports `computeFrontendProvenance` and writes
//     `dist/provenance.json` beside `dist/build-id.txt`.
//   * `src-tauri/build.rs` carries a Rust PORT of `foldLines` (a build script
//     cannot shell Node reliably). Both sides are pinned to
//     `scripts/fixtures/provenance-fold.json` by a test on each side.
//   * a temp-runner launcher being added to qontinui-claude-config
//     (`scripts/start-temp-runner.ps1`, a companion change) is to shell the
//     `verify` CLI below instead of re-implementing any of this.
//
// THE FOLD FORMAT — byte-defined, because two implementations must agree:
//   * one line per input: `<repo-relative-path> <40-hex-oid>`, or
//     `<path> absent` for an input recorded at build time and since deleted;
//   * paths are git's own spelling, verbatim (forward slashes, exactly the
//     bytes `git ls-files` prints — no Unicode normalization, which the Rust
//     side could not reproduce with `std` alone), sorted by the BYTE VALUE of
//     their UTF-8 encoding (LC_ALL=C order, not locale order);
//   * every line ends in exactly one `\n`, the last one included; no BOM.
// The fold text is then hashed with `git hash-object --stdin`, and every input
// oid with `git hash-object --stdin-paths` — git on BOTH sides, so
// `core.autocrlf` / `.gitattributes` normalization is identical everywhere. A
// language-native digest over raw bytes would disagree with git's normalized
// blob hash permanently on Windows: the exact permanent-false-positive shape
// that got the 2026-07-28 build-id banner deleted.
//
// Recompute failure is UNKNOWN, never a mismatch (plan D3): no git, not a repo,
// an input unreadable for a reason other than deletion — the verdict is
// `unknown` with the reason, never `mismatch`.

import { execFileSync } from "node:child_process";
import { existsSync, readFileSync, statSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

export const SCHEMA_VERSION = 1;
export const UNKNOWN = "unknown";

// Inputs that shape the bundle without appearing in Rollup's module graph.
// `package.json` is here because vite.config.ts bakes its `version` in.
const EXPLICIT_FRONTEND_INPUTS = [
  "vite.config.ts",
  "index.html",
  "package.json",
  "tsconfig.json",
  "tsconfig.node.json",
];

// The Rust half's binary-affecting inputs (plan D4 item 1, Phase 1b): the
// crate, its in-repo path dependencies, the vendored `[patch]`, and what
// `generate_context!` / `include_str!` embed. Pathspecs relative to the repo
// root; `build.rs` carries the same list. Out of scope, deliberately: the
// `../qontinui-schemas` sibling (sibling-pin-check.sh owns sibling drift).
// Untracked-but-unignored files under these paths count, which errs toward a
// false MISMATCH (a peer's scratch file), never toward a false match.
export const RUST_SRC_PATHSPECS = [
  "src-tauri/src",
  "src-tauri/build.rs",
  "src-tauri/Cargo.toml",
  "src-tauri/capabilities",
  "src-tauri/tauri.conf.json",
  "src-tauri/resources",
  "src-tauri/icons",
  "src-tauri/clorinde",
  "crates/spec-check",
  "crates/runner-stats",
  "crates/runner-win32",
  "vendor/tao-0.35.0",
  "Cargo.toml",
  "Cargo.lock",
];

// A git inherited from a hook (GIT_DIR / GIT_INDEX_FILE / GIT_WORK_TREE) would
// answer about a different repository or index than `-C root` names.
const GIT_ENV = Object.fromEntries(
  Object.entries(process.env).filter(([k]) => !["GIT_DIR", "GIT_INDEX_FILE", "GIT_WORK_TREE"].includes(k)),
);

function git(root, args, input) {
  return execFileSync("git", ["-C", root, ...args], {
    input,
    env: GIT_ENV,
    stdio: ["pipe", "pipe", "pipe"],
    maxBuffer: 256 * 1024 * 1024,
  });
}

/** Byte-order comparison of two strings' UTF-8 encodings (LC_ALL=C order). */
export function compareUtf8(a, b) {
  return Buffer.compare(Buffer.from(a, "utf8"), Buffer.from(b, "utf8"));
}

/** A repo-relative path in the fold's spelling: forward slashes, else verbatim. */
export function canonicalPath(p) {
  return p.replace(/\\/g, "/");
}

/**
 * The fold text for `entries` — `[{path, oid}]` where `oid` is a 40-hex string
 * or `null` for an absent input. Pure: no git, no filesystem. This is the
 * function `build.rs` ports; the shared fixture pins both.
 */
export function foldLines(entries) {
  const rows = entries.map((e) => ({ path: canonicalPath(e.path), oid: e.oid }));
  rows.sort((a, b) => compareUtf8(a.path, b.path));
  return rows.map((r) => `${r.path} ${r.oid ?? "absent"}\n`).join("");
}

/** `git hash-object --stdin` over a string. */
export function hashText(root, text) {
  return git(root, ["hash-object", "--stdin"], text).toString("utf8").trim();
}

/** A path that exists and is not a directory (symlinks followed). */
function isHashable(root, p) {
  try {
    return !statSync(path.join(root, p)).isDirectory();
  } catch {
    return false;
  }
}

/**
 * Hash each repo-relative path with ONE `git hash-object --stdin-paths` call.
 * A path that does not exist -- or is a directory, e.g. an untracked symlink to
 * one, which git cannot hash -- yields `null` (the `absent` marker; build.rs
 * applies the same rule); any other failure throws, which callers turn into
 * an UNKNOWN verdict.
 */
export function hashPaths(root, relPaths) {
  const present = relPaths.filter((p) => isHashable(root, p));
  const oids = new Map(relPaths.map((p) => [p, null]));
  if (present.length > 0) {
    const out = git(root, ["hash-object", "--stdin-paths"], present.join("\n") + "\n")
      .toString("utf8")
      .trim()
      .split("\n");
    if (out.length !== present.length) {
      throw new Error(`git hash-object returned ${out.length} oids for ${present.length} paths`);
    }
    present.forEach((p, i) => oids.set(p, out[i]));
  }
  return relPaths.map((p) => ({ path: p, oid: oids.get(p) }));
}

/** The set of paths git does NOT ignore (tracked plus untracked-not-ignored). */
export function unignoredPaths(root, pathspecs = []) {
  const out = git(root, [
    "ls-files", "-z", "--cached", "--others", "--exclude-standard", "--", ...pathspecs,
  ]).toString("utf8");
  return new Set(out.split("\0").filter(Boolean).map(canonicalPath));
}

export function gitHead(root) {
  return git(root, ["rev-parse", "HEAD"]).toString("utf8").trim();
}

export function gitDirty(root) {
  return git(root, ["--no-optional-locks", "status", "--porcelain"]).toString("utf8").trim() !== "";
}

/**
 * Reduce Rollup module ids to the repo-relative, git-unignored source files the
 * bundle was built from (plan D5): drop virtual ids (`\0…`), strip `?query`,
 * keep only files under `root` that git does not ignore. `node_modules/` is
 * dropped by that last rule ON PURPOSE — dependency freshness is the
 * supervisor's dep-hash gate, and `depLockOid` records the lockfile here.
 * Modules outside the repo (the `@qontinui/schemas` alias into
 * `../qontinui-schemas`) are likewise out of scope: sibling-pin-check.sh owns
 * sibling drift.
 */
export function frontendInputs(root, moduleIds) {
  const unignored = unignoredPaths(root);
  const rootAbs = path.resolve(root);
  const picked = new Set();
  for (const raw of moduleIds) {
    if (!raw || raw.startsWith("\0")) continue;
    const id = raw.split("?")[0];
    if (!path.isAbsolute(id)) continue;
    const rel = path.relative(rootAbs, id);
    if (rel.startsWith("..") || path.isAbsolute(rel)) continue;
    const canon = canonicalPath(rel);
    if (unignored.has(canon)) picked.add(canon);
  }
  const fromGraph = picked.size;
  for (const p of EXPLICIT_FRONTEND_INPUTS) if (unignored.has(p)) picked.add(p);
  return { inputs: [...picked].sort(compareUtf8), fromGraph };
}

/**
 * Everything `dist/provenance.json` records. Never throws: a failure to measure
 * produces `unknown` fields and an `unknownReason`, so a Vite build is never
 * broken by provenance (plan D3).
 */
export function computeFrontendProvenance({ root, moduleIds, buildId }) {
  const base = { schemaVersion: SCHEMA_VERSION, buildId: buildId ?? null };
  try {
    const { inputs: paths, fromGraph } = frontendInputs(root, moduleIds);
    // Zero module-graph files under root means the ids did not resolve against
    // it (e.g. root reached through a junction Vite realpaths past): a hash
    // over the config files alone would "match" any src edit. Say unknown.
    if (fromGraph === 0) {
      throw new Error(`no module-graph file resolved under ${root} - cannot say which sources this dist came from`);
    }
    const inputs = hashPaths(root, paths);
    const lock = hashPaths(root, ["pnpm-lock.yaml"])[0];
    return {
      ...base,
      gitSha: gitHead(root),
      gitDirty: gitDirty(root),
      frontendSrcHash: hashText(root, foldLines(inputs)),
      depLockOid: lock.oid,
      inputs,
    };
  } catch (err) {
    return {
      ...base,
      gitSha: UNKNOWN,
      gitDirty: null,
      frontendSrcHash: UNKNOWN,
      depLockOid: null,
      inputs: [],
      unknownReason: String(err?.message ?? err).split("\n")[0],
    };
  }
}

/** The Rust half's content hash over `RUST_SRC_PATHSPECS` (Phase 1b / D4). */
export function computeRustSrcHash(root) {
  const inputs = [...unignoredPaths(root, RUST_SRC_PATHSPECS)].sort(compareUtf8);
  return hashText(root, foldLines(hashPaths(root, inputs)));
}

/**
 * Verify the frontend half of `root` (plan D4 item 3). Returns one verdict
 * object; `verdict` is `match` | `mismatch` | `unknown`.
 *   * `expect` (optional) is the `frontendSrcHash` the exe EMBEDS (from
 *     `/health`). If `dist/provenance.json` on disk records a different hash,
 *     the exe embeds a different dist than the one on disk.
 *   * Otherwise the recorded inputs are re-hashed from the working tree; a
 *     different fold means `src` changed since the last frontend build.
 */
export function verifyFrontend(root, { expect } = {}) {
  const file = path.join(root, "dist", "provenance.json");
  let recorded;
  try {
    recorded = JSON.parse(readFileSync(file, "utf8"));
  } catch (err) {
    return {
      half: "frontend",
      verdict: "unknown",
      reason: existsSync(file)
        ? `dist/provenance.json unreadable: ${String(err?.message ?? err).split("\n")[0]}`
        : "dist/provenance.json absent - this dist predates provenance stamping; rebuild with `pnpm run build`",
    };
  }
  if (!recorded.frontendSrcHash || recorded.frontendSrcHash === UNKNOWN || !Array.isArray(recorded.inputs) || recorded.inputs.length === 0) {
    return {
      half: "frontend",
      verdict: "unknown",
      reason: `the frontend build could not measure its inputs (${recorded.unknownReason ?? "no reason recorded"})`,
    };
  }
  if (expect && expect !== recorded.frontendSrcHash) {
    return {
      half: "frontend",
      verdict: "mismatch",
      kind: "exe_embeds_different_dist",
      recorded: recorded.frontendSrcHash,
      expected: expect,
      reason: "the exe embeds a different dist than the one on disk - rebuild the Rust half (`pnpm run build:exe`)",
    };
  }
  let now;
  try {
    now = hashPaths(root, recorded.inputs.map((i) => i.path));
  } catch (err) {
    return { half: "frontend", verdict: "unknown", reason: `could not re-hash inputs: ${String(err?.message ?? err).split("\n")[0]}` };
  }
  let recomputed;
  try {
    recomputed = hashText(root, foldLines(now));
  } catch (err) {
    return { half: "frontend", verdict: "unknown", reason: `could not fold: ${String(err?.message ?? err).split("\n")[0]}` };
  }
  if (recomputed === recorded.frontendSrcHash) {
    return { half: "frontend", verdict: "match", recorded: recorded.frontendSrcHash };
  }
  const before = new Map(recorded.inputs.map((i) => [i.path, i.oid]));
  const differing = now.filter((i) => before.get(i.path) !== i.oid).map((i) => i.path);
  return {
    half: "frontend",
    verdict: "mismatch",
    kind: "src_changed_since_frontend_build",
    recorded: recorded.frontendSrcHash,
    recomputed,
    differing: differing.slice(0, 10),
    differingCount: differing.length,
    reason: "`src` changed since the last frontend build - rebuild the frontend (`pnpm run build:exe`)",
  };
}

/**
 * Verify the Rust half of `root` against what the exe embeds: `expectRust` is
 * `/health` `provenance.rustSrcHash`, `expectSha` is `provenance.gitSha`.
 */
export function verifyRust(root, { expectRust, expectSha } = {}) {
  if (!expectRust || expectRust === UNKNOWN) {
    return { half: "rust", verdict: "unknown", reason: "the exe carries no rustSrcHash (built before provenance stamping, or git was unavailable at build time)" };
  }
  let head;
  let recomputed;
  try {
    head = gitHead(root);
    recomputed = computeRustSrcHash(root);
  } catch (err) {
    return { half: "rust", verdict: "unknown", reason: `could not measure the worktree: ${String(err?.message ?? err).split("\n")[0]}` };
  }
  const shaAgrees = expectSha && expectSha !== UNKNOWN
    ? head.startsWith(expectSha) || expectSha.startsWith(head)
    : null;
  if (recomputed === expectRust) {
    return { half: "rust", verdict: "match", recorded: expectRust, headSha: head, gitShaAgrees: shaAgrees };
  }
  return {
    half: "rust",
    verdict: "mismatch",
    kind: "rust_src_changed_since_build",
    recorded: expectRust,
    recomputed,
    headSha: head,
    gitShaAgrees: shaAgrees,
    // Also the answer for an exe from a bare `cargo build` after a Rust edit:
    // build.rs stamps rustSrcHash when its script re-runs, which `build:exe`
    // always makes happen; the supervisor's frontend-failed path may not (see
    // build.rs, "What these stamps can and cannot see").
    reason: "Rust sources differ from the ones the exe's provenance describes - rebuild with `pnpm run build:exe` (a bare `cargo build` does not re-stamp)",
  };
}

function parseArgs(argv) {
  const out = { _: [] };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a.startsWith("--")) out[a.slice(2)] = argv[++i];
    else out._.push(a);
  }
  return out;
}

const EXIT = { match: 0, mismatch: 1, unknown: 2 };

function worst(verdicts) {
  if (verdicts.some((v) => v.verdict === "mismatch")) return "mismatch";
  if (verdicts.some((v) => v.verdict === "unknown")) return "unknown";
  return "match";
}

/**
 * CLI. One JSON line on stdout; exit 0 match / 1 mismatch / 2 unknown / 64 usage.
 *   verify   --root <repo> [--expect-frontend <h>] [--expect-rust <h>] [--expect-sha <s>]
 *   rust-src-hash --root <repo>
 */
export function main(argv) {
  const args = parseArgs(argv);
  const cmd = args._[0];
  const root = path.resolve(args.root ?? process.cwd());
  if (cmd === "rust-src-hash") {
    try {
      process.stdout.write(JSON.stringify({ rustSrcHash: computeRustSrcHash(root) }) + "\n");
      return 0;
    } catch (err) {
      process.stdout.write(JSON.stringify({ rustSrcHash: UNKNOWN, reason: String(err?.message ?? err).split("\n")[0] }) + "\n");
      return 2;
    }
  }
  if (cmd === "verify") {
    const verdicts = [verifyFrontend(root, { expect: args["expect-frontend"] })];
    if (args["expect-rust"] !== undefined || args["expect-sha"] !== undefined) {
      verdicts.push(verifyRust(root, { expectRust: args["expect-rust"], expectSha: args["expect-sha"] }));
    }
    const verdict = worst(verdicts);
    process.stdout.write(JSON.stringify({ verdict, root, halves: verdicts }) + "\n");
    return EXIT[verdict];
  }
  process.stderr.write(
    "usage: frontend-provenance.mjs verify --root <repo> [--expect-frontend <h>] [--expect-rust <h>] [--expect-sha <s>]\n" +
      "       frontend-provenance.mjs rust-src-hash --root <repo>\n",
  );
  return 64;
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  process.exit(main(process.argv.slice(2)));
}

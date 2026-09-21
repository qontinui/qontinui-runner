// Tests for scripts/frontend-provenance.mjs — run with
// `node --test scripts/__tests__/frontend-provenance.test.mjs`.
import { test } from "node:test";
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdtempSync, mkdirSync, readFileSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

import {
  computeFrontendProvenance,
  computeRustSrcHash,
  foldLines,
  frontendInputs,
  unignoredPaths,
  verifyFrontend,
  verifyRust,
} from "../frontend-provenance.mjs";

const here = path.dirname(fileURLToPath(import.meta.url));

function repo() {
  const dir = mkdtempSync(path.join(os.tmpdir(), "prov-"));
  const g = (...a) => execFileSync("git", ["-C", dir, ...a], { stdio: "pipe" });
  g("init", "-q");
  g("config", "user.email", "t@example.com");
  g("config", "user.name", "t");
  g("config", "core.autocrlf", "false");
  writeFileSync(path.join(dir, ".gitignore"), "node_modules\ndist\n");
  mkdirSync(path.join(dir, "src"));
  mkdirSync(path.join(dir, "node_modules/dep"), { recursive: true });
  mkdirSync(path.join(dir, "src-tauri/src"), { recursive: true });
  writeFileSync(path.join(dir, "src/App.tsx"), "export const a = 1;\n");
  writeFileSync(path.join(dir, "src/util.ts"), "export const b = 2;\n");
  writeFileSync(path.join(dir, "index.html"), "<html></html>\n");
  writeFileSync(path.join(dir, "vite.config.ts"), "export default {};\n");
  writeFileSync(path.join(dir, "pnpm-lock.yaml"), "lockfileVersion: 9\n");
  writeFileSync(path.join(dir, "node_modules/dep/index.js"), "module.exports = 1;\n");
  writeFileSync(path.join(dir, "src-tauri/src/main.rs"), "fn main() {}\n");
  writeFileSync(path.join(dir, "src-tauri/Cargo.toml"), "[package]\n");
  writeFileSync(path.join(dir, "Cargo.lock"), "# lock\n");
  g("add", "-A");
  g("commit", "-qm", "init");
  return dir;
}

function build(dir) {
  const ids = [
    path.join(dir, "src/App.tsx"),
    path.join(dir, "src/util.ts") + "?inline",
    path.join(dir, "node_modules/dep/index.js"),
    "\0virtual:thing",
    path.join(dir, "..", "outside.ts"),
  ];
  const prov = computeFrontendProvenance({ root: dir, moduleIds: ids, buildId: "abc-1" });
  mkdirSync(path.join(dir, "dist"), { recursive: true });
  writeFileSync(path.join(dir, "dist/provenance.json"), JSON.stringify(prov));
  return prov;
}

test("fold format matches the shared fixture build.rs is pinned to", () => {
  const fx = JSON.parse(readFileSync(path.join(here, "../fixtures/provenance-fold.json"), "utf8"));
  assert.equal(foldLines(fx.entries), fx.expected_fold);
});

test("input set: module graph ∩ git-unignored, plus explicit config inputs", () => {
  const dir = repo();
  try {
    const ids = [path.join(dir, "src/App.tsx"), path.join(dir, "node_modules/dep/index.js"), "\0x"];
    assert.deepEqual(frontendInputs(dir, ids), { inputs: ["index.html", "src/App.tsx", "vite.config.ts"], fromGraph: 1 });
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("unchanged tree folds identically twice; an edit changes it; a touch does not", () => {
  const dir = repo();
  try {
    const a = build(dir);
    const b = build(dir);
    assert.notEqual(a.frontendSrcHash, "unknown");
    assert.equal(a.frontendSrcHash, b.frontendSrcHash);
    assert.ok(!a.inputs.some((i) => i.oid === null), "no input absent on a clean tree");
    assert.match(a.depLockOid, /^[0-9a-f]{40}$/);
    assert.equal(a.gitDirty, false);
    // touch with identical bytes: still a match
    writeFileSync(path.join(dir, "src/App.tsx"), "export const a = 1;\n");
    assert.equal(verifyFrontend(dir).verdict, "match");
    writeFileSync(path.join(dir, "src/App.tsx"), "export const a = 2;\n");
    const v = verifyFrontend(dir);
    assert.equal(v.verdict, "mismatch");
    assert.equal(v.kind, "src_changed_since_frontend_build");
    assert.deepEqual(v.differing, ["src/App.tsx"]);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("a deleted input is `absent`, and is a mismatch", () => {
  const dir = repo();
  try {
    build(dir);
    rmSync(path.join(dir, "src/util.ts"));
    const v = verifyFrontend(dir);
    assert.equal(v.verdict, "mismatch");
    assert.deepEqual(v.differing, ["src/util.ts"]);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("exe embedding a different dist than the one on disk is its own mismatch", () => {
  const dir = repo();
  try {
    build(dir);
    const v = verifyFrontend(dir, { expect: "0".repeat(40) });
    assert.equal(v.verdict, "mismatch");
    assert.equal(v.kind, "exe_embeds_different_dist");
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("no module-graph file under root is UNKNOWN, never a config-only hash", () => {
  const dir = repo();
  try {
    const p = computeFrontendProvenance({ root: dir, moduleIds: ["/elsewhere/src/App.tsx"], buildId: "x" });
    assert.equal(p.frontendSrcHash, "unknown");
    assert.match(p.unknownReason, /no module-graph file/);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("a directory (e.g. an untracked symlink to one) hashes as absent, not a crash", () => {
  const dir = repo();
  try {
    // git lists an untracked symlink as a FILE but cannot hash-object it
    // when it points at a directory.
    symlinkSync(path.join(dir, "src"), path.join(dir, "src-tauri/src/link"));
    assert.ok(unignoredPaths(dir, ["src-tauri/src"]).has("src-tauri/src/link"));
    assert.match(computeRustSrcHash(dir), /^[0-9a-f]{40}$/);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("absent provenance.json and no-git are UNKNOWN, never mismatch (D3/D4)", () => {
  const dir = repo();
  try {
    assert.equal(verifyFrontend(dir).verdict, "unknown");
    const noGit = mkdtempSync(path.join(os.tmpdir(), "prov-nogit-"));
    const p = computeFrontendProvenance({ root: noGit, moduleIds: [], buildId: "x" });
    assert.equal(p.frontendSrcHash, "unknown");
    assert.ok(p.unknownReason);
    rmSync(noGit, { recursive: true, force: true });
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("rust half: match, then mismatch after an uncommitted .rs edit; absent expectation is UNKNOWN", () => {
  const dir = repo();
  try {
    const h = computeRustSrcHash(dir);
    assert.match(h, /^[0-9a-f]{40}$/);
    assert.equal(verifyRust(dir, { expectRust: h }).verdict, "match");
    writeFileSync(path.join(dir, "src-tauri/src/main.rs"), "fn main() { let _ = 1; }\n");
    assert.equal(verifyRust(dir, { expectRust: h }).verdict, "mismatch");
    // frontend edits do not move the Rust hash
    writeFileSync(path.join(dir, "src-tauri/src/main.rs"), "fn main() {}\n");
    writeFileSync(path.join(dir, "src/App.tsx"), "changed\n");
    assert.equal(computeRustSrcHash(dir), h);
    assert.equal(verifyRust(dir, {}).verdict, "unknown");
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

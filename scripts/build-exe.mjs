#!/usr/bin/env node
// `pnpm run build:exe` — build BOTH halves of the runner in one command (plan
// 2026-08-23-build-provenance-assertion, Phase 5): the Vite frontend into
// dist/, then the exe that embeds it.
//
// It also sets QONTINUI_PROVENANCE_NONCE to a fresh value for the cargo step.
// src-tauri/build.rs declares `rerun-if-env-changed` on it, so every build:exe
// re-runs the build script and re-stamps the tree-state provenance
// (gitDirty / treeHash / rustSrcHash) -- whether or not the Vite step changed
// dist/. Without it, a Rust edit followed by a build whose frontend step did
// not rewrite dist/ would embed the new code under the previous run's stamps.
// Watching `src` instead would re-run the script (and rebuild every target of
// the package) on every rust-analyzer check; a nonce set only here costs
// nothing outside a sanctioned build.
import { spawnSync } from "node:child_process";
import path from "node:path";
import { fileURLToPath } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const isWin = process.platform === "win32";

function run(cmd, args, env) {
  // Windows needs a shell to resolve pnpm.cmd; pass it ONE command string there
  // (the arguments are constants), since args + shell is deprecated (DEP0190).
  const r = isWin
    ? spawnSync([cmd, ...args].join(" "), { cwd: root, stdio: "inherit", env, shell: true })
    : spawnSync(cmd, args, { cwd: root, stdio: "inherit", env });
  if (r.error) {
    console.error(`build:exe: could not run ${cmd}: ${r.error.message}`);
    process.exit(1);
  }
  if (r.status !== 0) process.exit(r.status ?? 1);
}

run("pnpm", ["run", "build"], process.env);
run("cargo", ["build", "--bin", "qontinui-runner", "--features", "custom-protocol"], {
  ...process.env,
  QONTINUI_PROVENANCE_NONCE: `${Date.now()}-${process.pid}`,
});

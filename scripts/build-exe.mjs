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
//
// It builds in the `release-checked` cargo profile by default (plan
// 2026-10-19-primary-runner-is-a-debug-build-and-nothing-shows-it, Phase 3):
// release's optimization with debug assertions kept. `--profile <name>` or
// QONTINUI_RUNNER_BUILD_PROFILE=<name> picks another; `dev` is the announced
// debug opt-in. The sidecars the runner resolves beside its own exe
// (scripts/lib/sidecars.mjs PRIMARY_BINS) are built in the SAME cargo
// invocation and profile, and each is checked to be a real native executable
// before this script exits 0 -- a fresh profile dir would otherwise hold none.
import { spawnSync } from "node:child_process";
import path from "node:path";
import { fileURLToPath } from "node:url";

import {
  announcement,
  cargoArgs,
  debugOptInWarning,
  executablesFromCargoJson,
  profileDir,
  resolveProfile,
} from "./lib/build-profile.mjs";
import { nativeExecutableProblem } from "./lib/native-exe.mjs";
import { PRIMARY_BINS, SIDECAR_PACKAGES } from "./lib/sidecars.mjs";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const isWin = process.platform === "win32";

function run(cmd, args, env, cwd = root) {
  // Windows needs a shell to resolve pnpm.cmd; pass it ONE command string there
  // (the arguments are constants), since args + shell is deprecated (DEP0190).
  const r = isWin
    ? spawnSync([cmd, ...args].join(" "), { cwd, stdio: "inherit", env, shell: true })
    : spawnSync(cmd, args, { cwd, stdio: "inherit", env });
  if (r.error) {
    console.error(`build:exe: could not run ${cmd}: ${r.error.message}`);
    process.exit(1);
  }
  if (r.status !== 0) process.exit(r.status ?? 1);
}

const chosen = resolveProfile(process.argv.slice(2), process.env);
if (chosen.error) {
  console.error(`build:exe: ${chosen.error}`);
  process.exit(2);
}
const { profile, source } = chosen;
console.log(announcement(profile));
const warning = debugOptInWarning(profile, source);
if (warning) console.warn(warning);

function hostTriple() {
  const r = spawnSync("rustc", ["-vV"], { encoding: "utf8" });
  const m = r.status === 0 ? r.stdout.match(/^host:\s*(\S+)$/m) : null;
  if (!m) {
    console.error("build:exe: could not read the host target triple from `rustc -vV`");
    process.exit(1);
  }
  return m[1];
}

// Read before building: the binaries are checked against it afterwards, and a
// failure here must not cost a whole (possibly ~20-minute) cargo build first.
const triple = hostTriple();

run("pnpm", ["run", "build"], process.env);

// Cargo from src-tauri, NOT the repo root: cargo reads `.cargo/config.toml`
// from its CWD upward, and the runner's lives at src-tauri/.cargo/config.toml
// (/Brepro, the sccache port pin, the /STACK reserve). Run from the root, every
// one of those was silently dropped -- the exe linked with MSVC's 1 MB default
// stack and overflowed on its first IPC call. An unset or ABSOLUTE
// CARGO_TARGET_DIR resolves the same from either directory (unset = the
// workspace root's target/); a RELATIVE one now resolves against src-tauri,
// which is what dev-start.ps1 already assumes when it absolutizes it.
//
// stdout is cargo's JSON artifact stream (so the exact output paths are read,
// never guessed); diagnostics and progress render to the inherited stderr.
const cargo = spawnSync("cargo", cargoArgs(profile, SIDECAR_PACKAGES, PRIMARY_BINS), {
  cwd: path.join(root, "src-tauri"),
  encoding: "utf8",
  stdio: ["ignore", "pipe", "inherit"],
  maxBuffer: 512 * 1024 * 1024,
  env: {
    ...process.env,
    QONTINUI_PROVENANCE_NONCE: `${Date.now()}-${process.pid}`,
  },
});
if (cargo.error) {
  console.error(`build:exe: could not run cargo: ${cargo.error.message}`);
  process.exit(1);
}
if (cargo.status !== 0) process.exit(cargo.status ?? 1);

// Every bin must be a real native executable for the host, in this profile's
// output dir. A missing, empty or foreign file fails the build by name.
const wanted = ["qontinui-runner", ...PRIMARY_BINS];
const built = executablesFromCargoJson(cargo.stdout, wanted);
const problems = [];
for (const bin of wanted) {
  const exe = built.get(bin);
  if (!exe) {
    problems.push(`${bin}: cargo reported no executable`);
    continue;
  }
  const dir = path.basename(path.dirname(exe));
  if (dir !== profileDir(profile)) {
    problems.push(`${bin}: built into ${dir}/, expected ${profileDir(profile)}/ (${exe})`);
    continue;
  }
  const problem = nativeExecutableProblem(exe, triple);
  if (problem) problems.push(`${bin}: ${problem} (${exe})`);
}
if (problems.length > 0) {
  console.error(`build:exe: ${problems.length} built binar${problems.length === 1 ? "y is" : "ies are"} not usable:`);
  for (const p of problems) console.error(`  - ${p}`);
  process.exit(1);
}
console.log(
  `build:exe: profile=${profile} built ${wanted.length} binaries into ${path.dirname(built.get("qontinui-runner"))}`,
);

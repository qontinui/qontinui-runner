#!/usr/bin/env node
// Bundle the runner's CLI sidecars as Tauri externalBin binaries:
//   - `qontinui_profile` (Phase 1(a) of the devenv machine-enrollment UX plan:
//     kill the build-from-source step so every runner install already carries
//     the enroll/capture helper).
//   - `qontinui-pr` (plan qontinui-pr-credential-provisioning, Phase 2b: the
//     session PR CLI the identity-shim materializer hardlinks onto every
//     terminal's PATH — without this sidecar, installed/bundled runners have
//     no binary next to the exe and `qontinui-pr create` never materializes).
//   - `qontinui-pty-holder` (plan
//     2026-09-12-out-of-process-pty-owner-for-terminal-hosted-sessions, Phase 2:
//     the per-pane PTY holder, which the runner spawns from beside its own exe).
//
// `tauri.conf.json` declares `bundle.externalBin: ["binaries/qontinui_profile",
// "binaries/qontinui-pr", "binaries/qontinui-pty-holder"]`. Tauri resolves each to
// `src-tauri/binaries/<name>-<target-triple>[.exe]` at bundle time and copies it
// next to the app binary (as plain `<name>[.exe]`). We cargo-build them in
// release (one invocation over the packages that own them) and copy the
// artifacts to the triple-suffixed paths Tauri expects.
//
// Wired into `beforeBuildCommand` (`tauri.conf.json`), so it runs on every
// `tauri build` (CI release + local bundle) and NOT on `cargo build`/`cargo check`.
// Those cargo-only builds (the supervisor's debug-exe rebuild, CI `cargo test`)
// have no sidecars here, and externalBin is NOT bundling-only: tauri-build's
// build script copies every externalBin into the cargo target dir on every run.
// `src-tauri/build.rs` `hide_external_bin_from_tauri_build` therefore hides
// externalBin from tauri-build on every run; the bundler still reads
// `binaries/` itself, through the config the tauri CLI parsed. Until 2026-09-27
// build.rs wrote 0-byte placeholders instead, which tauri-build copied over the
// real `qontinui-pr` and the runner then published onto PATH as a CLI that exits
// 0 having opened no PR (plan
// 2026-09-27-qontinui-pr-zero-byte-sidecar-placeholder-published-as-session-cli).
// So every sidecar this script writes is checked to be a real native executable
// for the target (scripts/lib/native-exe.mjs) before the bundler can see it.
// Fails LOUD: a missing binary at bundle time would abort `tauri build`
// anyway, so surfacing the cause here (with the exact cargo error) is strictly
// better than a downstream "external binary not found".
//
// Target triple: derived from `rustc -vV` host. The runner's release matrix builds
// natively per-OS (host == target), so host triple is correct there. Cross-compile
// (`tauri build --target <other>`) is NOT handled — pass QONTINUI_SIDECAR_TARGET to
// override the triple in that case.

import { execFileSync } from "node:child_process";
import { copyFileSync, mkdirSync, existsSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { nativeExecutableProblem } from "./lib/native-exe.mjs";

const scriptDir = dirname(fileURLToPath(import.meta.url));
const runnerRoot = resolve(scriptDir, "..");
const srcTauri = join(runnerRoot, "src-tauri");

// Keep in sync with `tauri.conf.json` `bundle.externalBin` and
// `src-tauri/build.rs` `EXTERNAL_BIN_SIDECARS` (a build.rs test pins the
// latter to tauri.conf.json).
const SIDECAR_BINS = ["qontinui_profile", "qontinui-pr", "qontinui-pty-holder"];

// The cargo PACKAGES those bins live in. `qontinui-pty-holder` (plan
// 2026-09-12-out-of-process-pty-owner-for-terminal-hosted-sessions, Phase 2:
// the per-pane PTY holder the runner spawns from beside its own exe) is a bin
// of `crates/pty-holder`, not of this crate, so the build selects both
// packages; cargo applies the `--bin` filters across every selected package.
const SIDECAR_PACKAGES = ["qontinui-runner", "qontinui-pty-holder"];

function fail(msg) {
  console.error(`\n[bundle-profile-sidecar] ERROR: ${msg}\n`);
  process.exit(1);
}

// 1. Resolve the target triple (host, unless explicitly overridden).
function resolveTriple() {
  const override = process.env.QONTINUI_SIDECAR_TARGET?.trim();
  if (override) return override;
  let out;
  try {
    out = execFileSync("rustc", ["-vV"], { encoding: "utf8" });
  } catch (e) {
    fail(`could not run \`rustc -vV\` to derive the host target triple: ${e.message}`);
  }
  const m = out.match(/^host:\s*(\S+)$/m);
  if (!m) fail("could not parse a `host:` line out of `rustc -vV`");
  return m[1];
}

const triple = resolveTriple();
const isWindows = triple.includes("windows");
const exeExt = isWindows ? ".exe" : "";

// 2. Build the sidecar bins in release (one cargo invocation), capturing cargo's
//    JSON artifact stream so we learn the EXACT output paths. Do NOT assume
//    `src-tauri/target/` — this repo's cargo target dir is the workspace root
//    `target/` (and CI/CARGO_TARGET_DIR or a `--target` subdir can move it
//    further), so guessing the path is what broke CI. `json-render-diagnostics`
//    keeps machine JSON on stdout while rendering warnings/errors to stderr
//    (which we inherit for visibility).
console.log(
  `[bundle-profile-sidecar] cargo build --release ${SIDECAR_PACKAGES.map((p) => `-p ${p}`).join(" ")} ${SIDECAR_BINS.map((b) => `--bin ${b}`).join(" ")} (target=${triple})`,
);
let stdout;
try {
  stdout = execFileSync(
    "cargo",
    [
      "build",
      "--release",
      ...SIDECAR_PACKAGES.flatMap((p) => ["-p", p]),
      ...SIDECAR_BINS.flatMap((b) => ["--bin", b]),
      "--message-format=json-render-diagnostics",
    ],
    {
      cwd: srcTauri,
      encoding: "utf8",
      stdio: ["ignore", "pipe", "inherit"],
      maxBuffer: 512 * 1024 * 1024,
    },
  );
} catch (e) {
  fail(`cargo build of ${SIDECAR_BINS.join(", ")} failed: ${e.message}`);
}

// 3. Extract each executable path from the last matching compiler-artifact
//    message — the authoritative locations cargo actually wrote them to.
const builtExes = new Map(); // bin name -> executable path
for (const line of stdout.split("\n")) {
  const s = line.trim();
  if (!s.startsWith("{")) continue;
  let msg;
  try {
    msg = JSON.parse(s);
  } catch {
    continue;
  }
  if (
    msg.reason === "compiler-artifact" &&
    msg.target &&
    SIDECAR_BINS.includes(msg.target.name) &&
    msg.executable
  ) {
    builtExes.set(msg.target.name, msg.executable);
  }
}

// 4. Copy each to the triple-suffixed path Tauri's externalBin resolver expects.
const binariesDir = join(srcTauri, "binaries");
mkdirSync(binariesDir, { recursive: true });
for (const bin of SIDECAR_BINS) {
  const builtExe = builtExes.get(bin);
  if (!builtExe || !existsSync(builtExe)) {
    fail(
      `could not locate the built ${bin} executable from cargo's JSON output (parsed: ${builtExe})`,
    );
  }
  const dest = join(binariesDir, `${bin}-${triple}${exeExt}`);
  copyFileSync(builtExe, dest);
  // Validate what LANDED, not only what cargo reported: an empty or non-native
  // sidecar must fail the bundle here rather than ship as a silent no-op CLI.
  const problem = nativeExecutableProblem(dest, triple);
  if (problem) {
    fail(`${dest} is not a real ${bin} executable for ${triple}: ${problem} (copied from ${builtExe})`);
  }
  console.log(`[bundle-profile-sidecar] wrote sidecar -> ${dest}`);
}

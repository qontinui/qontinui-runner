#!/usr/bin/env node
// Bundle the runner's CLI sidecars as Tauri externalBin binaries:
//   - `qontinui_profile` (Phase 1(a) of the devenv machine-enrollment UX plan:
//     kill the build-from-source step so every runner install already carries
//     the enroll/capture helper).
//   - `qontinui-pr` (plan qontinui-pr-credential-provisioning, Phase 2b: the
//     session PR CLI the identity-shim materializer hardlinks onto every
//     terminal's PATH — without this sidecar, installed/bundled runners have
//     no binary next to the exe and `qontinui-pr create` never materializes).
//
// `tauri.conf.json` declares `bundle.externalBin: ["binaries/qontinui_profile",
// "binaries/qontinui-pr"]`. Tauri resolves each to
// `src-tauri/binaries/<name>-<target-triple>[.exe]` at bundle time and copies it
// next to the app binary (as plain `<name>[.exe]`). Both are `[[bin]]`s in the
// SAME cargo crate, so we just cargo-build them in release and copy the
// artifacts to the triple-suffixed paths Tauri expects.
//
// Wired into `beforeBuildCommand` (`tauri.conf.json`), so it runs on every
// `tauri build` (CI release + local bundle) and NOT on `cargo build`/`cargo check`
// (the supervisor's debug-exe rebuild path is unaffected — externalBin only gates
// bundling). Fails LOUD: a missing binary at bundle time would abort `tauri build`
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

const scriptDir = dirname(fileURLToPath(import.meta.url));
const runnerRoot = resolve(scriptDir, "..");
const srcTauri = join(runnerRoot, "src-tauri");

// Keep in sync with `tauri.conf.json` `bundle.externalBin` and
// `src-tauri/build.rs` `ensure_sidecar_placeholders`.
const SIDECAR_BINS = ["qontinui_profile", "qontinui-pr"];

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
  `[bundle-profile-sidecar] cargo build --release ${SIDECAR_BINS.map((b) => `--bin ${b}`).join(" ")} (target=${triple})`,
);
let stdout;
try {
  stdout = execFileSync(
    "cargo",
    [
      "build",
      "--release",
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
  console.log(`[bundle-profile-sidecar] wrote sidecar -> ${dest}`);
}

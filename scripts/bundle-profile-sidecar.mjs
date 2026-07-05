#!/usr/bin/env node
// Bundle the `qontinui_profile` CLI as a Tauri sidecar (Phase 1(a) of the devenv
// machine-enrollment UX plan: kill the build-from-source step so every runner
// install already carries the enroll/capture helper).
//
// `tauri.conf.json` declares `bundle.externalBin: ["binaries/qontinui_profile"]`.
// Tauri resolves that to `src-tauri/binaries/qontinui_profile-<target-triple>[.exe]`
// at bundle time and copies it next to the app binary. `qontinui_profile` is a
// `[[bin]]` in the SAME cargo crate, so we just cargo-build it in release and copy
// the artifact to the triple-suffixed path Tauri expects.
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

// 2. Build the sidecar bin in release, capturing cargo's JSON artifact stream so
//    we learn the EXACT output path. Do NOT assume `src-tauri/target/` — this
//    repo's cargo target dir is the workspace root `target/` (and CI/CARGO_TARGET_DIR
//    or a `--target` subdir can move it further), so guessing the path is what
//    broke CI. `json-render-diagnostics` keeps machine JSON on stdout while
//    rendering warnings/errors to stderr (which we inherit for visibility).
console.log(`[bundle-profile-sidecar] cargo build --release --bin qontinui_profile (target=${triple})`);
let stdout;
try {
  stdout = execFileSync(
    "cargo",
    [
      "build",
      "--release",
      "--bin",
      "qontinui_profile",
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
  fail(`cargo build of qontinui_profile failed: ${e.message}`);
}

// 3. Extract the executable path from the last matching compiler-artifact
//    message — the authoritative location cargo actually wrote it to.
let builtExe = null;
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
    msg.target.name === "qontinui_profile" &&
    msg.executable
  ) {
    builtExe = msg.executable;
  }
}
if (!builtExe || !existsSync(builtExe)) {
  fail(
    `could not locate the built qontinui_profile executable from cargo's JSON output (parsed: ${builtExe})`,
  );
}

// 4. Copy to the triple-suffixed path Tauri's externalBin resolver expects.
const binariesDir = join(srcTauri, "binaries");
mkdirSync(binariesDir, { recursive: true });
const dest = join(binariesDir, `qontinui_profile-${triple}${exeExt}`);
copyFileSync(builtExe, dest);
console.log(`[bundle-profile-sidecar] wrote sidecar -> ${dest}`);

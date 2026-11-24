#!/usr/bin/env node
// Drop cached `postgresql_embedded` build units whose embedded PostgreSQL
// release is not the one pinned in src-tauri/.cargo/config.toml
// (POSTGRESQL_VERSION).
//
// WHY: the crate's `bundled` build script returns early when its OUT_DIR
// already holds an archive. Cargo does re-run the script when the pin changes,
// but OUT_DIR persists across runs, so a restored CI cache (or any warm or
// persistent target dir) keeps embedding the old release. Measured 2026-10-19:
// main's ubuntu-22.04 rust-cache entry, saved from a failing run, held 18.6.1,
// whose initdb needs GLIBC 2.36. Removing the profile's
// build/postgresql_embedded-* and .fingerprint/postgresql_embedded-* dirs makes
// cargo rebuild the crate and run the script against an empty OUT_DIR.
//
// Node, not bash, on purpose: the runner CI-node lane spawns argv programs, and
// on Windows `bash` resolves to System32's WSL launcher, which neither sees the
// Windows CARGO_TARGET_DIR nor the Windows paths. Node built-ins only.
//
// Usage: node scripts/ci/purge-stale-bundled-postgres.mjs [target-dir]
//   target-dir defaults to $CARGO_TARGET_DIR (the CI-node executor exports its
//   persistent per-repo target dir there), else <repo>/target (Actions).
// Exit: 0 = done (purged or nothing to purge), 1 = no exact pin found.
import { existsSync, readFileSync, readdirSync, rmSync, statSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..", "..");
const config = join(root, "src-tauri", ".cargo", "config.toml");
const target = resolve(process.argv[2] || process.env.CARGO_TARGET_DIR || join(root, "target"));

const pinLine = readFileSync(config, "utf8")
  .split(/\r?\n/)
  .map((line) => /^\s*POSTGRESQL_VERSION\s*=\s*"=(\d+\.\d+\.\d+)"/.exec(line))
  .find(Boolean);
if (!pinLine) {
  console.log(`::error::no exact POSTGRESQL_VERSION = "=X.Y.Z" pin in ${config}`);
  process.exit(1);
}
const pin = pinLine[1];
console.log(`bundled PostgreSQL pin: ${pin} (target dir: ${target})`);

const isDir = (p) => {
  try {
    return statSync(p).isDirectory();
  } catch {
    return false;
  }
};
const children = (p) => (isDir(p) ? readdirSync(p).map((name) => join(p, name)) : []);

// Profile dirs: target/<profile> and target/<triple>/<profile>, i.e. any dir
// one or two levels down that holds a build/ dir.
const profiles = [];
for (const a of children(target)) {
  if (isDir(join(a, "build"))) profiles.push(a);
  for (const b of children(a)) if (isDir(join(b, "build"))) profiles.push(b);
}

let purged = 0;
for (const profile of profiles) {
  const units = children(join(profile, "build")).filter((p) =>
    /[\\/]postgresql_embedded-[^\\/]+$/.test(p),
  );
  const stale = [];
  for (const unit of units) {
    const versionFile = join(unit, "out", "postgresql.version");
    if (!existsSync(versionFile)) continue;
    const have = readFileSync(versionFile, "utf8").trim();
    if (have === pin) {
      console.log(`match ${unit} (embeds ${have})`);
    } else {
      stale.push(`${unit} embeds ${have}`);
    }
  }
  if (stale.length === 0) continue;
  // Purge the WHOLE profile's postgresql_embedded units, matching ones too: the
  // cost is one extra rebuild, and no fingerprint is left half-present.
  console.log(`purge ${profile}/{build,.fingerprint}/postgresql_embedded-* (${stale.join("; ")})`);
  for (const sub of ["build", ".fingerprint"]) {
    for (const p of children(join(profile, sub))) {
      if (/[\\/]postgresql_embedded-[^\\/]+$/.test(p)) rmSync(p, { recursive: true, force: true });
    }
  }
  purged += 1;
}
console.log(`bundled PostgreSQL: purged ${purged} profile(s) holding a release other than ${pin}`);

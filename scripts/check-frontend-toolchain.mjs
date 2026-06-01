#!/usr/bin/env node
// Frontend toolchain preflight guard (runs as `prebuild`).
//
// ROOT CAUSE this guards against: the pnpm store (node_modules/.pnpm) can
// corrupt (e.g. ENOENT on @babel/parser, typically from worktree-junction
// churn), which leaves node_modules/.bin/tsc and/or vite missing or
// unresolvable. The old `"build": "tsc && vite build"` then failed with
// `'tsc' is not recognized` (PATH lookup) — but in some CI/cargo paths a
// partially-built or stale `dist/` was embedded anyway, producing a BLANK
// runner ("asset not found: index.html"). The failure was silent and only
// caught far downstream.
//
// This guard verifies the toolchain is actually present/resolvable BEFORE
// `tsc`/`vite` run, and exits NON-ZERO with a clear, actionable message if
// not. Pure Node, no dependencies.

import { existsSync } from "node:fs";
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const __dirname = dirname(fileURLToPath(import.meta.url));
const repoRoot = join(__dirname, "..");
const require = createRequire(join(repoRoot, "package.json"));

const failures = [];

// 1. TypeScript: the package must be resolvable AND the tsc bin must exist.
//    (A resolvable package whose .bin is gone is exactly the corruption mode.)
const binExt = process.platform === "win32" ? ".CMD" : "";
const tscBin = join(repoRoot, "node_modules", ".bin", `tsc${binExt}`);
const tscBinPlain = join(repoRoot, "node_modules", ".bin", "tsc");
if (!existsSync(tscBin) && !existsSync(tscBinPlain)) {
  failures.push("tsc binary missing (node_modules/.bin/tsc)");
}
try {
  require.resolve("typescript/package.json");
} catch {
  failures.push("typescript package not resolvable");
}

// 2. Vite: the bin must exist AND the package must be resolvable.
const viteBin = join(repoRoot, "node_modules", ".bin", `vite${binExt}`);
const viteBinPlain = join(repoRoot, "node_modules", ".bin", "vite");
if (!existsSync(viteBin) && !existsSync(viteBinPlain)) {
  failures.push("vite binary missing (node_modules/.bin/vite)");
}
try {
  require.resolve("vite/package.json");
} catch {
  failures.push("vite package not resolvable");
}

if (failures.length > 0) {
  console.error(
    "\n" +
      "================================================================\n" +
      " Frontend toolchain missing (tsc/vite not resolvable).\n" +
      "================================================================\n" +
      failures.map((f) => `  - ${f}`).join("\n") +
      "\n\n" +
      " The pnpm store (node_modules/.pnpm) is likely corrupted — building\n" +
      " now would emit a BLANK dist (no index.html) and ship a blank runner.\n\n" +
      " Fix it:  pnpm install --force\n" +
      " (do NOT `rm -rf node_modules` — a harness guard blocks it)\n" +
      "================================================================\n"
  );
  process.exit(1);
}

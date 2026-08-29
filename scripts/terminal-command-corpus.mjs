#!/usr/bin/env node
/**
 * CLI front-end for the terminal CommandBar differential harness.
 *
 * The harness itself lives in
 * `src/components/terminal/commands/differential.testkit.ts` and runs under
 * vitest, because it needs vitest's module mocking to register the REAL
 * action set with stubbed context closures. This script is a thin wrapper so
 * the three workflows have a spelling that does not require remembering the
 * environment variables.
 *
 *   # Regenerate the committed golden characterization. The resulting
 *   # `git diff` is the review artifact.
 *   node scripts/terminal-command-corpus.mjs --update
 *
 *   # Emit a snapshot to an arbitrary path (for cross-commit comparison).
 *   node scripts/terminal-command-corpus.mjs --out /tmp/base.txt --tier fast
 *
 *   # Compare the current checkout against a snapshot taken elsewhere, and
 *   # print the four-class delta (strict and lenient arms).
 *   node scripts/terminal-command-corpus.mjs --baseline /tmp/base.txt --tier fast
 *   #   add --strict to make a non-zero strict count fail the process
 *
 * Cross-commit recipe — both sides run their own real modules and handlers:
 *
 *   git checkout <base>
 *   node scripts/terminal-command-corpus.mjs --out /tmp/base.txt --tier fast
 *   git checkout <head>
 *   node scripts/terminal-command-corpus.mjs --baseline /tmp/base.txt --tier fast
 *
 * Tiers: `golden` (~2.1k inputs), `fast` (~7.3k, the default), `full` (~92k).
 */

import { spawnSync } from "node:child_process";
import { createRequire } from "node:module";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const SPEC = "src/components/terminal/commands/differential.test.ts";

const argv = process.argv.slice(2);
const flag = (name) => argv.includes(name);
const value = (name) => {
  const i = argv.indexOf(name);
  return i === -1 ? undefined : argv[i + 1];
};

if (flag("--help") || flag("-h")) {
  process.stdout.write(
    [
      "usage: node scripts/terminal-command-corpus.mjs [options]",
      "",
      "  --update              rewrite the committed golden characterization",
      "  --out <path>          write a snapshot to <path>",
      "  --baseline <path>     diff the current checkout against <path>",
      "  --tier <t>            golden | fast | full   (default: fast)",
      "  --strict              fail the process when the strict delta is non-zero",
      "",
    ].join("\n"),
  );
  process.exit(0);
}

const tier = value("--tier") ?? "fast";
if (!["golden", "fast", "full"].includes(tier)) {
  process.stderr.write(`unknown tier: ${tier}\n`);
  process.exit(2);
}

const env = { ...process.env, TERMINAL_SNAPSHOT_TIER: tier };
// The sweep tier drives how much work the spec does; `full` is opt-in.
if (tier === "full") env.TERMINAL_CORPUS = "full";
if (flag("--update")) env.TERMINAL_GOLDEN_UPDATE = "1";
const out = value("--out");
if (out) env.TERMINAL_SNAPSHOT_OUT = resolve(out);
const baseline = value("--baseline");
if (baseline) env.TERMINAL_DIFF_BASELINE = resolve(baseline);
if (flag("--strict")) env.TERMINAL_DIFF_STRICT = "1";

// Spawn vitest's ESM entry with `node` directly rather than through `npx`:
// on Windows `npx` is a `.cmd` shim, which Node refuses to spawn without a
// shell, and the failure is SILENT (exit 0, no output) — the same
// false-green shape this whole harness exists to eliminate.
const require = createRequire(import.meta.url);
let bin;
try {
  bin = resolve(dirname(require.resolve("vitest/package.json")), "vitest.mjs");
} catch {
  process.stderr.write("vitest is not installed — run `pnpm install` first\n");
  process.exit(2);
}
const res = spawnSync(process.execPath, [bin, "run", SPEC, "--reporter=verbose"], {
  cwd: ROOT,
  env,
  stdio: "inherit",
});
if (res.error) {
  process.stderr.write(`failed to start vitest: ${res.error.message}\n`);
  process.exit(2);
}
process.exit(res.status ?? 1);

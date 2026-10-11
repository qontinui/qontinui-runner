/**
 * Source-scan guard (plan
 * `2026-09-20-terminal-session-state-comes-from-events-not-screen-scraping`,
 * Phase 5): no file under `src/components/terminal` compares a state against
 * `"needs-input"` directly — outside `agentTruth.ts`, the selector module.
 *
 * Display code asks `isNeedsInputState`; anything that TYPES into a pane asks
 * `isAuthoritativePermissionAsk`. With the literal comparison gone from every
 * other file, a new keystroke writer cannot quietly re-derive "needs input"
 * from the regex-inferred chip and skip the confidence check.
 *
 * Comment lines (`//`, ` * `) are skipped so a doc comment may quote the
 * defect it replaced. Test files are skipped: they assert on the chip value.
 */

import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, relative } from "node:path";
import { fileURLToPath } from "node:url";

import { describe, it, expect } from "vitest";

const ROOT = fileURLToPath(new URL(".", import.meta.url));
const SELECTOR_MODULE = "agentTruth.ts";
const COMPARISON = /[!=]==?\s*["']needs-input["']|["']needs-input["']\s*[!=]==?/;

function walk(dir: string): string[] {
  const out: string[] = [];
  for (const name of readdirSync(dir)) {
    const path = join(dir, name);
    if (statSync(path).isDirectory()) {
      if (name === "node_modules" || name.startsWith("__")) continue;
      out.push(...walk(path));
    } else if (/\.(ts|tsx)$/.test(name) && !/\.test\.(ts|tsx)$/.test(name)) {
      out.push(path);
    }
  }
  return out;
}

function isCommentLine(line: string): boolean {
  const t = line.trimStart();
  return t.startsWith("//") || t.startsWith("*") || t.startsWith("/*");
}

describe("needs-input comparisons go through the selector module", () => {
  const files = walk(ROOT);

  it("scans a non-trivial set of files (guards against a vacuous pass)", () => {
    expect(files.length).toBeGreaterThan(50);
    expect(files.some((f) => f.endsWith(SELECTOR_MODULE))).toBe(true);
  });

  it("finds no `=== \"needs-input\"` outside agentTruth.ts", () => {
    const offenders: string[] = [];
    for (const file of files) {
      const rel = relative(ROOT, file);
      if (rel === SELECTOR_MODULE) continue;
      readFileSync(file, "utf8")
        .split("\n")
        .forEach((line, i) => {
          if (!isCommentLine(line) && COMPARISON.test(line)) offenders.push(`${rel}:${i + 1}: ${line.trim()}`);
        });
    }
    expect(offenders).toEqual([]);
  });

  it("the pattern really catches the forms it forbids", () => {
    expect(COMPARISON.test('if (sessionStates[t.id] === "needs-input")')).toBe(true);
    expect(COMPARISON.test("if (s !== 'needs-input')")).toBe(true);
    expect(COMPARISON.test('"needs-input" === state')).toBe(true);
    expect(COMPARISON.test('return "needs-input";')).toBe(false);
  });

  it("the selector module is where the comparison lives", () => {
    const source = readFileSync(join(ROOT, SELECTOR_MODULE), "utf8");
    expect(COMPARISON.test(source)).toBe(true);
    expect(source).toContain("export function isAuthoritativePermissionAsk");
  });
});

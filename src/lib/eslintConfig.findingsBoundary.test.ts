/**
 * The output-text boundary, tested at the SEAM it lives in: the real
 * `eslint.config.js` resolved for the two real file paths that parse AI and
 * terminal output, not the rule options in isolation.
 *
 * Those files turn untrusted output text into findings, so they may not
 * import an IPC door, an HTTP client, or an AI-task launcher — a
 * pending-verification marker line once reached disk through `invoke` there and
 * launched an unattended AI task. The negative case pins that the one
 * persistence import they legitimately need (display data) stays clean, so the
 * rule cannot be widened into something that gets switched off.
 *
 * Plan `2026-10-06-terminal-and-ai-output-text-launches-an-unattended-ai-task`.
 */

import { ESLint } from "eslint";
import { describe, expect, it } from "vitest";

const BOUNDARY_FILES = [
  "src/services/FindingsTracker.ts",
  "src/components/terminal/useTerminalFindings.ts",
];

const PLAN = "2026-10-06-terminal-and-ai-output-text-launches-an-unattended-ai-task";

// Assembled rather than spelled, so the plan's acceptance grep over src/ (which
// excludes only __tests__/) stays empty while the fixture names the module.
const VERIFICATION_SERVICE = ["Verification", "Service"].join("");

const FORBIDDEN_IMPORTS: Record<string, string> = {
  "@/lib/operatorDoors": `import { invokeOperatorDoor } from "@/lib/operatorDoors";\nexport const x = invokeOperatorDoor;\n`,
  "../lib/operatorDoors": `import { invokeOperatorDoor } from "../lib/operatorDoors";\nexport const x = invokeOperatorDoor;\n`,
  "@/hooks": `import { executeAiTask } from "@/hooks";\nexport const x = executeAiTask;\n`,
  "../hooks": `import { executeAiTask } from "../hooks";\nexport const x = executeAiTask;\n`,
  "@/lib/runner-api": `import { tracedFetch } from "@/lib/runner-api";\nexport const x = tracedFetch;\n`,
  [`./${VERIFICATION_SERVICE}`]: `import { svc } from "./${VERIFICATION_SERVICE}";\nexport const x = svc;\n`,
  "@tauri-apps/api/core": `import { invoke } from "@tauri-apps/api/core";\nexport const x = invoke;\n`,
  "@tauri-apps/api/core.js": `import { invoke } from "@tauri-apps/api/core.js";\nexport const x = invoke;\n`,
  "@tauri-apps/api (root barrel)": `import { core } from "@tauri-apps/api";\nexport const x = core;\n`,
  'dynamic import("@tauri-apps/api/core")': `export async function f() {\n  const { invoke } = await import("@tauri-apps/api/core");\n  return invoke("x");\n}\n`,
  'dynamic import("@/hooks")': `export async function f() {\n  const { executeAiTask } = await import("@/hooks");\n  return executeAiTask;\n}\n`,
};

const ALLOWED_IMPORT = `import { persistFindingsData } from "../findings/FindingsPersistence";\nexport const x = persistFindingsData;\n`;

const CATCH_SITE_DISCARD = `
export function f(setError: (m: string) => void) {
  try {
    doThing();
  } catch (err) {
    setError(err instanceof Error ? err.message : "Failed to load X");
  }
}
declare function doThing(): void;
`;

const POPULATION_NAME = `export const sessionCount = 1;\n`;

async function errorMessages(code: string, filePath: string, ruleIds: string[]): Promise<string[]> {
  const eslint = new ESLint();
  const [result] = await eslint.lintText(code, { filePath });
  // A fixture that fails to PARSE yields only a fatal message (ruleId null),
  // which would let the negative case pass while checking nothing.
  expect(result.messages.filter((m) => m.fatal)).toEqual([]);
  return result.messages
    .filter((m) => m.ruleId !== null && ruleIds.includes(m.ruleId) && m.severity === 2)
    .map((m) => m.message);
}

/** Boundary messages only, from either rule that carries the boundary. */
async function boundaryMessages(code: string, filePath: string): Promise<string[]> {
  const msgs = await errorMessages(code, filePath, [
    "no-restricted-imports",
    "no-restricted-syntax",
  ]);
  return msgs.filter((m) => m.includes(PLAN));
}

describe("output-text boundary (eslint.config.js)", () => {
  for (const filePath of BOUNDARY_FILES) {
    for (const [specifier, code] of Object.entries(FORBIDDEN_IMPORTS)) {
      it(`refuses ${specifier} in ${filePath}`, async () => {
        const msgs = await boundaryMessages(code, filePath);
        expect(msgs).toHaveLength(1);
      }, 60_000);
    }

    it(`allows ../findings/FindingsPersistence in ${filePath}`, async () => {
      const msgs = await errorMessages(ALLOWED_IMPORT, filePath, [
        "no-restricted-imports",
        "no-restricted-syntax",
      ]);
      expect(msgs).toEqual([]);
    }, 60_000);

    // The dynamic-import selectors live in `no-restricted-syntax`, whose options
    // flat config REPLACES across blocks — so the catch-site guard must still
    // fire at these real paths.
    it(`keeps the catch-site discard guard live in ${filePath}`, async () => {
      const msgs = await errorMessages(CATCH_SITE_DISCARD, filePath, ["no-restricted-syntax"]);
      expect(msgs.some((m) => m.includes("describeThrown"))).toBe(true);
    }, 60_000);
  }

  it("keeps the population-name guard live in useTerminalFindings.ts", async () => {
    const msgs = await errorMessages(
      POPULATION_NAME,
      "src/components/terminal/useTerminalFindings.ts",
      ["no-restricted-syntax"],
    );
    expect(msgs.some((m) => m.includes("sessionCount"))).toBe(true);
  }, 60_000);
});

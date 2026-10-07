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

const FORBIDDEN_IMPORTS: Record<string, string> = {
  "@/lib/operatorDoors": `import { invokeOperatorDoor } from "@/lib/operatorDoors";\nexport const x = invokeOperatorDoor;\n`,
  "../lib/operatorDoors": `import { invokeOperatorDoor } from "../lib/operatorDoors";\nexport const x = invokeOperatorDoor;\n`,
  "@/hooks": `import { executeAiTask } from "@/hooks";\nexport const x = executeAiTask;\n`,
  "@tauri-apps/api/core": `import { invoke } from "@tauri-apps/api/core";\nexport const x = invoke;\n`,
};

const ALLOWED_IMPORT = `import { persistFindingsData } from "../findings/FindingsPersistence";\nexport const x = persistFindingsData;\n`;

async function restrictedImportMessages(code: string, filePath: string): Promise<string[]> {
  const eslint = new ESLint();
  const [result] = await eslint.lintText(code, { filePath });
  // A fixture that fails to PARSE yields only a fatal message (ruleId null),
  // which would let the negative case pass while checking nothing.
  expect(result.messages.filter((m) => m.fatal)).toEqual([]);
  return result.messages
    .filter((m) => m.ruleId === "no-restricted-imports" && m.severity === 2)
    .map((m) => m.message);
}

describe("output-text boundary (eslint.config.js)", () => {
  for (const filePath of BOUNDARY_FILES) {
    for (const [specifier, code] of Object.entries(FORBIDDEN_IMPORTS)) {
      it(`refuses ${specifier} in ${filePath}`, async () => {
        const msgs = await restrictedImportMessages(code, filePath);
        expect(msgs).toHaveLength(1);
        expect(msgs[0]).toContain(
          "2026-10-06-terminal-and-ai-output-text-launches-an-unattended-ai-task",
        );
      }, 60_000);
    }

    it(`allows ../findings/FindingsPersistence in ${filePath}`, async () => {
      const msgs = await restrictedImportMessages(ALLOWED_IMPORT, filePath);
      expect(msgs).toEqual([]);
    }, 60_000);
  }
});

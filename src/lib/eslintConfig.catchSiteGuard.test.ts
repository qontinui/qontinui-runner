/**
 * The catch-site discard guard, tested at the SEAM it lives in: the real
 * `eslint.config.js` resolved for a real path, not the selector string in
 * isolation.
 *
 * Two failure modes a selector-only test cannot see:
 *  - flat config REPLACES `no-restricted-syntax` options across blocks, so a
 *    later block (the terminal population-name guard) silently switches the
 *    catch-site guard off under its own `files` glob unless it spreads both
 *    selector lists;
 *  - a selector that is too wide fires on the value-preserving shapes
 *    (`err instanceof Error ? err : new Error(String(err))`), and a rule that
 *    fires on correct code gets disabled.
 *
 * Plan `2026-09-09-catch-site-discard-is-repo-wide-and-the-deferral-was-never-measured`.
 */

import { ESLint } from "eslint";
import { describe, expect, it } from "vitest";

const DISCARD = `
export function f(setError: (m: string) => void) {
  try {
    doThing();
  } catch (err) {
    setError(err instanceof Error ? err.message : "Failed to load X");
  }
}
declare function doThing(): void;
`;

const VALUE_PRESERVING = `
export function g(err: unknown, q: { error: unknown }) {
  const e = err instanceof Error ? err : new Error(String(err));
  const maybe = q.error instanceof Error ? q.error : null;
  return [e, maybe];
}
`;

const POPULATION_NAME = `
export const sessionCount = 1;
`;

async function restrictedSyntaxMessages(code: string, filePath: string): Promise<string[]> {
  const eslint = new ESLint();
  const [result] = await eslint.lintText(code, { filePath });
  // A fixture that fails to PARSE yields only a fatal message (ruleId null),
  // which would let the negative case pass while checking nothing.
  expect(result.messages.filter((m) => m.fatal)).toEqual([]);
  return result.messages.filter((m) => m.ruleId === "no-restricted-syntax").map((m) => m.message);
}

describe("catch-site discard guard (eslint.config.js)", () => {
  it("fires on the message-discarding ternary in an ordinary src file", async () => {
    const msgs = await restrictedSyntaxMessages(DISCARD, "src/hooks/__fixture__.ts");
    expect(msgs).toHaveLength(1);
    expect(msgs[0]).toContain("describeThrown");
  }, 60_000);

  it("stays live under src/components/terminal/** despite the population-name block", async () => {
    const msgs = await restrictedSyntaxMessages(DISCARD, "src/components/terminal/__fixture__.tsx");
    expect(msgs).toHaveLength(1);
    expect(msgs[0]).toContain("describeThrown");
  }, 60_000);

  it("does not switch the population-name guard off under terminal/**", async () => {
    const msgs = await restrictedSyntaxMessages(
      POPULATION_NAME,
      "src/components/terminal/__fixture__.tsx",
    );
    expect(msgs.some((m) => m.includes("sessionCount"))).toBe(true);
  }, 60_000);

  it("does not fire on the value-preserving shapes", async () => {
    const msgs = await restrictedSyntaxMessages(VALUE_PRESERVING, "src/hooks/__fixture__.ts");
    expect(msgs).toEqual([]);
  }, 60_000);
});

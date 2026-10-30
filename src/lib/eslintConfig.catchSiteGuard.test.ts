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
  const neg = !(err instanceof Error) ? new Error(String(err)) : err;
  const stack = err instanceof TypeError ? err.stack : undefined;
  return [e, maybe, neg, stack];
}
`;

/**
 * One fixture per escape route the 2026-10-08 widening closed. Each must report
 * EXACTLY once — the arms REPLACE the original plain-`Error` selector rather
 * than accompany it, so a double report would mean two arms overlap.
 */
const ESCAPE_ROUTES: Record<string, string> = {
  subclass: `export const a = (e: unknown) => (e instanceof TypeError ? e.message : "x");`,
  optionalChain: `export const a = (e: any) => (e instanceof Error ? e?.message : "x");`,
  negated: `export const a = (e: unknown) => (!(e instanceof Error) ? "x" : e.message);`,
  negatedOptionalChain: `export const a = (e: any) => (!(e instanceof Error) ? "x" : e?.message);`,
};

/** A subclass-specific text whose other branch keeps the cause: allowed. */
const SUBCLASS_KEEPS_CAUSE = `
declare class ApiError extends Error {}
declare function describeThrown(e: unknown, f: string): string;
export const a = (e: unknown) => (e instanceof ApiError ? e.message : describeThrown(e, "x"));
export const b = (e: unknown) => (!(e instanceof ApiError) ? describeThrown(e, "x") : e.message);
export const c = (e: any) => (e instanceof ApiError ? e?.message : describeThrown(e, "x"));
export const d = (e: any) => (!(e instanceof ApiError) ? describeThrown(e, "x") : e?.message);
`;

/** A cast OUTSIDE a catch is out of the cast arm's scope (the value's type is known there). */
const CAST_OUTSIDE_CATCH = `
export const m = (err: unknown) => (err as Error).message;
`;

/** The CAST spelling: production files only. */
const CAST_IN_CATCH = `
export function f(setError: (m: string) => void) {
  try {
    doThing();
  } catch (err) {
    setError((err as Error).message);
  }
}
declare function doThing(): void;
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

  for (const [name, source] of Object.entries(ESCAPE_ROUTES)) {
    it(`fires exactly once on the ${name} escape route`, async () => {
      const msgs = await restrictedSyntaxMessages(source, "src/hooks/__fixture__.ts");
      expect(msgs).toHaveLength(1);
      expect(msgs[0]).toContain("describeThrown");
    }, 60_000);
  }

  it("fires on a cast-in-catch in a production file, including under terminal/**", async () => {
    for (const path of ["src/hooks/__fixture__.ts", "src/components/terminal/__fixture__.tsx"]) {
      const msgs = await restrictedSyntaxMessages(CAST_IN_CATCH, path);
      expect(msgs).toHaveLength(1);
      expect(msgs[0]).toContain("is a cast, not a check");
    }
  }, 60_000);

  it("does NOT fire on a cast-in-catch in a test file, but keeps the ternary arms there", async () => {
    for (const path of [
      "src/hooks/__fixture__.test.ts",
      "src/components/terminal/__fixture__.test.tsx",
      "src/hooks/__tests__/fixture.ts",
      "src/lib/__test-helpers__/fixture.ts",
    ]) {
      expect(await restrictedSyntaxMessages(CAST_IN_CATCH, path)).toEqual([]);
      expect(await restrictedSyntaxMessages(DISCARD, path)).toHaveLength(1);
    }
    // The terminal test-file block must still carry the population-name list.
    const pop = await restrictedSyntaxMessages(
      POPULATION_NAME,
      "src/components/terminal/__fixture__.test.tsx",
    );
    expect(pop.some((m) => m.includes("sessionCount"))).toBe(true);
  }, 60_000);

  it("allows a subclass ternary whose other branch already calls describeThrown", async () => {
    expect(
      await restrictedSyntaxMessages(SUBCLASS_KEEPS_CAUSE, "src/hooks/__fixture__.ts"),
    ).toEqual([]);
  }, 60_000);

  it("scopes the cast arm to a catch clause", async () => {
    expect(await restrictedSyntaxMessages(CAST_OUTSIDE_CATCH, "src/hooks/__fixture__.ts")).toEqual(
      [],
    );
  }, 60_000);
});

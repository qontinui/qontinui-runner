/**
 * Tier-2 pattern resolver tests.
 *
 * Covers:
 *   1. No-pattern action never matches
 *   2. Named capture groups land in `args` with numeric coercion
 *   3. Optional groups are omitted when absent
 *   4. Leading-slash input is normalised before matching
 *   5. Registration-order is the iteration order — first hit wins
 *   6. Multi-pattern action: any pattern in the array can hit
 *   7. Empty / whitespace input returns null without scanning patterns
 *   8. Trailing free-form catch-all in spawn-ai survives quoting
 */

import { afterEach, describe, expect, it } from "vitest";

import { matchPattern } from "./patterns";
import { __resetForTest, register } from "./registry";
import type { CommandAction } from "./types";

afterEach(() => {
  __resetForTest();
});

const action = (overrides: Partial<CommandAction>): CommandAction => ({
  id: "test.action",
  slash: "/test",
  label: "Test action",
  description: "for the pattern spec",
  handler: async () => ({ ok: true }),
  ...overrides,
});

describe("matchPattern — basic resolution", () => {
  it("returns null when no action has any patterns", () => {
    register(action({ id: "a", slash: "/a", label: "A" }));
    expect(matchPattern("anything")).toBeNull();
  });

  it("returns null when patterns exist but nothing matches", () => {
    register(
      action({
        id: "swap",
        slash: "/swap",
        patterns: [/^swap\s+(?<a>\d+)\s+(?<b>\d+)$/i],
      }),
    );
    expect(matchPattern("xyz")).toBeNull();
    expect(matchPattern("swap 1")).toBeNull();
  });

  it("returns null for empty / whitespace input", () => {
    register(
      action({ id: "swap", slash: "/swap", patterns: [/^swap\s+(?<a>\d+)$/i] }),
    );
    expect(matchPattern("")).toBeNull();
    expect(matchPattern("   ")).toBeNull();
  });
});

describe("matchPattern — named groups → args", () => {
  it("extracts named groups and numeric-coerces", () => {
    register(
      action({
        id: "swap",
        slash: "/swap",
        patterns: [/^swap\s+(?<a>\d+)\s+(?<b>\d+)$/i],
      }),
    );
    const m = matchPattern("swap 1 4");
    expect(m).not.toBeNull();
    expect(m?.action.id).toBe("swap");
    expect(m?.args).toEqual({ a: 1, b: 4 });
  });

  it("leaves non-numeric tokens as strings", () => {
    register(
      action({
        id: "layout",
        slash: "/layout",
        patterns: [/^layout\s+(?<preset>single|split|six-pack)$/i],
      }),
    );
    expect(matchPattern("layout six-pack")?.args).toEqual({ preset: "six-pack" });
  });

  it("omits optional groups when absent (does not set args field to undefined)", () => {
    register(
      action({
        id: "maximize",
        slash: "/maximize",
        patterns: [/^maximize(?:\s+(?<zone>\d+))?$/i],
      }),
    );
    const bare = matchPattern("maximize");
    expect(bare?.args).toEqual({});
    expect("zone" in (bare?.args ?? {})).toBe(false);

    const withZone = matchPattern("maximize 3");
    expect(withZone?.args).toEqual({ zone: 3 });
  });
});

describe("matchPattern — input normalisation", () => {
  it("strips a single leading `/` before matching", () => {
    register(
      action({
        id: "swap",
        slash: "/swap",
        patterns: [/^swap\s+(?<a>\d+)\s+(?<b>\d+)$/i],
      }),
    );
    expect(matchPattern("/swap 1 2")?.args).toEqual({ a: 1, b: 2 });
  });

  it("is case-insensitive when patterns carry the `i` flag", () => {
    register(
      action({
        id: "swap",
        slash: "/swap",
        patterns: [/^swap\s+(?<a>\d+)\s+(?<b>\d+)$/i],
      }),
    );
    expect(matchPattern("SWAP 1 2")?.action.id).toBe("swap");
  });
});

describe("matchPattern — registration order", () => {
  it("returns the first registered action whose pattern hits", () => {
    register(
      action({
        id: "spawn",
        slash: "/spawn",
        patterns: [/^spawn\s+(?<count>\d+)(?:\s+plain)?$/i],
      }),
    );
    register(
      action({
        id: "spawn-ai",
        slash: "/spawn-ai",
        patterns: [/^spawn\s+(?<count>\d+)\s+(?<account>best|claude|[\w-]+)$/i],
      }),
    );
    // `spawn 3` matches spawn only (spawn-ai requires an account).
    expect(matchPattern("spawn 3")?.action.id).toBe("spawn");
    // `spawn 3 plain` matches spawn only.
    expect(matchPattern("spawn 3 plain")?.action.id).toBe("spawn");
    // `spawn 3 best` doesn't match spawn (`plain` is the only optional
    // trailing token allowed), so the spawn-ai pattern wins.
    expect(matchPattern("spawn 3 best")?.action.id).toBe("spawn-ai");
  });
});

describe("matchPattern — multi-pattern arrays", () => {
  it("matches if any pattern in the array hits", () => {
    register(
      action({
        id: "focus",
        slash: "/focus",
        patterns: [
          /^focus\s+(?<target>next|prev|previous|\d+)$/i,
          /^(?<target>next|previous|prev)\s+session$/i,
        ],
      }),
    );
    expect(matchPattern("focus 2")?.args).toEqual({ target: 2 });
    expect(matchPattern("focus prev")?.args).toEqual({ target: "prev" });
    expect(matchPattern("previous session")?.args).toEqual({ target: "previous" });
  });
});

describe("matchPattern — free-form trailing group", () => {
  it("captures multi-word context as a single string", () => {
    register(
      action({
        id: "spawn-ai",
        slash: "/spawn-ai",
        patterns: [
          /^spawn-ai\s+(?<count>\d+)\s+(?<account>[\w-]+|best)(?:\s+(?<context>.+))?$/i,
        ],
      }),
    );
    expect(matchPattern("spawn-ai 3 best fix the failing test")?.args).toEqual({
      count: 3,
      account: "best",
      context: "fix the failing test",
    });
  });
});

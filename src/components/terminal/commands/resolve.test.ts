/**
 * Tier-1 resolver + arg parser tests.
 *
 * Runs under vitest's `environment: "node"` (no jsdom). Each test
 * registers a small synthetic action set against the live registry
 * (resetting between cases via `__resetForTest`) and exercises the
 * resolver / parser as the CommandBar will see them.
 */

import { afterEach, describe, expect, it } from "vitest";

import { parseArgs, tokenize, coerceToken } from "./parse";
import { __resetForTest, register } from "./registry";
import { resolve } from "./resolve";
import type { CommandAction } from "./types";

afterEach(() => {
  __resetForTest();
});

const action = (overrides: Partial<CommandAction> = {}): CommandAction => ({
  id: overrides.id ?? "test.action",
  slash: overrides.slash ?? "/test",
  label: overrides.label ?? "Test action",
  description: overrides.description ?? "for the resolver spec",
  handler: overrides.handler ?? (async () => ({ ok: true, value: "ran" })),
  ...overrides,
});

describe("resolve — empty input", () => {
  it("returns all actions (capped at MAX_SUGGESTIONS) in registration order when no recents", () => {
    register(action({ id: "a", slash: "/a", label: "A" }));
    register(action({ id: "b", slash: "/b", label: "B" }));
    const matches = resolve("", []);
    expect(matches.map((m) => m.action.id)).toEqual(["a", "b"]);
    expect(matches.every((m) => !m.exact)).toBe(true);
    expect(matches.every((m) => !m.recent)).toBe(true);
  });

  it("places recents first, in last-used order", () => {
    register(action({ id: "a", slash: "/a", label: "A" }));
    register(action({ id: "b", slash: "/b", label: "B" }));
    register(action({ id: "c", slash: "/c", label: "C" }));
    const matches = resolve("", ["c", "a"]);
    expect(matches.map((m) => m.action.id)).toEqual(["c", "a", "b"]);
    expect(matches[0].recent).toBe(true);
    expect(matches[1].recent).toBe(true);
    expect(matches[2].recent).toBe(false);
  });

  it("ignores recents that refer to unknown ids", () => {
    register(action({ id: "a", slash: "/a", label: "A" }));
    const matches = resolve("", ["ghost", "a"]);
    expect(matches.map((m) => m.action.id)).toEqual(["a"]);
  });
});

describe("resolve — exact slash hit", () => {
  it("returns only the matching action when the first token matches a slash", () => {
    register(action({ id: "swap", slash: "/swap", label: "Swap zones" }));
    register(action({ id: "spawn", slash: "/spawn", label: "Spawn" }));
    const matches = resolve("/swap 1 2", []);
    expect(matches).toHaveLength(1);
    expect(matches[0].action.id).toBe("swap");
    expect(matches[0].exact).toBe(true);
  });

  it("works without a leading slash on the input", () => {
    register(action({ id: "spawn", slash: "/spawn", label: "Spawn" }));
    const matches = resolve("spawn 3", []);
    expect(matches).toHaveLength(1);
    expect(matches[0].action.id).toBe("spawn");
    expect(matches[0].exact).toBe(true);
  });

  it("matches via alias", () => {
    register(
      action({
        id: "spawn-ai",
        slash: "/spawn-ai",
        aliases: ["/spawn-best"],
      }),
    );
    const matches = resolve("/spawn-best 3", []);
    expect(matches).toHaveLength(1);
    expect(matches[0].action.id).toBe("spawn-ai");
    expect(matches[0].exact).toBe(true);
  });

  it("flags recent for exact hits too", () => {
    register(action({ id: "swap", slash: "/swap" }));
    const matches = resolve("/swap", ["swap"]);
    expect(matches[0].recent).toBe(true);
  });
});

describe("resolve — fuzzy match", () => {
  it("returns nothing for no-match queries", () => {
    register(action({ id: "spawn", slash: "/spawn", label: "Spawn" }));
    expect(resolve("/xyz", [])).toEqual([]);
  });

  it("prefers prefix matches over sequential fuzzy", () => {
    register(action({ id: "spawn", slash: "/spawn", label: "Spawn" }));
    register(action({ id: "swap", slash: "/swap", label: "Swap zones" }));
    const matches = resolve("/sp", []);
    expect(matches[0].action.id).toBe("spawn");
  });

  it("pulls recents above same-score peers", () => {
    register(action({ id: "spawn", slash: "/spawn", label: "Spawn plain" }));
    register(action({ id: "spawn-ai", slash: "/spawn-ai", label: "Spawn AI" }));
    const matches = resolve("/sp", ["spawn-ai"]);
    // Both are prefix matches with the same score — recent wins.
    expect(matches[0].action.id).toBe("spawn-ai");
  });

  it("caps to 8 suggestions", () => {
    for (let i = 0; i < 12; i++) {
      register(action({ id: `n${i}`, slash: `/n${i}`, label: `N${i}` }));
    }
    expect(resolve("/n", [])).toHaveLength(8);
  });
});

describe("parse — tokenize", () => {
  it("splits on whitespace", () => {
    expect(tokenize("a b c")).toEqual(["a", "b", "c"]);
  });

  it("treats quoted runs as one token", () => {
    expect(tokenize('3 best "fix the failing test"')).toEqual([
      "3",
      "best",
      "fix the failing test",
    ]);
  });

  it("collapses multiple spaces", () => {
    expect(tokenize("a    b\tc")).toEqual(["a", "b", "c"]);
  });

  it("returns empty for empty input", () => {
    expect(tokenize("")).toEqual([]);
    expect(tokenize("   ")).toEqual([]);
  });
});

describe("parse — coerceToken", () => {
  it("coerces clean integers to number", () => {
    expect(coerceToken("3")).toBe(3);
    expect(coerceToken("-7")).toBe(-7);
  });

  it("coerces decimals to number", () => {
    expect(coerceToken("1.5")).toBe(1.5);
  });

  it("leaves non-numeric tokens as string", () => {
    expect(coerceToken("best")).toBe("best");
    expect(coerceToken("3a")).toBe("3a");
    expect(coerceToken("")).toBe("");
  });
});

describe("parse — parseArgs", () => {
  const spawnAi = action({
    id: "spawn-ai",
    slash: "/spawn-ai",
    paramSchema: { count: "n", account: "s", context: "s" },
  });

  it("maps positional tokens to paramSchema field order", () => {
    expect(parseArgs("/spawn-ai 3 best", spawnAi)).toEqual({
      count: 3,
      account: "best",
    });
  });

  it("joins trailing tokens into the last field (free-form catch-all)", () => {
    expect(parseArgs("/spawn-ai 3 best fix the failing test", spawnAi)).toEqual({
      count: 3,
      account: "best",
      context: "fix the failing test",
    });
  });

  it("honors quoted runs as a single token", () => {
    expect(parseArgs('/spawn-ai 3 best "fix the failing test"', spawnAi)).toEqual({
      count: 3,
      account: "best",
      context: "fix the failing test",
    });
  });

  it("returns empty when no args follow the slash", () => {
    expect(parseArgs("/spawn-ai", spawnAi)).toEqual({});
    expect(parseArgs("/spawn-ai ", spawnAi)).toEqual({});
  });

  it("returns empty for actions with no paramSchema", () => {
    const noSchema = action({ id: "approve-all", slash: "/approve-all" });
    expect(parseArgs("/approve-all ignored", noSchema)).toEqual({});
  });
});

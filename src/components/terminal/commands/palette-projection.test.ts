/**
 * Tests for the Phase-7 registry → palette projection.
 *
 * Covers:
 *   1. Empty registry → empty projection
 *   2. Argless action — projects with label + slash + no params hint
 *   3. Action with paramSchema — label gains "(key1, key2)" hint
 *   4. All rows live under category "Commands" with priority 0
 *   5. Click handler invokes registry handler and swallows failures
 *   6. Click handler returns success to the operator's `void` shape
 *      (no throw on rejection)
 *   7. Projection is emitted in REGISTRY order, so an exact score tie
 *      breaks the same way it does in `resolve()`
 *   8. `scorePaletteLabel` scores a composed row label the way
 *      `resolve()` scores a registry action — slash form and
 *      description separately, with the within-tier slash tiebreak
 */

import { afterEach, describe, expect, it, vi } from "vitest";

import { resolve } from "./resolve";
import { getRegistryPaletteActions, scorePaletteLabel } from "./palette-projection";
import { __resetForTest, register } from "./registry";
import type { CommandAction } from "./types";

afterEach(() => {
  __resetForTest();
});

const action = (overrides: Partial<CommandAction>): CommandAction => ({
  id: "test.action",
  slash: "/test",
  label: "Test action",
  description: "spec",
  handler: async () => ({ ok: true }),
  ...overrides,
});

describe("getRegistryPaletteActions", () => {
  it("returns empty when the registry is empty", () => {
    expect(getRegistryPaletteActions()).toEqual([]);
  });

  it("projects an argless action with bare slash label", () => {
    register(action({ id: "approve-all", slash: "/approve-all", label: "Approve all" }));
    const rows = getRegistryPaletteActions();
    expect(rows).toHaveLength(1);
    expect(rows[0]).toMatchObject({
      id: "registry:approve-all",
      category: "Commands",
      priority: 0,
      label: "/approve-all — Approve all",
    });
  });

  it("appends a `(key1, key2)` params hint when paramSchema is non-empty", () => {
    register(
      action({
        id: "swap",
        slash: "/swap",
        label: "Swap two zones",
        paramSchema: { a: "n", b: "n" },
      }),
    );
    expect(getRegistryPaletteActions()[0].label).toBe("/swap — Swap two zones (a, b)");
  });

  it("treats an empty paramSchema object as argless (no hint)", () => {
    register(
      action({
        id: "x",
        slash: "/x",
        label: "X",
        paramSchema: {},
      }),
    );
    expect(getRegistryPaletteActions()[0].label).toBe("/x — X");
  });

  it("emits rows in registry order, not alphabetically", () => {
    // Lexical order was the palette half of a live divergence: `rst`
    // ties `/restart` and `/auto-restart` at the identical score, so the
    // winner is whatever the stable sort saw first. `resolve()` iterates
    // the registry, so the palette must too — otherwise the two surfaces
    // teach different slashes for the same query.
    register(action({ id: "z", slash: "/z", label: "Z" }));
    register(action({ id: "a", slash: "/a", label: "A" }));
    register(action({ id: "m", slash: "/m", label: "M" }));
    const labels = getRegistryPaletteActions().map((r) => r.label);
    expect(labels).toEqual(["/z — Z", "/a — A", "/m — M"]);
  });

  it("click invokes the registry handler with empty args", async () => {
    const handler = vi.fn(async () => ({ ok: true as const, value: 42 }));
    register(action({ id: "test", slash: "/test", label: "Test", handler }));
    const row = getRegistryPaletteActions()[0];
    row.action();
    // callRegistry is async-but-fire-and-forget; await a microtask so
    // the handler call lands before the assertion.
    await Promise.resolve();
    expect(handler).toHaveBeenCalledWith({}, { source: "uibridge" });
  });

  it("click swallows failures (does not throw to the caller)", async () => {
    const warnSpy = vi.spyOn(console, "warn").mockImplementation(() => {});
    register(
      action({
        id: "fail",
        slash: "/fail",
        label: "Fail",
        handler: async () => ({ ok: false, code: "invalid-args", message: "a and b required" }),
      }),
    );
    const row = getRegistryPaletteActions()[0];
    expect(() => row.action()).not.toThrow();
    // Drain the async chain to let the catch handler fire.
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(warnSpy).toHaveBeenCalled();
    warnSpy.mockRestore();
  });
});

describe("scorePaletteLabel", () => {
  it("scores the slash form on its own, keeping its prefix tier", () => {
    // Composed as one string, `"/restart — …"` starts with `/`, so the
    // slash lost its Tier-1 prefix band entirely.
    const slash = scorePaletteLabel("/restart — Restart session in zone", "rest");
    expect(slash?.score).toBe(204);
    expect(slash?.fromSlash).toBe(true);
    // Indices are shifted back into the composed label (past the `/`).
    expect(slash?.indices).toEqual([1, 2, 3, 4]);
  });

  it("flags a match that reached into the description prose", () => {
    const desc = scorePaletteLabel("/zzz — Restart something", "rst");
    expect(desc?.fromSlash).toBe(false);
    // "Restart" starts at index 7 of "/zzz — Restart something".
    expect(desc?.indices[0]).toBe(7);
  });

  it("falls through to a plain label score for a row with no slash form", () => {
    const plain = scorePaletteLabel("Focus zone 3: claude-gmail", "zone");
    expect(plain?.fromSlash).toBe(false);
    expect(plain?.score).toBeGreaterThan(0);
  });

  it("returns null when nothing matches", () => {
    expect(scorePaletteLabel("/restart — Restart session in zone", "qqq")).toBeNull();
  });
});

describe("palette ranking agrees with the CommandBar", () => {
  /** The palette's sort keys, extracted so the parity test can run them. */
  function paletteTop(query: string): string {
    const rows = getRegistryPaletteActions();
    const scored = rows
      .map((row) => ({ row, m: scorePaletteLabel(row.label, query) }))
      .filter((s): s is { row: (typeof rows)[number]; m: NonNullable<typeof s.m> } => s.m !== null);
    scored.sort((a, b) => {
      if (a.m.score !== b.m.score) return b.m.score - a.m.score;
      if (a.m.fromSlash !== b.m.fromSlash) return a.m.fromSlash ? -1 : 1;
      return 0;
    });
    return scored[0].row.id.replace(/^registry:/, "");
  }

  it("puts the same action first for the three shipped tie queries", () => {
    // The live divergence, reproduced with the real competitors: a
    // decoy whose LABEL ties (the within-tier slash tiebreak) plus a
    // slash that ties and sorts EARLIER alphabetically (the registry
    // order half). The palette used to answer /auto-restart, /doc-finder
    // and the decoy respectively.
    register(action({ id: "decoy-rst", slash: "/zz1", label: "Restart something" }));
    register(action({ id: "restart", slash: "/restart", label: "Restart session in zone" }));
    register(action({ id: "auto-restart", slash: "/auto-restart", label: "Toggle auto-restart" }));
    register(action({ id: "decoy-fnd", slash: "/zz2", label: "Find node data" }));
    register(action({ id: "findings", slash: "/findings", label: "Toggle findings panel" }));
    register(action({ id: "doc-finder", slash: "/doc-finder", label: "Open doc finder" }));
    register(action({ id: "decoy-ntf", slash: "/zz3", label: "Notify test flags" }));
    register(
      action({ id: "notify", slash: "/desktop-notify", label: "Toggle desktop notifications" }),
    );

    for (const [query, expected] of [
      ["rst", "restart"],
      ["fnd", "findings"],
      ["ntf", "notify"],
    ] as const) {
      expect(paletteTop(query), `palette top for ${query}`).toBe(expected);
      expect(resolve(query, [])[0].action.id, `bar top for ${query}`).toBe(expected);
    }
  });
});

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
 *   7. Projection is stable across calls — sorted by label
 */

import { afterEach, describe, expect, it, vi } from "vitest";

import { getRegistryPaletteActions } from "./palette-projection";
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

  it("sorts rows lexically by their composed label", () => {
    register(action({ id: "z", slash: "/z", label: "Z" }));
    register(action({ id: "a", slash: "/a", label: "A" }));
    register(action({ id: "m", slash: "/m", label: "M" }));
    const labels = getRegistryPaletteActions().map((r) => r.label);
    expect(labels).toEqual([
      "/a — A",
      "/m — M",
      "/z — Z",
    ]);
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

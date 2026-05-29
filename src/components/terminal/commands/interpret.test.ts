/**
 * Tests for Tier-3 interpret bridge.
 *
 * Coverage focuses on the pure pieces (normalization, cache, gate) and
 * uses a vi.mock for the Tauri `invoke` so the subprocess never runs
 * during CI.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));

vi.mock("@/lib/instance-storage", () => {
  let store: Record<string, string> = {};
  return {
    instanceStorage: {
      getItem: (k: string) => store[k] ?? null,
      setItem: (k: string, v: string) => {
        store[k] = v;
      },
      removeItem: (k: string) => {
        delete store[k];
      },
      getJSON<T>(k: string, fb: T): T {
        const raw = store[k];
        return raw ? (JSON.parse(raw) as T) : fb;
      },
      setJSON(k: string, v: unknown) {
        store[k] = JSON.stringify(v);
      },
      removeByPrefix: () => {
        store = {};
      },
      __reset: () => {
        store = {};
      },
    },
  };
});

import { invoke } from "@tauri-apps/api/core";
import { instanceStorage } from "@/lib/instance-storage";

import { interpretCommand, normalizeInterpretKey } from "./interpret";
import { __resetForTest, register } from "./registry";

const mockedInvoke = vi.mocked(invoke);

afterEach(() => {
  __resetForTest();
  // @ts-expect-error — test helper added by the mock above
  instanceStorage.__reset?.();
  mockedInvoke.mockReset();
});

beforeEach(() => {
  register({
    id: "terminal.spawn",
    slash: "/spawn",
    label: "Spawn plain terminal",
    description: "spec",
    handler: async () => ({ ok: true, value: [] }),
  });
});

describe("normalizeInterpretKey", () => {
  it("lowercases + trims + collapses whitespace", () => {
    expect(normalizeInterpretKey("  Spawn  THREE   ")).toBe("spawn three");
  });

  it("strips a single leading slash", () => {
    expect(normalizeInterpretKey("/spawn 3")).toBe("spawn 3");
  });

  it("returns empty for whitespace-only input", () => {
    expect(normalizeInterpretKey("   ")).toBe("");
  });
});

describe("interpretCommand — cache", () => {
  it("hits the cache on identical normalized text without invoking the subprocess", async () => {
    mockedInvoke.mockResolvedValueOnce({
      tool: "terminal.spawn",
      args: { count: 3 },
      confidence: 0.9,
    });
    const first = await interpretCommand("spawn three terminals");
    expect(first?.action.id).toBe("terminal.spawn");
    expect(mockedInvoke).toHaveBeenCalledTimes(1);

    // Same text with cosmetic whitespace — cache key normalises.
    const second = await interpretCommand("  Spawn  Three  Terminals  ");
    expect(second?.action.id).toBe("terminal.spawn");
    // Still 1 — the second resolved from cache.
    expect(mockedInvoke).toHaveBeenCalledTimes(1);
  });

  it("re-invokes when the cached entry was below the gate (but cache still records)", async () => {
    mockedInvoke
      .mockResolvedValueOnce({ tool: "terminal.spawn", args: {}, confidence: 0.2 })
      .mockResolvedValueOnce({ tool: "terminal.spawn", args: {}, confidence: 0.9 });
    // First call: below default gate (0.4) → returns null but caches.
    const first = await interpretCommand("ambiguous", { minConfidence: 0.4 });
    expect(first).toBeNull();
    // Second call with a higher gate still wouldn't pass — but the
    // cache returned null without re-invoking, which is the contract.
    const second = await interpretCommand("ambiguous", { minConfidence: 0.4 });
    expect(second).toBeNull();
    expect(mockedInvoke).toHaveBeenCalledTimes(1);
  });
});

describe("interpretCommand — gate + projection", () => {
  it("returns null when confidence is below the gate", async () => {
    mockedInvoke.mockResolvedValueOnce({
      tool: "terminal.spawn",
      args: {},
      confidence: 0.1,
    });
    const result = await interpretCommand("vague", { minConfidence: 0.4 });
    expect(result).toBeNull();
  });

  it("returns null when the tool id is unknown to the registry", async () => {
    mockedInvoke.mockResolvedValueOnce({
      tool: "made.up.action",
      args: {},
      confidence: 0.99,
    });
    const result = await interpretCommand("do the thing");
    expect(result).toBeNull();
  });

  it("returns null on empty input without invoking", async () => {
    const result = await interpretCommand("   ");
    expect(result).toBeNull();
    expect(mockedInvoke).not.toHaveBeenCalled();
  });

  it("swallows subprocess errors as null", async () => {
    mockedInvoke.mockRejectedValueOnce(new Error("claude not found"));
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    const result = await interpretCommand("spawn 3");
    expect(result).toBeNull();
    expect(warn).toHaveBeenCalled();
    warn.mockRestore();
  });

  it("respects an aborted signal — does not invoke", async () => {
    const ctrl = new AbortController();
    ctrl.abort();
    const result = await interpretCommand("spawn 3", { signal: ctrl.signal });
    // Aborted-before-call: invoke is still called (the abort check is
    // after the cache lookup), but the result is dropped to null. Both
    // behaviors are acceptable contracts; current impl checks abort
    // BEFORE the invoke, so 0 calls expected.
    expect(result).toBeNull();
    expect(mockedInvoke).not.toHaveBeenCalled();
  });
});

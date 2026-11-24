/**
 * Zone minimap position — persistence, reset, and clamping.
 *
 * Same harness as `useUIState.minimap.test.ts`: node environment, a
 * module-level mock of the storage surface, and the exported pure pieces.
 */

import { describe, it, expect, beforeEach, vi } from "vitest";

const store = new Map<string, string>();

vi.mock("@/lib/instance-storage", () => ({
  instanceStorage: {
    getItem: (k: string) => store.get(k) ?? null,
    setItem: (k: string, v: string) => void store.set(k, v),
    removeItem: (k: string) => void store.delete(k),
    getJSON: <T>(_k: string, fallback: T) => fallback,
    setJSON: () => {},
  },
}));

const { uiReducer, createInitialState } = await import("./useUIState");
const { MINIMAP_POSITION_KEY, parseMinimapOffset, clampMinimapOffset } =
  await import("./minimapPosition");

beforeEach(() => store.clear());

describe("minimapOffset · persistence", () => {
  it("defaults to null (the original location) when nothing is stored", () => {
    expect(createInitialState().minimapOffset).toBeNull();
  });

  it("SET_MINIMAP_OFFSET persists and survives a remount (restart)", () => {
    const moved = uiReducer(createInitialState(), {
      type: "SET_MINIMAP_OFFSET",
      payload: { top: 120, right: 300 },
    });
    expect(moved.minimapOffset).toEqual({ top: 120, right: 300 });
    expect(createInitialState().minimapOffset).toEqual({ top: 120, right: 300 });
  });

  it("RESET_MINIMAP_OFFSET removes the stored key and returns to the default", () => {
    const moved = uiReducer(createInitialState(), {
      type: "SET_MINIMAP_OFFSET",
      payload: { top: 50, right: 50 },
    });
    const reset = uiReducer(moved, { type: "RESET_MINIMAP_OFFSET" });
    expect(reset.minimapOffset).toBeNull();
    expect(store.has(MINIMAP_POSITION_KEY)).toBe(false);
    expect(createInitialState().minimapOffset).toBeNull();
  });

  it("does not touch the visibility flag", () => {
    const after = uiReducer(createInitialState(), {
      type: "SET_MINIMAP_OFFSET",
      payload: { top: 1, right: 1 },
    });
    expect(after.showMinimap).toBe(true);
    expect(store.has("zone-minimap")).toBe(false);
  });
});

describe("parseMinimapOffset", () => {
  it("rejects anything that is not two finite numbers", () => {
    for (const raw of [
      null,
      "",
      "nope",
      "null",
      "42",
      '{"top":1}',
      '{"top":"1","right":2}',
      '{"top":1e999,"right":0}',
    ]) {
      expect(parseMinimapOffset(raw)).toBeNull();
    }
  });

  it("accepts a well-formed value", () => {
    expect(parseMinimapOffset('{"top":10,"right":20}')).toEqual({ top: 10, right: 20 });
  });
});

describe("clampMinimapOffset", () => {
  const container = { width: 800, height: 600 };
  const box = { width: 128, height: 88 };

  it("keeps an in-bounds offset", () => {
    expect(clampMinimapOffset({ top: 100, right: 200 }, container, box)).toEqual({
      top: 100,
      right: 200,
    });
  });

  it("pulls an offset past any edge back inside the container", () => {
    expect(clampMinimapOffset({ top: -40, right: -10 }, container, box)).toEqual({
      top: 6,
      right: 6,
    });
    expect(clampMinimapOffset({ top: 9999, right: 9999 }, container, box)).toEqual({
      top: 600 - 88,
      right: 800 - 128,
    });
  });

  it("pins to the top-right corner when the container is smaller than the box", () => {
    expect(clampMinimapOffset({ top: 50, right: 50 }, { width: 60, height: 40 }, box)).toEqual({
      top: 6,
      right: 6,
    });
  });
});

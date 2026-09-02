/**
 * Minimap visibility — reducer + initial-state semantics.
 *
 * The runner's vitest config is `environment: "node"` (no jsdom), so this
 * follows the repo precedent (`CommitTrafficLight.test.ts`) and exercises the
 * exported pure pieces with a module-level mock of the storage surface.
 *
 * What is worth pinning here is the DEFAULT. Every other persisted toggle in
 * this reducer defaults OFF (`=== "true"`), but the minimap has always been
 * visible on arrival, so it defaults ON (`!== "false"`) and storage only has
 * to remember a deliberate hide. Flipping that comparison would silently make
 * the minimap disappear for every operator who has never touched the toggle —
 * a regression with no error and no visible cause.
 */

import { describe, it, expect, beforeEach, vi } from "vitest";

const store = new Map<string, string>();

vi.mock("@/lib/instance-storage", () => ({
  instanceStorage: {
    getItem: (k: string) => store.get(k) ?? null,
    setItem: (k: string, v: string) => void store.set(k, v),
    removeItem: (k: string) => void store.delete(k),
    getJSON: <T,>(_k: string, fallback: T) => fallback,
    setJSON: () => {},
  },
}));

const { uiReducer, createInitialState } = await import("./useUIState");

beforeEach(() => store.clear());

describe("showMinimap · default", () => {
  it("is ON when nothing has been stored (the never-touched-it case)", () => {
    expect(createInitialState().showMinimap).toBe(true);
  });

  it("is ON for any stored value that is not exactly 'false'", () => {
    for (const v of ["true", "", "1", "off", "FALSE"]) {
      store.set("zone-minimap", v);
      expect(createInitialState().showMinimap).toBe(true);
    }
  });

  it("is OFF only for the exact string 'false'", () => {
    store.set("zone-minimap", "false");
    expect(createInitialState().showMinimap).toBe(false);
  });
});

describe("showMinimap · reducer", () => {
  it("TOGGLE_MINIMAP flips the flag and persists the new value", () => {
    const off = uiReducer(createInitialState(), { type: "TOGGLE_MINIMAP" });
    expect(off.showMinimap).toBe(false);
    expect(store.get("zone-minimap")).toBe("false");

    const on = uiReducer(off, { type: "TOGGLE_MINIMAP" });
    expect(on.showMinimap).toBe(true);
    expect(store.get("zone-minimap")).toBe("true");
  });

  it("survives a remount: a hide is still hidden when state is rebuilt", () => {
    uiReducer(createInitialState(), { type: "TOGGLE_MINIMAP" });
    // The old local `useState` forgot this on every remount, which is half
    // the reason the X button was a dead end.
    expect(createInitialState().showMinimap).toBe(false);
  });

  it("SET_SHOW_MINIMAP persists both directions", () => {
    expect(
      uiReducer(createInitialState(), { type: "SET_SHOW_MINIMAP", payload: false }).showMinimap,
    ).toBe(false);
    expect(store.get("zone-minimap")).toBe("false");

    expect(
      uiReducer(createInitialState(), { type: "SET_SHOW_MINIMAP", payload: true }).showMinimap,
    ).toBe(true);
    expect(store.get("zone-minimap")).toBe("true");
  });

  it("does not disturb the neighbouring overlay toggles", () => {
    const before = createInitialState();
    const after = uiReducer(before, { type: "TOGGLE_MINIMAP" });
    expect(after.showControlPanel).toBe(before.showControlPanel);
    expect(after.focusMode).toBe(before.focusMode);
    expect(after.autoLayout).toBe(before.autoLayout);
    expect(store.has("zone-control-panel")).toBe(false);
  });
});

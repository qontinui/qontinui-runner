/**
 * The ninth emitter: `createControlSnapshot` must hand custom actions to the
 * activity timeline in the canonical `SerializedElementAction` shape, with the
 * author's `effect` intact, and must NOT flatten their names into `actions`.
 *
 * `serializeElementCustomActions` is the REAL SDK projection (only the registry
 * and the observer class are replaced), so this pins the runner to what the
 * pinned `@qontinui/ui-bridge` actually emits.
 */
import { afterEach, describe, expect, it, vi } from "vitest";

const captured: { deps?: { createControlSnapshot: () => unknown } } = {};

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const registry = {
  getAllElements: vi.fn(),
  getAllComponents: vi.fn(() => []),
  getAllWorkflows: vi.fn(() => []),
};

vi.mock("@qontinui/ui-bridge", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@qontinui/ui-bridge")>();
  return { ...actual, getGlobalRegistry: () => registry };
});

vi.mock("@qontinui/ui-bridge/ai", () => ({
  SemanticSnapshotManager: class {},
  BackgroundObserver: class {
    isRunning = false;
    constructor(deps: { createControlSnapshot: () => unknown }) {
      captured.deps = deps;
    }
    start() {
      this.isRunning = true;
    }
    stop() {
      this.isRunning = false;
    }
  },
}));

import { startBackgroundObserver, stopBackgroundObserver } from "./background-observer-service";

afterEach(() => {
  stopBackgroundObserver();
  captured.deps = undefined;
});

describe("background observer control snapshot", () => {
  it("emits custom actions as SerializedElementAction objects, not flattened names", () => {
    const handler = vi.fn();
    registry.getAllElements.mockReturnValue([
      {
        id: "terminal-input-term-1",
        type: "input",
        label: "Terminal",
        actions: ["focus", "blur"],
        customActions: {
          sendKeys: { handler, label: "Send keys", effect: "write" },
          getScrollback: { handler },
        },
        getState: () => ({}),
        registeredAt: 1,
        mounted: true,
      },
    ]);

    startBackgroundObserver();
    expect(captured.deps).toBeDefined();
    const snapshot = captured.deps!.createControlSnapshot() as {
      elements: Array<{ actions: string[]; customActions?: unknown }>;
    };
    const el = snapshot.elements[0];

    expect(el.actions).toEqual(["focus", "blur"]);
    expect(el.customActions).toEqual([
      { id: "sendKeys", label: "Send keys", effect: "write" },
      // Unclassified stays unclassified: no defaulted `effect`.
      { id: "getScrollback" },
    ]);
  });
});

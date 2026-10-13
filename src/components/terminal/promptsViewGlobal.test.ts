import { beforeEach, describe, expect, it, vi } from "vitest";

// The runner's vitest env is `node` (no `localStorage`): back
// `instanceStorage` with an in-memory map. Hoisted, because the module under
// test reads storage at import time — before a plain `const` would exist.
const { mem } = vi.hoisted(() => ({ mem: new Map<string, string>() }));
vi.mock("@/lib/instance-storage", () => ({
  instanceStorage: {
    getItem(key: string): string | null {
      return mem.get(key) ?? null;
    },
    setItem(key: string, value: string): void {
      mem.set(key, value);
    },
  },
}));

import {
  decodePromptOverrides,
  getPromptsViewGlobal,
  setPromptsViewForAll,
} from "./promptsViewGlobal";

beforeEach(() => {
  mem.clear();
});

describe("promptsViewGlobal", () => {
  it("defaults to off at epoch 0", () => {
    expect(getPromptsViewGlobal()).toEqual({ enabled: false, epoch: 0 });
  });

  it("bumps the epoch on every set, even when the default does not change", () => {
    expect(setPromptsViewForAll(true)).toBe(true);
    expect(getPromptsViewGlobal()).toEqual({ enabled: true, epoch: 1 });
    expect(setPromptsViewForAll(true)).toBe(true);
    expect(getPromptsViewGlobal()).toEqual({ enabled: true, epoch: 2 });
  });

  it("adopts a value another window wrote, and never reuses its epoch", () => {
    getPromptsViewGlobal(); // this window last saw { off, 0 }
    mem.set("zone-prompts-view-global", JSON.stringify({ enabled: true, epoch: 5 }));
    expect(getPromptsViewGlobal()).toEqual({ enabled: true, epoch: 5 });
    setPromptsViewForAll(false);
    expect(getPromptsViewGlobal()).toEqual({ enabled: false, epoch: 6 });
  });

  it("treats a corrupt stored value as the default", () => {
    mem.set("zone-prompts-view-global", "{not json");
    expect(getPromptsViewGlobal()).toEqual({ enabled: false, epoch: 0 });
  });
});

describe("decodePromptOverrides", () => {
  it("reads the legacy bare array only at epoch 0", () => {
    expect([...decodePromptOverrides(["a", "b"], 0)]).toEqual(["a", "b"]);
    expect(decodePromptOverrides(["a"], 1).size).toBe(0);
  });

  it("keeps overrides written under the current epoch and drops stale ones", () => {
    expect([...decodePromptOverrides({ epoch: 3, tabs: ["x"] }, 3)]).toEqual(["x"]);
    expect(decodePromptOverrides({ epoch: 2, tabs: ["x"] }, 3).size).toBe(0);
  });

  it("treats garbage as no overrides", () => {
    expect(decodePromptOverrides(null, 0).size).toBe(0);
    expect(decodePromptOverrides("nope", 0).size).toBe(0);
    expect(decodePromptOverrides({ epoch: 0, tabs: [1, "ok"] }, 0).size).toBe(1);
  });
});

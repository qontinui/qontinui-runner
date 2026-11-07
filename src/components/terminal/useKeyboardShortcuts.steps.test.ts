/**
 * The pure steps behind the tmux keyboard chords (plan
 * `2026-10-11-runner-terminal-tmux-work-mode`, Phase 3): the tab walk
 * `Ctrl+Tab` / `Ctrl+Shift+Tab` / `Ctrl+Shift+N` take, and the preset
 * `Ctrl+Shift+L` steps to. The chords themselves are driven through the real
 * hook in `useKeyboardShortcuts.tmux.test.ts`; this file holds the
 * producer-side cases, apart from that entry-point file so it imports nothing
 * the hook does not.
 */

import { describe, expect, it, vi } from "vitest";

// The slash-command registry pulls in the page contexts; nothing here reaches it.
vi.mock("./commands", () => ({ runRegistryAction: vi.fn() }));

import { nextCycledLayoutId } from "./useKeyboardShortcuts";
import { FLOW_GRID_ID, LAYOUT_PRESETS } from "./useZoneLayout";
import { stepShownTab } from "./terminalWorkMode";

describe("stepShownTab", () => {
  const tabs = [{ id: "a" }, { id: "b" }, { id: "c" }];

  it("steps forward and wraps from the last tab to the first", () => {
    expect(stepShownTab(tabs, "a", 1)).toBe("b");
    expect(stepShownTab(tabs, "c", 1)).toBe("a");
  });

  it("steps backward and wraps from the first tab to the last", () => {
    expect(stepShownTab(tabs, "b", -1)).toBe("a");
    expect(stepShownTab(tabs, "a", -1)).toBe("c");
  });

  it("skips exited tabs and tabs that do not match", () => {
    const withDead = [{ id: "a" }, { id: "b", isAlive: false }, { id: "c" }];
    expect(stepShownTab(withDead, "a", 1)).toBe("c");
    expect(stepShownTab(tabs, "a", 1, (id) => id === "a")).toBeNull();
    expect(stepShownTab(tabs, "c", 1, (id) => id === "b")).toBe("b");
  });

  it("has nowhere to go with one tab, and starts at the ends with none shown", () => {
    expect(stepShownTab([{ id: "a" }], "a", 1)).toBeNull();
    expect(stepShownTab(tabs, null, 1)).toBe("a");
    expect(stepShownTab(tabs, null, -1)).toBe("c");
    expect(stepShownTab([], null, 1)).toBeNull();
  });
});

describe("nextCycledLayoutId", () => {
  it("steps to the next preset and wraps", () => {
    expect(nextCycledLayoutId("single")).toBe("split");
    expect(nextCycledLayoutId(LAYOUT_PRESETS.at(-1)!.id)).toBe("single");
  });

  it("is null in the flow grid", () => {
    expect(nextCycledLayoutId(FLOW_GRID_ID)).toBeNull();
  });
});

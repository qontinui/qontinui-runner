/**
 * Keyboard session cycling in tmux work mode (plan
 * `2026-10-11-runner-terminal-tmux-work-mode`, Phase 3).
 *
 * In tmux mode only zone 0 exists and the off-screen tabs are in no zone, so
 * the grid's zone walk (`focusNextZone`, `focusNextNeedsInput`) cannot reach
 * them. `Ctrl+Tab` / `Ctrl+Shift+Tab` / `Ctrl+Shift+N` therefore walk the TAB
 * list (`stepShownTab`) and swap through `assignTabToZone(0, …)` — the same
 * assignment a session-manager click makes.
 *
 * The hook is driven for real: `react` is the hooks harness, the `window`
 * keydown listener it registers is captured, and key events are fed to it.
 */

import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("react", async () => {
  const { hooksHarness } = await import("@/lib/__test-helpers__/hooks-harness");
  return hooksHarness.reactMock;
});
vi.mock("@/lib/surfaceVisible", () => ({ isSurfaceVisible: () => true }));
// The slash-command registry (`runRegistryAction`) pulls in the page contexts;
// none of the chords exercised here reach it.
vi.mock("./commands", () => ({ runRegistryAction: vi.fn() }));

import { hooksHarness as harness } from "@/lib/__test-helpers__/hooks-harness";
import { nextCycledLayoutId, useKeyboardShortcuts } from "./useKeyboardShortcuts";
import { FLOW_GRID_ID, LAYOUT_PRESETS } from "./useZoneLayout";
import { stepShownTab } from "./terminalWorkMode";
import type { SessionState } from "./useZoneLayout";

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

type Handler = (e: Partial<KeyboardEvent>) => void;

interface Fixture {
  press: (e: Partial<KeyboardEvent>) => void;
  shown: () => string | null;
  assignCalls: Array<[number, string]>;
  setActiveIdCalls: string[];
  focusNextNeedsInputCalls: number;
  layoutCalls: Array<[string, { source: string }]>;
  pinCalls: number[];
}

/** Mount the hook with a fake zone layout whose zone 0 is the shown tab. */
function mount(opts: {
  workMode: "grid" | "tmux";
  tabs: Array<{ id: string; isAlive?: boolean }>;
  shown: string;
  sessionStates?: Record<string, SessionState>;
  /** The grid's layout (`gridView.layoutId`): the snapshot's, in tmux. */
  gridLayoutId?: string;
  /** tmux: the shown session's grid zone (`cosmeticZoneFor(0)`). */
  shownGridZone?: number | null;
}): Fixture {
  harness.reset();
  let handler: Handler | null = null;
  vi.stubGlobal("window", {
    addEventListener: (_type: string, h: Handler) => {
      handler = h;
    },
    removeEventListener: () => {},
  });
  const state = { shown: opts.shown };
  const fx: Fixture = {
    press: (e) => {
      if (!handler) throw new Error("no keydown listener registered");
      handler({ preventDefault: () => {}, ctrlKey: false, shiftKey: false, metaKey: false, ...e });
    },
    shown: () => state.shown,
    assignCalls: [],
    setActiveIdCalls: [],
    focusNextNeedsInputCalls: 0,
    layoutCalls: [],
    pinCalls: [],
  };
  const noop = () => {};
  const zoneLayout = {
    isMultiZone: false,
    focusedZone: 0,
    layoutId: "single",
    maximizedZone: null,
    get assignments() {
      return { 0: state.shown };
    },
    layout: { zones: [{}] },
    setFocusedZone: noop,
    focusPrevZone: noop,
    focusNextZone: noop,
    focusNextNeedsInput: () => {
      fx.focusNextNeedsInputCalls++;
    },
    toggleMaximize: noop,
    setMaximizedZone: noop,
    setLayoutId: (id: string, request: { source: string }) => {
      fx.layoutCalls.push([id, request]);
    },
    gridView: { layoutId: opts.gridLayoutId ?? "single" },
    cosmeticZoneFor: (zoneIdx: number) =>
      opts.workMode === "tmux" ? (zoneIdx === 0 ? (opts.shownGridZone ?? null) : null) : zoneIdx,
    assignTabToZone: (idx: number, tabId: string) => {
      fx.assignCalls.push([idx, tabId]);
      if (idx === 0) state.shown = tabId;
    },
    workMode: opts.workMode,
    get shownTabId() {
      return opts.workMode === "tmux" ? state.shown : null;
    },
    requestReveal: noop,
  };
  function Host() {
    harness.beginRender();
    useKeyboardShortcuts({
      activeId: state.shown,
      tabs: opts.tabs,
      dispatch: noop,
      swapSource: null,
      selectedZones: new Set(),
      createAndAssignTerminal: async () => null,
      closeTerminal: noop,
      setActiveId: (id) => fx.setActiveIdCalls.push(id),
      zoneLayout,
      sessionStates: opts.sessionStates ?? {},
      handleRestartInZone: noop,
      labelsAndTags: {
        allTags: [],
        togglePin: (zone: number) => fx.pinCalls.push(zone),
        setActiveTagFilters: noop,
      },
      focusHistory: { goBack: noop, goForward: noop },
      transitionEffects: {
        toggleAutoFocus: noop,
        toggleSound: noop,
        setUnseenNeedsInput: noop,
      },
      incrementMetric: noop,
      addHistoryEvent: noop,
      terminalRefs: new Map(),
      workflowGen: {
        rightPanelMode: null,
        showSidebar: true,
        setShowSidebar: noop,
        setRightPanelMode: noop,
        setSelectedTranscriptSessionId: noop,
      },
      surfaceRef: { current: null },
    });
  }
  harness.renderSettled(Host);
  return fx;
}

const ctrlTab = { key: "Tab", ctrlKey: true };
const ctrlShiftTab = { key: "Tab", ctrlKey: true, shiftKey: true };
const ctrlShiftN = { key: "N", ctrlKey: true, shiftKey: true };

beforeEach(() => vi.unstubAllGlobals());

describe("useKeyboardShortcuts in tmux mode", () => {
  const tabs = [{ id: "a" }, { id: "b" }, { id: "c" }];

  it("Ctrl+Tab swaps each next tab into zone 0 and wraps back to the first", () => {
    const fx = mount({ workMode: "tmux", tabs, shown: "a" });
    const seen: Array<string | null> = [];
    for (let i = 0; i < 3; i++) {
      fx.press(ctrlTab);
      seen.push(fx.shown());
    }
    expect(seen).toEqual(["b", "c", "a"]);
    expect(fx.assignCalls).toEqual([
      [0, "b"],
      [0, "c"],
      [0, "a"],
    ]);
    // The tmux arm swaps; it does not fall through to the single-zone setActiveId.
    expect(fx.setActiveIdCalls).toEqual([]);
  });

  it("Ctrl+Shift+Tab swaps backwards and wraps to the last tab", () => {
    const fx = mount({ workMode: "tmux", tabs, shown: "a" });
    fx.press(ctrlShiftTab);
    expect(fx.shown()).toBe("c");
    fx.press(ctrlShiftTab);
    expect(fx.shown()).toBe("b");
  });

  it("Ctrl+Shift+N swaps in the next session needing input, wrapping, else the next error", () => {
    const fx = mount({
      workMode: "tmux",
      tabs: [{ id: "a" }, { id: "b" }, { id: "c" }, { id: "d" }],
      shown: "c",
      sessionStates: { a: "needs-input", b: "error", d: "working" },
    });
    fx.press(ctrlShiftN);
    expect(fx.shown()).toBe("a");
    // `a` is now shown; no OTHER tab needs input, so the error fallback applies.
    fx.press(ctrlShiftN);
    expect(fx.shown()).toBe("b");
    expect(fx.focusNextNeedsInputCalls).toBe(0);
  });

  it("Ctrl+Shift+N with nothing waiting leaves the shown tab alone", () => {
    const fx = mount({ workMode: "tmux", tabs, shown: "a", sessionStates: { b: "working" } });
    fx.press(ctrlShiftN);
    expect(fx.shown()).toBe("a");
    expect(fx.assignCalls).toEqual([]);
  });

  it("grid mode keeps the single-zone arm: Ctrl+Tab moves activeId, assigns nothing", () => {
    const fx = mount({ workMode: "grid", tabs, shown: "a" });
    fx.press(ctrlTab);
    expect(fx.setActiveIdCalls).toEqual(["b"]);
    expect(fx.assignCalls).toEqual([]);
    fx.press(ctrlShiftN);
    expect(fx.focusNextNeedsInputCalls).toBe(1);
  });
});

describe("layout cycling and pinning in tmux mode", () => {
  const tabs = [{ id: "a" }, { id: "b" }];
  const ctrlShiftL = { key: "L", ctrlKey: true, shiftKey: true };
  const ctrlShiftO = { key: "O", ctrlKey: true, shiftKey: true };

  it("Ctrl+Shift+L steps from the grid tmux returns to, not from the viewport's single", () => {
    const fx = mount({ workMode: "tmux", tabs, shown: "a", gridLayoutId: "quad" });
    fx.press(ctrlShiftL);
    expect(fx.layoutCalls).toEqual([[nextCycledLayoutId("quad"), { source: "operator" }]]);
    expect(nextCycledLayoutId("quad")).not.toBe(nextCycledLayoutId("single"));
  });

  it("Ctrl+Shift+L is a no-op when the grid it would step from is the flow grid", () => {
    const fx = mount({ workMode: "tmux", tabs, shown: "a", gridLayoutId: "flow-grid" });
    fx.press(ctrlShiftL);
    expect(fx.layoutCalls).toEqual([]);
  });

  it("Ctrl+Shift+O pins the shown session's grid zone, or nothing when it has none", () => {
    const zoned = mount({ workMode: "tmux", tabs, shown: "a", shownGridZone: 3 });
    zoned.press(ctrlShiftO);
    expect(zoned.pinCalls).toEqual([3]);
    const unzoned = mount({ workMode: "tmux", tabs, shown: "a", shownGridZone: null });
    unzoned.press(ctrlShiftO);
    expect(unzoned.pinCalls).toEqual([]);
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

/**
 * `/terminal.mode` as the page registers it: `useTerminalCommands` mounted over
 * the terminal-session context, and the action run by id through
 * `runRegistryAction` — the door the keyboard chord and the UI Bridge action
 * use (plan `2026-10-11-runner-terminal-tmux-work-mode`, Phase 3). The context
 * closures are recording stubs; what is asserted is what the registered action
 * did to them and what it reported.
 */

import { beforeEach, describe, expect, it, vi } from "vitest";

const calls = vi.hoisted(() => [] as Array<{ name: string; args: unknown[] }>);
const recorder = vi.hoisted(() => (name: string) => (...args: unknown[]) => {
  calls.push({ name, args });
});
/** The page's work mode, as the mocked zone layout serves it. */
const page = vi.hoisted(() => ({ workMode: "grid" as "grid" | "tmux" }));

vi.mock("@tauri-apps/api/core", () => ({ invoke: async () => ({}) }));
vi.mock("@/lib/instance-storage", () => ({
  instanceStorage: {
    getJSON: (_key: string, fallback: unknown) => fallback,
    setJSON: () => {},
    getItem: () => null,
    setItem: () => {},
  },
}));
vi.mock("@/lib/clipboard", () => ({ writeClipboard: async () => true }));
vi.mock("../terminalHotStore", () => ({
  getTerminalHotStore: () => ({ getField: () => ({}) }),
}));
vi.mock("./orchestrateCommand", () => ({ useOrchestrateCommand: () => {} }));
vi.mock("./useCommandAction", async () => {
  const reg = await import("./registry");
  return { useCommandAction: (a: Parameters<typeof reg.register>[0]) => reg.register(a) };
});
vi.mock("../contexts", () => ({
  usePageSessionGetter: () => () => null,
  useTerminalSession: () => ({
    tabs: [{ id: "tab-a" }, { id: "tab-b" }],
    activeId: "tab-a",
    setActiveId: recorder("session.setActiveId"),
    closeTerminal: recorder("session.closeTerminal"),
    terminalRefs: { current: new Map() },
    sessionStates: {},
    pageId: "page-1",
    stateTimeAccum: { current: {} },
    zoneLayout: {
      focusedZone: 0,
      layout: { id: "quad", zones: [{}, {}, {}, {}] },
      layoutId: "quad",
      maximizedZone: null,
      assignments: { 0: "tab-a", 1: "tab-b" },
      get workMode() {
        return page.workMode;
      },
      toggleMaximize: recorder("zone.toggleMaximize"),
      enterTmuxMode: recorder("zone.enterTmuxMode"),
      exitTmuxMode: recorder("zone.exitTmuxMode"),
      setLayoutId: recorder("zone.setLayoutId"),
    },
    workflowGen: {
      showSidebar: false,
      setShowSidebar: recorder("workflow.setShowSidebar"),
    },
    findingsActions: {},
    analysis: {},
  }),
  useTransitionEffects: () => ({}),
  useUIStateCx: () => ({ state: {}, dispatch: recorder("ui.dispatch"), toggleFocusMode: () => {} }),
  useZoneMetadata: () => ({ labelsAndTags: {}, metrics: { current: {} }, eventHistory: [] }),
}));

import { runRegistryAction } from "./uibridge";
import { useTerminalCommands } from "./useTerminalCommands";
import { MAXIMIZE_IN_TMUX_ERROR } from "../zone-grid/maximizeActions";

beforeEach(() => {
  calls.length = 0;
  page.workMode = "grid";
});

describe("/terminal.mode, registered by the page", () => {
  it("toggles a grid page into tmux mode and opens the session manager", async () => {
    // `useTerminalCommands` is a hook by name only (no React state hooks); with
    // the contexts mocked, calling it is what registers the page's actions.
    useTerminalCommands({} as never);

    const result = await runRegistryAction("terminal.mode", {}, "hotkey");

    expect(result.ok).toBe(true);
    expect(calls.map((c) => [c.name, ...c.args])).toEqual([
      ["zone.enterTmuxMode"],
      ["workflow.setShowSidebar", true],
    ]);
  });
});

describe("/maximize, registered by the page", () => {
  it("maximizes the named zone in grid mode", async () => {
    useTerminalCommands({} as never);

    const result = await runRegistryAction("terminal.maximize", { zone: 2 }, "hotkey");

    expect(result.ok).toBe(true);
    expect(calls.map((c) => [c.name, ...c.args])).toEqual([["zone.toggleMaximize", 1]]);
  });

  it("refuses in tmux mode with the UI Bridge actions' refusal, changing nothing", async () => {
    page.workMode = "tmux";
    useTerminalCommands({} as never);

    const result = await runRegistryAction("terminal.maximize", { zone: 2 }, "hotkey");

    expect(result).toStrictEqual({ ok: false, code: "tmux-mode", message: MAXIMIZE_IN_TMUX_ERROR });
    expect(calls).toEqual([]);
  });
});

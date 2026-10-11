/**
 * `/terminal.mode` as the page registers it: `useTerminalCommands` mounted over
 * the terminal-session context, and the action run by id through
 * `runRegistryAction` — the door the keyboard chord and the UI Bridge action
 * use (plan `2026-10-11-runner-terminal-tmux-work-mode`, Phase 3). The context
 * closures are recording stubs; what is asserted is what the registered action
 * did to them and what it reported.
 */

import { describe, expect, it, vi } from "vitest";

const calls = vi.hoisted(() => [] as Array<{ name: string; args: unknown[] }>);
const recorder = vi.hoisted(() => (name: string) => (...args: unknown[]) => {
  calls.push({ name, args });
});

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
      workMode: "grid",
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

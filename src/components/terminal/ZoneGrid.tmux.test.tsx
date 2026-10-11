// @vitest-environment jsdom
/**
 * `ZoneGrid` in tmux work mode (plan `2026-10-11-runner-terminal-tmux-work-mode`,
 * Phase 2), entered where the page enters it: the REAL `ZoneGrid`, reading its
 * zone layout from the terminal-session context exactly as `TerminalPage` hands
 * it over. Everything below the grid that needs a canvas or the Tauri backend
 * is a stand-in; the decision under test — which tabs are mounted, where, and
 * whether a swap remounts one — is the grid's own.
 *
 * `TerminalInstance` is replaced by a pane that records its mounts, unmounts
 * and `visible` prop, so a swap that remounted a session (losing its xterm
 * buffer and its PTY subscription) shows up as a second mount of that id.
 */

import { act, createElement, forwardRef, useEffect, useState } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { TerminalTab } from "./useTerminalManager";

/** What the stand-in panes record. Hoisted: the mocks write it. */
const rec = vi.hoisted(() => ({
  mounts: [] as string[],
  unmounts: [] as string[],
  /** terminal id → the object minted by its (single) mount. */
  instances: new Map<string, object>(),
  /** terminal id → its current `visible` prop. */
  visible: new Map<string, boolean>(),
}));

/** The zone layout the mocked session context serves. Hoisted: the mock reads it. */
const page = vi.hoisted(() => ({
  zoneLayout: {} as Record<string, unknown>,
  tabs: [] as unknown[],
}));

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(() => Promise.resolve(null)) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(() => Promise.resolve(() => {})) }));
vi.mock("@qontinui/ui-bridge", () => ({ useUIElement: () => ({ ref: () => {} }) }));
vi.mock("@/lib/instance-storage", () => ({
  instanceStorage: {
    getJSON: (_key: string, fallback: unknown) => fallback,
    setJSON: () => {},
    getItem: () => null,
    setItem: () => {},
  },
}));
vi.mock("./useLiveClaudeSessionNames", () => ({ useLiveClaudeSessionNames: () => new Map() }));

vi.mock("./contexts", () => {
  const noop = () => {};
  const terminalRefs = { current: new Map() };
  const shellIntegration = {
    handleFirstInput: noop,
    handleShellIntegration: noop,
    commandHistories: {},
  };
  return {
    useTerminalSession: () => ({
      tabs: page.tabs,
      zoneLayout: page.zoneLayout,
      terminalRefs,
      pageId: "zone-grid-tmux-test",
      markReconnected: noop,
      renameTab: noop,
      closeTerminal: noop,
      sessionStates: {},
      staleTabs: new Set<string>(),
      shellIntegration,
    }),
    useZoneMetadata: () => ({
      labelsAndTags: {
        zoneLabels: {},
        setZoneLabel: noop,
        pinnedZones: new Set<number>(),
        togglePin: noop,
        labelColorMap: {},
        zoneTags: {},
        activeTagFilters: new Set<string>(),
        zoneNotes: {},
        setZoneNote: noop,
      },
      incrementMetric: noop,
    }),
    useTransitionEffects: () => ({
      flashingTabs: new Set<string>(),
      handleRestartInZone: noop,
      pendingRestarts: {},
      cancelPendingRestart: noop,
    }),
    useUIStateCx: () => ({
      state: {
        viewMode: "auto",
        selectedZones: new Set<number>(),
        outputSearch: "",
        swapSource: null,
        resetRatiosKey: 0,
        focusMode: false,
      },
    }),
  };
});

vi.mock("./TerminalInstance", () => {
  const Pane = forwardRef<unknown, { terminalId: string; visible?: boolean }>(function Pane(
    { terminalId, visible = true },
    _ref,
  ) {
    const [instance] = useState(() => ({ terminalId }));
    rec.visible.set(terminalId, visible);
    useEffect(() => {
      rec.mounts.push(terminalId);
      rec.instances.set(terminalId, instance);
      return () => {
        rec.unmounts.push(terminalId);
      };
    }, [terminalId, instance]);
    return createElement("div", { "data-pane": terminalId });
  });
  return { TerminalInstance: Pane };
});

vi.mock("./PlanViewer", () => ({ PlanViewer: () => null }));
vi.mock("./WorkerSessionCell", () => ({ WorkerSessionCell: () => null }));
vi.mock("./suggestions", () => ({ SuggestionChip: () => null }));
vi.mock("./ZoneHoverActions", () => ({ ZoneHoverActions: () => null }));
vi.mock("./SessionInfoDropdown", () => ({ SessionInfoDropdown: () => null }));
vi.mock("./ZonePromptsPanel", async (importOriginal) => ({
  ...(await importOriginal<typeof import("./ZonePromptsPanel")>()),
  ZonePromptsPanel: () => null,
}));
vi.mock("./SessionReviewPanel", async (importOriginal) => ({
  ...(await importOriginal<typeof import("./SessionReviewPanel")>()),
  SessionReviewPanel: () => null,
  SessionReviewToggle: () => null,
}));
vi.mock("./useSessionReview", () => ({
  useSessionReview: () => ({ changes: null, review: null }),
}));
vi.mock("./sessionReviewView", () => ({ reviewBadgeState: () => null }));
vi.mock("./TenantBadge", () => ({ TenantBadge: () => null }));
vi.mock("./RemoteTabControls", () => ({ RemoteTabControls: () => null }));
vi.mock("./useTerminalWindowActions", () => ({
  useTerminalWindowActions: () => ({ popOutTab: () => Promise.resolve() }),
}));

import { ZoneGrid } from "./ZoneGrid";

function pty(id: string): TerminalTab {
  return { id, title: `session ${id}`, pid: 1, isAlive: true, exitCode: null };
}

const noop = () => {};

let container: HTMLDivElement;
let root: Root;

/** Render the page's grid in tmux work mode with `shown` on screen. */
function renderTmuxPage(tabs: TerminalTab[], shown: string) {
  page.tabs = tabs;
  page.zoneLayout = {
    layout: { id: "single", name: "Single", columns: 1, rows: 1, zones: [{ col: "1", row: "1" }] },
    layoutId: "single",
    assignments: { 0: shown },
    focusedZone: 0,
    maximizedZone: null,
    isMultiZone: false,
    assignTabToZone: noop,
    setFocusedZone: noop,
    setMaximizedZone: noop,
    revealRequest: null,
    consumeReveal: noop,
    workMode: "tmux",
    shownTabId: shown,
  };
  // A fresh callback per render: the context is mocked rather than provided,
  // so this is what tells the memoised grid that its inputs moved (the page
  // re-renders it through the real context in the product).
  const onZoneClick = () => {};
  act(() => {
    root.render(createElement(ZoneGrid, { onZoneClick, onZoneDoubleClick: noop, onExit: noop }));
  });
}

function visiblePanes(): string[] {
  return [...rec.visible].filter(([, v]) => v).map(([id]) => id);
}

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  rec.mounts.length = 0;
  rec.unmounts.length = 0;
  rec.instances.clear();
  rec.visible.clear();
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
});

describe("ZoneGrid in tmux work mode", () => {
  it("a swap shows the target without remounting it or any other session", () => {
    const tabs = [pty("a"), pty("b"), pty("c")];
    renderTmuxPage(tabs, "a");
    expect([...rec.mounts].sort()).toEqual(["a", "b", "c"]);
    expect(visiblePanes()).toEqual(["a"]);
    const instanceOfB = rec.instances.get("b");

    renderTmuxPage(tabs, "b");
    expect(visiblePanes()).toEqual(["b"]);
    renderTmuxPage(tabs, "a");
    expect(visiblePanes()).toEqual(["a"]);

    // Every session stayed mounted across both swaps: one mount each, none torn down.
    expect([...rec.mounts].sort()).toEqual(["a", "b", "c"]);
    expect(rec.unmounts).toEqual([]);
    expect(rec.instances.get("b")).toBe(instanceOfB);
  });
});

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
  /** The flow grid's near-viewport tab ids, as the virtualization hook serves them. */
  near: new Set<string>(),
  /** What the grid last reported to `useZoneLayout().reportMountedTabs`. */
  reportedMounted: null as ReadonlySet<string> | null,
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
// jsdom has no IntersectionObserver: the flow grid's near-viewport set is the
// test's to set, as a scrolled grid would report it.
vi.mock("./useZoneVirtualization", async (importOriginal) => ({
  ...(await importOriginal<typeof import("./useZoneVirtualization")>()),
  useZoneVirtualization: () => ({
    nearViewport: page.near,
    registerCell: () => {},
    unregisterCell: () => {},
    cellFor: () => null,
  }),
}));
vi.mock("./useLiveClaudeSessions", () => ({
  useLiveClaudeSessions: () => ({
    read: "ok",
    bySessionId: new Map(),
    names: new Map(),
    pids: new Set(),
  }),
}));

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
vi.mock("./zone-grid/MaximizedHeader", () => ({
  MaximizedHeader: () => createElement("div", { "data-maximized-header": true }),
}));
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
import { mruOnEnter, TMUX_WARM_TAB_LIMIT } from "./terminalWorkMode";
import { synthesizeFlowGrid, type LayoutPreset } from "./useZoneLayout";

function pty(id: string): TerminalTab {
  return { id, title: `session ${id}`, pid: 1, isAlive: true, exitCode: null };
}

const noop = () => {};
const reportMountedTabs = (mounted: ReadonlySet<string>) => {
  page.reportedMounted = mounted;
};

let container: HTMLDivElement;
let root: Root;

/** A grid layout preset of `columns` × 2 zones, as `useZoneLayout` serves it. */
function gridLayout(columns: number) {
  const zones = [];
  for (let row = 1; row <= 2; row++) {
    for (let col = 1; col <= columns; col++) zones.push({ col: String(col), row: String(row) });
  }
  return { id: `grid-${columns}x2`, name: "Grid", columns, rows: 2, zones };
}

/**
 * Render the page's grid in grid work mode with `assignments` zoned, and
 * `maximizedZone` maximized (the maximized view) when given.
 */
function renderGridPage(
  tabs: TerminalTab[],
  layout: LayoutPreset,
  assignments: Record<number, string>,
  maximizedZone: number | null = null,
) {
  page.tabs = tabs;
  page.zoneLayout = {
    layout,
    layoutId: layout.id,
    assignments,
    focusedZone: maximizedZone ?? 0,
    maximizedZone,
    isMultiZone: true,
    assignTabToZone: noop,
    setFocusedZone: noop,
    setMaximizedZone: noop,
    revealRequest: null,
    consumeReveal: noop,
    workMode: "grid",
    shownTabId: null,
    mru: [],
    cosmeticZoneFor: (zoneIdx: number) => zoneIdx,
    reportMountedTabs,
  };
  const onZoneClick = () => {};
  act(() => {
    root.render(createElement(ZoneGrid, { onZoneClick, onZoneDoubleClick: noop, onExit: noop }));
  });
}

/**
 * Render the page's grid in tmux work mode with `shown` on screen. `mru` is
 * the hook's most-recently-shown list (shown first); by default every tab, so
 * every one is warm.
 */
function renderTmuxPage(
  tabs: TerminalTab[],
  shown: string,
  mru: string[] = [shown, ...tabs.map((t) => t.id).filter((id) => id !== shown)],
) {
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
    mru,
    cosmeticZoneFor: () => null,
    reportMountedTabs,
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
  page.near = new Set();
  page.reportedMounted = null;
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

describe("ZoneGrid across grid ⇄ tmux switches", () => {
  /** Every pane's instance, keyed by terminal id, as minted by its one mount. */
  function snapshotInstances(): Map<string, object> {
    return new Map(rec.instances);
  }

  function expectNoRemount(ids: string[], before: Map<string, object>) {
    expect([...rec.mounts].sort()).toEqual([...ids].sort());
    expect(rec.unmounts).toEqual([]);
    for (const id of ids) expect(rec.instances.get(id)).toBe(before.get(id));
  }

  it("keeps every zoned terminal mounted on entering tmux and on leaving it", () => {
    const tabs = [pty("a"), pty("b"), pty("c")];
    // Three sessions zoned in a 2 × 2 grid, one zone empty.
    renderGridPage(tabs, gridLayout(2), { 0: "a", 1: "b", 2: "c" });
    expect([...rec.mounts].sort()).toEqual(["a", "b", "c"]);
    expect(visiblePanes().sort()).toEqual(["a", "b", "c"]);
    const before = snapshotInstances();

    // Enter tmux on b (what `enterTmuxMode` commits: `single`, zone 0 = shown).
    renderTmuxPage(tabs, "b");
    expect(visiblePanes()).toEqual(["b"]);
    expectNoRemount(["a", "b", "c"], before);

    // A swap inside tmux, then back to the grid the operator left.
    renderTmuxPage(tabs, "c");
    renderGridPage(tabs, gridLayout(2), { 0: "a", 1: "b", 2: "c" });
    expect(visiblePanes().sort()).toEqual(["a", "b", "c"]);
    expectNoRemount(["a", "b", "c"], before);
  });

  it("keeps an unzoned grid tab mounted when tmux parks it, and when it is shown", () => {
    const tabs = [pty("a"), pty("b"), pty("c")];
    // A 1 × 2 grid holds two sessions; c has no zone and stays parked.
    const twoZones = { ...gridLayout(1), zones: gridLayout(1).zones.slice(0, 2) };
    renderGridPage(tabs, twoZones, { 0: "a", 1: "b" });
    expect([...rec.mounts].sort()).toEqual(["a", "b", "c"]);
    expect(rec.visible.get("c")).toBe(false);
    const before = snapshotInstances();

    renderTmuxPage(tabs, "a");
    renderTmuxPage(tabs, "c");
    expect(visiblePanes()).toEqual(["c"]);
    renderGridPage(tabs, twoZones, { 0: "a", 1: "b" });
    expect(visiblePanes().sort()).toEqual(["a", "b"]);
    expectNoRemount(["a", "b", "c"], before);
  });

  it("renders one root element whose work-mode marker follows the mode", () => {
    const tabs = [pty("a"), pty("b")];
    renderGridPage(tabs, gridLayout(2), { 0: "a", 1: "b" });
    const gridRoot = container.firstElementChild;
    expect(gridRoot?.getAttribute("data-terminal-work-mode")).toBe("grid");

    renderTmuxPage(tabs, "a");
    // The same DOM node: React kept the root, so it kept everything under it.
    expect(container.firstElementChild).toBe(gridRoot);
    expect(gridRoot?.getAttribute("data-terminal-work-mode")).toBe("tmux");
  });
});

describe("ZoneGrid across grid ⇄ maximized ⇄ tmux", () => {
  function expectNoRemount(ids: string[], before: Map<string, object>) {
    expect([...rec.mounts].sort()).toEqual([...ids].sort());
    expect(rec.unmounts).toEqual([]);
    for (const id of ids) expect(rec.instances.get(id)).toBe(before.get(id));
  }

  it("maximizing, entering tmux from the maximized view, and back never remounts", () => {
    const tabs = [pty("a"), pty("b"), pty("c")];
    const layout = gridLayout(2);
    const zoned = { 0: "a", 1: "b", 2: "c" };
    renderGridPage(tabs, layout, zoned);
    const before = new Map(rec.instances);
    const gridRoot = container.firstElementChild;

    renderGridPage(tabs, layout, zoned, 1);
    expect(visiblePanes()).toEqual(["b"]);
    expect(container.firstElementChild).toBe(gridRoot);
    expect(gridRoot?.getAttribute("data-maximized-zone")).toBe("1");
    // The maximized view keeps its own header.
    expect(container.querySelectorAll("[data-maximized-header]")).toHaveLength(1);

    renderTmuxPage(tabs, "b");
    expect(visiblePanes()).toEqual(["b"]);
    renderGridPage(tabs, layout, zoned, 1);
    renderGridPage(tabs, layout, zoned);
    expect(visiblePanes().sort()).toEqual(["a", "b", "c"]);
    expect(container.querySelectorAll("[data-maximized-header]")).toHaveLength(0);
    expect(container.firstElementChild).toBe(gridRoot);
    expectNoRemount(["a", "b", "c"], before);
  });

  it("stepping the maximized zone moves visibility without remounting", () => {
    const tabs = [pty("a"), pty("b")];
    const layout = gridLayout(1);
    renderGridPage(tabs, layout, { 0: "a", 1: "b" }, 0);
    const before = new Map(rec.instances);
    expect(visiblePanes()).toEqual(["a"]);
    renderGridPage(tabs, layout, { 0: "a", 1: "b" }, 1);
    expect(visiblePanes()).toEqual(["b"]);
    expectNoRemount(["a", "b"], before);
  });
});

describe("ZoneGrid in tmux mode keeps a bounded set mounted", () => {
  it("mounts the shown tab and the warm MRU only; a cold tab mounts when shown", () => {
    const tabs = Array.from({ length: TMUX_WARM_TAB_LIMIT + 4 }, (_, i) => pty(`t${i}`));
    const ids = tabs.map((t) => t.id);
    // MRU: t0 shown, then t1..t(K) warm; the rest have never been shown.
    renderTmuxPage(tabs, "t0", ids.slice(0, TMUX_WARM_TAB_LIMIT + 1));
    expect([...rec.mounts].sort()).toEqual(ids.slice(0, TMUX_WARM_TAB_LIMIT + 1).sort());

    // Showing a cold tab mounts it (a ring replay) and, the MRU now led by it,
    // the least recent warm tab goes cold.
    const cold = `t${TMUX_WARM_TAB_LIMIT + 2}`;
    renderTmuxPage(tabs, cold, [cold, ...ids.slice(0, TMUX_WARM_TAB_LIMIT + 1)]);
    expect(rec.mounts).toContain(cold);
    expect(visiblePanes()).toEqual([cold]);
    expect(rec.unmounts).toEqual([`t${TMUX_WARM_TAB_LIMIT}`]);
  });
});

describe("ZoneGrid DOM order", () => {
  it("follows the zones, then the parked cells — whatever the tab creation order", () => {
    const tabs = [pty("a"), pty("b"), pty("c"), pty("d")];
    const twoZones = { ...gridLayout(1), zones: gridLayout(1).zones.slice(0, 2) };
    renderGridPage(tabs, twoZones, { 0: "c", 1: "a" });
    const order = [...container.querySelectorAll("[data-pane]")].map((el) =>
      el.getAttribute("data-pane"),
    );
    expect(order).toEqual(["c", "a", "b", "d"]);
  });

  it("re-orders by moving cells, not remounting them, when zones swap", () => {
    const tabs = [pty("a"), pty("b")];
    const layout = gridLayout(1);
    renderGridPage(tabs, layout, { 0: "a", 1: "b" });
    const before = new Map(rec.instances);
    const paneA = container.querySelector("[data-pane='a']");
    renderGridPage(tabs, layout, { 0: "b", 1: "a" });
    const order = [...container.querySelectorAll("[data-pane]")].map((el) =>
      el.getAttribute("data-pane"),
    );
    expect(order).toEqual(["b", "a"]);
    // The same DOM node, moved: an xterm host keeps its buffer and canvases.
    expect(container.querySelector("[data-pane='a']")).toBe(paneA);
    expect(rec.unmounts).toEqual([]);
    for (const id of ["a", "b"]) expect(rec.instances.get(id)).toBe(before.get(id));
  });
});

describe("ZoneGrid: entering tmux from a scrolled flow grid", () => {
  it("unmounts none of the panes near the viewport", () => {
    const tabs = Array.from({ length: 20 }, (_, i) => pty(`t${i}`));
    const ids = tabs.map((t) => t.id);
    const assignments = Object.fromEntries(ids.map((id, zone) => [zone, id]));
    const nearIds = ids.slice(10, 16);
    page.near = new Set(nearIds);

    // The flow grid mounts only the zones near the viewport (10–15).
    renderGridPage(tabs, synthesizeFlowGrid(tabs.length), assignments);
    expect([...rec.mounts].sort()).toEqual([...nearIds].sort());
    expect(page.reportedMounted).toEqual(new Set(nearIds));

    // Enter tmux on zone 12, seeding the MRU exactly as `enterTmuxMode` does.
    const mru = mruOnEnter("t12", assignments, [], ids, page.reportedMounted ?? undefined);
    renderTmuxPage(tabs, "t12", mru);

    expect(visiblePanes()).toEqual(["t12"]);
    expect(rec.unmounts).toEqual([]);
    for (const id of nearIds) expect(rec.mounts.filter((m) => m === id)).toHaveLength(1);
  });
});

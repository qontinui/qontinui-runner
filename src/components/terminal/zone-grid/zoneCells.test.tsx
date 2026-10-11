// @vitest-environment jsdom
/**
 * `buildZoneCells` in tmux work mode — the single-viewport cell list (plan
 * `2026-10-11-runner-terminal-tmux-work-mode` Phase 2).
 *
 * The REAL cell list and the REAL `ZoneCell` it wraps every tab in are rendered
 * (React 19 `createRoot` + `act`, no Testing Library), with
 * `classifyTabs(…, "tmux")` deciding the mounts exactly as `ZoneGrid` does.
 *
 * `TerminalInstance` itself cannot run here (xterm needs a real canvas), so it
 * is replaced by a pane that performs the SAME visibility contract over the
 * REAL modules: it constructs a `PaneVisibilityService` from its `visible`
 * prop and drives it from a `[visible]` effect, claims a WebGL slot with a
 * restore path the way `XtermBackend.open()` does, and reports its visibility
 * to the REAL `webglContextLru`. The last `describe` pins those calls in
 * `TerminalInstance.tsx` / `XtermBackend.ts` at source level, so the stand-in
 * cannot silently drift from what it stands in for.
 */

import { act, createElement, forwardRef, useEffect, useRef, useState } from "react";
import { createRoot, type Root } from "react-dom/client";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { TerminalTab } from "../useTerminalManager";
import type { ZoneAssignments } from "../useZoneLayout";
import { classifyTabs } from "../classifyTabs";
import {
  MAX_WEBGL_PANES,
  __resetWebglLruForTest,
  __webglDormantKeys,
  __webglSlotKeys,
} from "../backends/webglContextLru";
import type { ZoneCellSharedProps } from "./ZoneCell";
import { buildZoneCells } from "./zoneCells";

/** What the stand-in panes and viewers record. Hoisted: the mocks write it. */
const rec = vi.hoisted(() => ({
  /** terminal ids in mount order (one entry per TerminalInstance mount). */
  mounts: [] as string[],
  unmounts: [] as string[],
  /** terminal id → the object minted by its (single) mount. */
  instances: new Map<string, object>(),
  /** terminal id → its current `visible` prop. */
  visible: new Map<string, boolean>(),
  /** terminal id → the tier its PaneVisibilityService last declared. */
  tiers: new Map<string, "focused" | "background" | null>(),
  /** WebGL evictions (`downgrade`) and reveal restores, by terminal id. */
  downgrades: [] as string[],
  restores: [] as string[],
  /** Live PlanViewer / WorkerSessionCell mounts, by tab id. */
  plans: new Set<string>(),
  workers: new Set<string>(),
}));

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(() => Promise.resolve(null)) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(() => Promise.resolve(() => {})) }));
vi.mock("@qontinui/ui-bridge", () => ({ useUIElement: () => ({ ref: () => {} }) }));

vi.mock("../TerminalInstance", async () => {
  const { PaneVisibilityService } = await import("../paneVisibilityService");
  const lru = await import("../backends/webglContextLru");
  const Pane = forwardRef<unknown, { terminalId: string; visible?: boolean }>(function Pane(
    { terminalId, visible = true },
    _ref,
  ) {
    const [instance] = useState(() => ({ terminalId }));
    const visibleRef = useRef(visible);
    visibleRef.current = visible;
    const serviceRef = useRef<InstanceType<typeof PaneVisibilityService> | null>(null);
    rec.visible.set(terminalId, visible);

    // Mount: TerminalInstance's init effect — the service is built from the
    // visible prop, the backend claims a WebGL slot as visible (it has no
    // visibility concept) with its eviction + restore paths, then the pane
    // seeds the LRU with its real visibility.
    useEffect(() => {
      rec.mounts.push(terminalId);
      rec.instances.set(terminalId, instance);
      const noop = () => {};
      const service = new PaneVisibilityService(
        {
          enterActiveTier: noop,
          leaveActiveTier: noop,
          enterQuietTier: noop,
          leaveQuietTier: noop,
          flushAck: noop,
          refit: noop,
          resync: noop,
          setVisibilityTier: (tier) => rec.tiers.set(terminalId, tier),
        },
        visibleRef.current,
      );
      serviceRef.current = service;
      service.markReady();
      const slot = lru.acquireWebglSlot(
        terminalId,
        () => rec.downgrades.push(terminalId),
        true,
        () => rec.restores.push(terminalId),
      );
      lru.setWebglSlotVisible(terminalId, visibleRef.current);
      return () => {
        rec.unmounts.push(terminalId);
        service.dispose();
        slot.release();
      };
      // eslint-disable-next-line react-hooks/exhaustive-deps
    }, []);

    // TerminalInstance's `[visible]` effect, verbatim in substance.
    useEffect(() => {
      serviceRef.current?.setVisible(visible);
      lru.setWebglSlotVisible(terminalId, visible);
    }, [visible, terminalId]);

    return createElement("div", { "data-pane": terminalId });
  });
  return { TerminalInstance: Pane };
});

vi.mock("../PlanViewer", () => ({
  PlanViewer: ({ filePath }: { filePath: string }) => {
    useEffect(() => {
      rec.plans.add(filePath);
      return () => void rec.plans.delete(filePath);
    }, [filePath]);
    return null;
  },
}));
vi.mock("../WorkerSessionCell", () => ({
  WorkerSessionCell: ({ taskRunId }: { taskRunId: string }) => {
    useEffect(() => {
      rec.workers.add(taskRunId);
      return () => void rec.workers.delete(taskRunId);
    }, [taskRunId]);
    return null;
  },
}));
vi.mock("../suggestions", () => ({ SuggestionChip: () => null }));
vi.mock("../ZoneHoverActions", () => ({ ZoneHoverActions: () => null }));
vi.mock("../SessionInfoDropdown", () => ({
  SessionInfoDropdown: ({ zoneIndex }: { zoneIndex: number }) =>
    createElement("span", { "data-session-info-zone": zoneIndex }),
}));
vi.mock("../ZonePromptsPanel", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../ZonePromptsPanel")>()),
  ZonePromptsPanel: () => null,
}));
vi.mock("../SessionReviewPanel", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../SessionReviewPanel")>()),
  SessionReviewPanel: () => null,
  SessionReviewToggle: () => null,
}));
vi.mock("../useSessionReview", () => ({
  useSessionReview: () => ({ changes: null, review: null }),
}));
vi.mock("../sessionReviewView", () => ({ reviewBadgeState: () => null }));
vi.mock("../TenantBadge", () => ({ TenantBadge: () => null }));
vi.mock("../RemoteTabControls", () => ({ RemoteTabControls: () => null }));
vi.mock("../useTerminalWindowActions", () => ({
  useTerminalWindowActions: () => ({ popOutTab: () => Promise.resolve() }),
}));

function pty(id: string): TerminalTab {
  return { id, title: `session ${id}`, pid: 1, isAlive: true, exitCode: null };
}
const planTab: TerminalTab = {
  ...pty("plan"),
  type: "plan",
  planFilePath: "/plans/p.md",
};
const workerTab: TerminalTab = { ...pty("worker"), sessionBacked: true, taskRunId: "run-1" };

const noop = () => {};

/** The bag `ZoneGrid` builds, for a tmux page whose zone 0 holds `shown`. */
function cellPropsFor(tabs: TerminalTab[], shown: string): ZoneCellSharedProps {
  const assignments: ZoneAssignments = { 0: shown };
  return {
    tabs,
    assignments,
    tabClassification: classifyTabs(tabs, assignments, false, new Set(), "tmux"),
    isFlowMode: false,
    registerCell: noop,
    unregisterCell: noop,
    focusedZone: 0,
    pageId: "tmux-viewport-test",
    sessionStates: {},
    terminalRefs: new Map(),
    onZoneClick: noop,
    onZoneDoubleClick: noop,
    onExit: noop,
    onFirstInput: noop,
    onShellIntegration: noop,
    onReconnected: noop,
    onTitleChange: noop,
    onAssignTab: noop,
    showLabels: false,
    isMultiZone: false,
    autoCompact: false,
    forceCompact: false,
    showFilterInput: null,
    zoneFilters: {},
    handleZoneMouseDown: noop,
    onSetDropTarget: noop,
    onSetContextMenu: noop,
    onToggleFilterInput: noop,
    onSetZoneFilter: noop,
    onClearZoneFilter: noop,
    onTogglePromptsForTab: noop,
    onToggleReviewForTab: noop,
    isSingleView: true,
  };
}

let container: HTMLDivElement;
let root: Root;

/** Render the tmux viewport — `ZoneGrid`'s root element around the cell list. */
function renderViewport(
  tabs: TerminalTab[],
  assigned: string,
  shownTabId: string | null,
  opts: {
    cellProps?: Partial<ZoneCellSharedProps>;
    cosmeticZoneFor?: (zoneIdx: number) => number | null;
  } = {},
) {
  act(() => {
    root.render(
      createElement(
        "div",
        { "data-terminal-work-mode": "tmux" },
        buildZoneCells({
          workMode: "tmux",
          zones: [{ col: "1", row: "1" }],
          cellProps: { ...cellPropsFor(tabs, assigned), ...opts.cellProps },
          shownTabId,
          maximizedZone: null,
          cosmeticZoneFor: opts.cosmeticZoneFor ?? (() => null),
          promptTabs: new Set<string>(),
          reviewTabs: new Set<string>(),
          dropTargetZone: null,
        }),
      ),
    );
  });
}

function show(tabs: TerminalTab[], shown: string) {
  renderViewport(tabs, shown, shown);
}

/** Viewport cells not parked behind the shown one. */
function visibleCells(): Element[] {
  const viewport = container.querySelector("[data-terminal-work-mode='tmux']")!;
  return [...viewport.children].filter((el) => !el.classList.contains("hidden"));
}

function visiblePanes(): string[] {
  return [...rec.visible].filter(([, v]) => v).map(([id]) => id);
}

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  __resetWebglLruForTest();
  rec.mounts.length = 0;
  rec.unmounts.length = 0;
  rec.instances.clear();
  rec.visible.clear();
  rec.tiers.clear();
  rec.downgrades.length = 0;
  rec.restores.length = 0;
  rec.plans.clear();
  rec.workers.clear();
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
});

describe("tmux cells — keep-mounted swap", () => {
  const tabs = [pty("a"), pty("b"), pty("c")];

  it("mounts every PTY tab once and keeps each instance across a swap", () => {
    show(tabs, "a");
    expect(rec.mounts).toEqual(["a", "b", "c"]);
    const before = new Map(rec.instances);

    show(tabs, "b");
    show(tabs, "c");
    show(tabs, "a");

    expect(rec.mounts).toEqual(["a", "b", "c"]);
    expect(rec.unmounts).toEqual([]);
    for (const id of ["a", "b", "c"]) expect(rec.instances.get(id)).toBe(before.get(id));
  });

  it("shows exactly one pane, and the swap moves it", () => {
    show(tabs, "a");
    expect(visiblePanes()).toEqual(["a"]);
    expect(visibleCells()).toHaveLength(1);

    show(tabs, "b");
    expect(visiblePanes()).toEqual(["b"]);
    expect(visibleCells()).toHaveLength(1);
    expect(visibleCells()[0].querySelector("[data-pane]")?.getAttribute("data-pane")).toBe("b");
  });

  it("declares focused for the shown pane and background for the parked ones", () => {
    show(tabs, "a");
    expect(Object.fromEntries(rec.tiers)).toEqual({
      a: "focused",
      b: "background",
      c: "background",
    });

    show(tabs, "c");
    expect(Object.fromEntries(rec.tiers)).toEqual({
      a: "background",
      b: "background",
      c: "focused",
    });
  });

  it("labels the shown pane's header with its place, k / n, and keeps the full name in a title", () => {
    show(tabs, "b");
    const badge = container.querySelectorAll("[data-tmux-position]");
    expect(badge).toHaveLength(1);
    expect(badge[0].textContent).toBe("2 / 3");
    expect(badge[0].getAttribute("title")).toBe("session b — session 2 of 3");
    expect(container.querySelector("[title='session b']")?.textContent).toBe("session b");
  });

  it("k / n counts the live sessions the session manager lists — not exited ones", () => {
    const withExited = [pty("a"), { ...pty("dead"), isAlive: false }, pty("b"), pty("c")];
    show(withExited, "b");
    expect(container.querySelector("[data-tmux-position]")?.textContent).toBe("2 / 3");
  });

  it("renders zone chrome only on the shown pane — every parked cell is zone 0 too", () => {
    show(tabs, "a");
    // One header's session-info trigger for the whole viewport: chrome on a
    // parked cell would register a second `…-0` UI Bridge id.
    expect(container.querySelectorAll("[data-session-info-zone]")).toHaveLength(1);
    expect(container.querySelectorAll("[data-zone-index]")).toHaveLength(1);
  });

  it("renders the empty zone, and still parks every tab, while no tab is shown", () => {
    renderViewport(tabs, "missing", null);
    expect(rec.mounts).toEqual(["a", "b", "c"]);
    expect(visiblePanes()).toEqual([]);
    expect(visibleCells()).toHaveLength(1);
    expect(visibleCells()[0].textContent).toContain("Zone 1");
  });
});

describe("tmux cells — plan and Conductor worker tabs", () => {
  const tabs = [pty("a"), planTab, workerTab, pty("b")];

  it("never mounts a parked plan or worker tab", () => {
    show(tabs, "a");
    expect(rec.mounts).toEqual(["a", "b"]);
    expect([...rec.plans]).toEqual([]);
    expect([...rec.workers]).toEqual([]);
  });

  it("renders a shown plan or worker tab through its zone path, and unmounts it when swapped away", () => {
    show(tabs, "plan");
    expect([...rec.plans]).toEqual(["/plans/p.md"]);
    expect([...rec.workers]).toEqual([]);
    expect(visiblePanes()).toEqual([]);

    show(tabs, "worker");
    expect([...rec.plans]).toEqual([]);
    expect([...rec.workers]).toEqual(["run-1"]);

    show(tabs, "b");
    expect([...rec.workers]).toEqual([]);
    expect(visiblePanes()).toEqual(["b"]);
    // The PTY panes rode out all three swaps without a remount.
    expect(rec.mounts).toEqual(["a", "b"]);
    expect(rec.unmounts).toEqual([]);
  });
});

describe("grid cells — one mount per tab", () => {
  /** Render the grid's cell list for `assignments` over a 1 × 2 layout. */
  function renderGrid(tabs: TerminalTab[], assignments: ZoneAssignments) {
    act(() => {
      root.render(
        createElement(
          "div",
          { "data-terminal-work-mode": "grid" },
          buildZoneCells({
            workMode: "grid",
            zones: [
              { col: "1", row: "1" },
              { col: "2", row: "1" },
            ],
            cellProps: {
              ...cellPropsFor(tabs, "unused"),
              assignments,
              tabClassification: classifyTabs(tabs, assignments, false, new Set(), "grid"),
              isMultiZone: true,
              showLabels: true,
              isSingleView: false,
            },
            shownTabId: null,
            maximizedZone: null,
            cosmeticZoneFor: (zoneIdx) => zoneIdx,
            promptTabs: new Set<string>(),
            reviewTabs: new Set<string>(),
            dropTargetZone: null,
          }),
        ),
      );
    });
  }

  it("parks an unzoned PTY tab, mounted and not visible, and never a plan or worker tab", () => {
    renderGrid([pty("a"), planTab, workerTab, pty("b"), pty("c")], { 0: "a", 1: "b" });
    expect(rec.mounts).toEqual(["a", "b", "c"]);
    expect(visiblePanes()).toEqual(["a", "b"]);
    expect(rec.visible.get("c")).toBe(false);
    expect([...rec.plans]).toEqual([]);
    expect([...rec.workers]).toEqual([]);
  });

  it("mounts a tab two zones name once, in the first, and leaves the second empty", () => {
    renderGrid([pty("a")], { 0: "a", 1: "a" });
    expect(rec.mounts).toEqual(["a"]);
    expect(container.textContent).toContain("Zone 2");
  });

  it("moves a tab between zones without remounting it", () => {
    const tabs = [pty("a"), pty("b")];
    renderGrid(tabs, { 0: "a", 1: "b" });
    const before = new Map(rec.instances);
    renderGrid(tabs, { 0: "b", 1: "a" });
    expect(rec.mounts).toEqual(["a", "b"]);
    expect(rec.unmounts).toEqual([]);
    for (const id of ["a", "b"]) expect(rec.instances.get(id)).toBe(before.get(id));
  });
});

describe("tmux cells — WebGL context cap", () => {
  // Two more PTY panes than the cap: mounting them evicts the two oldest
  // parked panes (t1, t2) to Canvas.
  const ids = Array.from({ length: MAX_WEBGL_PANES + 2 }, (_, i) => `t${i}`);
  const tabs = ids.map(pty);

  it("downgrades parked panes first when more panes mount than the cap allows", () => {
    show(tabs, "t0");
    expect(rec.downgrades).toEqual(["t1", "t2"]);
    expect(__webglDormantKeys()).toEqual(["t1", "t2"]);
    expect(__webglSlotKeys()).toEqual(["t0", "t3", "t4", "t5", "t6", "t7", "t8", "t9"]);
  });

  it("restores WebGL on the pane a swap reveals, evicting a parked pane instead", () => {
    show(tabs, "t0");
    show(tabs, "t1");

    expect(rec.restores).toEqual(["t1"]);
    // The shown cell comes first (cells follow zone order), so t1's reveal
    // runs before t0's hide: the oldest HIDDEN holder, t3, gives way, and t0 —
    // the pane the operator just left — keeps its context.
    expect(rec.downgrades).toEqual(["t1", "t2", "t3"]);
    expect(__webglDormantKeys()).toEqual(["t2", "t3"]);
    expect(__webglSlotKeys()).toEqual(["t0", "t4", "t5", "t6", "t7", "t8", "t9", "t1"]);
    expect(rec.mounts).toEqual(ids);
    expect(rec.unmounts).toEqual([]);
  });

  it("swapping back to the pane just left needs no restore: it kept its context", () => {
    show(tabs, "t0");
    show(tabs, "t1");
    show(tabs, "t0");
    expect(rec.restores).toEqual(["t1"]);
    expect(__webglSlotKeys().at(-1)).toBe("t0");
    expect(__webglDormantKeys()).toEqual(["t2", "t3"]);
  });

  it("swapping to a pane that gave way restores it", () => {
    show(tabs, "t0");
    show(tabs, "t1");
    show(tabs, "t3");
    expect(rec.restores).toEqual(["t1", "t3"]);
    expect(__webglSlotKeys().at(-1)).toBe("t3");
  });
});

describe("the stand-in pane matches the code it stands in for", () => {
  const instance = readFileSync(resolve(__dirname, "../TerminalInstance.tsx"), "utf8");
  const backend = readFileSync(resolve(__dirname, "../backends/XtermBackend.ts"), "utf8");

  it("TerminalInstance drives the service and the LRU from its visible prop", () => {
    expect(instance).toMatch(
      /useEffect\(\(\) => \{\s*paneServiceRef\.current\?\.setVisible\(visible\);\s*setWebglSlotVisible\(terminalId, visible\);\s*\}, \[visible, terminalId\]\);/,
    );
    expect(instance).toContain("setWebglSlotVisible(terminalId, visibleRef.current);");
  });

  it("XtermBackend claims its slot with an eviction AND a restore path", () => {
    expect(backend).toMatch(
      /acquireWebglSlot\(\s*this\.instanceKey,\s*\(\) => this\.evictWebglRenderer\(\),\s*true,\s*\(\) => this\.restoreWebglRenderer\(\),\s*\)/,
    );
  });
});

describe("tmux cells — zone cosmetics follow the session's grid zone", () => {
  const tabs = [pty("a"), pty("b")];
  // The grid: a in zone 0 labelled "zero", b in zone 2 labelled "two" and pinned.
  const cosmetics = {
    zoneLabels: { 0: "zero", 2: "two" },
    pinnedZones: new Set([2]),
  };

  it("shows the shown session's own label and pin, and edits land on its grid zone", () => {
    const setZoneLabel = vi.fn();
    const togglePin = vi.fn();
    renderViewport(tabs, "b", "b", {
      cellProps: { ...cosmetics, onSetZoneLabel: setZoneLabel, onTogglePin: togglePin },
      cosmeticZoneFor: (zoneIdx) => (zoneIdx === 0 ? 2 : null),
    });
    expect(container.textContent).toContain("two");
    expect(container.textContent).not.toContain("zero");
    const pin = container.querySelector("[title^='Unpin zone']") as HTMLElement | null;
    expect(pin).not.toBeNull();
    act(() => pin!.click());
    expect(togglePin.mock.calls).toEqual([[2]]);
  });

  it("a session with no grid zone shows no cosmetics and offers no editor for them", () => {
    renderViewport(tabs, "b", "b", {
      cellProps: { ...cosmetics, onSetZoneLabel: vi.fn(), onTogglePin: vi.fn() },
      cosmeticZoneFor: () => null,
    });
    expect(container.textContent).not.toContain("zero");
    expect(container.querySelector("[title^='Pin zone']")).toBeNull();
    expect(container.querySelector("[title^='Unpin zone']")).toBeNull();
    expect(container.querySelector("[title='Double-click to add label']")).toBeNull();
  });
});

describe("grid cells — the maximized view", () => {
  const zones = [
    { col: "1", row: "1" },
    { col: "2", row: "1" },
  ];
  function renderMaximized(tabs: TerminalTab[], assignments: ZoneAssignments, maximized: number) {
    act(() => {
      root.render(
        createElement(
          "div",
          { "data-terminal-work-mode": "grid" },
          buildZoneCells({
            workMode: "grid",
            zones,
            cellProps: {
              ...cellPropsFor(tabs, "unused"),
              assignments,
              tabClassification: classifyTabs(tabs, assignments, false, new Set(), "grid"),
              isMultiZone: true,
              showLabels: true,
              isSingleView: true,
            },
            shownTabId: null,
            maximizedZone: maximized,
            maximizedNav: {
              prevZone: null,
              nextZone: null,
              onGoToZone: noop,
              onDeleteAndForward: noop,
            },
            cosmeticZoneFor: (zoneIdx) => zoneIdx,
            promptTabs: new Set<string>(),
            reviewTabs: new Set<string>(),
            dropTargetZone: null,
          }),
        ),
      );
    });
  }

  it("shows the maximized zone's terminal under the maximized header, the others parked", () => {
    renderMaximized([pty("a"), pty("b")], { 0: "a", 1: "b" }, 1);
    expect(visiblePanes()).toEqual(["b"]);
    expect(rec.visible.get("a")).toBe(false);
    expect(container.textContent).toContain("Maximized");
    expect(container.querySelector("[aria-label='Previous terminal']")).not.toBeNull();
    // No zone chrome: the maximized header carries the session's info trigger.
    expect(container.querySelectorAll("[data-zone-index]")).toHaveLength(0);
  });

  it("an empty maximized zone shows its placeholder", () => {
    renderMaximized([pty("a")], { 0: "a" }, 1);
    expect(visiblePanes()).toEqual([]);
    expect(container.textContent).toContain("Zone 2");
  });
});

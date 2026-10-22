/**
 * The session manager drives tmux-mode swaps (plan
 * `2026-10-11-runner-terminal-tmux-work-mode`, Phase 3).
 *
 * A card click runs `openSession` → `liveTabIdForSession` → the context's
 * `onFocusSessionTab`, which this phase deliberately leaves UNCHANGED: with
 * tmux mode pinning the layout to `single`, its `planTabReveal` call already
 * answers `{ zone: 0, assign: true }` for an off-screen tab, so its existing
 * `assignTabToZone` IS the swap. `onFocusSessionTab` is an inline closure in
 * `TerminalSessionContext` (not importable on its own), so `clickRow` below
 * replays its body step for step against the REAL `useZoneLayout` hook,
 * mounted in tmux mode through the hooks harness.
 *
 * The visibility tier is what the tmux viewport declares for each mounted pane
 * — `focused` for `shownTabId`, `background` for every other tab (Phase 2) —
 * so the tier assertions declare exactly that from the hook's `shownTabId`
 * and read the reconciler's verdict back.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("react", async () => {
  const { hooksHarness } = await import("@/lib/__test-helpers__/hooks-harness");
  return hooksHarness.reactMock;
});

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => undefined) }));

const storage = vi.hoisted(() => ({ value: null as unknown }));
vi.mock("@/lib/instance-storage", () => ({
  instanceStorage: {
    getJSON: () => storage.value,
    setJSON: () => {},
  },
}));

import { hooksHarness as harness } from "@/lib/__test-helpers__/hooks-harness";
import { useZoneLayout } from "./useZoneLayout";
import { planTabReveal } from "./tabReveal";
import { liveTabIdForSession, type UnifiedSession } from "./useSessionManager";
import { isRowOnScreen, mergeLiveRows } from "./sessionManagerLiveRows";
import {
  __resetVisibilityTiersForTest,
  declarePaneTier,
  effectiveTier,
} from "./terminalVisibilityTiers";

type ZoneLayout = ReturnType<typeof useZoneLayout>;

const TABS = [
  { id: "a", title: "bash", isAlive: true, spawnName: "shell a" },
  { id: "b", title: "bash", isAlive: true, spawnName: "shell b" },
  { id: "c", title: "bash", isAlive: true, spawnName: "shell c" },
];

/** Mount the hook in tmux mode showing `a`; returns a settled re-render. */
function mountTmux(): () => ZoneLayout {
  harness.reset();
  storage.value = {
    layoutId: "single",
    assignments: { 0: "a" },
    focusedZone: 0,
    workMode: "tmux",
    mru: ["a"],
    gridSnapshot: null,
  };
  const ids = TABS.map((t) => t.id);
  function Host() {
    harness.beginRender();
    return useZoneLayout(ids, "panel-tmux-test", "main");
  }
  return () => harness.renderSettled(Host);
}

/**
 * `openSession` + `onFocusSessionTab` (`useSessionManager.ts`,
 * `contexts/TerminalSessionContext.tsx`), replayed against the hook. Returns
 * the id the panel then reports as selected.
 */
function clickRow(zl: ZoneLayout, row: UnifiedSession): string | null {
  const tabId = liveTabIdForSession(row, TABS);
  if (!tabId) return null;
  const plan = TABS.some((t) => t.id === tabId)
    ? planTabReveal(
        zl.assignments,
        zl.layout.zones.length,
        zl.focusedZone,
        tabId,
        zl.getReservedZones(),
      )
    : null;
  if (!plan) return row.sessionId;
  if (plan.assign) zl.assignTabToZone(plan.zone, tabId);
  if (zl.maximizedZone !== null && zl.maximizedZone !== plan.zone) {
    zl.setMaximizedZone(plan.zone);
  }
  zl.setFocusedZone(plan.zone);
  zl.requestReveal(tabId);
  return row.sessionId;
}

const rows = () => mergeLiveRows([], { tabs: TABS, sessionStates: {}, staleTabs: new Set() });
const rowFor = (tabId: string) => rows().find((r) => r.zoneTabId === tabId)!;

/** Each tab's pane declares its tier from `shownTabId`, as the tmux viewport does. */
function declareViewportTiers(zl: ZoneLayout) {
  const panes = TABS.map((t) =>
    declarePaneTier(t.id, t.id === zl.shownTabId ? "focused" : "background"),
  );
  return (next: ZoneLayout) =>
    TABS.forEach((t, i) => panes[i].update(t.id === next.shownTabId ? "focused" : "background"));
}

beforeEach(() => __resetVisibilityTiersForTest());
afterEach(() => __resetVisibilityTiersForTest());

describe("session manager in tmux mode", () => {
  it("lists every live tab, and marks only the shown one as on screen", () => {
    const zl = mountTmux()();
    expect(zl.layout.zones.length).toBe(1);
    expect(rows().map((r) => [r.sessionId, r.source, isRowOnScreen(r, zl.shownTabId)])).toEqual([
      ["a", "tab", true],
      ["b", "tab", false],
      ["c", "tab", false],
    ]);
  });

  it("clicking an off-screen row swaps it in; the previous tab stays alive in the background", () => {
    const render = mountTmux();
    let zl = render();
    const retier = declareViewportTiers(zl);
    expect(zl.shownTabId).toBe("a");
    expect(effectiveTier("a")).toBe("focused");

    const selected = clickRow(zl, rowFor("b"));
    zl = render();
    retier(zl);

    // Swapped: zone 0 now holds b, and only zone 0 exists.
    expect(zl.workMode).toBe("tmux");
    expect(zl.layoutId).toBe("single");
    expect(zl.assignments).toEqual({ 0: "b" });
    expect(zl.shownTabId).toBe("b");
    // Acknowledged: the clicked row is selected and is the one marked on screen.
    expect(selected).toBe("b");
    expect(isRowOnScreen(rowFor("b"), zl.shownTabId)).toBe(true);
    expect(isRowOnScreen(rowFor("a"), zl.shownTabId)).toBe(false);
    // The previous tab was not closed: still a live tab with a row, next in the
    // MRU, and served at the background tier rather than dropped.
    expect(TABS.find((t) => t.id === "a")?.isAlive).toBe(true);
    expect(rows().map((r) => r.sessionId)).toContain("a");
    expect(zl.mru.slice(0, 2)).toEqual(["b", "a"]);
    expect(effectiveTier("b")).toBe("focused");
    expect(effectiveTier("a")).toBe("background");
    expect(effectiveTier("c")).toBe("background");
    // Keyboard handed to the swapped-in pane.
    expect(zl.revealRequest?.tabId).toBe("b");
  });

  it("clicking the on-screen row changes nothing on screen but still reports it selected", () => {
    const render = mountTmux();
    let zl = render();
    const before = zl.assignments;

    const selected = clickRow(zl, rowFor("a"));
    zl = render();

    expect(zl.assignments).toBe(before);
    expect(zl.shownTabId).toBe("a");
    expect(zl.mru).toEqual(["a"]);
    expect(selected).toBe("a");
  });
});

import type { ReactElement } from "react";
import type { TabClassification } from "../classifyTabs";
import type { TerminalWorkMode } from "../terminalWorkMode";
import { isSyntheticTab } from "../syntheticTabs";
import { ZoneCell, type ZoneCellSharedProps } from "./ZoneCell";
import type { MaximizedNav } from "./MaximizedHeader";

/**
 * The zone cells of one `ZoneGrid` render pass, for BOTH work modes and for
 * the grid's maximized view — one keyed list, so a terminal keeps its React
 * identity when the page switches between grid, maximized and tmux (plan
 * `2026-10-11-runner-terminal-tmux-work-mode` D2).
 *
 * Every tab that owns a mount gets exactly one `ZoneCell`, keyed `tab:<id>`
 * everywhere, and `ZoneGrid` renders the list as the first child of the same
 * root element in every view. React therefore matches each tab's cell across a
 * switch and re-renders it with new props — a new zone, a new presentation —
 * instead of unmounting it and mounting a fresh `TerminalInstance`. A remount
 * throws the xterm buffer away and rebuilds it from the scrollback ring, which
 * is both slower and, for a pane mounted hidden, how the live check saw a
 * session's history come back spliced.
 *
 * DOM order follows the zones: zone 0's cell (its tab's, or the empty zone's),
 * then zone 1's, and so on, then the parked cells in tab creation order. The
 * grid places cells explicitly, so the order does not lay anything out; it is
 * what a screen reader and Tab-key navigation walk, and it should read the way
 * the page looks. Keys do not depend on the order, so when a swap or a mode
 * switch reorders the cells React MOVES their DOM nodes (it never remounts a
 * keyed child for moving). A moved xterm keeps its buffer, its canvases and
 * their WebGL contexts — a context belongs to its canvas element, not to where
 * it sits — and nothing in a cell is an iframe, the one element that reloads
 * when moved. What a move does drop is DOM focus inside the moved node, and
 * every path that changes which terminal the operator should type into hands
 * the keyboard over explicitly (`requestReveal`).
 *
 * Which tabs mount is `classifyTabs`' decision, read here and nowhere else:
 *  - `assigned` / `assigned-live` / `assigned-virtual` → the cell of the zone
 *    the tab owns (a virtual cell mounts its compact card, no instance);
 *  - `hidden` (grid, a live tab with no zone) and `offscreen` (tmux, a warm
 *    off-screen tab) → a PARKED cell: the terminal body alone, mounted with
 *    `visible={false}`. One parking for both modes is what lets a tab that
 *    was unzoned in the grid stay mounted when tmux parks it, and back;
 *  - `offscreen-unmounted` (tmux, a cold off-screen tab, or a plan / worker
 *    tab), and a `hidden` plan or Conductor worker tab → nothing. A cold tab
 *    mounts when it is shown; a plan or worker tab has no PTY to keep warm,
 *    and a parked `TerminalInstance` would attach to a terminal id that names
 *    no process.
 * In the maximized view the maximized zone's cell takes the whole page and
 * every other zone's tab is parked, exactly as an unzoned tab is.
 * One key per tab id makes a second mount owner for a tab structurally
 * impossible (the dual-mount race `classifyTabs` exists to prevent).
 */
export interface ZoneCellsInput {
  workMode: TerminalWorkMode;
  /** The active layout's zones (`layout.zones`); tmux uses zone 0 alone. */
  zones: readonly { col: string; row: string }[];
  /**
   * The bag every cell of this pass shares. Its `tabs` are the tabs in
   * creation order and its `tabClassification` comes from `classifyTabs` for
   * `workMode`.
   */
  cellProps: ZoneCellSharedProps;
  /** tmux: the tab on screen (`useZoneLayout().shownTabId`); ignored in grid mode. */
  shownTabId: string | null;
  /** Grid: the maximized zone, or `null` for the tiled grid. Ignored in tmux. */
  maximizedZone: number | null;
  /** The maximized view's header navigation (with `maximizedZone`). */
  maximizedNav?: MaximizedNav;
  /**
   * The zone whose cosmetics a zone shows (`useZoneLayout().cosmeticZoneFor`):
   * the zone itself in grid mode, the shown session's grid zone in tmux.
   */
  cosmeticZoneFor: (zoneIdx: number) => number | null;
  /** Tabs whose prompts panel is open (keyed by tab id). */
  promptTabs: ReadonlySet<string>;
  /** Tabs whose review panel is open (keyed by tab id). */
  reviewTabs: ReadonlySet<string>;
  /** The zone a tab is being dragged over, if any. */
  dropTargetZone: number | null;
}

/** The React key of a tab's cell — the same in every view. */
export function tabCellKey(tabId: string): string {
  return `tab:${tabId}`;
}

/** The one cell that fills the page: the tmux viewport, or the maximized zone. */
const FULL_PAGE_ZONE = { col: "1", row: "1" };

/**
 * The tmux viewport is one full-page zone whatever the grid-mode settings say:
 * the header always shows (it carries `k / n` and the session switcher), a
 * compact card would replace the only terminal on screen, and there is no grid
 * to scroll, tile or maximize within.
 */
function tmuxCellProps(cellProps: ZoneCellSharedProps): ZoneCellSharedProps {
  return {
    ...cellProps,
    isFlowMode: false,
    showLabels: true,
    isMultiZone: false,
    isSingleView: true,
    autoCompact: false,
    forceCompact: false,
  };
}

/**
 * Is this tab one of the tabs the session manager lists as a live row
 * (`buildTabRows`): alive, and not a synthetic test fixture. `k / n` counts
 * exactly this set, so the header and the panel never disagree about how
 * many sessions there are.
 */
export function isListedLiveTab(tab: { id: string; isAlive?: boolean; __synthetic?: boolean }) {
  return !!tab.isAlive && tab.__synthetic !== true && !isSyntheticTab(tab.id);
}

/**
 * tmux: the shown tab's 1-based place among the listed live tabs, and their
 * count. `undefined` when the shown tab is not one of them (an exited tab,
 * shown only because no live tab is left).
 */
export function panePositionOf(
  tabs: ZoneCellSharedProps["tabs"],
  shownTabId: string | null,
): { index: number; count: number } | undefined {
  const listed = tabs.filter(isListedLiveTab);
  const at = listed.findIndex((t) => t.id === shownTabId);
  return at < 0 ? undefined : { index: at + 1, count: listed.length };
}

/**
 * Does this tab take a PARKED cell — mounted, `visible={false}`, no zone on
 * screen? A Conductor worker or a plan tab never does (no PTY to keep warm).
 */
function takesParkedCell(
  tab: ZoneCellSharedProps["tabs"][number],
  classification: TabClassification | undefined,
): boolean {
  if (classification === "offscreen") return true;
  return classification === "hidden" && tab.type !== "plan" && !tab.sessionBacked;
}

export function buildZoneCells({
  workMode,
  zones,
  cellProps: baseCellProps,
  shownTabId,
  maximizedZone: maximizedInput,
  maximizedNav,
  cosmeticZoneFor,
  promptTabs,
  reviewTabs,
  dropTargetZone,
}: ZoneCellsInput): ReactElement[] {
  const isTmux = workMode === "tmux";
  const maximizedZone = isTmux ? null : maximizedInput;
  const cellProps = isTmux ? tmuxCellProps(baseCellProps) : baseCellProps;
  const { tabs, tabClassification, assignments } = cellProps;
  const visibleZones = isTmux ? zones.slice(0, 1) : zones;
  const parkedZone = zones[0];

  // The zone each tab owns. tmux: the shown tab owns zone 0. Grid: the first
  // zone naming it — a tab named by two zones renders in the first, and the
  // second shows empty rather than mounting the tab a second time.
  const tabOfZone = new Map<number, string>();
  const zoneOfTab = new Map<string, number>();
  const own = (zoneIdx: number, id: string) => {
    tabOfZone.set(zoneIdx, id);
    zoneOfTab.set(id, zoneIdx);
  };
  if (isTmux) {
    if (shownTabId !== null) own(0, shownTabId);
  } else {
    visibleZones.forEach((_, zoneIdx) => {
      const id = assignments[zoneIdx];
      if (id && !zoneOfTab.has(id)) own(zoneIdx, id);
    });
  }
  const tabById = new Map(tabs.map((t) => [t.id, t] as const));
  const panePosition = isTmux ? panePositionOf(tabs, shownTabId) : undefined;

  const parkedCell = (tabId: string, zoneIdx: number): ReactElement => (
    <ZoneCell
      key={tabCellKey(tabId)}
      zone={parkedZone}
      zoneIdx={zoneIdx}
      {...cellProps}
      paneTabId={tabId}
      presentation="parked"
      cosmeticZone={null}
      isDropTarget={false}
      promptsOpen={false}
      reviewOpen={false}
    />
  );

  const zoneCells: ReactElement[] = [];
  const parkedCells: ReactElement[] = [];
  const parkedByTab = new Map<string, ReactElement>();

  visibleZones.forEach((zone, zoneIdx) => {
    const id = tabOfZone.get(zoneIdx);
    const tab = id === undefined ? undefined : tabById.get(id);
    const classification = tab ? tabClassification.get(tab.id) : undefined;
    const ownsZone =
      classification === "assigned" ||
      classification === "assigned-live" ||
      classification === "assigned-virtual";
    const isMaximized = maximizedZone === zoneIdx;
    if (tab && ownsZone) {
      if (maximizedZone !== null && !isMaximized) {
        // Behind the maximized zone: parked, like a tab with no zone. A flow
        // grid's virtual tab mounts nothing here either.
        if (classification !== "assigned-virtual")
          parkedByTab.set(tab.id, parkedCell(tab.id, zoneIdx));
        return;
      }
      zoneCells.push(
        <ZoneCell
          key={tabCellKey(tab.id)}
          zone={isMaximized ? FULL_PAGE_ZONE : zone}
          zoneIdx={zoneIdx}
          {...cellProps}
          paneTabId={tab.id}
          presentation={isMaximized ? "maximized" : "zone"}
          cosmeticZone={cosmeticZoneFor(zoneIdx)}
          maximizedNav={isMaximized ? maximizedNav : undefined}
          panePosition={panePosition}
          isDropTarget={dropTargetZone === zoneIdx}
          promptsOpen={promptTabs.has(tab.id)}
          reviewOpen={reviewTabs.has(tab.id)}
        />,
      );
      return;
    }
    // An empty zone shows its placeholder — in the maximized view only when
    // it is the maximized one (the UI Bridge may maximize an empty zone).
    if (maximizedZone !== null && !isMaximized) return;
    zoneCells.push(
      <ZoneCell
        key={`zone:${zoneIdx}`}
        zone={isMaximized ? FULL_PAGE_ZONE : zone}
        zoneIdx={zoneIdx}
        {...cellProps}
        presentation="zone"
        cosmeticZone={cosmeticZoneFor(zoneIdx)}
        isDropTarget={dropTargetZone === zoneIdx}
        promptsOpen={false}
        reviewOpen={false}
      />,
    );
  });

  // Parked cells after the zones, in tab creation order.
  if (parkedZone) {
    for (const tab of tabs) {
      const parked = parkedByTab.get(tab.id);
      if (parked) {
        parkedCells.push(parked);
        continue;
      }
      if (zoneOfTab.has(tab.id)) continue;
      if (takesParkedCell(tab, tabClassification.get(tab.id))) {
        parkedCells.push(parkedCell(tab.id, 0));
      }
    }
  }
  return [...zoneCells, ...parkedCells];
}

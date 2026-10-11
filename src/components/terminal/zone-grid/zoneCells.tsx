import type { ReactElement } from "react";
import type { TabClassification } from "../classifyTabs";
import type { TerminalWorkMode } from "../terminalWorkMode";
import { ZoneCell, type ZoneCellSharedProps } from "./ZoneCell";

/**
 * The zone cells of one `ZoneGrid` render pass, for BOTH work modes — one
 * keyed list, so a terminal keeps its React identity when the page switches
 * between grid and tmux (plan `2026-10-11-runner-terminal-tmux-work-mode` D2).
 *
 * Every tab that owns a mount gets exactly one `ZoneCell`, keyed `tab:<id>` in
 * either mode, and `ZoneGrid` renders the list as the first child of the same
 * root element in either mode. React therefore matches each tab's cell across a
 * mode switch and re-renders it with new props — a new zone, a new `visible` —
 * instead of unmounting it and mounting a fresh `TerminalInstance`. A remount
 * throws the xterm buffer away and rebuilds it from the scrollback ring, which
 * is both slower and, for a pane mounted hidden, how the live check saw a
 * session's history come back spliced.
 *
 * Where a cell sits is layout, not identity: its `zone` (CSS grid column/row)
 * places it, and a parked cell is `display: none`. The list is in tab creation
 * order, not zone order, so a swap or a mode switch never even moves a DOM
 * node; the grid's explicit placement makes DOM order irrelevant to layout.
 * Empty zones follow, keyed `zone:<index>`.
 *
 * Which tabs mount is `classifyTabs`' decision, read here and nowhere else:
 *  - `assigned` / `assigned-live` / `assigned-virtual` → the cell of the zone
 *    the tab owns (a virtual cell mounts its compact card, no instance);
 *  - `hidden` (grid, a live tab with no zone) and `offscreen` (tmux, every tab
 *    but the shown one) → a PARKED cell: the terminal body alone, mounted with
 *    `visible={false}`. One parking for both modes is what lets a tab that
 *    was unzoned in the grid stay mounted when tmux parks it, and back;
 *  - `offscreen-unmounted`, and a `hidden` plan or Conductor worker tab →
 *    nothing. Neither has a PTY to keep warm, and a parked `TerminalInstance`
 *    would attach to a terminal id that names no process.
 * One key per tab id makes a second mount owner for a tab structurally
 * impossible (the dual-mount race `classifyTabs` exists to prevent).
 */
export interface ZoneCellsInput {
  workMode: TerminalWorkMode;
  /** The active layout's zones (`layout.zones`); tmux uses zone 0 alone. */
  zones: readonly { col: string; row: string }[];
  /**
   * The bag every cell of this pass shares. Its `tabs` are the live tabs in
   * creation order and its `tabClassification` comes from `classifyTabs` for
   * `workMode`.
   */
  cellProps: ZoneCellSharedProps;
  /** tmux: the tab on screen (`useZoneLayout().shownTabId`); ignored in grid mode. */
  shownTabId: string | null;
  /** Tabs whose prompts panel is open (keyed by tab id). */
  promptTabs: ReadonlySet<string>;
  /** Tabs whose review panel is open (keyed by tab id). */
  reviewTabs: ReadonlySet<string>;
  /** The zone a tab is being dragged over, if any. */
  dropTargetZone: number | null;
}

/** The React key of a tab's cell — the same in both work modes. */
export function tabCellKey(tabId: string): string {
  return `tab:${tabId}`;
}

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

function parkedCell(
  tabId: string,
  zone: { col: string; row: string },
  cellProps: ZoneCellSharedProps,
): ReactElement {
  return (
    <ZoneCell
      key={tabCellKey(tabId)}
      zone={zone}
      zoneIdx={0}
      {...cellProps}
      paneTabId={tabId}
      isDropTarget={false}
      promptsOpen={false}
      reviewOpen={false}
    />
  );
}

/**
 * The parked cells alone, for the grid's maximized view: it renders its zones'
 * terminals itself, and the tabs with no zone still need their mount.
 */
export function buildParkedCells(
  zone: { col: string; row: string },
  cellProps: ZoneCellSharedProps,
): ReactElement[] {
  return cellProps.tabs
    .filter((tab) => takesParkedCell(tab, cellProps.tabClassification.get(tab.id)))
    .map((tab) => parkedCell(tab.id, zone, cellProps));
}

export function buildZoneCells({
  workMode,
  zones,
  cellProps: baseCellProps,
  shownTabId,
  promptTabs,
  reviewTabs,
  dropTargetZone,
}: ZoneCellsInput): ReactElement[] {
  const isTmux = workMode === "tmux";
  const cellProps = isTmux ? tmuxCellProps(baseCellProps) : baseCellProps;
  const { tabs, tabClassification, assignments } = cellProps;
  const visibleZones = isTmux ? zones.slice(0, 1) : zones;

  // The zone each tab owns. tmux: the shown tab owns zone 0. Grid: the first
  // zone naming it — a tab named by two zones renders in the first, and the
  // second shows empty rather than mounting the tab a second time.
  const zoneOfTab = new Map<string, number>();
  if (isTmux) {
    if (shownTabId !== null) zoneOfTab.set(shownTabId, 0);
  } else {
    visibleZones.forEach((_, zoneIdx) => {
      const id = assignments[zoneIdx];
      if (id && !zoneOfTab.has(id)) zoneOfTab.set(id, zoneIdx);
    });
  }

  const parkedZone = zones[0];
  const occupiedZones = new Set<number>();
  const cells: ReactElement[] = [];
  tabs.forEach((tab, index) => {
    const classification = tabClassification.get(tab.id);
    if (
      classification === "assigned" ||
      classification === "assigned-live" ||
      classification === "assigned-virtual"
    ) {
      const zoneIdx = zoneOfTab.get(tab.id);
      // Assigned to a zone index past the layout's end: no cell to put it in.
      if (zoneIdx === undefined || zoneIdx >= visibleZones.length) return;
      occupiedZones.add(zoneIdx);
      cells.push(
        <ZoneCell
          key={tabCellKey(tab.id)}
          zone={visibleZones[zoneIdx]}
          zoneIdx={zoneIdx}
          {...cellProps}
          paneTabId={tab.id}
          panePosition={isTmux ? { index: index + 1, count: tabs.length } : undefined}
          isDropTarget={dropTargetZone === zoneIdx}
          promptsOpen={promptTabs.has(tab.id)}
          reviewOpen={reviewTabs.has(tab.id)}
        />,
      );
      return;
    }
    if (parkedZone && takesParkedCell(tab, classification)) {
      cells.push(parkedCell(tab.id, parkedZone, cellProps));
    }
  });

  visibleZones.forEach((zone, zoneIdx) => {
    if (occupiedZones.has(zoneIdx)) return;
    cells.push(
      <ZoneCell
        key={`zone:${zoneIdx}`}
        zone={zone}
        zoneIdx={zoneIdx}
        {...cellProps}
        isDropTarget={dropTargetZone === zoneIdx}
        promptsOpen={false}
        reviewOpen={false}
      />,
    );
  });
  return cells;
}

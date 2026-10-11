import { memo } from "react";
import { ZoneCell, type ZoneCellSharedProps } from "./ZoneCell";

/**
 * tmux work mode's renderer (plan `2026-10-11-runner-terminal-tmux-work-mode`
 * Phase 2, D2): ONE session on screen, full size, and every other live
 * session kept mounted behind it.
 *
 * Each live tab gets exactly one `ZoneCell`, keyed by tab id — the same zone
 * content the grid renders (state border, header with label / pin / session
 * switcher, prompts column, review panel + badge, hover actions with export).
 * The shown tab's cell is visible; every other cell is parked `display: none`
 * with its `TerminalInstance` still mounted and `visible={false}`. A swap
 * (`assignTabToZone(0, id)`) therefore flips two `visible` props and remounts
 * nothing: the revealed pane keeps its xterm scrollback and closes only the
 * gap since it was hidden, through the pane's existing visible → tier path
 * (`focused` on screen, `background` parked) and its ring resync.
 *
 * Which tabs get a parked mount is `classifyTabs`' tmux arm, not a rule of
 * this file: `offscreen` mounts, `offscreen-unmounted` (a plan tab or a
 * Conductor worker — nothing to keep warm, and no process behind its id)
 * mounts nothing until it is the shown tab. This viewport is the only mount
 * owner in tmux mode; `ZoneGrid` renders no `HiddenTerminal` parking beside it.
 */
export interface TmuxViewportProps {
  /**
   * The bag `ZoneGrid` hands every cell of this render pass. Its `tabs` are
   * the live tabs in creation order (the `k / n` order), and its
   * `tabClassification` must come from `classifyTabs(…, "tmux")`.
   */
  cellProps: ZoneCellSharedProps;
  /** The tab on screen — `useZoneLayout().shownTabId`; `null` while no tab is shown. */
  shownTabId: string | null;
  /** The one zone the viewport fills: zone 0 of the pinned `single` layout. */
  zone: { col: string; row: string };
  /** Tabs whose prompts panel is open (keyed by tab id, shared with the grid). */
  promptTabs: ReadonlySet<string>;
  /** Tabs whose review panel is open (keyed by tab id, shared with the grid). */
  reviewTabs: ReadonlySet<string>;
  /** A tab is being dragged over the viewport. */
  isDropTarget: boolean;
}

function TmuxViewportInner({
  cellProps,
  shownTabId,
  zone,
  promptTabs,
  reviewTabs,
  isDropTarget,
}: TmuxViewportProps) {
  const { tabs, tabClassification } = cellProps;
  // The viewport is one full-page zone whatever the grid-mode settings say:
  // the header always shows (it carries `k / n` and the session switcher), a
  // compact card would replace the only terminal on screen, and there is no
  // grid to scroll, tile or maximize within.
  const viewportCellProps: ZoneCellSharedProps = {
    ...cellProps,
    isFlowMode: false,
    showLabels: true,
    isMultiZone: false,
    isSingleView: true,
    autoCompact: false,
    forceCompact: false,
  };
  const shownIndex = shownTabId === null ? -1 : tabs.findIndex((t) => t.id === shownTabId);

  return (
    <div
      className="h-full w-full relative"
      style={{ display: "grid", gridTemplateColumns: "1fr", gridTemplateRows: "1fr" }}
      data-terminal-work-mode="tmux"
    >
      {/* No tab on screen (none live, or the reconcile has not refilled zone 0
          yet): the empty zone, exactly as the `single` layout shows it. */}
      {shownIndex < 0 && (
        <ZoneCell
          key="tmux-empty-zone"
          zone={zone}
          zoneIdx={0}
          {...viewportCellProps}
          isDropTarget={isDropTarget}
          promptsOpen={false}
          reviewOpen={false}
        />
      )}
      {tabs.map((tab, index) => {
        const classification = tabClassification.get(tab.id);
        if (classification !== "assigned" && classification !== "offscreen") return null;
        const shown = index === shownIndex;
        return (
          <ZoneCell
            key={tab.id}
            zone={zone}
            zoneIdx={0}
            {...viewportCellProps}
            paneTabId={tab.id}
            panePosition={shown ? { index: index + 1, count: tabs.length } : undefined}
            isDropTarget={shown && isDropTarget}
            promptsOpen={shown && promptTabs.has(tab.id)}
            reviewOpen={shown && reviewTabs.has(tab.id)}
          />
        );
      })}
    </div>
  );
}

export const TmuxViewport = memo(TmuxViewportInner);

/**
 * One zone's content: the state border, the zone header (label, notes, pin,
 * session switcher, prompts + review toggles), the prompts / review panels,
 * the quick and hover actions, and the terminal body — a `TerminalInstance`,
 * a `PlanViewer` or a Conductor `WorkerSessionCell`.
 *
 * `buildZoneCells` (`zoneCells.tsx`) mounts it for both work modes and for the
 * grid's maximized view: one cell per tab that owns a mount, keyed by tab id
 * everywhere so a grid ⇄ maximized ⇄ tmux switch re-renders a terminal rather
 * than remounting it, plus one cell per empty zone. Its `presentation` says
 * how it shows:
 *  - `zone`: a zone on screen, with the full zone chrome;
 *  - `parked`: no zone on screen (unzoned in the grid, off screen in tmux, or
 *    a zone behind the maximized one) — its terminal body alone,
 *    `display: none`;
 *  - `maximized`: the grid's maximized zone filling the page, under the
 *    maximized view's own header instead of the zone chrome.
 * The three render the SAME element tree with different pieces switched off,
 * so the terminal body sits at the same place in it whichever one a cell is —
 * that, plus the key, is what lets a presentation change leave the terminal
 * mounted. Every cell gets the same {@link ZoneCellSharedProps}, so a zone
 * looks and behaves the same in either mode.
 */
import { memo, useCallback, useMemo, useRef, type RefObject } from "react";
import { Terminal, Filter, RefreshCw, MessageSquare } from "lucide-react";
import type { TerminalTab } from "../useTerminalManager";
import type { SessionState, ZoneAssignments } from "../useZoneLayout";
import {
  TerminalInstance,
  type TerminalInstanceHandle,
  type ShellIntegrationEvent,
} from "../TerminalInstance";
import { PlanViewer } from "../PlanViewer";
import { WorkerSessionCell } from "../WorkerSessionCell";
import { SuggestionChip } from "../suggestions";
import { ZoneHoverActions } from "../ZoneHoverActions";
import { SessionInfoDropdown } from "../SessionInfoDropdown";
import { ZonePromptsPanel } from "../ZonePromptsPanel";
import { SessionReviewPanel, SessionReviewToggle } from "../SessionReviewPanel";
import { reviewBadgeState } from "../sessionReviewView";
import { useSessionReview } from "../useSessionReview";
import {
  PROMPTS_PANEL_GEOMETRY,
  REVIEW_PANEL_GEOMETRY,
  promptsPanelAvailable,
  promptsPanelOrientation,
  promptsStripHeight,
  zoneBodyPadding,
} from "../promptsPanelLayout";
import { useElementHeight } from "../useElementHeight";
import { MaximizedHeader, type MaximizedNav } from "./MaximizedHeader";
import type { TabClassification } from "../classifyTabs";
import { writeToTerminalById } from "../writeToTerminalById";
import { useTabHotSlice } from "../useTerminalHotStore";
import { CompactZoneCard } from "./CompactZoneCard";
import { ZoneLabel } from "./ZoneLabel";
import { ZoneQuickActions } from "./ZoneQuickActions";
import {
  STATE_BORDER_COLORS,
  STATE_COLORS,
  STATE_GLOW,
  ZONE_HEADER_HEIGHT_PX,
  ZONE_FILTER_BAR_HEIGHT_PX,
  MAXIMIZED_HEADER_HEIGHT_PX,
} from "./constants";
import { formatUptime, countMatches, showSoloSessionInfo } from "./utils";

/**
 * Whether a tab offers the PTY review panel: a Claude session to key the
 * review on, not a Conductor worker (its cell carries its own review in the
 * Changes pane) and not a plan viewer.
 */
export function reviewPanelAvailable(
  tab: TerminalTab | undefined,
  promptsAvailable: boolean,
): boolean {
  return promptsAvailable && !!tab && !tab.sessionBacked && tab.type !== "plan";
}

export interface ZoneCellProps {
  zone: { col: string; row: string };
  zoneIdx: number;
  tabs: TerminalTab[];
  assignments: ZoneAssignments;
  tabClassification: Map<string, TabClassification>;
  isFlowMode: boolean;
  registerCell: (tabId: string, el: HTMLElement) => void;
  unregisterCell: (tabId: string) => void;
  focusedZone: number;
  /**
   * Owning terminal page — selects this cell's slice of the hot store, and
   * feeds `TerminalInstance`'s stdin ownership gate (a page-bound pop-out
   * claims its tabs by page, not through the `session_owner` map).
   */
  pageId: string;
  sessionStates: Record<string, SessionState>;
  terminalRefs: Map<string, RefObject<TerminalInstanceHandle | null>>;
  onZoneClick: (zoneIndex: number, ctrlKey?: boolean) => void;
  onZoneDoubleClick: (zoneIndex: number) => void;
  onExit: (terminalId: string, exitCode: number | null) => void;
  onFirstInput: (terminalId: string, input: string) => void;
  onUserInputLine?: (terminalId: string, input: string) => void;
  onShellIntegration: (tabId: string, event: ShellIntegrationEvent) => void;
  onReconnected: (tabId: string) => void;
  onTitleChange: (tabId: string, title: string) => void;
  onAssignTab?: (zoneIndex: number, tabId: string) => void;
  flashingTabs?: Set<string>;
  selectedZones?: Set<number>;
  staleTabs?: Set<string>;
  pinnedZones?: Set<number>;
  onTogglePin?: (zoneIndex: number) => void;
  outputSearchQuery?: string;
  swapSource?: number | null;
  zoneLabels?: Record<number, string>;
  onSetZoneLabel?: (zoneIndex: number, label: string) => void;
  onRestartInZone?: (zoneIndex: number) => void;
  labelColorMap?: Record<string, string>;
  zoneTags?: Record<number, string[]>;
  /**
   * Tags the operator is filtering on. EMPTY means "no filter" — every
   * zone shows. Otherwise a zone is dimmed unless it carries at least
   * one of them (OR, not AND).
   *
   * OR is the semantics the surface implies. `/tag <name>` TOGGLES a tag
   * into a set and `/tag-clear`'s description calls itself "the 'All'
   * pill", which is pill-filter language — pills accumulate alternatives.
   * AND would also make Ctrl+Shift+G incoherent: it cycles the set to
   * exactly ONE tag at a time, so an AND reading would give it no way to
   * express a two-tag selection at all.
   */
  activeTagFilters?: Set<string>;
  commandHistories?: Record<string, { command: string; exitCode: number; timestamp: number }[]>;
  focusMode?: boolean;
  zoneNotes?: Record<number, string>;
  onSetZoneNote?: (zoneIndex: number, note: string) => void;
  onExportZone?: (zoneIndex: number, format: "text" | "markdown" | "json") => void;
  pendingRestarts?: Record<number, number>;
  onCancelRestart?: (zoneIndex: number) => void;
  isDropTarget: boolean;
  showLabels: boolean;
  isMultiZone: boolean;
  autoCompact: boolean;
  forceCompact: boolean;
  showFilterInput: number | null;
  zoneFilters: Record<number, string>;
  handleZoneMouseDown: (zoneIndex: number, e: React.MouseEvent) => void;
  onSetDropTarget: (zone: number | null) => void;
  onSetContextMenu: (menu: { x: number; y: number; zoneIndex: number } | null) => void;
  onToggleFilterInput: (zoneIndex: number) => void;
  onSetZoneFilter: (zoneIndex: number, value: string) => void;
  onClearZoneFilter: (zoneIndex: number) => void;
  /** Is the operator's prompts panel showing for this zone's tab? */
  promptsOpen: boolean;
  onTogglePromptsForTab: (tabId: string) => void;
  /** Is the review panel showing for this zone's tab? (Exclusive with prompts.) */
  reviewOpen: boolean;
  onToggleReviewForTab: (tabId: string) => void;
  /**
   * True when this zone has the whole page (the `single` layout). Selects the
   * prompts panel's orientation: a full-page zone gets the right-hand column,
   * a tiled one the strip under its title bar.
   */
  isSingleView: boolean;
  /**
   * The tab this cell renders; absent for an empty zone. Named explicitly
   * rather than read from `assignments[zoneIdx]` because a parked cell has no
   * zone of its own (every one of them sits in zone 0), and because the cell
   * list — not the cell — decides which tab owns which mount.
   */
  paneTabId?: string;
  /**
   * tmux work mode only: the shown tab's 1-based place among the live tabs and
   * their count, rendered as `k / n` in the zone header.
   */
  panePosition?: { index: number; count: number };
  /** How this cell shows — see the module comment. */
  presentation: ZoneCellPresentation;
  /**
   * The zone whose label, notes, pin, tags and output filter this cell shows
   * and edits (`useZoneLayout().cosmeticZoneFor`). The zone itself in grid
   * mode; in tmux mode the shown session's GRID zone, so each session keeps
   * its own. `null`: the session has no grid zone, so the cell shows no
   * cosmetics and offers no editor for them.
   */
  cosmeticZone: number | null;
  /** The maximized view's header navigation; the `maximized` cell only. */
  maximizedNav?: MaximizedNav;
}

export type ZoneCellPresentation = "zone" | "parked" | "maximized";

/**
 * The props every cell of one render pass shares. `ZoneGrid` builds this bag
 * once and `buildZoneCells` hands it to every cell of either work mode, so the
 * two modes cannot drift apart in what a zone is given.
 */
export type ZoneCellSharedProps = Omit<
  ZoneCellProps,
  | "zone"
  | "zoneIdx"
  | "isDropTarget"
  | "promptsOpen"
  | "reviewOpen"
  | "paneTabId"
  | "panePosition"
  | "presentation"
  | "cosmeticZone"
  | "maximizedNav"
>;

function ZoneCellInner({
  zone,
  zoneIdx,
  tabs,
  assignments,
  tabClassification,
  isFlowMode,
  registerCell,
  unregisterCell,
  focusedZone,
  pageId,
  sessionStates,
  terminalRefs,
  onZoneClick: _onZoneClick,
  onZoneDoubleClick,
  onExit,
  onFirstInput,
  onUserInputLine,
  onShellIntegration,
  onReconnected,
  onTitleChange,
  onAssignTab,
  flashingTabs,
  selectedZones,
  staleTabs,
  pinnedZones,
  onTogglePin,
  outputSearchQuery,
  swapSource,
  zoneLabels,
  onSetZoneLabel,
  onRestartInZone,
  labelColorMap,
  zoneTags,
  activeTagFilters,
  commandHistories,
  focusMode,
  zoneNotes,
  onSetZoneNote,
  onExportZone,
  pendingRestarts,
  onCancelRestart,
  isDropTarget,
  showLabels: showLabelsProp,
  isMultiZone,
  autoCompact,
  forceCompact,
  showFilterInput,
  zoneFilters,
  handleZoneMouseDown,
  onSetDropTarget,
  onSetContextMenu,
  onToggleFilterInput,
  onSetZoneFilter,
  onClearZoneFilter,
  promptsOpen,
  onTogglePromptsForTab,
  reviewOpen,
  onToggleReviewForTab,
  isSingleView,
  paneTabId,
  panePosition,
  presentation,
  cosmeticZone,
  maximizedNav,
}: ZoneCellProps) {
  const tabId = paneTabId;
  const tab = tabs.find((t) => t.id === tabId);
  const classification = tab ? tabClassification.get(tab.id) : undefined;
  // A tab with no zone on screen: parked behind the shown one in tmux mode,
  // unzoned in the grid, or behind the maximized zone. Its cell renders the
  // terminal body alone, `display: none` — no header, no panels, no overlays.
  // Every piece of chrome registers UI Bridge ids keyed by zone index, and a
  // parked cell's index is not a zone on screen, so chrome on it would collide
  // with the visible zone's own registrations.
  const isParked = presentation === "parked";
  // The maximized zone: the maximized view's header replaces the zone chrome.
  const isMaximizedPane = presentation === "maximized";
  const hideZoneChrome = isParked || isMaximizedPane;
  const showLabels = showLabelsProp && !hideZoneChrome;
  // Per-tab subscription — THIS is what stops one pane's output frame from
  // re-rendering the other 39 zones (plan §0 A1). Only the cell whose tab
  // produced output (or whose duration/sparkline/lock state moved) re-renders.
  // A parked tmux cell shows none of the hot data, so it does not subscribe.
  const hot = useTabHotSlice(pageId, isParked ? undefined : tabId);
  const lastLines = hot.lastOutputLines;
  // A far-offscreen assigned zone in flow mode: render only the CompactZoneCard,
  // NO TerminalInstance. Plan tabs are never virtualized (PlanViewer is already
  // lightweight and carries no xterm parser).
  // The maximized zone always mounts its terminal: it fills the page, so a
  // flow grid's virtualization of it would leave the page blank.
  const isVirtual =
    classification === "assigned-virtual" && tab?.type !== "plan" && !isMaximizedPane;
  // Mount the inline TerminalInstance only for a non-plan tab the classifier
  // owns as live (`assigned` in preset mode, `assigned-live` near the viewport
  // in flow mode, `assigned` in tmux mode) or as parked (`hidden` in the grid,
  // `offscreen` in tmux). A virtual zone mounts zero instances.
  const shouldMountInstance =
    !!tab &&
    tab.type !== "plan" &&
    (isMaximizedPane ||
      classification === "assigned" ||
      classification === "assigned-live" ||
      classification === "offscreen" ||
      classification === "hidden");

  // Zero-arg wrapper for ZoneLabel's `onToggleFilter` slot; stable so the
  // label doesn't get a fresh prop on every cell render. The filter is keyed
  // by the cosmetic zone, like the label it sits beside.
  const handleToggleFilterInput = useCallback(() => {
    if (cosmeticZone !== null) onToggleFilterInput(cosmeticZone);
  }, [onToggleFilterInput, cosmeticZone]);

  // Same shape for the prompts toggle. `tabId` (not zoneIdx) is the key — the
  // panel belongs to the conversation, not the tile.
  const handleTogglePrompts = useCallback(
    () => (tabId ? onTogglePromptsForTab(tabId) : undefined),
    [onTogglePromptsForTab, tabId],
  );
  const handleToggleReview = useCallback(
    () => (tabId ? onToggleReviewForTab(tabId) : undefined),
    [onToggleReviewForTab, tabId],
  );

  // Per-tab TerminalInstance callbacks, hoisted out of JSX so the memoized
  // instance actually short-circuits: inline arrows would hand it five fresh
  // props on every ZoneCell render (e.g. a duration tick) and force a full
  // re-render of the xterm host.
  const instanceHandlers = useMemo(
    () =>
      tabId
        ? {
            onReconnected: () => onReconnected(tabId),
            onExit: (code: number | null) => onExit(tabId, code),
            onFirstInput: (input: string) => onFirstInput(tabId, input),
            onUserInputLine: onUserInputLine
              ? (input: string) => onUserInputLine(tabId, input)
              : undefined,
            onShellIntegration: (event: ShellIntegrationEvent) => onShellIntegration(tabId, event),
            onTitleChange: (title: string) => onTitleChange(tabId, title),
          }
        : null,
    [
      tabId,
      onReconnected,
      onExit,
      onFirstInput,
      onUserInputLine,
      onShellIntegration,
      onTitleChange,
    ],
  );

  // Reusable zone-cell DOM registry (Phase 3 observer target + Phase 4 scroll
  // routing). Register only in flow mode — preset layouts never scroll.
  // Same node the flow-mode registry tracks, kept as a plain ref so the
  // prompts panel can measure the zone it has to fit inside.
  const zoneElRef = useRef<HTMLDivElement | null>(null);
  const cellRef = useCallback(
    (el: HTMLDivElement | null) => {
      zoneElRef.current = el;
      // Registered whatever the presentation: a cell parked behind the
      // maximized zone keeps its registration, so restoring the grid does not
      // re-register (and re-observe) every cell. A parked cell never
      // intersects, and nothing scrolls to one — the grid scrolls to a
      // focused tab's ZONE, and a parked tab has none.
      if (!isFlowMode || !tabId) return;
      if (el) registerCell(tabId, el);
      else unregisterCell(tabId);
    },
    [isFlowMode, tabId, registerCell, unregisterCell],
  );

  const isFocused = zoneIdx === focusedZone;
  const state = (tab ? (sessionStates[tab.id as string] ?? "idle") : "idle") as SessionState;
  const borderColor = isFocused
    ? STATE_BORDER_COLORS[state] === "#2a2d3d"
      ? "#7aa2f7"
      : STATE_BORDER_COLORS[state]
    : STATE_BORDER_COLORS[state];

  const isPinned = cosmeticZone !== null && !!pinnedZones?.has(cosmeticZone);
  const zoneLabel = cosmeticZone === null ? undefined : zoneLabels?.[cosmeticZone];
  const zoneNote = cosmeticZone === null ? undefined : zoneNotes?.[cosmeticZone];
  const zoneTagList = cosmeticZone === null ? [] : (zoneTags?.[cosmeticZone] ?? []);
  const setLabel =
    onSetZoneLabel && cosmeticZone !== null
      ? (label: string) => onSetZoneLabel(cosmeticZone, label)
      : undefined;
  const setNote =
    onSetZoneNote && cosmeticZone !== null
      ? (note: string) => onSetZoneNote(cosmeticZone, note)
      : undefined;
  const togglePin =
    onTogglePin && cosmeticZone !== null ? () => onTogglePin(cosmeticZone) : undefined;
  const zoneFilter = cosmeticZone === null ? undefined : zoneFilters[cosmeticZone];
  const useCompact =
    !hideZoneChrome &&
    tab &&
    tab.type !== "plan" &&
    !isFocused &&
    !isPinned &&
    (autoCompact || forceCompact);
  // The card overlays the instance region whenever we render CompactZoneCard —
  // either the operator forced compact/auto-compact (instance stays mounted but
  // hidden) OR the zone is virtualized (no instance at all this render).
  const showCompactCard = useCompact || isVirtual;
  // D1: does this zone need the standalone session-info strip? Computed once —
  // the strip and the terminal body's top padding have to agree, or the strip
  // covers the first output line.
  const soloSessionInfo =
    !hideZoneChrome &&
    showSoloSessionInfo({
      showLabels,
      showCompactCard,
      claudeSessionId: tab?.claudeSessionId,
    });
  // A parked tmux cell offers no prompts/review panel, so it reads no review.
  const promptsAvailable =
    !isParked &&
    promptsPanelAvailable({
      claudeSessionId: tab?.claudeSessionId,
      showCompactCard,
    });
  // Whether the terminal body is on screen: not behind a compact card and not
  // parked behind the shown tmux pane. The instance's `visible` prop — and with
  // it the pane's `focused`/`background` tier — follows this alone.
  const paneVisible = !showCompactCard && !isParked;
  // The filter bar is keyed by the cosmetic zone, like the label beside it.
  const showFilterBar =
    !showCompactCard &&
    !hideZoneChrome &&
    cosmeticZone !== null &&
    showFilterInput === cosmeticZone;
  const reviewAvailable = reviewPanelAvailable(tab, promptsAvailable);
  // The review panel and the prompts panel share one overlay slot.
  const showReviewPanel = reviewAvailable && reviewOpen;
  const showPromptsPanel = promptsAvailable && promptsOpen && !showReviewPanel;
  // Changes + read state for the badge and the panel. Visible-gated: a zone
  // behind a compact card (or virtualized), a parked cell, or one whose tab
  // the grid classifies hidden, reads nothing. The maximized zone is a cell
  // like any other, so its header badge and panel read here too — one read
  // per session, whichever way it is shown.
  const reviewReadable = reviewAvailable && classification !== "hidden";
  const review = useSessionReview(reviewReadable ? (tab?.claudeSessionId ?? null) : null, {
    visible: reviewReadable,
  });
  const reviewBadge = useMemo(
    () => reviewBadgeState(review.changes, review.review),
    [review.changes, review.review],
  );
  const panelOpen = showPromptsPanel || showReviewPanel;
  const panelGeometry = showReviewPanel ? REVIEW_PANEL_GEOMETRY : PROMPTS_PANEL_GEOMETRY;
  // Measured only while a panel is open — a closed panel allocates no observer.
  const zoneHeightPx = useElementHeight(zoneElRef, panelOpen);
  const zoneHeaderPx = isMaximizedPane
    ? MAXIMIZED_HEADER_HEIGHT_PX
    : showLabels || soloSessionInfo
      ? ZONE_HEADER_HEIGHT_PX
      : 0;
  const filterBarPx = showFilterBar ? ZONE_FILTER_BAR_HEIGHT_PX : 0;
  const promptsChromeTopPx = zoneHeaderPx + filterBarPx;
  const promptsOrientation = promptsPanelOrientation({
    isSingleView,
    zoneHeightPx,
    chromeTopPx: promptsChromeTopPx,
    geometry: panelGeometry,
  });
  const promptsStripPx = promptsStripHeight(zoneHeightPx, promptsChromeTopPx, panelGeometry);
  const bodyPad = zoneBodyPadding({
    zoneHeaderPx,
    filterBarPx,
    promptsOpen: panelOpen,
    isSingleView,
    zoneHeightPx,
    geometry: panelGeometry,
  });
  const isFlashing = tab && flashingTabs?.has(tab.id);
  const isStale = tab && staleTabs?.has(tab.id);
  const searchMatch =
    tab && outputSearchQuery
      ? lastLines.some((l) => l.toLowerCase().includes(outputSearchQuery.toLowerCase()))
      : false;
  const isSwapSource = swapSource === zoneIdx;
  const isSelected = selectedZones?.has(zoneIdx);

  const firstTagColor = zoneTagList[0] ? labelColorMap?.[zoneTagList[0]] : undefined;
  // Tag filter. Dimming (rather than unmounting) is the precedent this
  // grid already sets for `outputSearchQuery` a few lines below, and it
  // is the only honest option in a fixed CSS grid: removing a cell would
  // leave a hole where the layout preset put it, and would tear down the
  // zone's live PTY with it.
  const tagFilterActive = (activeTagFilters?.size ?? 0) > 0;
  const tagFilteredOut = tagFilterActive && !zoneTagList.some((t) => activeTagFilters?.has(t));
  const stateColor = STATE_COLORS[state];

  const baseBoxShadow = isDropTarget
    ? "inset 0 0 16px rgba(122, 162, 247, 0.15), 0 0 8px rgba(122, 162, 247, 0.3)"
    : isSwapSource
      ? "0 0 10px rgba(255, 158, 100, 0.5), inset 0 0 6px rgba(255, 158, 100, 0.1)"
      : isFlashing
        ? "0 0 12px rgba(224, 175, 104, 0.6), inset 0 0 8px rgba(224, 175, 104, 0.15)"
        : searchMatch
          ? "0 0 6px rgba(158, 206, 106, 0.4)"
          : isSelected
            ? "0 0 6px rgba(187, 154, 247, 0.3)"
            : isFocused
              ? STATE_GLOW[state]
              : "none";

  const zoneShadow =
    state === "needs-input" && baseBoxShadow === "none"
      ? `inset 3px 0 8px -4px ${stateColor}40`
      : baseBoxShadow;

  return (
    <div
      ref={cellRef}
      className={`group relative overflow-hidden ${isFlashing ? "zone-flash" : ""} ${isParked ? "hidden" : ""}`}
      // Ground truth for the tag filter, readable through the UI Bridge
      // without inferring anything from a computed opacity.
      data-zone-tag-filter={tagFilterActive ? (tagFilteredOut ? "hidden" : "shown") : undefined}
      style={
        isMaximizedPane
          ? // The maximized view draws no zone border: the page is the zone.
            { gridColumn: zone.col, gridRow: zone.row }
          : {
              gridColumn: zone.col,
              gridRow: zone.row,
              border: `${isFocused ? "2px" : isSwapSource ? "2px" : isSelected ? "2px" : searchMatch ? "2px" : "1px"} solid ${
                isDropTarget
                  ? "#7aa2f7"
                  : isSwapSource
                    ? "#ff9e64"
                    : isSelected
                      ? "#bb9af7"
                      : searchMatch
                        ? "#9ece6a"
                        : isStale
                          ? "#e0af68"
                          : borderColor
              }`,
              borderLeftWidth: state === "needs-input" ? "3px" : "2px",
              borderLeftColor: stateColor,
              borderStyle: isSwapSource
                ? "dashed"
                : isStale && !isFocused && !searchMatch
                  ? "dashed"
                  : "solid",
              borderLeftStyle: "solid",
              borderRadius: "4px",
              boxShadow: zoneShadow,
              transition: "border-color 0.2s, box-shadow 0.2s, opacity 0.3s",
              // EVERY dim lives in this one inline value, most-specific first.
              //
              // The output-search dim used to be a Tailwind `opacity-40` CLASS on
              // the same element — and an inline `style.opacity` beats a class,
              // so `opacity: 1` from the focus-mode expression below silently
              // won and the search dim never rendered at all. The tag filter was
              // written the same way and had the same non-effect; a `data-`
              // attribute test passed while the page showed nothing. Two
              // competing mechanisms for one property is the whole bug, so there
              // is now exactly one.
              opacity: tagFilteredOut
                ? 0.25
                : outputSearchQuery && !searchMatch
                  ? 0.4
                  : focusMode && !isFocused && state !== "needs-input" && state !== "error"
                    ? 0.3
                    : 1,
              animation: isFlashing ? "zone-flash-border 1s ease-out" : undefined,
              ...(firstTagColor
                ? {
                    background: `linear-gradient(135deg, ${firstTagColor}08 0%, transparent 60%)`,
                  }
                : {}),
            }
      }
      onMouseDown={(e) => handleZoneMouseDown(zoneIdx, e)}
      onDoubleClick={() => onZoneDoubleClick(zoneIdx)}
      onContextMenu={(e) => {
        e.preventDefault();
        onSetContextMenu({
          x: e.clientX,
          y: e.clientY,
          zoneIndex: zoneIdx,
        });
      }}
      onDragOver={(e) => {
        if (
          e.dataTransfer.types.includes("text/tab-id") ||
          e.dataTransfer.types.includes("text/zone-index")
        ) {
          e.preventDefault();
          e.dataTransfer.dropEffect = "move";
          onSetDropTarget(zoneIdx);
        }
      }}
      onDragLeave={(e) => {
        if (!e.currentTarget.contains(e.relatedTarget as Node)) {
          onSetDropTarget(null);
        }
      }}
      onDrop={(e) => {
        e.preventDefault();
        onSetDropTarget(null);
        const droppedTabId = e.dataTransfer.getData("text/tab-id");
        if (droppedTabId && onAssignTab) {
          onAssignTab(zoneIdx, droppedTabId);
          return;
        }
        const srcZoneStr = e.dataTransfer.getData("text/zone-index");
        if (srcZoneStr && onAssignTab) {
          const srcZone = Number(srcZoneStr);
          if (srcZone !== zoneIdx) {
            const srcTabId = assignments[srcZone];
            const dstTabId = assignments[zoneIdx];
            if (srcTabId) onAssignTab(zoneIdx, srcTabId);
            if (dstTabId) onAssignTab(srcZone, dstTabId);
            if (onSetZoneLabel) {
              const srcLabel = zoneLabels?.[srcZone] ?? "";
              const dstLabel = zoneLabels?.[zoneIdx] ?? "";
              onSetZoneLabel(zoneIdx, srcLabel);
              onSetZoneLabel(srcZone, dstLabel);
            }
            if (onSetZoneNote) {
              const srcNote = zoneNotes?.[srcZone] ?? "";
              const dstNote = zoneNotes?.[zoneIdx] ?? "";
              onSetZoneNote(zoneIdx, srcNote);
              onSetZoneNote(srcZone, dstNote);
            }
          }
        }
      }}
    >
      {!hideZoneChrome &&
        (() => {
          const groupColor = zoneLabel && labelColorMap?.[zoneLabel];
          return groupColor ? (
            <div
              className="absolute left-0 top-0 bottom-0 w-[3px] z-10 rounded-l"
              style={{ backgroundColor: groupColor, opacity: 0.6 }}
            />
          ) : null;
        })()}

      {!hideZoneChrome && pendingRestarts?.[zoneIdx] && (
        <div className="absolute inset-0 z-20 flex items-center justify-center bg-black/40 rounded">
          <div className="flex items-center gap-2 bg-[#1a1b26] border border-[#7dcfff]/30 rounded-lg px-3 py-1.5">
            <RefreshCw className="w-3 h-3 text-[#7dcfff] animate-spin" />
            <span className="text-[10px] text-[#7dcfff]">Restarting...</span>
            <button
              onClick={(e) => {
                e.stopPropagation();
                onCancelRestart?.(zoneIdx);
              }}
              className="text-[9px] text-[#f7768e] hover:text-[#ff9e9e] px-1.5 py-0.5 rounded bg-[#f7768e]/10 hover:bg-[#f7768e]/20 transition-colors"
            >
              Cancel
            </button>
          </div>
        </div>
      )}

      {tab ? (
        <>
          {showCompactCard && (
            <CompactZoneCard
              tab={tab}
              state={state}
              zoneIndex={zoneIdx}
              lastLines={lastLines}
              onQuickApprove={() => writeToTerminalById(terminalRefs, tab.id, "y\r")}
              onQuickReject={() => writeToTerminalById(terminalRefs, tab.id, "n\r")}
              onSendCommand={(text) => writeToTerminalById(terminalRefs, tab.id, `${text}\r`)}
              duration={hot.stateDuration}
              isStale={isStale ?? false}
              searchQuery={outputSearchQuery}
              activity={hot.activity}
              zoneLabel={zoneLabel}
              onSetZoneLabel={setLabel}
              onRestart={onRestartInZone ? () => onRestartInZone(zoneIdx) : undefined}
              groupColor={zoneLabel ? labelColorMap?.[zoneLabel] : undefined}
              uptime={formatUptime(tab.createdAt)}
              lastCommand={commandHistories?.[tab.id]?.slice(-1)[0]?.command}
              note={zoneNote}
              onSetNote={setNote}
              allTabs={tabs}
              assignments={assignments}
              sessionStates={sessionStates}
              onAssignTab={onAssignTab}
              tagColor={firstTagColor}
            />
          )}
          {!showCompactCard && showLabels && (
            <ZoneLabel
              tab={tab}
              state={state}
              zoneIndex={zoneIdx}
              allTabs={tabs}
              assignments={assignments}
              sessionStates={sessionStates}
              onAssignTab={onAssignTab}
              isPinned={isPinned}
              onTogglePin={togglePin}
              zoneLabel={zoneLabel}
              onSetZoneLabel={setLabel}
              onScrollToBottom={() => {
                const ref = terminalRefs.get(tab.id);
                ref?.current?.scrollToBottom();
              }}
              outputLineCount={lastLines.length}
              outputByteSize={lastLines.reduce((sum, l) => sum + l.length, 0)}
              onToggleFilter={cosmeticZone !== null ? handleToggleFilterInput : undefined}
              filterActive={!!zoneFilter}
              onTogglePrompts={promptsAvailable ? handleTogglePrompts : undefined}
              promptsVisible={showPromptsPanel}
              position={panePosition}
              reviewSlot={
                reviewAvailable ? (
                  <SessionReviewToggle
                    open={showReviewPanel}
                    onToggle={handleToggleReview}
                    badge={reviewBadge}
                  />
                ) : undefined
              }
            />
          )}
          {isMaximizedPane && maximizedNav && (
            <MaximizedHeader
              tab={tab}
              zoneIdx={zoneIdx}
              nav={maximizedNav}
              onTogglePrompts={promptsAvailable ? handleTogglePrompts : undefined}
              promptsVisible={showPromptsPanel}
              reviewSlot={
                reviewAvailable ? (
                  <SessionReviewToggle
                    open={showReviewPanel}
                    onToggle={handleToggleReview}
                    badge={reviewBadge}
                    iconClassName="w-3 h-3"
                  />
                ) : undefined
              }
            />
          )}
          {/* D1: the single-zone layout renders no `ZoneLabel`, so before this
              the session-info trigger had NO mount site on a one-terminal page
              — the layout most operators run. This is the same 20px strip
              `ZoneLabel` occupies, carrying only the trigger, and the terminal
              body below gets the same 20px of top padding, so it displaces the
              first output line rather than covering it. Left-anchored, matching
              `ZoneLabel`: `ZoneHoverActions` owns the top-RIGHT corner
              (`top-1 right-1 z-30`) and is NOT multi-zone gated, so a
              right-anchored trigger would sit under it on every hover. */}
          {soloSessionInfo && (
            <div
              className="absolute top-0 left-0 right-0 flex items-center gap-1.5 px-1 bg-[#13141f]/85 backdrop-blur-sm z-10"
              style={{ height: `${ZONE_HEADER_HEIGHT_PX}px` }}
            >
              <SessionInfoDropdown claudeSessionId={tab.claudeSessionId} zoneIndex={zoneIdx} />
              {promptsAvailable && (
                <button
                  onClick={(e) => {
                    e.stopPropagation();
                    handleTogglePrompts();
                  }}
                  onMouseDown={(e) => e.stopPropagation()}
                  className={`p-0.5 rounded transition-colors shrink-0 ${
                    showPromptsPanel
                      ? "text-[#9ece6a] bg-[#9ece6a]/15"
                      : "text-[#565f89] hover:text-[#a9b1d6] hover:bg-[#2a2d3d]/50"
                  }`}
                  title={showPromptsPanel ? "Hide my prompts" : "Show my prompts"}
                  aria-pressed={showPromptsPanel}
                >
                  <MessageSquare className="w-2.5 h-2.5" />
                </button>
              )}
              {reviewAvailable && (
                <SessionReviewToggle
                  open={showReviewPanel}
                  onToggle={handleToggleReview}
                  badge={reviewBadge}
                />
              )}
            </div>
          )}
          {showFilterBar && (
            <div
              className="absolute left-0 right-0 flex items-center gap-2 px-2 bg-[#1a1b26] border-b border-[#2a2d3d] z-10"
              style={{ top: `${zoneHeaderPx}px`, height: `${ZONE_FILTER_BAR_HEIGHT_PX}px` }}
            >
              <Filter className="w-3 h-3 text-[#565f89]" />
              <input
                autoFocus
                value={zoneFilter ?? ""}
                onChange={(e) => onSetZoneFilter(cosmeticZone, e.target.value)}
                onKeyDown={(e) => {
                  e.stopPropagation();
                  if (e.key === "Escape") {
                    onToggleFilterInput(cosmeticZone);
                  }
                }}
                onMouseDown={(e) => e.stopPropagation()}
                onClick={(e) => e.stopPropagation()}
                placeholder="Filter output..."
                className="flex-1 bg-transparent text-[10px] text-[#c0caf5] placeholder-[#565f89] outline-hidden font-mono"
              />
              {zoneFilter && (
                <>
                  <span className="text-[9px] text-[#e0af68] font-mono">
                    {countMatches(lastLines, zoneFilter)} matches
                  </span>
                  <button
                    onClick={() => onClearZoneFilter(cosmeticZone)}
                    onMouseDown={(e) => e.stopPropagation()}
                    className="text-[9px] text-[#565f89] hover:text-[#f7768e] px-1"
                  >
                    Clear
                  </button>
                </>
              )}
            </div>
          )}
          {showPromptsPanel && tab.claudeSessionId && (
            <ZonePromptsPanel
              claudeSessionId={tab.claudeSessionId}
              configDir={tab.claudeConfigDir}
              projectPath={tab.workingDir}
              orientation={promptsOrientation}
              topOffsetPx={promptsChromeTopPx}
              heightPx={promptsStripPx}
              onClose={handleTogglePrompts}
            />
          )}
          {showReviewPanel && (
            <SessionReviewPanel
              handle={review}
              terminalId={tab.id}
              orientation={promptsOrientation}
              topOffsetPx={promptsChromeTopPx}
              heightPx={promptsStripPx}
              onClose={handleToggleReview}
            />
          )}
          {!showCompactCard && !hideZoneChrome && isMultiZone && (
            <ZoneQuickActions
              zoneIndex={zoneIdx}
              isPinned={isPinned}
              onTogglePin={togglePin}
              onMaximize={() => onZoneDoubleClick(zoneIdx)}
              onCopyOutput={() => {
                const lines = lastLines;
                navigator.clipboard.writeText(lines.join("\n"));
              }}
              onScrollToBottom={() => {
                const ref = terminalRefs.get(tab.id);
                ref?.current?.scrollToBottom();
              }}
              lastLines={lastLines}
              state={state}
              onRestart={onRestartInZone ? () => onRestartInZone(zoneIdx) : undefined}
              onExportZone={onExportZone ? (fmt) => onExportZone(zoneIdx, fmt) : undefined}
            />
          )}
          {tab.type === "plan" && tab.planFilePath ? (
            <div className="h-full w-full">
              <PlanViewer filePath={tab.planFilePath} visible={!useCompact && !isParked} />
            </div>
          ) : (
            <div
              className={`h-full w-full ${showCompactCard ? "hidden" : ""}`}
              style={{
                paddingTop: showCompactCard || bodyPad.top === 0 ? undefined : `${bodyPad.top}px`,
                paddingRight:
                  showCompactCard || bodyPad.right === 0 ? undefined : `${bodyPad.right}px`,
              }}
            >
              {/* Layer 1 invariant: mount the TerminalInstance only when the
                  centralized classification owns this tab as live or parked
                  (`shouldMountInstance`). A virtualized (`assigned-virtual`)
                  zone mounts ZERO instances — its CompactZoneCard above is the
                  sole renderer. `buildZoneCells` gives each tab ONE cell, keyed
                  by its id, so the dual-mount race (two owners evicting each
                  other's UI Bridge registrations) stays impossible. */}
              {shouldMountInstance && tab.sessionBacked && tab.taskRunId && (
                <WorkerSessionCell tab={tab} taskRunId={tab.taskRunId} visible={paneVisible} />
              )}
              {shouldMountInstance && !tab.sessionBacked && instanceHandlers && (
                <TerminalInstance
                  ref={terminalRefs.get(tab.id)}
                  terminalId={tab.id}
                  pageId={pageId}
                  visible={paneVisible}
                  isReconnecting={tab.isReconnecting}
                  onReconnected={instanceHandlers.onReconnected}
                  onExit={instanceHandlers.onExit}
                  onFirstInput={instanceHandlers.onFirstInput}
                  onUserInputLine={instanceHandlers.onUserInputLine}
                  onShellIntegration={instanceHandlers.onShellIntegration}
                  onTitleChange={instanceHandlers.onTitleChange}
                />
              )}
            </div>
          )}
        </>
      ) : (
        <div
          className={`h-full w-full flex flex-col items-center justify-center text-xs gap-2 transition-colors ${
            isDropTarget ? "bg-[#7aa2f7]/5" : "bg-transparent"
          }`}
        >
          <div
            className={`p-3 rounded-lg border-2 border-dashed transition-colors ${
              isDropTarget
                ? "border-[#7aa2f7]/50 text-[#7aa2f7]"
                : "border-[#2a2d3d] text-[#565f89]"
            }`}
          >
            <Terminal className="w-5 h-5 mx-auto mb-1.5 opacity-50" />
            <span className="text-[10px] font-medium block text-center">Zone {zoneIdx + 1}</span>
            <span className="text-[9px] opacity-60 block text-center mt-0.5">
              {isDropTarget ? "Release to assign" : "Drag a tab here"}
            </span>
          </div>
        </div>
      )}

      {/* Phase 4 — per-zone suggestion chip overlay. Renders nothing
          when the engine has no chip for this zone, keeping cell
          chrome quiet by default. */}
      {!hideZoneChrome && <SuggestionChip zoneIdx={zoneIdx} />}

      {/* Phase 5 — per-zone hover-revealed action cluster (maximize /
          restart / label / export / close). Fades in on cell hover
          via group-hover from the cell's outer div. */}
      {!hideZoneChrome && (
        <ZoneHoverActions zoneIdx={zoneIdx} labelZone={cosmeticZone} onExportZone={onExportZone} />
      )}
    </div>
  );
}

export const ZoneCell = memo(ZoneCellInner);

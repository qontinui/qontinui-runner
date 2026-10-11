import { memo, useCallback, useMemo, useReducer, useRef, useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { instanceStorage } from "@/lib/instance-storage";
import { pageKey } from "./TerminalPageContext";
import { MessageSquare, ChevronLeft, ChevronRight, Trash2 } from "lucide-react";
import { TerminalInstance } from "./TerminalInstance";
import { PlanViewer } from "./PlanViewer";
import { TabTitle, useDisplayTitleResolver } from "./displayTitle";
import { WorkerSessionCell } from "./WorkerSessionCell";
import type { LayoutPreset } from "./useZoneLayout";
import { FLOW_GRID_ID, FLOW_COLS, MIN_TILE_HEIGHT_PX, findNextZone } from "./useZoneLayout";
import {
  ZoneContextMenu,
  buildZoneCells,
  buildParkedCells,
  reviewPanelAvailable,
  type ZoneCellSharedProps,
} from "./zone-grid";
import {
  useTerminalSession,
  useZoneMetadata,
  useTransitionEffects,
  useUIStateCx,
} from "./contexts";
import { SessionInfoDropdown } from "./SessionInfoDropdown";
import { ZonePromptsPanel } from "./ZonePromptsPanel";
import { SessionReviewPanel, SessionReviewToggle } from "./SessionReviewPanel";
import { reviewBadgeState } from "./sessionReviewView";
import { useSessionReview } from "./useSessionReview";
import {
  PROMPTS_PANEL_GEOMETRY,
  REVIEW_PANEL_GEOMETRY,
  toggleZonePanel,
} from "./promptsPanelLayout";
import { classifyTabs } from "./classifyTabs";
import { useZoneVirtualization } from "./useZoneVirtualization";
import {
  scrollCellIntoView,
  focusedTabIdFor,
  newestAddedTabId,
  zoneForTab,
} from "./flowScrollRouting";
import { writeToTerminalById } from "./writeToTerminalById";

export type ViewMode = "auto" | "full" | "compact";

/** Per-page storage key for the set of tabs showing their prompts panel. */
export function promptTabsStorageKey(pageId: string): string {
  return pageKey(pageId, "zone-prompt-tabs");
}

function loadPromptTabs(pageId: string): Set<string> {
  return new Set(instanceStorage.getJSON<string[]>(promptTabsStorageKey(pageId), []));
}

/** Per-page storage key for the set of tabs showing their review panel. */
export function reviewTabsStorageKey(pageId: string): string {
  return pageKey(pageId, "zone-review-tabs");
}

interface PanelTabs {
  prompts: ReadonlySet<string>;
  review: ReadonlySet<string>;
}

function loadPanelTabs(pageId: string): PanelTabs {
  return {
    prompts: loadPromptTabs(pageId),
    review: new Set(instanceStorage.getJSON<string[]>(reviewTabsStorageKey(pageId), [])),
  };
}

/**
 * Height of the maximized-view header strip. It used to be implicit (`py-1`
 * around a 10px line); the prompts column has to start exactly below it, so
 * the value is now stated once and shared by both.
 */
const MAXIMIZED_HEADER_HEIGHT_PX = 26;

interface ZoneGridProps {
  /** Callbacks from useZoneActions — kept as props until that hook moves to context */
  onZoneClick: (zoneIndex: number, ctrlKey?: boolean) => void;
  onZoneDoubleClick: (zoneIndex: number) => void;
  onExit: (terminalId: string, exitCode: number | null) => void;
  onExportZone?: (zoneIndex: number, format: "text" | "markdown" | "json") => void;
  /**
   * Per-line input forwarder for mid-session probes (Phase 3 of
   * `plans/pty-launched-ai-tabs-warning-plan.md`). Fires for every
   * non-empty newline-terminated line typed into any terminal. Consumer
   * applies its own debounce + gating.
   */
  onUserInputLine?: (terminalId: string, input: string) => void;
}

interface GridState {
  dropTargetZone: number | null;
  contextMenu: { x: number; y: number; zoneIndex: number } | null;
  zoneFilters: Record<number, string>;
  showFilterInput: number | null;
  colRatios: number[];
  rowRatios: number[];
  uptimeTick: number;
}

type GridAction =
  | { type: "SET_DROP_TARGET"; zone: number | null }
  | { type: "SET_CONTEXT_MENU"; menu: { x: number; y: number; zoneIndex: number } | null }
  | { type: "SET_ZONE_FILTER"; zoneIndex: number; value: string }
  | { type: "CLEAR_ZONE_FILTER"; zoneIndex: number }
  | { type: "TOGGLE_FILTER_INPUT"; zoneIndex: number }
  | { type: "SET_COL_RATIOS"; ratios: number[] }
  | { type: "SET_ROW_RATIOS"; ratios: number[] }
  | { type: "TICK_UPTIME" };

function gridReducer(state: GridState, action: GridAction): GridState {
  switch (action.type) {
    case "SET_DROP_TARGET":
      return { ...state, dropTargetZone: action.zone };
    case "SET_CONTEXT_MENU":
      return { ...state, contextMenu: action.menu };
    case "SET_ZONE_FILTER":
      return {
        ...state,
        zoneFilters: { ...state.zoneFilters, [action.zoneIndex]: action.value },
      };
    case "CLEAR_ZONE_FILTER": {
      const next = { ...state.zoneFilters };
      delete next[action.zoneIndex];
      return { ...state, zoneFilters: next };
    }
    case "TOGGLE_FILTER_INPUT":
      return {
        ...state,
        showFilterInput: state.showFilterInput === action.zoneIndex ? null : action.zoneIndex,
      };
    case "SET_COL_RATIOS":
      return { ...state, colRatios: action.ratios };
    case "SET_ROW_RATIOS":
      return { ...state, rowRatios: action.ratios };
    case "TICK_UPTIME":
      return { ...state, uptimeTick: state.uptimeTick + 1 };
  }
}

function createInitialGridState(layout: LayoutPreset): GridState {
  // Uses un-namespaced keys for initial state (covers the "default" page).
  // Non-default pages get corrected immediately by the layout-change effect.
  const parsedCols = instanceStorage.getJSON<number[]>(`zone-col-ratios-${layout.id}`, []);
  const parsedRows = instanceStorage.getJSON<number[]>(`zone-row-ratios-${layout.id}`, []);
  return {
    dropTargetZone: null,
    contextMenu: null,
    zoneFilters: {},
    showFilterInput: null,
    colRatios: parsedCols.length === layout.columns ? parsedCols : Array(layout.columns).fill(1),
    rowRatios: parsedRows.length === layout.rows ? parsedRows : Array(layout.rows).fill(1),
    uptimeTick: 0,
  };
}

/**
 * `React.memo` (plan `2026-07-28-runner-many-sessions-performance` Phase 1):
 * `TerminalPage` re-renders for plenty of reasons that have nothing to do with
 * the grid, and every one of those used to walk the whole zone tree. The five
 * props are stable `useCallback`s from `useZoneActions`, so the memo actually
 * holds. Hot per-tab data no longer flows through here at all — each
 * `ZoneCell` subscribes to its own tab's slice of the terminal hot store.
 */
function ZoneGridInner({
  onZoneClick,
  onZoneDoubleClick,
  onExit,
  onExportZone,
  onUserInputLine,
}: ZoneGridProps) {
  const resolveTitle = useDisplayTitleResolver();
  // Read all data from contexts
  const session = useTerminalSession();
  const {
    tabs,
    zoneLayout,
    terminalRefs: terminalRefsRef,
    pageId,
    markReconnected,
    renameTab,
    closeTerminal,
  } = session;
  // terminalRefsRef holds a stable Map<tabId, ref> — it's a per-tab ref cache
  // that's written by TerminalInstance's ref callbacks, not state that drives
  // rendering. The Map identity is stable for the component's lifetime, so
  // reading .current during render is safe. react-hooks/refs can't tell this
  // apart from a ref whose .current mutation should trigger a re-render.
  // eslint-disable-next-line react-hooks/refs
  const terminalRefs = terminalRefsRef.current;
  const layout = zoneLayout.layout;
  const assignments = zoneLayout.assignments;
  const focusedZone = zoneLayout.focusedZone;
  const maximizedZone = zoneLayout.maximizedZone;
  const onAssignTab = zoneLayout.assignTabToZone;
  const stateTracking = session;
  const sessionStates = stateTracking.sessionStates;
  const staleTabs = stateTracking.staleTabs;
  // Session-state tracking is fed by the single global `terminal-output` tap in
  // `TerminalSessionContext.PageSessionScope` (Phase 2), NOT by instance
  // `onOutput` callbacks — so tracking survives Phase 3 instance unmounting.
  // ZoneGrid no longer routes output into `handleOutput`; instances keep their
  // own `terminal-output` listener purely for the xterm write/render path.
  const onReconnected = markReconnected;
  /**
   * Layer 4 polish (OSC 0/2 title): plumb the latest title from the Rust
   * grid up to the tab title via `renameTab`. The title field is already
   * surfaced in `GridSnapshot` (parsed by `vte::Perform::osc_dispatch` in
   * `src-tauri/src/terminal/grid.rs`); this is the wire-up.
   *
   * Skip empty / whitespace-only titles defensively. The TerminalInstance
   * de-dupes against its last-reported value, so this fires at most once
   * per real title change despite the 200ms idle repaint cadence.
   *
   * There is no worker-tab case to skip here. A Conductor worker's tab is
   * `sessionBacked` and both mount sites below render it through
   * `WorkerSessionCell`, never `TerminalInstance` — and `onTitleChange` is
   * passed to `TerminalInstance` alone. A worker emits no OSC 0/2 at all
   * (it is an in-process stream-json session, not a pty), so the `Worker N`
   * pin is enforced by the mount split, not by a test in this callback. The
   * `taskRunId` test that used to stand here was kept after its subject was
   * deleted; it could no longer fire, and the only tab it could have fired
   * for would have been a PTY tab that is not a worker.
   */
  const onTitleChange = useCallback(
    (tabId: string, title: string) => {
      const trimmed = title.trim();
      if (!trimmed) return;
      renameTab(tabId, trimmed);
      // Phase 2: bi-directional title sync. The local React rename above
      // updates UI immediately; fire-and-forget the backend write so other
      // observers (`GET /terminals`, multi-window setups) see the same
      // value. Failures are non-fatal — at worst the backend keeps the
      // spawn-time title; we still rendered the new one locally.
      invoke("terminal_set_title", { terminalId: tabId, title: trimmed }).catch((e) => {
        console.warn(`[ZoneGrid] terminal_set_title failed for ${tabId}:`, e);
      });
    },
    [renameTab],
  );
  const { labelsAndTags, incrementMetric: _incrementMetric } = useZoneMetadata();
  const zoneLabels = labelsAndTags.zoneLabels;
  const onSetZoneLabel = labelsAndTags.setZoneLabel;
  const pinnedZones = labelsAndTags.pinnedZones;
  const onTogglePin = labelsAndTags.togglePin;
  const labelColorMap = labelsAndTags.labelColorMap;
  const zoneTags = labelsAndTags.zoneTags;
  // The READER for `activeTagFilters`. Until now the set had three
  // occurrences in all of `src/` — its declaration, its initializer and
  // its return — and no consumer whatsoever, so `/tag`, `/tag-toggle`,
  // `/filter-tag`, `/tag-clear` and Ctrl+Shift+G all rendered a verdict
  // for a state change nothing looked at. `/tag`'s own description
  // promises it "narrows the visible zones to those matching the
  // selected tags"; this is where that becomes true.
  const activeTagFilters = labelsAndTags.activeTagFilters;
  const zoneNotes = labelsAndTags.zoneNotes;
  const onSetZoneNote = labelsAndTags.setZoneNote;
  const transitionEffects = useTransitionEffects();
  const flashingTabs = transitionEffects.flashingTabs;
  const onRestartInZone = transitionEffects.handleRestartInZone;
  const pendingRestarts = transitionEffects.pendingRestarts;
  const onCancelRestart = transitionEffects.cancelPendingRestart;
  const { shellIntegration } = session;
  const onFirstInput = shellIntegration.handleFirstInput;
  const onShellIntegration = shellIntegration.handleShellIntegration;
  const commandHistories = shellIntegration.commandHistories;
  const { state: uiState } = useUIStateCx();
  const viewMode = uiState.viewMode;
  const selectedZones = uiState.selectedZones;
  const outputSearchQuery = uiState.outputSearch || undefined;
  const swapSource = uiState.swapSource;
  const resetRatiosKey = uiState.resetRatiosKey;
  const focusMode = uiState.focusMode;
  // eslint-disable-next-line react-hooks/exhaustive-deps
  const pk = (key: string) => pageKey(pageId, key);
  const [gridState, dispatch] = useReducer(gridReducer, layout, createInitialGridState);

  // ── Prompts panel: which sessions are showing the operator's own prompts ──
  //
  // Keyed by TAB id, not zone index: the panel describes a conversation, so it
  // has to follow its session when the tab is dragged to another zone or the
  // layout changes underneath it (unlike `pinnedZones`, which is a property of
  // the zone and gets migrated on layout change). Persisted per page so a
  // reopened window comes back with the same panels showing.
  //
  // The review panel (`SessionReviewPanel`) shares the same overlay slot, so
  // both open-sets live in one state and `toggleZonePanel` keeps them
  // mutually exclusive per tab.
  const [panelTabs, setPanelTabs] = useState<PanelTabs>(() => loadPanelTabs(pageId));
  const promptTabs = panelTabs.prompts;
  const reviewTabs = panelTabs.review;
  // Which page the state in `promptTabs` actually belongs to.
  //
  // ZoneGrid does NOT remount on a page switch (App.tsx dropped the
  // `key={activePageId}` remount), so `pageId` changes underneath live state:
  // the lazy initializer above runs once and never re-reads. Without this
  // guard the persist effect would write page A's Set into page B's key and
  // destroy B's saved panels — silently, since switching back to A looks fine.
  const promptTabsPageRef = useRef(pageId);
  useEffect(() => {
    if (promptTabsPageRef.current !== pageId) {
      promptTabsPageRef.current = pageId;
      // Adopting the new page's persisted state IS the effect; the write
      // below is skipped this pass, so the old page's Set is never persisted
      // under the new page's key.
      setPanelTabs(loadPanelTabs(pageId));
      return;
    }
    instanceStorage.setJSON(promptTabsStorageKey(pageId), [...panelTabs.prompts]);
    instanceStorage.setJSON(reviewTabsStorageKey(pageId), [...panelTabs.review]);
  }, [panelTabs, pageId]);
  const togglePromptsForTab = useCallback((tabId: string) => {
    setPanelTabs((prev) => toggleZonePanel(prev, tabId, "prompts"));
  }, []);
  const toggleReviewForTab = useCallback((tabId: string) => {
    setPanelTabs((prev) => toggleZonePanel(prev, tabId, "review"));
  }, []);

  const gridRef = useRef<HTMLDivElement>(null);
  const prevLayoutIdRef = useRef(layout.id);

  // Flow-grid mode (past-9 synthesized scrolling grid): uniform tiles, no
  // resize handles, no per-layout ratio persistence — the row count follows
  // `flowGridSlotCount` as tabs open and close, which would otherwise thrash
  // the `zone-*-ratios-*` storage keyed on `layout.id`. All ratio machinery
  // below is skipped here.
  const isFlowMode = layout.id === FLOW_GRID_ID;

  // Flow-grid virtualization (Phase 3): observe each zone cell against the
  // scrolling grid container (`gridRef`) and expose the set of tab-ids near the
  // viewport. `registerCell`/`cellFor` also form the reusable zone-cell DOM
  // registry Phase 4 (scroll routing) consumes. No-op outside flow mode.
  const { nearViewport, registerCell, unregisterCell, cellFor } = useZoneVirtualization(
    gridRef,
    isFlowMode,
  );
  const setFocusedZone = zoneLayout.setFocusedZone;

  // ── Phase 4: scroll-aware attention routing (flow mode only) ─────────────
  //
  // Every attention path — keyboard nav (`focusNextZone`/`focusPrevZone`), the
  // needs-input jump (`focusNextNeedsInput`, which also un-maximizes), the
  // error jump + stuck-lock jump (StatusStrip pills), and a freshly docked
  // session (effect below) — funnels through the single `focusedZone` state,
  // so this one effect keyed on the focused TAB covers them all. When the
  // focused tab changes in flow mode, scroll its (Phase-3-registered) cell into
  // view. Preset layouts never scroll (all zones already visible), so it no-ops
  // outside flow mode. `scrollIntoView({block:"nearest"})` minimizes movement,
  // so re-focusing an already-visible cell doesn't fight the operator's manual
  // scroll — and because we only scroll when `focusedTabId` actually CHANGES,
  // plain scrolling (which leaves `focusedZone` untouched) never triggers one.
  const focusedTabId = focusedTabIdFor(assignments, focusedZone);
  const prevFocusedTabIdRef = useRef<string | null>(null);
  const didSeedFocusScrollRef = useRef(false);
  useEffect(() => {
    if (!isFlowMode) {
      // Leaving flow mode: forget the baseline so re-entering doesn't scroll
      // from a stale focused tab.
      prevFocusedTabIdRef.current = focusedTabId;
      didSeedFocusScrollRef.current = false;
      return;
    }
    if (!didSeedFocusScrollRef.current) {
      // First flow-mode pass — seed the baseline WITHOUT scrolling so the
      // persisted focused zone / the operator's restored scroll position on
      // mount is left untouched.
      didSeedFocusScrollRef.current = true;
      prevFocusedTabIdRef.current = focusedTabId;
      return;
    }
    if (focusedTabId === prevFocusedTabIdRef.current) return;
    prevFocusedTabIdRef.current = focusedTabId;
    if (!focusedTabId) return;
    // `cellFor` may return a virtual cell; scrolling it flips it to
    // `assigned-live` via the observer + overscan (the instance cold-mounts
    // async — we intentionally don't touch the instance here).
    scrollCellIntoView(cellFor(focusedTabId));
  }, [isFlowMode, focusedTabId, cellFor]);

  // ── Explicit reveal (Session Manager card click) ─────────────────────────
  //
  // The effect above only scrolls when the focused tab CHANGES, so re-clicking
  // the session that is already focused but scrolled away would do nothing.
  // A reveal request always scrolls (flow mode) and hands the terminal the
  // keyboard, so the operator can type into the session they just picked.
  // Runs after the commit that applied the request's zone assignment + focus.
  // A flow cell that was virtual has no mounted instance yet, so its focus is
  // a no-op until the scroll mounts it; the zone is still focused and visible.
  //
  // Each request is handled once and then consumed (an emptied `tabId`), so
  // switching back to this page, or remounting the grid, never replays it.
  // The focus frame is deliberately not cancelled on cleanup: consuming the
  // request re-runs this effect immediately, which would cancel it.
  const revealRequest = zoneLayout.revealRequest;
  const consumeReveal = zoneLayout.consumeReveal;
  useEffect(() => {
    if (!revealRequest?.tabId) return;
    const { tabId, seq } = revealRequest;
    if (isFlowMode) scrollCellIntoView(cellFor(tabId));
    requestAnimationFrame(() => {
      terminalRefs.get(tabId)?.current?.focus();
    });
    consumeReveal(seq);
    // Keyed on the request object alone: each click mints a new one, and a
    // layout change must not re-handle a click.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [revealRequest]);

  // ── Phase 4: focus + scroll a newly docked session (flow mode only) ──────
  //
  // A new session (gate-continuation dock / new-session dock via the
  // `terminal-created` ingest, or an operator-created tab) lands in the LAST
  // flow row — the furthest-scrolled spot the operator can't see. Its zone is
  // assigned ASYNCHRONOUSLY by the creation-order auto-fill / auto-grow
  // reconcile in `useZoneLayout` (a couple of effect passes after ingest), so
  // we watch `tabs` for a genuinely-new id and, once it holds a zone, route it
  // through `focusedZone` — the SAME mechanism the scroll effect above watches,
  // so there's exactly one scroll path. Seeding the baseline on the first pass
  // (and on flow-mode entry) means a restore burst / initial mount never
  // hijacks the operator's focus, and preset mode is untouched entirely.
  const prevTabIdsRef = useRef<Set<string> | null>(null);
  const pendingFocusTabIdRef = useRef<string | null>(null);
  useEffect(() => {
    const currentIds = tabs.map((t) => t.id);
    if (!isFlowMode) {
      // Keep the baseline current in preset mode so switching INTO flow mode
      // doesn't treat every already-open tab as newly added.
      prevTabIdsRef.current = new Set(currentIds);
      pendingFocusTabIdRef.current = null;
      return;
    }
    const prev = prevTabIdsRef.current;
    prevTabIdsRef.current = new Set(currentIds);
    if (prev === null) {
      // First flow-mode pass — seed without focusing (restore/initial burst).
      return;
    }
    const added = newestAddedTabId(prev, currentIds);
    if (added) pendingFocusTabIdRef.current = added;
    const pending = pendingFocusTabIdRef.current;
    if (!pending) return;
    const zone = zoneForTab(assignments, pending);
    // Not placed yet — keep it pending; the reconcile that assigns the zone
    // re-runs this effect (assignments is a dep) and we route focus then.
    if (zone === null) return;
    pendingFocusTabIdRef.current = null;
    if (zone !== focusedZone) setFocusedZone(zone);
  }, [isFlowMode, tabs, assignments, focusedZone, setFocusedZone]);

  useEffect(() => {
    if (isFlowMode) return;
    instanceStorage.setJSON(pk(`zone-col-ratios-${layout.id}`), gridState.colRatios);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [gridState.colRatios, layout.id]);
  useEffect(() => {
    if (isFlowMode) return;
    instanceStorage.setJSON(pk(`zone-row-ratios-${layout.id}`), gridState.rowRatios);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [gridState.rowRatios, layout.id]);

  // Load namespaced ratios on mount for non-default pages
  // (createInitialGridState uses un-namespaced keys for backward compat)
  const didMountLoadRef = useRef(false);
  useEffect(() => {
    if (didMountLoadRef.current || pageId === "default") return;
    // Inline the flow check (vs the `isFlowMode` const) so exhaustive-deps only
    // tracks `layout.id`, already a dep — flow mode has no ratios to load.
    if (layout.id === FLOW_GRID_ID) return;
    didMountLoadRef.current = true;
    const parsedCols = instanceStorage.getJSON<number[]>(pk(`zone-col-ratios-${layout.id}`), []);
    dispatch({
      type: "SET_COL_RATIOS",
      ratios: parsedCols.length === layout.columns ? parsedCols : Array(layout.columns).fill(1),
    });
    const parsedRows = instanceStorage.getJSON<number[]>(pk(`zone-row-ratios-${layout.id}`), []);
    dispatch({
      type: "SET_ROW_RATIOS",
      ratios: parsedRows.length === layout.rows ? parsedRows : Array(layout.rows).fill(1),
    });
  }, [pageId, layout.id, layout.columns, layout.rows, pk]);

  useEffect(() => {
    if (prevLayoutIdRef.current !== layout.id) {
      prevLayoutIdRef.current = layout.id;
      // Flow mode uses uniform tiles — skip ratio reads/resets entirely (the
      // deps below re-fire on every row-count change as tabs open/close).
      if (isFlowMode) return;
      const parsedCols = instanceStorage.getJSON<number[]>(pk(`zone-col-ratios-${layout.id}`), []);
      dispatch({
        type: "SET_COL_RATIOS",
        ratios: parsedCols.length === layout.columns ? parsedCols : Array(layout.columns).fill(1),
      });
      const parsedRows = instanceStorage.getJSON<number[]>(pk(`zone-row-ratios-${layout.id}`), []);
      dispatch({
        type: "SET_ROW_RATIOS",
        ratios: parsedRows.length === layout.rows ? parsedRows : Array(layout.rows).fill(1),
      });
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [layout.id, layout.columns, layout.rows]);

  const prevResetKeyRef = useRef(resetRatiosKey);
  useEffect(() => {
    if (resetRatiosKey !== undefined && resetRatiosKey !== prevResetKeyRef.current) {
      prevResetKeyRef.current = resetRatiosKey;
      if (isFlowMode) return;
      dispatch({ type: "SET_COL_RATIOS", ratios: Array(layout.columns).fill(1) });
      dispatch({ type: "SET_ROW_RATIOS", ratios: Array(layout.rows).fill(1) });
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [resetRatiosKey, layout.columns, layout.rows]);

  useEffect(() => {
    const interval = setInterval(() => dispatch({ type: "TICK_UPTIME" }), 30000);
    return () => clearInterval(interval);
  }, []);

  // `zoneLayout.isMultiZone` is the ONE multi-zone definition
  // (`useZoneLayout`); this file used to carry an inline copy of it.
  const showLabels = zoneLayout.isMultiZone;
  const autoCompact = false;
  const forceCompact = viewMode === "compact";

  const isSingleView = layout.id === "single" || maximizedZone !== null;
  const singleViewZone = maximizedZone ?? 0;

  // Maximized-view navigation: step to the previous/next OCCUPIED zone and stay
  // maximized. `findNextZone` skips empty zones and wraps; zone indices are
  // stable across a close (`reconcileAssignments` preserves in-range zones), so
  // the target index computed before the close is still right after it.
  const maximizedNeighbour = useCallback(
    (direction: 1 | -1): number | null => {
      const current = assignments[singleViewZone];
      const next = findNextZone(
        layout.zones.length,
        singleViewZone,
        assignments,
        (id) => id !== current,
        direction,
      );
      return next === singleViewZone ? null : next;
    },
    [assignments, layout.zones.length, singleViewZone],
  );

  const goToMaximizedZone = useCallback(
    (zone: number) => {
      zoneLayout.setMaximizedZone(zone);
      zoneLayout.setFocusedZone(zone);
    },
    [zoneLayout],
  );

  const deleteAndForward = useCallback(() => {
    const tabId = assignments[singleViewZone];
    if (!tabId) return;
    const next = maximizedNeighbour(1);
    closeTerminal(tabId);
    // With nowhere to go, leave `maximizedZone` alone: `useZoneLayout` returns
    // to the grid when the maximized tab was closed.
    if (next !== null) goToMaximizedZone(next);
  }, [assignments, singleViewZone, maximizedNeighbour, closeTerminal, goToMaximizedZone]);

  const handleZoneMouseDown = useCallback(
    (zoneIndex: number, e: React.MouseEvent) => {
      onZoneClick(zoneIndex, e.ctrlKey || e.metaKey);
    },
    [onZoneClick],
  );

  // Stable grid-state handlers. These were five inline lambdas at the ZoneCell
  // call site, so every ZoneCell got five fresh props on every ZoneGrid render
  // and could never be memoized. `dispatch` from useReducer is stable, so
  // these identities never change; the per-zone ones take the zone index as an
  // argument instead of closing over it.
  const setDropTarget = useCallback(
    (zone: number | null) => dispatch({ type: "SET_DROP_TARGET", zone }),
    [],
  );
  const setContextMenu = useCallback(
    (menu: { x: number; y: number; zoneIndex: number } | null) =>
      dispatch({ type: "SET_CONTEXT_MENU", menu }),
    [],
  );
  const toggleFilterInput = useCallback(
    (zoneIndex: number) => dispatch({ type: "TOGGLE_FILTER_INPUT", zoneIndex }),
    [],
  );
  const setZoneFilter = useCallback(
    (zoneIndex: number, value: string) => dispatch({ type: "SET_ZONE_FILTER", zoneIndex, value }),
    [],
  );
  const clearZoneFilter = useCallback(
    (zoneIndex: number) => dispatch({ type: "CLEAR_ZONE_FILTER", zoneIndex }),
    [],
  );

  // Layer 1 of plans/terminal-grid-bootstrap-redesign.md — exclusive
  // classification per render. Without this, a tab can briefly satisfy
  // BOTH the unassigned (HiddenTerminal) and assigned (inline visible)
  // mount paths during a single React render pass — two TerminalInstance
  // subtrees mount, both register the same terminal-input-<id> with the
  // UI Bridge and listen on the same terminal-output Tauri event, and
  // their lifecycle effects evict each other. Centralizing classification
  // in one Map keyed off `assignments` guarantees that for any tab.id,
  // exactly one of {visible inline, hidden} renders per render pass.
  //
  // Phase 3 (flow-grid virtualization) extends this to a THREE-way result in
  // flow mode: `assigned-live` (near viewport → mount a real TerminalInstance),
  // `assigned-virtual` (far offscreen → CompactZoneCard, zero instances), and
  // `hidden`. The exactly-one-owner invariant relaxes to exactly-one-OR-zero (a
  // virtual zone owns zero mounts) while the dual-mount race stays impossible
  // (each tab.id maps to exactly one classification). Preset (non-flow) layouts
  // keep the two-way `assigned`/`hidden` result unchanged. The decision itself
  // lives in the pure `classifyTabs` so the invariant is unit-testable without
  // an IntersectionObserver.
  //
  // tmux work mode adds its own arm: the shown tab is `assigned`, every other
  // PTY tab `offscreen` (parked), and a parked plan or worker tab
  // `offscreen-unmounted`. Nothing is `hidden` in that mode.
  //
  // `buildZoneCells` turns the classification into mounts for both modes: one
  // cell per mounted tab, keyed by tab id, so a grid ⇄ tmux switch keeps every
  // terminal mounted (plan `2026-10-11-runner-terminal-tmux-work-mode` D2). A
  // `hidden` plan or Conductor worker tab (`sessionBacked`) takes no parked
  // mount — it has no PTY, and a parked `TerminalInstance` would attach to a
  // terminal id that names no process.
  const workMode = zoneLayout.workMode;
  const isTmux = workMode === "tmux";
  const tabClassification = useMemo(
    () => classifyTabs(tabs, assignments, isFlowMode, nearViewport, workMode),
    [tabs, assignments, isFlowMode, nearViewport, workMode],
  );

  // The maximized view renders its own header (below, after the early
  // return), so its review read is hooked here, unconditionally.
  const maximizedTab =
    isSingleView && layout.id !== "single"
      ? tabs.find((t) => t.id === assignments[singleViewZone])
      : undefined;
  const maximizedReviewAvailable =
    reviewPanelAvailable(maximizedTab, !!maximizedTab?.claudeSessionId) &&
    tabClassification.get(maximizedTab?.id ?? "") !== "hidden";
  const maximizedReview = useSessionReview(
    maximizedReviewAvailable ? (maximizedTab?.claudeSessionId ?? null) : null,
    { visible: maximizedReviewAvailable },
  );
  // Memoised on the two reads: the badge walks every hunk of every file.
  const maximizedBadge = useMemo(
    () => reviewBadgeState(maximizedReview.changes, maximizedReview.review),
    [maximizedReview.changes, maximizedReview.review],
  );

  // The props every zone cell of this render shares, built once for both work
  // modes (`buildZoneCells` adds tmux's viewport overrides), so the tmux pane
  // is the grid's zone content and not a second copy of it. Every value is a
  // stable identity, so the spread keeps `ZoneCell`'s memo holding.
  const cellProps: ZoneCellSharedProps = {
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
    onZoneClick,
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
    showLabels,
    isMultiZone: zoneLayout.isMultiZone,
    autoCompact,
    forceCompact,
    showFilterInput: gridState.showFilterInput,
    zoneFilters: gridState.zoneFilters,
    handleZoneMouseDown,
    onSetDropTarget: setDropTarget,
    onSetContextMenu: setContextMenu,
    onToggleFilterInput: toggleFilterInput,
    onSetZoneFilter: setZoneFilter,
    onClearZoneFilter: clearZoneFilter,
    onTogglePromptsForTab: togglePromptsForTab,
    onToggleReviewForTab: toggleReviewForTab,
    isSingleView,
  };

  // The zone context menu, shared by the grid and the tmux viewport (a
  // right-click on the shown pane opens it there too; left unrendered, the
  // request would sit in grid state and pop up on the next exit to grid).
  const menu = gridState.contextMenu;
  const contextMenu = menu
    ? (() => {
        const cmTabId = assignments[menu.zoneIndex];
        const cmTab = tabs.find((t) => t.id === cmTabId);
        const cmState = cmTab ? (sessionStates[cmTab.id] ?? "idle") : "idle";
        const others = layout.zones
          .map((_, idx) => {
            if (idx === menu.zoneIndex) return null;
            const otherTabId = assignments[idx];
            const otherTab = tabs.find((t) => t.id === otherTabId);
            return {
              index: idx,
              title: otherTab ? resolveTitle(otherTab) : `Zone ${idx + 1}`,
            };
          })
          .filter((z): z is { index: number; title: string } => z !== null);

        return (
          <ZoneContextMenu
            x={menu.x}
            y={menu.y}
            zoneIndex={menu.zoneIndex}
            tab={cmTab}
            state={cmState}
            otherZones={others}
            onClose={() => dispatch({ type: "SET_CONTEXT_MENU", menu: null })}
            onFocus={() => onZoneClick(menu.zoneIndex)}
            onMaximize={() => onZoneDoubleClick(menu.zoneIndex)}
            onApprove={() => {
              // Route through writeToTerminalById so approving from a
              // virtualized (unmounted) zone falls back to `terminal_write`
              // instead of silently no-op'ing on a missing instance ref.
              if (cmTab) writeToTerminalById(terminalRefs, cmTab.id, "y\r");
            }}
            onReject={() => {
              if (cmTab) writeToTerminalById(terminalRefs, cmTab.id, "n\r");
            }}
            onSwap={(targetZone) => {
              if (cmTab && onAssignTab) {
                onAssignTab(targetZone, cmTab.id);
              }
            }}
            onUnassign={() => {
              if (onAssignTab) {
                const unassigned = tabs.find((t) => !Object.values(assignments).includes(t.id));
                if (unassigned) {
                  onAssignTab(menu.zoneIndex, unassigned.id);
                }
              }
            }}
            onRestart={onRestartInZone ? () => onRestartInZone(menu.zoneIndex) : undefined}
          />
        );
      })()
    : null;

  // `maximizedZone` is always null in tmux mode, so this branch is grid-only.
  if (isSingleView && layout.id !== "single") {
    const tab = maximizedTab;
    // A maximized zone owns the page, so its prompts go in the full-height
    // right-hand column rather than the short strip a tiled zone gets.
    const maximizedPromptsOpen = !!tab?.claudeSessionId && promptTabs.has(tab.id);
    const prevZone = maximizedNeighbour(-1);
    const nextZone = maximizedNeighbour(1);
    const maximizedReviewOpen = maximizedReviewAvailable && !!tab && reviewTabs.has(tab.id);
    const maximizedPanelWidth = maximizedReviewOpen
      ? REVIEW_PANEL_GEOMETRY.rightWidthPx
      : maximizedPromptsOpen
        ? PROMPTS_PANEL_GEOMETRY.rightWidthPx
        : undefined;

    return (
      <div
        className="h-full w-full relative"
        onDoubleClickCapture={() => onZoneDoubleClick(singleViewZone)}
      >
        {tab && (
          <div
            className="absolute top-0 left-0 right-0 flex items-center gap-2 px-3 bg-[#13141f]/90 backdrop-blur-sm z-20 cursor-pointer"
            style={{ height: `${MAXIMIZED_HEADER_HEIGHT_PX}px` }}
          >
            <span className="text-[10px] text-[#7aa2f7] font-medium">Maximized</span>
            <span className="text-[10px] text-[#a9b1d6]">
              <TabTitle tab={tab} />
            </span>
            <SessionInfoDropdown claudeSessionId={tab.claudeSessionId} zoneIndex={singleViewZone} />
            {tab.claudeSessionId && (
              <button
                onClick={(e) => {
                  e.stopPropagation();
                  togglePromptsForTab(tab.id);
                }}
                onMouseDown={(e) => e.stopPropagation()}
                onDoubleClick={(e) => e.stopPropagation()}
                className={`p-0.5 rounded transition-colors shrink-0 ${
                  maximizedPromptsOpen
                    ? "text-[#9ece6a] bg-[#9ece6a]/15"
                    : "text-[#565f89] hover:text-[#a9b1d6] hover:bg-[#2a2d3d]/50"
                }`}
                title={maximizedPromptsOpen ? "Hide my prompts" : "Show my prompts"}
                aria-pressed={maximizedPromptsOpen}
              >
                <MessageSquare className="w-3 h-3" />
              </button>
            )}
            {maximizedReviewAvailable && (
              <SessionReviewToggle
                open={maximizedReviewOpen}
                onToggle={() => toggleReviewForTab(tab.id)}
                badge={maximizedBadge}
                iconClassName="w-3 h-3"
              />
            )}
            <div className="flex items-center gap-0.5 ml-auto">
              <button
                onClick={(e) => {
                  e.stopPropagation();
                  if (prevZone !== null) goToMaximizedZone(prevZone);
                }}
                onMouseDown={(e) => e.stopPropagation()}
                onDoubleClick={(e) => e.stopPropagation()}
                disabled={prevZone === null}
                className="p-0.5 rounded shrink-0 text-[#565f89] hover:text-[#a9b1d6] hover:bg-[#2a2d3d]/50 disabled:opacity-30 disabled:pointer-events-none "
                title="Previous terminal"
                aria-label="Previous terminal"
              >
                <ChevronLeft className="w-3 h-3" />
              </button>
              <button
                onClick={(e) => {
                  e.stopPropagation();
                  if (nextZone !== null) goToMaximizedZone(nextZone);
                }}
                onMouseDown={(e) => e.stopPropagation()}
                onDoubleClick={(e) => e.stopPropagation()}
                disabled={nextZone === null}
                className="p-0.5 rounded shrink-0 text-[#565f89] hover:text-[#a9b1d6] hover:bg-[#2a2d3d]/50 disabled:opacity-30 disabled:pointer-events-none "
                title="Next terminal"
                aria-label="Next terminal"
              >
                <ChevronRight className="w-3 h-3" />
              </button>
              <button
                onClick={(e) => {
                  e.stopPropagation();
                  deleteAndForward();
                }}
                onMouseDown={(e) => e.stopPropagation()}
                onDoubleClick={(e) => e.stopPropagation()}
                className="p-0.5 rounded shrink-0 text-[#565f89] hover:text-[#a9b1d6] hover:bg-[#2a2d3d]/50 disabled:opacity-30 disabled:pointer-events-none hover:!text-[#f7768e]"
                title="Delete this terminal and go to the next"
                aria-label="Delete and forward"
              >
                <Trash2 className="w-3 h-3" />
              </button>
            </div>
            <span className="text-[9px] text-[#565f89]">Esc or double-click to restore</span>
          </div>
        )}

        {tab?.claudeSessionId &&
          maximizedPromptsOpen &&
          tabClassification.get(tab.id) !== "hidden" && (
            <ZonePromptsPanel
              claudeSessionId={tab.claudeSessionId}
              configDir={tab.claudeConfigDir}
              projectPath={tab.workingDir}
              orientation="right"
              topOffsetPx={MAXIMIZED_HEADER_HEIGHT_PX}
              onClose={() => togglePromptsForTab(tab.id)}
            />
          )}

        {tab && maximizedReviewOpen && (
          <SessionReviewPanel
            handle={maximizedReview}
            terminalId={tab.id}
            orientation="right"
            topOffsetPx={MAXIMIZED_HEADER_HEIGHT_PX}
            onClose={() => toggleReviewForTab(tab.id)}
          />
        )}

        {/* eslint-disable-next-line react-hooks/refs -- terminalRefs is a stable Map-cache (see above). */}
        {layout.zones.map((_, zoneIdx) => {
          const zoneTabId = assignments[zoneIdx];
          const zoneTab = tabs.find((t) => t.id === zoneTabId);
          if (!zoneTab) return null;
          // Layer 1 invariant: only render the visible inline TerminalInstance
          // when the centralized classification agrees this tab is NOT hidden
          // (i.e. `assigned` in preset mode, or `assigned-live`/`assigned-virtual`
          // in flow mode — the maximized/single view is a full-screen single
          // terminal, not the scrolling grid, so it mounts every assigned zone
          // as before rather than virtualizing). If classification still says
          // "hidden" (e.g. assignments updated mid render before the memo
          // recomputed), defer the visible mount so the hidden mount stays the
          // sole owner this render pass.
          if (tabClassification.get(zoneTab.id) === "hidden") return null;

          const isVisible = zoneIdx === singleViewZone;
          const ref = terminalRefs.get(zoneTab.id);

          return (
            <div
              key={zoneTab.id}
              className={isVisible ? "h-full w-full" : "hidden"}
              style={
                isVisible
                  ? {
                      // The header is an absolute overlay here too, so without
                      // this it covers the terminal's first line — it always
                      // did; naming its height is what made that visible.
                      paddingTop: `${MAXIMIZED_HEADER_HEIGHT_PX}px`,
                      paddingRight:
                        maximizedPanelWidth !== undefined ? `${maximizedPanelWidth}px` : undefined,
                    }
                  : undefined
              }
              onMouseDown={(e) => handleZoneMouseDown(zoneIdx, e)}
            >
              {zoneTab.type === "plan" && zoneTab.planFilePath ? (
                <PlanViewer filePath={zoneTab.planFilePath} visible={isVisible} />
              ) : zoneTab.sessionBacked && zoneTab.taskRunId ? (
                <WorkerSessionCell
                  tab={zoneTab}
                  taskRunId={zoneTab.taskRunId}
                  visible={isVisible}
                />
              ) : (
                <TerminalInstance
                  ref={ref}
                  terminalId={zoneTab.id}
                  pageId={pageId}
                  visible={isVisible}
                  isReconnecting={zoneTab.isReconnecting}
                  onReconnected={() => onReconnected(zoneTab.id)}
                  onExit={(code) => onExit(zoneTab.id, code)}
                  onFirstInput={(input) => onFirstInput(zoneTab.id, input)}
                  onUserInputLine={
                    onUserInputLine ? (input) => onUserInputLine(zoneTab.id, input) : undefined
                  }
                  onShellIntegration={(event) => onShellIntegration(zoneTab.id, event)}
                  onTitleChange={(title) => onTitleChange(zoneTab.id, title)}
                />
              )}
            </div>
          );
        })}

        {/* The tabs with no zone keep their parked mount, as in the grid. */}
        {/* eslint-disable-next-line react-hooks/refs -- cellProps carries the stable terminalRefs Map-cache (see above). */}
        {buildParkedCells(layout.zones[0], cellProps)}
      </div>
    );
  }

  // Grid and tmux render through ONE root element with ONE keyed cell list as
  // its first child, so switching modes re-renders every tab's cell in place —
  // no terminal unmounts (see `buildZoneCells`). tmux mode is the `single`
  // layout's one zone filling the page, with no resize handles.
  // eslint-disable-next-line react-hooks/refs -- cellProps carries the stable terminalRefs Map-cache (see above).
  const cells = buildZoneCells({
    workMode,
    zones: layout.zones,
    // eslint-disable-next-line react-hooks/refs -- cellProps carries the stable terminalRefs Map-cache (see above).
    cellProps,
    shownTabId: zoneLayout.shownTabId,
    promptTabs,
    reviewTabs,
    dropTargetZone: gridState.dropTargetZone,
  });

  return (
    <div
      ref={gridRef}
      className="h-full w-full relative"
      data-terminal-work-mode={workMode}
      style={
        isTmux
          ? { display: "grid", gridTemplateColumns: "1fr", gridTemplateRows: "1fr" }
          : isFlowMode
            ? {
                // Flow-grid: fixed 3 equal columns, rows at least MIN_TILE_HEIGHT_PX
                // tall (expanding to fill when few) and the container scrolls once
                // rows overflow. No ratio machinery / resize handles — uniform tiles.
                display: "grid",
                gridTemplateColumns: `repeat(${FLOW_COLS}, 1fr)`,
                gridTemplateRows: `repeat(${layout.rows}, minmax(${MIN_TILE_HEIGHT_PX}px, 1fr))`,
                gap: "2px",
                overflowY: "auto",
              }
            : {
                display: "grid",
                gridTemplateColumns: gridState.colRatios.map((r) => `${r}fr`).join(" "),
                gridTemplateRows: gridState.rowRatios.map((r) => `${r}fr`).join(" "),
                gap: "2px",
              }
      }
      onDragEnd={() => dispatch({ type: "SET_DROP_TARGET", zone: null })}
    >
      {cells}

      {!isTmux &&
        !isFlowMode &&
        layout.columns > 1 &&
        Array.from({ length: layout.columns - 1 }, (_, i) => {
          const leftFr = gridState.colRatios.slice(0, i + 1).reduce((a, b) => a + b, 0);
          const totalFr = gridState.colRatios.reduce((a, b) => a + b, 0);
          const pct = (leftFr / totalFr) * 100;
          return (
            <div
              key={`col-handle-${i}`}
              className="absolute top-0 bottom-0 z-20 cursor-col-resize group"
              style={{ left: `${pct}%`, width: "8px", marginLeft: "-4px" }}
              onDoubleClick={(e) => {
                e.preventDefault();
                dispatch({
                  type: "SET_COL_RATIOS",
                  ratios: Array(layout.columns).fill(1),
                });
              }}
              onMouseDown={(e) => {
                e.preventDefault();
                const startX = e.clientX;
                const gridWidth = gridRef.current?.getBoundingClientRect().width ?? 1;
                const startRatios = [...gridState.colRatios];
                const onMove = (ev: MouseEvent) => {
                  const dx = ev.clientX - startX;
                  const dFr = (dx / gridWidth) * totalFr;
                  const newLeft = Math.max(0.15, startRatios[i] + dFr);
                  const newRight = Math.max(0.15, startRatios[i + 1] - dFr);
                  const next = [...startRatios];
                  next[i] = newLeft;
                  next[i + 1] = newRight;
                  dispatch({ type: "SET_COL_RATIOS", ratios: next });
                };
                const onUp = () => {
                  document.removeEventListener("mousemove", onMove);
                  document.removeEventListener("mouseup", onUp);
                };
                document.addEventListener("mousemove", onMove);
                document.addEventListener("mouseup", onUp);
              }}
            >
              <div className="w-px h-full mx-auto bg-[#2a2d3d] group-hover:bg-[#7aa2f7] transition-colors" />
            </div>
          );
        })}

      {!isTmux &&
        !isFlowMode &&
        layout.rows > 1 &&
        Array.from({ length: layout.rows - 1 }, (_, i) => {
          const topFr = gridState.rowRatios.slice(0, i + 1).reduce((a, b) => a + b, 0);
          const totalFr = gridState.rowRatios.reduce((a, b) => a + b, 0);
          const pct = (topFr / totalFr) * 100;
          return (
            <div
              key={`row-handle-${i}`}
              className="absolute left-0 right-0 z-20 cursor-row-resize group"
              style={{ top: `${pct}%`, height: "8px", marginTop: "-4px" }}
              onDoubleClick={(e) => {
                e.preventDefault();
                dispatch({
                  type: "SET_ROW_RATIOS",
                  ratios: Array(layout.rows).fill(1),
                });
              }}
              onMouseDown={(e) => {
                e.preventDefault();
                const startY = e.clientY;
                const gridHeight = gridRef.current?.getBoundingClientRect().height ?? 1;
                const startRatios = [...gridState.rowRatios];
                const onMove = (ev: MouseEvent) => {
                  const dy = ev.clientY - startY;
                  const dFr = (dy / gridHeight) * totalFr;
                  const newTop = Math.max(0.15, startRatios[i] + dFr);
                  const newBottom = Math.max(0.15, startRatios[i + 1] - dFr);
                  const next = [...startRatios];
                  next[i] = newTop;
                  next[i + 1] = newBottom;
                  dispatch({ type: "SET_ROW_RATIOS", ratios: next });
                };
                const onUp = () => {
                  document.removeEventListener("mousemove", onMove);
                  document.removeEventListener("mouseup", onUp);
                };
                document.addEventListener("mousemove", onMove);
                document.addEventListener("mouseup", onUp);
              }}
            >
              <div className="h-px w-full my-auto bg-[#2a2d3d] group-hover:bg-[#7aa2f7] transition-colors" />
            </div>
          );
        })}

      {contextMenu}
    </div>
  );
}

export const ZoneGrid = memo(ZoneGridInner);

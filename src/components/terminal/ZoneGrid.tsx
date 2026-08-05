import {
  memo,
  useCallback,
  useMemo,
  useReducer,
  useRef,
  useEffect,
  type RefObject,
} from "react";
import { invoke } from "@tauri-apps/api/core";
import type { TerminalTab } from "./useTerminalManager";
import type { SessionState, ZoneAssignments } from "./useZoneLayout";
import { instanceStorage } from "@/lib/instance-storage";
import { pageKey } from "./TerminalPageContext";
import { Terminal, Filter, RefreshCw } from "lucide-react";
import {
  TerminalInstance,
  type TerminalInstanceHandle,
  type ShellIntegrationEvent,
} from "./TerminalInstance";
import { PlanViewer } from "./PlanViewer";
import { SuggestionChip } from "./suggestions";
import { ZoneHoverActions } from "./ZoneHoverActions";
import type { LayoutPreset } from "./useZoneLayout";
import { FLOW_GRID_ID, FLOW_COLS, MIN_TILE_HEIGHT_PX } from "./useZoneLayout";
import {
  STATE_BORDER_COLORS,
  STATE_COLORS,
  STATE_GLOW,
  CompactZoneCard,
  ZoneContextMenu,
  ZoneQuickActions,
  ZoneLabel,
  SessionPrDropdown,
  HiddenTerminal,
  formatUptime,
  countMatches,
} from "./zone-grid";
import {
  useTerminalSession,
  useZoneMetadata,
  useTransitionEffects,
  useUIStateCx,
} from "./contexts";
import { classifyTabs, type TabClassification } from "./classifyTabs";
import { useZoneVirtualization } from "./useZoneVirtualization";
import {
  scrollCellIntoView,
  focusedTabIdFor,
  newestAddedTabId,
  zoneForTab,
} from "./flowScrollRouting";
import { writeToTerminalById } from "./writeToTerminalById";
import { useTabHotSlice } from "./useTerminalHotStore";

export type ViewMode = "auto" | "full" | "compact";

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
  // Read all data from contexts
  const session = useTerminalSession();
  const {
    tabs,
    zoneLayout,
    terminalRefs: terminalRefsRef,
    pageId,
    markReconnected,
    renameTab,
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
   * Worker tabs (presence of `taskRunId`) are pinned at `Worker N` per the
   * Phase 1 backend gate (`set_title_unless_worker`); skip the local rename
   * *and* the backend invoke for those so OSC 0 emissions from the embedded
   * Claude CLI don't clobber the operator-facing identifier.
   */
  // tabsRef keeps the worker-marker lookup current without re-creating
  // onTitleChange on every tab mutation (TerminalInstance refs onto the
  // latest callback, so this ref pattern also avoids re-render thrash).
  const tabsRef = useRef(tabs);
  useEffect(() => {
    tabsRef.current = tabs;
  }, [tabs]);
  const onTitleChange = useCallback(
    (tabId: string, title: string) => {
      const trimmed = title.trim();
      if (!trimmed) return;
      const tab = tabsRef.current.find((t) => t.id === tabId);
      if (tab?.taskRunId) return;
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
  const gridRef = useRef<HTMLDivElement>(null);
  const prevLayoutIdRef = useRef(layout.id);

  // Flow-grid mode (past-9 synthesized scrolling grid): uniform tiles, no
  // resize handles, no per-layout ratio persistence — the row count changes on
  // every tab open/close, which would otherwise thrash the `zone-*-ratios-*`
  // storage keyed on `layout.id`. All ratio machinery below is skipped here.
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

  const isMultiZone = layout.zones.length > 1;
  const showLabels = isMultiZone;
  const autoCompact = false;
  const forceCompact = viewMode === "compact";

  const isSingleView = layout.id === "single" || maximizedZone !== null;
  const singleViewZone = maximizedZone ?? 0;

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
    (zoneIndex: number, value: string) =>
      dispatch({ type: "SET_ZONE_FILTER", zoneIndex, value }),
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
  const tabClassification = useMemo(
    () => classifyTabs(tabs, assignments, isFlowMode, nearViewport),
    [tabs, assignments, isFlowMode, nearViewport],
  );

  const unassignedTerminals = tabs.filter(
    (t) => tabClassification.get(t.id) === "hidden" && t.type !== "plan",
  );

  const renderHiddenTabs = (extraTabs: TerminalTab[]) =>
    extraTabs.map((tab) => (
      <HiddenTerminal
        key={tab.id}
        tab={tab}
        pageId={pageId}
        terminalRef={terminalRefs.get(tab.id)}
        onExit={onExit}
        onFirstInput={onFirstInput}
        onUserInputLine={onUserInputLine}
        onShellIntegration={onShellIntegration}
        onReconnected={onReconnected}
        onTitleChange={onTitleChange}
      />
    ));

  if (isSingleView && layout.id !== "single") {
    const tabId = assignments[singleViewZone];
    const tab = tabs.find((t) => t.id === tabId);

    return (
      <div
        className="h-full w-full relative"
        onDoubleClickCapture={() => onZoneDoubleClick(singleViewZone)}
      >
        {tab && (
          <div className="absolute top-0 left-0 right-0 flex items-center gap-2 px-3 py-1 bg-[#13141f]/90 backdrop-blur-sm z-20 cursor-pointer">
            <span className="text-[10px] text-[#7aa2f7] font-medium">Maximized</span>
            <span className="text-[10px] text-[#a9b1d6]">{tab.title}</span>
            <SessionPrDropdown claudeSessionId={tab.claudeSessionId} />
            <span className="text-[9px] text-[#565f89] ml-auto">
              Esc or double-click to restore
            </span>
          </div>
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
              onMouseDown={(e) => handleZoneMouseDown(zoneIdx, e)}
            >
              {zoneTab.type === "plan" && zoneTab.planFilePath ? (
                <PlanViewer filePath={zoneTab.planFilePath} visible={isVisible} />
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

        {/* eslint-disable-next-line react-hooks/refs -- renderHiddenTabs reads the stable terminalRefs Map-cache (see above). */}
        {renderHiddenTabs(unassignedTerminals)}
      </div>
    );
  }

  return (
    <div
      ref={gridRef}
      className="h-full w-full relative"
      style={
        isFlowMode
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
      {/* eslint-disable-next-line react-hooks/refs -- ZoneCell receives the stable terminalRefs Map-cache (see above). */}
      {layout.zones.map((zone, zoneIdx) => (
        <ZoneCell
          key={`zone-${zoneIdx}`}
          zone={zone}
          zoneIdx={zoneIdx}
          tabs={tabs}
          assignments={assignments}
          tabClassification={tabClassification}
          isFlowMode={isFlowMode}
          registerCell={registerCell}
          unregisterCell={unregisterCell}
          focusedZone={focusedZone}
          pageId={pageId}
          sessionStates={sessionStates}
          terminalRefs={terminalRefs}
          onZoneClick={onZoneClick}
          onZoneDoubleClick={onZoneDoubleClick}
          onExit={onExit}
          onFirstInput={onFirstInput}
          onUserInputLine={onUserInputLine}
          onShellIntegration={onShellIntegration}
          onReconnected={onReconnected}
          onTitleChange={onTitleChange}
          onAssignTab={onAssignTab}
          flashingTabs={flashingTabs}
          selectedZones={selectedZones}
          staleTabs={staleTabs}
          pinnedZones={pinnedZones}
          onTogglePin={onTogglePin}
          outputSearchQuery={outputSearchQuery}
          swapSource={swapSource}
          zoneLabels={zoneLabels}
          onSetZoneLabel={onSetZoneLabel}
          onRestartInZone={onRestartInZone}
          labelColorMap={labelColorMap}
          zoneTags={zoneTags}
          commandHistories={commandHistories}
          focusMode={focusMode}
          zoneNotes={zoneNotes}
          onSetZoneNote={onSetZoneNote}
          onExportZone={onExportZone}
          pendingRestarts={pendingRestarts}
          onCancelRestart={onCancelRestart}
          isDropTarget={gridState.dropTargetZone === zoneIdx}
          showLabels={showLabels}
          isMultiZone={isMultiZone}
          autoCompact={autoCompact}
          forceCompact={forceCompact}
          showFilterInput={gridState.showFilterInput}
          zoneFilters={gridState.zoneFilters}
          handleZoneMouseDown={handleZoneMouseDown}
          onSetDropTarget={setDropTarget}
          onSetContextMenu={setContextMenu}
          onToggleFilterInput={toggleFilterInput}
          onSetZoneFilter={setZoneFilter}
          onClearZoneFilter={clearZoneFilter}
        />
      ))}

      {!isFlowMode &&
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

      {!isFlowMode &&
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

      {/* eslint-disable-next-line react-hooks/refs -- renderHiddenTabs reads the stable terminalRefs Map-cache (see above). */}
      {renderHiddenTabs(unassignedTerminals)}

      {gridState.contextMenu &&
        (() => {
          const cmTabId = assignments[gridState.contextMenu.zoneIndex];
          const cmTab = tabs.find((t) => t.id === cmTabId);
          const cmState = cmTab ? (sessionStates[cmTab.id] ?? "idle") : "idle";
          const others = layout.zones
            .map((_, idx) => {
              if (idx === gridState.contextMenu!.zoneIndex) return null;
              const otherTabId = assignments[idx];
              const otherTab = tabs.find((t) => t.id === otherTabId);
              return {
                index: idx,
                title: otherTab?.title ?? `Zone ${idx + 1}`,
              };
            })
            .filter((z): z is { index: number; title: string } => z !== null);

          return (
            <ZoneContextMenu
              x={gridState.contextMenu.x}
              y={gridState.contextMenu.y}
              zoneIndex={gridState.contextMenu.zoneIndex}
              tab={cmTab}
              state={cmState}
              otherZones={others}
              onClose={() => dispatch({ type: "SET_CONTEXT_MENU", menu: null })}
              onFocus={() => onZoneClick(gridState.contextMenu!.zoneIndex)}
              onMaximize={() => onZoneDoubleClick(gridState.contextMenu!.zoneIndex)}
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
                    onAssignTab(gridState.contextMenu!.zoneIndex, unassigned.id);
                  }
                }
              }}
              onRestart={
                onRestartInZone
                  ? () => onRestartInZone(gridState.contextMenu!.zoneIndex)
                  : undefined
              }
            />
          );
        })()}
    </div>
  );
}

export const ZoneGrid = memo(ZoneGridInner);

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
  commandHistories,
  focusMode,
  zoneNotes,
  onSetZoneNote,
  onExportZone,
  pendingRestarts,
  onCancelRestart,
  isDropTarget,
  showLabels,
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
}: {
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
}) {
  const tabId = assignments[zoneIdx];
  // Per-tab subscription — THIS is what stops one pane's output frame from
  // re-rendering the other 39 zones (plan §0 A1). Only the cell whose tab
  // produced output (or whose duration/sparkline/lock state moved) re-renders.
  const hot = useTabHotSlice(pageId, tabId);
  const lastLines = hot.lastOutputLines;
  const tab = tabs.find((t) => t.id === tabId);
  const classification = tab ? tabClassification.get(tab.id) : undefined;
  // A far-offscreen assigned zone in flow mode: render only the CompactZoneCard,
  // NO TerminalInstance. Plan tabs are never virtualized (PlanViewer is already
  // lightweight and carries no xterm parser).
  const isVirtual = classification === "assigned-virtual" && tab?.type !== "plan";
  // Mount the inline TerminalInstance only for a non-plan tab the classifier
  // owns as live (`assigned` in preset mode, `assigned-live` near the viewport
  // in flow mode). Virtual + hidden mount zero instances here.
  const shouldMountInstance =
    !!tab &&
    tab.type !== "plan" &&
    (classification === "assigned" || classification === "assigned-live");

  // Zero-arg wrapper for ZoneLabel's `onToggleFilter` slot; stable so the
  // label doesn't get a fresh prop on every cell render.
  const handleToggleFilterInput = useCallback(
    () => onToggleFilterInput(zoneIdx),
    [onToggleFilterInput, zoneIdx],
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
            onShellIntegration: (event: ShellIntegrationEvent) =>
              onShellIntegration(tabId, event),
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
  const cellRef = useCallback(
    (el: HTMLDivElement | null) => {
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

  const isPinned = pinnedZones?.has(zoneIdx);
  const useCompact =
    tab && tab.type !== "plan" && !isFocused && !isPinned && (autoCompact || forceCompact);
  // The card overlays the instance region whenever we render CompactZoneCard —
  // either the operator forced compact/auto-compact (instance stays mounted but
  // hidden) OR the zone is virtualized (no instance at all this render).
  const showCompactCard = useCompact || isVirtual;
  const isFlashing = tab && flashingTabs?.has(tab.id);
  const isStale = tab && staleTabs?.has(tab.id);
  const searchMatch =
    tab && outputSearchQuery
      ? lastLines.some((l) =>
          l.toLowerCase().includes(outputSearchQuery.toLowerCase()),
        )
      : false;
  const isSwapSource = swapSource === zoneIdx;
  const isSelected = selectedZones?.has(zoneIdx);

  const firstTagColor = zoneTags?.[zoneIdx]?.[0]
    ? labelColorMap?.[zoneTags[zoneIdx][0]]
    : undefined;
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
      className={`group relative overflow-hidden ${isFlashing ? "zone-flash" : ""} ${
        outputSearchQuery && !searchMatch ? "opacity-40" : ""
      }`}
      style={{
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
        opacity: focusMode && !isFocused && state !== "needs-input" && state !== "error" ? 0.3 : 1,
        animation: isFlashing ? "zone-flash-border 1s ease-out" : undefined,
        ...(firstTagColor
          ? {
              background: `linear-gradient(135deg, ${firstTagColor}08 0%, transparent 60%)`,
            }
          : {}),
      }}
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
      {(() => {
        const label = zoneLabels?.[zoneIdx];
        const groupColor = label && labelColorMap?.[label];
        return groupColor ? (
          <div
            className="absolute left-0 top-0 bottom-0 w-[3px] z-10 rounded-l"
            style={{ backgroundColor: groupColor, opacity: 0.6 }}
          />
        ) : null;
      })()}

      {pendingRestarts?.[zoneIdx] && (
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
              zoneLabel={zoneLabels?.[zoneIdx]}
              onSetZoneLabel={
                onSetZoneLabel ? (label) => onSetZoneLabel(zoneIdx, label) : undefined
              }
              onRestart={onRestartInZone ? () => onRestartInZone(zoneIdx) : undefined}
              groupColor={zoneLabels?.[zoneIdx] ? labelColorMap?.[zoneLabels[zoneIdx]] : undefined}
              uptime={formatUptime(tab.createdAt)}
              lastCommand={commandHistories?.[tab.id]?.slice(-1)[0]?.command}
              note={zoneNotes?.[zoneIdx]}
              onSetNote={onSetZoneNote ? (n) => onSetZoneNote(zoneIdx, n) : undefined}
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
              onTogglePin={onTogglePin ? () => onTogglePin(zoneIdx) : undefined}
              zoneLabel={zoneLabels?.[zoneIdx]}
              onSetZoneLabel={
                onSetZoneLabel ? (label) => onSetZoneLabel(zoneIdx, label) : undefined
              }
              onScrollToBottom={() => {
                const ref = terminalRefs.get(tab.id);
                ref?.current?.scrollToBottom();
              }}
              outputLineCount={lastLines.length}
              outputByteSize={lastLines.reduce((sum, l) => sum + l.length, 0)}
              onToggleFilter={handleToggleFilterInput}
              filterActive={!!zoneFilters[zoneIdx]}
            />
          )}
          {!showCompactCard && showFilterInput === zoneIdx && (
            <div
              className="absolute left-0 right-0 flex items-center gap-2 px-2 py-1 bg-[#1a1b26] border-b border-[#2a2d3d] z-10"
              style={{ top: showLabels ? "20px" : "0px" }}
            >
              <Filter className="w-3 h-3 text-[#565f89]" />
              <input
                autoFocus
                value={zoneFilters[zoneIdx] ?? ""}
                onChange={(e) => onSetZoneFilter(zoneIdx, e.target.value)}
                onKeyDown={(e) => {
                  e.stopPropagation();
                  if (e.key === "Escape") {
                    onToggleFilterInput(zoneIdx);
                  }
                }}
                onMouseDown={(e) => e.stopPropagation()}
                onClick={(e) => e.stopPropagation()}
                placeholder="Filter output..."
                className="flex-1 bg-transparent text-[10px] text-[#c0caf5] placeholder-[#565f89] outline-hidden font-mono"
              />
              {zoneFilters[zoneIdx] && (
                <>
                  <span className="text-[9px] text-[#e0af68] font-mono">
                    {countMatches(lastLines, zoneFilters[zoneIdx])} matches
                  </span>
                  <button
                    onClick={() => onClearZoneFilter(zoneIdx)}
                    onMouseDown={(e) => e.stopPropagation()}
                    className="text-[9px] text-[#565f89] hover:text-[#f7768e] px-1"
                  >
                    Clear
                  </button>
                </>
              )}
            </div>
          )}
          {!showCompactCard && isMultiZone && (
            <ZoneQuickActions
              zoneIndex={zoneIdx}
              isPinned={isPinned}
              onTogglePin={onTogglePin ? () => onTogglePin(zoneIdx) : undefined}
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
              <PlanViewer filePath={tab.planFilePath} visible={!useCompact} />
            </div>
          ) : (
            <div
              className={`h-full w-full ${showCompactCard ? "hidden" : ""}`}
              style={{
                paddingTop: showCompactCard
                  ? undefined
                  : showLabels
                    ? showFilterInput === zoneIdx
                      ? "46px"
                      : "20px"
                    : showFilterInput === zoneIdx
                      ? "26px"
                      : undefined,
              }}
            >
              {/* Layer 1 invariant: mount the inline visible TerminalInstance
                  only when the centralized classification owns this tab as live
                  (`assigned` in preset mode, `assigned-live` near the viewport in
                  flow mode). A virtualized (`assigned-virtual`) zone mounts ZERO
                  instances — its CompactZoneCard above is the sole renderer, and
                  the hidden mount (renderHiddenTabs) owns unassigned tabs. This
                  keeps the dual-mount race impossible (exactly-one-or-zero owner
                  per tab) that would otherwise evict UI Bridge registrations. */}
              {shouldMountInstance && instanceHandlers && (
                <TerminalInstance
                  ref={terminalRefs.get(tab.id)}
                  terminalId={tab.id}
                  pageId={pageId}
                  visible={!showCompactCard}
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
      <SuggestionChip zoneIdx={zoneIdx} />

      {/* Phase 5 — per-zone hover-revealed action cluster (maximize /
          restart / label / export / close). Fades in on cell hover
          via group-hover from the cell's outer div. */}
      <ZoneHoverActions zoneIdx={zoneIdx} onExportZone={onExportZone} />
    </div>
  );
}

const ZoneCell = memo(ZoneCellInner);

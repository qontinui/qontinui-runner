import { useCallback, useReducer, useRef, useEffect, type RefObject } from "react";
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
import type { LayoutPreset } from "./useZoneLayout";
import {
  STATE_BORDER_COLORS,
  STATE_COLORS,
  STATE_GLOW,
  CompactZoneCard,
  ZoneContextMenu,
  ZoneQuickActions,
  ZoneLabel,
  HiddenTerminal,
  formatUptime,
  countMatches,
} from "./zone-grid";
import {
  useTerminalCore,
  useSessionState,
  useZoneMetadata,
  useTransitionEffects,
  useAiFeatures,
  useUIStateCx,
} from "./contexts";

export type ViewMode = "auto" | "full" | "compact";

interface ZoneGridProps {
  /** Callbacks from useZoneActions — kept as props until that hook moves to context */
  onZoneClick: (zoneIndex: number, ctrlKey?: boolean) => void;
  onZoneDoubleClick: (zoneIndex: number) => void;
  onExit: (terminalId: string, exitCode: number | null) => void;
  onExportZone?: (zoneIndex: number, format: "text" | "markdown" | "json") => void;
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

export function ZoneGrid({
  onZoneClick,
  onZoneDoubleClick,
  onExit,
  onExportZone,
}: ZoneGridProps) {
  // Read all data from contexts
  const { tabs, zoneLayout, terminalRefs: terminalRefsRef, pageId, markReconnected } = useTerminalCore();
  const terminalRefs = terminalRefsRef.current;
  const layout = zoneLayout.layout;
  const assignments = zoneLayout.assignments;
  const focusedZone = zoneLayout.focusedZone;
  const maximizedZone = zoneLayout.maximizedZone;
  const onAssignTab = zoneLayout.assignTabToZone;
  const stateTracking = useSessionState();
  const sessionStates = stateTracking.sessionStates;
  const lastOutputLines = stateTracking.lastOutputLines;
  const stateDurations = stateTracking.stateDurations;
  const staleTabs = stateTracking.staleTabs;
  const activityData = stateTracking.activityData;
  const onOutput = stateTracking.handleOutput;
  const onReconnected = markReconnected;
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
  const { shellIntegration } = useAiFeatures();
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
  const pk = (key: string) => pageKey(pageId, key);
  const [gridState, dispatch] = useReducer(gridReducer, layout, createInitialGridState);
  const gridRef = useRef<HTMLDivElement>(null);
  const prevLayoutIdRef = useRef(layout.id);

  useEffect(() => {
    instanceStorage.setJSON(pk(`zone-col-ratios-${layout.id}`), gridState.colRatios);
  }, [gridState.colRatios, layout.id]);
  useEffect(() => {
    instanceStorage.setJSON(pk(`zone-row-ratios-${layout.id}`), gridState.rowRatios);
  }, [gridState.rowRatios, layout.id]);

  // Load namespaced ratios on mount for non-default pages
  // (createInitialGridState uses un-namespaced keys for backward compat)
  const didMountLoadRef = useRef(false);
  useEffect(() => {
    if (didMountLoadRef.current || pageId === "default") return;
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
  }, [layout.id, layout.columns, layout.rows]);

  const prevResetKeyRef = useRef(resetRatiosKey);
  useEffect(() => {
    if (resetRatiosKey !== undefined && resetRatiosKey !== prevResetKeyRef.current) {
      prevResetKeyRef.current = resetRatiosKey;
      dispatch({ type: "SET_COL_RATIOS", ratios: Array(layout.columns).fill(1) });
      dispatch({ type: "SET_ROW_RATIOS", ratios: Array(layout.rows).fill(1) });
    }
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

  const unassignedTabs = tabs.filter((t) => !Object.values(assignments).includes(t.id));
  const unassignedTerminals = unassignedTabs.filter((t) => t.type !== "plan");

  const renderHiddenTabs = (extraTabs: TerminalTab[]) =>
    extraTabs.map((tab) => (
      <HiddenTerminal
        key={tab.id}
        tab={tab}
        terminalRef={terminalRefs.get(tab.id)}
        onExit={onExit}
        onFirstInput={onFirstInput}
        onShellIntegration={onShellIntegration}
        onOutput={onOutput}
        onReconnected={onReconnected}
      />
    ));

  if (isSingleView && layout.id !== "single") {
    const tabId = assignments[singleViewZone];
    const tab = tabs.find((t) => t.id === tabId);

    return (
      <div className="h-full w-full relative">
        {tab && (
          <div className="absolute top-0 left-0 right-0 flex items-center gap-2 px-3 py-1 bg-[#13141f]/90 backdrop-blur-sm z-20">
            <span className="text-[10px] text-[#7aa2f7] font-medium">Maximized</span>
            <span className="text-[10px] text-[#a9b1d6]">{tab.title}</span>
            <span className="text-[9px] text-[#565f89] ml-auto">
              Esc or double-click to restore
            </span>
          </div>
        )}

        {layout.zones.map((_, zoneIdx) => {
          const zoneTabId = assignments[zoneIdx];
          const zoneTab = tabs.find((t) => t.id === zoneTabId);
          if (!zoneTab) return null;

          const isVisible = zoneIdx === singleViewZone;
          const ref = terminalRefs.get(zoneTab.id);

          return (
            <div
              key={zoneTab.id}
              className={isVisible ? "h-full w-full" : "hidden"}
              onMouseDown={(e) => handleZoneMouseDown(zoneIdx, e)}
              onDoubleClick={() => onZoneDoubleClick(zoneIdx)}
            >
              {zoneTab.type === "plan" && zoneTab.planFilePath ? (
                <PlanViewer filePath={zoneTab.planFilePath} visible={isVisible} />
              ) : (
                <TerminalInstance
                  ref={ref}
                  terminalId={zoneTab.id}
                  visible={isVisible}
                  isReconnecting={zoneTab.isReconnecting}
                  onReconnected={() => onReconnected(zoneTab.id)}
                  onExit={(code) => onExit(zoneTab.id, code)}
                  onFirstInput={(input) => onFirstInput(zoneTab.id, input)}
                  onShellIntegration={(event) => onShellIntegration(zoneTab.id, event)}
                  onOutput={(text) => onOutput(zoneTab.id, text)}
                />
              )}
            </div>
          );
        })}

        {renderHiddenTabs(unassignedTerminals)}
      </div>
    );
  }

  return (
    <div
      ref={gridRef}
      className="h-full w-full relative"
      style={{
        display: "grid",
        gridTemplateColumns: gridState.colRatios.map((r) => `${r}fr`).join(" "),
        gridTemplateRows: gridState.rowRatios.map((r) => `${r}fr`).join(" "),
        gap: "2px",
      }}
      onDragEnd={() => dispatch({ type: "SET_DROP_TARGET", zone: null })}
    >
      {layout.zones.map((zone, zoneIdx) => (
        <ZoneCell
          key={`zone-${zoneIdx}`}
          zone={zone}
          zoneIdx={zoneIdx}
          tabs={tabs}
          assignments={assignments}
          focusedZone={focusedZone}
          sessionStates={sessionStates}
          lastOutputLines={lastOutputLines}
          terminalRefs={terminalRefs}
          onZoneClick={onZoneClick}
          onZoneDoubleClick={onZoneDoubleClick}
          onExit={onExit}
          onFirstInput={onFirstInput}
          onShellIntegration={onShellIntegration}
          onOutput={onOutput}
          onReconnected={onReconnected}
          onAssignTab={onAssignTab}
          flashingTabs={flashingTabs}
          stateDurations={stateDurations}
          selectedZones={selectedZones}
          staleTabs={staleTabs}
          pinnedZones={pinnedZones}
          onTogglePin={onTogglePin}
          outputSearchQuery={outputSearchQuery}
          swapSource={swapSource}
          activityData={activityData}
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
          onSetDropTarget={(zone) => dispatch({ type: "SET_DROP_TARGET", zone })}
          onSetContextMenu={(menu) => dispatch({ type: "SET_CONTEXT_MENU", menu })}
          onToggleFilterInput={() => dispatch({ type: "TOGGLE_FILTER_INPUT", zoneIndex: zoneIdx })}
          onSetZoneFilter={(value) =>
            dispatch({
              type: "SET_ZONE_FILTER",
              zoneIndex: zoneIdx,
              value,
            })
          }
          onClearZoneFilter={() => dispatch({ type: "CLEAR_ZONE_FILTER", zoneIndex: zoneIdx })}
        />
      ))}

      {layout.columns > 1 &&
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

      {layout.rows > 1 &&
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
                if (cmTab) {
                  const ref = terminalRefs.get(cmTab.id);
                  ref?.current?.writeToTerminal("y\r");
                }
              }}
              onReject={() => {
                if (cmTab) {
                  const ref = terminalRefs.get(cmTab.id);
                  ref?.current?.writeToTerminal("n\r");
                }
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

function ZoneCell({
  zone,
  zoneIdx,
  tabs,
  assignments,
  focusedZone,
  sessionStates,
  lastOutputLines,
  terminalRefs,
  onZoneClick: _onZoneClick,
  onZoneDoubleClick,
  onExit,
  onFirstInput,
  onShellIntegration,
  onOutput,
  onReconnected,
  onAssignTab,
  flashingTabs,
  stateDurations,
  selectedZones,
  staleTabs,
  pinnedZones,
  onTogglePin,
  outputSearchQuery,
  swapSource,
  activityData,
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
  focusedZone: number;
  sessionStates: Record<string, SessionState>;
  lastOutputLines: Record<string, string[]>;
  terminalRefs: Map<string, RefObject<TerminalInstanceHandle | null>>;
  onZoneClick: (zoneIndex: number, ctrlKey?: boolean) => void;
  onZoneDoubleClick: (zoneIndex: number) => void;
  onExit: (terminalId: string, exitCode: number | null) => void;
  onFirstInput: (terminalId: string, input: string) => void;
  onShellIntegration: (tabId: string, event: ShellIntegrationEvent) => void;
  onOutput: (tabId: string, text: string) => void;
  onReconnected: (tabId: string) => void;
  onAssignTab?: (zoneIndex: number, tabId: string) => void;
  flashingTabs?: Set<string>;
  stateDurations?: Record<string, string>;
  selectedZones?: Set<number>;
  staleTabs?: Set<string>;
  pinnedZones?: Set<number>;
  onTogglePin?: (zoneIndex: number) => void;
  outputSearchQuery?: string;
  swapSource?: number | null;
  activityData?: Record<string, number[]>;
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
  onToggleFilterInput: () => void;
  onSetZoneFilter: (value: string) => void;
  onClearZoneFilter: () => void;
}) {
  const tabId = assignments[zoneIdx];
  const tab = tabs.find((t) => t.id === tabId);
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
  const isFlashing = tab && flashingTabs?.has(tab.id);
  const isStale = tab && staleTabs?.has(tab.id);
  const searchMatch =
    tab && outputSearchQuery
      ? (lastOutputLines[tab.id] ?? []).some((l) =>
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
      className={`relative overflow-hidden ${isFlashing ? "zone-flash" : ""} ${
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
          {useCompact && (
            <CompactZoneCard
              tab={tab}
              state={state}
              zoneIndex={zoneIdx}
              lastLines={lastOutputLines[tab.id] ?? []}
              onQuickApprove={() => {
                const ref = terminalRefs.get(tab.id);
                ref?.current?.writeToTerminal("y\r");
              }}
              onQuickReject={() => {
                const ref = terminalRefs.get(tab.id);
                ref?.current?.writeToTerminal("n\r");
              }}
              onSendCommand={(text) => {
                const ref = terminalRefs.get(tab.id);
                ref?.current?.writeToTerminal(`${text}\r`);
              }}
              duration={stateDurations?.[tab.id]}
              isStale={isStale ?? false}
              searchQuery={outputSearchQuery}
              activity={activityData?.[tab.id]}
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
          {!useCompact && showLabels && (
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
              outputLineCount={(lastOutputLines[tab.id] ?? []).length}
              outputByteSize={(lastOutputLines[tab.id] ?? []).reduce((sum, l) => sum + l.length, 0)}
              onToggleFilter={onToggleFilterInput}
              filterActive={!!zoneFilters[zoneIdx]}
            />
          )}
          {!useCompact && showFilterInput === zoneIdx && (
            <div
              className="absolute left-0 right-0 flex items-center gap-2 px-2 py-1 bg-[#1a1b26] border-b border-[#2a2d3d] z-10"
              style={{ top: showLabels ? "20px" : "0px" }}
            >
              <Filter className="w-3 h-3 text-[#565f89]" />
              <input
                autoFocus
                value={zoneFilters[zoneIdx] ?? ""}
                onChange={(e) => onSetZoneFilter(e.target.value)}
                onKeyDown={(e) => {
                  e.stopPropagation();
                  if (e.key === "Escape") {
                    onToggleFilterInput();
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
                    {countMatches(lastOutputLines[tab.id] ?? [], zoneFilters[zoneIdx])} matches
                  </span>
                  <button
                    onClick={() => onClearZoneFilter()}
                    onMouseDown={(e) => e.stopPropagation()}
                    className="text-[9px] text-[#565f89] hover:text-[#f7768e] px-1"
                  >
                    Clear
                  </button>
                </>
              )}
            </div>
          )}
          {!useCompact && isMultiZone && (
            <ZoneQuickActions
              zoneIndex={zoneIdx}
              isPinned={isPinned}
              onTogglePin={onTogglePin ? () => onTogglePin(zoneIdx) : undefined}
              onMaximize={() => onZoneDoubleClick(zoneIdx)}
              onCopyOutput={() => {
                const lines = lastOutputLines[tab.id] ?? [];
                navigator.clipboard.writeText(lines.join("\n"));
              }}
              onScrollToBottom={() => {
                const ref = terminalRefs.get(tab.id);
                ref?.current?.scrollToBottom();
              }}
              lastLines={lastOutputLines[tab.id] ?? []}
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
              className={`h-full w-full ${useCompact ? "hidden" : ""}`}
              style={{
                paddingTop: useCompact
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
              <TerminalInstance
                ref={terminalRefs.get(tab.id)}
                terminalId={tab.id}
                visible={!useCompact}
                isReconnecting={tab.isReconnecting}
                onReconnected={() => onReconnected(tab.id)}
                onExit={(code) => onExit(tab.id, code)}
                onFirstInput={(input) => onFirstInput(tab.id, input)}
                onShellIntegration={(event) => onShellIntegration(tab.id, event)}
                onOutput={(text) => onOutput(tab.id, text)}
              />
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
    </div>
  );
}

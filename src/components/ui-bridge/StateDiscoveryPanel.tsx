/**
 * StateDiscoveryPanel
 *
 * Visualizes state discovery data from capture sessions:
 * - State candidates (groups of co-occurring elements)
 * - Transition graph (actions that change states)
 * - Co-occurrence statistics
 */

import { useReducer, useMemo, useCallback, useRef } from "react";
import { Circle, AlertCircle, Network, RefreshCw, CheckCircle2, GitBranch } from "lucide-react";
import type { CooccurrenceExport } from "../../types/ui-bridge-types";
import { StateGraphView } from "./StateGraphView";
import { ExportConfigDialog } from "./ExportConfigDialog";
import { StateComparisonView } from "./StateComparisonView";
import { NavigationBreadcrumb } from "./NavigationBreadcrumb";
import { FingerprintStatesPanel } from "./FingerprintStatesPanel";
import {
  type NavigationState,
  type NavigationItem,
  createInitialNavigationState,
  addToHistory,
  createTransitionKey,
  parseTransitionKey,
} from "./navigation";
import type { FingerprintDiscoveryResult } from "./discovery-types";
import {
  StateCandidateCard,
  TransitionCard,
  DiscoveredStateCard,
  DiscoveredTransitionCard,
} from "./StateDiscoveryCards";
import { StatsView, CooccurrenceMatrixView } from "./StateDiscoveryViews";
import { StateDiscoveryToolbar } from "./StateDiscoveryToolbar";

export type { FingerprintDiscoveryResult };

interface StateDiscoveryPanelProps {
  cooccurrenceData: CooccurrenceExport | null;
  isLoading: boolean;
  sessionActive: boolean;
  captureCount: number;
  onRefresh: () => Promise<CooccurrenceExport | null>;
  onSelectFingerprint?: (hash: string) => void;
  disabled?: boolean;
  onRunDiscovery?: (data: CooccurrenceExport) => Promise<FingerprintDiscoveryResult | null>;
  isRunningDiscovery?: boolean;
  discoveryResult?: FingerprintDiscoveryResult | null;
}

type ViewMode = "states" | "graph" | "transitions" | "matrix" | "stats";

interface PanelState {
  viewMode: ViewMode;
  expandedStates: Set<number>;
  expandedDiscoveredStates: Set<string>;
  copied: boolean;
  showExportDialog: boolean;
  showDiscoveredStates: boolean;
  compareMode: boolean;
  selectedForComparison: Set<string>;
  showComparisonView: boolean;
  navigationState: NavigationState;
  showFingerprintPanel: boolean;
}

type PanelAction =
  | { type: "SET_VIEW_MODE"; mode: ViewMode }
  | { type: "TOGGLE_STATE_EXPANDED"; index: number }
  | { type: "TOGGLE_DISCOVERED_STATE_EXPANDED"; stateId: string }
  | { type: "EXPAND_DISCOVERED_STATE"; stateId: string }
  | { type: "SET_COPIED"; value: boolean }
  | { type: "SET_SHOW_EXPORT_DIALOG"; value: boolean }
  | { type: "SET_SHOW_DISCOVERED_STATES"; value: boolean }
  | { type: "TOGGLE_DISCOVERED_STATES" }
  | { type: "SET_COMPARE_MODE"; value: boolean }
  | { type: "TOGGLE_STATE_FOR_COMPARISON"; stateId: string }
  | { type: "EXIT_COMPARE_MODE" }
  | { type: "SET_SHOW_COMPARISON_VIEW"; value: boolean }
  | { type: "SET_NAVIGATION_STATE"; updater: (prev: NavigationState) => NavigationState }
  | { type: "CLEAR_NAVIGATION" }
  | { type: "SET_SHOW_FINGERPRINT_PANEL"; value: boolean }
  | {
      type: "NAVIGATE_TO_STATE";
      stateId: string;
      stateName: string;
    }
  | {
      type: "NAVIGATE_TO_TRANSITION";
      transitionKey: string;
      label: string;
    }
  | {
      type: "NAVIGATE_TO_FINGERPRINT";
      hash: string;
      label: string;
    }
  | {
      type: "SHOW_STATE_IN_GRAPH";
      stateId: string;
    }
  | {
      type: "SHOW_TRANSITION_IN_GRAPH";
      transitionKey: string;
    }
  | { type: "CLEAR_SELECTION" };

function createInitialState(): PanelState {
  return {
    viewMode: "states",
    expandedStates: new Set(),
    expandedDiscoveredStates: new Set(),
    copied: false,
    showExportDialog: false,
    showDiscoveredStates: false,
    compareMode: false,
    selectedForComparison: new Set(),
    showComparisonView: false,
    navigationState: createInitialNavigationState(),
    showFingerprintPanel: false,
  };
}

function panelReducer(state: PanelState, action: PanelAction): PanelState {
  switch (action.type) {
    case "SET_VIEW_MODE":
      return { ...state, viewMode: action.mode };

    case "TOGGLE_STATE_EXPANDED": {
      const next = new Set(state.expandedStates);
      if (next.has(action.index)) {
        next.delete(action.index);
      } else {
        next.add(action.index);
      }
      return { ...state, expandedStates: next };
    }

    case "TOGGLE_DISCOVERED_STATE_EXPANDED": {
      const next = new Set(state.expandedDiscoveredStates);
      if (next.has(action.stateId)) {
        next.delete(action.stateId);
      } else {
        next.add(action.stateId);
      }
      return { ...state, expandedDiscoveredStates: next };
    }

    case "EXPAND_DISCOVERED_STATE": {
      const next = new Set(state.expandedDiscoveredStates);
      next.add(action.stateId);
      return { ...state, expandedDiscoveredStates: next };
    }

    case "SET_COPIED":
      return { ...state, copied: action.value };

    case "SET_SHOW_EXPORT_DIALOG":
      return { ...state, showExportDialog: action.value };

    case "SET_SHOW_DISCOVERED_STATES":
      return { ...state, showDiscoveredStates: action.value };

    case "TOGGLE_DISCOVERED_STATES":
      return { ...state, showDiscoveredStates: !state.showDiscoveredStates };

    case "SET_COMPARE_MODE":
      return { ...state, compareMode: action.value };

    case "TOGGLE_STATE_FOR_COMPARISON": {
      const next = new Set(state.selectedForComparison);
      if (next.has(action.stateId)) {
        next.delete(action.stateId);
      } else if (next.size < 2) {
        next.add(action.stateId);
      }
      return { ...state, selectedForComparison: next };
    }

    case "EXIT_COMPARE_MODE":
      return {
        ...state,
        compareMode: false,
        selectedForComparison: new Set(),
        showComparisonView: false,
      };

    case "SET_SHOW_COMPARISON_VIEW":
      return { ...state, showComparisonView: action.value };

    case "SET_NAVIGATION_STATE":
      return { ...state, navigationState: action.updater(state.navigationState) };

    case "CLEAR_NAVIGATION":
      return {
        ...state,
        navigationState: createInitialNavigationState(),
        showFingerprintPanel: false,
      };

    case "SET_SHOW_FINGERPRINT_PANEL":
      return { ...state, showFingerprintPanel: action.value };

    case "NAVIGATE_TO_STATE": {
      const navState: NavigationState = {
        ...state.navigationState,
        selectedStateId: action.stateId,
        selectedTransitionId: null,
        selectedFingerprintId: null,
        history: addToHistory(state.navigationState.history, {
          type: "state",
          id: action.stateId,
          label: action.stateName,
        }),
      };
      const expanded = new Set(state.expandedDiscoveredStates);
      expanded.add(action.stateId);
      return {
        ...state,
        navigationState: navState,
        viewMode: "states",
        showDiscoveredStates: true,
        expandedDiscoveredStates: expanded,
      };
    }

    case "NAVIGATE_TO_TRANSITION": {
      const navState: NavigationState = {
        ...state.navigationState,
        selectedStateId: null,
        selectedTransitionId: action.transitionKey,
        selectedFingerprintId: null,
        history: addToHistory(state.navigationState.history, {
          type: "transition",
          id: action.transitionKey,
          label: action.label,
        }),
      };
      return {
        ...state,
        navigationState: navState,
        viewMode: "graph",
      };
    }

    case "NAVIGATE_TO_FINGERPRINT": {
      const navState: NavigationState = {
        ...state.navigationState,
        selectedStateId: null,
        selectedTransitionId: null,
        selectedFingerprintId: action.hash,
        history: addToHistory(state.navigationState.history, {
          type: "fingerprint",
          id: action.hash,
          label: action.label,
        }),
      };
      return {
        ...state,
        navigationState: navState,
        showFingerprintPanel: true,
      };
    }

    case "SHOW_STATE_IN_GRAPH":
      return {
        ...state,
        navigationState: {
          ...state.navigationState,
          selectedStateId: action.stateId,
          selectedTransitionId: null,
        },
        viewMode: "graph",
      };

    case "SHOW_TRANSITION_IN_GRAPH":
      return {
        ...state,
        navigationState: {
          ...state.navigationState,
          selectedTransitionId: action.transitionKey,
          selectedStateId: null,
        },
        viewMode: "graph",
      };

    case "CLEAR_SELECTION":
      return {
        ...state,
        navigationState: {
          ...state.navigationState,
          selectedStateId: null,
          selectedTransitionId: null,
        },
      };

    default:
      return state;
  }
}

export function StateDiscoveryPanel({
  cooccurrenceData,
  isLoading,
  sessionActive,
  captureCount,
  onRefresh,
  onSelectFingerprint,
  disabled,
  onRunDiscovery,
  isRunningDiscovery,
  discoveryResult,
}: StateDiscoveryPanelProps) {
  const [state, dispatch] = useReducer(panelReducer, undefined, createInitialState);

  const stateCardRefs = useRef<Map<string, HTMLDivElement>>(new Map());

  const selectedStates = useMemo(() => {
    if (!discoveryResult) return [];
    return discoveryResult.states.filter((s) => state.selectedForComparison.has(s.stateId));
  }, [discoveryResult, state.selectedForComparison]);

  const navigateToState = useCallback((stateId: string, stateName: string) => {
    dispatch({ type: "NAVIGATE_TO_STATE", stateId, stateName });
    setTimeout(() => {
      const cardElement = stateCardRefs.current.get(stateId);
      cardElement?.scrollIntoView({ behavior: "smooth", block: "center" });
    }, 100);
  }, []);

  const navigateToTransition = useCallback(
    (fromStateId: string, toStateId: string, actionType?: string) => {
      const transitionKey = createTransitionKey(fromStateId, toStateId);
      const label = actionType ? `${actionType}: ${fromStateId} -> ${toStateId}` : transitionKey;
      dispatch({ type: "NAVIGATE_TO_TRANSITION", transitionKey, label });
    },
    [],
  );

  const navigateToFingerprint = useCallback(
    (hash: string, label?: string) => {
      const displayLabel = label || hash.slice(0, 12) + "...";
      dispatch({ type: "NAVIGATE_TO_FINGERPRINT", hash, label: displayLabel });
      onSelectFingerprint?.(hash);
    },
    [onSelectFingerprint],
  );

  const handleBreadcrumbNavigate = useCallback(
    (item: NavigationItem) => {
      switch (item.type) {
        case "graph":
          dispatch({ type: "SET_VIEW_MODE", mode: "graph" });
          break;
        case "state":
          navigateToState(item.id, item.label);
          break;
        case "transition": {
          const parsed = parseTransitionKey(item.id);
          if (parsed) {
            navigateToTransition(parsed.fromStateId, parsed.toStateId);
          }
          break;
        }
        case "fingerprint":
          navigateToFingerprint(item.id, item.label);
          break;
      }
    },
    [navigateToState, navigateToTransition, navigateToFingerprint],
  );

  const clearNavigation = useCallback(() => {
    dispatch({ type: "CLEAR_NAVIGATION" });
  }, []);

  const handleShowStateInGraph = useCallback((stateId: string) => {
    dispatch({ type: "SHOW_STATE_IN_GRAPH", stateId });
  }, []);

  const handleShowTransitionInGraph = useCallback((fromStateId: string, toStateId: string) => {
    const transitionKey = createTransitionKey(fromStateId, toStateId);
    dispatch({ type: "SHOW_TRANSITION_IN_GRAPH", transitionKey });
  }, []);

  const handleGraphNodeClick = useCallback(
    (stateId: string | null) => {
      if (stateId) {
        const foundState = discoveryResult?.states.find((s) => s.stateId === stateId);
        if (foundState) {
          navigateToState(stateId, foundState.name);
        }
      } else {
        dispatch({ type: "CLEAR_SELECTION" });
      }
    },
    [discoveryResult, navigateToState],
  );

  const handleGraphEdgeClick = useCallback(
    (fromStateId: string, toStateId: string) => {
      navigateToTransition(fromStateId, toStateId);
    },
    [navigateToTransition],
  );

  const handleRunDiscovery = async () => {
    if (!cooccurrenceData || !onRunDiscovery) return;
    const result = await onRunDiscovery(cooccurrenceData);
    if (result) {
      dispatch({ type: "SET_SHOW_DISCOVERED_STATES", value: true });
    }
  };

  const handleCopyData = async () => {
    if (!cooccurrenceData) return;
    try {
      await navigator.clipboard.writeText(JSON.stringify(cooccurrenceData, null, 2));
      dispatch({ type: "SET_COPIED", value: true });
      setTimeout(() => dispatch({ type: "SET_COPIED", value: false }), 2000);
    } catch {
      // Ignore
    }
  };

  const handleExportData = () => {
    if (!cooccurrenceData) return;
    const blob = new Blob([JSON.stringify(cooccurrenceData, null, 2)], {
      type: "application/json",
    });
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url;
    a.download = `cooccurrence-export-${cooccurrenceData.sessionId}.json`;
    a.click();
    URL.revokeObjectURL(url);
  };

  if (disabled) {
    return (
      <div className="flex flex-col items-center justify-center h-full text-muted-foreground text-sm gap-2">
        <Network className="w-8 h-8 opacity-50" />
        <p>Connect to a browser tab to use state discovery</p>
      </div>
    );
  }

  if (!sessionActive && !cooccurrenceData) {
    return (
      <div className="flex flex-col items-center justify-center h-full text-muted-foreground text-sm gap-4">
        <Network className="w-12 h-12 opacity-50" />
        <div className="text-center space-y-2">
          <p className="font-medium">No Capture Session Active</p>
          <p className="text-xs max-w-md">
            Start a capture session from the toolbar to begin collecting element co-occurrence data.
            Visit multiple pages and perform actions to build a state model.
          </p>
        </div>
      </div>
    );
  }

  if (sessionActive && captureCount === 0) {
    return (
      <div className="flex flex-col items-center justify-center h-full text-muted-foreground text-sm gap-4">
        <Circle className="w-6 h-6 fill-red-500 text-red-500 animate-pulse" />
        <div className="text-center space-y-2">
          <p className="font-medium">Capture Session Active</p>
          <p className="text-xs max-w-md">
            Refresh elements or navigate to different pages to capture element data. Perform actions
            to record state transitions.
          </p>
        </div>
      </div>
    );
  }

  const { navigationState } = state;

  const currentSelection = navigationState.selectedStateId
    ? {
        type: "state" as const,
        id: navigationState.selectedStateId,
        label:
          discoveryResult?.states.find((s) => s.stateId === navigationState.selectedStateId)
            ?.name || navigationState.selectedStateId,
      }
    : navigationState.selectedTransitionId
      ? {
          type: "transition" as const,
          id: navigationState.selectedTransitionId,
          label: navigationState.selectedTransitionId,
        }
      : navigationState.selectedFingerprintId
        ? {
            type: "fingerprint" as const,
            id: navigationState.selectedFingerprintId,
            label: navigationState.selectedFingerprintId.slice(0, 12) + "...",
          }
        : null;

  const showBreadcrumb =
    navigationState.history.items.length > 0 ||
    navigationState.selectedStateId ||
    navigationState.selectedTransitionId ||
    navigationState.selectedFingerprintId;

  return (
    <div className="h-full flex flex-col">
      <StateDiscoveryToolbar
        viewMode={state.viewMode}
        onViewModeChange={(mode) => dispatch({ type: "SET_VIEW_MODE", mode })}
        cooccurrenceData={cooccurrenceData}
        discoveryResult={discoveryResult}
        isLoading={isLoading}
        sessionActive={sessionActive}
        copied={state.copied}
        showDiscoveredStates={state.showDiscoveredStates}
        compareMode={state.compareMode}
        selectedStatesCount={selectedStates.length}
        isRunningDiscovery={isRunningDiscovery}
        hasRunDiscovery={!!onRunDiscovery}
        onRunDiscovery={handleRunDiscovery}
        onToggleDiscoveredStates={() => dispatch({ type: "TOGGLE_DISCOVERED_STATES" })}
        onToggleCompareMode={() => dispatch({ type: "SET_COMPARE_MODE", value: true })}
        onExitCompareMode={() => dispatch({ type: "EXIT_COMPARE_MODE" })}
        onOpenComparison={() => {
          if (selectedStates.length === 2) {
            dispatch({ type: "SET_SHOW_COMPARISON_VIEW", value: true });
          }
        }}
        onRefresh={onRefresh}
        onCopyData={handleCopyData}
        onExportData={handleExportData}
        onOpenExportDialog={() => dispatch({ type: "SET_SHOW_EXPORT_DIALOG", value: true })}
      />

      {showBreadcrumb && (
        <div className="mb-2">
          <NavigationBreadcrumb
            history={navigationState.history}
            onNavigate={handleBreadcrumbNavigate}
            onClear={clearNavigation}
            currentSelection={currentSelection}
          />
        </div>
      )}

      {state.showFingerprintPanel && navigationState.selectedFingerprintId && cooccurrenceData && (
        <div className="mb-3">
          <FingerprintStatesPanel
            fingerprintHash={navigationState.selectedFingerprintId}
            fingerprintDetails={cooccurrenceData.fingerprintDetails}
            discoveryResult={discoveryResult ?? null}
            onSelectState={navigateToState}
            onClose={() => {
              dispatch({ type: "SET_SHOW_FINGERPRINT_PANEL", value: false });
              dispatch({
                type: "SET_NAVIGATION_STATE",
                updater: (prev) => ({ ...prev, selectedFingerprintId: null }),
              });
            }}
          />
        </div>
      )}

      <ExportConfigDialog
        isOpen={state.showExportDialog}
        onClose={() => dispatch({ type: "SET_SHOW_EXPORT_DIALOG", value: false })}
        cooccurrenceData={cooccurrenceData}
        discoveryResult={discoveryResult ?? null}
      />

      {state.showComparisonView && selectedStates.length === 2 && cooccurrenceData && (
        <StateComparisonView
          stateA={selectedStates[0]}
          stateB={selectedStates[1]}
          fingerprintDetails={cooccurrenceData.fingerprintDetails}
          onClose={() => dispatch({ type: "SET_SHOW_COMPARISON_VIEW", value: false })}
          onSelectFingerprint={onSelectFingerprint}
        />
      )}

      <div className="flex-1 overflow-y-auto">
        {isLoading ? (
          <div className="flex items-center justify-center h-32">
            <RefreshCw className="w-6 h-6 animate-spin text-muted-foreground" />
          </div>
        ) : !cooccurrenceData ? (
          <div className="flex flex-col items-center justify-center h-32 text-muted-foreground text-sm gap-2">
            <AlertCircle className="w-6 h-6" />
            <p>No co-occurrence data available yet</p>
            <p className="text-xs">Refresh elements to generate data</p>
          </div>
        ) : state.viewMode === "states" ? (
          <StatesContent
            cooccurrenceData={cooccurrenceData}
            discoveryResult={discoveryResult}
            showDiscoveredStates={state.showDiscoveredStates}
            expandedStates={state.expandedStates}
            expandedDiscoveredStates={state.expandedDiscoveredStates}
            compareMode={state.compareMode}
            selectedForComparison={state.selectedForComparison}
            selectedStateId={navigationState.selectedStateId}
            stateCardRefs={stateCardRefs}
            onToggleStateExpanded={(index) => dispatch({ type: "TOGGLE_STATE_EXPANDED", index })}
            onToggleDiscoveredStateExpanded={(stateId) =>
              dispatch({ type: "TOGGLE_DISCOVERED_STATE_EXPANDED", stateId })
            }
            onToggleStateForComparison={(stateId) =>
              dispatch({ type: "TOGGLE_STATE_FOR_COMPARISON", stateId })
            }
            onSelectFingerprint={onSelectFingerprint}
            onNavigateToFingerprint={navigateToFingerprint}
            onShowInGraph={handleShowStateInGraph}
          />
        ) : state.viewMode === "graph" ? (
          <div className="h-full min-h-[400px] relative">
            {discoveryResult && discoveryResult.states.length > 0 ? (
              <StateGraphView
                discoveryResult={discoveryResult}
                fingerprintDetails={cooccurrenceData.fingerprintDetails}
                onSelectState={handleGraphNodeClick}
                onSelectTransition={handleGraphEdgeClick}
                selectedStateId={navigationState.selectedStateId}
                selectedTransitionId={navigationState.selectedTransitionId}
              />
            ) : (
              <div className="flex flex-col items-center justify-center h-full text-muted-foreground text-sm gap-2">
                <AlertCircle className="w-8 h-8 opacity-50" />
                <p>No states discovered yet</p>
                <p className="text-xs">Run state discovery to visualize the state graph</p>
              </div>
            )}
          </div>
        ) : state.viewMode === "transitions" ? (
          <TransitionsContent
            cooccurrenceData={cooccurrenceData}
            discoveryResult={discoveryResult}
            selectedTransitionId={navigationState.selectedTransitionId}
            onNavigateToState={navigateToState}
            onNavigateToFingerprint={navigateToFingerprint}
            onShowInGraph={handleShowTransitionInGraph}
          />
        ) : state.viewMode === "matrix" ? (
          <CooccurrenceMatrixView data={cooccurrenceData} />
        ) : (
          <StatsView data={cooccurrenceData} />
        )}
      </div>
    </div>
  );
}

function StatesContent({
  cooccurrenceData,
  discoveryResult,
  showDiscoveredStates,
  expandedStates,
  expandedDiscoveredStates,
  compareMode,
  selectedForComparison,
  selectedStateId,
  stateCardRefs,
  onToggleStateExpanded,
  onToggleDiscoveredStateExpanded,
  onToggleStateForComparison,
  onSelectFingerprint,
  onNavigateToFingerprint,
  onShowInGraph,
}: {
  cooccurrenceData: CooccurrenceExport;
  discoveryResult?: FingerprintDiscoveryResult | null;
  showDiscoveredStates: boolean;
  expandedStates: Set<number>;
  expandedDiscoveredStates: Set<string>;
  compareMode: boolean;
  selectedForComparison: Set<string>;
  selectedStateId: string | null;
  stateCardRefs: React.RefObject<Map<string, HTMLDivElement>>;
  onToggleStateExpanded: (index: number) => void;
  onToggleDiscoveredStateExpanded: (stateId: string) => void;
  onToggleStateForComparison: (stateId: string) => void;
  onSelectFingerprint?: (hash: string) => void;
  onNavigateToFingerprint: (hash: string, label?: string) => void;
  onShowInGraph: (stateId: string) => void;
}) {
  if (showDiscoveredStates && discoveryResult) {
    if (discoveryResult.states.length === 0) {
      return (
        <div className="text-center text-muted-foreground text-sm py-8">
          <p>No states discovered</p>
          <p className="text-xs mt-1">
            The algorithm found no distinct states from the capture data
          </p>
        </div>
      );
    }

    return (
      <div className="space-y-2">
        <div className="bg-muted/30 rounded-lg p-3 mb-3 grid grid-cols-4 gap-3 text-xs">
          <div className="text-center">
            <div className="text-lg font-bold text-primary">
              {discoveryResult.statistics.discoveredStates}
            </div>
            <div className="text-muted-foreground">States</div>
          </div>
          <div className="text-center">
            <div className="text-lg font-bold text-blue-400">
              {discoveryResult.statistics.globalStates}
            </div>
            <div className="text-muted-foreground">Global</div>
          </div>
          <div className="text-center">
            <div className="text-lg font-bold text-orange-400">
              {discoveryResult.statistics.modalStates}
            </div>
            <div className="text-muted-foreground">Modal</div>
          </div>
          <div className="text-center">
            <div className="text-lg font-bold text-green-400">
              {discoveryResult.statistics.discoveredTransitions}
            </div>
            <div className="text-muted-foreground">Transitions</div>
          </div>
        </div>
        {discoveryResult.states.map((discoveredState) => (
          <DiscoveredStateCard
            key={discoveredState.stateId}
            state={discoveredState}
            fingerprintDetails={cooccurrenceData.fingerprintDetails}
            expanded={expandedDiscoveredStates.has(discoveredState.stateId)}
            onToggle={() => onToggleDiscoveredStateExpanded(discoveredState.stateId)}
            onSelectFingerprint={onNavigateToFingerprint}
            compareMode={compareMode}
            isSelected={selectedForComparison.has(discoveredState.stateId)}
            onToggleSelect={() => onToggleStateForComparison(discoveredState.stateId)}
            onShowInGraph={onShowInGraph}
            isHighlighted={selectedStateId === discoveredState.stateId}
            cardRef={
              selectedStateId === discoveredState.stateId
                ? { current: stateCardRefs.current.get(discoveredState.stateId) || null }
                : undefined
            }
          />
        ))}
      </div>
    );
  }

  if (cooccurrenceData.stateCandidates.length === 0) {
    return (
      <div className="text-center text-muted-foreground text-sm py-8">
        <p>No state candidates found yet</p>
        <p className="text-xs mt-1">Capture more pages to find co-occurring element groups</p>
      </div>
    );
  }

  return (
    <div className="space-y-2">
      {cooccurrenceData.stateCandidates.map((candidate, index) => (
        <StateCandidateCard
          key={candidate.fingerprints[0] ?? `candidate-${index}`}
          candidate={candidate}
          fingerprintDetails={cooccurrenceData.fingerprintDetails}
          index={index}
          expanded={expandedStates.has(index)}
          onToggle={() => onToggleStateExpanded(index)}
          onSelectFingerprint={onSelectFingerprint}
        />
      ))}
    </div>
  );
}

function TransitionsContent({
  cooccurrenceData,
  discoveryResult,
  selectedTransitionId,
  onNavigateToState,
  onNavigateToFingerprint,
  onShowInGraph,
}: {
  cooccurrenceData: CooccurrenceExport;
  discoveryResult?: FingerprintDiscoveryResult | null;
  selectedTransitionId: string | null;
  onNavigateToState: (stateId: string, stateName: string) => void;
  onNavigateToFingerprint: (hash: string, label?: string) => void;
  onShowInGraph: (fromStateId: string, toStateId: string) => void;
}) {
  return (
    <div className="space-y-2">
      {discoveryResult && discoveryResult.transitions.length > 0 && (
        <div className="mb-4">
          <div className="text-sm font-medium mb-2 flex items-center gap-2">
            <CheckCircle2 className="w-4 h-4 text-green-500" />
            Discovered Transitions ({discoveryResult.transitions.length})
          </div>
          <div className="space-y-2">
            {discoveryResult.transitions.map((transition) => {
              const transitionKey = createTransitionKey(
                transition.fromStateId,
                transition.toStateId,
              );
              return (
                <DiscoveredTransitionCard
                  key={transitionKey}
                  transition={transition}
                  discoveryResult={discoveryResult}
                  isHighlighted={selectedTransitionId === transitionKey}
                  onNavigateToState={onNavigateToState}
                  onShowInGraph={onShowInGraph}
                />
              );
            })}
          </div>
        </div>
      )}

      <div className="text-sm font-medium mb-2 flex items-center gap-2">
        <GitBranch className="w-4 h-4" />
        Raw Transition Events ({cooccurrenceData.transitions.length})
      </div>
      {cooccurrenceData.transitions.length === 0 ? (
        <div className="text-center text-muted-foreground text-sm py-8">
          <p>No transitions recorded yet</p>
          <p className="text-xs mt-1">Execute actions to record state transitions</p>
        </div>
      ) : (
        cooccurrenceData.transitions.map((transition) => (
          <TransitionCard
            key={transition.actionId}
            transition={transition}
            fingerprintDetails={cooccurrenceData.fingerprintDetails}
            onSelectFingerprint={onNavigateToFingerprint}
          />
        ))
      )}
    </div>
  );
}

export default StateDiscoveryPanel;

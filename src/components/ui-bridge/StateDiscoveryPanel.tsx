/**
 * StateDiscoveryPanel
 *
 * Visualizes state discovery data from capture sessions:
 * - State candidates (groups of co-occurring elements)
 * - Transition graph (actions that change states)
 * - Co-occurrence statistics
 */

import { useState, useMemo, useCallback, useRef } from "react";
import { Badge } from "../ui/Badge";
import { Button } from "../ui/Button";
import {
  Network,
  Layers,
  Hash,
  MapPin,
  Repeat,
  ChevronDown,
  ChevronRight,
  Circle,
  AlertCircle,
  CheckCircle2,
  Fingerprint,
  BarChart3,
  GitBranch,
  RefreshCw,
  Download,
  Copy,
  Check,
  Share2,
  GitCompare,
  Square,
  CheckSquare,
  Eye,
  ArrowRight,
} from "lucide-react";
import type { CooccurrenceExport, ElementFingerprint } from "../../types/ui-bridge-types";
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

/** Result from fingerprint state discovery */
export interface FingerprintDiscoveryResult {
  states: Array<{
    stateId: string;
    name: string;
    fingerprintHashes: string[];
    elementIds?: string[];
    positionZone: string;
    landmarkContext?: string;
    isGlobal: boolean;
    isModal: boolean;
    repeatPatternCount?: number;
    confidence: number;
    observationCount?: number;
  }>;
  transitions: Array<{
    fromStateId: string;
    toStateId: string;
    actionType: string;
    count: number;
  }>;
  statistics: {
    totalCaptures: number;
    totalTransitions: number;
    uniqueFingerprints: number;
    discoveredStates: number;
    globalStates: number;
    modalStates: number;
    discoveredTransitions: number;
  };
}

interface StateDiscoveryPanelProps {
  cooccurrenceData: CooccurrenceExport | null;
  isLoading: boolean;
  sessionActive: boolean;
  captureCount: number;
  onRefresh: () => Promise<CooccurrenceExport | null>;
  onSelectFingerprint?: (hash: string) => void;
  disabled?: boolean;
  /** Run actual state discovery through Python library */
  onRunDiscovery?: (data: CooccurrenceExport) => Promise<FingerprintDiscoveryResult | null>;
  /** Whether discovery is in progress */
  isRunningDiscovery?: boolean;
  /** Discovery results from Python library */
  discoveryResult?: FingerprintDiscoveryResult | null;
}

type ViewMode = "states" | "graph" | "transitions" | "matrix" | "stats";

// Position zone colors for visual distinction
const ZONE_COLORS: Record<string, string> = {
  header: "bg-blue-500/20 text-blue-400 border-blue-500/30",
  footer: "bg-purple-500/20 text-purple-400 border-purple-500/30",
  "sidebar-left": "bg-green-500/20 text-green-400 border-green-500/30",
  "sidebar-right": "bg-green-500/20 text-green-400 border-green-500/30",
  main: "bg-zinc-500/20 text-zinc-400 border-zinc-500/30",
  modal: "bg-orange-500/20 text-orange-400 border-orange-500/30",
};

// Size category labels
const SIZE_LABELS: Record<string, string> = {
  icon: "Icon",
  button: "Button",
  small: "Small",
  medium: "Medium",
  large: "Large",
  fullwidth: "Full Width",
  panel: "Panel",
};

function StateCandidateCard({
  candidate,
  fingerprintDetails,
  index,
  expanded,
  onToggle,
  onSelectFingerprint,
}: {
  candidate: { fingerprints: string[]; cooccurrenceRate: number };
  fingerprintDetails: Record<string, ElementFingerprint>;
  index: number;
  expanded: boolean;
  onToggle: () => void;
  onSelectFingerprint?: (hash: string) => void;
}) {
  // Analyze the state composition
  const analysis = useMemo(() => {
    const zones: Record<string, number> = {};
    const landmarks: Record<string, number> = {};
    const sizes: Record<string, number> = {};
    let repeatingCount = 0;

    candidate.fingerprints.forEach((hash) => {
      const fp = fingerprintDetails[hash];
      if (!fp) return;

      zones[fp.positionZone] = (zones[fp.positionZone] || 0) + 1;
      landmarks[fp.landmarkContext] = (landmarks[fp.landmarkContext] || 0) + 1;
      sizes[fp.sizeCategory] = (sizes[fp.sizeCategory] || 0) + 1;
      if (fp.isRepeating) repeatingCount++;
    });

    // Determine dominant characteristics
    const dominantZone = Object.entries(zones).sort((a, b) => b[1] - a[1])[0]?.[0] || "unknown";
    const dominantLandmark =
      Object.entries(landmarks).sort((a, b) => b[1] - a[1])[0]?.[0] || "unknown";

    // Classify state type
    let stateType: "global" | "modal" | "content" | "mixed" = "content";
    if (dominantZone === "header" || dominantZone === "footer") {
      stateType = "global";
    } else if (dominantZone === "modal") {
      stateType = "modal";
    } else if (Object.keys(zones).length > 2) {
      stateType = "mixed";
    }

    return {
      zones,
      landmarks,
      sizes,
      repeatingCount,
      dominantZone,
      dominantLandmark,
      stateType,
    };
  }, [candidate.fingerprints, fingerprintDetails]);

  const stateTypeColors: Record<string, string> = {
    global: "bg-blue-500/20 text-blue-400",
    modal: "bg-orange-500/20 text-orange-400",
    content: "bg-green-500/20 text-green-400",
    mixed: "bg-purple-500/20 text-purple-400",
  };

  return (
    <div className="border border-border/50 rounded-lg overflow-hidden">
      {/* Header */}
      <button
        className="w-full flex items-center gap-3 p-3 hover:bg-muted/30 transition-colors text-left"
        onClick={onToggle}
      >
        {expanded ? (
          <ChevronDown className="w-4 h-4 text-muted-foreground flex-shrink-0" />
        ) : (
          <ChevronRight className="w-4 h-4 text-muted-foreground flex-shrink-0" />
        )}

        <div className="flex items-center gap-2 flex-1 min-w-0">
          <Badge variant="default" className="font-mono">
            State {index + 1}
          </Badge>
          <Badge className={stateTypeColors[analysis.stateType]}>{analysis.stateType}</Badge>
          <span className="text-xs text-muted-foreground">
            {candidate.fingerprints.length} elements
          </span>
        </div>

        <div className="flex items-center gap-2 flex-shrink-0">
          {analysis.repeatingCount > 0 && (
            <Badge variant="info" size="sm" title={`${analysis.repeatingCount} repeating elements`}>
              <Repeat className="w-3 h-3 mr-1" />
              {analysis.repeatingCount}
            </Badge>
          )}
          <Badge variant={candidate.cooccurrenceRate === 1 ? "success" : "warning"} size="sm">
            {(candidate.cooccurrenceRate * 100).toFixed(0)}% co-occur
          </Badge>
        </div>
      </button>

      {/* Expanded Content */}
      {expanded && (
        <div className="border-t border-border/50 p-3 space-y-3 bg-muted/10">
          {/* Zone Distribution */}
          <div className="space-y-1">
            <div className="text-xs font-medium text-muted-foreground">Position Zones</div>
            <div className="flex flex-wrap gap-1">
              {Object.entries(analysis.zones).map(([zone, count]) => (
                <Badge key={zone} className={`${ZONE_COLORS[zone] || "bg-muted"} border`} size="sm">
                  <MapPin className="w-3 h-3 mr-1" />
                  {zone}: {count}
                </Badge>
              ))}
            </div>
          </div>

          {/* Landmark Distribution */}
          <div className="space-y-1">
            <div className="text-xs font-medium text-muted-foreground">Landmarks</div>
            <div className="flex flex-wrap gap-1">
              {Object.entries(analysis.landmarks).map(([landmark, count]) => (
                <Badge key={landmark} variant="purple" size="sm">
                  {landmark}: {count}
                </Badge>
              ))}
            </div>
          </div>

          {/* Size Distribution */}
          <div className="space-y-1">
            <div className="text-xs font-medium text-muted-foreground">Size Categories</div>
            <div className="flex flex-wrap gap-1">
              {Object.entries(analysis.sizes).map(([size, count]) => (
                <Badge key={size} variant="muted" size="sm">
                  {SIZE_LABELS[size] || size}: {count}
                </Badge>
              ))}
            </div>
          </div>

          {/* Element List */}
          <div className="space-y-1">
            <div className="text-xs font-medium text-muted-foreground">Elements</div>
            <div className="max-h-48 overflow-y-auto space-y-1">
              {candidate.fingerprints.map((hash) => {
                const fp = fingerprintDetails[hash];
                if (!fp) return null;

                return (
                  <button
                    key={hash}
                    className="w-full flex items-center gap-2 p-2 text-xs bg-muted/30 rounded hover:bg-muted/50 transition-colors text-left"
                    onClick={() => onSelectFingerprint?.(hash)}
                  >
                    <Hash className="w-3 h-3 text-muted-foreground flex-shrink-0" />
                    <span className="font-mono text-[10px] text-muted-foreground">{hash}</span>
                    <Badge variant="muted" size="sm">
                      {fp.role || fp.tagName}
                    </Badge>
                    {fp.accessibleName && (
                      <span className="truncate text-muted-foreground">"{fp.accessibleName}"</span>
                    )}
                    {fp.isRepeating && <Repeat className="w-3 h-3 text-blue-400 flex-shrink-0" />}
                  </button>
                );
              })}
            </div>
          </div>
        </div>
      )}
    </div>
  );
}

function TransitionCard({
  transition,
  fingerprintDetails,
  onSelectFingerprint,
}: {
  transition: {
    actionId: string;
    actionType: string;
    targetFingerprint: string;
    beforeCaptureId: string;
    afterCaptureId: string;
    appearedFingerprints: string[];
    disappearedFingerprints: string[];
    timestamp: number;
  };
  fingerprintDetails: Record<string, ElementFingerprint>;
  onSelectFingerprint?: (hash: string) => void;
}) {
  const [expanded, setExpanded] = useState(false);
  const targetFp = transition.targetFingerprint
    ? fingerprintDetails[transition.targetFingerprint]
    : null;

  return (
    <div className="border border-border/50 rounded-lg overflow-hidden">
      <button
        className="w-full flex items-center gap-3 p-3 hover:bg-muted/30 transition-colors text-left"
        onClick={() => setExpanded(!expanded)}
      >
        {expanded ? (
          <ChevronDown className="w-4 h-4 text-muted-foreground flex-shrink-0" />
        ) : (
          <ChevronRight className="w-4 h-4 text-muted-foreground flex-shrink-0" />
        )}

        <Badge variant="info">{transition.actionType}</Badge>

        {targetFp && (
          <span className="text-xs text-muted-foreground truncate">
            on {targetFp.role || targetFp.tagName}
            {targetFp.accessibleName && ` "${targetFp.accessibleName}"`}
          </span>
        )}

        <div className="flex items-center gap-2 ml-auto flex-shrink-0">
          {transition.appearedFingerprints.length > 0 && (
            <Badge variant="success" size="sm">
              +{transition.appearedFingerprints.length}
            </Badge>
          )}
          {transition.disappearedFingerprints.length > 0 && (
            <Badge variant="danger" size="sm">
              -{transition.disappearedFingerprints.length}
            </Badge>
          )}
        </div>
      </button>

      {expanded && (
        <div className="border-t border-border/50 p-3 space-y-3 bg-muted/10">
          {/* Target Element */}
          {targetFp && (
            <div className="space-y-1">
              <div className="text-xs font-medium text-muted-foreground">Target Element</div>
              <button
                className="w-full flex items-center gap-2 p-2 bg-muted/30 rounded text-xs hover:bg-muted/50 transition-colors text-left"
                onClick={(e) => {
                  e.stopPropagation();
                  onSelectFingerprint?.(transition.targetFingerprint);
                }}
              >
                <Fingerprint className="w-3 h-3 text-muted-foreground" />
                <span className="font-mono text-[10px]">{transition.targetFingerprint}</span>
                <Badge variant="muted" size="sm">
                  {targetFp.role}
                </Badge>
                {targetFp.accessibleName && (
                  <span className="truncate">"{targetFp.accessibleName}"</span>
                )}
              </button>
            </div>
          )}

          {/* Appeared Elements */}
          {transition.appearedFingerprints.length > 0 && (
            <div className="space-y-1">
              <div className="text-xs font-medium text-green-400 flex items-center gap-1">
                <CheckCircle2 className="w-3 h-3" />
                Appeared ({transition.appearedFingerprints.length})
              </div>
              <div className="max-h-32 overflow-y-auto space-y-1">
                {transition.appearedFingerprints.slice(0, 10).map((hash) => {
                  const fp = fingerprintDetails[hash];
                  return (
                    <button
                      key={hash}
                      className="w-full flex items-center gap-2 p-1.5 bg-green-500/10 rounded text-xs hover:bg-green-500/20 transition-colors text-left"
                      onClick={(e) => {
                        e.stopPropagation();
                        onSelectFingerprint?.(hash);
                      }}
                    >
                      <span className="font-mono text-[10px] text-muted-foreground">{hash}</span>
                      {fp && (
                        <>
                          <Badge variant="muted" size="sm">
                            {fp.role || fp.tagName}
                          </Badge>
                          {fp.accessibleName && (
                            <span className="truncate text-muted-foreground">
                              "{fp.accessibleName}"
                            </span>
                          )}
                        </>
                      )}
                    </button>
                  );
                })}
                {transition.appearedFingerprints.length > 10 && (
                  <div className="text-xs text-muted-foreground pl-2">
                    +{transition.appearedFingerprints.length - 10} more
                  </div>
                )}
              </div>
            </div>
          )}

          {/* Disappeared Elements */}
          {transition.disappearedFingerprints.length > 0 && (
            <div className="space-y-1">
              <div className="text-xs font-medium text-red-400 flex items-center gap-1">
                <AlertCircle className="w-3 h-3" />
                Disappeared ({transition.disappearedFingerprints.length})
              </div>
              <div className="max-h-32 overflow-y-auto space-y-1">
                {transition.disappearedFingerprints.slice(0, 10).map((hash) => {
                  const fp = fingerprintDetails[hash];
                  return (
                    <button
                      key={hash}
                      className="w-full flex items-center gap-2 p-1.5 bg-red-500/10 rounded text-xs hover:bg-red-500/20 transition-colors text-left"
                      onClick={(e) => {
                        e.stopPropagation();
                        onSelectFingerprint?.(hash);
                      }}
                    >
                      <span className="font-mono text-[10px] text-muted-foreground">{hash}</span>
                      {fp && (
                        <>
                          <Badge variant="muted" size="sm">
                            {fp.role || fp.tagName}
                          </Badge>
                          {fp.accessibleName && (
                            <span className="truncate text-muted-foreground">
                              "{fp.accessibleName}"
                            </span>
                          )}
                        </>
                      )}
                    </button>
                  );
                })}
                {transition.disappearedFingerprints.length > 10 && (
                  <div className="text-xs text-muted-foreground pl-2">
                    +{transition.disappearedFingerprints.length - 10} more
                  </div>
                )}
              </div>
            </div>
          )}

          {/* Timestamp */}
          <div className="text-[10px] text-muted-foreground">
            {new Date(transition.timestamp).toLocaleTimeString()}
          </div>
        </div>
      )}
    </div>
  );
}

/**
 * Card for displaying a discovered state from the Python library analysis
 */
function DiscoveredStateCard({
  state,
  fingerprintDetails,
  expanded,
  onToggle,
  onSelectFingerprint,
  compareMode,
  isSelected,
  onToggleSelect,
  onShowInGraph,
  isHighlighted,
  cardRef,
}: {
  state: FingerprintDiscoveryResult["states"][0];
  fingerprintDetails: Record<string, ElementFingerprint>;
  expanded: boolean;
  onToggle: () => void;
  onSelectFingerprint?: (hash: string) => void;
  /** Whether compare mode is active */
  compareMode?: boolean;
  /** Whether this state is selected for comparison */
  isSelected?: boolean;
  /** Toggle selection for comparison */
  onToggleSelect?: () => void;
  /** Callback to show this state in the graph view */
  onShowInGraph?: (stateId: string) => void;
  /** Whether this state is highlighted (from navigation) */
  isHighlighted?: boolean;
  /** Ref for scrolling into view */
  cardRef?: React.RefObject<HTMLDivElement | null>;
}) {
  const stateTypeColors: Record<string, string> = {
    global: "bg-blue-500/20 text-blue-400",
    modal: "bg-orange-500/20 text-orange-400",
    content: "bg-green-500/20 text-green-400",
  };

  const stateType = state.isGlobal ? "global" : state.isModal ? "modal" : "content";
  const confidenceColor =
    state.confidence >= 0.9
      ? "text-green-400"
      : state.confidence >= 0.7
        ? "text-yellow-400"
        : "text-red-400";

  const handleClick = () => {
    if (compareMode && onToggleSelect) {
      onToggleSelect();
    } else {
      onToggle();
    }
  };

  return (
    <div
      ref={cardRef}
      className={`border rounded-lg overflow-hidden transition-colors ${
        isHighlighted
          ? "border-primary ring-2 ring-primary/30 bg-primary/5"
          : compareMode && isSelected
            ? "border-primary bg-primary/5"
            : "border-border/50"
      }`}
    >
      {/* Header */}
      <button
        className="w-full flex items-center gap-3 p-3 hover:bg-muted/30 transition-colors text-left"
        onClick={handleClick}
      >
        {/* Compare mode checkbox or expand chevron */}
        {compareMode ? (
          <div
            className="flex-shrink-0"
            onClick={(e) => {
              e.stopPropagation();
              onToggleSelect?.();
            }}
          >
            {isSelected ? (
              <CheckSquare className="w-4 h-4 text-primary" />
            ) : (
              <Square className="w-4 h-4 text-muted-foreground" />
            )}
          </div>
        ) : expanded ? (
          <ChevronDown className="w-4 h-4 text-muted-foreground flex-shrink-0" />
        ) : (
          <ChevronRight className="w-4 h-4 text-muted-foreground flex-shrink-0" />
        )}

        <div className="flex items-center gap-2 flex-1 min-w-0">
          <CheckCircle2 className="w-4 h-4 text-green-500" />
          <span className="font-medium text-sm truncate">{state.name}</span>
          <Badge className={stateTypeColors[stateType]}>{stateType}</Badge>
          <Badge className={ZONE_COLORS[state.positionZone] || ZONE_COLORS.main}>
            {state.positionZone}
          </Badge>
        </div>

        <div className="flex items-center gap-2 flex-shrink-0">
          <span className={`text-xs font-mono ${confidenceColor}`}>
            {(state.confidence * 100).toFixed(0)}%
          </span>
          <Badge variant="muted" size="sm">
            {state.fingerprintHashes.length} elements
          </Badge>
        </div>
      </button>

      {/* Expanded Content - only show when not in compare mode */}
      {expanded && !compareMode && (
        <div className="p-3 pt-0 space-y-3 border-t border-border/30">
          {/* Navigation button */}
          {onShowInGraph && (
            <div className="flex gap-2">
              <Button
                variant="outline"
                size="sm"
                onClick={(e) => {
                  e.stopPropagation();
                  onShowInGraph(state.stateId);
                }}
                className="text-xs"
              >
                <Share2 className="w-3 h-3 mr-1" />
                Show in Graph
              </Button>
            </div>
          )}

          {/* State metadata */}
          <div className="flex flex-wrap gap-2 text-xs">
            {state.landmarkContext && (
              <Badge variant="purple" size="sm">
                <MapPin className="w-3 h-3 mr-1" />
                {state.landmarkContext}
              </Badge>
            )}
            {state.repeatPatternCount && state.repeatPatternCount > 0 && (
              <Badge variant="muted" size="sm">
                <Repeat className="w-3 h-3 mr-1" />
                {state.repeatPatternCount} repeating
              </Badge>
            )}
            {state.observationCount && (
              <Badge variant="muted" size="sm">
                Observed {state.observationCount}x
              </Badge>
            )}
          </div>

          {/* Elements in this state */}
          <div className="space-y-1">
            <div className="text-xs font-medium flex items-center gap-1">
              <Hash className="w-3 h-3" />
              Fingerprints ({state.fingerprintHashes.length})
            </div>
            <div className="max-h-48 overflow-y-auto space-y-1">
              {state.fingerprintHashes.map((hash) => {
                const fp = fingerprintDetails[hash];
                return (
                  <button
                    key={hash}
                    className="w-full flex items-center gap-2 p-1.5 bg-muted/30 rounded text-xs hover:bg-muted/50 transition-colors text-left"
                    onClick={(e) => {
                      e.stopPropagation();
                      onSelectFingerprint?.(hash);
                    }}
                  >
                    <Fingerprint className="w-3 h-3 text-muted-foreground flex-shrink-0" />
                    <span className="font-mono text-[10px] text-muted-foreground">
                      {hash.slice(0, 12)}...
                    </span>
                    {fp && (
                      <>
                        <Badge variant="muted" size="sm">
                          {fp.role || fp.tagName}
                        </Badge>
                        {fp.accessibleName && (
                          <span className="truncate text-muted-foreground">
                            "{fp.accessibleName}"
                          </span>
                        )}
                      </>
                    )}
                  </button>
                );
              })}
            </div>
          </div>
        </div>
      )}
    </div>
  );
}

/**
 * Card for displaying discovered transitions with navigation to connected states
 */
function DiscoveredTransitionCard({
  transition,
  discoveryResult,
  isHighlighted,
  onNavigateToState,
  onShowInGraph,
}: {
  transition: FingerprintDiscoveryResult["transitions"][0];
  discoveryResult: FingerprintDiscoveryResult;
  isHighlighted?: boolean;
  onNavigateToState?: (stateId: string, stateName: string) => void;
  onShowInGraph?: (fromStateId: string, toStateId: string) => void;
}) {
  const [expanded, setExpanded] = useState(false);

  // Find state names
  const fromState = discoveryResult.states.find((s) => s.stateId === transition.fromStateId);
  const toState = discoveryResult.states.find((s) => s.stateId === transition.toStateId);

  return (
    <div
      className={`border rounded-lg overflow-hidden transition-colors ${
        isHighlighted ? "border-primary ring-2 ring-primary/30 bg-primary/5" : "border-border/50"
      }`}
    >
      <button
        className="w-full flex items-center gap-3 p-3 hover:bg-muted/30 transition-colors text-left"
        onClick={() => setExpanded(!expanded)}
      >
        {expanded ? (
          <ChevronDown className="w-4 h-4 text-muted-foreground flex-shrink-0" />
        ) : (
          <ChevronRight className="w-4 h-4 text-muted-foreground flex-shrink-0" />
        )}

        <Badge variant="info">{transition.actionType}</Badge>

        <div className="flex items-center gap-1 text-xs text-muted-foreground flex-1 min-w-0">
          <span className="truncate">{fromState?.name || transition.fromStateId}</span>
          <ArrowRight className="w-3 h-3 flex-shrink-0" />
          <span className="truncate">{toState?.name || transition.toStateId}</span>
        </div>

        <Badge variant="muted" size="sm">
          {transition.count}x
        </Badge>
      </button>

      {expanded && (
        <div className="border-t border-border/50 p-3 space-y-3 bg-muted/10">
          {/* Navigation buttons */}
          <div className="flex flex-wrap gap-2">
            {onNavigateToState && fromState && (
              <Button
                variant="outline"
                size="sm"
                onClick={(e) => {
                  e.stopPropagation();
                  onNavigateToState(fromState.stateId, fromState.name);
                }}
                className="text-xs"
              >
                <Eye className="w-3 h-3 mr-1" />
                From: {fromState.name}
              </Button>
            )}
            {onNavigateToState && toState && (
              <Button
                variant="outline"
                size="sm"
                onClick={(e) => {
                  e.stopPropagation();
                  onNavigateToState(toState.stateId, toState.name);
                }}
                className="text-xs"
              >
                <Eye className="w-3 h-3 mr-1" />
                To: {toState.name}
              </Button>
            )}
            {onShowInGraph && (
              <Button
                variant="outline"
                size="sm"
                onClick={(e) => {
                  e.stopPropagation();
                  onShowInGraph(transition.fromStateId, transition.toStateId);
                }}
                className="text-xs"
              >
                <Share2 className="w-3 h-3 mr-1" />
                Show in Graph
              </Button>
            )}
          </div>

          {/* Transition details */}
          <div className="text-xs space-y-1">
            <div className="flex items-center gap-2">
              <span className="text-muted-foreground">Action:</span>
              <Badge variant="info" size="sm">
                {transition.actionType}
              </Badge>
            </div>
            <div className="flex items-center gap-2">
              <span className="text-muted-foreground">Occurrences:</span>
              <span>{transition.count}</span>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}

function StatsView({ data }: { data: CooccurrenceExport }) {
  const stats = useMemo(() => {
    const zoneDistribution: Record<string, number> = {};
    const landmarkDistribution: Record<string, number> = {};
    const sizeDistribution: Record<string, number> = {};
    let repeatingCount = 0;
    let totalAppearances = 0;

    Object.values(data.fingerprintDetails).forEach((fp) => {
      zoneDistribution[fp.positionZone] = (zoneDistribution[fp.positionZone] || 0) + 1;
      landmarkDistribution[fp.landmarkContext] =
        (landmarkDistribution[fp.landmarkContext] || 0) + 1;
      sizeDistribution[fp.sizeCategory] = (sizeDistribution[fp.sizeCategory] || 0) + 1;
      if (fp.isRepeating) repeatingCount++;
    });

    Object.values(data.fingerprintStats).forEach((stat) => {
      totalAppearances += stat.totalAppearances;
    });

    return {
      totalFingerprints: data.allFingerprints.length,
      totalCaptures: data.presenceMatrix.length,
      totalTransitions: data.transitions.length,
      stateCandidates: data.stateCandidates.length,
      repeatingCount,
      avgAppearances:
        data.allFingerprints.length > 0
          ? (totalAppearances / data.allFingerprints.length).toFixed(1)
          : "0",
      zoneDistribution,
      landmarkDistribution,
      sizeDistribution,
    };
  }, [data]);

  return (
    <div className="space-y-4">
      {/* Summary Stats */}
      <div className="grid grid-cols-2 md:grid-cols-4 gap-3">
        <div className="p-3 bg-muted/30 rounded-lg">
          <div className="text-2xl font-bold">{stats.totalFingerprints}</div>
          <div className="text-xs text-muted-foreground">Unique Fingerprints</div>
        </div>
        <div className="p-3 bg-muted/30 rounded-lg">
          <div className="text-2xl font-bold">{stats.totalCaptures}</div>
          <div className="text-xs text-muted-foreground">Captures</div>
        </div>
        <div className="p-3 bg-muted/30 rounded-lg">
          <div className="text-2xl font-bold">{stats.stateCandidates}</div>
          <div className="text-xs text-muted-foreground">State Candidates</div>
        </div>
        <div className="p-3 bg-muted/30 rounded-lg">
          <div className="text-2xl font-bold">{stats.totalTransitions}</div>
          <div className="text-xs text-muted-foreground">Transitions</div>
        </div>
      </div>

      {/* Additional Stats */}
      <div className="grid grid-cols-2 gap-3">
        <div className="p-3 bg-muted/30 rounded-lg">
          <div className="text-lg font-bold">{stats.repeatingCount}</div>
          <div className="text-xs text-muted-foreground">Repeating Elements</div>
        </div>
        <div className="p-3 bg-muted/30 rounded-lg">
          <div className="text-lg font-bold">{stats.avgAppearances}</div>
          <div className="text-xs text-muted-foreground">Avg Appearances/Element</div>
        </div>
      </div>

      {/* Zone Distribution */}
      <div className="space-y-2">
        <div className="text-sm font-medium">Position Zone Distribution</div>
        <div className="space-y-1">
          {Object.entries(stats.zoneDistribution)
            .sort((a, b) => b[1] - a[1])
            .map(([zone, count]) => {
              const percentage = ((count / stats.totalFingerprints) * 100).toFixed(0);
              return (
                <div key={zone} className="flex items-center gap-2">
                  <div className="w-24 text-xs text-muted-foreground">{zone}</div>
                  <div className="flex-1 h-4 bg-muted/30 rounded overflow-hidden">
                    <div
                      className={`h-full ${ZONE_COLORS[zone]?.split(" ")[0] || "bg-primary/50"}`}
                      style={{ width: `${percentage}%` }}
                    />
                  </div>
                  <div className="w-12 text-xs text-right">{count}</div>
                </div>
              );
            })}
        </div>
      </div>

      {/* Size Distribution */}
      <div className="space-y-2">
        <div className="text-sm font-medium">Size Category Distribution</div>
        <div className="space-y-1">
          {Object.entries(stats.sizeDistribution)
            .sort((a, b) => b[1] - a[1])
            .map(([size, count]) => {
              const percentage = ((count / stats.totalFingerprints) * 100).toFixed(0);
              return (
                <div key={size} className="flex items-center gap-2">
                  <div className="w-24 text-xs text-muted-foreground">
                    {SIZE_LABELS[size] || size}
                  </div>
                  <div className="flex-1 h-4 bg-muted/30 rounded overflow-hidden">
                    <div className="h-full bg-primary/50" style={{ width: `${percentage}%` }} />
                  </div>
                  <div className="w-12 text-xs text-right">{count}</div>
                </div>
              );
            })}
        </div>
      </div>

      {/* Landmark Distribution */}
      <div className="space-y-2">
        <div className="text-sm font-medium">Landmark Distribution</div>
        <div className="flex flex-wrap gap-2">
          {Object.entries(stats.landmarkDistribution)
            .sort((a, b) => b[1] - a[1])
            .map(([landmark, count]) => (
              <Badge key={landmark} variant="purple">
                {landmark}: {count}
              </Badge>
            ))}
        </div>
      </div>
    </div>
  );
}

function CooccurrenceMatrixView({ data }: { data: CooccurrenceExport }) {
  // For large matrices, show a summary instead of full matrix
  const matrixSize = data.allFingerprints.length;
  const showFullMatrix = matrixSize <= 20;

  if (!showFullMatrix) {
    return (
      <div className="space-y-4">
        <div className="flex items-center gap-2 text-yellow-500">
          <AlertCircle className="w-4 h-4" />
          <span className="text-sm">
            Matrix too large to display ({matrixSize}x{matrixSize}). Showing summary instead.
          </span>
        </div>

        {/* Summary of high co-occurrence pairs */}
        <div className="space-y-2">
          <div className="text-sm font-medium">High Co-occurrence Pairs</div>
          <div className="text-xs text-muted-foreground mb-2">
            Fingerprint pairs that always appear together (100% co-occurrence)
          </div>
          <div className="max-h-64 overflow-y-auto space-y-1">
            {data.stateCandidates.slice(0, 10).map((candidate, i) => (
              <div key={i} className="p-2 bg-muted/30 rounded text-xs flex items-center gap-2">
                <Badge variant="success" size="sm">
                  {candidate.fingerprints.length} elements
                </Badge>
                <span className="text-muted-foreground">always appear together</span>
              </div>
            ))}
          </div>
        </div>
      </div>
    );
  }

  // Render actual matrix for small datasets
  return (
    <div className="space-y-4">
      <div className="text-sm font-medium">Co-occurrence Matrix</div>
      <div className="text-xs text-muted-foreground">
        Shows how often fingerprint pairs appear together (normalized 0-1)
      </div>
      <div className="overflow-auto max-h-96">
        <table className="text-[10px] border-collapse">
          <thead>
            <tr>
              <th className="p-1" />
              {data.allFingerprints.slice(0, 20).map((fp, i) => (
                <th
                  key={fp}
                  className="p-1 font-mono text-muted-foreground rotate-45 origin-left"
                  title={fp}
                >
                  {i + 1}
                </th>
              ))}
            </tr>
          </thead>
          <tbody>
            {data.allFingerprints.slice(0, 20).map((fp1, i) => (
              <tr key={fp1}>
                <td className="p-1 font-mono text-muted-foreground" title={fp1}>
                  {i + 1}
                </td>
                {data.allFingerprints.slice(0, 20).map((fp2) => {
                  const count1 = data.cooccurrenceCounts[fp1]?.[fp2] || 0;
                  const total1 = data.fingerprintStats[fp1]?.totalAppearances || 1;
                  const rate = count1 / total1;

                  // Color based on rate
                  const bgColor =
                    fp1 === fp2
                      ? "bg-primary/30"
                      : rate > 0.9
                        ? "bg-green-500/50"
                        : rate > 0.5
                          ? "bg-yellow-500/30"
                          : rate > 0
                            ? "bg-red-500/20"
                            : "";

                  return (
                    <td
                      key={fp2}
                      className={`p-1 text-center border border-border/20 ${bgColor}`}
                      title={`${fp1} + ${fp2}: ${(rate * 100).toFixed(0)}%`}
                    >
                      {rate > 0 ? (rate * 100).toFixed(0) : ""}
                    </td>
                  );
                })}
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </div>
  );
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
  const [viewMode, setViewMode] = useState<ViewMode>("states");
  const [expandedStates, setExpandedStates] = useState<Set<number>>(new Set());
  const [expandedDiscoveredStates, setExpandedDiscoveredStates] = useState<Set<string>>(new Set());
  const [copied, setCopied] = useState(false);
  const [showExportDialog, setShowExportDialog] = useState(false);

  // Toggle for showing discovered states vs raw state candidates
  const [showDiscoveredStates, setShowDiscoveredStates] = useState(false);

  // Compare mode state
  const [compareMode, setCompareMode] = useState(false);
  const [selectedForComparison, setSelectedForComparison] = useState<Set<string>>(new Set());
  const [showComparisonView, setShowComparisonView] = useState(false);

  // Navigation state for cross-view navigation
  const [navigationState, setNavigationState] = useState<NavigationState>(
    createInitialNavigationState,
  );
  const [showFingerprintPanel, setShowFingerprintPanel] = useState(false);

  // Refs for scrolling to elements
  const stateCardRefs = useRef<Map<string, HTMLDivElement>>(new Map());

  // Get the selected states for comparison
  const selectedStates = useMemo(() => {
    if (!discoveryResult) return [];
    return discoveryResult.states.filter((s) => selectedForComparison.has(s.stateId));
  }, [discoveryResult, selectedForComparison]);

  // Navigation handlers
  const navigateToState = useCallback((stateId: string, stateName: string) => {
    setNavigationState((prev) => ({
      ...prev,
      selectedStateId: stateId,
      selectedTransitionId: null,
      selectedFingerprintId: null,
      history: addToHistory(prev.history, { type: "state", id: stateId, label: stateName }),
    }));
    setViewMode("states");
    setShowDiscoveredStates(true);
    setExpandedDiscoveredStates((prev) => new Set([...prev, stateId]));

    // Scroll to the state card after a short delay
    setTimeout(() => {
      const cardElement = stateCardRefs.current.get(stateId);
      cardElement?.scrollIntoView({ behavior: "smooth", block: "center" });
    }, 100);
  }, []);

  const navigateToTransition = useCallback(
    (fromStateId: string, toStateId: string, actionType?: string) => {
      const transitionKey = createTransitionKey(fromStateId, toStateId);
      const label = actionType ? `${actionType}: ${fromStateId} -> ${toStateId}` : transitionKey;
      setNavigationState((prev) => ({
        ...prev,
        selectedStateId: null,
        selectedTransitionId: transitionKey,
        selectedFingerprintId: null,
        history: addToHistory(prev.history, { type: "transition", id: transitionKey, label }),
      }));
      setViewMode("graph"); // Show in graph to see the transition
    },
    [],
  );

  const navigateToFingerprint = useCallback(
    (hash: string, label?: string) => {
      const displayLabel = label || hash.slice(0, 12) + "...";
      setNavigationState((prev) => ({
        ...prev,
        selectedStateId: null,
        selectedTransitionId: null,
        selectedFingerprintId: hash,
        history: addToHistory(prev.history, { type: "fingerprint", id: hash, label: displayLabel }),
      }));
      setShowFingerprintPanel(true);
      // Also call the parent callback if provided
      onSelectFingerprint?.(hash);
    },
    [onSelectFingerprint],
  );

  const handleBreadcrumbNavigate = useCallback(
    (item: NavigationItem) => {
      switch (item.type) {
        case "graph":
          setViewMode("graph");
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
    setNavigationState(createInitialNavigationState());
    setShowFingerprintPanel(false);
  }, []);

  // Handle showing state in graph
  const handleShowStateInGraph = useCallback((stateId: string) => {
    setNavigationState((prev) => ({
      ...prev,
      selectedStateId: stateId,
      selectedTransitionId: null,
    }));
    setViewMode("graph");
  }, []);

  // Handle showing transition in graph
  const handleShowTransitionInGraph = useCallback((fromStateId: string, toStateId: string) => {
    const transitionKey = createTransitionKey(fromStateId, toStateId);
    setNavigationState((prev) => ({
      ...prev,
      selectedTransitionId: transitionKey,
      selectedStateId: null,
    }));
    setViewMode("graph");
  }, []);

  // Handle graph node click - navigate to state
  const handleGraphNodeClick = useCallback(
    (stateId: string | null) => {
      if (stateId) {
        const state = discoveryResult?.states.find((s) => s.stateId === stateId);
        if (state) {
          navigateToState(stateId, state.name);
        }
      } else {
        // Clicked on empty space, clear selection
        setNavigationState((prev) => ({
          ...prev,
          selectedStateId: null,
          selectedTransitionId: null,
        }));
      }
    },
    [discoveryResult, navigateToState],
  );

  // Handle graph edge click - navigate to transition
  const handleGraphEdgeClick = useCallback(
    (fromStateId: string, toStateId: string) => {
      navigateToTransition(fromStateId, toStateId);
    },
    [navigateToTransition],
  );

  const toggleDiscoveredStateExpanded = (stateId: string) => {
    setExpandedDiscoveredStates((prev) => {
      const next = new Set(prev);
      if (next.has(stateId)) {
        next.delete(stateId);
      } else {
        next.add(stateId);
      }
      return next;
    });
  };

  // Toggle state selection for comparison
  const toggleStateForComparison = (stateId: string) => {
    setSelectedForComparison((prev) => {
      const next = new Set(prev);
      if (next.has(stateId)) {
        next.delete(stateId);
      } else {
        // Only allow selecting up to 2 states
        if (next.size < 2) {
          next.add(stateId);
        }
      }
      return next;
    });
  };

  // Exit compare mode and reset selection
  const exitCompareMode = () => {
    setCompareMode(false);
    setSelectedForComparison(new Set());
    setShowComparisonView(false);
  };

  // Open comparison view
  const openComparisonView = () => {
    if (selectedStates.length === 2) {
      setShowComparisonView(true);
    }
  };

  const handleRunDiscovery = async () => {
    if (!cooccurrenceData || !onRunDiscovery) return;
    const result = await onRunDiscovery(cooccurrenceData);
    if (result) {
      setShowDiscoveredStates(true);
    }
  };

  const toggleStateExpanded = (index: number) => {
    setExpandedStates((prev) => {
      const next = new Set(prev);
      if (next.has(index)) {
        next.delete(index);
      } else {
        next.add(index);
      }
      return next;
    });
  };

  const handleCopyData = async () => {
    if (!cooccurrenceData) return;
    try {
      await navigator.clipboard.writeText(JSON.stringify(cooccurrenceData, null, 2));
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
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

  return (
    <div className="h-full flex flex-col">
      {/* Toolbar */}
      <div className="flex items-center justify-between mb-3">
        <div className="flex gap-1 p-1 bg-muted/30 rounded-lg">
          <button
            onClick={() => setViewMode("states")}
            className={`flex items-center gap-1.5 px-3 py-1.5 text-xs font-medium rounded-md transition-colors ${
              viewMode === "states"
                ? "bg-primary text-primary-foreground"
                : "text-muted-foreground hover:text-foreground hover:bg-muted/50"
            }`}
          >
            <Layers className="w-3.5 h-3.5" />
            States
            {cooccurrenceData && (
              <Badge variant={viewMode === "states" ? "default" : "muted"} size="sm">
                {cooccurrenceData.stateCandidates.length}
              </Badge>
            )}
          </button>
          <button
            onClick={() => setViewMode("graph")}
            className={`flex items-center gap-1.5 px-3 py-1.5 text-xs font-medium rounded-md transition-colors ${
              viewMode === "graph"
                ? "bg-primary text-primary-foreground"
                : "text-muted-foreground hover:text-foreground hover:bg-muted/50"
            }`}
            disabled={!discoveryResult || discoveryResult.states.length === 0}
            title={
              !discoveryResult || discoveryResult.states.length === 0
                ? "Run discovery to see state graph"
                : "Interactive state graph"
            }
          >
            <Share2 className="w-3.5 h-3.5" />
            Graph
          </button>
          <button
            onClick={() => setViewMode("transitions")}
            className={`flex items-center gap-1.5 px-3 py-1.5 text-xs font-medium rounded-md transition-colors ${
              viewMode === "transitions"
                ? "bg-primary text-primary-foreground"
                : "text-muted-foreground hover:text-foreground hover:bg-muted/50"
            }`}
          >
            <GitBranch className="w-3.5 h-3.5" />
            Transitions
            {cooccurrenceData && (
              <Badge variant={viewMode === "transitions" ? "default" : "muted"} size="sm">
                {cooccurrenceData.transitions.length}
              </Badge>
            )}
          </button>
          <button
            onClick={() => setViewMode("matrix")}
            className={`flex items-center gap-1.5 px-3 py-1.5 text-xs font-medium rounded-md transition-colors ${
              viewMode === "matrix"
                ? "bg-primary text-primary-foreground"
                : "text-muted-foreground hover:text-foreground hover:bg-muted/50"
            }`}
          >
            <Network className="w-3.5 h-3.5" />
            Matrix
          </button>
          <button
            onClick={() => setViewMode("stats")}
            className={`flex items-center gap-1.5 px-3 py-1.5 text-xs font-medium rounded-md transition-colors ${
              viewMode === "stats"
                ? "bg-primary text-primary-foreground"
                : "text-muted-foreground hover:text-foreground hover:bg-muted/50"
            }`}
          >
            <BarChart3 className="w-3.5 h-3.5" />
            Stats
          </button>
        </div>

        <div className="flex items-center gap-2">
          {/* Run Discovery button */}
          {onRunDiscovery && (
            <Button
              variant="primary"
              size="sm"
              onClick={handleRunDiscovery}
              disabled={
                isRunningDiscovery ||
                !cooccurrenceData ||
                cooccurrenceData.presenceMatrix.length < 2
              }
              title={
                !cooccurrenceData
                  ? "No data to analyze"
                  : cooccurrenceData.presenceMatrix.length < 2
                    ? "Need at least 2 captures for discovery"
                    : "Run state discovery analysis"
              }
            >
              <Fingerprint
                className={`w-4 h-4 mr-1.5 ${isRunningDiscovery ? "animate-pulse" : ""}`}
              />
              {isRunningDiscovery ? "Running..." : "Run Discovery"}
            </Button>
          )}

          {/* Toggle for discovered states */}
          {discoveryResult && discoveryResult.states.length > 0 && viewMode === "states" && (
            <Button
              variant={showDiscoveredStates ? "primary" : "outline"}
              size="sm"
              onClick={() => setShowDiscoveredStates(!showDiscoveredStates)}
              title={showDiscoveredStates ? "Show raw state candidates" : "Show discovered states"}
            >
              {showDiscoveredStates ? (
                <>
                  <CheckCircle2 className="w-4 h-4 mr-1.5" />
                  Discovered ({discoveryResult.states.length})
                </>
              ) : (
                <>
                  <Layers className="w-4 h-4 mr-1.5" />
                  Raw
                </>
              )}
            </Button>
          )}

          {/* Compare mode toggle - only available when showing discovered states */}
          {discoveryResult &&
            discoveryResult.states.length >= 2 &&
            viewMode === "states" &&
            showDiscoveredStates && (
              <Button
                variant={compareMode ? "primary" : "outline"}
                size="sm"
                onClick={() => (compareMode ? exitCompareMode() : setCompareMode(true))}
                title={compareMode ? "Exit compare mode" : "Compare two states side-by-side"}
              >
                <GitCompare className="w-4 h-4 mr-1.5" />
                {compareMode ? "Exit Compare" : "Compare"}
              </Button>
            )}

          {/* Compare selected button - visible when exactly 2 states selected */}
          {compareMode && selectedStates.length === 2 && (
            <Button variant="success" size="sm" onClick={openComparisonView}>
              Compare Selected ({selectedStates.length})
            </Button>
          )}

          {/* Selection info in compare mode */}
          {compareMode && selectedStates.length < 2 && (
            <span className="text-xs text-muted-foreground px-2">
              Select {2 - selectedStates.length} more state{selectedStates.length === 1 ? "" : "s"}
            </span>
          )}

          <Button
            variant="outline"
            size="sm"
            onClick={onRefresh}
            disabled={isLoading || !sessionActive}
            title="Refresh co-occurrence data"
          >
            <RefreshCw className={`w-4 h-4 ${isLoading ? "animate-spin" : ""}`} />
          </Button>
          <Button
            variant="outline"
            size="sm"
            onClick={handleCopyData}
            disabled={!cooccurrenceData}
            title="Copy data to clipboard"
          >
            {copied ? <Check className="w-4 h-4" /> : <Copy className="w-4 h-4" />}
          </Button>
          <Button
            variant="outline"
            size="sm"
            onClick={handleExportData}
            disabled={!cooccurrenceData}
            title="Download as JSON"
          >
            <Download className="w-4 h-4" />
          </Button>
          <Button
            variant="outline"
            size="sm"
            onClick={() => setShowExportDialog(true)}
            disabled={!cooccurrenceData && !discoveryResult}
            title="Export to Automation Config"
          >
            <Share2 className="w-4 h-4 mr-1.5" />
            Export Config
          </Button>
        </div>
      </div>

      {/* Navigation Breadcrumb */}
      {(navigationState.history.items.length > 0 ||
        navigationState.selectedStateId ||
        navigationState.selectedTransitionId ||
        navigationState.selectedFingerprintId) && (
        <div className="mb-2">
          <NavigationBreadcrumb
            history={navigationState.history}
            onNavigate={handleBreadcrumbNavigate}
            onClear={clearNavigation}
            currentSelection={
              navigationState.selectedStateId
                ? {
                    type: "state",
                    id: navigationState.selectedStateId,
                    label:
                      discoveryResult?.states.find(
                        (s) => s.stateId === navigationState.selectedStateId,
                      )?.name || navigationState.selectedStateId,
                  }
                : navigationState.selectedTransitionId
                  ? {
                      type: "transition",
                      id: navigationState.selectedTransitionId,
                      label: navigationState.selectedTransitionId,
                    }
                  : navigationState.selectedFingerprintId
                    ? {
                        type: "fingerprint",
                        id: navigationState.selectedFingerprintId,
                        label: navigationState.selectedFingerprintId.slice(0, 12) + "...",
                      }
                    : null
            }
          />
        </div>
      )}

      {/* Fingerprint States Panel - shown when a fingerprint is selected */}
      {showFingerprintPanel && navigationState.selectedFingerprintId && cooccurrenceData && (
        <div className="mb-3">
          <FingerprintStatesPanel
            fingerprintHash={navigationState.selectedFingerprintId}
            fingerprintDetails={cooccurrenceData.fingerprintDetails}
            discoveryResult={discoveryResult ?? null}
            onSelectState={navigateToState}
            onClose={() => {
              setShowFingerprintPanel(false);
              setNavigationState((prev) => ({ ...prev, selectedFingerprintId: null }));
            }}
          />
        </div>
      )}

      {/* Export Config Dialog */}
      <ExportConfigDialog
        isOpen={showExportDialog}
        onClose={() => setShowExportDialog(false)}
        cooccurrenceData={cooccurrenceData}
        discoveryResult={discoveryResult ?? null}
      />

      {/* State Comparison View */}
      {showComparisonView && selectedStates.length === 2 && cooccurrenceData && (
        <StateComparisonView
          stateA={selectedStates[0]}
          stateB={selectedStates[1]}
          fingerprintDetails={cooccurrenceData.fingerprintDetails}
          onClose={() => setShowComparisonView(false)}
          onSelectFingerprint={onSelectFingerprint}
        />
      )}

      {/* Content */}
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
        ) : viewMode === "states" ? (
          <div className="space-y-2">
            {/* Show discovered states from Python library */}
            {showDiscoveredStates && discoveryResult ? (
              discoveryResult.states.length === 0 ? (
                <div className="text-center text-muted-foreground text-sm py-8">
                  <p>No states discovered</p>
                  <p className="text-xs mt-1">
                    The algorithm found no distinct states from the capture data
                  </p>
                </div>
              ) : (
                <>
                  {/* Discovery statistics summary */}
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
                  {discoveryResult.states.map((state) => (
                    <DiscoveredStateCard
                      key={state.stateId}
                      state={state}
                      fingerprintDetails={cooccurrenceData.fingerprintDetails}
                      expanded={expandedDiscoveredStates.has(state.stateId)}
                      onToggle={() => toggleDiscoveredStateExpanded(state.stateId)}
                      onSelectFingerprint={navigateToFingerprint}
                      compareMode={compareMode}
                      isSelected={selectedForComparison.has(state.stateId)}
                      onToggleSelect={() => toggleStateForComparison(state.stateId)}
                      onShowInGraph={handleShowStateInGraph}
                      isHighlighted={navigationState.selectedStateId === state.stateId}
                      cardRef={
                        navigationState.selectedStateId === state.stateId
                          ? { current: stateCardRefs.current.get(state.stateId) || null }
                          : undefined
                      }
                    />
                  ))}
                </>
              )
            ) : cooccurrenceData.stateCandidates.length === 0 ? (
              <div className="text-center text-muted-foreground text-sm py-8">
                <p>No state candidates found yet</p>
                <p className="text-xs mt-1">
                  Capture more pages to find co-occurring element groups
                </p>
              </div>
            ) : (
              cooccurrenceData.stateCandidates.map((candidate, index) => (
                <StateCandidateCard
                  key={index}
                  candidate={candidate}
                  fingerprintDetails={cooccurrenceData.fingerprintDetails}
                  index={index}
                  expanded={expandedStates.has(index)}
                  onToggle={() => toggleStateExpanded(index)}
                  onSelectFingerprint={onSelectFingerprint}
                />
              ))
            )}
          </div>
        ) : viewMode === "graph" ? (
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
        ) : viewMode === "transitions" ? (
          <div className="space-y-2">
            {/* Show discovered transitions if available */}
            {discoveryResult && discoveryResult.transitions.length > 0 && (
              <div className="mb-4">
                <div className="text-sm font-medium mb-2 flex items-center gap-2">
                  <CheckCircle2 className="w-4 h-4 text-green-500" />
                  Discovered Transitions ({discoveryResult.transitions.length})
                </div>
                <div className="space-y-2">
                  {discoveryResult.transitions.map((transition, index) => (
                    <DiscoveredTransitionCard
                      key={`${transition.fromStateId}-${transition.toStateId}-${index}`}
                      transition={transition}
                      discoveryResult={discoveryResult}
                      isHighlighted={
                        navigationState.selectedTransitionId ===
                        createTransitionKey(transition.fromStateId, transition.toStateId)
                      }
                      onNavigateToState={navigateToState}
                      onShowInGraph={handleShowTransitionInGraph}
                    />
                  ))}
                </div>
              </div>
            )}

            {/* Raw transitions */}
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
              cooccurrenceData.transitions.map((transition, index) => (
                <TransitionCard
                  key={index}
                  transition={transition}
                  fingerprintDetails={cooccurrenceData.fingerprintDetails}
                  onSelectFingerprint={navigateToFingerprint}
                />
              ))
            )}
          </div>
        ) : viewMode === "matrix" ? (
          <CooccurrenceMatrixView data={cooccurrenceData} />
        ) : (
          <StatsView data={cooccurrenceData} />
        )}
      </div>
    </div>
  );
}

export default StateDiscoveryPanel;

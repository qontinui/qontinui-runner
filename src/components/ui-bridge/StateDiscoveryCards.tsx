import { useState, useMemo } from "react";
import { Badge } from "../ui/Badge";
import { Button } from "../ui/Button";
import {
  MapPin,
  Repeat,
  ChevronDown,
  ChevronRight,
  AlertCircle,
  CheckCircle2,
  Fingerprint,
  Hash,
  Share2,
  Square,
  CheckSquare,
  Eye,
  ArrowRight,
} from "lucide-react";
import type { ElementFingerprint } from "../../types/ui-bridge-types";
import type { FingerprintDiscoveryResult } from "./discovery-types";
import { ZONE_COLORS } from "./state-discovery-constants";

interface StateCandidateCardProps {
  candidate: { fingerprints: string[]; cooccurrenceRate: number };
  fingerprintDetails: Record<string, ElementFingerprint>;
  index: number;
  expanded: boolean;
  onToggle: () => void;
  onSelectFingerprint?: (hash: string) => void;
}

export function StateCandidateCard({
  candidate,
  fingerprintDetails,
  index,
  expanded,
  onToggle,
  onSelectFingerprint,
}: StateCandidateCardProps) {
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

    const dominantZone = Object.entries(zones).sort((a, b) => b[1] - a[1])[0]?.[0] || "unknown";

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
      stateType,
    };
  }, [candidate.fingerprints, fingerprintDetails]);

  const stateTypeColors: Record<string, string> = {
    global: "bg-blue-500/20 text-blue-400",
    modal: "bg-orange-500/20 text-orange-400",
    content: "bg-green-500/20 text-green-400",
    mixed: "bg-purple-500/20 text-purple-400",
  };

  const SIZE_LABELS: Record<string, string> = {
    icon: "Icon",
    button: "Button",
    small: "Small",
    medium: "Medium",
    large: "Large",
    fullwidth: "Full Width",
    panel: "Panel",
  };

  return (
    <div className="border border-border/50 rounded-lg overflow-hidden">
      <button
        className="w-full flex items-center gap-3 p-3 hover:bg-muted/30 transition-colors text-left"
        onClick={onToggle}
      >
        {expanded ? (
          <ChevronDown className="w-4 h-4 text-muted-foreground shrink-0" />
        ) : (
          <ChevronRight className="w-4 h-4 text-muted-foreground shrink-0" />
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

        <div className="flex items-center gap-2 shrink-0">
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

      {expanded && (
        <div className="border-t border-border/50 p-3 space-y-3 bg-muted/10">
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
                    <Hash className="w-3 h-3 text-muted-foreground shrink-0" />
                    <span className="font-mono text-[10px] text-muted-foreground">{hash}</span>
                    <Badge variant="muted" size="sm">
                      {fp.role || fp.tagName}
                    </Badge>
                    {fp.accessibleName && (
                      <span className="truncate text-muted-foreground">"{fp.accessibleName}"</span>
                    )}
                    {fp.isRepeating && <Repeat className="w-3 h-3 text-blue-400 shrink-0" />}
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

interface TransitionCardProps {
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
}

export function TransitionCard({
  transition,
  fingerprintDetails,
  onSelectFingerprint,
}: TransitionCardProps) {
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
          <ChevronDown className="w-4 h-4 text-muted-foreground shrink-0" />
        ) : (
          <ChevronRight className="w-4 h-4 text-muted-foreground shrink-0" />
        )}

        <Badge variant="info">{transition.actionType}</Badge>

        {targetFp && (
          <span className="text-xs text-muted-foreground truncate">
            on {targetFp.role || targetFp.tagName}
            {targetFp.accessibleName && ` "${targetFp.accessibleName}"`}
          </span>
        )}

        <div className="flex items-center gap-2 ml-auto shrink-0">
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

          {transition.appearedFingerprints.length > 0 && (
            <FingerprintChangeList
              hashes={transition.appearedFingerprints}
              fingerprintDetails={fingerprintDetails}
              onSelectFingerprint={onSelectFingerprint}
              variant="appeared"
            />
          )}

          {transition.disappearedFingerprints.length > 0 && (
            <FingerprintChangeList
              hashes={transition.disappearedFingerprints}
              fingerprintDetails={fingerprintDetails}
              onSelectFingerprint={onSelectFingerprint}
              variant="disappeared"
            />
          )}

          <div className="text-[10px] text-muted-foreground">
            {new Date(transition.timestamp).toLocaleTimeString()}
          </div>
        </div>
      )}
    </div>
  );
}

function FingerprintChangeList({
  hashes,
  fingerprintDetails,
  onSelectFingerprint,
  variant,
}: {
  hashes: string[];
  fingerprintDetails: Record<string, ElementFingerprint>;
  onSelectFingerprint?: (hash: string) => void;
  variant: "appeared" | "disappeared";
}) {
  const isAppeared = variant === "appeared";
  const colorClasses = isAppeared
    ? { label: "text-green-400", bg: "bg-green-500/10", bgHover: "hover:bg-green-500/20" }
    : { label: "text-red-400", bg: "bg-red-500/10", bgHover: "hover:bg-red-500/20" };
  const Icon = isAppeared ? CheckCircle2 : AlertCircle;
  const label = isAppeared ? "Appeared" : "Disappeared";

  return (
    <div className="space-y-1">
      <div className={`text-xs font-medium ${colorClasses.label} flex items-center gap-1`}>
        <Icon className="w-3 h-3" />
        {label} ({hashes.length})
      </div>
      <div className="max-h-32 overflow-y-auto space-y-1">
        {hashes.slice(0, 10).map((hash) => {
          const fp = fingerprintDetails[hash];
          return (
            <button
              key={hash}
              className={`w-full flex items-center gap-2 p-1.5 ${colorClasses.bg} rounded text-xs ${colorClasses.bgHover} transition-colors text-left`}
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
                    <span className="truncate text-muted-foreground">"{fp.accessibleName}"</span>
                  )}
                </>
              )}
            </button>
          );
        })}
        {hashes.length > 10 && (
          <div className="text-xs text-muted-foreground pl-2">+{hashes.length - 10} more</div>
        )}
      </div>
    </div>
  );
}

interface DiscoveredStateCardProps {
  state: FingerprintDiscoveryResult["states"][0];
  fingerprintDetails: Record<string, ElementFingerprint>;
  expanded: boolean;
  onToggle: () => void;
  onSelectFingerprint?: (hash: string) => void;
  compareMode?: boolean;
  isSelected?: boolean;
  onToggleSelect?: () => void;
  onShowInGraph?: (stateId: string) => void;
  isHighlighted?: boolean;
  cardRef?: React.RefObject<HTMLDivElement | null>;
}

export function DiscoveredStateCard({
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
}: DiscoveredStateCardProps) {
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
      <button
        className="w-full flex items-center gap-3 p-3 hover:bg-muted/30 transition-colors text-left"
        onClick={handleClick}
      >
        {compareMode ? (
          <div
            className="shrink-0"
            onClick={(e) => {
              e.stopPropagation();
              onToggleSelect?.();
            }}
            onKeyDown={(e) => {
              if (e.key === "Enter" || e.key === " ") {
                e.preventDefault();
                e.stopPropagation();
                onToggleSelect?.();
              }
            }}
            role="button"
            tabIndex={0}
          >
            {isSelected ? (
              <CheckSquare className="w-4 h-4 text-primary" />
            ) : (
              <Square className="w-4 h-4 text-muted-foreground" />
            )}
          </div>
        ) : expanded ? (
          <ChevronDown className="w-4 h-4 text-muted-foreground shrink-0" />
        ) : (
          <ChevronRight className="w-4 h-4 text-muted-foreground shrink-0" />
        )}

        <div className="flex items-center gap-2 flex-1 min-w-0">
          <CheckCircle2 className="w-4 h-4 text-green-500" />
          <span className="font-medium text-sm truncate">{state.name}</span>
          <Badge className={stateTypeColors[stateType]}>{stateType}</Badge>
          <Badge className={ZONE_COLORS[state.positionZone] || ZONE_COLORS.main}>
            {state.positionZone}
          </Badge>
        </div>

        <div className="flex items-center gap-2 shrink-0">
          <span className={`text-xs font-mono ${confidenceColor}`}>
            {(state.confidence * 100).toFixed(0)}%
          </span>
          <Badge variant="muted" size="sm">
            {state.fingerprintHashes.length} elements
          </Badge>
        </div>
      </button>

      {expanded && !compareMode && (
        <div className="p-3 pt-0 space-y-3 border-t border-border/30">
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
                    <Fingerprint className="w-3 h-3 text-muted-foreground shrink-0" />
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

interface DiscoveredTransitionCardProps {
  transition: FingerprintDiscoveryResult["transitions"][0];
  discoveryResult: FingerprintDiscoveryResult;
  isHighlighted?: boolean;
  onNavigateToState?: (stateId: string, stateName: string) => void;
  onShowInGraph?: (fromStateId: string, toStateId: string) => void;
}

export function DiscoveredTransitionCard({
  transition,
  discoveryResult,
  isHighlighted,
  onNavigateToState,
  onShowInGraph,
}: DiscoveredTransitionCardProps) {
  const [expanded, setExpanded] = useState(false);

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
          <ChevronDown className="w-4 h-4 text-muted-foreground shrink-0" />
        ) : (
          <ChevronRight className="w-4 h-4 text-muted-foreground shrink-0" />
        )}

        <Badge variant="info">{transition.actionType}</Badge>

        <div className="flex items-center gap-1 text-xs text-muted-foreground flex-1 min-w-0">
          <span className="truncate">{fromState?.name || transition.fromStateId}</span>
          <ArrowRight className="w-3 h-3 shrink-0" />
          <span className="truncate">{toState?.name || transition.toStateId}</span>
        </div>

        <Badge variant="muted" size="sm">
          {transition.count}x
        </Badge>
      </button>

      {expanded && (
        <div className="border-t border-border/50 p-3 space-y-3 bg-muted/10">
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

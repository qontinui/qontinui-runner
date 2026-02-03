/**
 * StateComparisonView
 *
 * Side-by-side comparison of two states to understand their differences.
 * Shows common fingerprints, fingerprints unique to each state, and
 * compares metadata like confidence, position zone, and state type.
 */

import { useMemo } from "react";
import { X, Hash, MapPin, Fingerprint, Percent, Layers, ArrowRight } from "lucide-react";
import { Badge } from "../ui/Badge";
import { Button } from "../ui/Button";
import type { FingerprintDiscoveryResult } from "./StateDiscoveryPanel";
import type { ElementFingerprint } from "../../hooks/useExternalUIBridge";

interface StateComparisonViewProps {
  /** First state to compare */
  stateA: FingerprintDiscoveryResult["states"][0];
  /** Second state to compare */
  stateB: FingerprintDiscoveryResult["states"][0];
  /** Fingerprint details for enriched display */
  fingerprintDetails: Record<string, ElementFingerprint>;
  /** Close the comparison view */
  onClose: () => void;
  /** Callback when a fingerprint is selected */
  onSelectFingerprint?: (hash: string) => void;
}

/**
 * Compare two states and compute the intersection and differences
 */
interface StateComparisonResult {
  /** Fingerprints present in both states */
  common: string[];
  /** Fingerprints only in state A */
  onlyInA: string[];
  /** Fingerprints only in state B */
  onlyInB: string[];
  /** Jaccard similarity coefficient (0-1) */
  similarity: number;
}

/**
 * Calculate Jaccard similarity between two sets
 */
function jaccardSimilarity(setA: Set<string>, setB: Set<string>): number {
  if (setA.size === 0 && setB.size === 0) return 1;

  const intersection = new Set([...setA].filter((x) => setB.has(x)));
  const union = new Set([...setA, ...setB]);

  return intersection.size / union.size;
}

/**
 * Compare two states and return detailed comparison result
 */
function compareStates(
  stateA: FingerprintDiscoveryResult["states"][0],
  stateB: FingerprintDiscoveryResult["states"][0],
): StateComparisonResult {
  const fingerprintsA = new Set(stateA.fingerprintHashes);
  const fingerprintsB = new Set(stateB.fingerprintHashes);

  return {
    common: [...fingerprintsA].filter((f) => fingerprintsB.has(f)),
    onlyInA: [...fingerprintsA].filter((f) => !fingerprintsB.has(f)),
    onlyInB: [...fingerprintsB].filter((f) => !fingerprintsA.has(f)),
    similarity: jaccardSimilarity(fingerprintsA, fingerprintsB),
  };
}

// Position zone colors for visual distinction
const ZONE_COLORS: Record<string, string> = {
  header: "bg-blue-500/20 text-blue-400 border-blue-500/30",
  footer: "bg-purple-500/20 text-purple-400 border-purple-500/30",
  "sidebar-left": "bg-green-500/20 text-green-400 border-green-500/30",
  "sidebar-right": "bg-green-500/20 text-green-400 border-green-500/30",
  main: "bg-zinc-500/20 text-zinc-400 border-zinc-500/30",
  modal: "bg-orange-500/20 text-orange-400 border-orange-500/30",
};

// State type colors
const STATE_TYPE_COLORS: Record<string, string> = {
  global: "bg-blue-500/20 text-blue-400",
  modal: "bg-orange-500/20 text-orange-400",
  content: "bg-green-500/20 text-green-400",
};

function getStateType(state: FingerprintDiscoveryResult["states"][0]): string {
  return state.isGlobal ? "global" : state.isModal ? "modal" : "content";
}

function getConfidenceColor(confidence: number): string {
  if (confidence >= 0.9) return "text-green-400";
  if (confidence >= 0.7) return "text-yellow-400";
  return "text-red-400";
}

/**
 * Renders a single fingerprint item
 */
function FingerprintItem({
  hash,
  fingerprintDetails,
  variant,
  onSelect,
}: {
  hash: string;
  fingerprintDetails: Record<string, ElementFingerprint>;
  variant: "common" | "added" | "removed";
  onSelect?: (hash: string) => void;
}) {
  const fp = fingerprintDetails[hash];

  const variantStyles = {
    common: "bg-muted/30 hover:bg-muted/50",
    added: "bg-green-500/10 hover:bg-green-500/20 border-l-2 border-green-500",
    removed: "bg-red-500/10 hover:bg-red-500/20 border-l-2 border-red-500",
  };

  return (
    <button
      className={`w-full flex items-center gap-2 p-2 rounded text-xs transition-colors text-left ${variantStyles[variant]}`}
      onClick={() => onSelect?.(hash)}
    >
      {variant === "added" && <span className="text-green-400 font-bold">+</span>}
      {variant === "removed" && <span className="text-red-400 font-bold">-</span>}
      {variant === "common" && <Hash className="w-3 h-3 text-muted-foreground flex-shrink-0" />}

      <span className="font-mono text-[10px] text-muted-foreground">{hash.slice(0, 12)}...</span>

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
}

/**
 * State summary column
 */
function StateSummary({
  state,
  label,
}: {
  state: FingerprintDiscoveryResult["states"][0];
  label: string;
}) {
  const stateType = getStateType(state);
  const confidenceColor = getConfidenceColor(state.confidence);

  return (
    <div className="flex-1 p-4 bg-muted/20 rounded-lg">
      <div className="text-xs text-muted-foreground mb-1">{label}</div>
      <h3 className="font-semibold text-lg mb-3 truncate" title={state.name}>
        {state.name}
      </h3>

      <div className="space-y-2 text-sm">
        <div className="flex items-center justify-between">
          <span className="text-muted-foreground flex items-center gap-1">
            <Layers className="w-3 h-3" />
            Type
          </span>
          <Badge className={STATE_TYPE_COLORS[stateType]}>{stateType}</Badge>
        </div>

        <div className="flex items-center justify-between">
          <span className="text-muted-foreground flex items-center gap-1">
            <MapPin className="w-3 h-3" />
            Zone
          </span>
          <Badge className={ZONE_COLORS[state.positionZone] || ZONE_COLORS.main}>
            {state.positionZone}
          </Badge>
        </div>

        <div className="flex items-center justify-between">
          <span className="text-muted-foreground flex items-center gap-1">
            <Percent className="w-3 h-3" />
            Confidence
          </span>
          <span className={`font-mono ${confidenceColor}`}>
            {(state.confidence * 100).toFixed(0)}%
          </span>
        </div>

        <div className="flex items-center justify-between">
          <span className="text-muted-foreground flex items-center gap-1">
            <Fingerprint className="w-3 h-3" />
            Elements
          </span>
          <span className="font-mono">{state.fingerprintHashes.length}</span>
        </div>
      </div>
    </div>
  );
}

export function StateComparisonView({
  stateA,
  stateB,
  fingerprintDetails,
  onClose,
  onSelectFingerprint,
}: StateComparisonViewProps) {
  const comparison = useMemo(() => compareStates(stateA, stateB), [stateA, stateB]);

  const similarityColor =
    comparison.similarity >= 0.7
      ? "text-green-400"
      : comparison.similarity >= 0.4
        ? "text-yellow-400"
        : "text-red-400";

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50">
      <div
        data-ui-id="dialog-state-comparison"
        className="bg-card border border-border rounded-lg shadow-xl w-full max-w-4xl mx-4 max-h-[90vh] flex flex-col"
      >
        {/* Header */}
        <div className="flex items-center justify-between p-4 border-b border-border flex-shrink-0">
          <div className="flex items-center gap-3">
            <div className="flex items-center gap-2">
              <Layers className="w-5 h-5 text-primary" />
              <h2 className="text-lg font-semibold">Comparing States</h2>
            </div>
            <Badge variant="muted">
              <span className={similarityColor}>
                {(comparison.similarity * 100).toFixed(0)}% similar
              </span>
            </Badge>
          </div>
          <button
            onClick={onClose}
            className="p-1 hover:bg-muted rounded-md transition-colors"
            data-ui-id="state-comparison-close-btn"
          >
            <X className="w-5 h-5" />
          </button>
        </div>

        {/* Content */}
        <div className="p-4 space-y-4 overflow-y-auto flex-1">
          {/* State summaries side by side */}
          <div className="flex gap-4">
            <StateSummary state={stateA} label="State A" />
            <div className="flex items-center">
              <ArrowRight className="w-6 h-6 text-muted-foreground" />
            </div>
            <StateSummary state={stateB} label="State B" />
          </div>

          {/* Similarity visualization */}
          <div className="p-4 bg-muted/30 rounded-lg">
            <div className="flex items-center justify-between mb-2">
              <span className="text-sm font-medium">Jaccard Similarity</span>
              <span className={`font-mono text-lg ${similarityColor}`}>
                {(comparison.similarity * 100).toFixed(1)}%
              </span>
            </div>
            <div className="h-3 bg-muted rounded-full overflow-hidden">
              <div
                className={`h-full transition-all duration-500 ${
                  comparison.similarity >= 0.7
                    ? "bg-green-500"
                    : comparison.similarity >= 0.4
                      ? "bg-yellow-500"
                      : "bg-red-500"
                }`}
                style={{ width: `${comparison.similarity * 100}%` }}
              />
            </div>
            <div className="flex justify-between mt-1 text-xs text-muted-foreground">
              <span>0% (completely different)</span>
              <span>100% (identical)</span>
            </div>
          </div>

          {/* Common elements */}
          <div className="border border-border/50 rounded-lg overflow-hidden">
            <div className="flex items-center gap-2 p-3 bg-muted/20">
              <Hash className="w-4 h-4 text-muted-foreground" />
              <span className="font-medium text-sm">Common Elements</span>
              <Badge variant="muted" size="sm">
                {comparison.common.length}
              </Badge>
            </div>
            {comparison.common.length > 0 ? (
              <div className="p-2 max-h-40 overflow-y-auto space-y-1">
                {comparison.common.map((hash) => (
                  <FingerprintItem
                    key={hash}
                    hash={hash}
                    fingerprintDetails={fingerprintDetails}
                    variant="common"
                    onSelect={onSelectFingerprint}
                  />
                ))}
              </div>
            ) : (
              <div className="p-4 text-sm text-muted-foreground text-center">
                No common elements between these states
              </div>
            )}
          </div>

          {/* Differences side by side */}
          <div className="grid grid-cols-2 gap-4">
            {/* Only in State A */}
            <div className="border border-red-500/30 rounded-lg overflow-hidden">
              <div className="flex items-center gap-2 p-3 bg-red-500/10">
                <span className="text-red-400 font-bold">-</span>
                <span className="font-medium text-sm text-red-400">Only in {stateA.name}</span>
                <Badge variant="danger" size="sm">
                  {comparison.onlyInA.length}
                </Badge>
              </div>
              {comparison.onlyInA.length > 0 ? (
                <div className="p-2 max-h-48 overflow-y-auto space-y-1">
                  {comparison.onlyInA.map((hash) => (
                    <FingerprintItem
                      key={hash}
                      hash={hash}
                      fingerprintDetails={fingerprintDetails}
                      variant="removed"
                      onSelect={onSelectFingerprint}
                    />
                  ))}
                </div>
              ) : (
                <div className="p-4 text-sm text-muted-foreground text-center">
                  All elements from {stateA.name} are also in {stateB.name}
                </div>
              )}
            </div>

            {/* Only in State B */}
            <div className="border border-green-500/30 rounded-lg overflow-hidden">
              <div className="flex items-center gap-2 p-3 bg-green-500/10">
                <span className="text-green-400 font-bold">+</span>
                <span className="font-medium text-sm text-green-400">Only in {stateB.name}</span>
                <Badge variant="success" size="sm">
                  {comparison.onlyInB.length}
                </Badge>
              </div>
              {comparison.onlyInB.length > 0 ? (
                <div className="p-2 max-h-48 overflow-y-auto space-y-1">
                  {comparison.onlyInB.map((hash) => (
                    <FingerprintItem
                      key={hash}
                      hash={hash}
                      fingerprintDetails={fingerprintDetails}
                      variant="added"
                      onSelect={onSelectFingerprint}
                    />
                  ))}
                </div>
              ) : (
                <div className="p-4 text-sm text-muted-foreground text-center">
                  All elements from {stateB.name} are also in {stateA.name}
                </div>
              )}
            </div>
          </div>

          {/* Summary statistics */}
          <div className="grid grid-cols-4 gap-3">
            <div className="p-3 bg-muted/30 rounded-lg text-center">
              <div className="text-2xl font-bold">{stateA.fingerprintHashes.length}</div>
              <div className="text-xs text-muted-foreground">Elements in A</div>
            </div>
            <div className="p-3 bg-muted/30 rounded-lg text-center">
              <div className="text-2xl font-bold">{stateB.fingerprintHashes.length}</div>
              <div className="text-xs text-muted-foreground">Elements in B</div>
            </div>
            <div className="p-3 bg-muted/30 rounded-lg text-center">
              <div className="text-2xl font-bold text-blue-400">{comparison.common.length}</div>
              <div className="text-xs text-muted-foreground">Shared</div>
            </div>
            <div className="p-3 bg-muted/30 rounded-lg text-center">
              <div className="text-2xl font-bold text-purple-400">
                {comparison.onlyInA.length + comparison.onlyInB.length}
              </div>
              <div className="text-xs text-muted-foreground">Different</div>
            </div>
          </div>
        </div>

        {/* Footer */}
        <div className="flex items-center justify-end gap-2 p-4 border-t border-border flex-shrink-0">
          <Button variant="outline" size="sm" onClick={onClose} data-ui-id="state-comparison-close">
            Close
          </Button>
        </div>
      </div>
    </div>
  );
}

export default StateComparisonView;

/**
 * FingerprintStatesPanel Component
 *
 * Shows all states that contain a selected fingerprint.
 * Can be used as a popover or collapsible panel.
 */

import { memo, useMemo } from "react";
import { Layers, Circle, Fingerprint, ChevronRight, X } from "lucide-react";
import { Badge } from "../ui/Badge";
import type { FingerprintDiscoveryResult } from "./StateDiscoveryPanel";
import type { ElementFingerprint } from "../../hooks/useExternalUIBridge";

// =============================================================================
// Types
// =============================================================================

interface FingerprintStatesPanelProps {
  /** The selected fingerprint hash */
  fingerprintHash: string;
  /** Fingerprint details */
  fingerprintDetails: Record<string, ElementFingerprint>;
  /** Discovery result containing states */
  discoveryResult: FingerprintDiscoveryResult | null;
  /** Callback when a state is clicked */
  onSelectState: (stateId: string, stateName: string) => void;
  /** Callback to close the panel */
  onClose?: () => void;
  /** Whether to show as a compact inline panel */
  compact?: boolean;
}

// =============================================================================
// Helper Components
// =============================================================================

const stateTypeColors: Record<string, string> = {
  global: "bg-blue-500/20 text-blue-400",
  modal: "bg-orange-500/20 text-orange-400",
  content: "bg-green-500/20 text-green-400",
};

function StateListItem({
  state,
  onClick,
}: {
  state: FingerprintDiscoveryResult["states"][0];
  onClick: () => void;
}) {
  const stateType = state.isGlobal ? "global" : state.isModal ? "modal" : "content";

  return (
    <button
      onClick={onClick}
      className="w-full flex items-center gap-2 p-2 rounded hover:bg-muted/50 transition-colors text-left group"
    >
      <Circle
        className="w-2 h-2 flex-shrink-0"
        style={{
          fill:
            stateType === "global" ? "#3b82f6" : stateType === "modal" ? "#f97316" : "#22c55e",
          color:
            stateType === "global" ? "#3b82f6" : stateType === "modal" ? "#f97316" : "#22c55e",
        }}
      />
      <div className="flex-1 min-w-0">
        <div className="flex items-center gap-2">
          <span className="text-sm font-medium truncate">{state.name}</span>
          <Badge className={`${stateTypeColors[stateType]} text-[10px] px-1.5 py-0`} size="sm">
            {stateType}
          </Badge>
        </div>
        <div className="text-xs text-muted-foreground">
          {state.fingerprintHashes.length} elements
          {state.positionZone && ` | ${state.positionZone}`}
        </div>
      </div>
      <ChevronRight className="w-4 h-4 text-muted-foreground opacity-0 group-hover:opacity-100 transition-opacity" />
    </button>
  );
}

// =============================================================================
// Main Component
// =============================================================================

function FingerprintStatesPanelComponent({
  fingerprintHash,
  fingerprintDetails,
  discoveryResult,
  onSelectState,
  onClose,
  compact = false,
}: FingerprintStatesPanelProps) {
  const fingerprint = fingerprintDetails[fingerprintHash];

  // Find all states containing this fingerprint
  const containingStates = useMemo(() => {
    if (!discoveryResult) return [];

    return discoveryResult.states.filter((state) =>
      state.fingerprintHashes.includes(fingerprintHash),
    );
  }, [discoveryResult, fingerprintHash]);

  if (!fingerprint) {
    return (
      <div className="p-3 text-center text-muted-foreground text-sm">
        Fingerprint not found
      </div>
    );
  }

  return (
    <div
      className={`bg-card border border-border/50 rounded-lg shadow-lg ${compact ? "max-w-xs" : "w-80"}`}
    >
      {/* Header */}
      <div className="flex items-center justify-between p-3 border-b border-border/50">
        <div className="flex items-center gap-2 min-w-0">
          <Fingerprint className="w-4 h-4 text-primary flex-shrink-0" />
          <div className="min-w-0">
            <div className="text-xs font-mono text-muted-foreground truncate">
              {fingerprintHash.slice(0, 16)}...
            </div>
            <div className="text-sm font-medium truncate">
              {fingerprint.accessibleName || fingerprint.role || fingerprint.tagName}
            </div>
          </div>
        </div>
        {onClose && (
          <button
            onClick={onClose}
            className="p-1 rounded hover:bg-muted/50 text-muted-foreground hover:text-foreground transition-colors"
          >
            <X className="w-4 h-4" />
          </button>
        )}
      </div>

      {/* Fingerprint details */}
      <div className="px-3 py-2 bg-muted/20 text-xs space-y-1">
        <div className="flex items-center gap-2">
          <Badge variant="muted" size="sm">
            {fingerprint.role || fingerprint.tagName}
          </Badge>
          <Badge variant="muted" size="sm">
            {fingerprint.sizeCategory}
          </Badge>
          <Badge variant="muted" size="sm">
            {fingerprint.positionZone}
          </Badge>
        </div>
        {fingerprint.landmarkContext && (
          <div className="text-muted-foreground">Landmark: {fingerprint.landmarkContext}</div>
        )}
      </div>

      {/* States containing this fingerprint */}
      <div className="p-2">
        <div className="flex items-center gap-2 px-2 pb-2 text-xs font-medium text-muted-foreground">
          <Layers className="w-3 h-3" />
          States containing this fingerprint ({containingStates.length})
        </div>

        {containingStates.length === 0 ? (
          <div className="text-center text-muted-foreground text-sm py-4">
            <p>No states contain this fingerprint</p>
            <p className="text-xs mt-1">Run discovery to identify states</p>
          </div>
        ) : (
          <div className="space-y-1 max-h-64 overflow-y-auto">
            {containingStates.map((state) => (
              <StateListItem
                key={state.stateId}
                state={state}
                onClick={() => onSelectState(state.stateId, state.name)}
              />
            ))}
          </div>
        )}
      </div>
    </div>
  );
}

export const FingerprintStatesPanel = memo(FingerprintStatesPanelComponent);

export default FingerprintStatesPanel;

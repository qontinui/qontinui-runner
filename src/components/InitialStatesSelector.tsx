/**
 * InitialStatesSelector Component
 *
 * Displays and allows override of initial active states for workflow execution.
 * Shows the source of current states (defaults, workflow, or override) and
 * allows session-only overrides that reset on config reload or workflow change.
 */

import { useMemo } from "react";
import { Check, Info, RotateCcw, CircleDot } from "lucide-react";
import type { ConfigState } from "../hooks/useConfiguration";
import type { InitialStatesSource, ResolvedInitialStates } from "../types/state-machine";
import { getAccentColors } from "@/design-system";

interface InitialStatesSelectorProps {
  /** All available states from loaded config */
  states: ConfigState[];

  /** Resolved initial states from config (without override) */
  resolvedStates: ResolvedInitialStates;

  /** Current override state IDs (null = no override) */
  overrideStateIds: string[] | null;

  /** Called when override changes */
  onOverrideChange: (ids: string[] | null) => void;

  /** Whether the selector is disabled */
  disabled?: boolean;

  /** Optional class name */
  className?: string;
}

/**
 * Get human-readable label for initial states source
 */
function getSourceLabel(source: InitialStatesSource): string {
  switch (source) {
    case "defaults":
      return "State Machine Defaults";
    case "workflow":
      return "Workflow Configuration";
    case "override":
      return "Session Override";
    default:
      return "Unknown";
  }
}

/**
 * Get source color classes
 */
function getSourceColor(source: InitialStatesSource): string {
  switch (source) {
    case "defaults":
      return "text-muted-foreground bg-muted/50";
    case "workflow":
      return `${getAccentColors("blue").text} ${getAccentColors("blue").bg}`;
    case "override":
      return `${getAccentColors("amber").text} ${getAccentColors("amber").bg}`;
    default:
      return "text-muted-foreground bg-muted/50";
  }
}

export function InitialStatesSelector({
  states,
  resolvedStates,
  overrideStateIds,
  onOverrideChange,
  disabled = false,
  className = "",
}: InitialStatesSelectorProps) {
  // Determine effective source and state IDs
  const hasOverride = overrideStateIds !== null;
  const effectiveSource: InitialStatesSource = hasOverride ? "override" : resolvedStates.source;
  const effectiveStateIds = hasOverride ? overrideStateIds : resolvedStates.stateIds;

  // Build a map of state ID to name for display
  const stateNameMap = useMemo(() => {
    const map = new Map<string, string>();
    states.forEach((state) => {
      map.set(state.id, state.name || state.id);
    });
    // Also include names from resolved states (in case some states aren't in config)
    resolvedStates.states?.forEach((state) => {
      if (!map.has(state.id)) {
        map.set(state.id, state.name);
      }
    });
    return map;
  }, [states, resolvedStates.states]);

  /**
   * Toggle a state in the override
   */
  const handleStateToggle = (stateId: string) => {
    if (disabled) return;

    if (hasOverride) {
      // Already have an override - toggle within it
      if (overrideStateIds.includes(stateId)) {
        // Remove state (but keep at least empty array to maintain override)
        onOverrideChange(overrideStateIds.filter((id) => id !== stateId));
      } else {
        // Add state
        onOverrideChange([...overrideStateIds, stateId]);
      }
    } else {
      // No override yet - create one based on current resolved states
      if (effectiveStateIds.includes(stateId)) {
        // Remove state from resolved set
        onOverrideChange(effectiveStateIds.filter((id) => id !== stateId));
      } else {
        // Add state to resolved set
        onOverrideChange([...effectiveStateIds, stateId]);
      }
    }
  };

  /**
   * Clear the override
   */
  const handleClearOverride = () => {
    if (disabled) return;
    onOverrideChange(null);
  };

  if (states.length === 0) {
    return (
      <div className={`text-sm text-muted-foreground ${className}`}>
        No states defined in configuration
      </div>
    );
  }

  return (
    <div className={`space-y-3 ${className}`}>
      {/* Source indicator */}
      <div className="flex items-center justify-between gap-2">
        <div className="flex items-center gap-2">
          <CircleDot className="w-4 h-4 text-muted-foreground" />
          <span className="text-sm text-muted-foreground">Source:</span>
          <span className={`text-xs px-2 py-0.5 rounded-full ${getSourceColor(effectiveSource)}`}>
            {getSourceLabel(effectiveSource)}
          </span>
        </div>

        {/* Clear override button */}
        {hasOverride && (
          <button
            onClick={handleClearOverride}
            disabled={disabled}
            className="flex items-center gap-1 text-xs text-muted-foreground hover:text-foreground transition-colors disabled:opacity-50"
          >
            <RotateCcw className="w-3 h-3" />
            Reset
          </button>
        )}
      </div>

      {/* Override info */}
      {hasOverride && (
        <div
          className={`flex items-start gap-2 p-2 ${getAccentColors("amber").bg} border ${getAccentColors("amber").border} rounded-lg text-xs`}
        >
          <Info className={`w-4 h-4 flex-shrink-0 ${getAccentColors("amber").text} mt-0.5`} />
          <div className={getAccentColors("amber").text}>
            Session override active. Changes reset when config reloads or workflow changes.
          </div>
        </div>
      )}

      {/* State list */}
      <div className="space-y-1 max-h-48 overflow-y-auto">
        {states.map((state) => {
          const isSelected = effectiveStateIds.includes(state.id);
          const stateName = stateNameMap.get(state.id) || state.id;

          return (
            <button
              key={state.id}
              onClick={() => handleStateToggle(state.id)}
              disabled={disabled}
              className={`w-full flex items-center gap-2 p-2 rounded-lg border text-left transition-all ${
                isSelected
                  ? "bg-primary/10 border-primary/50 text-foreground"
                  : "bg-input/50 border-border/50 hover:border-primary/30 hover:bg-accent/30 text-muted-foreground"
              } ${disabled ? "cursor-not-allowed opacity-60" : "cursor-pointer"}`}
            >
              <div
                className={`w-4 h-4 rounded border flex items-center justify-center flex-shrink-0 transition-colors ${
                  isSelected
                    ? "bg-primary border-primary"
                    : "border-muted-foreground/50 bg-transparent"
                }`}
              >
                {isSelected && <Check className="w-3 h-3 text-primary-foreground" />}
              </div>
              <span className="text-sm truncate flex-1">{stateName}</span>
              {/* Show if this state is from override vs resolved */}
              {hasOverride && resolvedStates.stateIds.includes(state.id) !== isSelected && (
                <span
                  className={`text-xs ${getAccentColors("amber").text} ${getAccentColors("amber").bg} px-1.5 py-0.5 rounded`}
                >
                  {isSelected ? "added" : "removed"}
                </span>
              )}
            </button>
          );
        })}
      </div>

      {/* Summary */}
      <div className="text-xs text-muted-foreground">
        {effectiveStateIds.length === 0 ? (
          <span>No initial states selected</span>
        ) : (
          <span>
            {effectiveStateIds.length} state{effectiveStateIds.length !== 1 ? "s" : ""} will be
            active at workflow start
          </span>
        )}
      </div>
    </div>
  );
}

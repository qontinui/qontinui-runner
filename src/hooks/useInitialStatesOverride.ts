/**
 * useInitialStatesOverride
 *
 * Hook for managing session-only override of initial active states.
 * The override is temporary and resets when:
 * - A new configuration is loaded
 * - The selected workflow changes
 * - The app restarts
 *
 * This implements the highest priority level of the initial states system:
 * 1. State Machine defaults (states with is_initial=true) - lowest priority
 * 2. Workflow definition (initialStateIds) - medium priority
 * 3. Runner override (this hook) - highest priority
 */

import { useState, useCallback, useEffect } from "react";
import type { InitialStatesSource } from "../types/state-machine";
import { createLogger } from "@/lib/logger";

const log = createLogger("useInitialStatesOverride");

export interface UseInitialStatesOverrideReturn {
  /**
   * Current override state IDs (null = use resolved from config)
   */
  overrideStateIds: string[] | null;

  /**
   * Set the override state IDs
   */
  setOverrideStateIds: (ids: string[] | null) => void;

  /**
   * Clear the override (revert to config-resolved states)
   */
  clearOverride: () => void;

  /**
   * Whether an override is currently active
   */
  hasOverride: boolean;

  /**
   * Get the effective source given the current override state
   */
  getEffectiveSource: (configSource: InitialStatesSource) => InitialStatesSource;
}

/**
 * Hook to manage session-only initial states override
 *
 * @param selectedWorkflow - Current workflow ID (override clears when this changes)
 * @param configVersion - Config version/load count (override clears when this changes)
 */
export function useInitialStatesOverride(
  selectedWorkflow: string,
  configVersion: number = 0,
): UseInitialStatesOverrideReturn {
  const [overrideStateIds, setOverrideStateIdsInternal] = useState<string[] | null>(null);

  // Clear override when workflow changes
  useEffect(() => {
    if (overrideStateIds !== null) {
      log.debug("Clearing override due to workflow change");

      setOverrideStateIdsInternal(null);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps -- Only trigger on workflow change, not on overrideStateIds
  }, [selectedWorkflow]);

  // Clear override when config is reloaded
  useEffect(() => {
    if (configVersion > 0 && overrideStateIds !== null) {
      log.debug("Clearing override due to config reload");

      setOverrideStateIdsInternal(null);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps -- Only trigger on config version change
  }, [configVersion]);

  /**
   * Set the override state IDs
   */
  const setOverrideStateIds = useCallback((ids: string[] | null) => {
    log.debug("Setting override:", ids);
    setOverrideStateIdsInternal(ids);
  }, []);

  /**
   * Clear the override
   */
  const clearOverride = useCallback(() => {
    log.debug("Clearing override");
    setOverrideStateIdsInternal(null);
  }, []);

  /**
   * Whether an override is currently active
   */
  const hasOverride = overrideStateIds !== null && overrideStateIds.length >= 0;

  /**
   * Get the effective source given the current override state
   */
  const getEffectiveSource = useCallback(
    (configSource: InitialStatesSource): InitialStatesSource => {
      if (hasOverride) {
        return "override";
      }
      return configSource;
    },
    [hasOverride],
  );

  return {
    overrideStateIds,
    setOverrideStateIds,
    clearOverride,
    hasOverride,
    getEffectiveSource,
  };
}

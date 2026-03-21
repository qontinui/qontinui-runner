/**
 * useSubStepProgress Hook
 *
 * Manages sub-step progress state and subscribes to sub-step events
 * from the Rust backend. Provides granular visibility into AI session progress.
 */

import { useEffect, useState, useCallback, useRef } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { createLogger } from "@/lib/logger";

const logger = createLogger("SubStepProgress");
import {
  type SubStepInfo,
  type SubStepStatus_Display,
  type RawSubStepCompleteEvent,
  type RawSubStepStartedEvent,
  createDefaultSubStepStatus,
} from "../types/executionStatus";

/** Maximum number of sub-steps to keep in history */
const MAX_SUB_STEP_HISTORY = 100;

export interface UseSubStepProgressReturn {
  /** Current sub-step progress status */
  subSteps: SubStepStatus_Display;
  /** Clear all sub-step progress */
  clearProgress: () => void;
  /** Whether currently connected to events */
  isConnected: boolean;
  /** Reset for a new phase/batch */
  resetForPhase: (phase: string) => void;
}

export function useSubStepProgress(): UseSubStepProgressReturn {
  const [subSteps, setSubSteps] = useState<SubStepStatus_Display>(createDefaultSubStepStatus);
  const [isConnected, setIsConnected] = useState(false);
  const unlistenCompleteRef = useRef<UnlistenFn | null>(null);
  const unlistenStartedRef = useRef<UnlistenFn | null>(null);

  // Handle sub-step started event
  const handleSubStepStarted = useCallback((event: RawSubStepStartedEvent) => {
    const newStep: SubStepInfo = {
      id: event.sub_step_id,
      parentId: event.checkpoint_id,
      name: event.description || `Step ${event.sub_step_index + 1}`,
      status: "running",
      index: event.sub_step_index,
      totalCount: event.total_sub_steps,
      startedAt: event.timestamp,
      completedAt: null,
      durationMs: null,
      phase: event.phase || undefined,
    };

    setSubSteps((prev) => {
      // Check if this is a new batch (different total or reset)
      const isNewBatch = prev.totalCount !== event.total_sub_steps || event.sub_step_index === 0;

      const existingSteps = isNewBatch ? [] : prev.steps;
      const updatedSteps = [...existingSteps];

      // Update or add the step
      const existingIndex = updatedSteps.findIndex((s) => s.id === event.sub_step_id);
      if (existingIndex >= 0) {
        updatedSteps[existingIndex] = newStep;
      } else {
        updatedSteps.push(newStep);
      }

      // Keep history limited
      const limitedSteps = updatedSteps.slice(-MAX_SUB_STEP_HISTORY);

      const completedCount = limitedSteps.filter((s) => s.status === "completed").length;
      const progressPercent =
        event.total_sub_steps > 0 ? (completedCount / event.total_sub_steps) * 100 : 0;

      return {
        current: newStep,
        steps: limitedSteps,
        progressPercent,
        completedCount,
        totalCount: event.total_sub_steps,
        isActive: true,
        currentPhase: event.phase || prev.currentPhase,
      };
    });
  }, []);

  // Handle sub-step complete event
  const handleSubStepComplete = useCallback((event: RawSubStepCompleteEvent) => {
    setSubSteps((prev) => {
      const updatedSteps = prev.steps.map((step) => {
        if (step.id === event.sub_step_id) {
          return {
            ...step,
            status: "completed" as const,
            completedAt: event.timestamp,
            durationMs: step.startedAt ? event.timestamp - step.startedAt : null,
            name: event.description || step.name,
          };
        }
        return step;
      });

      const completedCount = updatedSteps.filter((s) => s.status === "completed").length;
      const progressPercent = prev.totalCount > 0 ? (completedCount / prev.totalCount) * 100 : 0;

      // Check if all steps are complete
      const allComplete = completedCount >= prev.totalCount && prev.totalCount > 0;

      return {
        ...prev,
        current: allComplete ? null : prev.current,
        steps: updatedSteps,
        progressPercent,
        completedCount,
        isActive: !allComplete,
      };
    });
  }, []);

  // Subscribe to sub-step events
  useEffect(() => {
    let isMounted = true;

    const setupListeners = async () => {
      try {
        // Listen for sub_step_complete events
        const unlistenComplete = await listen<RawSubStepCompleteEvent>(
          "sub_step_complete",
          (event) => {
            if (isMounted) {
              handleSubStepComplete(event.payload);
            }
          },
        );

        // Listen for sub_step_started events (if backend emits them)
        const unlistenStarted = await listen<RawSubStepStartedEvent>(
          "sub_step_started",
          (event) => {
            if (isMounted) {
              handleSubStepStarted(event.payload);
            }
          },
        );

        if (isMounted) {
          unlistenCompleteRef.current = unlistenComplete;
          unlistenStartedRef.current = unlistenStarted;
          setIsConnected(true);
          logger.info("Connected to sub-step events");
        } else {
          unlistenComplete();
          unlistenStarted();
        }
      } catch (error) {
        console.error("[useSubStepProgress] Failed to set up event listeners:", error);
        if (isMounted) {
          setIsConnected(false);
        }
      }
    };

    setupListeners();

    return () => {
      isMounted = false;
      if (unlistenCompleteRef.current) {
        unlistenCompleteRef.current();
        unlistenCompleteRef.current = null;
      }
      if (unlistenStartedRef.current) {
        unlistenStartedRef.current();
        unlistenStartedRef.current = null;
      }
      setIsConnected(false);
    };
  }, [handleSubStepComplete, handleSubStepStarted]);

  // Clear progress
  const clearProgress = useCallback(() => {
    setSubSteps(createDefaultSubStepStatus());
  }, []);

  // Reset for a new phase
  const resetForPhase = useCallback((phase: string) => {
    setSubSteps({
      ...createDefaultSubStepStatus(),
      currentPhase: phase,
      isActive: true,
    });
  }, []);

  return {
    subSteps,
    clearProgress,
    isConnected,
    resetForPhase,
  };
}

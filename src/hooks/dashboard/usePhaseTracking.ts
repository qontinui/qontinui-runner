/**
 * usePhaseTracking Hook
 *
 * Tracks the current phase in the task iteration cycle.
 * Phases: setup -> verification -> ai_work
 */

import { useMemo } from "react";
import type { ActivityType, TaskPhase, ActivityStatus } from "../../types/dashboard/activity-types";

/**
 * Props for usePhaseTracking hook.
 */
export interface UsePhaseTrackingProps {
  /** Map of activity states */
  activities: Map<ActivityType, { status: ActivityStatus }>;
  /** Whether any task is running */
  isRunning: boolean;
  /** Whether the task has GUI automation setup */
  hasGuiSetup: boolean;
  /** Whether the task has Playwright setup */
  hasPlaywrightSetup: boolean;
  /** Whether the task has verification */
  hasVerification: boolean;
  /** Whether the task has AI work */
  hasAiWork: boolean;
}

/**
 * Result of phase tracking.
 */
export interface UsePhaseTrackingResult {
  /** Current phase in the iteration */
  currentPhase: TaskPhase;
  /** Whether to show the phase badge (some tasks don't use phases) */
  showPhaseBadge: boolean;
  /** Phases that are part of this task */
  taskPhases: TaskPhase[];
}

/**
 * Determine if an activity is currently running.
 */
function isActivityRunning(
  activities: Map<ActivityType, { status: ActivityStatus }>,
  type: ActivityType,
): boolean {
  const state = activities.get(type);
  return state?.status === "running";
}

/**
 * Hook for tracking the current phase in task execution.
 *
 * The phase is determined by which activity is currently running:
 * - setup: gui_automation or playwright is running (for setup, not verification)
 * - verification: verification activity is running
 * - ai_work: ai_conversation is running
 * - idle: nothing is running
 */
export function usePhaseTracking({
  activities,
  isRunning,
  hasGuiSetup,
  hasPlaywrightSetup,
  hasVerification,
  hasAiWork,
}: UsePhaseTrackingProps): UsePhaseTrackingResult {
  const result = useMemo(() => {
    // Determine which phases this task uses
    const taskPhases: TaskPhase[] = [];

    if (hasGuiSetup || hasPlaywrightSetup) {
      taskPhases.push("setup");
    }
    if (hasVerification) {
      taskPhases.push("verification");
    }
    if (hasAiWork) {
      taskPhases.push("ai_work");
    }

    // Only show phase badge if task has multiple phases
    const showPhaseBadge = taskPhases.length > 1;

    // Determine current phase based on running activities
    let currentPhase: TaskPhase = "idle";

    if (isRunning) {
      // Check in phase order: setup -> verification -> ai_work
      if (isActivityRunning(activities, "gui_automation")) {
        // GUI automation can be setup or verification
        // For now, assume it's setup unless verification is also detected
        // A more sophisticated approach would track the task iteration state
        currentPhase = "setup";
      } else if (isActivityRunning(activities, "playwright")) {
        // Playwright can be setup or verification
        currentPhase = "setup";
      } else if (isActivityRunning(activities, "verification")) {
        currentPhase = "verification";
      } else if (isActivityRunning(activities, "ai_conversation")) {
        currentPhase = "ai_work";
      } else if (isActivityRunning(activities, "findings")) {
        // Findings are generated during AI work
        currentPhase = "ai_work";
      }
    }

    return {
      currentPhase,
      showPhaseBadge,
      taskPhases,
    };
  }, [activities, isRunning, hasGuiSetup, hasPlaywrightSetup, hasVerification, hasAiWork]);

  return result;
}

export default usePhaseTracking;

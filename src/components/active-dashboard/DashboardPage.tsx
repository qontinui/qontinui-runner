/**
 * DashboardPage Component
 *
 * Main page component for the Active Dashboard.
 * Orchestrates the dynamic widget-based layout based on task activities.
 * Supports multi-run view with ActiveRunsBar for concurrent workflows.
 * Includes keyboard shortcuts and workflow completion summary overlay.
 */

import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useDashboardState, useWidgetPreferences } from "../../hooks/dashboard";
import { TaskProvider, SharedStepDataProvider, useActiveRunsOptional } from "../../contexts";
import { DashboardLayout } from "./DashboardLayout";
import { ControlBar } from "./ControlBar";
import { BottomBar } from "./BottomBar";
import { ActiveRunsBar } from "./ActiveRunsBar";
import { ShortcutsModal } from "./ShortcutsModal";
import { CompletionSummary } from "./CompletionSummary";
import { ApprovalDialog } from "./ApprovalDialog";
import { registerAllWidgets } from "./widgets";
import { useFlowExecutionData } from "./widgets/flow-execution";
import { windowManager } from "../../managers";
import type { ActivityType } from "../../types/dashboard/activity-types";
import type { DashboardStatus } from "../../hooks/dashboard/useDashboardState";
import type { StepStats } from "./widgets/shared/types";
import type { CommandResponse } from "../../types/displayProfile";
import { getApiBase, tracedFetch } from "@/lib/runner-api";

// Register widgets at module load time (before any component renders)
// This ensures widgets are available when hooks run
registerAllWidgets();

/**
 * Props for DashboardPage.
 */
export interface DashboardPageProps {
  /** Callback to navigate to the Execute page */
  onGoToExecute: () => void;
  /** Callback to navigate to the Recap page and set session workflow */
  onGoToRecap?: () => void;
  /** Callback to run the last workflow again */
  onRunLastWorkflow?: () => void;
  /** Whether the last workflow is currently being started */
  isRunningLastWorkflow?: boolean;
  /** Name of the last run workflow */
  lastRunWorkflowName?: string | null;
  /** ID of the last run workflow */
  lastRunWorkflowId?: string | null;
}

/**
 * Compute basic step stats from dashboard activity states.
 * Used for the completion summary when SharedStepDataContext is not available.
 */
function computeStatsFromActivities(activities: Map<ActivityType, { status: string }>): StepStats {
  const total = activities.size;
  let successful = 0;
  let failed = 0;
  let pending = 0;

  for (const [, activity] of activities) {
    if (activity.status === "completed" || activity.status === "success") {
      successful++;
    } else if (activity.status === "failed") {
      failed++;
    } else if (
      activity.status === "idle" ||
      activity.status === "pending" ||
      activity.status === "running"
    ) {
      pending++;
    }
  }

  const completed = successful + failed;
  const successRate = completed > 0 ? (successful / completed) * 100 : 100;

  return {
    total,
    completed,
    successful,
    failed,
    pending,
    elapsedTime: 0,
    successRate,
  };
}

/**
 * DashboardPage - The new Active Dashboard.
 *
 * Features:
 * - Dynamic widget selection based on task activities
 * - Active widget takes 65% of space with highlighted border
 * - Summary widgets stack on the right (35%)
 * - Phase tracking and iteration display
 * - Links to detail pages
 * - Keyboard shortcuts for quick navigation
 * - Completion summary overlay when workflow finishes
 */
export function DashboardPage({
  onGoToExecute,
  onGoToRecap,
  onRunLastWorkflow,
  isRunningLastWorkflow,
  lastRunWorkflowName,
  lastRunWorkflowId,
}: DashboardPageProps) {
  // Get dashboard state
  const { state, setActiveWidget, navigateToDetail, refresh } = useDashboardState();

  // Widget pin/hide preferences
  const widgetPreferences = useWidgetPreferences();

  // Get flow execution data for flow-specific controls
  const flowExecutionData = useFlowExecutionData();

  // Track paused state locally (for GUI automation)
  const [isPaused, setIsPaused] = useState(false);

  // --- Shortcuts modal state ---
  const [showShortcuts, setShowShortcuts] = useState(false);

  // --- Completion summary state ---
  const [showCompletion, setShowCompletion] = useState(false);
  const [completionStatus, setCompletionStatus] = useState<DashboardStatus>("idle");
  const [completionStats, setCompletionStats] = useState<StepStats>({
    total: 0,
    completed: 0,
    successful: 0,
    failed: 0,
    pending: 0,
    elapsedTime: 0,
    successRate: 100,
  });
  const [completionDuration, setCompletionDuration] = useState(0);
  const prevIsRunningRef = useRef(state.isRunning);
  const taskStartTimeRef = useRef<number | null>(null);

  // Track task start time when a task begins running
  useEffect(() => {
    if (state.isRunning && !prevIsRunningRef.current) {
      taskStartTimeRef.current = Date.now();
    }
  }, [state.isRunning]);

  // Detect running -> not-running transition to show completion summary
  useEffect(() => {
    const wasRunning = prevIsRunningRef.current;
    prevIsRunningRef.current = state.isRunning;

    if (wasRunning && !state.isRunning) {
      // Workflow just finished
      const finalStatus =
        state.status === "completed" || state.status === "failed" ? state.status : "completed";
      setCompletionStatus(finalStatus);
      setCompletionStats(computeStatsFromActivities(state.layout.activities));

      // Calculate duration from tracked start time
      const startTime = taskStartTimeRef.current;
      if (startTime) {
        setCompletionDuration(Math.round((Date.now() - startTime) / 1000));
      } else {
        setCompletionDuration(0);
      }

      setShowCompletion(true);
    }
  }, [state.isRunning, state.status, state.layout.activities]);

  const handleDismissCompletion = useCallback(() => {
    setShowCompletion(false);
  }, []);

  // Determine if a flow is currently active and should receive control commands
  const isFlowActive =
    flowExecutionData.isActive &&
    flowExecutionData.instanceId !== null &&
    state.layout.activeWidget === "flow_execution";

  // Handle widget click (switch active widget)
  const handleWidgetClick = useCallback(
    (type: ActivityType) => {
      setActiveWidget(type);
    },
    [setActiveWidget],
  );

  // Handle navigation to detail page
  const handleNavigateToDetail = useCallback(
    (type: ActivityType) => {
      navigateToDetail(type);
    },
    [navigateToDetail],
  );

  // Handle stop execution - works for unified workflows, GUI automation, and flow execution
  const handleStop = useCallback(async () => {
    console.log("[DASHBOARD] Stop execution called");
    const taskId = state.taskInfo?.taskId;
    let stopped = false;

    // 1. If a flow is active, cancel it via Tauri command
    if (isFlowActive && flowExecutionData.instanceId) {
      try {
        console.log(`[DASHBOARD] Cancelling flow execution: ${flowExecutionData.instanceId}`);
        const result = await invoke<boolean>("cancel_flow_execution", {
          instanceId: flowExecutionData.instanceId,
        });
        if (result) {
          console.log("[DASHBOARD] Flow execution cancelled successfully");
          stopped = true;
        }
      } catch (error) {
        console.error("[DASHBOARD] Failed to cancel flow execution:", error);
      }
    }

    // 2. Stop unified workflow (AI task) via HTTP API if we have a task ID
    if (taskId) {
      try {
        console.log(`[DASHBOARD] Stopping unified workflow: ${taskId}`);
        const response = await tracedFetch(`${getApiBase()}/task-runs/${taskId}/stop`, {
          method: "POST",
          headers: { "Content-Type": "application/json" },
        });
        if (response.ok) {
          const data = await response.json();
          if (data.success) {
            console.log("[DASHBOARD] Unified workflow stopped successfully");
            stopped = true;
          }
        }
      } catch (error) {
        console.error("[DASHBOARD] Failed to stop unified workflow:", error);
      }
    }

    // 3. Also try to stop GUI automation via Tauri command
    try {
      const result = await invoke<CommandResponse>("stop_execution");
      if (result.success) {
        console.log("[DASHBOARD] GUI execution stopped successfully");
        stopped = true;
      }
    } catch {
      // This may fail if no GUI execution is running, which is fine
      console.log("[DASHBOARD] No GUI execution to stop (or already stopped)");
    }

    if (stopped) {
      setIsPaused(false); // Reset pause state on stop
      // Restore window if it was auto-minimized
      await windowManager.restoreIfMinimized();
    }
  }, [state.taskInfo?.taskId, isFlowActive, flowExecutionData.instanceId]);

  // Handle play/pause toggle - works for GUI automation and flow execution
  const handlePlayPause = useCallback(async () => {
    // Check if we should handle flow execution pause/resume
    if (isFlowActive && flowExecutionData.instanceId) {
      try {
        const flowIsPaused =
          flowExecutionData.status === "paused" || flowExecutionData.status === "waiting_for_input";
        if (flowIsPaused) {
          console.log(`[DASHBOARD] Resume flow execution: ${flowExecutionData.instanceId}`);
          const result = await invoke<boolean>("resume_flow_execution", {
            instanceId: flowExecutionData.instanceId,
          });
          if (result) {
            console.log("[DASHBOARD] Flow execution resumed successfully");
          }
        } else {
          console.log(`[DASHBOARD] Pause flow execution: ${flowExecutionData.instanceId}`);
          const result = await invoke<boolean>("pause_flow_execution", {
            instanceId: flowExecutionData.instanceId,
          });
          if (result) {
            console.log("[DASHBOARD] Flow execution paused successfully");
          }
        }
      } catch (error) {
        console.error("[DASHBOARD] Failed to pause/resume flow execution:", error);
      }
      return;
    }

    // Handle GUI automation pause/resume
    try {
      if (isPaused) {
        console.log("[DASHBOARD] Resume execution called");
        const result = await invoke<CommandResponse>("resume_execution");
        if (result.success) {
          console.log("[DASHBOARD] Execution resumed successfully");
          setIsPaused(false);
        }
      } else {
        console.log("[DASHBOARD] Pause execution called");
        const result = await invoke<CommandResponse>("pause_execution");
        if (result.success) {
          console.log("[DASHBOARD] Execution paused successfully");
          setIsPaused(true);
        }
      }
    } catch {
      // Pause/resume only works for GUI automation via Python bridge
      // Unified workflows (AI tasks) don't support pause yet
      console.log("[DASHBOARD] Pause/resume not available for this workflow type");
    }
  }, [isPaused, isFlowActive, flowExecutionData.instanceId, flowExecutionData.status]);

  // Get current action text for bottom bar
  const _currentAction = state.layout.activeWidget
    ? state.layout.activities.get(state.layout.activeWidget)?.type
    : null;

  // Access active runs context if available (for multi-run support)
  const activeRunsContext = useActiveRunsOptional();
  const hasMultipleRuns = activeRunsContext?.hasMultipleRuns ?? false;

  // Handler for "New Run" button in ActiveRunsBar
  const handleNewRun = useCallback(() => {
    // Navigate to execute page to start a new run
    onGoToExecute();
  }, [onGoToExecute]);

  // Determine the display status - prioritize flow execution status when a flow is active
  const displayStatus = (() => {
    if (isFlowActive) {
      // Map flow execution status to dashboard status
      switch (flowExecutionData.status) {
        case "running":
          return "running";
        case "paused":
        case "waiting_for_input":
          return "paused";
        case "completed":
          return "completed";
        case "failed":
          return "failed";
        case "idle":
        default:
          return state.status;
      }
    }
    // For GUI automation, use local isPaused state
    if (isPaused && state.status === "running") {
      return "paused";
    }
    return state.status;
  })();

  // --- Keyboard shortcuts ---
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      // Ignore shortcuts when typing in an input element
      const target = e.target as HTMLElement;
      if (
        target.tagName === "INPUT" ||
        target.tagName === "TEXTAREA" ||
        target.tagName === "SELECT" ||
        target.isContentEditable
      ) {
        return;
      }

      // ? key (no modifiers) -> toggle shortcuts modal
      if (e.key === "?" && !e.ctrlKey && !e.metaKey && !e.altKey) {
        e.preventDefault();
        setShowShortcuts((prev) => !prev);
        return;
      }

      // Ctrl+1..8 -> switch widget by position
      if ((e.ctrlKey || e.metaKey) && e.key >= "1" && e.key <= "8") {
        e.preventDefault();
        const index = parseInt(e.key) - 1;
        const widgets = state.layout.detectedWidgets;
        if (index < widgets.length) {
          setActiveWidget(widgets[index]);
        }
        return;
      }

      // Ctrl+R -> refresh data
      if ((e.ctrlKey || e.metaKey) && e.key === "r") {
        e.preventDefault();
        refresh();
        return;
      }
    };

    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, [state.layout.detectedWidgets, setActiveWidget, refresh]);

  return (
    <TaskProvider taskInfo={state.taskInfo} isRunning={state.isRunning}>
      <SharedStepDataProvider>
        <div className="flex h-full flex-col bg-background">
          {/* Control Bar - always visible */}
          <ControlBar
            taskName={
              isFlowActive
                ? (flowExecutionData.flowName ?? state.taskInfo?.taskName ?? null)
                : (state.taskInfo?.taskName ?? null)
            }
            phase={state.currentPhase}
            showPhaseBadge={state.showPhaseBadge}
            status={displayStatus}
            workflowStage={state.workflowStage}
            isOrchestrated={state.isOrchestrated}
            iteration={state.isOrchestrated ? state.iteration : undefined}
            maxIterations={state.isOrchestrated ? state.maxIterations : undefined}
            isPlan={state.planPhaseName != null || state.planPhaseIndex != null}
            planPhaseName={state.planPhaseName}
            planPhaseIndex={state.planPhaseIndex}
            planTotalPhases={state.planTotalPhases}
            currentStageIndex={state.currentStageIndex}
            currentStageName={state.currentStageName}
            totalStages={state.totalStages}
            onPlayPause={handlePlayPause}
            onStop={handleStop}
          />

          {/* Main Content Area */}
          <div className="flex-1 overflow-hidden">
            <DashboardLayout
              layout={state.layout}
              onWidgetClick={handleWidgetClick}
              onNavigateToDetail={handleNavigateToDetail}
              onGoToRecap={onGoToRecap}
              onRunLastWorkflow={onRunLastWorkflow}
              isRunningLastWorkflow={isRunningLastWorkflow}
              lastRunWorkflowName={lastRunWorkflowName}
              lastRunWorkflowId={lastRunWorkflowId}
              widgetPreferences={widgetPreferences}
            />
          </div>

          {/* Active Runs Bar - shown when multiple runs are active */}
          {hasMultipleRuns && <ActiveRunsBar onNewRun={handleNewRun} />}

          {/* Bottom Bar - always visible */}
          <BottomBar
            activeActivity={state.layout.activeWidget}
            isRunning={state.isRunning}
            currentOrchestratorAgent={state.currentOrchestratorAgent}
            taskStartTime={state.taskInfo?.startTime ?? null}
          />

          {/* Shortcuts Modal */}
          <ShortcutsModal isOpen={showShortcuts} onClose={() => setShowShortcuts(false)} />

          {/* Approval Dialog Overlay - shown when workflow is paused for human review */}
          {state.taskInfo?.taskId && (
            <ApprovalDialog taskRunId={state.taskInfo.taskId} onResolved={() => refresh()} />
          )}

          {/* Completion Summary Overlay */}
          <CompletionSummary
            isOpen={showCompletion}
            onDismiss={handleDismissCompletion}
            status={completionStatus}
            stats={completionStats}
            durationSeconds={completionDuration}
          />
        </div>
      </SharedStepDataProvider>
    </TaskProvider>
  );
}

export default DashboardPage;

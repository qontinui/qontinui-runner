/**
 * DashboardPage Component
 *
 * Main page component for the Active Dashboard.
 * Orchestrates the dynamic widget-based layout based on task activities.
 * Supports multi-run view with ActiveRunsBar for concurrent workflows.
 */

import { useCallback, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useDashboardState } from "../../hooks/dashboard";
import { TaskProvider, useActiveRunsOptional } from "../../contexts";
import { DashboardLayout } from "./DashboardLayout";
import { ControlBar } from "./ControlBar";
import { BottomBar } from "./BottomBar";
import { ActiveRunsBar } from "./ActiveRunsBar";
import { registerAllWidgets } from "./widgets";
import { useFlowExecutionData } from "./widgets/flow-execution";
import { windowManager } from "../../managers";
import type { ActivityType } from "../../types/dashboard/activity-types";
import type { CommandResponse } from "../../types/displayProfile";

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
  /** Name of the last run workflow */
  lastRunWorkflowName?: string | null;
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
 */
export function DashboardPage({
  onGoToExecute,
  onGoToRecap,
  lastRunWorkflowName,
}: DashboardPageProps) {
  // Get dashboard state
  const { state, setActiveWidget, navigateToDetail } = useDashboardState();

  // Get flow execution data for flow-specific controls
  const flowExecutionData = useFlowExecutionData();

  // Track paused state locally (for GUI automation)
  const [isPaused, setIsPaused] = useState(false);

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
        const response = await fetch(`http://localhost:9876/task-runs/${taskId}/stop`, {
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
        const flowIsPaused = flowExecutionData.status === "paused" || flowExecutionData.status === "waiting_for_input";
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

  return (
    <TaskProvider taskInfo={state.taskInfo} isRunning={state.isRunning}>
      <div className="flex h-full flex-col bg-background">
        {/* Control Bar - always visible */}
        <ControlBar
          taskName={isFlowActive ? (flowExecutionData.flowName ?? state.taskInfo?.taskName ?? null) : (state.taskInfo?.taskName ?? null)}
          phase={state.currentPhase}
          showPhaseBadge={state.showPhaseBadge}
          status={displayStatus}
          workflowStage={state.workflowStage}
          isOrchestrated={state.isOrchestrated}
          iteration={state.isOrchestrated ? state.iteration : undefined}
          maxIterations={state.isOrchestrated ? state.maxIterations : undefined}
          onPlayPause={handlePlayPause}
          onStop={handleStop}
        />

        {/* Main Content Area */}
        <div className="flex-1 overflow-hidden">
          <DashboardLayout
            layout={state.layout}
            onWidgetClick={handleWidgetClick}
            onNavigateToDetail={handleNavigateToDetail}
            onGoToExecute={onGoToExecute}
            onGoToRecap={onGoToRecap}
            lastRunWorkflowName={lastRunWorkflowName}
          />
        </div>

        {/* Active Runs Bar - shown when multiple runs are active */}
        {hasMultipleRuns && <ActiveRunsBar onNewRun={handleNewRun} />}

        {/* Bottom Bar - always visible */}
        <BottomBar
          iteration={state.isOrchestrated ? state.iteration : (state.taskInfo?.iteration ?? 1)}
          maxIterations={state.isOrchestrated ? state.maxIterations : (state.taskInfo?.maxIterations ?? 1)}
          activeActivity={state.layout.activeWidget}
          isRunning={state.isRunning}
          currentOrchestratorAgent={state.currentOrchestratorAgent}
        />
      </div>
    </TaskProvider>
  );
}

export default DashboardPage;

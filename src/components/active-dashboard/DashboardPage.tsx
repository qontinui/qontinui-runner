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

  // Track paused state locally
  const [isPaused, setIsPaused] = useState(false);

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

  // Handle stop execution - works for both unified workflows and GUI automation
  const handleStop = useCallback(async () => {
    console.log("[DASHBOARD] Stop execution called");
    const taskId = state.taskInfo?.taskId;
    let stopped = false;

    // 1. Stop unified workflow (AI task) via HTTP API if we have a task ID
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

    // 2. Also try to stop GUI automation via Tauri command
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
  }, [state.taskInfo?.taskId]);

  // Handle play/pause toggle - only works for GUI automation, not unified workflows
  const handlePlayPause = useCallback(async () => {
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
  }, [isPaused]);

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

  return (
    <TaskProvider taskInfo={state.taskInfo} isRunning={state.isRunning}>
      <div className="flex h-full flex-col bg-background">
        {/* Control Bar - always visible */}
        <ControlBar
          taskName={state.taskInfo?.taskName ?? null}
          phase={state.currentPhase}
          showPhaseBadge={state.showPhaseBadge}
          status={isPaused && state.status === "running" ? "paused" : state.status}
          workflowStage={state.workflowStage}
          isOrchestrated={state.isOrchestrated}
          isComplete={state.status === "completed"}
          isFailed={state.status === "failed"}
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

/**
 * DashboardPage Component
 *
 * Main page component for the Active Dashboard.
 * Orchestrates the dynamic widget-based layout based on task activities.
 */

import { useCallback, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useDashboardState } from "../../hooks/dashboard";
import { TaskProvider } from "../../contexts";
import { DashboardLayout } from "./DashboardLayout";
import { ControlBar } from "./ControlBar";
import { BottomBar } from "./BottomBar";
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

  // Handle stop execution
  const handleStop = useCallback(async () => {
    try {
      console.log("[DASHBOARD] Stop execution called");
      const result = await invoke<CommandResponse>("stop_execution");
      if (result.success) {
        console.log("[DASHBOARD] Execution stopped successfully");
        setIsPaused(false); // Reset pause state on stop
        // Restore window if it was auto-minimized
        await windowManager.restoreIfMinimized();
      }
    } catch (error) {
      console.error("[DASHBOARD] Failed to stop execution:", error);
    }
  }, []);

  // Handle play/pause toggle
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
    } catch (error) {
      console.error("[DASHBOARD] Failed to toggle pause:", error);
    }
  }, [isPaused]);

  // Get current action text for bottom bar
  const _currentAction = state.layout.activeWidget
    ? state.layout.activities.get(state.layout.activeWidget)?.type
    : null;

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

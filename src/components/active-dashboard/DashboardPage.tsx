/**
 * DashboardPage Component
 *
 * Main page component for the Active Dashboard.
 * Orchestrates the dynamic widget-based layout based on task activities.
 */

import { useCallback } from "react";
import { useDashboardState } from "../../hooks/dashboard";
import { DashboardLayout } from "./DashboardLayout";
import { ControlBar } from "./ControlBar";
import { BottomBar } from "./BottomBar";
import { registerAllWidgets } from "./widgets";
import type { ActivityType } from "../../types/dashboard/activity-types";

// Register widgets at module load time (before any component renders)
// This ensures widgets are available when hooks run
registerAllWidgets();

/**
 * Props for DashboardPage.
 */
export interface DashboardPageProps {
  /** Callback to navigate to the Execute page */
  onGoToExecute: () => void;
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
export function DashboardPage({ onGoToExecute }: DashboardPageProps) {
  // Get dashboard state
  const { state, setActiveWidget, navigateToDetail } = useDashboardState();

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

  // Get current action text for bottom bar
  const _currentAction = state.layout.activeWidget
    ? state.layout.activities.get(state.layout.activeWidget)?.type
    : null;

  return (
    <div className="flex h-full flex-col bg-background">
      {/* Control Bar - always visible */}
      <ControlBar
        taskName={state.taskInfo?.taskName ?? null}
        phase={state.currentPhase}
        showPhaseBadge={state.showPhaseBadge}
        status={state.status}
      />

      {/* Main Content Area */}
      <div className="flex-1 overflow-hidden">
        <DashboardLayout
          layout={state.layout}
          onWidgetClick={handleWidgetClick}
          onNavigateToDetail={handleNavigateToDetail}
          onGoToExecute={onGoToExecute}
        />
      </div>

      {/* Bottom Bar - always visible */}
      <BottomBar
        iteration={state.taskInfo?.iteration ?? 1}
        maxIterations={state.taskInfo?.maxIterations ?? 1}
        activeActivity={state.layout.activeWidget}
        isRunning={state.isRunning}
        currentOrchestratorAgent={state.currentOrchestratorAgent}
      />
    </div>
  );
}

export default DashboardPage;

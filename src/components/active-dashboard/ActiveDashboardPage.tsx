/**
 * ActiveDashboardPage Component
 *
 * Main page component for the Active Dashboard.
 * Assembles the ControlBar, LiveExecutionView, StatusPanel, BottomBar,
 * and IdleState components into a cohesive live execution monitoring view.
 *
 * Layout:
 * - ControlBar: Top bar with workflow name, status badge, and playback controls
 * - Main area (when running):
 *   - LiveExecutionView (70%): Screenshot view + action stream
 *   - StatusPanel (30%): Summary stats, screenshots, image recognition, warnings
 * - Main area (when idle):
 *   - IdleState: Prompt to navigate to Execute page
 * - BottomBar: Iteration progress, current action, connection status
 */

import { ControlBar } from "./ControlBar";
import { LiveExecutionView } from "./LiveExecutionView";
import { StatusPanel } from "./StatusPanel";
import { BottomBar } from "./BottomBar";
import { IdleState } from "./IdleState";
import { useDashboardState } from "./useDashboardState";

export interface ActiveDashboardPageProps {
  /** Callback to navigate to the Execute tab/page */
  onGoToExecute: () => void;
}

export function ActiveDashboardPage({ onGoToExecute }: ActiveDashboardPageProps) {
  const dashboard = useDashboardState();

  const isIdle = dashboard.executionState.status === "idle";

  return (
    <div className="flex h-full flex-col bg-background">
      {/* Control Bar - always visible */}
      <ControlBar
        workflowName={dashboard.executionState.workflowName}
        status={dashboard.executionState.status}
        onPlayPause={dashboard.onPlayPause}
        onStop={dashboard.onStop}
        onGoToExecute={onGoToExecute}
      />

      {/* Main Content Area */}
      {isIdle ? (
        <IdleState onGoToExecute={onGoToExecute} />
      ) : (
        <div className="flex flex-1 overflow-hidden">
          {/* Left: Live Execution View (70%) */}
          <LiveExecutionView
            actions={dashboard.actions}
            status={dashboard.executionState.status}
            currentAction={dashboard.currentAction}
          />

          {/* Right: Status Panel (30%) */}
          <StatusPanel executionState={dashboard.executionState} />
        </div>
      )}

      {/* Bottom Bar - always visible */}
      <BottomBar
        currentAction={dashboard.currentAction}
        iteration={dashboard.executionState.iteration}
        maxIterations={dashboard.executionState.maxIterations}
      />
    </div>
  );
}

export default ActiveDashboardPage;

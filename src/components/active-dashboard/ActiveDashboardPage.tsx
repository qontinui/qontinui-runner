/**
 * ActiveDashboardPage Component
 *
 * Main page component for the Active Dashboard.
 * Uses the new dynamic widget-based system that shows only activities
 * relevant to the currently running task.
 *
 * Layout:
 * - ControlBar: Top bar with task name, phase badge, status, and controls
 * - Main area: Dynamic widget layout
 *   - Active widget (65%): Currently running activity
 *   - Summary widgets (35%): Other activities in compact view
 * - ActiveRunsBar: Multi-run switcher (shown when multiple runs are active)
 * - BottomBar: Iteration progress, current activity, connection status
 *
 * Widget Types:
 * - GUI Automation: Screenshot, action stream, image recognition
 * - Playwright: Test execution and results
 * - AI Conversation: Chat history and thinking indicator
 * - Verification: "Did the fix work?" results
 * - Findings: AI-detected issues
 *
 * Multi-Run Support:
 * - Wraps dashboard in ActiveRunsProvider for tracking concurrent runs
 * - Shows ActiveRunsBar when multiple runs are active
 * - Allows switching between runs to view their dashboards
 *
 * State Management:
 * - ActiveRunsProvider: Tracks multiple concurrent runs
 * - WorkflowExecutionProvider: Unified state store for workflow execution data
 *   (orchestrator state, checkpoints, progress markers, restart recovery)
 */

import { useCallback } from "react";
import { useUIComponent } from "@qontinui/ui-bridge";
import { invoke } from "@tauri-apps/api/core";
import { ActiveRunsProvider, WorkflowExecutionProvider } from "../../contexts";
import { DashboardPage, type DashboardPageProps } from "./DashboardPage";
import { getApiBase, tracedFetch } from "@/lib/runner-api";
import type { CommandResponse } from "../../types/displayProfile";
import { createLogger } from "@/lib/logger";

const log = createLogger("ActiveDashboard");

export type ActiveDashboardPageProps = DashboardPageProps;

/**
 * ActiveDashboardPage - The Active Dashboard entry point.
 *
 * This component wraps the DashboardPage with:
 * 1. ActiveRunsProvider - for tracking concurrent runs
 * 2. WorkflowExecutionProvider - for unified workflow state management
 */
export function ActiveDashboardPage(props: ActiveDashboardPageProps) {
  const { onGoToExecute } = props;
  const handleStartRun = useCallback(() => {
    // Navigate to the Execute page where runs can be started
    onGoToExecute();
  }, [onGoToExecute]);

  const handleStopRun = useCallback(async () => {
    // Stop any active execution via the GUI automation command
    try {
      const result = await invoke<CommandResponse>("stop_execution");
      if (result.success) {
        log.debug("Execution stopped via UI Bridge action");
      }
    } catch {
      log.debug("No active execution to stop");
    }
  }, []);

  const handleRefresh = useCallback(async () => {
    // Force a refresh by re-fetching dashboard data via the API
    try {
      await tracedFetch(`${getApiBase()}/status`);
      log.debug("Dashboard data refreshed via UI Bridge action");
    } catch (err) {
      console.error("[ActiveDashboard] Failed to refresh dashboard data:", err);
    }
  }, []);

  // UI Bridge: Component-level actions for AI control
  useUIComponent({
    id: "active-dashboard",
    name: "Active Dashboard",
    description: "Main dashboard for monitoring and controlling active workflow runs",
    actions: [
      {
        id: "start-run",
        label: "Start Run",
        // A fourth description added by Phase 2 beyond the three the plan named,
        // for the same reason: the label asserts an effect the handler does not
        // have, and a session establishing this action's effect from the
        // registry would have been misled. Same class as the three mislabelled
        // Session Manager controls recorded in
        // [policy: what-makes-an-action-destructive].
        description:
          "Navigate to the Execute page, where a run can be started. Does NOT start a " +
          "run — nothing is executed by invoking this.",
        // `read` — NAVIGATES to the Execute page (`onGoToExecute()`); it starts nothing.
        // The label says otherwise, which is exactly the label-is-not-a-specification
        // case [policy: what-makes-an-action-destructive]; hence the description above.
        effect: "read",
        handler: async () => {
          handleStartRun();
        },
      },
      {
        id: "stop-run",
        label: "Stop Run",
        // `destructive` — aborts an in-flight GUI automation run (`stop_execution`).
        // Dim 1: restarting does not undo what the run already clicked, and its partial
        // progress is not reconstructible. Dim 2: the run's state is not this view's.
        effect: "destructive",
        handler: async () => {
          await handleStopRun();
        },
      },
      {
        id: "refresh",
        label: "Refresh",
        // `read` — a GET against `/status`. Query, no state change.
        effect: "read",
        handler: async () => {
          await handleRefresh();
        },
      },
    ],
  });

  return (
    <ActiveRunsProvider>
      <WorkflowExecutionProvider>
        <DashboardPage {...props} />
      </WorkflowExecutionProvider>
    </ActiveRunsProvider>
  );
}

export default ActiveDashboardPage;

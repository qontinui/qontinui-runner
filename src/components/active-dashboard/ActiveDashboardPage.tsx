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
import { useUIComponent } from "ui-bridge";
import { invoke } from "@tauri-apps/api/core";
import { ActiveRunsProvider, WorkflowExecutionProvider } from "../../contexts";
import { DashboardPage, type DashboardPageProps } from "./DashboardPage";
import { getApiBase, tracedFetch } from "@/lib/runner-api";
import type { CommandResponse } from "../../types/displayProfile";

export type ActiveDashboardPageProps = DashboardPageProps;

/**
 * ActiveDashboardPage - The Active Dashboard entry point.
 *
 * This component wraps the DashboardPage with:
 * 1. ActiveRunsProvider - for tracking concurrent runs
 * 2. WorkflowExecutionProvider - for unified workflow state management
 */
export function ActiveDashboardPage(props: ActiveDashboardPageProps) {
  const handleStartRun = useCallback(() => {
    // Navigate to the Execute page where runs can be started
    props.onGoToExecute();
  }, [props.onGoToExecute]);

  const handleStopRun = useCallback(async () => {
    // Stop any active execution via the GUI automation command
    try {
      const result = await invoke<CommandResponse>("stop_execution");
      if (result.success) {
        console.log("[ActiveDashboard] Execution stopped via UI Bridge action");
      }
    } catch {
      console.log("[ActiveDashboard] No active execution to stop");
    }
  }, []);

  const handleRefresh = useCallback(async () => {
    // Force a refresh by re-fetching dashboard data via the API
    try {
      await tracedFetch(`${getApiBase()}/status`);
      console.log("[ActiveDashboard] Dashboard data refreshed via UI Bridge action");
    } catch (err) {
      console.error("[ActiveDashboard] Failed to refresh dashboard data:", err);
    }
  }, []);

  // UI Bridge: Component-level actions for AI control
  useUIComponent({
    id: 'active-dashboard',
    name: 'Active Dashboard',
    description: 'Main dashboard for monitoring and controlling active workflow runs',
    actions: [
      {
        id: 'start-run',
        label: 'Start Run',
        handler: async () => {
          handleStartRun();
        },
      },
      {
        id: 'stop-run',
        label: 'Stop Run',
        handler: async () => {
          await handleStopRun();
        },
      },
      {
        id: 'refresh',
        label: 'Refresh',
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

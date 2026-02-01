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
 */

import { ActiveRunsProvider } from "../../contexts";
import { DashboardPage, type DashboardPageProps } from "./DashboardPage";

export type ActiveDashboardPageProps = DashboardPageProps;

/**
 * ActiveDashboardPage - The Active Dashboard entry point.
 *
 * This component wraps the DashboardPage with ActiveRunsProvider
 * to enable multi-run support.
 */
export function ActiveDashboardPage(props: ActiveDashboardPageProps) {
  return (
    <ActiveRunsProvider>
      <DashboardPage {...props} />
    </ActiveRunsProvider>
  );
}

export default ActiveDashboardPage;

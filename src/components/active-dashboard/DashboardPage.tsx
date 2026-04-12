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
import { OrchestrationLoopBanner } from "./OrchestrationLoopBanner";
import { TopPatternsWidget } from "./TopPatternsWidget";
import { BreakpointInspector } from "./BreakpointInspector";
import { registerAllWidgets } from "@/components/widgets";
import { useFlowExecutionData } from "@/components/widgets/flow-execution";
import { windowManager } from "../../managers";
import type { ActivityType } from "../../types/dashboard/activity-types";
import type { DashboardStatus } from "../../hooks/dashboard/useDashboardState";
import type { StepStats } from "@/components/widgets/shared/types";
import type { CommandResponse } from "../../types/displayProfile";
import type { BreakpointSnapshot } from "./types";
import { getApiBase, tracedFetch, fetchBreakpoints, resumeBreakpoint } from "@/lib/runner-api";
import { useTaskRunControls } from "@/hooks/graphql";
import { createLogger } from "@/lib/logger";

const log = createLogger("Dashboard");

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

  // GraphQL mutations for task run lifecycle (stop, pause, unpause)
  const taskControls = useTaskRunControls();

  // Get flow execution data for flow-specific controls
  const flowExecutionData = useFlowExecutionData();

  // Track paused state locally (for GUI automation and unified workflows)
  const [isPaused, setIsPaused] = useState(false);

  // Sync isPaused state with backend when task ID changes
  useEffect(() => {
    const taskId = state.taskInfo?.taskId;
    if (!taskId) {
      // eslint-disable-next-line react-hooks/set-state-in-effect -- reset on task change
      setIsPaused(false);
      return;
    }

    const checkPauseState = async () => {
      try {
        const response = await tracedFetch(`${getApiBase()}/task-runs/${taskId}/workflow-state`);
        if (response.ok) {
          const data = await response.json();
          setIsPaused(data.is_paused === true);
        }
      } catch {
        // Ignore - workflow state may not be available yet
      }
    };

    checkPauseState();
  }, [state.taskInfo?.taskId]);

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
      // Workflow just finished — batch state updates for completion overlay
      /* eslint-disable react-hooks/set-state-in-effect -- state machine transition detection */
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
      /* eslint-enable react-hooks/set-state-in-effect */
    }
  }, [state.isRunning, state.status, state.layout.activities]);

  const handleDismissCompletion = useCallback(() => {
    setShowCompletion(false);
  }, []);

  // --- Breakpoint state ---
  const [breakpointSnapshot, setBreakpointSnapshot] = useState<BreakpointSnapshot | null>(null);
  const [showBreakpointInspector, setShowBreakpointInspector] = useState(false);
  const breakpointPollRef = useRef<ReturnType<typeof setInterval> | null>(null);

  // Poll for active breakpoints while a task is running
  useEffect(() => {
    const taskId = state.taskInfo?.taskId;
    if (!taskId || !state.isRunning) {
      setBreakpointSnapshot(null);
      if (breakpointPollRef.current) {
        clearInterval(breakpointPollRef.current);
        breakpointPollRef.current = null;
      }
      return;
    }

    const pollBreakpoints = async () => {
      try {
        const snapshots = await fetchBreakpoints(taskId);
        const waiting = snapshots.find((s) => s.status === "waiting") ?? null;
        // eslint-disable-next-line react-hooks/set-state-in-effect -- polling callback
        setBreakpointSnapshot(waiting);
      } catch {
        // Endpoint may not exist yet or task has no breakpoints - ignore
      }
    };

    // Initial poll
    pollBreakpoints();
    // Poll every 3 seconds
    breakpointPollRef.current = setInterval(pollBreakpoints, 3000);

    return () => {
      if (breakpointPollRef.current) {
        clearInterval(breakpointPollRef.current);
        breakpointPollRef.current = null;
      }
    };
  }, [state.taskInfo?.taskId, state.isRunning]);

  const handleResumeBreakpoint = useCallback(async () => {
    const taskId = state.taskInfo?.taskId;
    if (!taskId || !breakpointSnapshot) return;

    try {
      await resumeBreakpoint(taskId, breakpointSnapshot.id);
      setBreakpointSnapshot(null);
      setShowBreakpointInspector(false);
      log.debug(`Resumed breakpoint ${breakpointSnapshot.id}`);
    } catch (error) {
      console.error("[Dashboard] Failed to resume breakpoint:", error);
    }
  }, [state.taskInfo?.taskId, breakpointSnapshot]);

  const handleInspectBreakpoint = useCallback(() => {
    setShowBreakpointInspector(true);
  }, []);

  const handleCloseBreakpointInspector = useCallback(() => {
    setShowBreakpointInspector(false);
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
    log.debug("Stop execution called");
    const taskId = state.taskInfo?.taskId;
    let stopped = false;

    // 1. If a flow is active, cancel it via Tauri command
    if (isFlowActive && flowExecutionData.instanceId) {
      try {
        log.debug(`Cancelling flow execution: ${flowExecutionData.instanceId}`);
        const result = await invoke<boolean>("cancel_flow_execution", {
          instanceId: flowExecutionData.instanceId,
        });
        if (result) {
          log.debug("Flow execution cancelled successfully");
          stopped = true;
        }
      } catch (error) {
        console.error("[Dashboard] Failed to cancel flow execution:", error);
      }
    }

    // 2. Stop unified workflow (AI task) via GraphQL mutation
    if (taskId) {
      try {
        log.debug(`Stopping unified workflow: ${taskId}`);
        const success = await taskControls.stop(taskId);
        if (success) {
          log.debug("Unified workflow stopped successfully via GraphQL");
          stopped = true;
        }
      } catch (error) {
        console.error("[Dashboard] Failed to stop unified workflow:", error);
      }
    }

    // 3. Also try to stop GUI automation via Tauri command
    try {
      const result = await invoke<CommandResponse>("stop_execution");
      if (result.success) {
        log.debug("GUI execution stopped successfully");
        stopped = true;
      }
    } catch {
      // This may fail if no GUI execution is running, which is fine
      log.debug("No GUI execution to stop (or already stopped)");
    }

    if (stopped) {
      setIsPaused(false); // Reset pause state on stop
      // Restore window if it was auto-minimized
      await windowManager.restoreIfMinimized();
    }
  }, [state.taskInfo?.taskId, isFlowActive, flowExecutionData.instanceId, taskControls]);

  // Handle play/pause toggle - works for GUI automation and flow execution
  const handlePlayPause = useCallback(async () => {
    // Check if we should handle flow execution pause/resume
    if (isFlowActive && flowExecutionData.instanceId) {
      try {
        const flowIsPaused =
          flowExecutionData.status === "paused" || flowExecutionData.status === "waiting_for_input";
        if (flowIsPaused) {
          log.debug(`Resume flow execution: ${flowExecutionData.instanceId}`);
          const result = await invoke<boolean>("resume_flow_execution", {
            instanceId: flowExecutionData.instanceId,
          });
          if (result) {
            log.debug("Flow execution resumed successfully");
          }
        } else {
          log.debug(`Pause flow execution: ${flowExecutionData.instanceId}`);
          const result = await invoke<boolean>("pause_flow_execution", {
            instanceId: flowExecutionData.instanceId,
          });
          if (result) {
            log.debug("Flow execution paused successfully");
          }
        }
      } catch (error) {
        console.error("[Dashboard] Failed to pause/resume flow execution:", error);
      }
      return;
    }

    // Handle unified workflow (AI task) pause/resume via REST API
    const taskId = state.taskInfo?.taskId;
    if (taskId) {
      try {
        // Check if the task is currently paused by querying the workflow state
        const stateResponse = await tracedFetch(
          `${getApiBase()}/task-runs/${taskId}/workflow-state`,
        );
        if (stateResponse.ok) {
          const stateData = await stateResponse.json();
          const taskIsPaused = stateData.is_paused === true;

          if (taskIsPaused) {
            log.debug(`Unpausing unified workflow: ${taskId}`);
            const success = await taskControls.unpause(taskId);
            if (success) {
              log.debug("Unified workflow resumed successfully via GraphQL");
              setIsPaused(false);
              return;
            }
          } else {
            log.debug(`Pausing unified workflow: ${taskId}`);
            const success = await taskControls.pause(taskId);
            if (success) {
              log.debug("Unified workflow paused successfully via GraphQL");
              setIsPaused(true);
              return;
            }
          }
        }
      } catch (error) {
        console.error("[Dashboard] Failed to pause/resume unified workflow:", error);
      }
      // Do not fall through to GUI automation IPC for unified workflows
      return;
    }

    // Fallback: Handle GUI automation pause/resume via Tauri IPC
    try {
      if (isPaused) {
        log.debug("Resume execution called");
        const result = await invoke<CommandResponse>("resume_execution");
        if (result.success) {
          log.debug("Execution resumed successfully");
          setIsPaused(false);
        }
      } else {
        log.debug("Pause execution called");
        const result = await invoke<CommandResponse>("pause_execution");
        if (result.success) {
          log.debug("Execution paused successfully");
          setIsPaused(true);
        }
      }
    } catch {
      log.debug("Pause/resume not available for this workflow type");
    }
  }, [
    isPaused,
    state.taskInfo?.taskId,
    isFlowActive,
    flowExecutionData.instanceId,
    flowExecutionData.status,
    taskControls,
  ]);

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
            breakpointSnapshot={breakpointSnapshot}
            onResumeBreakpoint={handleResumeBreakpoint}
            onInspectBreakpoint={handleInspectBreakpoint}
          />

          {/* Orchestration Loop Banner - shown when this runner is orchestrating */}
          <OrchestrationLoopBanner />

          {/* Cross-Run Patterns Alert */}
          <div className="px-4 pt-2">
            <TopPatternsWidget />
          </div>

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

          {/* Breakpoint Inspector Panel */}
          {showBreakpointInspector && (
            <BreakpointInspector
              snapshot={breakpointSnapshot}
              onClose={handleCloseBreakpointInspector}
            />
          )}

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

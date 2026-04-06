/**
 * RunRecapTab (Summary Page)
 *
 * Redesigned layout:
 *   1. Compact status strip (status + iterations + duration + sessions)
 *   2. Two-column grid: AI Summary (left ~60%) + Run Details sidebar (right ~40%)
 *   3. Failure section (if applicable, full width above grid)
 *   4. Tabbed content (Timeline, Verification, Knowledge, Context, Canvas, Errors)
 *
 * Collapses to single-column below ~900px.
 */

import { useState } from "react";
import { useUIElement, useUIComponent } from "ui-bridge";
import {
  Activity,
  AlertCircle,
  XCircle,
  LayoutDashboard,
  GitCommitHorizontal,
  BarChart3,
  DollarSign,
} from "lucide-react";
import { cn } from "../../lib/utils";
import { useRunSelectionOptional } from "../../contexts/RunSelectionContext";
import { useTaskRunRecap } from "../../hooks/useTaskRunRecap";
import { useReopenTaskRun } from "../../hooks/useAiData";
import { Tabs, TabsList, TabsTrigger, TabsContent } from "../ui/Tabs";
import { CompactStatusStrip } from "./CompactStatusStrip";
import { RunDetailsSidebar } from "./RunDetailsSidebar";
import { FailureSection } from "./FailureSection";
import { AISummarySection } from "./AISummarySection";
import { StagedTimeline } from "./StagedTimeline";
import { StepsTimeline } from "./StepsTimeline";
import { VerificationTab } from "./VerificationTab";
import { KnowledgeTab } from "./KnowledgeTab";
import { ContextTab } from "./ContextTab";
import { ErrorMonitorTab } from "../error-monitor/ErrorMonitorTab";
import { CanvasRecapTab } from "./CanvasRecapTab";
import { TaskRunLivePanel } from "../graphql/TaskRunLivePanel";
import { DurableExecutionTab } from "./DurableExecutionTab";
import { useErrorBadge } from "../../hooks/useErrorMonitor";
import { FeedbackScoresPanel } from "../feedback-scores/FeedbackScoresPanel";
import { RunCostBreakdown } from "./RunCostBreakdown";

// ============================================================================
// Main Component
// ============================================================================

interface RunRecapTabProps {
  onNavigateToAiOutput?: (phase: string, iteration?: number) => void;
}

export function RunRecapTab({ onNavigateToAiOutput }: RunRecapTabProps = {}) {
  const runSelection = useRunSelectionOptional();
  const selectedRun = runSelection?.selectedRun;
  const { data, isLoading, error, refetch } = useTaskRunRecap(runSelection?.selectedRunId);
  const [activeTab, setActiveTab] = useState("timeline");

  useUIComponent({ id: "run-summary-page", name: "Run Summary", description: "Task run recap with timeline, verification, knowledge, and context tabs" });
  const { ref: timelineRef } = useUIElement({ id: "run-tab-timeline", type: "button", label: "Timeline tab", actions: ["click"] });
  const { ref: verificationRef } = useUIElement({ id: "run-tab-verification", type: "button", label: "Verification tab", actions: ["click"] });
  const { ref: knowledgeRef } = useUIElement({ id: "run-tab-knowledge", type: "button", label: "Knowledge tab", actions: ["click"] });
  const { ref: contextRef } = useUIElement({ id: "run-tab-context", type: "button", label: "Context tab", actions: ["click"] });
  const { ref: canvasRef } = useUIElement({ id: "run-tab-canvas", type: "button", label: "Canvas tab", actions: ["click"] });
  const { ref: durableRef } = useUIElement({ id: "run-tab-durable", type: "button", label: "Diffs and Replay tab", actions: ["click"] });
  const { ref: feedbackRef } = useUIElement({ id: "run-tab-feedback", type: "button", label: "Feedback Scores tab", actions: ["click"] });
  const { ref: costsRef } = useUIElement({ id: "run-tab-llm-costs", type: "button", label: "LLM Costs tab", actions: ["click"] });
  const { ref: errorsRef } = useUIElement({ id: "run-tab-errors", type: "button", label: "Errors tab", actions: ["click"] });
  const [additionalSessions, setAdditionalSessions] = useState(3);
  const reopenMutation = useReopenTaskRun();
  const errorBadge = useErrorBadge(runSelection?.selectedRunId || undefined);

  const isFinished =
    selectedRun?.status === "complete" ||
    selectedRun?.status === "failed" ||
    selectedRun?.status === "stopped";

  const handleContinueRun = () => {
    if (!selectedRun) return;
    reopenMutation.mutate({
      taskId: selectedRun.id,
      additionalSessions,
    });
  };

  // Loading state
  if (isLoading) {
    return (
      <div className="h-full flex flex-col items-center justify-center text-muted-foreground p-8">
        <Activity className="w-12 h-12 mb-4 opacity-50 animate-pulse" />
        <p className="text-lg font-medium">Loading recap...</p>
      </div>
    );
  }

  // Error state
  if (error) {
    return (
      <div className="h-full flex flex-col items-center justify-center text-muted-foreground p-8">
        <XCircle className="w-12 h-12 mb-4 text-red-500 opacity-50" />
        <p className="text-lg font-medium">Failed to load recap</p>
        <p className="text-sm mt-2">{error instanceof Error ? error.message : "Unknown error"}</p>
      </div>
    );
  }

  // No data state
  if (!data) {
    return (
      <div className="h-full flex flex-col items-center justify-center text-muted-foreground p-8">
        <Activity className="w-12 h-12 mb-4 opacity-50" />
        <p className="text-lg font-medium">No Recap Data</p>
        <p className="text-sm mt-2 text-center max-w-md">
          Select a run to see its recap, or run a task first.
        </p>
      </div>
    );
  }

  const aiSummary = selectedRun?.summary || selectedRun?.ai_summary || data.summary || null;
  const goalAchieved = selectedRun?.goal_achieved ?? data.goal_achieved;
  const remainingWork = selectedRun?.remaining_work || null;
  const summaryGeneratedAt = selectedRun?.summary_generated_at || null;
  const taskRunId = selectedRun?.id || data.task_run_id;

  // Show live panel for running tasks — provides real-time events via GraphQL subscriptions
  const isLive = !isFinished && selectedRun?.status === "running";

  return (
    <div className="h-full overflow-y-auto p-4 space-y-4">
      {/* Live panel for active tasks — shows real-time progress, findings, output */}
      {isLive && taskRunId && (
        <TaskRunLivePanel taskRunId={taskRunId} compact className="border rounded-lg bg-card" />
      )}

      {/* 1. Compact Status Strip */}
      <CompactStatusStrip
        status={data.status}
        duration={data.duration_ms}
        loopResult={selectedRun?.loop_result}
        sessionsCount={selectedRun?.sessions_count}
        maxSessions={selectedRun?.max_sessions}
        outputLog={selectedRun?.output_log}
        autoContinue={selectedRun?.auto_continue}
      />

      {/* Failure Section - full width above the grid when present */}
      {data.failure_info && <FailureSection failure={data.failure_info} />}

      {/* 2. Two-column grid: AI Summary + Run Details sidebar */}
      <div className="grid grid-cols-1 lg:grid-cols-[1fr_280px] gap-4">
        {/* Left: AI Summary */}
        <AISummarySection
          aiSummary={aiSummary}
          goalAchieved={goalAchieved}
          remainingWork={remainingWork}
          summaryGeneratedAt={summaryGeneratedAt}
          taskRunId={taskRunId}
          status={data.status}
          onSummaryGenerated={() => refetch()}
        />

        {/* Right: Run Details sidebar */}
        {selectedRun && (
          <RunDetailsSidebar
            taskRunId={taskRunId}
            startTime={data.created_at}
            endTime={data.completed_at}
            isFinished={isFinished}
            goalAchieved={goalAchieved}
            onContinueRun={handleContinueRun}
            isContinuePending={reopenMutation.isPending}
            continueError={
              reopenMutation.isError
                ? reopenMutation.error?.message || "Failed to reopen run"
                : null
            }
            continueSuccess={reopenMutation.isSuccess}
            additionalSessions={additionalSessions}
            onAdditionalSessionsChange={setAdditionalSessions}
          />
        )}
      </div>

      {/* 3. Tabbed Content */}
      <Tabs value={activeTab} onValueChange={setActiveTab} className="w-full">
        <TabsList className="w-full justify-start flex-wrap gap-1">
          <TabsTrigger ref={timelineRef} value="timeline">Timeline</TabsTrigger>
          <TabsTrigger ref={verificationRef} value="verification">Verification</TabsTrigger>
          <TabsTrigger ref={knowledgeRef} value="knowledge">Knowledge</TabsTrigger>
          <TabsTrigger ref={contextRef} value="context">Context</TabsTrigger>
          <TabsTrigger ref={canvasRef} value="canvas" className="flex items-center gap-1.5">
            <LayoutDashboard className="w-3.5 h-3.5" />
            Canvas
          </TabsTrigger>
          <TabsTrigger ref={durableRef} value="durable" className="flex items-center gap-1.5">
            <GitCommitHorizontal className="w-3.5 h-3.5" />
            Diffs &amp; Replay
          </TabsTrigger>
          <TabsTrigger ref={feedbackRef} value="feedback" className="flex items-center gap-1.5">
            <BarChart3 className="w-3.5 h-3.5" />
            Feedback Scores
          </TabsTrigger>
          <TabsTrigger ref={costsRef} value="llm-costs" className="flex items-center gap-1.5">
            <DollarSign className="w-3.5 h-3.5" />
            LLM Costs
          </TabsTrigger>
          <TabsTrigger ref={errorsRef} value="errors" className="flex items-center gap-1.5">
            <AlertCircle className="w-3.5 h-3.5" />
            Errors
            {errorBadge.count > 0 && (
              <span
                className={cn(
                  "px-1.5 py-0.5 text-xs rounded-full",
                  errorBadge.highestSeverity === "critical" ||
                    errorBadge.highestSeverity === "error"
                    ? "bg-red-500/20 text-red-500"
                    : "bg-yellow-500/20 text-yellow-500",
                )}
              >
                {errorBadge.count}
              </span>
            )}
          </TabsTrigger>
        </TabsList>

        <TabsContent value="timeline" className="space-y-3">
          {data.stages && data.stages.length > 0 ? (
            <StagedTimeline stages={data.stages} onAiStepClick={onNavigateToAiOutput} />
          ) : (
            <StepsTimeline steps={data.steps} onAiStepClick={onNavigateToAiOutput} />
          )}
        </TabsContent>

        <TabsContent value="verification">
          <VerificationTab taskRunId={taskRunId} />
        </TabsContent>

        <TabsContent value="knowledge">
          <KnowledgeTab taskRunId={taskRunId} />
        </TabsContent>

        <TabsContent value="context">
          <ContextTab taskRunId={taskRunId} />
        </TabsContent>

        <TabsContent value="canvas">
          <CanvasRecapTab taskRunId={taskRunId} />
        </TabsContent>

        <TabsContent value="durable">
          <DurableExecutionTab taskRunId={taskRunId} />
        </TabsContent>

        <TabsContent value="feedback">
          <FeedbackScoresPanel runId={taskRunId} />
        </TabsContent>

        <TabsContent value="llm-costs">
          <RunCostBreakdown runId={taskRunId} />
        </TabsContent>

        <TabsContent value="errors" className="h-full">
          <ErrorMonitorTab
            taskRunId={runSelection?.selectedRunId || undefined}
            taskRunName={selectedRun?.task_name}
          />
        </TabsContent>
      </Tabs>
    </div>
  );
}


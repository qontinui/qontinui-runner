/**
 * RunRecapTab (renamed from Recap to Summary)
 *
 * Session Summary page - the primary view for understanding what happened during a run.
 * Combines the former Dashboard stats with comprehensive run analysis.
 *
 * Organized into tabs: Timeline, Verification, Knowledge, Tests, Context
 *
 * This is the main container component. Individual sections are extracted into separate files:
 * - StatusBanner: Overall status display
 * - FailureSection: Detailed failure information
 * - AISummarySection: AI-generated summary with goal achievement
 * - StagedTimeline: Steps grouped by workflow phases
 * - VerificationTab: Verification plan and results
 * - KnowledgeTab: Findings and knowledge entries
 * - TestsTab: Test results
 * - ContextTab: Variables and retry history
 */

import { useState } from "react";
import { Activity, XCircle, Users, FileText, Play, Loader2, Target } from "lucide-react";
import { useRunSelectionOptional } from "../../contexts/RunSelectionContext";
import { useTaskRunRecap } from "../../hooks/useTaskRunRecap";
import { useReopenTaskRun } from "../../hooks/useAiData";
import { Tabs, TabsList, TabsTrigger, TabsContent } from "../ui/Tabs";
import { StatusBanner } from "./StatusBanner";
import { FailureSection } from "./FailureSection";
import { AISummarySection } from "./AISummarySection";
import { StagedTimeline } from "./StagedTimeline";
import { StepsTimeline } from "./StepsTimeline";
import { VerificationTab } from "./VerificationTab";
import { KnowledgeTab } from "./KnowledgeTab";
import { AutomationTab } from "./AutomationTab";
import { TestsTab } from "./TestsTab";
import { ContextTab } from "./ContextTab";
import { getAccentColors, getStatusColors } from "@/design-system";

// ============================================================================
// Main Component
// ============================================================================

export function RunRecapTab() {
  const runSelection = useRunSelectionOptional();
  const selectedRun = runSelection?.selectedRun;
  const { data, isLoading, error, refetch } = useTaskRunRecap(runSelection?.selectedRunId);
  const [activeTab, setActiveTab] = useState("timeline");
  const [additionalSessions, setAdditionalSessions] = useState(3);
  const reopenMutation = useReopenTaskRun();

  // Check if run is finished (can be continued)
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

  // Get summary data from selectedRun (which has AI-generated summaries)
  // Fall back to data.summary which is extracted from output_log (## Summary section)
  const aiSummary = selectedRun?.summary || selectedRun?.ai_summary || data.summary || null;
  const goalAchieved = selectedRun?.goal_achieved ?? data.goal_achieved;
  const remainingWork = selectedRun?.remaining_work || null;
  const summaryGeneratedAt = selectedRun?.summary_generated_at || null;

  const taskRunId = selectedRun?.id || data.task_run_id;

  return (
    <div data-ui-id="recap-tab" className="h-full overflow-y-auto p-4 space-y-4">
      {/* AI Summary Section - Most prominent at the top */}
      <AISummarySection
        aiSummary={aiSummary}
        goalAchieved={goalAchieved}
        remainingWork={remainingWork}
        summaryGeneratedAt={summaryGeneratedAt}
        taskRunId={taskRunId}
        status={data.status}
        onSummaryGenerated={() => refetch()}
      />

      {/* Status Banner */}
      <StatusBanner
        status={data.status}
        goalAchieved={goalAchieved}
        duration={data.duration_ms}
        startTime={data.created_at}
        endTime={data.completed_at}
        verificationPassed={selectedRun?.verification_passed}
        loopResult={selectedRun?.loop_result}
      />

      {/* Session Stats Bar - Compact stats from former Dashboard */}
      {selectedRun && (
        <div className="flex items-center gap-4 p-3 rounded-lg border border-border bg-muted/30 text-sm">
          <div className="flex items-center gap-2 text-muted-foreground">
            <Users className="w-4 h-4" />
            <span>
              {selectedRun.sessions_count} session{selectedRun.sessions_count !== 1 ? "s" : ""}
              {selectedRun.max_sessions ? ` / ${selectedRun.max_sessions} max` : ""}
            </span>
          </div>
          <div className="w-px h-4 bg-border" />
          <div className="flex items-center gap-2 text-muted-foreground">
            <FileText className="w-4 h-4" />
            <span>
              {selectedRun.output_log
                ? `${Math.round(selectedRun.output_log.length / 1024)}KB output`
                : "No output"}
            </span>
          </div>
          {selectedRun.auto_continue && (
            <>
              <div className="w-px h-4 bg-border" />
              <span className="text-xs px-2 py-0.5 rounded bg-blue-500/10 text-blue-500">
                Auto-continue enabled
              </span>
            </>
          )}
        </div>
      )}

      {/* Failure Section - Prominent when failed */}
      {data.failure_info && <FailureSection failure={data.failure_info} />}

      {/* Continue Run Section - For finished runs */}
      {isFinished && selectedRun && (
        <div className="p-4 rounded-lg border border-border bg-card space-y-3">
          <div className="flex items-center gap-2">
            <Play className="w-4 h-4 text-muted-foreground" />
            <h3 className="text-sm font-medium">Continue This Run</h3>
            {selectedRun.goal_achieved === false && (
              <span
                className={`flex items-center gap-1.5 px-2 py-0.5 text-xs rounded ${getAccentColors("amber").bg} ${getAccentColors("amber").text}`}
              >
                <Target className="w-3 h-3" />
                Goal Not Achieved
              </span>
            )}
          </div>
          <p className="text-sm text-muted-foreground">
            Add more iterations to continue working toward the goal. New sessions will
            have access to all previous output and context.
          </p>
          <div className="flex items-center gap-3 flex-wrap">
            <div className="flex items-center gap-2">
              <label htmlFor="additionalSessions" className="text-sm text-muted-foreground">
                Additional sessions:
              </label>
              <input
                id="additionalSessions"
                type="number"
                min={1}
                max={20}
                value={additionalSessions}
                onChange={(e) =>
                  setAdditionalSessions(
                    Math.max(1, Math.min(20, parseInt(e.target.value) || 1)),
                  )
                }
                className="w-16 px-2 py-1 text-sm border border-border rounded bg-background text-foreground"
              />
            </div>
            <button
              onClick={handleContinueRun}
              disabled={reopenMutation.isPending}
              className="flex items-center gap-2 px-4 py-2 bg-primary text-primary-foreground rounded hover:bg-primary/90 disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
            >
              {reopenMutation.isPending ? (
                <>
                  <Loader2 className="w-4 h-4 animate-spin" />
                  <span>Reopening...</span>
                </>
              ) : (
                <>
                  <Play className="w-4 h-4" />
                  <span>Continue Run</span>
                </>
              )}
            </button>
          </div>
          {reopenMutation.isError && (
            <p className={`text-sm ${getStatusColors("error").text}`}>
              Error: {reopenMutation.error?.message || "Failed to reopen run"}
            </p>
          )}
          {reopenMutation.isSuccess && (
            <p className={`text-sm ${getStatusColors("success").text}`}>
              Run reopened successfully! It will continue automatically.
            </p>
          )}
        </div>
      )}

      {/* Tabbed Content */}
      <Tabs
        value={activeTab}
        onValueChange={setActiveTab}
        className="w-full"
      >
        <TabsList data-ui-id="recap-tabs" className="w-full justify-start flex-wrap gap-1">
          <TabsTrigger data-ui-id="recap-tab-timeline" value="timeline">
            Timeline
          </TabsTrigger>
          <TabsTrigger data-ui-id="recap-tab-verification" value="verification">
            Verification
          </TabsTrigger>
          <TabsTrigger data-ui-id="recap-tab-knowledge" value="knowledge">
            Knowledge
          </TabsTrigger>
          <TabsTrigger data-ui-id="recap-tab-automation" value="automation">
            Automation
          </TabsTrigger>
          <TabsTrigger data-ui-id="recap-tab-tests" value="tests">
            Tests
          </TabsTrigger>
          <TabsTrigger data-ui-id="recap-tab-context" value="context">
            Context
          </TabsTrigger>
        </TabsList>

        {/* Timeline Tab */}
        <TabsContent value="timeline" className="space-y-3">
          {data.stages && data.stages.length > 0 ? (
            <StagedTimeline stages={data.stages} />
          ) : (
            <StepsTimeline steps={data.steps} />
          )}
        </TabsContent>

        {/* Verification Tab */}
        <TabsContent value="verification">
          <VerificationTab taskRunId={taskRunId} />
        </TabsContent>

        {/* Knowledge Tab */}
        <TabsContent value="knowledge">
          <KnowledgeTab taskRunId={taskRunId} />
        </TabsContent>

        {/* Automation Tab */}
        <TabsContent value="automation">
          <AutomationTab taskRunId={taskRunId} stats={data.stats} />
        </TabsContent>

        {/* Tests Tab */}
        <TabsContent value="tests">
          <TestsTab taskRunId={taskRunId} loopResult={selectedRun?.loop_result} />
        </TabsContent>

        {/* Context Tab */}
        <TabsContent value="context">
          <ContextTab taskRunId={taskRunId} />
        </TabsContent>
      </Tabs>
    </div>
  );
}

export default RunRecapTab;

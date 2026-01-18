/**
 * RunRecapTab
 *
 * Session Recap page component that provides a quick overview of what happened during a run,
 * prominently shows the AI summary at the top, failure reasons, and displays all steps with brief summaries.
 */

import { useState } from "react";
import {
  CheckCircle2,
  XCircle,
  AlertCircle,
  Clock,
  Activity,
  ChevronDown,
  ChevronRight,
  Zap,
  Bot,
  TestTube,
  Play,
  Sparkles,
  Target,
  AlertTriangle,
  Loader2,
  FileText,
} from "lucide-react";
import { useRunSelectionOptional } from "../../contexts/RunSelectionContext";
import { useTaskRunRecap } from "../../hooks/useTaskRunRecap";
import type { RecapData, RecapStep, FailureInfo } from "../../types/recap";
import { getStatusColors, getAccentColors } from "@/design-system";
import { StagedTimeline } from "./StagedTimeline";
import { aiDataService } from "../../services";
import { useQueryClient } from "@tanstack/react-query";
import { aiDataKeys } from "../../hooks/useAiData";

/**
 * Format duration in milliseconds to a human-readable string.
 */
function formatDuration(ms: number | undefined): string {
  if (ms === undefined || ms === null) return "-";

  if (ms < 1000) {
    return `${ms}ms`;
  }

  const seconds = Math.floor(ms / 1000);
  if (seconds < 60) {
    return `${seconds}s`;
  }

  const minutes = Math.floor(seconds / 60);
  const remainingSeconds = seconds % 60;
  if (minutes < 60) {
    return remainingSeconds > 0 ? `${minutes}m ${remainingSeconds}s` : `${minutes}m`;
  }

  const hours = Math.floor(minutes / 60);
  const remainingMinutes = minutes % 60;
  return remainingMinutes > 0 ? `${hours}h ${remainingMinutes}m` : `${hours}h`;
}

/**
 * Get the icon for a step type.
 */
function getStepIcon(stepType: string) {
  switch (stepType) {
    case "workflow":
      return Play;
    case "action":
      return Zap;
    case "ai_session":
      return Bot;
    case "test":
      return TestTube;
    default:
      return Activity;
  }
}

/**
 * Get the status icon component.
 */
function getStatusIcon(status: string) {
  switch (status) {
    case "success":
    case "complete":
      return <CheckCircle2 className="w-4 h-4 text-green-500" />;
    case "failed":
      return <XCircle className="w-4 h-4 text-red-500" />;
    case "running":
      return <Activity className="w-4 h-4 text-blue-500 animate-pulse" />;
    case "skipped":
      return <AlertCircle className="w-4 h-4 text-yellow-500" />;
    default:
      return <Clock className="w-4 h-4 text-muted-foreground" />;
  }
}

// ============================================================================
// Sub-components
// ============================================================================

interface StatusBannerProps {
  status: string;
  goalAchieved?: boolean;
  duration?: number;
}

function StatusBanner({ status, goalAchieved, duration }: StatusBannerProps) {
  const isSuccess = status === "complete" || (status === "complete" && goalAchieved === true);
  const isFailed = status === "failed";
  const isRunning = status === "running";

  let bgColor = "bg-muted";
  let textColor = "text-muted-foreground";
  let icon = <Clock className="w-5 h-5" />;
  let label = "Unknown Status";

  if (isSuccess) {
    bgColor = "bg-green-500/10";
    textColor = "text-green-500";
    icon = <CheckCircle2 className="w-5 h-5" />;
    label = goalAchieved ? "Goal Achieved" : "Run Completed Successfully";
  } else if (isFailed) {
    bgColor = "bg-red-500/10";
    textColor = "text-red-500";
    icon = <XCircle className="w-5 h-5" />;
    label = "Run Failed";
  } else if (isRunning) {
    bgColor = "bg-blue-500/10";
    textColor = "text-blue-500";
    icon = <Activity className="w-5 h-5 animate-pulse" />;
    label = "Run In Progress";
  }

  return (
    <div className={`${bgColor} ${textColor} rounded-lg p-4 flex items-center justify-between`}>
      <div className="flex items-center gap-3">
        {icon}
        <span className="font-medium">{label}</span>
      </div>
      {duration !== undefined && (
        <div className="flex items-center gap-2 text-sm">
          <Clock className="w-4 h-4" />
          <span>Duration: {formatDuration(duration)}</span>
        </div>
      )}
    </div>
  );
}

interface FailureSectionProps {
  failure: FailureInfo;
}

function FailureSection({ failure }: FailureSectionProps) {
  return (
    <div className="bg-red-500/10 border border-red-500/30 rounded-lg p-4 space-y-3">
      <div className="flex items-start gap-3">
        <XCircle className="w-5 h-5 text-red-500 flex-shrink-0 mt-0.5" />
        <div className="flex-1 min-w-0">
          <h3 className="font-medium text-red-500">Run Failed</h3>
          <p className="text-sm text-red-400 mt-1">{failure.reason}</p>
        </div>
      </div>

      {failure.failed_step && (
        <div className="ml-8 text-sm">
          <span className="text-muted-foreground">Failed at: </span>
          <span className="text-red-400 font-medium">{failure.failed_step}</span>
        </div>
      )}

      {failure.error_type && (
        <div className="ml-8 text-sm">
          <span className="text-muted-foreground">Error type: </span>
          <span className="text-red-400">{failure.error_type}</span>
        </div>
      )}

      {failure.error_details && failure.error_details !== failure.reason && (
        <div className="ml-8 mt-2">
          <details className="text-sm">
            <summary className="cursor-pointer text-muted-foreground hover:text-foreground">
              Show error details
            </summary>
            <pre className="mt-2 p-3 bg-background/50 rounded text-xs overflow-x-auto whitespace-pre-wrap text-red-300">
              {failure.error_details}
            </pre>
          </details>
        </div>
      )}
    </div>
  );
}

interface SummaryCardProps {
  summary: string;
}

function SummaryCard({ summary }: SummaryCardProps) {
  return (
    <div className="card p-4">
      <h3 className="text-sm font-medium text-muted-foreground mb-2">Summary</h3>
      <p className="text-sm">{summary}</p>
    </div>
  );
}

interface AISummarySectionProps {
  aiSummary?: string | null;
  goalAchieved?: boolean | null;
  remainingWork?: string | null;
  summaryGeneratedAt?: string | null;
  taskRunId?: string;
  status?: string;
  onSummaryGenerated?: () => void;
}

function AISummarySection({
  aiSummary,
  goalAchieved,
  remainingWork,
  summaryGeneratedAt,
  taskRunId,
  status,
  onSummaryGenerated,
}: AISummarySectionProps) {
  const [isGenerating, setIsGenerating] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const queryClient = useQueryClient();

  const handleGenerateSummary = async () => {
    if (!taskRunId) return;

    setIsGenerating(true);
    setError(null);

    try {
      const result = await aiDataService.generateSummary(taskRunId);
      if (result.success) {
        queryClient.invalidateQueries({ queryKey: aiDataKeys.taskRun(taskRunId) });
        queryClient.invalidateQueries({ queryKey: aiDataKeys.taskRuns() });
        onSummaryGenerated?.();
      } else {
        setError(result.error || "Failed to generate summary");
      }
    } catch (err) {
      setError(err instanceof Error ? err.message : "Failed to generate summary");
    } finally {
      setIsGenerating(false);
    }
  };

  // If we have a summary, display it prominently
  if (aiSummary) {
    return (
      <div className="rounded-xl border-2 border-primary/30 bg-gradient-to-br from-primary/5 to-primary/10 p-5 space-y-4">
        {/* Header */}
        <div className="flex items-center justify-between">
          <div className="flex items-center gap-3">
            <div className="p-2 bg-primary/20 rounded-lg">
              <Sparkles className="w-5 h-5 text-primary" />
            </div>
            <div>
              <h2 className="font-semibold text-lg text-foreground">AI Summary</h2>
              {summaryGeneratedAt && (
                <p className="text-xs text-muted-foreground">
                  Generated {new Date(summaryGeneratedAt).toLocaleString()}
                </p>
              )}
            </div>
          </div>
          {/* Goal Achievement Badge */}
          {goalAchieved !== undefined && goalAchieved !== null && (
            <span
              className={`flex items-center gap-2 px-3 py-1.5 text-sm font-medium rounded-full ${
                goalAchieved
                  ? `${getStatusColors("success").bg} ${getStatusColors("success").text}`
                  : `${getAccentColors("amber").bg} ${getAccentColors("amber").text}`
              }`}
            >
              <Target className="w-4 h-4" />
              {goalAchieved ? "Goal Achieved" : "Goal Not Achieved"}
            </span>
          )}
        </div>

        {/* Summary Text */}
        <p className="text-foreground/90 leading-relaxed whitespace-pre-wrap">{aiSummary}</p>

        {/* Remaining Work (if goal not achieved) */}
        {remainingWork && !goalAchieved && (
          <div
            className={`p-4 rounded-lg ${getAccentColors("amber").bg} border ${getAccentColors("amber").border}`}
          >
            <div className="flex items-center gap-2 mb-2">
              <AlertTriangle className={`w-4 h-4 ${getAccentColors("amber").text}`} />
              <span className={`font-medium ${getAccentColors("amber").text}`}>Remaining Work</span>
            </div>
            <p className="text-amber-300/90 whitespace-pre-wrap">{remainingWork}</p>
          </div>
        )}
      </div>
    );
  }

  // No summary yet - show placeholder or loading state
  const isComplete = status === "complete";
  const completedRecently =
    isComplete && status === "complete"; // Can enhance with timestamp check

  if (isGenerating) {
    return (
      <div className="rounded-xl border border-border bg-muted/20 p-5">
        <div className="flex items-center gap-3 text-muted-foreground">
          <Loader2 className="w-5 h-5 animate-spin" />
          <span>Generating AI summary...</span>
        </div>
      </div>
    );
  }

  if (isComplete) {
    return (
      <div className="rounded-xl border border-border bg-muted/20 p-5">
        <div className="flex items-center justify-between">
          <div className="flex items-center gap-3 text-muted-foreground">
            <FileText className="w-5 h-5 opacity-50" />
            <span>No AI summary available for this run.</span>
          </div>
          <button
            onClick={handleGenerateSummary}
            className="flex items-center gap-2 px-4 py-2 text-sm bg-primary/10 hover:bg-primary/20 text-primary rounded-lg transition-colors"
          >
            <Sparkles className="w-4 h-4" />
            Generate Summary
          </button>
        </div>
        {error && <p className="mt-3 text-sm text-red-400">Error: {error}</p>}
      </div>
    );
  }

  // Run is still in progress or not complete
  return null;
}

interface StepItemProps {
  step: RecapStep;
  depth?: number;
}

function StepItem({ step, depth = 0 }: StepItemProps) {
  const [expanded, setExpanded] = useState(step.status === "failed");
  const hasChildren = step.children && step.children.length > 0;
  const Icon = getStepIcon(step.step_type);

  return (
    <div className={depth > 0 ? "ml-6 border-l border-border pl-4" : ""}>
      <button
        onClick={() => hasChildren && setExpanded(!expanded)}
        className={`
          w-full flex items-center gap-3 p-3 rounded-lg text-left transition-colors
          ${hasChildren ? "hover:bg-muted/50 cursor-pointer" : "cursor-default"}
          ${step.status === "failed" ? "bg-red-500/5" : ""}
        `}
        disabled={!hasChildren}
      >
        {/* Expand/collapse indicator */}
        <div className="w-4 h-4 flex-shrink-0">
          {hasChildren ? (
            expanded ? (
              <ChevronDown className="w-4 h-4 text-muted-foreground" />
            ) : (
              <ChevronRight className="w-4 h-4 text-muted-foreground" />
            )
          ) : null}
        </div>

        {/* Status icon */}
        {getStatusIcon(step.status)}

        {/* Step type icon */}
        <div className="p-1.5 rounded bg-muted/50">
          <Icon className="w-3 h-3 text-muted-foreground" />
        </div>

        {/* Content */}
        <div className="flex-1 min-w-0">
          <div className="flex items-center gap-2">
            <span className="font-medium truncate">{step.name}</span>
            {step.duration_ms !== undefined && (
              <span className="text-xs text-muted-foreground">
                ({formatDuration(step.duration_ms)})
              </span>
            )}
          </div>
          {step.summary && (
            <p className="text-xs text-muted-foreground truncate mt-0.5">{step.summary}</p>
          )}
          {step.error && <p className="text-xs text-red-400 truncate mt-0.5">{step.error}</p>}
        </div>
      </button>

      {/* Children */}
      {hasChildren && expanded && (
        <div className="mt-1 space-y-1">
          {step.children.map((child, index) => (
            <StepItem key={`${child.name}-${index}`} step={child} depth={depth + 1} />
          ))}
        </div>
      )}
    </div>
  );
}

interface StepsTimelineProps {
  steps: RecapStep[];
}

function StepsTimeline({ steps }: StepsTimelineProps) {
  if (steps.length === 0) {
    return (
      <div className="card p-6 text-center text-muted-foreground">
        <Activity className="w-8 h-8 mx-auto mb-2 opacity-50" />
        <p>No steps recorded</p>
      </div>
    );
  }

  return (
    <div className="card">
      <div className="px-4 py-3 border-b border-border">
        <h3 className="text-sm font-medium">Steps ({steps.length})</h3>
      </div>
      <div className="p-2 space-y-1 max-h-[400px] overflow-y-auto">
        {steps.map((step, index) => (
          <StepItem key={`${step.name}-${index}`} step={step} />
        ))}
      </div>
    </div>
  );
}

// ============================================================================
// Main Component
// ============================================================================

export function RunRecapTab() {
  const runSelection = useRunSelectionOptional();
  const selectedRun = runSelection?.selectedRun;
  const { data, isLoading, error, refetch } = useTaskRunRecap(runSelection?.selectedRunId);

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

  // Get summary data from selectedRun (which has more complete data)
  // Fall back to data.summary for backward compatibility
  const aiSummary = selectedRun?.summary || selectedRun?.ai_summary || null;
  const goalAchieved = selectedRun?.goal_achieved ?? data.goal_achieved;
  const remainingWork = selectedRun?.remaining_work || null;
  const summaryGeneratedAt = selectedRun?.summary_generated_at || null;

  return (
    <div className="h-full overflow-y-auto p-4 space-y-4">
      {/* AI Summary Section - Most prominent at the top */}
      <AISummarySection
        aiSummary={aiSummary}
        goalAchieved={goalAchieved}
        remainingWork={remainingWork}
        summaryGeneratedAt={summaryGeneratedAt}
        taskRunId={selectedRun?.id}
        status={data.status}
        onSummaryGenerated={() => refetch()}
      />

      {/* Status Banner */}
      <StatusBanner
        status={data.status}
        goalAchieved={goalAchieved}
        duration={data.duration_ms}
      />

      {/* Failure Section - Prominent when failed */}
      {data.failure_info && <FailureSection failure={data.failure_info} />}

      {/* Steps/Stages Timeline */}
      {data.stages && data.stages.length > 0 ? (
        <StagedTimeline stages={data.stages} />
      ) : (
        <StepsTimeline steps={data.steps} />
      )}
    </div>
  );
}

export default RunRecapTab;

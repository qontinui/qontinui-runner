/**
 * RunDashboard Component
 *
 * Central dashboard for viewing and selecting task runs.
 * Shows run selector, status overview, and quick links to detail pages.
 */

import { useState } from "react";
import {
  CheckCircle,
  XCircle,
  Loader2,
  Clock,
  Zap,
  Image,
  ClipboardList,
  FileText,
  FileSearch,
  Bot,
  BarChart3,
  ArrowRight,
  Activity,
  StopCircle,
  Users,
  Play,
  Target,
  TestTube,
} from "lucide-react";
import { useRunSelection } from "../../contexts/RunSelectionContext";
import { RunSelector } from "../run-selection/RunSelector";
import { useReopenTaskRun } from "../../hooks/useAiData";
import type { TaskRunStatus } from "../../types/aiData";

interface RunDashboardProps {
  /** Callback to navigate to a specific tab */
  onNavigate?: (tabId: string) => void;
}

// Status display configuration for TaskRun statuses
const statusConfig: Record<
  TaskRunStatus,
  { icon: typeof CheckCircle; color: string; bgColor: string; label: string }
> = {
  running: {
    icon: Loader2,
    color: "text-blue-400",
    bgColor: "bg-blue-500/10",
    label: "In Progress",
  },
  complete: {
    icon: CheckCircle,
    color: "text-green-400",
    bgColor: "bg-green-500/10",
    label: "Complete",
  },
  failed: {
    icon: XCircle,
    color: "text-red-400",
    bgColor: "bg-red-500/10",
    label: "Failed",
  },
  stopped: {
    icon: StopCircle,
    color: "text-slate-400",
    bgColor: "bg-slate-500/10",
    label: "Stopped",
  },
};

/**
 * Calculate duration from created_at and completed_at timestamps
 */
function calculateDuration(createdAt: string, completedAt?: string): number | undefined {
  if (!completedAt) return undefined;
  const start = new Date(createdAt).getTime();
  const end = new Date(completedAt).getTime();
  return end - start;
}

/**
 * Format duration from milliseconds to human-readable string
 */
function formatDuration(ms: number | undefined): string {
  if (!ms) return "-";
  const seconds = Math.floor(ms / 1000);
  const minutes = Math.floor(seconds / 60);
  const hours = Math.floor(minutes / 60);

  if (hours > 0) {
    return `${hours}h ${minutes % 60}m ${seconds % 60}s`;
  } else if (minutes > 0) {
    return `${minutes}m ${seconds % 60}s`;
  } else {
    return `${seconds}s`;
  }
}

/**
 * Stat Card Component
 */
function StatCard({
  icon: Icon,
  label,
  value,
  subValue,
  color = "text-foreground",
  bgColor = "bg-muted/30",
}: {
  icon: typeof Activity;
  label: string;
  value: string | number;
  subValue?: string;
  color?: string;
  bgColor?: string;
}) {
  return (
    <div className={`p-4 rounded-lg border border-border ${bgColor}`}>
      <div className="flex items-center gap-2 text-muted-foreground mb-2">
        <Icon className="w-4 h-4" />
        <span className="text-xs font-medium uppercase">{label}</span>
      </div>
      <div className={`text-2xl font-bold ${color}`}>{value}</div>
      {subValue && <div className="text-xs text-muted-foreground mt-1">{subValue}</div>}
    </div>
  );
}

/**
 * Quick Link Card Component
 */
function QuickLinkCard({
  icon: Icon,
  title,
  description,
  onClick,
}: {
  icon: typeof Activity;
  title: string;
  description: string;
  onClick: () => void;
}) {
  return (
    <button
      onClick={onClick}
      className="flex items-start gap-3 p-4 rounded-lg border border-border bg-card hover:bg-muted/30 transition-colors text-left group"
    >
      <div className="p-2 rounded bg-muted">
        <Icon className="w-4 h-4 text-muted-foreground" />
      </div>
      <div className="flex-1 min-w-0">
        <div className="font-medium text-foreground group-hover:text-primary transition-colors">
          {title}
        </div>
        <div className="text-sm text-muted-foreground truncate">{description}</div>
      </div>
      <ArrowRight className="w-4 h-4 text-muted-foreground group-hover:text-primary transition-colors mt-2" />
    </button>
  );
}

export function RunDashboard({ onNavigate }: RunDashboardProps) {
  const { selectedRun, isLoadingRuns, recentRuns } = useRunSelection();
  const [additionalSessions, setAdditionalSessions] = useState(3);
  const reopenMutation = useReopenTaskRun();

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
  if (isLoadingRuns && recentRuns.length === 0) {
    return (
      <div className="h-full flex flex-col items-center justify-center text-muted-foreground p-8">
        <Loader2 className="w-8 h-8 animate-spin mb-4" />
        <p className="text-sm">Loading runs...</p>
      </div>
    );
  }

  // No runs yet
  if (recentRuns.length === 0) {
    return (
      <div className="h-full flex flex-col items-center justify-center text-muted-foreground p-8">
        <Clock className="w-12 h-12 mb-4 opacity-50" />
        <p className="text-lg font-medium">No Task Runs Yet</p>
        <p className="text-sm mt-2 text-center max-w-md">
          Start a task to see run data here. Tasks can be started from the Active page.
        </p>
      </div>
    );
  }

  const statusInfo = selectedRun ? statusConfig[selectedRun.status] : null;
  const StatusIcon = statusInfo?.icon || Activity;
  const duration = selectedRun
    ? calculateDuration(selectedRun.created_at, selectedRun.completed_at)
    : undefined;

  return (
    <div className="h-full overflow-y-auto p-6 space-y-6">
      {/* Header with Run Selector */}
      <div className="space-y-4">
        <div>
          <h1 className="text-xl font-semibold text-foreground">Run Dashboard</h1>
          <p className="text-sm text-muted-foreground mt-1">
            Select a task run to view detailed information across all pages
          </p>
        </div>

        <div className="max-w-md">
          <RunSelector />
        </div>
      </div>

      {/* Selected Run Overview */}
      {selectedRun ? (
        <>
          {/* Status Cards */}
          <div className="grid grid-cols-2 md:grid-cols-4 gap-4">
            {/* Status */}
            <StatCard
              icon={StatusIcon}
              label="Status"
              value={statusInfo?.label || "Unknown"}
              subValue={selectedRun.task_name}
              color={statusInfo?.color}
              bgColor={statusInfo?.bgColor}
            />

            {/* Duration */}
            <StatCard
              icon={Clock}
              label="Duration"
              value={formatDuration(duration)}
              subValue={new Date(selectedRun.created_at).toLocaleString()}
            />

            {/* Sessions */}
            <StatCard
              icon={Users}
              label="Sessions"
              value={selectedRun.sessions_count}
              subValue={
                selectedRun.max_sessions
                  ? `Max: ${selectedRun.max_sessions}`
                  : selectedRun.auto_continue
                    ? "Auto-continue enabled"
                    : undefined
              }
            />

            {/* Output Length */}
            <StatCard
              icon={FileText}
              label="Output"
              value={
                selectedRun.output_log
                  ? `${Math.round(selectedRun.output_log.length / 1024)}KB`
                  : "0KB"
              }
              subValue={
                selectedRun.output_log
                  ? `${selectedRun.output_log.split("\n").length} lines`
                  : undefined
              }
            />
          </div>

          {/* Error Message (if failed) */}
          {selectedRun.status === "failed" && selectedRun.error_message && (
            <div className="p-4 rounded-lg border border-red-500/30 bg-red-500/10">
              <div className="flex items-center gap-2 text-red-400 mb-2">
                <XCircle className="w-4 h-4" />
                <span className="font-medium">Error</span>
              </div>
              <p className="text-sm text-red-300">{selectedRun.error_message}</p>
            </div>
          )}

          {/* Prompt Preview */}
          {selectedRun.prompt && (
            <div className="space-y-2">
              <h2 className="text-sm font-medium text-muted-foreground uppercase tracking-wider">
                Task Prompt
              </h2>
              <div className="p-4 rounded-lg border border-border bg-muted/30">
                <p className="text-sm text-foreground whitespace-pre-wrap line-clamp-4">
                  {selectedRun.prompt}
                </p>
              </div>
            </div>
          )}

          {/* Quick Links to Detail Pages */}
          <div className="space-y-3">
            <h2 className="text-sm font-medium text-muted-foreground uppercase tracking-wider">
              View Details
            </h2>
            <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 gap-3">
              <QuickLinkCard
                icon={Zap}
                title="Actions"
                description="Action execution logs"
                onClick={() => onNavigate?.("run-actions")}
              />
              <QuickLinkCard
                icon={Image}
                title="Image Recognition"
                description="Pattern matching results"
                onClick={() => onNavigate?.("run-image")}
              />
              <QuickLinkCard
                icon={ClipboardList}
                title="Summary"
                description="AI output summary and conclusions"
                onClick={() => onNavigate?.("run-summary")}
              />
              <QuickLinkCard
                icon={FileText}
                title="Findings"
                description="All findings and issues detected"
                onClick={() => onNavigate?.("run-findings")}
              />
              <QuickLinkCard
                icon={FileSearch}
                title="Verification"
                description="Verification history"
                onClick={() => onNavigate?.("run-verification")}
              />
              <QuickLinkCard
                icon={TestTube}
                title="Test Results"
                description="Verification test execution"
                onClick={() => onNavigate?.("run-tests")}
              />
              <QuickLinkCard
                icon={Bot}
                title="AI Output"
                description="Full AI conversation"
                onClick={() => onNavigate?.("run-ai-output")}
              />
              <QuickLinkCard
                icon={BarChart3}
                title="Statistics"
                description="Performance statistics"
                onClick={() => onNavigate?.("run-statistics")}
              />
            </div>
          </div>

          {/* Continue Run Section - shown for finished runs */}
          {isFinished && (
            <div className="space-y-3 pt-4 border-t border-border">
              <div className="flex items-center gap-2">
                <Play className="w-4 h-4 text-muted-foreground" />
                <h2 className="text-sm font-medium text-muted-foreground uppercase tracking-wider">
                  Continue This Run
                </h2>
              </div>
              <div className="p-4 rounded-lg border border-border bg-card">
                <div className="flex items-center gap-2 mb-2">
                  {selectedRun?.goal_achieved === false && (
                    <span className="flex items-center gap-1.5 px-2 py-1 text-xs rounded bg-amber-500/10 text-amber-400">
                      <Target className="w-3 h-3" />
                      Goal Not Achieved
                    </span>
                  )}
                </div>
                <p className="text-sm text-muted-foreground mb-4">
                  Add more iterations to continue working toward the goal. The new sessions will
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
                  <p className="mt-2 text-sm text-red-400">
                    Error: {reopenMutation.error?.message || "Failed to reopen run"}
                  </p>
                )}
                {reopenMutation.isSuccess && (
                  <p className="mt-2 text-sm text-green-400">
                    Run reopened successfully! It will continue automatically.
                  </p>
                )}
              </div>
            </div>
          )}
        </>
      ) : (
        <div className="flex flex-col items-center justify-center py-12 text-muted-foreground">
          <Activity className="w-8 h-8 mb-3 opacity-50" />
          <p className="text-sm">Select a run from the dropdown above to view details</p>
        </div>
      )}
    </div>
  );
}

export default RunDashboard;

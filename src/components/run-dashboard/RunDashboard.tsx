/**
 * RunDashboard Component
 *
 * Central dashboard for viewing and selecting runs.
 * Shows run selector, status overview, and quick links to detail pages.
 */

import {
  CheckCircle,
  XCircle,
  Timer,
  AlertCircle,
  Loader2,
  Clock,
  Zap,
  Image,
  ClipboardList,
  FileText,
  AlertTriangle,
  FileSearch,
  Bot,
  BarChart3,
  ArrowRight,
  Activity,
} from "lucide-react";
import { useRunSelection } from "../../contexts/RunSelectionContext";
import { RunSelector } from "../run-selection/RunSelector";
import type { RunStatus } from "../../types/statistics";

interface RunDashboardProps {
  /** Callback to navigate to a specific tab */
  onNavigate?: (tabId: string) => void;
}

// Status display configuration
const statusConfig: Record<
  RunStatus,
  { icon: typeof CheckCircle; color: string; bgColor: string; label: string }
> = {
  running: {
    icon: Loader2,
    color: "text-blue-400",
    bgColor: "bg-blue-500/10",
    label: "In Progress",
  },
  completed: {
    icon: CheckCircle,
    color: "text-green-400",
    bgColor: "bg-green-500/10",
    label: "Completed",
  },
  failed: {
    icon: XCircle,
    color: "text-red-400",
    bgColor: "bg-red-500/10",
    label: "Failed",
  },
  timeout: {
    icon: Timer,
    color: "text-orange-400",
    bgColor: "bg-orange-500/10",
    label: "Timeout",
  },
  cancelled: {
    icon: AlertCircle,
    color: "text-slate-400",
    bgColor: "bg-slate-500/10",
    label: "Cancelled",
  },
};

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
  const { selectedRun, configId, isLoadingRuns, recentRuns } = useRunSelection();

  // No config loaded
  if (!configId) {
    return (
      <div className="h-full flex flex-col items-center justify-center text-muted-foreground p-8">
        <Activity className="w-12 h-12 mb-4 opacity-50" />
        <p className="text-lg font-medium">No Configuration Loaded</p>
        <p className="text-sm mt-2 text-center max-w-md">
          Load a workflow configuration to view run history and statistics.
        </p>
      </div>
    );
  }

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
        <p className="text-lg font-medium">No Runs Yet</p>
        <p className="text-sm mt-2 text-center max-w-md">
          Execute a workflow to see run data here. You can start a workflow from the Execute page.
        </p>
      </div>
    );
  }

  const statusInfo = selectedRun ? statusConfig[selectedRun.status] : null;
  const StatusIcon = statusInfo?.icon || Activity;

  return (
    <div className="h-full overflow-y-auto p-6 space-y-6">
      {/* Header with Run Selector */}
      <div className="space-y-4">
        <div>
          <h1 className="text-xl font-semibold text-foreground">Run Dashboard</h1>
          <p className="text-sm text-muted-foreground mt-1">
            Select a run to view detailed information across all pages
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
              subValue={selectedRun.workflow_name}
              color={statusInfo?.color}
              bgColor={statusInfo?.bgColor}
            />

            {/* Duration */}
            <StatCard
              icon={Clock}
              label="Duration"
              value={formatDuration(selectedRun.duration_ms)}
              subValue={
                selectedRun.started_at
                  ? new Date(selectedRun.started_at).toLocaleString()
                  : undefined
              }
            />

            {/* Actions */}
            <StatCard
              icon={Zap}
              label="Actions"
              value={selectedRun.actions_summary?.total ?? 0}
              subValue={
                selectedRun.actions_summary
                  ? `${selectedRun.actions_summary.success} succeeded, ${selectedRun.actions_summary.failed} failed`
                  : undefined
              }
              color={selectedRun.actions_summary?.failed ? "text-orange-400" : "text-foreground"}
            />

            {/* States Visited */}
            <StatCard
              icon={Activity}
              label="States Visited"
              value={selectedRun.states_visited?.length ?? 0}
              subValue={
                selectedRun.transitions_executed
                  ? `${selectedRun.transitions_executed.length} transitions`
                  : undefined
              }
            />
          </div>

          {/* Error Message (if failed) */}
          {selectedRun.status === "failed" && selectedRun.error_message && (
            <div className="p-4 rounded-lg border border-red-500/30 bg-red-500/10">
              <div className="flex items-center gap-2 text-red-400 mb-2">
                <XCircle className="w-4 h-4" />
                <span className="font-medium">{selectedRun.error_type || "Error"}</span>
              </div>
              <p className="text-sm text-red-300">{selectedRun.error_message}</p>
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
                description={`${selectedRun.actions_summary?.total ?? 0} actions executed`}
                onClick={() => onNavigate?.("run-actions")}
              />
              <QuickLinkCard
                icon={Image}
                title="Image Recognition"
                description={`${selectedRun.template_matches?.length ?? 0} template matches`}
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
                description="Code findings and issues detected"
                onClick={() => onNavigate?.("run-findings")}
              />
              <QuickLinkCard
                icon={AlertTriangle}
                title="Issues"
                description={`${selectedRun.anomalies?.length ?? 0} anomalies detected`}
                onClick={() => onNavigate?.("run-issues")}
              />
              <QuickLinkCard
                icon={FileSearch}
                title="Verification"
                description="Verification history"
                onClick={() => onNavigate?.("run-verification")}
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
                description="Config-level performance stats"
                onClick={() => onNavigate?.("run-statistics")}
              />
            </div>
          </div>

          {/* Anomalies Preview (if any) */}
          {selectedRun.anomalies && selectedRun.anomalies.length > 0 && (
            <div className="space-y-3">
              <h2 className="text-sm font-medium text-muted-foreground uppercase tracking-wider">
                Anomalies Detected ({selectedRun.anomalies.length})
              </h2>
              <div className="space-y-2">
                {selectedRun.anomalies.slice(0, 3).map((anomaly, index) => (
                  <div
                    key={index}
                    className={`p-3 rounded-lg border ${
                      anomaly.severity === "high"
                        ? "border-red-500/30 bg-red-500/5"
                        : anomaly.severity === "medium"
                          ? "border-orange-500/30 bg-orange-500/5"
                          : "border-yellow-500/30 bg-yellow-500/5"
                    }`}
                  >
                    <div className="flex items-center gap-2 mb-1">
                      <span
                        className={`text-xs font-medium px-1.5 py-0.5 rounded ${
                          anomaly.severity === "high"
                            ? "bg-red-500/20 text-red-400"
                            : anomaly.severity === "medium"
                              ? "bg-orange-500/20 text-orange-400"
                              : "bg-yellow-500/20 text-yellow-400"
                        }`}
                      >
                        {anomaly.severity.toUpperCase()}
                      </span>
                      <span className="text-sm font-medium text-foreground">
                        {anomaly.anomaly_type.replace(/_/g, " ")}
                      </span>
                    </div>
                    <p className="text-sm text-muted-foreground">{anomaly.description}</p>
                  </div>
                ))}
                {selectedRun.anomalies.length > 3 && (
                  <button
                    onClick={() => onNavigate?.("run-issues")}
                    className="text-sm text-primary hover:underline"
                  >
                    View all {selectedRun.anomalies.length} anomalies →
                  </button>
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

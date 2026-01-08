/**
 * ExecutionSummaryTab.tsx
 *
 * Shows a comprehensive summary of AI execution runs.
 * - AI-generated paragraph summary and goal achievement status at the top
 * - Code-related findings (bugs, security, performance) below
 *
 * Uses RunSelectionContext when available to filter to the selected run.
 */

import { useState, useMemo, useEffect } from "react";
import {
  FileText,
  Clock,
  CheckCircle,
  AlertCircle,
  Bug,
  Shield,
  Zap,
  BookOpen,
  Lightbulb,
  ChevronDown,
  ChevronRight,
  ExternalLink,
  Loader2,
  XCircle,
  StopCircle,
  Activity,
  ClipboardList,
  Users,
  Target,
  AlertTriangle,
  Sparkles,
} from "lucide-react";
import type { Finding, FindingSeverity } from "../../types/findings";
import { findingsTracker, getVisibleCategories } from "../../services";
import { useRunSelectionOptional } from "../../contexts/RunSelectionContext";
import type { TaskRunStatus } from "../../types/aiData";

// Categories to include in the summary (code-focused)
const CODE_FOCUSED_CATEGORIES = [
  "code_bug",
  "security",
  "performance",
  "enhancement",
  "documentation",
  "already_fixed",
  "warning",
];

const severityColors: Record<FindingSeverity, string> = {
  critical: "text-red-500 bg-red-500/10",
  high: "text-orange-500 bg-orange-500/10",
  medium: "text-amber-500 bg-amber-500/10",
  low: "text-blue-500 bg-blue-500/10",
  info: "text-slate-400 bg-slate-400/10",
};

const categoryIcons: Record<string, React.ComponentType<{ className?: string }>> = {
  code_bug: Bug,
  security: Shield,
  performance: Zap,
  enhancement: Lightbulb,
  documentation: BookOpen,
  already_fixed: CheckCircle,
  warning: AlertCircle,
};

const statusIcons: Record<TaskRunStatus, React.ComponentType<{ className?: string }>> = {
  complete: CheckCircle,
  failed: XCircle,
  stopped: StopCircle,
  running: Loader2,
};

const statusColors: Record<TaskRunStatus, string> = {
  complete: "text-green-400 bg-green-500/10",
  failed: "text-red-400 bg-red-500/10",
  stopped: "text-slate-400 bg-slate-500/10",
  running: "text-blue-400 bg-blue-500/10",
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

export function ExecutionSummaryTab() {
  const [findings, setFindings] = useState<Finding[]>([]);
  const [expandedCategories, setExpandedCategories] = useState<Set<string>>(
    new Set(["code_bug", "security"]),
  );

  // Use RunSelectionContext if available
  const runSelection = useRunSelectionOptional();
  const selectedRun = runSelection?.selectedRun;

  // Load findings and subscribe to updates
  useEffect(() => {
    findingsTracker.loadFromDisk().then(() => {
      setFindings(findingsTracker.getAllFindings());
    });

    const unsubscribe = findingsTracker.subscribe((event) => {
      if (
        event.type === "finding_detected" ||
        event.type === "finding_updated" ||
        event.type === "finding_resolved" ||
        event.type === "finding_removed" ||
        event.type === "report_created" ||
        event.type === "report_updated" ||
        event.type === "findings_cleared"
      ) {
        setFindings(findingsTracker.getAllFindings());
      }
    });

    return unsubscribe;
  }, []);

  // Filter findings to code-focused categories
  const filterCodeFocusedFindings = (allFindings: Finding[]) => {
    return allFindings.filter((f) => CODE_FOCUSED_CATEGORIES.includes(f.categoryId));
  };

  // Get the findings to display
  const displayFindings = useMemo(() => {
    return filterCodeFocusedFindings(findings);
  }, [findings]);

  // Group findings by category
  const findingsByCategory = useMemo(() => {
    const grouped = new Map<string, Finding[]>();
    for (const finding of displayFindings) {
      const existing = grouped.get(finding.categoryId) || [];
      grouped.set(finding.categoryId, [...existing, finding]);
    }
    return grouped;
  }, [displayFindings]);

  // Calculate summary statistics
  const summary = useMemo(() => {
    const total = displayFindings.length;
    const resolved = displayFindings.filter((f) => f.status === "resolved").length;
    const pending = displayFindings.filter(
      (f) => f.status === "detected" || f.status === "in_progress",
    ).length;
    const critical = displayFindings.filter((f) => f.severity === "critical").length;
    const high = displayFindings.filter((f) => f.severity === "high").length;

    return { total, resolved, pending, critical, high };
  }, [displayFindings]);

  // Get visible categories with findings
  const categoriesWithFindings = useMemo(() => {
    const categories = getVisibleCategories();
    return categories.filter(
      (cat) =>
        CODE_FOCUSED_CATEGORIES.includes(cat.id) &&
        findingsByCategory.has(cat.id) &&
        (findingsByCategory.get(cat.id)?.length ?? 0) > 0,
    );
  }, [findingsByCategory]);

  const toggleCategory = (categoryId: string) => {
    setExpandedCategories((prev) => {
      const next = new Set(prev);
      if (next.has(categoryId)) {
        next.delete(categoryId);
      } else {
        next.add(categoryId);
      }
      return next;
    });
  };

  const formatDuration = (ms: number | undefined) => {
    if (!ms) return "-";
    const seconds = Math.floor(ms / 1000);
    const minutes = Math.floor(seconds / 60);
    const hours = Math.floor(minutes / 60);

    if (hours > 0) {
      return `${hours}h ${minutes % 60}m`;
    } else if (minutes > 0) {
      return `${minutes}m ${seconds % 60}s`;
    } else {
      return `${seconds}s`;
    }
  };

  // No run selected state (when context is present but no run selected)
  if (runSelection && !selectedRun) {
    return (
      <div className="h-full flex flex-col items-center justify-center text-muted-foreground p-8">
        <Activity className="w-12 h-12 mb-4 opacity-50" />
        <p className="text-lg font-medium">No Run Selected</p>
        <p className="text-sm mt-2 text-center max-w-md">
          Select a run from the Run Dashboard to view the summary.
        </p>
      </div>
    );
  }

  // Get current report info
  const currentReport = findingsTracker.getCurrentReport();
  const isRunning = currentReport?.status === "running";

  // Calculate duration for selected run
  const duration = selectedRun
    ? calculateDuration(selectedRun.created_at, selectedRun.completed_at)
    : undefined;

  // Empty state
  if (displayFindings.length === 0 && !isRunning) {
    return (
      <div className="flex-1 flex flex-col items-center justify-center text-muted-foreground p-8">
        <FileText className="w-12 h-12 mb-4 opacity-50" />
        <p className="text-lg font-medium">No Summary Data Yet</p>
        <p className="text-sm mt-2 text-center max-w-md">
          Run an AI workflow to see a summary of code findings.
          <br />
          This view focuses on code-related issues (bugs, security, performance) and excludes
          automation infrastructure problems.
        </p>
      </div>
    );
  }

  return (
    <div className="h-full flex flex-col overflow-hidden">
      {/* Header */}
      <div className="flex-shrink-0 border-b border-border p-4 space-y-3">
        {/* Run Info Header */}
        <div className="flex items-center justify-between">
          <div className="flex items-center gap-3">
            <div className="p-2 bg-primary/10 rounded-lg">
              <ClipboardList className="w-5 h-5 text-primary" />
            </div>
            <div>
              <h2 className="font-semibold text-foreground">
                {selectedRun ? selectedRun.task_name || "Run Summary" : "Current Session"}
              </h2>
              {selectedRun ? (
                <div className="flex items-center gap-2 text-xs text-muted-foreground">
                  <Clock className="w-3 h-3" />
                  {new Date(selectedRun.created_at).toLocaleString()}
                  <span>({formatDuration(duration)})</span>
                </div>
              ) : isRunning ? (
                <div className="flex items-center gap-2 text-xs text-blue-400">
                  <Loader2 className="w-3 h-3 animate-spin" />
                  Running...
                </div>
              ) : (
                <div className="text-xs text-muted-foreground">
                  {findings.length > 0
                    ? `${displayFindings.length} code findings`
                    : "No active run"}
                </div>
              )}
            </div>
          </div>
        </div>

        {/* Run Status Badge */}
        {selectedRun && (
          <div className="flex items-center gap-2">
            {(() => {
              const StatusIcon = statusIcons[selectedRun.status] || AlertCircle;
              const color = statusColors[selectedRun.status] || "text-slate-400 bg-slate-500/10";
              return (
                <span className={`flex items-center gap-1.5 px-2 py-1 text-xs rounded ${color}`}>
                  <StatusIcon
                    className={`w-3 h-3 ${selectedRun.status === "running" ? "animate-spin" : ""}`}
                  />
                  {selectedRun.status.charAt(0).toUpperCase() + selectedRun.status.slice(1)}
                </span>
              );
            })()}
            {selectedRun.sessions_count > 0 && (
              <span className="flex items-center gap-1.5 text-xs text-muted-foreground px-2 py-1 bg-muted/30 rounded">
                <Users className="w-3 h-3" />
                {selectedRun.sessions_count} session{selectedRun.sessions_count !== 1 ? "s" : ""}
              </span>
            )}
            {/* Goal Achievement Badge */}
            {selectedRun.goal_achieved !== undefined && selectedRun.goal_achieved !== null && (
              <span
                className={`flex items-center gap-1.5 px-2 py-1 text-xs rounded ${
                  selectedRun.goal_achieved
                    ? "text-green-400 bg-green-500/10"
                    : "text-amber-400 bg-amber-500/10"
                }`}
              >
                <Target className="w-3 h-3" />
                {selectedRun.goal_achieved ? "Goal Achieved" : "Goal Not Achieved"}
              </span>
            )}
          </div>
        )}

        {/* Error message if run failed */}
        {selectedRun?.status === "failed" && selectedRun.error_message && (
          <div className="p-2 bg-red-500/10 border border-red-500/20 rounded text-sm">
            <span className="font-medium text-red-400">Error: </span>
            <span className="text-red-300">{selectedRun.error_message}</span>
          </div>
        )}
      </div>

      {/* AI Summary Section */}
      {selectedRun?.ai_summary && (
        <div className="flex-shrink-0 border-b border-border p-4 space-y-3 bg-muted/20">
          <div className="flex items-center gap-2">
            <Sparkles className="w-4 h-4 text-primary" />
            <h3 className="font-medium text-foreground">AI Summary</h3>
            {selectedRun.summary_generated_at && (
              <span className="text-xs text-muted-foreground">
                Generated {new Date(selectedRun.summary_generated_at).toLocaleString()}
              </span>
            )}
          </div>
          <p className="text-sm text-foreground/90 whitespace-pre-wrap leading-relaxed">
            {selectedRun.ai_summary}
          </p>
          {selectedRun.remaining_work && !selectedRun.goal_achieved && (
            <div className="mt-3 p-3 bg-amber-500/10 border border-amber-500/20 rounded">
              <div className="flex items-center gap-2 mb-1">
                <AlertTriangle className="w-4 h-4 text-amber-400" />
                <span className="font-medium text-amber-400 text-sm">Remaining Work</span>
              </div>
              <p className="text-sm text-amber-300/90 whitespace-pre-wrap">
                {selectedRun.remaining_work}
              </p>
            </div>
          )}
        </div>
      )}

      {/* Summary waiting indicator (when run complete but summary not generated) */}
      {selectedRun &&
        selectedRun.status !== "running" &&
        !selectedRun.ai_summary &&
        selectedRun.status === "complete" && (
          <div className="flex-shrink-0 border-b border-border p-4 bg-muted/20">
            <div className="flex items-center gap-2 text-muted-foreground">
              <Loader2 className="w-4 h-4 animate-spin" />
              <span className="text-sm">Generating AI summary...</span>
            </div>
          </div>
        )}

      {/* Findings Summary Stats */}
      <div className="flex-shrink-0 border-b border-border p-4">
        <div className="flex items-center gap-3 text-sm flex-wrap">
          <div className="flex items-center gap-2 px-3 py-1.5 bg-muted/30 rounded-lg">
            <span className="text-muted-foreground">Code Findings:</span>
            <span className="font-semibold">{summary.total}</span>
          </div>
          {summary.resolved > 0 && (
            <div className="flex items-center gap-2 px-3 py-1.5 bg-green-500/10 rounded-lg text-green-400">
              <CheckCircle className="w-4 h-4" />
              <span>{summary.resolved} Resolved</span>
            </div>
          )}
          {summary.pending > 0 && (
            <div className="flex items-center gap-2 px-3 py-1.5 bg-amber-500/10 rounded-lg text-amber-400">
              <AlertCircle className="w-4 h-4" />
              <span>{summary.pending} Pending</span>
            </div>
          )}
          {summary.critical > 0 && (
            <div className="flex items-center gap-2 px-3 py-1.5 bg-red-500/10 rounded-lg text-red-400">
              <span>{summary.critical} Critical</span>
            </div>
          )}
          {summary.high > 0 && (
            <div className="flex items-center gap-2 px-3 py-1.5 bg-orange-500/10 rounded-lg text-orange-400">
              <span>{summary.high} High</span>
            </div>
          )}
        </div>
      </div>

      {/* Findings by Category */}
      <div className="flex-1 min-h-0 overflow-y-auto p-4 space-y-4">
        {categoriesWithFindings.length === 0 ? (
          <div className="text-center text-muted-foreground py-8">
            <p>No code-related findings in this run.</p>
            <p className="text-sm mt-1">
              This view filters out automation/test infrastructure issues.
            </p>
          </div>
        ) : (
          categoriesWithFindings.map((category) => {
            const categoryFindings = findingsByCategory.get(category.id) || [];
            const isExpanded = expandedCategories.has(category.id);
            const resolved = categoryFindings.filter((f) => f.status === "resolved").length;
            const Icon = categoryIcons[category.id] || AlertCircle;

            return (
              <div key={category.id} className="border border-border rounded-lg overflow-hidden">
                {/* Category Header */}
                <button
                  onClick={() => toggleCategory(category.id)}
                  className="w-full flex items-center justify-between p-3 bg-muted/30 hover:bg-muted/50 transition-colors"
                >
                  <div className="flex items-center gap-3">
                    <Icon className={`w-5 h-5 text-${category.color}-400`} />
                    <span className="font-medium">{category.name}</span>
                    <span className="text-sm text-muted-foreground">
                      ({categoryFindings.length} finding{categoryFindings.length !== 1 ? "s" : ""},{" "}
                      {resolved} resolved)
                    </span>
                  </div>
                  {isExpanded ? (
                    <ChevronDown className="w-4 h-4 text-muted-foreground" />
                  ) : (
                    <ChevronRight className="w-4 h-4 text-muted-foreground" />
                  )}
                </button>

                {/* Findings List */}
                {isExpanded && (
                  <div className="divide-y divide-border">
                    {categoryFindings.map((finding) => (
                      <div
                        key={finding.id}
                        className={`p-3 ${finding.status === "resolved" ? "bg-muted/20" : ""}`}
                      >
                        <div className="flex items-start justify-between gap-3">
                          <div className="flex-1 min-w-0">
                            <div className="flex items-center gap-2 flex-wrap">
                              <span
                                className={`px-2 py-0.5 text-xs rounded ${
                                  severityColors[finding.severity]
                                }`}
                              >
                                {finding.severity.toUpperCase()}
                              </span>
                              {finding.status === "resolved" && (
                                <span className="px-2 py-0.5 text-xs rounded bg-green-500/10 text-green-400">
                                  RESOLVED
                                </span>
                              )}
                              <span className="font-medium truncate">{finding.title}</span>
                            </div>
                            <p className="text-sm text-muted-foreground mt-1">
                              {finding.description}
                            </p>
                            {finding.codeContext?.file && (
                              <div className="flex items-center gap-1 text-xs text-muted-foreground mt-1">
                                <ExternalLink className="w-3 h-3" />
                                <span>
                                  {finding.codeContext.file}
                                  {finding.codeContext.line && `:${finding.codeContext.line}`}
                                </span>
                              </div>
                            )}
                            {finding.resolution && (
                              <div className="mt-2 p-2 bg-green-500/5 border border-green-500/20 rounded text-sm">
                                <span className="font-medium text-green-400">Resolution: </span>
                                <span className="text-green-300">{finding.resolution}</span>
                              </div>
                            )}
                          </div>
                        </div>
                      </div>
                    ))}
                  </div>
                )}
              </div>
            );
          })
        )}

        {/* Note about excluded categories */}
        {displayFindings.length > 0 && (
          <p className="text-xs text-muted-foreground text-center mt-4">
            This summary shows code-related findings only.
            {findings.length - displayFindings.length > 0 && (
              <span>
                {" "}
                {findings.length - displayFindings.length} automation/infrastructure findings are
                hidden (view in Findings tab).
              </span>
            )}
          </p>
        )}
      </div>
    </div>
  );
}

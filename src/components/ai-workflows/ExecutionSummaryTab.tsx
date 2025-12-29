/**
 * ExecutionSummaryTab.tsx
 *
 * Shows a comprehensive summary of AI execution sessions.
 * Focuses on code-related findings (bugs, security, performance)
 * and excludes automation/test infrastructure issues.
 *
 * Shows per-session summaries with session history.
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
  History,
  Timer,
  XCircle,
  AlertOctagon,
} from "lucide-react";
import type { Finding, FindingSeverity } from "../../types/findings";
import { findingsTracker, getVisibleCategories } from "../../services";
import type { SessionHistoryEntry } from "../../services/FindingsTracker";

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

const statusIcons: Record<string, React.ComponentType<{ className?: string }>> = {
  completed: CheckCircle,
  failed: XCircle,
  timeout: Timer,
  context_limit: AlertOctagon,
  running: Loader2,
};

const statusColors: Record<string, string> = {
  completed: "text-green-400 bg-green-500/10",
  failed: "text-red-400 bg-red-500/10",
  timeout: "text-amber-400 bg-amber-500/10",
  context_limit: "text-orange-400 bg-orange-500/10",
  running: "text-blue-400 bg-blue-500/10",
};

export function ExecutionSummaryTab() {
  const [findings, setFindings] = useState<Finding[]>([]);
  const [sessionHistory, setSessionHistory] = useState<SessionHistoryEntry[]>([]);
  const [selectedSession, setSelectedSession] = useState<SessionHistoryEntry | null>(null);
  const [expandedCategories, setExpandedCategories] = useState<Set<string>>(
    new Set(["code_bug", "security"]),
  );
  const [showHistory, setShowHistory] = useState(false);

  // Load session history and subscribe to updates
  useEffect(() => {
    // Load history from disk on mount
    findingsTracker.loadFromDisk().then(() => {
      setSessionHistory(findingsTracker.getSessionHistory());
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
        setSessionHistory(findingsTracker.getSessionHistory());
      }
    });

    // Initialize with current state
    setFindings(findingsTracker.getAllFindings());

    return unsubscribe;
  }, []);

  // Filter findings to code-focused categories
  const filterCodeFocusedFindings = (allFindings: Finding[]) => {
    return allFindings.filter((f) => CODE_FOCUSED_CATEGORIES.includes(f.categoryId));
  };

  // Get the findings to display (either from selected session or current)
  const displayFindings = useMemo(() => {
    if (selectedSession) {
      return filterCodeFocusedFindings(selectedSession.findings);
    }
    return filterCodeFocusedFindings(findings);
  }, [selectedSession, findings]);

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

  const formatDuration = (ms: number) => {
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

  const formatDate = (timestamp: number) => {
    const date = new Date(timestamp);
    const now = new Date();
    const isToday = date.toDateString() === now.toDateString();

    if (isToday) {
      return date.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
    }
    return date.toLocaleDateString([], { month: "short", day: "numeric" });
  };

  // Get current report info
  const currentReport = findingsTracker.getCurrentReport();
  const isRunning = currentReport?.status === "running";

  // Empty state
  if (displayFindings.length === 0 && sessionHistory.length === 0 && !isRunning) {
    return (
      <div className="flex-1 flex flex-col items-center justify-center text-muted-foreground p-8">
        <FileText className="w-12 h-12 mb-4 opacity-50" />
        <p className="text-lg font-medium">No Execution Summary Yet</p>
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
        {/* Title and History Toggle */}
        <div className="flex items-center justify-between">
          <div className="flex items-center gap-3">
            <div className="p-2 bg-primary/10 rounded-lg">
              <FileText className="w-5 h-5 text-primary" />
            </div>
            <div>
              <h2 className="font-semibold text-foreground">
                {selectedSession ? selectedSession.promptName : "Current Session"}
              </h2>
              {selectedSession ? (
                <div className="flex items-center gap-2 text-xs text-muted-foreground">
                  <Clock className="w-3 h-3" />
                  {new Date(selectedSession.startedAt).toLocaleString()}
                  <span>({formatDuration(selectedSession.duration)})</span>
                  <span className="px-1.5 py-0.5 bg-muted rounded">
                    {selectedSession.phases} phase{selectedSession.phases !== 1 ? "s" : ""}
                  </span>
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
                    : "No active session"}
                </div>
              )}
            </div>
          </div>

          {/* Session Controls */}
          <div className="flex items-center gap-2">
            {selectedSession && (
              <button
                onClick={() => setSelectedSession(null)}
                className="px-3 py-1.5 text-sm bg-primary/10 text-primary rounded-lg hover:bg-primary/20 transition-colors"
              >
                View Current
              </button>
            )}
            <button
              onClick={() => setShowHistory(!showHistory)}
              className={`flex items-center gap-2 px-3 py-1.5 text-sm rounded-lg transition-colors ${
                showHistory
                  ? "bg-primary text-primary-foreground"
                  : "bg-muted/50 hover:bg-muted text-foreground"
              }`}
            >
              <History className="w-4 h-4" />
              History ({sessionHistory.length})
            </button>
          </div>
        </div>

        {/* Session Status Badge */}
        {selectedSession && (
          <div className="flex items-center gap-2">
            {(() => {
              const StatusIcon = statusIcons[selectedSession.status] || AlertCircle;
              const color =
                statusColors[selectedSession.status] || "text-slate-400 bg-slate-500/10";
              return (
                <span className={`flex items-center gap-1.5 px-2 py-1 text-xs rounded ${color}`}>
                  <StatusIcon className="w-3 h-3" />
                  {selectedSession.status === "context_limit"
                    ? "Context Limit"
                    : selectedSession.status.charAt(0).toUpperCase() +
                      selectedSession.status.slice(1)}
                </span>
              );
            })()}
          </div>
        )}

        {/* Summary Stats */}
        <div className="flex items-center gap-3 text-sm flex-wrap">
          <div className="flex items-center gap-2 px-3 py-1.5 bg-muted/30 rounded-lg">
            <span className="text-muted-foreground">Total:</span>
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

      {/* Session History Panel (collapsible) */}
      {showHistory && sessionHistory.length > 0 && (
        <div className="flex-shrink-0 border-b border-border bg-muted/20 p-3 max-h-48 overflow-y-auto">
          <div className="space-y-2">
            {sessionHistory.map((session) => {
              const StatusIcon = statusIcons[session.status] || AlertCircle;
              const color = statusColors[session.status] || "text-slate-400";
              const codeFindingsCount = filterCodeFocusedFindings(session.findings).length;

              return (
                <button
                  key={session.id}
                  onClick={() => {
                    setSelectedSession(session);
                    setShowHistory(false);
                  }}
                  className={`w-full flex items-center justify-between p-2 rounded-lg transition-colors ${
                    selectedSession?.id === session.id
                      ? "bg-primary/10 border border-primary/30"
                      : "bg-background hover:bg-muted/50"
                  }`}
                >
                  <div className="flex items-center gap-3">
                    <StatusIcon className={`w-4 h-4 ${color}`} />
                    <div className="text-left">
                      <div className="text-sm font-medium truncate max-w-[200px]">
                        {session.promptName}
                      </div>
                      <div className="text-xs text-muted-foreground">
                        {formatDate(session.startedAt)} · {formatDuration(session.duration)}
                      </div>
                    </div>
                  </div>
                  <div className="flex items-center gap-2 text-xs">
                    <span className="text-muted-foreground">
                      {codeFindingsCount} finding{codeFindingsCount !== 1 ? "s" : ""}
                    </span>
                    <span
                      className={`px-1.5 py-0.5 rounded ${
                        session.summary.resolved > 0 ? "bg-green-500/10 text-green-400" : ""
                      }`}
                    >
                      {session.summary.resolved} fixed
                    </span>
                  </div>
                </button>
              );
            })}
          </div>
        </div>
      )}

      {/* Findings by Category */}
      <div className="flex-1 min-h-0 overflow-y-auto p-4 space-y-4">
        {categoriesWithFindings.length === 0 ? (
          <div className="text-center text-muted-foreground py-8">
            <p>No code-related findings in this session.</p>
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
            {findings.length - displayFindings.length > 0 && !selectedSession && (
              <span>
                {" "}
                {findings.length - displayFindings.length} automation/infrastructure findings are
                hidden (view in All Findings tab).
              </span>
            )}
          </p>
        )}
      </div>
    </div>
  );
}

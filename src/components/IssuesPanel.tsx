/**
 * IssuesPanel.tsx
 *
 * Panel component that displays detected issues during AI-assisted automation.
 * Shows a summary of issues by status and severity, with expandable details.
 */

import { useState, useEffect, useCallback } from "react";
import {
  AlertTriangle,
  CheckCircle,
  Clock,
  XCircle,
  ChevronDown,
  ChevronRight,
  FileText,
  Image,
  Terminal,
  TestTube,
  Brain,
  HelpCircle,
  Trash2,
} from "lucide-react";
import {
  DetectedIssue,
  IssueSessionSummary,
  SEVERITY_CONFIG,
  STATUS_CONFIG,
  SOURCE_TYPE_LABELS,
  IssueSourceType,
} from "../types/issues";
import { issueTracker, IssueTrackerEvent } from "../services/IssueTracker";

interface IssuesPanelProps {
  /** Callback when an issue is clicked for details */
  onIssueClick?: (issue: DetectedIssue) => void;
}

/** Icon for source types */
const SOURCE_ICONS: Record<IssueSourceType, React.ReactNode> = {
  log: <FileText className="w-3 h-3" />,
  screenshot: <Image className="w-3 h-3" />,
  console: <Terminal className="w-3 h-3" />,
  test_output: <TestTube className="w-3 h-3" />,
  ai_analysis: <Brain className="w-3 h-3" />,
  other: <HelpCircle className="w-3 h-3" />,
};

/** Status icons */
const STATUS_ICONS: Record<string, React.ReactNode> = {
  detected: <AlertTriangle className="w-4 h-4" />,
  in_progress: <Clock className="w-4 h-4" />,
  resolved: <CheckCircle className="w-4 h-4" />,
  skipped: <XCircle className="w-4 h-4" />,
};

export function IssuesPanel({ onIssueClick }: IssuesPanelProps) {
  const [issues, setIssues] = useState<DetectedIssue[]>([]);
  const [summary, setSummary] = useState<IssueSessionSummary | null>(null);
  const [expandedIssues, setExpandedIssues] = useState<Set<string>>(new Set());

  // Subscribe to issue changes
  useEffect(() => {
    const updateIssues = () => {
      setIssues(issueTracker.getSessionIssues());
      setSummary(issueTracker.getSessionSummary());
    };

    // Initial load
    updateIssues();

    // Subscribe to changes
    const unsubscribe = issueTracker.subscribe((event: IssueTrackerEvent) => {
      updateIssues();
    });

    return unsubscribe;
  }, []);

  const toggleExpanded = useCallback((issueId: string) => {
    setExpandedIssues((prev) => {
      const next = new Set(prev);
      if (next.has(issueId)) {
        next.delete(issueId);
      } else {
        next.add(issueId);
      }
      return next;
    });
  }, []);

  const handleStatusChange = useCallback(
    (issueId: string, newStatus: "in_progress" | "resolved" | "skipped") => {
      if (newStatus === "resolved") {
        issueTracker.resolveIssue(issueId, "Manually marked as resolved");
      } else if (newStatus === "skipped") {
        issueTracker.skipIssue(issueId);
      } else {
        issueTracker.updateIssueStatus(issueId, newStatus);
      }
    },
    [],
  );

  const handleClearAll = useCallback(() => {
    issueTracker.clearSession();
  }, []);

  if (issues.length === 0) {
    return (
      <div className="h-full flex flex-col items-center justify-center text-muted-foreground">
        <AlertTriangle className="w-12 h-12 mb-4 opacity-50" />
        <p className="text-lg font-medium">No Issues Detected</p>
        <p className="text-sm mt-2">Issues found by AI will appear here during automation</p>
      </div>
    );
  }

  return (
    <div className="h-full flex flex-col">
      {/* Summary Stats */}
      {summary && (
        <div className="flex-shrink-0 p-4 border-b border-border">
          <div className="flex items-center justify-between mb-3">
            <h3 className="text-sm font-semibold">Session Summary</h3>
            <button
              onClick={handleClearAll}
              className="flex items-center gap-1 px-2 py-1 text-xs text-muted-foreground hover:text-foreground transition-colors"
              title="Clear all issues"
            >
              <Trash2 className="w-3 h-3" />
              Clear
            </button>
          </div>

          {/* Status counts */}
          <div className="grid grid-cols-4 gap-2 mb-3">
            <div className="flex flex-col items-center p-2 rounded bg-red-500/10">
              <span className="text-lg font-bold text-red-400">{summary.by_status.detected}</span>
              <span className="text-xs text-muted-foreground">Detected</span>
            </div>
            <div className="flex flex-col items-center p-2 rounded bg-yellow-500/10">
              <span className="text-lg font-bold text-yellow-400">
                {summary.by_status.in_progress}
              </span>
              <span className="text-xs text-muted-foreground">In Progress</span>
            </div>
            <div className="flex flex-col items-center p-2 rounded bg-green-500/10">
              <span className="text-lg font-bold text-green-400">{summary.by_status.resolved}</span>
              <span className="text-xs text-muted-foreground">Resolved</span>
            </div>
            <div className="flex flex-col items-center p-2 rounded bg-gray-500/10">
              <span className="text-lg font-bold text-gray-400">{summary.by_status.skipped}</span>
              <span className="text-xs text-muted-foreground">Skipped</span>
            </div>
          </div>

          {/* Severity breakdown */}
          <div className="flex gap-4 text-xs">
            {(["critical", "high", "medium", "low"] as const).map((severity) => {
              const count = summary.by_severity[severity];
              if (count === 0) return null;
              const config = SEVERITY_CONFIG[severity];
              return (
                <div key={severity} className="flex items-center gap-1">
                  <span className={`w-2 h-2 rounded-full ${config.bgColor}`} />
                  <span className={config.color}>{count}</span>
                  <span className="text-muted-foreground">{config.label}</span>
                </div>
              );
            })}
          </div>
        </div>
      )}

      {/* Issues List */}
      <div className="flex-1 overflow-auto p-2">
        <div className="space-y-2">
          {issues.map((issue) => {
            const isExpanded = expandedIssues.has(issue.id);
            const severityConfig = SEVERITY_CONFIG[issue.severity];
            const statusConfig = STATUS_CONFIG[issue.status];

            return (
              <div
                key={issue.id}
                className={`border rounded-lg overflow-hidden ${severityConfig.bgColor} border-${issue.severity === "critical" ? "red" : issue.severity === "high" ? "orange" : issue.severity === "medium" ? "yellow" : "blue"}-500/30`}
              >
                {/* Issue Header */}
                <div
                  className="flex items-center gap-2 p-3 cursor-pointer hover:bg-black/5 transition-colors"
                  onClick={() => toggleExpanded(issue.id)}
                >
                  {/* Expand/Collapse */}
                  {isExpanded ? (
                    <ChevronDown className="w-4 h-4 text-muted-foreground flex-shrink-0" />
                  ) : (
                    <ChevronRight className="w-4 h-4 text-muted-foreground flex-shrink-0" />
                  )}

                  {/* Status Icon */}
                  <span className={statusConfig.color}>{STATUS_ICONS[issue.status]}</span>

                  {/* Title */}
                  <span className="flex-1 font-medium text-sm truncate">{issue.title}</span>

                  {/* Severity Badge */}
                  <span
                    className={`px-2 py-0.5 text-xs rounded ${severityConfig.bgColor} ${severityConfig.color}`}
                  >
                    {severityConfig.label}
                  </span>

                  {/* Source indicator */}
                  <span
                    className="flex items-center gap-1 text-xs text-muted-foreground"
                    title={SOURCE_TYPE_LABELS[issue.source.type]}
                  >
                    {SOURCE_ICONS[issue.source.type]}
                  </span>
                </div>

                {/* Expanded Details */}
                {isExpanded && (
                  <div className="px-3 pb-3 border-t border-border/50">
                    {/* Description */}
                    <div className="mt-2">
                      <p className="text-sm text-foreground/80 whitespace-pre-wrap">
                        {issue.description}
                      </p>
                    </div>

                    {/* Location */}
                    {(issue.file || issue.line) && (
                      <div className="mt-2 flex items-center gap-2 text-xs text-muted-foreground">
                        <FileText className="w-3 h-3" />
                        <span>
                          {issue.file}
                          {issue.line && `:${issue.line}`}
                        </span>
                      </div>
                    )}

                    {/* Source */}
                    <div className="mt-2 flex items-center gap-2 text-xs text-muted-foreground">
                      {SOURCE_ICONS[issue.source.type]}
                      <span>
                        Found in:{" "}
                        {issue.source.description || SOURCE_TYPE_LABELS[issue.source.type]}
                        {issue.source.path && ` (${issue.source.path})`}
                        {issue.source.line_range &&
                          ` lines ${issue.source.line_range[0]}-${issue.source.line_range[1]}`}
                      </span>
                    </div>

                    {/* Resolution */}
                    {issue.resolution && (
                      <div className="mt-2 p-2 rounded bg-green-500/10 text-sm">
                        <span className="font-medium text-green-400">Resolution: </span>
                        <span className="text-foreground/80">{issue.resolution}</span>
                      </div>
                    )}

                    {/* Timestamps */}
                    <div className="mt-2 flex gap-4 text-xs text-muted-foreground">
                      <span>Detected: {new Date(issue.detected_at).toLocaleTimeString()}</span>
                      {issue.resolved_at && (
                        <span>Resolved: {new Date(issue.resolved_at).toLocaleTimeString()}</span>
                      )}
                    </div>

                    {/* Actions */}
                    {issue.status !== "resolved" && issue.status !== "skipped" && (
                      <div className="mt-3 flex gap-2">
                        {issue.status === "detected" && (
                          <button
                            onClick={(e) => {
                              e.stopPropagation();
                              handleStatusChange(issue.id, "in_progress");
                            }}
                            className="px-2 py-1 text-xs bg-yellow-500/20 text-yellow-400 rounded hover:bg-yellow-500/30 transition-colors"
                          >
                            Mark In Progress
                          </button>
                        )}
                        <button
                          onClick={(e) => {
                            e.stopPropagation();
                            handleStatusChange(issue.id, "resolved");
                          }}
                          className="px-2 py-1 text-xs bg-green-500/20 text-green-400 rounded hover:bg-green-500/30 transition-colors"
                        >
                          Mark Resolved
                        </button>
                        <button
                          onClick={(e) => {
                            e.stopPropagation();
                            handleStatusChange(issue.id, "skipped");
                          }}
                          className="px-2 py-1 text-xs bg-gray-500/20 text-gray-400 rounded hover:bg-gray-500/30 transition-colors"
                        >
                          Skip
                        </button>
                      </div>
                    )}
                  </div>
                )}
              </div>
            );
          })}
        </div>
      </div>
    </div>
  );
}

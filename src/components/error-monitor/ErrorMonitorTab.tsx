/**
 * ErrorMonitorTab.tsx
 *
 * Main tab component for the error monitoring system.
 * Shows a list of errors with filtering, status management, and fix workflow generation.
 */

import { useState, useMemo, useEffect } from "react";
import {
  AlertCircle,
  AlertTriangle,
  Bug,
  CheckCircle,
  ChevronDown,
  Clock,
  Eye,
  Filter,
  ExternalLink,
  FolderOpen,
  Info,
  Play,
  RefreshCw,
  Search,
  Settings,
  X,
} from "lucide-react";
import { cn } from "../../lib/utils";
import { ScrollArea } from "../ui/ScrollArea";
import { getStatusColors } from "@/design-system";
import { useErrorEvents, useErrorSummary, useDebugContext, ERROR_MONITOR_REFRESH_INTERVAL } from "../../hooks/useErrorMonitor";
import { errorMonitorService, formatErrorTime } from "../../services/error-monitor-service";
import type { StoredErrorEvent, ErrorSeverity, ErrorStatus, RecurrenceEntry } from "../../types/errorMonitor";
import { FixErrorsButton } from "./FixErrorsButton";
import { BrowserErrorsPanel } from "./BrowserErrorsPanel";
// Log source management is now in Settings > Log Sources (LogSourcesSettings.tsx)

// =============================================================================
// Sub-components
// =============================================================================

function SeverityIcon({ severity }: { severity: ErrorSeverity }) {
  const className = "w-4 h-4";
  switch (severity) {
    case "critical":
      return <AlertCircle className={cn(className, "text-red-600")} />;
    case "error":
      return <Bug className={cn(className, "text-red-500")} />;
    case "warning":
      return <AlertTriangle className={cn(className, "text-yellow-500")} />;
    case "info":
      return <Info className={cn(className, "text-blue-500")} />;
    case "debug":
      return <Settings className={cn(className, "text-gray-500")} />;
    default:
      return <Info className={cn(className, "text-muted-foreground")} />;
  }
}

function SeverityBadge({ severity, count }: { severity: ErrorSeverity; count: number }) {
  if (count === 0) return null;

  const colors = {
    critical: "bg-red-600/20 text-red-600 border-red-600/30",
    error: "bg-red-500/20 text-red-500 border-red-500/30",
    warning: "bg-yellow-500/20 text-yellow-500 border-yellow-500/30",
    info: "bg-blue-500/20 text-blue-500 border-blue-500/30",
    debug: "bg-gray-500/20 text-gray-500 border-gray-500/30",
  };

  return (
    <span
      data-content-role="badge"
      data-content-label={`${severity} error count`}
      className={cn("px-2 py-0.5 rounded-md text-xs font-medium border", colors[severity])}
    >
      {count} {severity}
    </span>
  );
}

function StatusBadge({ status }: { status: ErrorStatus }) {
  const statusConfig: Record<ErrorStatus, { label: string; className: string }> = {
    new: { label: "New", className: "bg-red-500/20 text-red-500" },
    acknowledged: {
      label: "Acknowledged",
      className: "bg-yellow-500/20 text-yellow-500",
    },
    in_progress: {
      label: "In Progress",
      className: "bg-blue-500/20 text-blue-500",
    },
    resolved: {
      label: "Resolved",
      className: "bg-green-500/20 text-green-500",
    },
    ignored: { label: "Ignored", className: "bg-gray-500/20 text-gray-500" },
    recurring: {
      label: "Recurring",
      className: "bg-orange-500/20 text-orange-500",
    },
    promoted: {
      label: "Promoted",
      className: "bg-purple-500/20 text-purple-500",
    },
  };

  const config = statusConfig[status] || { label: status, className: "" };

  return (
    <span
      data-content-role="badge"
      data-content-label="error status"
      className={cn("px-2 py-0.5 rounded-md text-xs font-medium", config.className)}
    >
      {config.label}
    </span>
  );
}

function RecurrenceHistory({ signatureHash }: { signatureHash: string }) {
  const [entries, setEntries] = useState<RecurrenceEntry[] | null>(null);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    errorMonitorService
      .getRecurrenceHistory(signatureHash)
      .then((data) => {
        if (!cancelled) setEntries(data);
      })
      .catch(() => {
        if (!cancelled) setEntries([]);
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [signatureHash]);

  if (loading) {
    return (
      <div className="text-xs text-muted-foreground py-1">Loading recurrence history...</div>
    );
  }

  if (!entries || entries.length === 0) {
    return null;
  }

  return (
    <div>
      <span className="text-xs font-medium text-orange-400">
        Previously resolved {entries.length} time{entries.length !== 1 ? "s" : ""}
      </span>
      <div className="mt-1 space-y-1">
        {entries.map((entry) => (
          <div
            key={entry.id}
            className="flex items-center gap-2 text-xs text-muted-foreground bg-orange-500/5 px-2 py-1 rounded"
          >
            <span className="shrink-0">
              {entry.resolvedAt
                ? new Date(entry.resolvedAt).toLocaleDateString()
                : "unknown date"}
            </span>
            <span className="text-orange-400/60">|</span>
            <span className="shrink-0">x{entry.occurrenceCount}</span>
            {entry.resolutionNotes && (
              <>
                <span className="text-orange-400/60">|</span>
                <span className="truncate">{entry.resolutionNotes}</span>
              </>
            )}
          </div>
        ))}
      </div>
    </div>
  );
}

function ErrorItem({
  error,
  onAcknowledge,
  onResolve,
  onIgnore,
  isExpanded,
  onToggleExpand,
}: {
  error: StoredErrorEvent;
  onAcknowledge: () => void;
  onResolve: () => void;
  onIgnore: () => void;
  isExpanded: boolean;
  onToggleExpand: () => void;
}) {
  return (
    <div
      className={cn(
        "border-b border-border/50 last:border-b-0",
        "hover:bg-muted/30 transition-colors",
      )}
    >
      {/* Header row */}
      <div
        role="button"
        tabIndex={0}
        className="flex items-start gap-3 px-4 py-3 cursor-pointer"
        onClick={onToggleExpand}
        onKeyDown={(e) => {
          if (e.key === "Enter" || e.key === " ") {
            e.preventDefault();
            onToggleExpand();
          }
        }}
      >
        {/* Severity icon */}
        <div className="shrink-0 mt-0.5">
          <SeverityIcon severity={error.severity} />
        </div>

        {/* Content */}
        <div className="flex-1 min-w-0">
          <div className="flex items-center gap-2 mb-1">
            <span
              data-content-role="label"
              data-content-label="error type"
              className="text-sm font-medium truncate"
            >
              {error.errorType || "Unknown Error"}
            </span>
            <StatusBadge status={error.status} />
            {error.occurrenceCount > 1 && (
              <span className="text-xs text-muted-foreground">x{error.occurrenceCount}</span>
            )}
          </div>
          <p className="text-sm text-muted-foreground line-clamp-2">{error.message}</p>
          <div className="flex items-center gap-3 mt-1 text-xs text-muted-foreground">
            <span className="flex items-center gap-1">
              <FolderOpen className="w-3 h-3" />
              {error.logSourceName}
            </span>
            {error.workflowName && (
              <span className="flex items-center gap-1">
                <Play className="w-3 h-3" />
                {error.workflowName}
              </span>
            )}
            {error.location && (
              <span>
                {error.location.filePath}
                {error.location.lineNumber && `:${error.location.lineNumber}`}
              </span>
            )}
            <span className="flex items-center gap-1">
              <Clock className="w-3 h-3" />
              {formatErrorTime(error.capturedAt)}
            </span>
          </div>
        </div>

        {/* Expand indicator */}
        <ChevronDown
          className={cn(
            "w-4 h-4 text-muted-foreground transition-transform",
            isExpanded && "rotate-180",
          )}
        />
      </div>

      {/* Expanded details */}
      {isExpanded && (
        <div className="px-4 pb-3 pl-11 space-y-3">
          {/* Full message */}
          <div>
            <span className="text-xs font-medium text-muted-foreground">Full Message</span>
            <p className="text-sm mt-1 whitespace-pre-wrap bg-muted/50 p-2 rounded">
              {error.message}
            </p>
          </div>

          {/* Stack trace */}
          {error.stackTrace && (
            <div>
              <span className="text-xs font-medium text-muted-foreground">Stack Trace</span>
              <pre className="text-xs mt-1 bg-muted/50 p-2 rounded overflow-x-auto max-h-48">
                {error.stackTrace}
              </pre>
            </div>
          )}

          {/* Context lines */}
          {error.contextLines && (
            <div>
              <span className="text-xs font-medium text-muted-foreground">Context</span>
              <pre className="text-xs mt-1 bg-muted/50 p-2 rounded overflow-x-auto">
                {error.contextLines}
              </pre>
            </div>
          )}

          {/* Resolution notes */}
          {error.resolutionNotes && (
            <div>
              <span className="text-xs font-medium text-muted-foreground">Resolution Notes</span>
              <p className="text-sm mt-1 bg-green-500/10 p-2 rounded">{error.resolutionNotes}</p>
            </div>
          )}

          {/* Recurrence history (only for recurring errors) */}
          {error.status === "recurring" && (
            <RecurrenceHistory signatureHash={error.signatureHash} />
          )}

          {/* Open in Editor */}
          {error.location?.filePath && (
            <div className="flex gap-2 pt-2">
              <button
                onClick={(e) => {
                  e.stopPropagation();
                  errorMonitorService.openInEditor(
                    error.location!.filePath,
                    error.location!.lineNumber,
                    error.location!.columnNumber,
                  );
                }}
                className="flex items-center gap-1 px-3 py-1.5 text-xs bg-primary/20 text-primary rounded hover:bg-primary/30 transition-colors"
              >
                <ExternalLink className="w-3 h-3" />
                Open in Editor
              </button>
            </div>
          )}

          {/* Actions */}
          {error.status !== "resolved" && error.status !== "ignored" && (
            <div className="flex gap-2 pt-2">
              {error.status === "new" && (
                <button
                  onClick={(e) => {
                    e.stopPropagation();
                    onAcknowledge();
                  }}
                  className="flex items-center gap-1 px-3 py-1.5 text-xs bg-yellow-500/20 text-yellow-600 rounded hover:bg-yellow-500/30 transition-colors"
                >
                  <Eye className="w-3 h-3" />
                  Acknowledge
                </button>
              )}
              <button
                onClick={(e) => {
                  e.stopPropagation();
                  onResolve();
                }}
                className="flex items-center gap-1 px-3 py-1.5 text-xs bg-green-500/20 text-green-600 rounded hover:bg-green-500/30 transition-colors"
              >
                <CheckCircle className="w-3 h-3" />
                Mark Resolved
              </button>
              <button
                onClick={(e) => {
                  e.stopPropagation();
                  onIgnore();
                }}
                className="flex items-center gap-1 px-3 py-1.5 text-xs bg-gray-500/20 text-gray-600 rounded hover:bg-gray-500/30 transition-colors"
              >
                <X className="w-3 h-3" />
                Ignore
              </button>
            </div>
          )}
        </div>
      )}
    </div>
  );
}

function EmptyState({ isScoped }: { isScoped?: boolean }) {
  const successColors = getStatusColors("success");
  return (
    <div className="flex-1 flex flex-col items-center justify-center text-center p-8">
      <div
        className={cn(
          "w-16 h-16 rounded-full flex items-center justify-center mb-4",
          successColors.bg,
        )}
      >
        <CheckCircle className={cn("w-8 h-8", successColors.text)} />
      </div>
      <h3 className="text-lg font-medium mb-2">
        {isScoped ? "No Errors in This Run" : "No Errors Detected"}
      </h3>
      <p className="text-sm text-muted-foreground max-w-xs">
        {isScoped
          ? "This task run did not produce any errors matching the current filters."
          : "Configure log sources to start monitoring your application for errors."}
      </p>
    </div>
  );
}

// =============================================================================
// Main Component
// =============================================================================

interface ErrorMonitorTabProps {
  /** When set, show only errors from this task run */
  taskRunId?: string;
  /** Display name for the run (shown in scope indicator) */
  taskRunName?: string;
  /** Callback to clear the per-run filter */
  onClearScope?: () => void;
}

export function ErrorMonitorTab({
  taskRunId,
  taskRunName,
  onClearScope,
}: ErrorMonitorTabProps = {}) {
  const [searchText, setSearchText] = useState("");
  const [selectedSeverities, setSelectedSeverities] = useState<ErrorSeverity[]>([]);
  const [selectedStatuses, setSelectedStatuses] = useState<ErrorStatus[]>([
    "new",
    "acknowledged",
    "in_progress",
    "promoted",
  ]);
  const [showFilters, setShowFilters] = useState(false);
  const [expandedErrorId, setExpandedErrorId] = useState<number | null>(null);
  const [selectedPatternIds, setSelectedPatternIds] = useState<Set<number> | null>(null);
  // Data hooks
  const {
    errors,
    loading,
    error: fetchError,
    refresh,
    acknowledge,
    resolve,
    ignore,
  } = useErrorEvents({
    taskRunId,
    severities: selectedSeverities.length > 0 ? selectedSeverities : undefined,
    statuses: selectedStatuses.length > 0 ? selectedStatuses : undefined,
    refreshInterval: ERROR_MONITOR_REFRESH_INTERVAL,
  });

  const { summary } = useErrorSummary({ taskRunId, refreshInterval: ERROR_MONITOR_REFRESH_INTERVAL });

  const { context: debugContext } = useDebugContext({ taskRunId });
  const patterns = debugContext?.patterns ?? [];

  // Filter errors by search text and selected pattern
  const filteredErrors = useMemo(() => {
    let result = errors;

    // Pattern cluster filter
    if (selectedPatternIds) {
      result = result.filter((e) => selectedPatternIds.has(e.id));
    }

    // Text search filter
    if (searchText) {
      const search = searchText.toLowerCase();
      result = result.filter(
        (e) =>
          e.message.toLowerCase().includes(search) ||
          e.errorType?.toLowerCase().includes(search) ||
          e.logSourceName.toLowerCase().includes(search) ||
          e.workflowName?.toLowerCase().includes(search),
      );
    }

    return result;
  }, [errors, searchText, selectedPatternIds]);

  const severityOptions: ErrorSeverity[] = ["critical", "error", "warning", "info", "debug"];
  const statusOptions: ErrorStatus[] = [
    "new",
    "acknowledged",
    "in_progress",
    "resolved",
    "ignored",
    "recurring",
  ];

  const toggleSeverity = (severity: ErrorSeverity) => {
    setSelectedSeverities((prev) =>
      prev.includes(severity) ? prev.filter((s) => s !== severity) : [...prev, severity],
    );
  };

  const toggleStatus = (status: ErrorStatus) => {
    setSelectedStatuses((prev) =>
      prev.includes(status) ? prev.filter((s) => s !== status) : [...prev, status],
    );
  };

  const clearFilters = () => {
    setSelectedSeverities([]);
    setSelectedStatuses([]);
    setSearchText("");
    setSelectedPatternIds(null);
  };

  const hasActiveFilters =
    selectedSeverities.length > 0 || selectedStatuses.length > 0 || searchText.length > 0 || selectedPatternIds !== null;

  return (
    <div className="flex flex-col h-full">
      {/* Header */}
      <div className="px-4 py-3 border-b border-border bg-card">
        <div className="flex items-center justify-between mb-3">
          <div className="flex items-center gap-2">
            <Bug className="w-5 h-5 text-primary" />
            <h2 className="text-lg font-semibold">Error Monitor</h2>
            {summary && summary.unresolvedCount > 0 && (
              <span
                data-content-role="badge"
                data-content-label="unresolved error count"
                className="px-2 py-0.5 text-xs bg-red-500/20 text-red-500 rounded-full"
              >
                {summary.unresolvedCount} unresolved
              </span>
            )}
          </div>
          <div className="flex items-center gap-2">
            <FixErrorsButton taskRunId={taskRunId} />
            <span className="text-xs text-muted-foreground">
              Manage sources in Settings &gt; Log Sources
            </span>
            <button
              onClick={refresh}
              disabled={loading}
              className="p-1.5 hover:bg-muted rounded transition-colors"
              title="Refresh"
            >
              <RefreshCw className={cn("w-4 h-4", loading && "animate-spin")} />
            </button>
          </div>
        </div>

        {/* Summary badges */}
        {summary && (
          <div className="flex items-center gap-2 flex-wrap">
            <SeverityBadge severity="critical" count={summary.criticalCount || 0} />
            <SeverityBadge severity="error" count={summary.errorCount || 0} />
            <SeverityBadge severity="warning" count={summary.warningCount || 0} />
            <span className="text-xs text-muted-foreground ml-1">
              Errors resolved by workflows are cleared automatically. Only errors the model could
              not resolve should appear here.
            </span>
          </div>
        )}
      </div>

      {/* Per-run scope indicator */}
      {taskRunId && (
        <div className="px-4 py-2 border-b border-border bg-blue-500/10 flex items-center justify-between">
          <div className="flex items-center gap-2 text-sm">
            <Info className="w-4 h-4 text-blue-500" />
            <span className="text-blue-400">
              Showing errors from: <span className="font-medium">{taskRunName || taskRunId}</span>
            </span>
          </div>
          {onClearScope && (
            <button
              onClick={onClearScope}
              className="text-xs text-blue-400 hover:text-blue-300 flex items-center gap-1"
            >
              <X className="w-3 h-3" />
              Show all errors
            </button>
          )}
        </div>
      )}

      {/* Search and filters */}
      <div className="px-4 py-2 border-b border-border bg-muted/30">
        <div className="flex items-center gap-2">
          {/* Search */}
          <div className="flex-1 flex items-center gap-2 bg-background border border-border rounded px-3 py-1.5">
            <Search className="w-4 h-4 text-muted-foreground" />
            <input
              type="text"
              value={searchText}
              onChange={(e) => setSearchText(e.target.value)}
              placeholder="Search errors..."
              className="flex-1 bg-transparent text-sm focus:outline-hidden"
            />
            {searchText && (
              <button
                onClick={() => setSearchText("")}
                className="text-muted-foreground hover:text-foreground"
              >
                <X className="w-4 h-4" />
              </button>
            )}
          </div>

          {/* Filter toggle */}
          <button
            onClick={() => setShowFilters(!showFilters)}
            className={cn(
              "flex items-center gap-1.5 px-3 py-1.5 rounded transition-colors",
              showFilters || hasActiveFilters
                ? "bg-primary/20 text-primary"
                : "bg-background border border-border hover:bg-muted",
            )}
          >
            <Filter className="w-4 h-4" />
            Filters
            {hasActiveFilters && (
              <span className="ml-1 w-5 h-5 text-xs bg-primary text-primary-foreground rounded-full flex items-center justify-center">
                {selectedSeverities.length + selectedStatuses.length}
              </span>
            )}
          </button>

          {hasActiveFilters && (
            <button
              onClick={clearFilters}
              className="text-xs text-muted-foreground hover:text-foreground"
            >
              Clear all
            </button>
          )}
        </div>

        {/* Filter options */}
        {showFilters && (
          <div className="mt-3 pt-3 border-t border-border space-y-3">
            {/* Severity filters */}
            <div>
              <span className="text-xs font-medium text-muted-foreground">Severity</span>
              <div className="flex gap-2 mt-1 flex-wrap">
                {severityOptions.map((severity) => (
                  <button
                    key={severity}
                    onClick={() => toggleSeverity(severity)}
                    className={cn(
                      "px-2 py-1 text-xs rounded border transition-colors capitalize",
                      selectedSeverities.includes(severity)
                        ? "bg-primary text-primary-foreground border-primary"
                        : "bg-background border-border hover:bg-muted",
                    )}
                  >
                    {severity}
                  </button>
                ))}
              </div>
            </div>

            {/* Status filters */}
            <div>
              <span className="text-xs font-medium text-muted-foreground">Status</span>
              <div className="flex gap-2 mt-1 flex-wrap">
                {statusOptions.map((status) => (
                  <button
                    key={status}
                    onClick={() => toggleStatus(status)}
                    className={cn(
                      "px-2 py-1 text-xs rounded border transition-colors capitalize",
                      selectedStatuses.includes(status)
                        ? "bg-primary text-primary-foreground border-primary"
                        : "bg-background border-border hover:bg-muted",
                    )}
                  >
                    {status.replace("_", " ")}
                  </button>
                ))}
              </div>
            </div>
          </div>
        )}
      </div>

      {/* Browser errors from UI Bridge SDK */}
      <BrowserErrorsPanel defaultCollapsed={true} />

      {/* Detected patterns */}
      {patterns.length > 0 && (
        <div className="px-4 py-2 border-b border-border bg-muted/20">
          <div className="flex items-center justify-between">
            <span className="text-xs font-medium text-muted-foreground">
              Detected Patterns ({patterns.length})
            </span>
            {selectedPatternIds && (
              <button
                onClick={() => setSelectedPatternIds(null)}
                className="text-xs text-purple-400 hover:text-purple-300 flex items-center gap-1"
              >
                <X className="w-3 h-3" />
                Clear pattern filter
              </button>
            )}
          </div>
          <div className="mt-1 space-y-1">
            {patterns.map((p, i) => {
              const patternErrorIds = p.errorIds ?? [];
              const isSelected =
                selectedPatternIds !== null &&
                patternErrorIds.length > 0 &&
                patternErrorIds.every((id: number) => selectedPatternIds.has(id)) &&
                selectedPatternIds.size === patternErrorIds.length;

              return (
                <div
                  key={i}
                  role="button"
                  tabIndex={0}
                  onClick={() => {
                    if (isSelected) {
                      setSelectedPatternIds(null);
                    } else if (patternErrorIds.length > 0) {
                      setSelectedPatternIds(new Set(patternErrorIds));
                    }
                  }}
                  onKeyDown={(e) => {
                    if (e.key === "Enter" || e.key === " ") {
                      e.preventDefault();
                      if (isSelected) {
                        setSelectedPatternIds(null);
                      } else if (patternErrorIds.length > 0) {
                        setSelectedPatternIds(new Set(patternErrorIds));
                      }
                    }
                  }}
                  className={cn(
                    "text-xs flex items-center gap-2 text-muted-foreground px-2 py-1 rounded cursor-pointer transition-colors",
                    "hover:bg-muted/40",
                    isSelected && "ring-1 ring-purple-500 bg-purple-500/10",
                  )}
                >
                  <span className="px-1.5 py-0.5 bg-purple-500/20 text-purple-400 rounded">
                    {(p.patternType ?? "unknown").replace(/_/g, " ")}
                  </span>
                  <span className="flex-1">{p.name}</span>
                  <span className={cn("text-muted-foreground/60", isSelected && "text-purple-400")}>
                    {p.frequency ?? p.matchCount ?? 0} errors
                  </span>
                </div>
              );
            })}
          </div>
        </div>
      )}

      {/* Error list */}
      <ScrollArea className="flex-1">
        {fetchError ? (
          <div className="p-4 text-center text-red-500">
            <AlertCircle className="w-8 h-8 mx-auto mb-2" />
            <p>{fetchError}</p>
          </div>
        ) : loading && errors.length === 0 ? (
          <div className="p-4 text-center text-muted-foreground">
            <RefreshCw className="w-8 h-8 mx-auto mb-2 animate-spin" />
            <p>Loading errors...</p>
          </div>
        ) : filteredErrors.length === 0 ? (
          <EmptyState isScoped={!!taskRunId} />
        ) : (
          <div className="flex flex-col">
            {filteredErrors.map((error) => (
              <ErrorItem
                key={error.id}
                error={error}
                isExpanded={expandedErrorId === error.id}
                onToggleExpand={() =>
                  setExpandedErrorId(expandedErrorId === error.id ? null : error.id)
                }
                onAcknowledge={() => acknowledge(error.id)}
                onResolve={() => resolve(error.id)}
                onIgnore={() => ignore(error.id)}
              />
            ))}
          </div>
        )}
      </ScrollArea>

      {/* Footer */}
      <div className="px-4 py-2 border-t border-border bg-muted/30 flex items-center justify-between">
        <span
          data-content-role="metric"
          data-content-label="error count"
          className="text-xs text-muted-foreground"
        >
          {filteredErrors.length} error{filteredErrors.length !== 1 ? "s" : ""}
          {hasActiveFilters && ` (filtered from ${errors.length})`}
        </span>
        <span className="text-xs text-muted-foreground">Auto-refresh: 30s</span>
      </div>
    </div>
  );
}

export default ErrorMonitorTab;

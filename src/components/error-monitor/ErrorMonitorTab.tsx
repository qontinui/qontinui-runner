/**
 * ErrorMonitorTab.tsx
 *
 * Main tab component for the error monitoring system.
 * Shows a list of errors with filtering, status management, and fix workflow generation.
 */

import { useState, useMemo } from "react";
import {
  AlertCircle,
  AlertTriangle,
  Bug,
  CheckCircle,
  ChevronDown,
  Clock,
  Eye,
  Filter,
  FolderOpen,
  Info,
  RefreshCw,
  Search,
  Settings,
  X,
} from "lucide-react";
import { cn } from "../../lib/utils";
import { ScrollArea } from "../ui/ScrollArea";
import { getStatusColors } from "@/design-system";
import { useErrorEvents, useErrorSummary } from "../../hooks/useErrorMonitor";
import { formatErrorTime } from "../../services/error-monitor-service";
import type { StoredErrorEvent, ErrorSeverity, ErrorStatus } from "../../types/errorMonitor";
import { FixErrorsButton } from "./FixErrorsButton";
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
    <span className={cn("px-2 py-0.5 rounded-md text-xs font-medium border", colors[severity])}>
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
  };

  const config = statusConfig[status] || { label: status, className: "" };

  return (
    <span className={cn("px-2 py-0.5 rounded-md text-xs font-medium", config.className)}>
      {config.label}
    </span>
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
      <div className="flex items-start gap-3 px-4 py-3 cursor-pointer" onClick={onToggleExpand}>
        {/* Severity icon */}
        <div className="flex-shrink-0 mt-0.5">
          <SeverityIcon severity={error.severity} />
        </div>

        {/* Content */}
        <div className="flex-1 min-w-0">
          <div className="flex items-center gap-2 mb-1">
            <span className="text-sm font-medium truncate">
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

function EmptyState() {
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
      <h3 className="text-lg font-medium mb-2">No Errors Detected</h3>
      <p className="text-sm text-muted-foreground max-w-xs">
        Configure log sources to start monitoring your application for errors.
      </p>
    </div>
  );
}

// =============================================================================
// Main Component
// =============================================================================

export function ErrorMonitorTab() {
  const [searchText, setSearchText] = useState("");
  const [selectedSeverities, setSelectedSeverities] = useState<ErrorSeverity[]>([]);
  const [selectedStatuses, setSelectedStatuses] = useState<ErrorStatus[]>([]);
  const [showFilters, setShowFilters] = useState(false);
  const [expandedErrorId, setExpandedErrorId] = useState<number | null>(null);
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
    severities: selectedSeverities.length > 0 ? selectedSeverities : undefined,
    statuses: selectedStatuses.length > 0 ? selectedStatuses : undefined,
    refreshInterval: 30000, // Refresh every 30 seconds
  });

  const { summary } = useErrorSummary({ refreshInterval: 30000 });

  // Filter errors by search text
  const filteredErrors = useMemo(() => {
    if (!searchText) return errors;
    const search = searchText.toLowerCase();
    return errors.filter(
      (e) =>
        e.message.toLowerCase().includes(search) ||
        e.errorType?.toLowerCase().includes(search) ||
        e.logSourceName.toLowerCase().includes(search),
    );
  }, [errors, searchText]);

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
  };

  const hasActiveFilters =
    selectedSeverities.length > 0 || selectedStatuses.length > 0 || searchText.length > 0;

  return (
    <div className="flex flex-col h-full">
      {/* Header */}
      <div className="px-4 py-3 border-b border-border bg-surface-raised">
        <div className="flex items-center justify-between mb-3">
          <div className="flex items-center gap-2">
            <Bug className="w-5 h-5 text-primary" />
            <h2 className="text-lg font-semibold">Error Monitor</h2>
            {summary && summary.unresolvedCount > 0 && (
              <span className="px-2 py-0.5 text-xs bg-red-500/20 text-red-500 rounded-full">
                {summary.unresolvedCount} unresolved
              </span>
            )}
          </div>
          <div className="flex items-center gap-2">
            <FixErrorsButton />
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
          </div>
        )}
      </div>

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
              className="flex-1 bg-transparent text-sm focus:outline-none"
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
          <EmptyState />
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
        <span className="text-xs text-muted-foreground">
          {filteredErrors.length} error{filteredErrors.length !== 1 ? "s" : ""}
          {hasActiveFilters && ` (filtered from ${errors.length})`}
        </span>
        <span className="text-xs text-muted-foreground">Auto-refresh: 30s</span>
      </div>
    </div>
  );
}

export default ErrorMonitorTab;

/**
 * ExplorationHistory.tsx
 *
 * Displays history of past exploration runs with summary information.
 */

import { useCallback } from "react";
import {
  History,
  CheckCircle,
  XCircle,
  AlertCircle,
  Clock,
  RefreshCw,
  Trash2,
  Loader2,
  FileSearch,
} from "lucide-react";
import { getStatusColors } from "@/design-system";
import { useStateExplorer } from "../../hooks/useStateExplorer";
import type { ExplorationHistoryItem } from "../../types/state-explorer";

interface ExplorationHistoryProps {
  /** Callback when a run is selected */
  onSelectRun: (runId: string) => void;
}

export function ExplorationHistory({ onSelectRun }: ExplorationHistoryProps) {
  const { history, historyLoading, refreshHistory, clearHistory, error } = useStateExplorer();

  const handleClearHistory = useCallback(async () => {
    if (window.confirm("Clear exploration reports older than 30 days?")) {
      const removed = await clearHistory(30);
      if (removed > 0) {
        console.log(`Removed ${removed} old exploration reports`);
      }
    }
  }, [clearHistory]);

  const formatDate = (dateStr?: string) => {
    if (!dateStr) return "Unknown";
    try {
      return new Date(dateStr).toLocaleString();
    } catch {
      return dateStr;
    }
  };

  const getStatusFromSummary = (summary?: string): "passed" | "failed" | "unknown" => {
    if (!summary) return "unknown";
    if (summary.includes("0 failed")) return "passed";
    if (summary.includes("failed")) return "failed";
    return "unknown";
  };

  const getStatusIcon = (item: ExplorationHistoryItem) => {
    const status = getStatusFromSummary(item.summary);
    switch (status) {
      case "passed":
        return <CheckCircle className={`w-5 h-5 ${getStatusColors("success").icon}`} />;
      case "failed":
        return <XCircle className={`w-5 h-5 ${getStatusColors("error").icon}`} />;
      default:
        return <AlertCircle className={`w-5 h-5 ${getStatusColors("warning").icon}`} />;
    }
  };

  const getStatusBadge = (item: ExplorationHistoryItem) => {
    const status = getStatusFromSummary(item.summary);
    switch (status) {
      case "passed": {
        const colors = getStatusColors("success");
        return (
          <span className={`px-2 py-0.5 text-xs rounded-full ${colors.bg} ${colors.text}`}>
            Passed
          </span>
        );
      }
      case "failed": {
        const colors = getStatusColors("error");
        return (
          <span className={`px-2 py-0.5 text-xs rounded-full ${colors.bg} ${colors.text}`}>
            Failed
          </span>
        );
      }
      default: {
        const colors = getStatusColors("warning");
        return (
          <span className={`px-2 py-0.5 text-xs rounded-full ${colors.bg} ${colors.text}`}>
            Unknown
          </span>
        );
      }
    }
  };

  return (
    <div className="h-full flex flex-col">
      {/* Header */}
      <div className="shrink-0 p-4 border-b border-border">
        <div className="flex items-center justify-between">
          <div className="flex items-center gap-3">
            <History className="w-5 h-5 text-primary" />
            <h2 className="text-lg font-semibold">Exploration History</h2>
            <span className="text-sm text-muted-foreground">({history.length} runs)</span>
          </div>
          <div className="flex items-center gap-2">
            <button
              onClick={handleClearHistory}
              disabled={historyLoading || history.length === 0}
              className="flex items-center gap-1.5 px-2.5 py-1.5 text-sm text-muted-foreground hover:text-foreground disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
              title="Clear old reports"
            >
              <Trash2 className="w-4 h-4" />
              Clear Old
            </button>
            <button
              onClick={refreshHistory}
              disabled={historyLoading}
              className="flex items-center gap-1.5 px-2.5 py-1.5 text-sm bg-muted hover:bg-muted/80 rounded-lg transition-colors"
              title="Refresh history"
            >
              <RefreshCw className={`w-4 h-4 ${historyLoading ? "animate-spin" : ""}`} />
              Refresh
            </button>
          </div>
        </div>
      </div>

      {/* Error Display */}
      {error && (
        <div className="mx-4 mt-4 p-3 bg-destructive/10 text-destructive rounded-lg text-sm flex items-center gap-2">
          <AlertCircle className="w-4 h-4 shrink-0" />
          <span>{error}</span>
        </div>
      )}

      {/* History List */}
      <div className="flex-1 overflow-auto p-4">
        {historyLoading && history.length === 0 ? (
          <div className="flex flex-col items-center justify-center h-full text-muted-foreground">
            <Loader2 className="w-8 h-8 mb-3 animate-spin" />
            <p>Loading history...</p>
          </div>
        ) : history.length === 0 ? (
          <div className="flex flex-col items-center justify-center h-full text-muted-foreground">
            <FileSearch className="w-12 h-12 mb-4 opacity-50" />
            <p className="text-lg font-medium">No Exploration Runs</p>
            <p className="text-sm mt-2">Run an exploration task to see history here</p>
          </div>
        ) : (
          <div className="space-y-3">
            {history.map((item) => (
              <button
                key={item.run_id}
                onClick={() => onSelectRun(item.run_id)}
                className="w-full text-left p-4 bg-card border border-border rounded-lg hover:border-primary/50 hover:bg-muted/30 transition-colors"
              >
                <div className="flex items-start gap-3">
                  {/* Status Icon */}
                  <div className="shrink-0 mt-0.5">{getStatusIcon(item)}</div>

                  {/* Content */}
                  <div className="flex-1 min-w-0">
                    {/* Top Row */}
                    <div className="flex items-center justify-between gap-3">
                      <div className="flex items-center gap-2">
                        <span className="font-medium truncate">
                          {item.config_name || "Unknown Config"}
                        </span>
                        {getStatusBadge(item)}
                      </div>
                      <span className="text-xs text-muted-foreground flex items-center gap-1 shrink-0">
                        <Clock className="w-3 h-3" />
                        {formatDate(item.started_at)}
                      </span>
                    </div>

                    {/* Strategy */}
                    {item.strategy && (
                      <div className="mt-1 text-sm text-muted-foreground">
                        Strategy: {item.strategy}
                      </div>
                    )}

                    {/* Summary */}
                    {item.summary && (
                      <div className="mt-2 text-sm text-foreground/80 truncate">{item.summary}</div>
                    )}

                    {/* Run ID */}
                    <div className="mt-2 text-xs text-muted-foreground font-mono">
                      ID: {item.run_id}
                    </div>
                  </div>
                </div>
              </button>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}

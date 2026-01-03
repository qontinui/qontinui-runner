/**
 * GeneralLogsTab.tsx
 *
 * Standalone tab for general application logs.
 * This is the simplified version that only shows general runner logs,
 * not run-specific logs like Image Recognition or Actions.
 */

import { useRef } from "react";
import { FileText, Filter, Trash2, Copy, Check } from "lucide-react";
import { GeneralLogTab } from "./GeneralLogTab";
import { useAutoScroll } from "../hooks";
import type { LogEntry } from "../managers/LogManager";
import type { LogLevel } from "../hooks/useLogFilter";

interface GeneralLogsTabProps {
  /** All log entries */
  logs: LogEntry[];
  /** Filtered log entries (based on log level) */
  filteredLogs: LogEntry[];
  /** Current log level filter */
  logLevel: LogLevel;
  /** Callback when log level changes */
  onLogLevelChange: (level: LogLevel) => void;
  /** Whether the log filter is shown */
  showLogFilter: boolean;
  /** Toggle log filter visibility */
  onToggleLogFilter: (show: boolean) => void;
  /** Log count for display */
  logCount: number;
  /** Clear general logs */
  onClearGeneralLogs: () => void;
  /** Copy logs to clipboard */
  onCopyLogs: () => void;
  /** Whether copy was successful (for feedback) */
  copySuccess: boolean;
}

const LOG_LEVELS: LogLevel[] = ["all", "info", "warning", "error", "debug"];

export function GeneralLogsTab({
  logs,
  filteredLogs,
  logLevel,
  onLogLevelChange,
  showLogFilter,
  onToggleLogFilter,
  logCount,
  onClearGeneralLogs,
  onCopyLogs,
  copySuccess,
}: GeneralLogsTabProps) {
  const logViewerRef = useRef<HTMLDivElement>(null);

  // Auto-scroll to bottom when new logs arrive
  useAutoScroll({
    enabled: true,
    containerRef: logViewerRef,
    dependencies: [logs],
  });

  return (
    <div className="h-full flex flex-col overflow-hidden">
      {/* Header */}
      <div className="flex-shrink-0 bg-background flex items-center justify-between border-b border-border p-3">
        <div className="flex items-center gap-2">
          <FileText className="w-4 h-4 text-muted-foreground" />
          <span className="text-sm font-medium">General Logs</span>
          {logCount > 0 && (
            <span className="px-1.5 py-0.5 text-xs rounded-full bg-muted text-muted-foreground">
              {logCount}
            </span>
          )}
        </div>

        {/* Actions */}
        <div className="flex items-center gap-2">
          {/* Log Level Filter */}
          <button
            onClick={() => onToggleLogFilter(!showLogFilter)}
            className={`p-1.5 rounded hover:bg-accent transition-colors ${
              showLogFilter ? "bg-accent text-accent-foreground" : "text-muted-foreground"
            }`}
            title="Toggle log filter"
          >
            <Filter className="w-4 h-4" />
          </button>

          {showLogFilter && (
            <select
              value={logLevel}
              onChange={(e) => onLogLevelChange(e.target.value as LogLevel)}
              className="text-xs bg-muted border border-border rounded px-2 py-1"
            >
              {LOG_LEVELS.map((level) => (
                <option key={level} value={level}>
                  {level.charAt(0).toUpperCase() + level.slice(1)}
                </option>
              ))}
            </select>
          )}

          {/* Copy */}
          <button
            onClick={onCopyLogs}
            className="p-1.5 rounded hover:bg-accent transition-colors text-muted-foreground"
            title="Copy logs to clipboard"
          >
            {copySuccess ? (
              <Check className="w-4 h-4 text-green-500" />
            ) : (
              <Copy className="w-4 h-4" />
            )}
          </button>

          {/* Clear */}
          <button
            onClick={onClearGeneralLogs}
            className="p-1.5 rounded hover:bg-accent transition-colors text-muted-foreground"
            title="Clear logs"
          >
            <Trash2 className="w-4 h-4" />
          </button>
        </div>
      </div>

      {/* Log Content */}
      <div className="flex-1 min-h-0 overflow-hidden p-4">
        <GeneralLogTab logs={filteredLogs} containerRef={logViewerRef} />
      </div>
    </div>
  );
}

export default GeneralLogsTab;

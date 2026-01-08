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
    <div className="h-full flex flex-col overflow-hidden p-4">
      <div className="flex-1 min-h-0 flex flex-col rounded-lg bg-card/50">
        {/* Header */}
        <div className="flex-shrink-0 flex items-center justify-between px-3 py-2">
          <div className="flex items-center gap-2">
            <FileText className="w-4 h-4 text-blue-400" />
            <span className="text-sm font-medium">General Logs</span>
            {logCount > 0 && <span className="badge badge-muted">{logCount}</span>}
          </div>

          {/* Actions */}
          <div className="flex items-center gap-1">
            {/* Log Level Filter */}
            <button
              onClick={() => onToggleLogFilter(!showLogFilter)}
              className={`p-1.5 rounded-md transition-colors ${
                showLogFilter
                  ? "bg-blue-500/20 text-blue-400"
                  : "text-muted-foreground hover:bg-muted/50"
              }`}
              title="Toggle log filter"
            >
              <Filter className="w-3.5 h-3.5" />
            </button>

            {showLogFilter && (
              <select
                value={logLevel}
                onChange={(e) => onLogLevelChange(e.target.value as LogLevel)}
                className="text-xs bg-muted/50 rounded-md px-2 py-1 border-0 outline-none focus:ring-1 focus:ring-blue-500/50"
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
              className="p-1.5 rounded-md hover:bg-muted/50 transition-colors text-muted-foreground"
              title="Copy logs to clipboard"
            >
              {copySuccess ? (
                <Check className="w-3.5 h-3.5 text-green-500" />
              ) : (
                <Copy className="w-3.5 h-3.5" />
              )}
            </button>

            {/* Clear */}
            <button
              onClick={onClearGeneralLogs}
              className="p-1.5 rounded-md hover:bg-muted/50 transition-colors text-muted-foreground"
              title="Clear logs"
            >
              <Trash2 className="w-3.5 h-3.5" />
            </button>
          </div>
        </div>

        {/* Log Content */}
        <div className="flex-1 min-h-0 overflow-hidden px-3 pb-3">
          <GeneralLogTab logs={filteredLogs} containerRef={logViewerRef} />
        </div>
      </div>
    </div>
  );
}

export default GeneralLogsTab;

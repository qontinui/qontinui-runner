import { useState } from "react";
import type { TriggerHistoryEntry } from "../../types/triggers";
import { getActionColor } from "../../types/triggers";
import type { HistoryFilters } from "../../hooks/useTriggers";

const ACTION_OPTIONS = [
  { value: "", label: "All Actions" },
  { value: "executed", label: "Executed" },
  { value: "debounced", label: "Debounced" },
  { value: "throttled", label: "Throttled" },
  { value: "condition_failed", label: "Condition Failed" },
  { value: "chain_depth_exceeded", label: "Chain Depth Exceeded" },
  { value: "disabled", label: "Disabled" },
  { value: "error", label: "Error" },
];

interface TriggerHistoryProps {
  entries: TriggerHistoryEntry[];
  triggerName?: string;
  triggerId?: string;
  onFilterChange?: (filters: HistoryFilters) => void;
  onNavigateToRun?: (taskRunId: string) => void;
}

export function TriggerHistory({
  entries,
  triggerName,
  triggerId,
  onFilterChange,
  onNavigateToRun,
}: TriggerHistoryProps) {
  const [actionFilter, setActionFilter] = useState("");

  const handleActionFilter = (value: string) => {
    setActionFilter(value);
    onFilterChange?.({ action: value || undefined });
  };

  if (!triggerId) {
    return (
      <div className="flex flex-col items-center justify-center h-full text-gray-400">
        <p className="text-lg">No trigger selected</p>
        <p className="text-sm mt-1">Select a trigger to view its history</p>
      </div>
    );
  }

  return (
    <div className="flex flex-col h-full">
      {/* Filters */}
      <div className="px-4 py-2 border-b border-gray-700 flex items-center gap-3">
        <span className="text-xs text-gray-400">Filter:</span>
        <select
          className="px-2 py-1 text-xs bg-gray-800 border border-gray-600 rounded text-white"
          value={actionFilter}
          onChange={(e) => handleActionFilter(e.target.value)}
        >
          {ACTION_OPTIONS.map((opt) => (
            <option key={opt.value} value={opt.value}>
              {opt.label}
            </option>
          ))}
        </select>
        <span className="text-xs text-gray-500 ml-auto">
          {entries.length} {entries.length === 1 ? "entry" : "entries"}
          {triggerName && ` for "${triggerName}"`}
        </span>
      </div>

      {/* Entries */}
      <div className="flex-1 overflow-auto">
        {entries.length === 0 ? (
          <div className="flex flex-col items-center justify-center h-full text-gray-400">
            <p className="text-lg">No history entries</p>
            <p className="text-sm mt-1">
              {actionFilter
                ? `No "${actionFilter}" entries found`
                : `No recorded events for "${triggerName}"`}
            </p>
          </div>
        ) : (
          <div className="divide-y divide-gray-700">
            {entries.map((entry) => (
              <div key={entry.id} className="px-4 py-3 hover:bg-gray-800/50">
                <div className="flex items-center justify-between">
                  <div className="flex-1 min-w-0">
                    <div className="flex items-center gap-2">
                      <span className={`text-sm font-medium ${getActionColor(entry.action)}`}>
                        {entry.action}
                      </span>
                      <span className="px-2 py-0.5 text-xs rounded bg-gray-700 text-gray-300">
                        {entry.event_type}
                      </span>
                    </div>
                    {entry.error_message && (
                      <p className="text-xs text-red-400 mt-0.5 truncate">{entry.error_message}</p>
                    )}
                    {entry.task_run_id && (
                      <p className="text-xs text-gray-500 mt-0.5">
                        Run:{" "}
                        {onNavigateToRun ? (
                          <button
                            onClick={() => onNavigateToRun(entry.task_run_id!)}
                            className="text-blue-400 hover:text-blue-300 hover:underline font-mono"
                          >
                            {entry.task_run_id.slice(0, 16)}...
                          </button>
                        ) : (
                          <span
                            className="font-mono text-gray-400 cursor-pointer hover:text-white"
                            title={entry.task_run_id}
                            role="button"
                            tabIndex={0}
                            onClick={() => {
                              navigator.clipboard.writeText(entry.task_run_id!).catch(() => {});
                            }}
                            onKeyDown={(e) => {
                              if (e.key === "Enter" || e.key === " ") {
                                navigator.clipboard.writeText(entry.task_run_id!).catch(() => {});
                              }
                            }}
                          >
                            {entry.task_run_id.slice(0, 16)}...
                          </span>
                        )}
                      </p>
                    )}
                  </div>
                  <div className="text-xs text-gray-500 ml-3 whitespace-nowrap">
                    {new Date(entry.triggered_at).toLocaleString()}
                  </div>
                </div>
                {Object.keys(entry.event_data).length > 0 && (
                  <details className="mt-1">
                    <summary className="text-xs text-gray-500 cursor-pointer hover:text-gray-300">
                      Event data
                    </summary>
                    <pre className="mt-1 text-xs text-gray-400 bg-gray-900 rounded p-2 overflow-auto max-h-32">
                      {JSON.stringify(entry.event_data, null, 2)}
                    </pre>
                  </details>
                )}
              </div>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}

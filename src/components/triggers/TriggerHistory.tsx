import type { TriggerHistoryEntry } from "../../types/triggers";
import { getActionColor } from "../../types/triggers";

interface TriggerHistoryProps {
  entries: TriggerHistoryEntry[];
  triggerName?: string;
}

export function TriggerHistory({ entries, triggerName }: TriggerHistoryProps) {
  if (entries.length === 0) {
    return (
      <div className="flex flex-col items-center justify-center h-full text-gray-400">
        <p className="text-lg">No history entries</p>
        <p className="text-sm mt-1">
          {triggerName
            ? `No recorded events for "${triggerName}"`
            : "Select a trigger to view its history"}
        </p>
      </div>
    );
  }

  return (
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
                  Run: {entry.task_run_id.slice(0, 8)}...
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
  );
}

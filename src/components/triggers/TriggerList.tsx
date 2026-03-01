import type { WorkflowTrigger } from "../../types/triggers";
import { getTriggerTypeLabel } from "../../types/triggers";

interface TriggerListProps {
  triggers: WorkflowTrigger[];
  onEdit: (trigger: WorkflowTrigger) => void;
  onDelete: (id: string) => void;
  onToggle: (id: string, enabled: boolean) => void;
  onTest: (id: string) => void;
  onViewHistory: (trigger: WorkflowTrigger) => void;
}

export function TriggerList({
  triggers,
  onEdit,
  onDelete,
  onToggle,
  onTest,
  onViewHistory,
}: TriggerListProps) {
  if (triggers.length === 0) {
    return (
      <div className="flex flex-col items-center justify-center h-full text-gray-400">
        <p className="text-lg">No triggers configured</p>
        <p className="text-sm mt-1">Create a trigger to automate workflow execution</p>
      </div>
    );
  }

  return (
    <div className="divide-y divide-gray-700">
      {triggers.map((trigger) => (
        <div
          key={trigger.id}
          className={`px-4 py-3 hover:bg-gray-800/50 ${!trigger.enabled ? "opacity-50" : ""}`}
        >
          <div className="flex items-center justify-between">
            {/* Left: Info */}
            <div className="flex-1 min-w-0">
              <div className="flex items-center gap-2">
                <span className="text-sm font-medium text-white truncate">{trigger.name}</span>
                <span className="px-2 py-0.5 text-xs rounded bg-gray-700 text-gray-300">
                  {getTriggerTypeLabel(trigger.trigger_type)}
                </span>
                {!trigger.enabled && (
                  <span className="px-2 py-0.5 text-xs rounded bg-gray-600 text-gray-400">
                    Disabled
                  </span>
                )}
              </div>
              {trigger.description && (
                <p className="text-xs text-gray-400 mt-0.5 truncate">{trigger.description}</p>
              )}
              <div className="flex items-center gap-3 mt-1 text-xs text-gray-500">
                <span>Fired {trigger.trigger_count}x</span>
                {trigger.last_triggered_at && (
                  <span>Last: {new Date(trigger.last_triggered_at).toLocaleString()}</span>
                )}
                <span>
                  Debounce: {trigger.debounce_ms}ms | Cooldown: {trigger.cooldown_seconds}s | Max:{" "}
                  {trigger.max_concurrent}
                </span>
              </div>
            </div>

            {/* Right: Actions */}
            <div className="flex items-center gap-1 ml-3">
              <button
                onClick={() => onToggle(trigger.id, !trigger.enabled)}
                className={`px-2 py-1 text-xs rounded ${
                  trigger.enabled
                    ? "bg-green-600/20 text-green-400 hover:bg-green-600/30"
                    : "bg-gray-600/20 text-gray-400 hover:bg-gray-600/30"
                }`}
                title={trigger.enabled ? "Disable" : "Enable"}
              >
                {trigger.enabled ? "ON" : "OFF"}
              </button>
              <button
                onClick={() => onTest(trigger.id)}
                className="px-2 py-1 text-xs text-gray-300 bg-gray-700 rounded hover:bg-gray-600"
                title="Test (dry run)"
              >
                Test
              </button>
              <button
                onClick={() => onViewHistory(trigger)}
                className="px-2 py-1 text-xs text-gray-300 bg-gray-700 rounded hover:bg-gray-600"
                title="View history"
              >
                History
              </button>
              <button
                onClick={() => onEdit(trigger)}
                className="px-2 py-1 text-xs text-gray-300 bg-gray-700 rounded hover:bg-gray-600"
                title="Edit"
              >
                Edit
              </button>
              <button
                onClick={() => onDelete(trigger.id)}
                className="px-2 py-1 text-xs text-red-400 bg-gray-700 rounded hover:bg-red-900/30"
                title="Delete"
              >
                Del
              </button>
            </div>
          </div>
        </div>
      ))}
    </div>
  );
}

import { useState } from "react";
import type { WorkflowTrigger } from "../../types/triggers";
import { getTriggerTypeLabel } from "../../types/triggers";
import { getApiBase } from "@/lib/runner-api";

interface TriggerListProps {
  triggers: WorkflowTrigger[];
  onEdit: (trigger: WorkflowTrigger) => void;
  onDelete: (id: string) => void;
  onToggle: (id: string, enabled: boolean) => void;
  onTest: (id: string) => void;
  onViewHistory: (trigger: WorkflowTrigger) => void;
  operationLoading: Record<string, boolean>;
}

export function TriggerList({
  triggers,
  onEdit,
  onDelete,
  onToggle,
  onTest,
  onViewHistory,
  operationLoading,
}: TriggerListProps) {
  const [copiedId, setCopiedId] = useState<string | null>(null);

  if (triggers.length === 0) {
    return (
      <div className="flex flex-col items-center justify-center h-full text-gray-400">
        <p className="text-lg">No triggers configured</p>
        <p className="text-sm mt-1">Create a trigger to automate workflow execution</p>
      </div>
    );
  }

  const copyWebhookUrl = async (triggerId: string) => {
    const url = `${getApiBase()}/triggers/webhook/${triggerId}`;
    try {
      await navigator.clipboard.writeText(url);
      setCopiedId(triggerId);
      setTimeout(() => setCopiedId(null), 2000);
    } catch {
      // Fallback for non-HTTPS contexts
      const textarea = document.createElement("textarea");
      textarea.value = url;
      document.body.appendChild(textarea);
      textarea.select();
      document.execCommand("copy");
      document.body.removeChild(textarea);
      setCopiedId(triggerId);
      setTimeout(() => setCopiedId(null), 2000);
    }
  };

  return (
    <div className="divide-y divide-gray-700">
      {triggers.map((trigger) => {
        const isToggling = operationLoading[`toggle-${trigger.id}`];
        const isTesting = operationLoading[`test-${trigger.id}`];
        const isDeleting = operationLoading[`delete-${trigger.id}`];

        return (
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
                {/* Webhook URL display */}
                {trigger.trigger_type === "webhook" && (
                  <div className="flex items-center gap-1.5 mt-1">
                    <code className="text-[10px] text-gray-500 font-mono truncate max-w-[280px]">
                      {`${getApiBase()}/triggers/webhook/${trigger.id}`}
                    </code>
                    <button
                      onClick={() => copyWebhookUrl(trigger.id)}
                      className="px-1.5 py-0.5 text-[10px] text-gray-400 bg-gray-700/50 rounded hover:bg-gray-600 hover:text-white shrink-0"
                      title="Copy webhook URL"
                    >
                      {copiedId === trigger.id ? "Copied!" : "Copy"}
                    </button>
                  </div>
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
                  disabled={isToggling}
                  className={`px-2 py-1 text-xs rounded ${
                    trigger.enabled
                      ? "bg-green-600/20 text-green-400 hover:bg-green-600/30"
                      : "bg-gray-600/20 text-gray-400 hover:bg-gray-600/30"
                  } disabled:opacity-50`}
                  title={trigger.enabled ? "Disable" : "Enable"}
                >
                  {isToggling ? "..." : trigger.enabled ? "ON" : "OFF"}
                </button>
                <button
                  onClick={() => onTest(trigger.id)}
                  disabled={isTesting}
                  className="px-2 py-1 text-xs text-gray-300 bg-gray-700 rounded hover:bg-gray-600 disabled:opacity-50"
                  title="Test (dry run)"
                >
                  {isTesting ? "..." : "Test"}
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
                  disabled={isDeleting}
                  className="px-2 py-1 text-xs text-red-400 bg-gray-700 rounded hover:bg-red-900/30 disabled:opacity-50"
                  title="Delete"
                >
                  {isDeleting ? "..." : "Del"}
                </button>
              </div>
            </div>
          </div>
        );
      })}
    </div>
  );
}

import { useState } from "react";
import { useTriggers, type TriggerToast } from "../../hooks/useTriggers";
import { TriggerList } from "./TriggerList";
import { TriggerEditor } from "./TriggerEditor";
import { TriggerHistory } from "./TriggerHistory";
import type {
  WorkflowTrigger,
  CreateTriggerRequest,
  UpdateTriggerRequest,
} from "../../types/triggers";

type SubTab = "list" | "history";

interface TriggersTabProps {
  className?: string;
}

/** Toast notification bar */
function ToastBar({
  toasts,
  onDismiss,
}: {
  toasts: TriggerToast[];
  onDismiss: (id: string) => void;
}) {
  if (toasts.length === 0) return null;

  return (
    <div className="fixed bottom-4 right-4 z-50 space-y-2 max-w-sm">
      {toasts.map((toast) => (
        <div
          key={toast.id}
          className={`flex items-center gap-2 px-4 py-2.5 rounded-lg shadow-lg text-sm ${
            toast.type === "success"
              ? "bg-green-900/90 text-green-200 border border-green-700"
              : toast.type === "error"
                ? "bg-red-900/90 text-red-200 border border-red-700"
                : "bg-blue-900/90 text-blue-200 border border-blue-700"
          }`}
        >
          <span className="flex-1">{toast.message}</span>
          <button
            onClick={() => onDismiss(toast.id)}
            className="text-current opacity-60 hover:opacity-100 text-xs ml-2"
          >
            x
          </button>
        </div>
      ))}
    </div>
  );
}

/** Test result modal */
function TestResultModal({
  result,
  onClose,
}: {
  result: Record<string, unknown>;
  onClose: () => void;
}) {
  const wouldExecute = result.would_execute as boolean;
  const action = result.action as string;
  const triggerName = result.trigger_name as string;
  const workflowId = result.workflow_id as string;

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60">
      <div className="bg-gray-800 border border-gray-600 rounded-lg shadow-xl w-96 max-w-[90vw]">
        <div className="flex items-center justify-between px-4 py-3 border-b border-gray-700">
          <h3 className="text-sm font-semibold text-white">Test Result</h3>
          <button onClick={onClose} className="text-gray-400 hover:text-white text-sm">
            x
          </button>
        </div>
        <div className="p-4 space-y-3">
          <div className="flex items-center gap-3">
            <span
              className={`inline-block w-3 h-3 rounded-full ${
                wouldExecute ? "bg-green-400" : "bg-yellow-400"
              }`}
            />
            <span className="text-sm text-white font-medium">
              {wouldExecute ? "Would execute" : "Would NOT execute"}
            </span>
          </div>
          <div className="space-y-1.5 text-xs">
            <div className="flex justify-between">
              <span className="text-gray-400">Trigger</span>
              <span className="text-white">{triggerName}</span>
            </div>
            <div className="flex justify-between">
              <span className="text-gray-400">Action</span>
              <span
                className={`${
                  action === "executed"
                    ? "text-green-400"
                    : action === "disabled"
                      ? "text-gray-400"
                      : "text-yellow-400"
                }`}
              >
                {action}
              </span>
            </div>
            <div className="flex justify-between">
              <span className="text-gray-400">Workflow</span>
              <span className="text-white font-mono text-[10px]">{workflowId}</span>
            </div>
          </div>
        </div>
        <div className="px-4 py-3 border-t border-gray-700 flex justify-end">
          <button
            onClick={onClose}
            className="px-3 py-1.5 text-xs text-white bg-gray-700 rounded hover:bg-gray-600"
          >
            Close
          </button>
        </div>
      </div>
    </div>
  );
}

export function TriggersTab({ className = "" }: TriggersTabProps) {
  const [activeSubTab, setActiveSubTab] = useState<SubTab>("list");
  const [editorOpen, setEditorOpen] = useState(false);
  const [editingTrigger, setEditingTrigger] = useState<WorkflowTrigger | null>(null);
  const [testResult, setTestResult] = useState<Record<string, unknown> | null>(null);

  const {
    triggers,
    selectedTrigger,
    triggerHistory,
    status,
    loading,
    toasts,
    operationLoading,
    createTrigger,
    updateTrigger,
    deleteTrigger,
    setEnabled,
    testTrigger,
    selectTrigger,
    loadHistory,
    refresh,
    dismissToast,
  } = useTriggers();

  const handleCreate = () => {
    setEditingTrigger(null);
    setEditorOpen(true);
  };

  const handleEdit = (trigger: WorkflowTrigger) => {
    setEditingTrigger(trigger);
    setEditorOpen(true);
  };

  const handleSave = async (data: CreateTriggerRequest | UpdateTriggerRequest) => {
    if (editingTrigger) {
      await updateTrigger(editingTrigger.id, data as UpdateTriggerRequest);
    } else {
      await createTrigger(data as CreateTriggerRequest);
    }
    setEditorOpen(false);
    setEditingTrigger(null);
  };

  const handleDelete = async (id: string) => {
    if (confirm("Delete this trigger? This action cannot be undone.")) {
      await deleteTrigger(id);
    }
  };

  const handleTest = async (id: string) => {
    const result = await testTrigger(id);
    if (result) {
      setTestResult(result);
    }
  };

  const handleViewHistory = (trigger: WorkflowTrigger) => {
    selectTrigger(trigger);
    setActiveSubTab("history");
  };

  if (editorOpen) {
    return (
      <div className={`flex flex-col h-full ${className}`}>
        <TriggerEditor
          trigger={editingTrigger}
          onSave={handleSave}
          onCancel={() => {
            setEditorOpen(false);
            setEditingTrigger(null);
          }}
        />
        <ToastBar toasts={toasts} onDismiss={dismissToast} />
      </div>
    );
  }

  return (
    <div className={`flex flex-col h-full ${className}`}>
      {/* Header */}
      <div className="flex items-center justify-between px-4 py-3 border-b border-gray-700">
        <div className="flex items-center gap-3">
          <h2 className="text-lg font-semibold text-white">Triggers</h2>
          {status && (
            <div className="flex items-center gap-2 text-xs text-gray-400">
              <span
                className={`inline-block w-2 h-2 rounded-full ${
                  status.running ? "bg-green-400" : "bg-red-400"
                }`}
              />
              <span>
                {status.enabled_triggers}/{status.total_triggers} enabled
              </span>
              {status.active_executions > 0 && (
                <span className="text-yellow-400">{status.active_executions} running</span>
              )}
              {status.dropped_events > 0 && (
                <span className="text-red-400" title="Events dropped due to channel overflow">
                  {status.dropped_events} dropped
                </span>
              )}
            </div>
          )}
        </div>
        <div className="flex items-center gap-2">
          <button
            onClick={refresh}
            className="px-3 py-1.5 text-xs text-gray-300 bg-gray-700 rounded hover:bg-gray-600"
            disabled={loading}
          >
            {loading ? "Loading..." : "Refresh"}
          </button>
          <button
            onClick={handleCreate}
            className="px-3 py-1.5 text-xs text-white bg-blue-600 rounded hover:bg-blue-500"
          >
            + New Trigger
          </button>
        </div>
      </div>

      {/* Sub-tabs */}
      <div className="flex border-b border-gray-700">
        <button
          className={`px-4 py-2 text-sm ${
            activeSubTab === "list"
              ? "text-white border-b-2 border-blue-500"
              : "text-gray-400 hover:text-gray-200"
          }`}
          onClick={() => setActiveSubTab("list")}
        >
          All Triggers ({triggers.length})
        </button>
        <button
          className={`px-4 py-2 text-sm ${
            activeSubTab === "history"
              ? "text-white border-b-2 border-blue-500"
              : "text-gray-400 hover:text-gray-200"
          }`}
          onClick={() => setActiveSubTab("history")}
        >
          History{selectedTrigger ? ` (${selectedTrigger.name})` : ""}
        </button>
      </div>

      {/* Content */}
      <div className="flex-1 overflow-auto">
        {activeSubTab === "list" ? (
          <TriggerList
            triggers={triggers}
            onEdit={handleEdit}
            onDelete={handleDelete}
            onToggle={setEnabled}
            onTest={handleTest}
            onViewHistory={handleViewHistory}
            operationLoading={operationLoading}
          />
        ) : (
          <TriggerHistory
            entries={triggerHistory}
            triggerName={selectedTrigger?.name}
            triggerId={selectedTrigger?.id}
            onFilterChange={(filters) => {
              if (selectedTrigger) {
                loadHistory(selectedTrigger.id, filters);
              }
            }}
          />
        )}
      </div>

      {/* Test result modal */}
      {testResult && <TestResultModal result={testResult} onClose={() => setTestResult(null)} />}

      {/* Toast notifications */}
      <ToastBar toasts={toasts} onDismiss={dismissToast} />
    </div>
  );
}

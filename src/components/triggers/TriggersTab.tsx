import { useState } from "react";
import { useTriggers } from "../../hooks/useTriggers";
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

export function TriggersTab({ className = "" }: TriggersTabProps) {
  const [activeSubTab, setActiveSubTab] = useState<SubTab>("list");
  const [editorOpen, setEditorOpen] = useState(false);
  const [editingTrigger, setEditingTrigger] = useState<WorkflowTrigger | null>(null);

  const {
    triggers,
    selectedTrigger,
    triggerHistory,
    status,
    loading,
    createTrigger,
    updateTrigger,
    deleteTrigger,
    setEnabled,
    testTrigger,
    selectTrigger,
    refresh,
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
      alert(`Test result:\n${JSON.stringify(result, null, 2)}`);
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
          />
        ) : (
          <TriggerHistory entries={triggerHistory} triggerName={selectedTrigger?.name} />
        )}
      </div>
    </div>
  );
}

/**
 * ConfigHeader — config selector dropdown with create/delete actions.
 */

import { useState, useCallback } from "react";
import { GitBranch, Plus, Trash2, Loader2 } from "lucide-react";
import type { StateMachineConfig, StateMachineConfigCreate } from "@qontinui/shared-types";

interface ConfigHeaderProps {
  configs: StateMachineConfig[];
  activeConfigId: string | null;
  onSelectConfig: (id: string) => void;
  onCreateConfig: (req: StateMachineConfigCreate) => Promise<StateMachineConfig>;
  onDeleteConfig: (id: string) => Promise<void>;
  isLoading: boolean;
}

export function ConfigHeader({
  configs,
  activeConfigId,
  onSelectConfig,
  onCreateConfig,
  onDeleteConfig,
  isLoading,
}: ConfigHeaderProps) {
  const [showNewInput, setShowNewInput] = useState(false);
  const [newName, setNewName] = useState("");
  const [isCreating, setIsCreating] = useState(false);

  const handleCreate = useCallback(async () => {
    if (!newName.trim()) return;
    setIsCreating(true);
    try {
      const config = await onCreateConfig({ name: newName.trim() });
      onSelectConfig(config.id);
      setNewName("");
      setShowNewInput(false);
    } finally {
      setIsCreating(false);
    }
  }, [newName, onCreateConfig, onSelectConfig]);

  const handleDelete = useCallback(async () => {
    if (!activeConfigId) return;
    const name = configs.find((c) => c.id === activeConfigId)?.name ?? "this config";
    if (!window.confirm(`Delete "${name}"? This cannot be undone.`)) return;
    await onDeleteConfig(activeConfigId);
  }, [activeConfigId, configs, onDeleteConfig]);

  return (
    <div className="flex items-center gap-3">
      <GitBranch className="w-5 h-5 text-text-secondary shrink-0" />
      <h1 className="text-xl font-bold text-text-primary shrink-0">State Machine</h1>

      {/* Config selector */}
      <div className="relative min-w-[180px]">
        <select
          value={activeConfigId ?? ""}
          onChange={(e) => {
            if (e.target.value) onSelectConfig(e.target.value);
          }}
          aria-label="Select config..."
          className={`w-full px-2 py-1.5 text-sm bg-bg-tertiary border border-border-secondary rounded appearance-none pr-6 ${activeConfigId ? "text-text-primary" : "text-transparent"}`}
        >
          <option value="">Select config...</option>
          {configs.map((c) => (
            <option key={c.id} value={c.id}>
              {c.name}
            </option>
          ))}
        </select>
        {!activeConfigId && (
          <span className="pointer-events-none absolute inset-0 flex items-center px-2 text-sm text-text-primary">
            Select config...
          </span>
        )}
      </div>

      {/* New config button / input */}
      {showNewInput ? (
        <div className="flex items-center gap-1.5">
          <input
            type="text"
            value={newName}
            onChange={(e) => setNewName(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") handleCreate();
              if (e.key === "Escape") setShowNewInput(false);
            }}
            placeholder="Config name..."
            aria-label="Config name..."
            autoFocus
            className="px-2 py-1.5 text-sm bg-bg-tertiary border border-border-secondary rounded text-text-primary w-40"
          />
          <button
            onClick={handleCreate}
            disabled={!newName.trim() || isCreating}
            className="px-2 py-1.5 text-sm font-medium text-white bg-brand-primary hover:bg-brand-primary/90 disabled:opacity-50 rounded"
          >
            {isCreating ? <Loader2 className="w-4 h-4 animate-spin" /> : "Create"}
          </button>
          <button
            onClick={() => setShowNewInput(false)}
            className="px-2 py-1.5 text-sm text-text-secondary hover:text-text-primary"
          >
            Cancel
          </button>
        </div>
      ) : (
        <button
          onClick={() => setShowNewInput(true)}
          className="flex items-center gap-1 px-2 py-1.5 text-sm text-text-secondary hover:text-text-primary border border-border-secondary hover:border-border-primary rounded"
        >
          <Plus className="w-3.5 h-3.5" />
          New
        </button>
      )}

      {/* Delete active config */}
      {activeConfigId && (
        <button
          onClick={handleDelete}
          className="p-1.5 text-text-muted hover:text-red-400 rounded"
          title="Delete config"
          aria-label="Delete config"
        >
          <Trash2 className="w-4 h-4" />
        </button>
      )}

      {isLoading && <Loader2 className="w-4 h-4 text-text-muted animate-spin ml-auto" />}
    </div>
  );
}

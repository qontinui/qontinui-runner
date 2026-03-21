/**
 * ConstraintsPanel.tsx
 *
 * Main panel for managing constraint engine configuration.
 * Rendered as a collapsible panel below the header, matching the
 * SettingsPanel pattern in WorkflowBuilderTab.
 *
 * Three sections:
 *   1. Built-in Constraints — toggleable, non-editable
 *   2. Custom Constraints — fully editable cards
 *   3. Resource Limits — collapsible number inputs
 */

import { useState } from "react";
import { X, Save, Loader2, AlertCircle, Plus, CheckCircle2 } from "lucide-react";
import type { Constraint } from "@qontinui/shared-types/constraints";
import { generateConstraintId } from "@qontinui/workflow-utils";
import { useConstraints } from "../../../hooks/useConstraints";
import { BuiltinConstraintRow } from "./BuiltinConstraintRow";
import { ConstraintCard } from "./ConstraintCard";
import { ResourceLimitsSection } from "./ResourceLimitsSection";

interface ConstraintsPanelProps {
  onClose: () => void;
}

export function ConstraintsPanel({ onClose }: ConstraintsPanelProps) {
  const {
    builtinConstraints,
    customConstraints,
    aiConstraints,
    resourceLimits,
    isDirty,
    loading,
    error,
    toggleBuiltin,
    addConstraint,
    updateConstraint,
    removeConstraint,
    promoteConstraint,
    updateResourceLimits,
    save,
  } = useConstraints();

  const [isSaving, setIsSaving] = useState(false);
  const [saveResult, setSaveResult] = useState<{ success: boolean; message?: string } | null>(null);

  const handleSave = async () => {
    setIsSaving(true);
    setSaveResult(null);
    try {
      const success = await save();
      if (success) {
        setSaveResult({ success: true, message: "Constraints saved" });
        // Clear the success message after a few seconds
        setTimeout(() => setSaveResult(null), 3000);
      }
      // If save() returns false, the hook sets the error state
    } catch {
      // Error is handled by the hook
    } finally {
      setIsSaving(false);
    }
  };

  const handleAddConstraint = () => {
    const name = "New constraint";
    const newConstraint: Constraint = {
      id: generateConstraintId(name + "-" + Date.now()),
      name,
      description: "",
      severity: "warn",
      enabled: true,
      check: {
        type: "grep_forbidden",
        pattern: "",
      },
    };
    addConstraint(newConstraint);
  };

  if (loading) {
    return (
      <div className="p-4 border-t border-zinc-700">
        <div className="flex items-center justify-center gap-2 py-8 text-zinc-400">
          <Loader2 className="w-4 h-4 animate-spin" />
          <span className="text-sm">Loading constraints...</span>
        </div>
      </div>
    );
  }

  return (
    <div className="p-4 border-t border-zinc-700 space-y-4 animate-slideDown">
      {/* Header */}
      <div className="flex items-center justify-between">
        <h2 className="text-sm font-semibold text-zinc-200">Constraints</h2>
        <div className="flex items-center gap-2">
          <button
            type="button"
            onClick={handleSave}
            disabled={!isDirty || isSaving}
            className="flex items-center gap-1.5 px-2.5 py-1 rounded-md bg-zinc-700 hover:bg-zinc-600 text-zinc-200 text-xs transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
            title="Save constraints"
          >
            {isSaving ? (
              <Loader2 className="w-3.5 h-3.5 animate-spin" />
            ) : (
              <Save className="w-3.5 h-3.5" />
            )}
            {isSaving ? "Saving..." : "Save"}
          </button>
          <button
            type="button"
            onClick={onClose}
            className="p-1 text-zinc-500 hover:text-zinc-300 rounded transition-colors"
            title="Close constraints panel"
          >
            <X className="w-4 h-4" />
          </button>
        </div>
      </div>

      {/* Error banner */}
      {error && (
        <div className="flex items-start gap-2 px-3 py-2 rounded-md bg-red-500/10 border border-red-500/20">
          <AlertCircle className="w-4 h-4 text-red-400 shrink-0 mt-0.5" />
          <span className="text-xs text-red-400">{error}</span>
        </div>
      )}

      {/* Success banner */}
      {saveResult?.success && (
        <div className="flex items-center gap-2 px-3 py-2 rounded-md bg-green-500/10 border border-green-500/20">
          <CheckCircle2 className="w-4 h-4 text-green-400 shrink-0" />
          <span className="text-xs text-green-400">{saveResult.message}</span>
        </div>
      )}

      {/* Built-in Constraints Section */}
      <div>
        <h3 className="text-xs font-medium text-zinc-400 mb-2 uppercase tracking-wider">
          Built-in Constraints
        </h3>
        <div className="space-y-1.5">
          {builtinConstraints.length === 0 ? (
            <p className="text-xs text-zinc-500 px-3 py-2">No built-in constraints loaded</p>
          ) : (
            builtinConstraints.map((c) => (
              <BuiltinConstraintRow
                key={c.id}
                constraint={c}
                onToggle={(enabled) => {
                  const suffix = c.id.replace(/^builtin:/, "");
                  toggleBuiltin(suffix, enabled);
                }}
              />
            ))
          )}
        </div>
      </div>

      {/* Custom Constraints Section */}
      <div>
        <div className="flex items-center justify-between mb-2">
          <h3 className="text-xs font-medium text-zinc-400 uppercase tracking-wider">
            Custom Constraints
          </h3>
          <button
            type="button"
            onClick={handleAddConstraint}
            className="flex items-center gap-1 text-xs text-blue-400 hover:text-blue-300 transition-colors"
          >
            <Plus className="w-3 h-3" />
            Add
          </button>
        </div>
        <div className="space-y-2">
          {customConstraints.length === 0 ? (
            <p className="text-xs text-zinc-500 px-3 py-2">
              No custom constraints. Click &quot;Add&quot; to create one.
            </p>
          ) : (
            customConstraints.map((c) => (
              <ConstraintCard
                key={c.id}
                constraint={c}
                onUpdate={(updates) => updateConstraint(c.id, updates)}
                onRemove={() => removeConstraint(c.id)}
              />
            ))
          )}
        </div>
      </div>

      {/* AI-Proposed Constraints Section */}
      {aiConstraints.length > 0 && (
        <div>
          <h3 className="text-xs font-medium text-purple-400 mb-2 uppercase tracking-wider">
            AI-Proposed Constraints
          </h3>
          <p className="text-[10px] text-zinc-500 mb-2">
            Proposed during the current run. Promote to keep them as project constraints.
          </p>
          <div className="space-y-2">
            {aiConstraints.map((c) => (
              <ConstraintCard
                key={c.id}
                constraint={c}
                onUpdate={() => {}}
                onRemove={() => {}}
                onPromote={() => promoteConstraint(c.id)}
              />
            ))}
          </div>
        </div>
      )}

      {/* Resource Limits Section */}
      <div className="border-t border-zinc-700/50 pt-3">
        <ResourceLimitsSection resourceLimits={resourceLimits} onUpdate={updateResourceLimits} />
      </div>
    </div>
  );
}

/**
 * NewWorkflowConfirmDialog.tsx
 *
 * Confirmation dialog shown when creating a new workflow with unsaved changes.
 */

import { AlertTriangle, Trash2, X } from "lucide-react";
import { useAiBuilder } from "./AiBuilderContext";

export function NewWorkflowConfirmDialog() {
  const {
    showNewConfirmDialog,
    setShowNewConfirmDialog,
    resetToNewWorkflow,
    executionSteps,
    goal,
  } = useAiBuilder();

  if (!showNewConfirmDialog) {
    return null;
  }

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center">
      {/* Backdrop */}
      <div
        className="absolute inset-0 bg-black/50"
        onClick={() => setShowNewConfirmDialog(false)}
      />

      {/* Dialog */}
      <div className="relative bg-card border border-border rounded-lg shadow-xl w-full max-w-md mx-4 p-6 space-y-4">
        <div className="flex items-center justify-between">
          <div className="flex items-center gap-2">
            <AlertTriangle className="w-5 h-5 text-yellow-500" />
            <h3 className="text-lg font-semibold">Unsaved Changes</h3>
          </div>
          <button
            onClick={() => setShowNewConfirmDialog(false)}
            className="p-1 text-muted-foreground hover:text-foreground transition-colors"
          >
            <X className="w-5 h-5" />
          </button>
        </div>

        <p className="text-sm text-muted-foreground">
          You have unsaved changes to your current workflow. Creating a new workflow will discard
          these changes.
        </p>

        <div className="text-xs text-muted-foreground bg-yellow-500/10 border border-yellow-500/20 p-3 rounded">
          <p>
            <strong>Current workflow includes:</strong>
          </p>
          <ul className="list-disc list-inside mt-1 space-y-0.5">
            <li>
              {executionSteps.length} execution step{executionSteps.length !== 1 ? "s" : ""}
            </li>
            {goal.trim() && (
              <li>
                Goal: {goal.trim().slice(0, 50)}
                {goal.trim().length > 50 ? "..." : ""}
              </li>
            )}
          </ul>
        </div>

        <div className="flex gap-3 pt-2">
          <button
            onClick={() => setShowNewConfirmDialog(false)}
            className="flex-1 px-4 py-2 bg-muted text-foreground rounded-md font-medium hover:bg-muted/80 transition-colors"
          >
            Cancel
          </button>
          <button
            onClick={resetToNewWorkflow}
            className="flex-1 flex items-center justify-center gap-2 px-4 py-2 bg-red-500 text-white rounded-md font-medium hover:bg-red-600 transition-colors"
          >
            <Trash2 className="w-4 h-4" />
            Discard & Create New
          </button>
        </div>
      </div>
    </div>
  );
}

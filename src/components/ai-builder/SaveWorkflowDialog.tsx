/**
 * SaveWorkflowDialog.tsx
 *
 * Modal dialog for saving a new workflow to the library.
 */

import { Loader2, Save, X } from "lucide-react";
import { useAiBuilder } from "./AiBuilderContext";
import { getAccentColors, getStatusColors } from "@/design-system";

export function SaveWorkflowDialog() {
  const {
    showSaveDialog,
    setShowSaveDialog,
    saveName,
    setSaveName,
    saveDescription,
    setSaveDescription,
    isSaving,
    saveAsNewWorkflow,
    executionSteps,
    goal,
    maxIterations,
  } = useAiBuilder();

  if (!showSaveDialog) {
    return null;
  }

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center">
      {/* Backdrop */}
      <div className="absolute inset-0 bg-black/50" onClick={() => setShowSaveDialog(false)} />

      {/* Dialog */}
      <div className="relative bg-card border border-border rounded-lg shadow-xl w-full max-w-md mx-4 p-6 space-y-4">
        <div className="flex items-center justify-between">
          <div className="flex items-center gap-2">
            <Save className={`w-5 h-5 ${getStatusColors("success").text}`} />
            <h3 className="text-lg font-semibold">Save Workflow</h3>
          </div>
          <button
            onClick={() => setShowSaveDialog(false)}
            className="p-1 text-muted-foreground hover:text-foreground transition-colors"
          >
            <X className="w-5 h-5" />
          </button>
        </div>

        <p className="text-sm text-muted-foreground">
          Save this workflow to the Prompt Library for easy reuse.
        </p>

        <div className="space-y-3">
          <div>
            <label className="block text-sm font-medium mb-1">Name</label>
            <input
              type="text"
              value={saveName}
              onChange={(e) => setSaveName(e.target.value)}
              placeholder="Enter workflow name..."
              className="w-full px-3 py-2 bg-background border border-border rounded-md text-sm focus:outline-none focus:ring-2 focus:ring-primary"
              autoFocus
            />
          </div>

          <div>
            <label className="block text-sm font-medium mb-1">Description (optional)</label>
            <textarea
              value={saveDescription}
              onChange={(e) => setSaveDescription(e.target.value)}
              placeholder="Describe what this workflow does..."
              className="w-full px-3 py-2 bg-background border border-border rounded-md text-sm resize-none h-20 focus:outline-none focus:ring-2 focus:ring-primary"
            />
          </div>

          <div className="text-xs text-muted-foreground bg-muted/30 p-2 rounded">
            <p>
              <strong>Includes:</strong>
            </p>
            <ul className="list-disc list-inside mt-1 space-y-0.5">
              <li>
                {executionSteps.length} execution step{executionSteps.length !== 1 ? "s" : ""}
              </li>
              <li>Goal: {goal.trim() || "(none set)"}</li>
              <li>Max iterations: {maxIterations}</li>
            </ul>
          </div>
        </div>

        <div className="flex gap-3 pt-2">
          <button
            onClick={() => setShowSaveDialog(false)}
            className="flex-1 px-4 py-2 bg-muted text-foreground rounded-md font-medium hover:bg-muted/80 transition-colors"
          >
            Cancel
          </button>
          <button
            onClick={saveAsNewWorkflow}
            disabled={isSaving || !saveName.trim()}
            className={`flex-1 flex items-center justify-center gap-2 px-4 py-2 ${getAccentColors("green").bgSolid} text-white rounded-md font-medium hover:bg-green-600 disabled:opacity-50 disabled:cursor-not-allowed transition-colors`}
          >
            {isSaving ? (
              <>
                <Loader2 className="w-4 h-4 animate-spin" />
                Saving...
              </>
            ) : (
              <>
                <Save className="w-4 h-4" />
                Save to Library
              </>
            )}
          </button>
        </div>
      </div>
    </div>
  );
}

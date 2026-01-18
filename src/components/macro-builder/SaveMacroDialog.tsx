/**
 * SaveMacroDialog Component
 *
 * Dialog for saving a new macro.
 */

import { Save, X, Loader2 } from "lucide-react";
import { useMacroBuilder } from "./MacroBuilderContext";

export function SaveMacroDialog() {
  const {
    showSaveDialog,
    setShowSaveDialog,
    formState,
    setFormState,
    isSaving,
    handleSaveAsNew,
    steps,
  } = useMacroBuilder();

  if (!showSaveDialog) return null;

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center">
      {/* Backdrop */}
      <div className="absolute inset-0 bg-black/50" onClick={() => setShowSaveDialog(false)} />

      {/* Dialog */}
      <div className="relative bg-card border border-border rounded-lg shadow-xl w-full max-w-md mx-4 p-6 space-y-4">
        <div className="flex items-center justify-between">
          <div className="flex items-center gap-2">
            <Save className="w-5 h-5 text-primary" />
            <h3 className="text-lg font-semibold">Save Macro</h3>
          </div>
          <button
            onClick={() => setShowSaveDialog(false)}
            className="p-1 text-muted-foreground hover:text-foreground transition-colors"
          >
            <X className="w-5 h-5" />
          </button>
        </div>

        <p className="text-sm text-muted-foreground">
          Save this macro to the library for easy reuse.
        </p>

        <div className="space-y-3">
          <div>
            <label className="block text-sm font-medium mb-1">Name</label>
            <input
              type="text"
              value={formState.name}
              onChange={(e) => setFormState((prev) => ({ ...prev, name: e.target.value }))}
              placeholder="Enter macro name..."
              className="w-full px-3 py-2 bg-background border border-border rounded-md text-sm focus:outline-none focus:ring-2 focus:ring-primary"
              autoFocus
            />
          </div>

          <div>
            <label className="block text-sm font-medium mb-1">Description (optional)</label>
            <textarea
              value={formState.description}
              onChange={(e) =>
                setFormState((prev) => ({
                  ...prev,
                  description: e.target.value,
                }))
              }
              placeholder="Describe what this macro does..."
              className="w-full px-3 py-2 bg-background border border-border rounded-md text-sm resize-none h-20 focus:outline-none focus:ring-2 focus:ring-primary"
            />
          </div>

          <div>
            <label className="block text-sm font-medium mb-1">Category</label>
            <input
              type="text"
              value={formState.category}
              onChange={(e) => setFormState((prev) => ({ ...prev, category: e.target.value }))}
              placeholder="e.g., general, testing"
              className="w-full px-3 py-2 bg-background border border-border rounded-md text-sm focus:outline-none focus:ring-2 focus:ring-primary"
            />
          </div>

          <div className="text-xs text-muted-foreground bg-muted/30 p-2 rounded">
            <p>
              <strong>Includes:</strong>
            </p>
            <ul className="list-disc list-inside mt-1 space-y-0.5">
              <li>
                {steps.length} step{steps.length !== 1 ? "s" : ""}
              </li>
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
            onClick={handleSaveAsNew}
            disabled={isSaving || !formState.name.trim()}
            className="flex-1 flex items-center justify-center gap-2 px-4 py-2 bg-primary text-primary-foreground rounded-md font-medium hover:bg-primary/90 disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
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

/**
 * SavedWorkflowsPanel.tsx
 *
 * Panel displaying saved AI workflows with load/delete functionality.
 */

import { BookOpen, ChevronDown, Sparkles, Trash2 } from "lucide-react";
import { useAiBuilder } from "./AiBuilderContext";
import { getStatusColors } from "@/design-system";

export function SavedWorkflowsPanel() {
  const {
    savedAiWorkflows,
    showWorkflowsPanel,
    setShowWorkflowsPanel,
    loadAiWorkflow,
    deleteAiWorkflow,
  } = useAiBuilder();

  return (
    <div className="card p-4 space-y-3">
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-2">
          <BookOpen className={`w-4 h-4 ${getStatusColors("success").text}`} />
          <span className="font-medium">Saved Workflows</span>
          <span className="text-xs text-muted-foreground">({savedAiWorkflows.length})</span>
        </div>
        <button
          onClick={() => setShowWorkflowsPanel(!showWorkflowsPanel)}
          className={`flex items-center gap-1 px-2 py-1 text-xs ${getStatusColors("success").bg} ${getStatusColors("success").text} rounded hover:bg-green-500/20 transition-colors`}
        >
          {showWorkflowsPanel ? "Hide" : "Show"}
          <ChevronDown
            className={`w-3 h-3 transition-transform ${showWorkflowsPanel ? "rotate-180" : ""}`}
          />
        </button>
      </div>

      {showWorkflowsPanel && (
        <div className="space-y-2 max-h-48 overflow-y-auto">
          {savedAiWorkflows.length === 0 ? (
            <p className="text-sm text-muted-foreground text-center py-4">
              No saved workflows yet. Use the Save button to save your current workflow.
            </p>
          ) : (
            savedAiWorkflows.map((workflow) => (
              <div
                key={workflow.id}
                className="flex items-center gap-2 p-2 bg-background rounded-md border border-border/50 hover:border-green-500/30 transition-colors"
              >
                <Sparkles className={`w-4 h-4 ${getStatusColors("success").text} flex-shrink-0`} />
                <div className="flex-1 min-w-0">
                  <p className="text-sm font-medium truncate">{workflow.name}</p>
                  <p className="text-xs text-muted-foreground truncate">
                    {workflow.steps.length} step{workflow.steps.length !== 1 ? "s" : ""}
                  </p>
                </div>
                <button
                  onClick={() => loadAiWorkflow(workflow)}
                  className={`px-2 py-1 text-xs ${getStatusColors("success").bg} ${getStatusColors("success").text} rounded hover:bg-green-500/20 transition-colors`}
                >
                  Load
                </button>
                <button
                  onClick={() => deleteAiWorkflow(workflow.id)}
                  className={`p-1 text-muted-foreground hover:${getStatusColors("error").text} transition-colors`}
                  title="Delete workflow"
                >
                  <Trash2 className="w-4 h-4" />
                </button>
              </div>
            ))
          )}
        </div>
      )}
    </div>
  );
}

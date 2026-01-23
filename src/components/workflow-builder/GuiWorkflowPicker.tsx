/**
 * GuiWorkflowPicker.tsx
 *
 * Dropdown picker for selecting GUI workflows from loaded configuration.
 * Used by WorkflowRefConfig to select workflows to run.
 */

import React, { useState, useMemo } from "react";
import { ChevronDown, Search, AlertCircle, FolderOpen } from "lucide-react";
import { useExecution } from "../../contexts/ExecutionContext";
import type { Workflow } from "../../hooks";

// =============================================================================
// Types
// =============================================================================

interface GuiWorkflowPickerProps {
  /** Currently selected workflow ID */
  selectedWorkflowId: string;
  /** Called when a workflow is selected */
  onSelect: (workflow: { id: string; name: string }) => void;
  /** Optional placeholder text */
  placeholder?: string;
}

// =============================================================================
// Component
// =============================================================================

export function GuiWorkflowPicker({
  selectedWorkflowId,
  onSelect,
  placeholder = "Select a workflow...",
}: GuiWorkflowPickerProps) {
  const { workflows, configLoaded } = useExecution();
  const [isOpen, setIsOpen] = useState(false);
  const [searchQuery, setSearchQuery] = useState("");

  // Find the selected workflow
  const selectedWorkflow = useMemo(() => {
    return workflows.find((w) => w.id === selectedWorkflowId);
  }, [workflows, selectedWorkflowId]);

  // Group workflows by category
  const groupedWorkflows = useMemo(() => {
    const filtered = workflows.filter(
      (w) =>
        w.name.toLowerCase().includes(searchQuery.toLowerCase()) ||
        (w.category && w.category.toLowerCase().includes(searchQuery.toLowerCase())),
    );

    const groups: Record<string, Workflow[]> = {};
    for (const workflow of filtered) {
      const category = workflow.category || "Uncategorized";
      if (!groups[category]) {
        groups[category] = [];
      }
      groups[category].push(workflow);
    }

    // Sort categories alphabetically, but put "Uncategorized" last
    const sortedCategories = Object.keys(groups).sort((a, b) => {
      if (a === "Uncategorized") return 1;
      if (b === "Uncategorized") return -1;
      return a.localeCompare(b);
    });

    return sortedCategories.map((category) => ({
      category,
      workflows: groups[category],
    }));
  }, [workflows, searchQuery]);

  // Show search if >5 workflows
  const showSearch = workflows.length > 5;

  // Handle workflow selection
  const handleSelect = (workflow: Workflow) => {
    onSelect({ id: workflow.id, name: workflow.name });
    setIsOpen(false);
    setSearchQuery("");
  };

  // Render edge case: No config loaded
  if (!configLoaded) {
    return (
      <div className="p-3 bg-zinc-800/50 border border-zinc-700 rounded-md">
        <div className="flex items-center gap-2 text-zinc-500">
          <AlertCircle className="w-4 h-4" />
          <span className="text-sm">Load a configuration first to select workflows</span>
        </div>
      </div>
    );
  }

  // Render edge case: No workflows in config
  if (workflows.length === 0) {
    return (
      <div className="p-3 bg-zinc-800/50 border border-zinc-700 rounded-md">
        <div className="flex items-center gap-2 text-zinc-500">
          <FolderOpen className="w-4 h-4" />
          <span className="text-sm">No workflows in loaded configuration</span>
        </div>
      </div>
    );
  }

  return (
    <div className="relative">
      {/* Trigger Button */}
      <button
        type="button"
        onClick={() => setIsOpen(!isOpen)}
        className="w-full flex items-center justify-between px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 hover:border-zinc-600 focus:outline-none focus:ring-2 focus:ring-purple-500/50 transition-colors"
        data-ui-id="workflow-builder-gui-workflow-picker-toggle-btn"
      >
        <span className={selectedWorkflow ? "text-zinc-200" : "text-zinc-500"}>
          {selectedWorkflow ? selectedWorkflow.name : placeholder}
        </span>
        <ChevronDown
          className={`w-4 h-4 text-zinc-400 transition-transform ${isOpen ? "rotate-180" : ""}`}
        />
      </button>

      {/* Warning if selected workflow no longer exists */}
      {selectedWorkflowId && !selectedWorkflow && (
        <div className="mt-1 flex items-center gap-1 text-xs text-amber-400">
          <AlertCircle className="w-3 h-3" />
          <span>Selected workflow not found in current config</span>
        </div>
      )}

      {/* Dropdown */}
      {isOpen && (
        <div className="absolute z-50 w-full mt-1 bg-zinc-800 border border-zinc-700 rounded-md shadow-xl overflow-hidden">
          {/* Search Input */}
          {showSearch && (
            <div className="p-2 border-b border-zinc-700">
              <div className="relative">
                <Search className="absolute left-2.5 top-1/2 -translate-y-1/2 w-4 h-4 text-zinc-500" />
                <input
                  type="text"
                  value={searchQuery}
                  onChange={(e) => setSearchQuery(e.target.value)}
                  placeholder="Search workflows..."
                  className="w-full pl-8 pr-3 py-1.5 bg-zinc-700 border border-zinc-600 rounded text-sm text-zinc-200 placeholder-zinc-500 focus:outline-none focus:ring-1 focus:ring-purple-500/50"
                  autoFocus
                  data-ui-id="workflow-builder-gui-workflow-picker-search-input"
                />
              </div>
            </div>
          )}

          {/* Workflow List */}
          <div className="max-h-64 overflow-y-auto">
            {groupedWorkflows.length === 0 ? (
              <div className="p-3 text-center text-sm text-zinc-500">No workflows found</div>
            ) : (
              groupedWorkflows.map((group) => (
                <div key={group.category}>
                  {/* Category Header */}
                  {groupedWorkflows.length > 1 && (
                    <div className="px-3 py-1.5 bg-zinc-750 text-xs font-medium text-zinc-500 uppercase tracking-wider">
                      {group.category}
                    </div>
                  )}

                  {/* Workflows in Category */}
                  {group.workflows.map((workflow) => (
                    <button
                      key={workflow.id}
                      onClick={() => handleSelect(workflow)}
                      className={`w-full flex items-center gap-2 px-3 py-2 text-sm text-left hover:bg-zinc-700/50 transition-colors ${
                        workflow.id === selectedWorkflowId
                          ? "bg-purple-500/10 text-purple-400"
                          : "text-zinc-300"
                      }`}
                      data-ui-id={`workflow-builder-gui-workflow-picker-item-${workflow.id}`}
                    >
                      <span className="truncate">{workflow.name}</span>
                      {workflow.id === selectedWorkflowId && (
                        <span className="ml-auto text-xs text-purple-400">Selected</span>
                      )}
                    </button>
                  ))}
                </div>
              ))
            )}
          </div>
        </div>
      )}

      {/* Click outside to close */}
      {isOpen && (
        <div className="fixed inset-0 z-40" onClick={() => setIsOpen(false)} aria-hidden="true" />
      )}
    </div>
  );
}

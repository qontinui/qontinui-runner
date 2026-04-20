/**
 * Workflow Library Picker
 *
 * Modal dialog for selecting a saved workflow to reference as a workflow step.
 * Used for workflow composition - running one workflow as a step in another.
 */

import { useState, useEffect, useCallback } from "react";
import { Search, X, Check as CheckIcon, Workflow, FolderOpen, Layers } from "lucide-react";
import type { WorkflowPhase, UnifiedWorkflow } from "../../types/unified-workflow";
import { getTotalStepCount } from "../../types/unified-workflow";
import { getApiBase, tracedFetch } from "@/lib/runner-api";

interface WorkflowLibraryPickerProps {
  isOpen: boolean;
  onClose: () => void;
  onSelect: (workflow: UnifiedWorkflow, phase: WorkflowPhase) => void;
  phase: WorkflowPhase;
  /** Current workflow ID to exclude from the list (prevent self-reference) */
  excludeWorkflowId?: string;
}

export function WorkflowLibraryPicker({
  isOpen,
  onClose,
  onSelect,
  phase,
  excludeWorkflowId,
}: WorkflowLibraryPickerProps) {
  const [workflows, setWorkflows] = useState<UnifiedWorkflow[]>([]);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [searchQuery, setSearchQuery] = useState("");
  const [filterCategory, setFilterCategory] = useState<string | null>(null);
  const [isLoading, setIsLoading] = useState(false);

  // Fetch workflows from API
  useEffect(() => {
    if (!isOpen) return;

    const controller = new AbortController();
    let cancelled = false;

    const fetchWorkflows = async () => {
      setIsLoading(true);
      try {
        const response = await tracedFetch(`${getApiBase()}/unified-workflows`, {
          signal: controller.signal,
        });
        const result = await response.json();
        if (!cancelled) {
          if (result.success && result.data) {
            setWorkflows(result.data);
          } else if (Array.isArray(result)) {
            setWorkflows(result);
          } else {
            setWorkflows([]);
          }
        }
      } catch (err) {
        if (!cancelled) {
          console.error("Failed to fetch workflows:", err);
        }
      } finally {
        if (!cancelled) {
          setIsLoading(false);
        }
      }
    };

    fetchWorkflows();

    return () => {
      cancelled = true;
      controller.abort();
    };
  }, [isOpen]);

  // Reset selection when modal opens
  useEffect(() => {
    if (isOpen) {
      // eslint-disable-next-line react-hooks/set-state-in-effect
      setSelectedId(null);
      setSearchQuery("");
      setFilterCategory(null);
    }
  }, [isOpen]);

  // Filter workflows (excluding current workflow to prevent self-reference)
  const filteredWorkflows = workflows.filter((workflow) => {
    // Exclude current workflow
    if (excludeWorkflowId && workflow.id === excludeWorkflowId) return false;

    const matchesSearch =
      !searchQuery ||
      workflow.name.toLowerCase().includes(searchQuery.toLowerCase()) ||
      workflow.description?.toLowerCase().includes(searchQuery.toLowerCase());

    const matchesCategory = !filterCategory || workflow.category === filterCategory;

    return matchesSearch && matchesCategory;
  });

  // Get unique categories for filter
  const categories = [...new Set(workflows.map((w) => w.category).filter(Boolean))];

  const handleSelect = useCallback(() => {
    const selected = workflows.find((w) => w.id === selectedId);
    if (selected) {
      onSelect(selected, phase);
      onClose();
    }
  }, [workflows, selectedId, onSelect, phase, onClose]);

  const handleDoubleClick = useCallback(
    (workflow: UnifiedWorkflow) => {
      onSelect(workflow, phase);
      onClose();
    },
    [onSelect, phase, onClose],
  );

  if (!isOpen) return null;

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60">
      <div className="bg-card border border-border rounded-lg shadow-2xl w-[700px] max-w-[90vw] max-h-[80vh] flex flex-col">
        {/* Header */}
        <div className="flex items-center justify-between px-4 py-3 border-b border-border">
          <div>
            <h2 className="text-lg font-semibold">Select Workflow to Reference</h2>
            <p className="text-sm text-muted-foreground">
              Choose a workflow to run as a {phase} step
            </p>
          </div>
          <button
            onClick={onClose}
            className="p-1.5 hover:bg-muted/80 rounded-md transition-colors"
          >
            <X className="w-5 h-5" />
          </button>
        </div>

        {/* Search and Filter */}
        <div className="flex items-center gap-3 px-4 py-3 border-b border-border">
          <div className="relative flex-1">
            <Search className="absolute left-3 top-1/2 -translate-y-1/2 w-4 h-4 text-muted-foreground" />
            <input
              type="text"
              placeholder="Search workflows..."
              value={searchQuery}
              onChange={(e) => setSearchQuery(e.target.value)}
              className="w-full pl-9 pr-3 py-2 text-sm bg-muted border border-border rounded-md
                       focus:outline-hidden focus:ring-2 focus:ring-cyan-500/50"
              autoFocus
            />
          </div>
          {categories.length > 0 && (
            <select
              value={filterCategory || ""}
              onChange={(e) => setFilterCategory(e.target.value || null)}
              className="px-3 py-2 text-sm bg-muted border border-border rounded-md
                       focus:outline-hidden focus:ring-2 focus:ring-cyan-500/50"
            >
              <option value="">All Categories</option>
              {categories.map((cat) => (
                <option key={cat} value={cat}>
                  {cat}
                </option>
              ))}
            </select>
          )}
        </div>

        {/* Workflow List */}
        <div className="flex-1 overflow-y-auto p-4">
          {isLoading ? (
            <div className="text-center py-8 text-muted-foreground">Loading workflows...</div>
          ) : filteredWorkflows.length === 0 ? (
            <div className="text-center py-8 text-muted-foreground">
              {workflows.length === 0
                ? "No workflows saved yet. Create workflows in the Workflow Builder first."
                : "No workflows match your search."}
            </div>
          ) : (
            <div className="grid grid-cols-1 gap-2">
              {filteredWorkflows.map((workflow) => {
                const isSelected = selectedId === workflow.id;
                const stepCount = getTotalStepCount(workflow);

                return (
                  <button
                    key={workflow.id}
                    onClick={() => setSelectedId(workflow.id)}
                    onDoubleClick={() => handleDoubleClick(workflow)}
                    className={`
                      relative flex items-start gap-3 p-3 rounded-lg text-left transition-all
                      ${
                        isSelected
                          ? "bg-cyan-500/15 border-2 border-cyan-500"
                          : "bg-muted/50 border-2 border-transparent hover:bg-muted hover:border-border"
                      }
                    `}
                  >
                    {isSelected && (
                      <div className="absolute top-2 right-2">
                        <CheckIcon className="w-4 h-4 text-cyan-400" />
                      </div>
                    )}
                    <div className="p-2 rounded-md bg-card text-blue-400">
                      <Workflow className="w-4 h-4" />
                    </div>
                    <div className="flex-1 min-w-0">
                      <div className="flex items-center gap-2">
                        <span className="font-medium">{workflow.name}</span>
                      </div>
                      {workflow.description && (
                        <p className="text-sm text-muted-foreground mt-0.5 line-clamp-1">
                          {workflow.description}
                        </p>
                      )}
                      <div className="flex items-center gap-3 mt-1.5 text-xs text-muted-foreground">
                        <span className="flex items-center gap-1">
                          <Layers className="w-3 h-3" />
                          {stepCount} step{stepCount !== 1 ? "s" : ""}
                        </span>
                        {workflow.category && (
                          <>
                            <span className="text-border">|</span>
                            <span className="flex items-center gap-1">
                              <FolderOpen className="w-3 h-3" />
                              {workflow.category}
                            </span>
                          </>
                        )}
                        {workflow.maxIterations && workflow.maxIterations > 1 && (
                          <>
                            <span className="text-border">|</span>
                            <span>Max {workflow.maxIterations} iterations</span>
                          </>
                        )}
                      </div>
                      {/* Show tags */}
                      {workflow.tags && workflow.tags.length > 0 && (
                        <div className="mt-2 flex flex-wrap gap-1">
                          {workflow.tags.slice(0, 5).map((tag) => (
                            <span key={tag} className="text-xs px-1.5 py-0.5 bg-muted/80 rounded">
                              {tag}
                            </span>
                          ))}
                          {workflow.tags.length > 5 && (
                            <span className="text-xs px-1.5 py-0.5 text-muted-foreground">
                              +{workflow.tags.length - 5} more
                            </span>
                          )}
                        </div>
                      )}
                    </div>
                  </button>
                );
              })}
            </div>
          )}
        </div>

        {/* Footer */}
        <div className="flex items-center justify-between px-4 py-3 border-t border-border bg-muted/50">
          <span
            data-content-role="metric"
            data-content-label="available workflow count"
            className="text-sm text-muted-foreground"
          >
            {filteredWorkflows.length} workflow{filteredWorkflows.length !== 1 ? "s" : ""} available
          </span>
          <div className="flex items-center gap-2">
            <button
              onClick={onClose}
              className="px-4 py-2 text-sm rounded-md hover:bg-muted/80 transition-colors"
            >
              Cancel
            </button>
            <button
              onClick={handleSelect}
              disabled={!selectedId}
              className="px-4 py-2 text-sm bg-cyan-600 hover:bg-cyan-700 text-white rounded-md
                       transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
            >
              Add to Workflow
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}

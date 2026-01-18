/**
 * WorkflowLibraryPicker.tsx
 *
 * Modal dialog for selecting a saved workflow from the library.
 * Used when opening an existing workflow in the Workflow Builder.
 */

import { useState, useEffect, useMemo } from "react";
import { GitBranch, Search, X, Loader2, Check, Play, Clock } from "lucide-react";
import type { UnifiedWorkflow } from "../../types";
import { getAccentColors } from "@/design-system";

const API_BASE = "http://localhost:9876";

interface WorkflowLibraryPickerProps {
  /** Whether the picker is open */
  isOpen: boolean;
  /** Called when the picker is closed */
  onClose: () => void;
  /** Called when a workflow is selected */
  onSelect: (workflow: UnifiedWorkflow) => void;
}

// Get step count for display
function getStepCount(workflow: UnifiedWorkflow): number {
  return (
    workflow.setup_steps.length + workflow.verification_steps.length + workflow.agentic_steps.length
  );
}

// Format date for display
function formatDate(dateStr: string): string {
  try {
    const date = new Date(dateStr);
    return date.toLocaleDateString(undefined, {
      month: "short",
      day: "numeric",
      year: "numeric",
    });
  } catch {
    return dateStr;
  }
}

export function WorkflowLibraryPicker({ isOpen, onClose, onSelect }: WorkflowLibraryPickerProps) {
  const [workflows, setWorkflows] = useState<UnifiedWorkflow[]>([]);
  const [isLoading, setIsLoading] = useState(false);
  const [searchQuery, setSearchQuery] = useState("");
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [categoryFilter, setCategoryFilter] = useState<string>("");

  // Fetch saved workflows
  useEffect(() => {
    if (!isOpen) return;

    const fetchWorkflows = async () => {
      setIsLoading(true);
      try {
        const response = await fetch(`${API_BASE}/unified-workflows`);
        const data = await response.json();
        if (data.success && data.data) {
          setWorkflows(data.data);
        }
      } catch (error) {
        console.error("Failed to fetch workflows:", error);
      } finally {
        setIsLoading(false);
      }
    };

    fetchWorkflows();
    setSearchQuery("");
    setSelectedId(null);
    setCategoryFilter("");
  }, [isOpen]);

  // Get unique categories
  const categories = useMemo(() => {
    const cats = new Set<string>();
    workflows.forEach((w) => {
      if (w.category) cats.add(w.category);
    });
    return Array.from(cats).sort();
  }, [workflows]);

  // Filter workflows
  const filteredWorkflows = useMemo(() => {
    return workflows.filter((w) => {
      const matchesSearch =
        !searchQuery ||
        w.name.toLowerCase().includes(searchQuery.toLowerCase()) ||
        w.description?.toLowerCase().includes(searchQuery.toLowerCase());

      const matchesCategory = !categoryFilter || w.category === categoryFilter;

      return matchesSearch && matchesCategory;
    });
  }, [workflows, searchQuery, categoryFilter]);

  // Handle selection
  const handleSelect = () => {
    const selected = workflows.find((w) => w.id === selectedId);
    if (selected) {
      onSelect(selected);
      onClose();
    }
  };

  if (!isOpen) return null;

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center">
      {/* Backdrop */}
      <div className="absolute inset-0 bg-black/50" onClick={onClose} />

      {/* Dialog */}
      <div className="relative bg-card border border-border rounded-lg shadow-xl w-full max-w-2xl mx-4 max-h-[80vh] flex flex-col overflow-hidden">
        {/* Header */}
        <div className="flex items-center justify-between px-6 py-4 border-b border-border">
          <div className="flex items-center gap-2">
            <GitBranch className={`w-5 h-5 ${getAccentColors("purple").text}`} />
            <h3 className="text-lg font-semibold">Open Workflow</h3>
          </div>
          <button
            onClick={onClose}
            className="p-1 text-muted-foreground hover:text-foreground transition-colors"
          >
            <X className="w-5 h-5" />
          </button>
        </div>

        {/* Search and Filter */}
        <div className="px-6 py-3 border-b border-border flex gap-3">
          <div className="relative flex-1">
            <Search className="absolute left-3 top-1/2 -translate-y-1/2 w-4 h-4 text-muted-foreground" />
            <input
              type="text"
              value={searchQuery}
              onChange={(e) => setSearchQuery(e.target.value)}
              placeholder="Search by name or description..."
              className="w-full pl-10 pr-4 py-2 bg-background border border-border rounded-md text-sm focus:outline-none focus:ring-2 focus:ring-primary"
              autoFocus
            />
          </div>
          {categories.length > 0 && (
            <select
              value={categoryFilter}
              onChange={(e) => setCategoryFilter(e.target.value)}
              className="px-3 py-2 bg-background border border-border rounded-md text-sm focus:outline-none focus:ring-2 focus:ring-primary"
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

        {/* Content */}
        <div className="flex-1 overflow-y-auto p-4">
          {isLoading ? (
            <div className="flex items-center justify-center h-40">
              <Loader2 className="w-6 h-6 animate-spin text-muted-foreground" />
            </div>
          ) : filteredWorkflows.length === 0 ? (
            <div className="flex flex-col items-center justify-center h-40 text-muted-foreground">
              <GitBranch className="w-8 h-8 mb-2 opacity-50" />
              <p className="text-sm">
                {searchQuery ? "No matching workflows found" : "No saved workflows yet"}
              </p>
              <p className="text-xs mt-1">
                {searchQuery ? "Try a different search term" : "Save a workflow to see it here"}
              </p>
            </div>
          ) : (
            <div className="grid grid-cols-1 gap-2">
              {filteredWorkflows.map((workflow) => (
                <button
                  key={workflow.id}
                  onClick={() => setSelectedId(workflow.id)}
                  onDoubleClick={() => {
                    setSelectedId(workflow.id);
                    handleSelect();
                  }}
                  className={`w-full text-left p-3 rounded-lg border transition-colors ${
                    selectedId === workflow.id
                      ? "border-primary bg-primary/5"
                      : "border-border hover:border-muted-foreground hover:bg-muted/50"
                  }`}
                >
                  <div className="flex items-start gap-3">
                    <GitBranch className="w-5 h-5 text-purple-400 flex-shrink-0 mt-0.5" />
                    <div className="flex-1 min-w-0">
                      <div className="flex items-center gap-2">
                        <span className="font-medium text-sm">{workflow.name || "Untitled"}</span>
                        {selectedId === workflow.id && (
                          <Check className="w-4 h-4 text-primary flex-shrink-0" />
                        )}
                      </div>
                      {workflow.description && (
                        <div className="text-xs text-muted-foreground mt-0.5 line-clamp-1">
                          {workflow.description}
                        </div>
                      )}
                      <div className="flex items-center gap-3 mt-2 text-xs text-muted-foreground">
                        <span className="flex items-center gap-1">
                          <Play className="w-3 h-3" />
                          {getStepCount(workflow)} steps
                        </span>
                        <span className="flex items-center gap-1">
                          <Clock className="w-3 h-3" />
                          {formatDate(workflow.modified_at)}
                        </span>
                        {workflow.category && workflow.category !== "general" && (
                          <span className="px-1.5 py-0.5 bg-muted rounded text-xs">
                            {workflow.category}
                          </span>
                        )}
                      </div>
                    </div>
                  </div>
                </button>
              ))}
            </div>
          )}
        </div>

        {/* Footer */}
        <div className="flex gap-3 px-6 py-4 border-t border-border">
          <button
            onClick={onClose}
            className="flex-1 px-4 py-2 bg-muted text-foreground rounded-md font-medium hover:bg-muted/80 transition-colors"
          >
            Cancel
          </button>
          <button
            onClick={handleSelect}
            disabled={!selectedId}
            className={`flex-1 flex items-center justify-center gap-2 px-4 py-2 ${getAccentColors("purple").bgSolid} text-white rounded-md font-medium hover:opacity-90 disabled:opacity-50 disabled:cursor-not-allowed transition-colors`}
          >
            <Check className="w-4 h-4" />
            Open
          </button>
        </div>
      </div>
    </div>
  );
}

export default WorkflowLibraryPicker;

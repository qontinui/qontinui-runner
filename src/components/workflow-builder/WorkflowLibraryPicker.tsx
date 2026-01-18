/**
 * WorkflowLibraryPicker.tsx
 *
 * Modal dialog for selecting a saved workflow from the library.
 * Used when opening an existing workflow in the Workflow Builder.
 */

import { useState, useEffect, useMemo, useRef } from "react";
import { GitBranch, Search, X, Loader2, Check, Play, Clock, Download, Upload, MoreVertical } from "lucide-react";
import type { UnifiedWorkflow, WorkflowExport } from "../../types";
import { getAccentColors } from "@/design-system";

const API_BASE = "http://localhost:9876";

interface WorkflowLibraryPickerProps {
  /** Whether the picker is open */
  isOpen: boolean;
  /** Called when the picker is closed */
  onClose: () => void;
  /** Called when a workflow is selected */
  onSelect: (workflow: UnifiedWorkflow) => void;
  /** Called when a workflow is imported */
  onImport?: (workflow: UnifiedWorkflow) => void;
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

export function WorkflowLibraryPicker({ isOpen, onClose, onSelect, onImport }: WorkflowLibraryPickerProps) {
  const [workflows, setWorkflows] = useState<UnifiedWorkflow[]>([]);
  const [isLoading, setIsLoading] = useState(false);
  const [searchQuery, setSearchQuery] = useState("");
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [categoryFilter, setCategoryFilter] = useState<string>("");
  const [menuOpenId, setMenuOpenId] = useState<string | null>(null);
  const [isExporting, setIsExporting] = useState(false);
  const [importError, setImportError] = useState<string | null>(null);
  const fileInputRef = useRef<HTMLInputElement>(null);

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

  // Export a workflow
  const handleExport = async (workflowId: string) => {
    setIsExporting(true);
    setMenuOpenId(null);
    try {
      const response = await fetch(`${API_BASE}/unified-workflows/${workflowId}/export`);
      const data = await response.json();
      if (data.success && data.data) {
        const exportData = data.data as WorkflowExport;
        const blob = new Blob([JSON.stringify(exportData, null, 2)], {
          type: "application/json",
        });
        const url = URL.createObjectURL(blob);
        const a = document.createElement("a");
        a.href = url;
        a.download = `workflow-${exportData.workflow.name.replace(/[^a-zA-Z0-9]/g, "-")}.json`;
        a.click();
        URL.revokeObjectURL(url);
      }
    } catch (error) {
      console.error("Failed to export workflow:", error);
    } finally {
      setIsExporting(false);
    }
  };

  // Import a workflow from file
  const handleImport = async (event: React.ChangeEvent<HTMLInputElement>) => {
    const file = event.target.files?.[0];
    if (!file) return;

    setImportError(null);
    setIsLoading(true);

    try {
      const text = await file.text();
      const data = JSON.parse(text);

      // Validate the import format
      let workflowData: UnifiedWorkflow;
      if (data.manifest && data.workflow) {
        // Full export format
        workflowData = data.workflow;
      } else if (data.setup_steps !== undefined) {
        // Direct workflow object
        workflowData = data;
      } else {
        throw new Error("Invalid workflow file format");
      }

      // Call the import API
      const response = await fetch(`${API_BASE}/unified-workflows/import`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          workflow: workflowData,
          conflict_strategy: "generate",
        }),
      });

      const result = await response.json();
      if (result.success && result.data) {
        // Refresh the list
        const listResponse = await fetch(`${API_BASE}/unified-workflows`);
        const listData = await listResponse.json();
        if (listData.success && listData.data) {
          setWorkflows(listData.data);
        }
        // If there's an onImport callback, call it
        if (onImport) {
          onImport(result.data.workflow);
        }
      } else {
        setImportError(result.error || "Failed to import workflow");
      }
    } catch (error) {
      console.error("Failed to import workflow:", error);
      setImportError(error instanceof Error ? error.message : "Failed to import workflow");
    } finally {
      setIsLoading(false);
      // Reset the file input
      if (fileInputRef.current) {
        fileInputRef.current.value = "";
      }
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
          <div className="flex items-center gap-2">
            {/* Import button */}
            <input
              ref={fileInputRef}
              type="file"
              accept=".json"
              onChange={handleImport}
              className="hidden"
              id="workflow-import-input"
            />
            <button
              onClick={() => fileInputRef.current?.click()}
              disabled={isLoading}
              className="flex items-center gap-1.5 px-3 py-1.5 text-sm bg-muted hover:bg-muted/80 rounded-md transition-colors disabled:opacity-50"
              title="Import workflow from file"
            >
              <Upload className="w-4 h-4" />
              Import
            </button>
            <button
              onClick={onClose}
              className="p-1 text-muted-foreground hover:text-foreground transition-colors"
            >
              <X className="w-5 h-5" />
            </button>
          </div>
        </div>

        {/* Import error message */}
        {importError && (
          <div className="mx-6 mt-3 p-2 bg-red-500/10 border border-red-500/30 rounded-md text-red-500 text-sm">
            {importError}
          </div>
        )}

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
                <div
                  key={workflow.id}
                  className={`relative w-full text-left p-3 rounded-lg border transition-colors ${
                    selectedId === workflow.id
                      ? "border-primary bg-primary/5"
                      : "border-border hover:border-muted-foreground hover:bg-muted/50"
                  }`}
                >
                  <button
                    onClick={() => setSelectedId(workflow.id)}
                    onDoubleClick={() => {
                      setSelectedId(workflow.id);
                      handleSelect();
                    }}
                    className="w-full text-left"
                  >
                    <div className="flex items-start gap-3">
                      <GitBranch className="w-5 h-5 text-purple-400 flex-shrink-0 mt-0.5" />
                      <div className="flex-1 min-w-0 pr-8">
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
                  {/* Export button */}
                  <button
                    onClick={(e) => {
                      e.stopPropagation();
                      handleExport(workflow.id);
                    }}
                    disabled={isExporting}
                    className="absolute right-3 top-3 p-1.5 text-muted-foreground hover:text-foreground hover:bg-muted rounded transition-colors"
                    title="Export workflow"
                  >
                    <Download className="w-4 h-4" />
                  </button>
                </div>
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

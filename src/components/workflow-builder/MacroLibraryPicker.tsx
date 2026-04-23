/**
 * Macro Library Picker
 *
 * Modal dialog for selecting a saved macro from the Macro Manager library
 * to add as a workflow step.
 */

import { useState, useEffect, useCallback } from "react";
import { Search, X, Check as CheckIcon, Play, FolderOpen, Tag } from "lucide-react";
import type { WorkflowPhase } from "../../types/unified-workflow";
import { getApiBase, tracedFetch } from "@/lib/runner-api";

// Macro type from Macro Manager
interface Macro {
  id: string;
  name: string;
  description: string;
  steps: MacroStep[];
  category: string;
  tags: string[];
  created_at: string;
  modified_at: string;
  run_count: number;
}

interface MacroStep {
  id: string;
  action_type: string;
  description: string;
}

interface MacroLibraryPickerProps {
  isOpen: boolean;
  onClose: () => void;
  onSelect: (macro: Macro, phase: WorkflowPhase) => void;
  phase: WorkflowPhase;
}

export function MacroLibraryPicker({ isOpen, onClose, onSelect, phase }: MacroLibraryPickerProps) {
  const [macros, setMacros] = useState<Macro[]>([]);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [searchQuery, setSearchQuery] = useState("");
  const [filterCategory, setFilterCategory] = useState<string | null>(null);
  const [isLoading, setIsLoading] = useState(false);

  // Fetch macros from API
  useEffect(() => {
    if (!isOpen) return;

    const controller = new AbortController();
    let cancelled = false;

    const fetchMacros = async () => {
      setIsLoading(true);
      try {
        const response = await tracedFetch(`${getApiBase()}/macros`, { signal: controller.signal });
        if (response.ok) {
          const data = await response.json();
          if (!cancelled) {
            setMacros(data.macros || []);
          }
        }
      } catch (err) {
        if (!cancelled) {
          console.error("Failed to fetch macros:", err);
        }
      } finally {
        if (!cancelled) {
          setIsLoading(false);
        }
      }
    };

    fetchMacros();

    return () => {
      cancelled = true;
      controller.abort();
    };
  }, [isOpen]);

  // Reset selection when modal opens. Deferred into a microtask so the
  // setState calls don't fire synchronously from the effect body
  // (react-hooks/set-state-in-effect).
  useEffect(() => {
    if (!isOpen) return;
    let cancelled = false;
    void Promise.resolve().then(() => {
      if (cancelled) return;
      setSelectedId(null);
      setSearchQuery("");
      setFilterCategory(null);
    });
    return () => {
      cancelled = true;
    };
  }, [isOpen]);

  // Filter macros
  const filteredMacros = macros.filter((macro) => {
    const matchesSearch =
      !searchQuery ||
      macro.name.toLowerCase().includes(searchQuery.toLowerCase()) ||
      macro.description?.toLowerCase().includes(searchQuery.toLowerCase());

    const matchesCategory = !filterCategory || macro.category === filterCategory;

    return matchesSearch && matchesCategory;
  });

  // Get unique categories for filter
  const categories = [...new Set(macros.map((m) => m.category).filter(Boolean))];

  const handleSelect = useCallback(() => {
    const selected = macros.find((m) => m.id === selectedId);
    if (selected) {
      onSelect(selected, phase);
      onClose();
    }
  }, [macros, selectedId, onSelect, phase, onClose]);

  const handleDoubleClick = useCallback(
    (macro: Macro) => {
      onSelect(macro, phase);
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
            <h2 className="text-lg font-semibold">Select Macro from Library</h2>
            <p className="text-sm text-muted-foreground">
              Choose a saved macro to add as a {phase} step
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
              placeholder="Search macros..."
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

        {/* Macro List */}
        <div className="flex-1 overflow-y-auto p-4">
          {isLoading ? (
            <div className="text-center py-8 text-muted-foreground">Loading macros...</div>
          ) : filteredMacros.length === 0 ? (
            <div className="text-center py-8 text-muted-foreground">
              {macros.length === 0
                ? "No macros saved yet. Create macros in the Macro Manager first."
                : "No macros match your search."}
            </div>
          ) : (
            <div className="grid grid-cols-1 gap-2">
              {filteredMacros.map((macro) => {
                const isSelected = selectedId === macro.id;

                return (
                  <button
                    key={macro.id}
                    onClick={() => setSelectedId(macro.id)}
                    onDoubleClick={() => handleDoubleClick(macro)}
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
                    <div className="p-2 rounded-md bg-card text-emerald-400">
                      <Play className="w-4 h-4" />
                    </div>
                    <div className="flex-1 min-w-0">
                      <div className="flex items-center gap-2">
                        <span className="font-medium">{macro.name}</span>
                      </div>
                      {macro.description && (
                        <p className="text-sm text-muted-foreground mt-0.5 line-clamp-1">
                          {macro.description}
                        </p>
                      )}
                      <div className="flex items-center gap-3 mt-1.5 text-xs text-muted-foreground">
                        <span className="flex items-center gap-1">
                          <Play className="w-3 h-3" />
                          {macro.steps?.length || 0} step
                          {(macro.steps?.length || 0) !== 1 ? "s" : ""}
                        </span>
                        {macro.category && (
                          <>
                            <span className="text-border">|</span>
                            <span className="flex items-center gap-1">
                              <FolderOpen className="w-3 h-3" />
                              {macro.category}
                            </span>
                          </>
                        )}
                        {macro.run_count > 0 && (
                          <>
                            <span className="text-border">|</span>
                            <span>Run {macro.run_count}x</span>
                          </>
                        )}
                      </div>
                      {/* Show tags */}
                      {macro.tags && macro.tags.length > 0 && (
                        <div className="mt-2 flex flex-wrap gap-1">
                          {macro.tags.slice(0, 5).map((tag) => (
                            <span
                              key={tag}
                              className="text-xs px-1.5 py-0.5 bg-muted/80 rounded flex items-center gap-1"
                            >
                              <Tag className="w-2.5 h-2.5" />
                              {tag}
                            </span>
                          ))}
                          {macro.tags.length > 5 && (
                            <span className="text-xs px-1.5 py-0.5 text-muted-foreground">
                              +{macro.tags.length - 5} more
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
            data-content-label="available macro count"
            className="text-sm text-muted-foreground"
          >
            {filteredMacros.length} macro{filteredMacros.length !== 1 ? "s" : ""} available
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

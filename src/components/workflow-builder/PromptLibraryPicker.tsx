/**
 * PromptLibraryPicker.tsx
 *
 * Modal dialog for selecting a saved prompt from the library.
 * Used when adding a prompt step "From Library".
 */

import { useState, useEffect, useMemo } from "react";
import { MessageSquare, Search, X, Loader2, Check } from "lucide-react";
import type { SavedPrompt, WorkflowPhase } from "../../types";
import { getAccentColors } from "@/design-system";
import { getApiBase, tracedFetch } from "@/lib/runner-api";

interface PromptLibraryPickerProps {
  /** Whether the picker is open */
  isOpen: boolean;
  /** Called when the picker is closed */
  onClose: () => void;
  /** Called when a prompt is selected */
  onSelect: (prompt: SavedPrompt, phase: WorkflowPhase) => void;
  /** The phase for the new step */
  phase: WorkflowPhase;
}

export function PromptLibraryPicker({
  isOpen,
  onClose,
  onSelect,
  phase,
}: PromptLibraryPickerProps) {
  const [savedPrompts, setSavedPrompts] = useState<SavedPrompt[]>([]);
  const [isLoading, setIsLoading] = useState(false);
  const [searchQuery, setSearchQuery] = useState("");
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [categoryFilter, setCategoryFilter] = useState<string>("");

  // Fetch saved prompts
  useEffect(() => {
    if (!isOpen) return;

    const controller = new AbortController();
    let cancelled = false;

    const fetchPrompts = async () => {
      setIsLoading(true);
      try {
        const response = await tracedFetch(`${getApiBase()}/prompts`, {
          signal: controller.signal,
        });
        const data = await response.json();
        if (!cancelled && data.success && data.data) {
          setSavedPrompts(data.data);
        }
      } catch (error) {
        if (!cancelled) {
          console.error("Failed to fetch saved prompts:", error);
        }
      } finally {
        if (!cancelled) {
          setIsLoading(false);
        }
      }
    };

    fetchPrompts();

    setSearchQuery("");
    setSelectedId(null);
    setCategoryFilter("");

    return () => {
      cancelled = true;
      controller.abort();
    };
  }, [isOpen]);

  // Get unique categories
  const categories = useMemo(() => {
    const cats = new Set<string>();
    savedPrompts.forEach((p) => {
      if (p.category) cats.add(p.category);
    });
    return Array.from(cats).sort();
  }, [savedPrompts]);

  // Filter prompts
  const filteredPrompts = useMemo(() => {
    return savedPrompts.filter((p) => {
      const matchesSearch =
        !searchQuery ||
        p.name.toLowerCase().includes(searchQuery.toLowerCase()) ||
        p.content.toLowerCase().includes(searchQuery.toLowerCase()) ||
        p.description?.toLowerCase().includes(searchQuery.toLowerCase());

      const matchesCategory = !categoryFilter || p.category === categoryFilter;

      return matchesSearch && matchesCategory;
    });
  }, [savedPrompts, searchQuery, categoryFilter]);

  // Handle selection
  const handleSelect = () => {
    const selected = savedPrompts.find((p) => p.id === selectedId);
    if (selected) {
      onSelect(selected, phase);
      onClose();
    }
  };

  if (!isOpen) return null;

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center">
      {/* Backdrop */}
      <div
        role="button"
        tabIndex={0}
        aria-label="Close prompt picker"
        className="absolute inset-0 bg-black/50"
        onClick={onClose}
        onKeyDown={(e) => {
          if (e.key === "Enter" || e.key === " ") onClose();
        }}
      />

      {/* Dialog */}
      <div className="relative bg-card border border-border rounded-lg shadow-xl w-full max-w-2xl mx-4 max-h-[80vh] flex flex-col overflow-hidden">
        {/* Header */}
        <div className="flex items-center justify-between px-6 py-4 border-b border-border">
          <div className="flex items-center gap-2">
            <MessageSquare className={`w-5 h-5 ${getAccentColors("amber").text}`} />
            <h3 className="text-lg font-semibold">Select AI Prompt</h3>
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
              placeholder="Search by name or content..."
              className="w-full pl-10 pr-4 py-2 bg-background border border-border rounded-md text-sm focus:outline-hidden focus:ring-2 focus:ring-primary"
              autoFocus
            />
          </div>
          {categories.length > 0 && (
            <select
              value={categoryFilter}
              onChange={(e) => setCategoryFilter(e.target.value)}
              className="px-3 py-2 bg-background border border-border rounded-md text-sm focus:outline-hidden focus:ring-2 focus:ring-primary"
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
          ) : filteredPrompts.length === 0 ? (
            <div className="flex flex-col items-center justify-center h-40 text-muted-foreground">
              <MessageSquare className="w-8 h-8 mb-2 opacity-50" />
              <p className="text-sm">
                {searchQuery ? "No matching prompts found" : "No saved prompts yet"}
              </p>
              <p className="text-xs mt-1">
                {searchQuery
                  ? "Try a different search term"
                  : "Create prompts in the Prompt Library tab"}
              </p>
            </div>
          ) : (
            <div className="grid grid-cols-1 gap-2">
              {filteredPrompts.map((prompt) => (
                <button
                  key={prompt.id}
                  onClick={() => setSelectedId(prompt.id)}
                  onDoubleClick={() => {
                    setSelectedId(prompt.id);
                    handleSelect();
                  }}
                  className={`w-full text-left p-3 rounded-lg border transition-colors ${
                    selectedId === prompt.id
                      ? "border-primary bg-primary/5"
                      : "border-border hover:border-muted-foreground hover:bg-muted/50"
                  }`}
                >
                  <div className="flex items-start gap-3">
                    <MessageSquare
                      className={`w-4 h-4 mt-0.5 shrink-0 ${getAccentColors("amber").text}`}
                    />
                    <div className="flex-1 min-w-0">
                      <div className="flex items-center gap-2">
                        <span className="font-medium text-sm">{prompt.name}</span>
                        {prompt.category && (
                          <span className="px-1.5 py-0.5 text-xs bg-zinc-700 text-zinc-400 rounded">
                            {prompt.category}
                          </span>
                        )}
                        {selectedId === prompt.id && (
                          <Check className="w-4 h-4 text-primary shrink-0" />
                        )}
                      </div>
                      {prompt.description && (
                        <div className="text-xs text-muted-foreground mt-0.5 line-clamp-1">
                          {prompt.description}
                        </div>
                      )}
                      <div className="text-xs text-zinc-500 font-mono mt-1 line-clamp-2">
                        {prompt.content.substring(0, 150)}
                        {prompt.content.length > 150 && "..."}
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
            className={`flex-1 flex items-center justify-center gap-2 px-4 py-2 ${getAccentColors("amber").bgSolid} text-white rounded-md font-medium hover:opacity-90 disabled:opacity-50 disabled:cursor-not-allowed transition-colors`}
          >
            <Check className="w-4 h-4" />
            Select
          </button>
        </div>
      </div>
    </div>
  );
}

export default PromptLibraryPicker;

/**
 * Playwright Script Library Picker
 *
 * Modal dialog for selecting a saved Playwright script from the library
 * to add as a workflow step.
 */

import { useState, useEffect, useCallback } from "react";
import {
  Search,
  X,
  Check as CheckIcon,
  TestTube2,
  Globe,
  Clock,
  Tag,
  FolderOpen,
} from "lucide-react";
import type { WorkflowPhase } from "../../types/unified-workflow";

// PlaywrightScript type from script storage
interface PlaywrightScript {
  id: string;
  name: string;
  description: string;
  target_url: string;
  script_content: string;
  category: string;
  tags: string[];
  timeout_seconds: number;
  display_mode: string;
  browser: string;
  headless: boolean;
  created_at: string;
  modified_at: string;
}

interface PlaywrightScriptLibraryPickerProps {
  isOpen: boolean;
  onClose: () => void;
  onSelect: (script: PlaywrightScript, phase: WorkflowPhase) => void;
  phase: WorkflowPhase;
}

const API_BASE = "http://localhost:9876";

export function PlaywrightScriptLibraryPicker({
  isOpen,
  onClose,
  onSelect,
  phase,
}: PlaywrightScriptLibraryPickerProps) {
  const [scripts, setScripts] = useState<PlaywrightScript[]>([]);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [searchQuery, setSearchQuery] = useState("");
  const [filterCategory, setFilterCategory] = useState<string | null>(null);
  const [isLoading, setIsLoading] = useState(false);

  // Fetch scripts from API
  useEffect(() => {
    if (!isOpen) return;

    const fetchScripts = async () => {
      setIsLoading(true);
      try {
        const response = await fetch(`${API_BASE}/playwright/tests`);
        if (response.ok) {
          const data = await response.json();
          setScripts(data.scripts || []);
        }
      } catch (err) {
        console.error("Failed to fetch Playwright scripts:", err);
      } finally {
        setIsLoading(false);
      }
    };

    fetchScripts();
  }, [isOpen]);

  // Reset selection when modal opens
  useEffect(() => {
    if (isOpen) {
      setSelectedId(null);
      setSearchQuery("");
      setFilterCategory(null);
    }
  }, [isOpen]);

  // Filter scripts
  const filteredScripts = scripts.filter((script) => {
    const matchesSearch =
      !searchQuery ||
      script.name.toLowerCase().includes(searchQuery.toLowerCase()) ||
      script.description?.toLowerCase().includes(searchQuery.toLowerCase()) ||
      script.target_url?.toLowerCase().includes(searchQuery.toLowerCase());

    const matchesCategory = !filterCategory || script.category === filterCategory;

    return matchesSearch && matchesCategory;
  });

  // Get unique categories for filter
  const categories = [...new Set(scripts.map((s) => s.category).filter(Boolean))];

  const handleSelect = useCallback(() => {
    const selected = scripts.find((s) => s.id === selectedId);
    if (selected) {
      onSelect(selected, phase);
      onClose();
    }
  }, [scripts, selectedId, onSelect, phase, onClose]);

  const handleDoubleClick = useCallback(
    (script: PlaywrightScript) => {
      onSelect(script, phase);
      onClose();
    },
    [onSelect, phase, onClose],
  );

  if (!isOpen) return null;

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60">
      <div
        data-ui-id="dialog-playwright-script-library-picker"
        className="bg-neutral-900 border border-neutral-700 rounded-lg shadow-2xl w-[700px] max-w-[90vw] max-h-[80vh] flex flex-col"
      >
        {/* Header */}
        <div className="flex items-center justify-between px-4 py-3 border-b border-neutral-700">
          <div>
            <h2 className="text-lg font-semibold">Select Playwright Script</h2>
            <p className="text-sm text-muted-foreground">
              Choose a saved script to add as a {phase} step
            </p>
          </div>
          <button
            onClick={onClose}
            className="p-1.5 hover:bg-neutral-700 rounded-md transition-colors"
            data-ui-id="workflow-builder-script-picker-close-btn"
          >
            <X className="w-5 h-5" />
          </button>
        </div>

        {/* Search and Filter */}
        <div className="flex items-center gap-3 px-4 py-3 border-b border-neutral-800">
          <div className="relative flex-1">
            <Search className="absolute left-3 top-1/2 -translate-y-1/2 w-4 h-4 text-muted-foreground" />
            <input
              type="text"
              placeholder="Search scripts..."
              value={searchQuery}
              onChange={(e) => setSearchQuery(e.target.value)}
              className="w-full pl-9 pr-3 py-2 text-sm bg-neutral-800 border border-neutral-700 rounded-md
                       focus:outline-none focus:ring-2 focus:ring-cyan-500/50"
              autoFocus
              data-ui-id="workflow-builder-script-picker-search-input"
            />
          </div>
          {categories.length > 0 && (
            <select
              value={filterCategory || ""}
              onChange={(e) => setFilterCategory(e.target.value || null)}
              className="px-3 py-2 text-sm bg-neutral-800 border border-neutral-700 rounded-md
                       focus:outline-none focus:ring-2 focus:ring-cyan-500/50"
              data-ui-id="workflow-builder-script-picker-category-select"
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

        {/* Script List */}
        <div className="flex-1 overflow-y-auto p-4">
          {isLoading ? (
            <div className="text-center py-8 text-muted-foreground">Loading scripts...</div>
          ) : filteredScripts.length === 0 ? (
            <div className="text-center py-8 text-muted-foreground">
              {scripts.length === 0
                ? "No Playwright scripts saved yet. Create scripts in the Playwright Runner first."
                : "No scripts match your search."}
            </div>
          ) : (
            <div className="grid grid-cols-1 gap-2">
              {filteredScripts.map((script) => {
                const isSelected = selectedId === script.id;

                return (
                  <button
                    key={script.id}
                    onClick={() => setSelectedId(script.id)}
                    onDoubleClick={() => handleDoubleClick(script)}
                    className={`
                      relative flex items-start gap-3 p-3 rounded-lg text-left transition-all
                      ${
                        isSelected
                          ? "bg-cyan-500/15 border-2 border-cyan-500"
                          : "bg-neutral-800/50 border-2 border-transparent hover:bg-neutral-800 hover:border-neutral-600"
                      }
                    `}
                    data-ui-id={`workflow-builder-script-picker-item-${script.id}`}
                  >
                    {isSelected && (
                      <div className="absolute top-2 right-2">
                        <CheckIcon className="w-4 h-4 text-cyan-400" />
                      </div>
                    )}
                    <div className="p-2 rounded-md bg-neutral-900 text-green-400">
                      <TestTube2 className="w-4 h-4" />
                    </div>
                    <div className="flex-1 min-w-0">
                      <div className="flex items-center gap-2">
                        <span className="font-medium">{script.name}</span>
                        {script.headless === false && (
                          <span className="text-xs px-1.5 py-0.5 bg-blue-500/20 text-blue-400 rounded">
                            headed
                          </span>
                        )}
                      </div>
                      {script.description && (
                        <p className="text-sm text-muted-foreground mt-0.5 line-clamp-1">
                          {script.description}
                        </p>
                      )}
                      <div className="flex items-center gap-3 mt-1.5 text-xs text-muted-foreground">
                        {script.target_url && (
                          <span className="flex items-center gap-1 truncate max-w-[200px]">
                            <Globe className="w-3 h-3 flex-shrink-0" />
                            {script.target_url}
                          </span>
                        )}
                        {script.category && (
                          <>
                            <span className="text-neutral-600">|</span>
                            <span className="flex items-center gap-1">
                              <FolderOpen className="w-3 h-3" />
                              {script.category}
                            </span>
                          </>
                        )}
                        {script.timeout_seconds && (
                          <>
                            <span className="text-neutral-600">|</span>
                            <span className="flex items-center gap-1">
                              <Clock className="w-3 h-3" />
                              {script.timeout_seconds}s
                            </span>
                          </>
                        )}
                        {script.browser && (
                          <>
                            <span className="text-neutral-600">|</span>
                            <span className="capitalize">{script.browser}</span>
                          </>
                        )}
                      </div>
                      {/* Show tags */}
                      {script.tags && script.tags.length > 0 && (
                        <div className="mt-2 flex flex-wrap gap-1">
                          {script.tags.slice(0, 5).map((tag) => (
                            <span
                              key={tag}
                              className="text-xs px-1.5 py-0.5 bg-neutral-700 rounded flex items-center gap-1"
                            >
                              <Tag className="w-2.5 h-2.5" />
                              {tag}
                            </span>
                          ))}
                          {script.tags.length > 5 && (
                            <span className="text-xs px-1.5 py-0.5 text-muted-foreground">
                              +{script.tags.length - 5} more
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
        <div className="flex items-center justify-between px-4 py-3 border-t border-neutral-700 bg-neutral-800/50">
          <span
            data-content-role="metric"
            data-content-label="available script count"
            className="text-sm text-muted-foreground"
          >
            {filteredScripts.length} script{filteredScripts.length !== 1 ? "s" : ""} available
          </span>
          <div className="flex items-center gap-2">
            <button
              onClick={onClose}
              className="px-4 py-2 text-sm rounded-md hover:bg-neutral-700 transition-colors"
              data-ui-id="workflow-builder-script-picker-cancel-btn"
            >
              Cancel
            </button>
            <button
              onClick={handleSelect}
              disabled={!selectedId}
              className="px-4 py-2 text-sm bg-cyan-600 hover:bg-cyan-700 text-white rounded-md
                       transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
              data-ui-id="workflow-builder-script-picker-add-btn"
            >
              Add to Workflow
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}

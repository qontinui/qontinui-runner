/**
 * State Library Picker
 *
 * Modal dialog for selecting a state from the state machine to navigate to
 * as a workflow step.
 */

import { useState, useEffect, useCallback } from "react";
import { Search, X, Check as CheckIcon, Navigation, Tag, AlertCircle } from "lucide-react";
import type { WorkflowPhase } from "../../types/unified-workflow";

// State type from the state machine
interface State {
  id: string;
  name: string;
  description?: string;
  tags?: string[];
  is_initial?: boolean;
  is_terminal?: boolean;
}

interface StateLibraryPickerProps {
  isOpen: boolean;
  onClose: () => void;
  onSelect: (state: State, phase: WorkflowPhase) => void;
  phase: WorkflowPhase;
}

const API_BASE = "http://localhost:9876";

export function StateLibraryPicker({ isOpen, onClose, onSelect, phase }: StateLibraryPickerProps) {
  const [states, setStates] = useState<State[]>([]);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [searchQuery, setSearchQuery] = useState("");
  const [isLoading, setIsLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Fetch states from API
  useEffect(() => {
    if (!isOpen) return;

    const controller = new AbortController();
    let cancelled = false;

    const fetchStates = async () => {
      setIsLoading(true);
      setError(null);
      try {
        const response = await fetch(`${API_BASE}/testing/states`, {
          signal: controller.signal,
        });
        const result = await response.json();
        if (!cancelled) {
          if (result.success && result.data?.states) {
            setStates(result.data.states);
          } else if (result.states) {
            setStates(result.states);
          } else if (Array.isArray(result)) {
            setStates(result);
          } else {
            setStates([]);
            if (!result.success) {
              setError(result.error || "Failed to load states");
            }
          }
        }
      } catch (err) {
        if (!cancelled) {
          console.error("Failed to fetch states:", err);
          setError("Failed to connect to the runner. Make sure a config is loaded.");
        }
      } finally {
        if (!cancelled) {
          setIsLoading(false);
        }
      }
    };

    fetchStates();

    return () => {
      cancelled = true;
      controller.abort();
    };
  }, [isOpen]);

  // Reset selection when modal opens
  useEffect(() => {
    if (isOpen) {
      setSelectedId(null);
      setSearchQuery("");
    }
  }, [isOpen]);

  // Filter states
  const filteredStates = states.filter((state) => {
    const matchesSearch =
      !searchQuery ||
      state.name.toLowerCase().includes(searchQuery.toLowerCase()) ||
      state.description?.toLowerCase().includes(searchQuery.toLowerCase()) ||
      state.id.toLowerCase().includes(searchQuery.toLowerCase());

    return matchesSearch;
  });

  const handleSelect = useCallback(() => {
    const selected = states.find((s) => s.id === selectedId);
    if (selected) {
      onSelect(selected, phase);
      onClose();
    }
  }, [states, selectedId, onSelect, phase, onClose]);

  const handleDoubleClick = useCallback(
    (state: State) => {
      onSelect(state, phase);
      onClose();
    },
    [onSelect, phase, onClose],
  );

  if (!isOpen) return null;

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60">
      <div
        data-ui-id="dialog-state-library-picker"
        className="bg-neutral-900 border border-neutral-700 rounded-lg shadow-2xl w-[700px] max-w-[90vw] max-h-[80vh] flex flex-col"
      >
        {/* Header */}
        <div className="flex items-center justify-between px-4 py-3 border-b border-neutral-700">
          <div>
            <h2 className="text-lg font-semibold">Select State to Navigate</h2>
            <p className="text-sm text-muted-foreground">
              Choose a state to navigate to as a {phase} step
            </p>
          </div>
          <button
            onClick={onClose}
            className="p-1.5 hover:bg-neutral-700 rounded-md transition-colors"
            data-ui-id="workflow-builder-state-picker-close-btn"
          >
            <X className="w-5 h-5" />
          </button>
        </div>

        {/* Search */}
        <div className="flex items-center gap-3 px-4 py-3 border-b border-neutral-800">
          <div className="relative flex-1">
            <Search className="absolute left-3 top-1/2 -translate-y-1/2 w-4 h-4 text-muted-foreground" />
            <input
              type="text"
              placeholder="Search states..."
              value={searchQuery}
              onChange={(e) => setSearchQuery(e.target.value)}
              className="w-full pl-9 pr-3 py-2 text-sm bg-neutral-800 border border-neutral-700 rounded-md
                       focus:outline-none focus:ring-2 focus:ring-cyan-500/50"
              autoFocus
              data-ui-id="workflow-builder-state-picker-search-input"
            />
          </div>
        </div>

        {/* State List */}
        <div className="flex-1 overflow-y-auto p-4">
          {isLoading ? (
            <div className="text-center py-8 text-muted-foreground">Loading states...</div>
          ) : error ? (
            <div className="text-center py-8">
              <AlertCircle className="w-8 h-8 text-amber-400 mx-auto mb-2" />
              <p className="text-muted-foreground">{error}</p>
              <p className="text-xs text-muted-foreground mt-2">
                Load a workflow config file to access states from the state machine.
              </p>
            </div>
          ) : filteredStates.length === 0 ? (
            <div className="text-center py-8 text-muted-foreground">
              {states.length === 0
                ? "No states found. Load a config file with a state machine first."
                : "No states match your search."}
            </div>
          ) : (
            <div className="grid grid-cols-1 gap-2">
              {filteredStates.map((state) => {
                const isSelected = selectedId === state.id;

                return (
                  <button
                    key={state.id}
                    onClick={() => setSelectedId(state.id)}
                    onDoubleClick={() => handleDoubleClick(state)}
                    className={`
                      relative flex items-start gap-3 p-3 rounded-lg text-left transition-all
                      ${
                        isSelected
                          ? "bg-cyan-500/15 border-2 border-cyan-500"
                          : "bg-neutral-800/50 border-2 border-transparent hover:bg-neutral-800 hover:border-neutral-600"
                      }
                    `}
                    data-ui-id={`workflow-builder-state-picker-item-${state.id}`}
                  >
                    {isSelected && (
                      <div className="absolute top-2 right-2">
                        <CheckIcon className="w-4 h-4 text-cyan-400" />
                      </div>
                    )}
                    <div className="p-2 rounded-md bg-neutral-900 text-purple-400">
                      <Navigation className="w-4 h-4" />
                    </div>
                    <div className="flex-1 min-w-0">
                      <div className="flex items-center gap-2">
                        <span className="font-medium">{state.name}</span>
                        {state.is_initial && (
                          <span className="text-xs px-1.5 py-0.5 bg-green-500/20 text-green-400 rounded">
                            initial
                          </span>
                        )}
                        {state.is_terminal && (
                          <span className="text-xs px-1.5 py-0.5 bg-red-500/20 text-red-400 rounded">
                            terminal
                          </span>
                        )}
                      </div>
                      {state.description && (
                        <p className="text-sm text-muted-foreground mt-0.5 line-clamp-1">
                          {state.description}
                        </p>
                      )}
                      <div className="flex items-center gap-2 mt-1.5 text-xs text-muted-foreground">
                        <span className="font-mono text-neutral-500">{state.id}</span>
                      </div>
                      {/* Show tags */}
                      {state.tags && state.tags.length > 0 && (
                        <div className="mt-2 flex flex-wrap gap-1">
                          {state.tags.slice(0, 5).map((tag) => (
                            <span
                              key={tag}
                              className="text-xs px-1.5 py-0.5 bg-neutral-700 rounded flex items-center gap-1"
                            >
                              <Tag className="w-2.5 h-2.5" />
                              {tag}
                            </span>
                          ))}
                          {state.tags.length > 5 && (
                            <span className="text-xs px-1.5 py-0.5 text-muted-foreground">
                              +{state.tags.length - 5} more
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
            data-content-label="available state count"
            className="text-sm text-muted-foreground"
          >
            {filteredStates.length} state{filteredStates.length !== 1 ? "s" : ""} available
          </span>
          <div className="flex items-center gap-2">
            <button
              onClick={onClose}
              className="px-4 py-2 text-sm rounded-md hover:bg-neutral-700 transition-colors"
              data-ui-id="workflow-builder-state-picker-cancel-btn"
            >
              Cancel
            </button>
            <button
              onClick={handleSelect}
              disabled={!selectedId}
              className="px-4 py-2 text-sm bg-cyan-600 hover:bg-cyan-700 text-white rounded-md
                       transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
              data-ui-id="workflow-builder-state-picker-add-btn"
            >
              Add to Workflow
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}

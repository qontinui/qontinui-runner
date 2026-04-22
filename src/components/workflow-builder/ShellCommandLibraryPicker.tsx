/**
 * ShellCommandLibraryPicker.tsx
 *
 * Modal dialog for selecting a saved shell command from the library.
 * Used when adding a shell command step "From Library".
 */

import { useState, useEffect, useMemo } from "react";
import { Terminal, Search, X, Loader2, Check } from "lucide-react";
import { invoke } from "@tauri-apps/api/core";
import type { SavedShellCommand, WorkflowPhase } from "../../types";
import { getAccentColors } from "@/design-system";

interface ShellCommandLibraryPickerProps {
  /** Whether the picker is open */
  isOpen: boolean;
  /** Called when the picker is closed */
  onClose: () => void;
  /** Called when a shell command is selected */
  onSelect: (command: SavedShellCommand, phase: WorkflowPhase) => void;
  /** The phase for the new step */
  phase: WorkflowPhase;
}

export function ShellCommandLibraryPicker({
  isOpen,
  onClose,
  onSelect,
  phase,
}: ShellCommandLibraryPickerProps) {
  const [savedCommands, setSavedCommands] = useState<SavedShellCommand[]>([]);
  const [isLoading, setIsLoading] = useState(false);
  const [searchQuery, setSearchQuery] = useState("");
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [categoryFilter, setCategoryFilter] = useState<string>("");

  // Fetch saved shell commands via Tauri IPC
  useEffect(() => {
    if (!isOpen) return;

    const fetchCommands = async () => {
      setIsLoading(true);
      try {
        const response = await invoke<{
          success: boolean;
          data?: SavedShellCommand[];
          error?: string;
        }>("list_shell_commands", { enabledOnly: false, category: null });
        if (response.success && response.data) {
          setSavedCommands(response.data);
        }
      } catch (error) {
        console.error("Failed to fetch saved shell commands:", error);
      } finally {
        setIsLoading(false);
      }
    };

    fetchCommands();

    setSearchQuery("");
    setSelectedId(null);
    setCategoryFilter("");
  }, [isOpen]);

  // Get unique categories
  const categories = useMemo(() => {
    const cats = new Set<string>();
    savedCommands.forEach((c) => {
      if (c.category) cats.add(c.category);
    });
    return Array.from(cats).sort();
  }, [savedCommands]);

  // Filter commands
  const filteredCommands = useMemo(() => {
    return savedCommands.filter((c) => {
      const matchesSearch =
        !searchQuery ||
        c.name.toLowerCase().includes(searchQuery.toLowerCase()) ||
        c.command.toLowerCase().includes(searchQuery.toLowerCase()) ||
        c.description?.toLowerCase().includes(searchQuery.toLowerCase());

      const matchesCategory = !categoryFilter || c.category === categoryFilter;

      return matchesSearch && matchesCategory;
    });
  }, [savedCommands, searchQuery, categoryFilter]);

  // Handle selection
  const handleSelect = () => {
    const selected = savedCommands.find((c) => c.id === selectedId);
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
        aria-label="Close shell command picker"
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
            <Terminal className={`w-5 h-5 ${getAccentColors("violet").text}`} />
            <h3 className="text-lg font-semibold">Select Shell Command</h3>
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
              placeholder="Search by name or command..."
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
          ) : filteredCommands.length === 0 ? (
            <div className="flex flex-col items-center justify-center h-40 text-muted-foreground">
              <Terminal className="w-8 h-8 mb-2 opacity-50" />
              <p className="text-sm">
                {searchQuery ? "No matching shell commands found" : "No saved shell commands yet"}
              </p>
              <p className="text-xs mt-1">
                {searchQuery
                  ? "Try a different search term"
                  : "Create shell commands in the Shell Commands library"}
              </p>
            </div>
          ) : (
            <div className="grid grid-cols-1 gap-2">
              {filteredCommands.map((cmd) => (
                <button
                  key={cmd.id}
                  onClick={() => setSelectedId(cmd.id)}
                  onDoubleClick={() => {
                    setSelectedId(cmd.id);
                    handleSelect();
                  }}
                  className={`w-full text-left p-3 rounded-lg border transition-colors ${
                    selectedId === cmd.id
                      ? "border-primary bg-primary/5"
                      : "border-border hover:border-muted-foreground hover:bg-muted/50"
                  }`}
                >
                  <div className="flex items-start gap-3">
                    <Terminal
                      className={`w-4 h-4 mt-0.5 shrink-0 ${getAccentColors("violet").text}`}
                    />
                    <div className="flex-1 min-w-0">
                      <div className="flex items-center gap-2">
                        <span className="font-medium text-sm">{cmd.name}</span>
                        {cmd.category && (
                          <span className="px-1.5 py-0.5 text-xs bg-zinc-700 text-zinc-400 rounded">
                            {cmd.category}
                          </span>
                        )}
                        {!cmd.enabled && (
                          <span className="px-1.5 py-0.5 text-xs bg-yellow-500/20 text-yellow-400 rounded">
                            Disabled
                          </span>
                        )}
                        {selectedId === cmd.id && (
                          <Check className="w-4 h-4 text-primary shrink-0" />
                        )}
                      </div>
                      {cmd.description && (
                        <div className="text-xs text-muted-foreground mt-0.5 line-clamp-1">
                          {cmd.description}
                        </div>
                      )}
                      <div className="text-xs text-zinc-500 font-mono mt-1 line-clamp-1 bg-zinc-800/50 px-2 py-1 rounded">
                        {cmd.command}
                      </div>
                      <div className="flex items-center gap-3 mt-1.5 text-xs text-zinc-500">
                        <span>Timeout: {cmd.timeout_seconds}s</span>
                        <span>{cmd.fail_on_error ? "Fails on error" : "Continues on error"}</span>
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
            className={`flex-1 flex items-center justify-center gap-2 px-4 py-2 ${getAccentColors("violet").bgSolid} text-white rounded-md font-medium hover:opacity-90 disabled:opacity-50 disabled:cursor-not-allowed transition-colors`}
          >
            <Check className="w-4 h-4" />
            Select
          </button>
        </div>
      </div>
    </div>
  );
}

export default ShellCommandLibraryPicker;

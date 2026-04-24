import { TestTube, Loader2, Search, X, Check } from "lucide-react";
import { BuilderToolbar, toolbarActions } from "../ui/BuilderToolbar";
import type { PlaywrightScript } from "./types";

interface ScriptListPanelProps {
  scripts: PlaywrightScript[];
  filteredScripts: PlaywrightScript[];
  loading: boolean;
  searchQuery: string;
  onSearchChange: (query: string) => void;
  editingScriptId: string | undefined;
  onSelectScript: (script: PlaywrightScript) => void;
  onStartCreate: () => void;
  isSelectionMode: boolean;
  onToggleSelectionMode: () => void;
  selectedIds: Set<string>;
  onToggleSelection: (id: string) => void;
  onSelectAll: () => void;
  onClearSelection: () => void;
  onDeleteSelected: () => void;
  onExitSelectionMode: () => void;
  accentColor: string;
}

export function ScriptListPanel({
  filteredScripts,
  loading,
  searchQuery,
  onSearchChange,
  editingScriptId,
  onSelectScript,
  onStartCreate,
  isSelectionMode,
  onToggleSelectionMode,
  selectedIds,
  onToggleSelection,
  onSelectAll,
  onClearSelection,
  onDeleteSelected,
  onExitSelectionMode,
  accentColor,
}: ScriptListPanelProps) {
  return (
    <div className="w-80 border-r border-border flex flex-col bg-card">
      <div className="p-4 border-b border-border">
        <div className="flex items-center justify-between mb-2">
          <h2 className="text-lg font-semibold flex items-center gap-2">
            <TestTube className="w-5 h-5" style={{ color: accentColor }} />
            Scripts
          </h2>
        </div>

        <BuilderToolbar
          className="mb-3"
          actions={[
            toolbarActions.aiPlaceholder(),
            toolbarActions.editInWeb("/build/playwright-tests"),
            toolbarActions.delete(onToggleSelectionMode, isSelectionMode),
            toolbarActions.new(onStartCreate, "New"),
          ]}
        />

        <div className="relative">
          <Search className="absolute left-3 top-1/2 -translate-y-1/2 w-4 h-4 text-muted-foreground" />
          <input
            id="script-search"
            type="text"
            aria-label="Search scripts"
            placeholder="Search scripts..."
            value={searchQuery}
            onChange={(e) => onSearchChange(e.target.value)}
            className="w-full pl-9 pr-3 py-2 bg-muted border border-border rounded-lg text-sm focus:outline-hidden focus:border-primary"
          />
        </div>
      </div>

      {isSelectionMode && (
        <div className="flex items-center justify-between px-4 py-2 bg-red-500/10 border-b border-red-500/30">
          <span className="text-sm text-red-400">{selectedIds.size} selected</span>
          <div className="flex items-center gap-2">
            <button
              onClick={onSelectAll}
              className="px-2 py-1 text-xs bg-muted/80 hover:bg-muted text-foreground rounded"
            >
              Select All
            </button>
            <button
              onClick={onClearSelection}
              disabled={selectedIds.size === 0}
              className="px-2 py-1 text-xs bg-muted/80 hover:bg-muted text-foreground rounded disabled:opacity-50"
            >
              Clear
            </button>
            <button
              onClick={onDeleteSelected}
              disabled={selectedIds.size === 0}
              className="px-2 py-1 text-xs bg-red-600 hover:bg-red-500 text-white rounded disabled:opacity-50 disabled:cursor-not-allowed"
            >
              Delete Selected
            </button>
            <button
              onClick={onExitSelectionMode}
              className="p-1 text-muted-foreground hover:text-foreground"
              title="Cancel selection"
            >
              <X className="w-4 h-4" />
            </button>
          </div>
        </div>
      )}

      <div className="flex-1 overflow-y-auto p-2">
        {loading ? (
          <div className="flex items-center justify-center py-8">
            <Loader2 className="w-6 h-6 animate-spin text-muted-foreground" />
          </div>
        ) : filteredScripts.length === 0 ? (
          <div className="text-center py-8 text-muted-foreground">
            <TestTube className="w-8 h-8 mx-auto mb-2 opacity-50" />
            <p className="text-sm">No scripts found</p>
          </div>
        ) : (
          <div className="space-y-1">
            {filteredScripts.map((script) => (
              <button
                key={script.id}
                onClick={() =>
                  isSelectionMode ? onToggleSelection(script.id) : onSelectScript(script)
                }
                className={`w-full text-left p-3 rounded-lg transition-colors ${
                  editingScriptId === script.id ? "bg-muted/80" : "hover:bg-muted"
                } ${isSelectionMode && selectedIds.has(script.id) ? "bg-red-500/10" : ""}`}
              >
                <div className="flex items-center gap-2">
                  {isSelectionMode && (
                    <div
                      className={`w-4 h-4 rounded border shrink-0 flex items-center justify-center ${
                        selectedIds.has(script.id)
                          ? "bg-red-500 border-red-500"
                          : "border-muted-foreground hover:border-red-400"
                      }`}
                    >
                      {selectedIds.has(script.id) && <Check className="w-3 h-3 text-white" />}
                    </div>
                  )}
                  <div className="flex-1 min-w-0">
                    <div className="font-medium text-sm truncate">{script.name}</div>
                    {script.target_url && (
                      <div className="text-xs text-muted-foreground truncate mt-0.5">
                        {script.target_url}
                      </div>
                    )}
                    {script.category && (
                      <span className="text-xs px-1.5 py-0.5 rounded bg-muted text-muted-foreground mt-1.5 inline-block">
                        {script.category}
                      </span>
                    )}
                  </div>
                </div>
              </button>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}

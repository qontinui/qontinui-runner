/**
 * MacrosPanel Component
 *
 * Side panel showing saved macros with load/delete options.
 */

import { X, Trash2, Play, FolderOpen, Clock } from "lucide-react";
import { useMacroBuilder } from "./MacroBuilderContext";
import type { SavedMacro } from "../../types/macro";

interface MacrosPanelProps {
  className?: string;
}

function formatDate(dateString: string): string {
  const date = new Date(dateString);
  return date.toLocaleDateString(undefined, {
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });
}

export function MacrosPanel({ className }: MacrosPanelProps) {
  const { savedMacros, currentMacroId, loadMacro, deleteMacro, setShowMacrosPanel } =
    useMacroBuilder();

  const handleDelete = async (e: React.MouseEvent, macro: SavedMacro) => {
    e.stopPropagation();
    if (confirm(`Are you sure you want to delete "${macro.name}"?`)) {
      try {
        await deleteMacro(macro.id);
      } catch (error) {
        console.error("Failed to delete macro:", error);
      }
    }
  };

  return (
    <div className={`flex flex-col border-l border-border bg-background h-full ${className || ""}`}>
      {/* Header */}
      <div className="flex items-center justify-between p-4 border-b border-border">
        <h3 className="font-semibold flex items-center gap-2">
          <FolderOpen className="h-4 w-4" />
          Saved Macros
        </h3>
        <button
          className="p-1.5 rounded hover:bg-muted text-muted-foreground hover:text-foreground transition-colors"
          onClick={() => setShowMacrosPanel(false)}
        >
          <X className="h-4 w-4" />
        </button>
      </div>

      {/* Macros List */}
      <div className="flex-1 overflow-y-auto">
        <div className="p-2 space-y-1">
          {savedMacros.length === 0 ? (
            <div className="text-sm text-muted-foreground text-center py-8">
              No saved macros yet
            </div>
          ) : (
            savedMacros.map((macro) => (
              <div
                key={macro.id}
                className={`group p-3 rounded-lg border cursor-pointer transition-colors ${
                  currentMacroId === macro.id
                    ? "border-primary bg-primary/5"
                    : "border-transparent hover:border-border hover:bg-muted/50"
                }`}
                onClick={() => loadMacro(macro)}
              >
                <div className="flex items-start justify-between gap-2">
                  <div className="flex-1 min-w-0">
                    <div className="font-medium truncate">{macro.name}</div>
                    {macro.description && (
                      <div className="text-sm text-muted-foreground truncate">
                        {macro.description}
                      </div>
                    )}
                    <div className="flex items-center gap-3 mt-1 text-xs text-muted-foreground">
                      <span>{macro.steps.length} steps</span>
                      {macro.run_count > 0 && (
                        <span className="flex items-center gap-1">
                          <Play className="h-3 w-3" />
                          {macro.run_count}
                        </span>
                      )}
                    </div>
                    <div className="flex items-center gap-1 mt-1 text-xs text-muted-foreground">
                      <Clock className="h-3 w-3" />
                      {formatDate(macro.modified_at)}
                    </div>
                  </div>
                  <button
                    className="p-1.5 rounded hover:bg-muted text-destructive opacity-0 group-hover:opacity-100 transition-opacity"
                    onClick={(e) => handleDelete(e, macro)}
                  >
                    <Trash2 className="h-3.5 w-3.5" />
                  </button>
                </div>
              </div>
            ))
          )}
        </div>
      </div>
    </div>
  );
}

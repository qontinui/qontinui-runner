/**
 * LogTabActions Component
 *
 * Renders action buttons for the active log tab (filter, clear, copy).
 * Single responsibility: Log tab action buttons UI.
 */

import { Filter, Trash2, Copy, Check } from "lucide-react";
import type { LogTab } from "../hooks/useUIState";
import type { LogLevel } from "../hooks/useLogFilter";

export interface LogTabActionsProps {
  activeTab: LogTab;

  // General tab actions
  showLogFilter: boolean;
  onToggleLogFilter: (show: boolean) => void;
  logLevel: LogLevel;
  onLogLevelChange: (level: LogLevel) => void;
  onClearGeneralLogs: () => void;

  // Image tab actions
  onClearImageLogs: () => void;

  // Actions tab actions
  onClearActionLogs: () => void;

  // Common actions
  onClearAllLogs: () => void;
  onCopyLogs: () => void;
  copySuccess: boolean;
}

const LOG_LEVELS: LogLevel[] = ["all", "info", "warning", "error", "debug", "success"];

export function LogTabActions({
  activeTab,
  showLogFilter,
  onToggleLogFilter,
  logLevel,
  onLogLevelChange,
  onClearGeneralLogs,
  onClearImageLogs,
  onClearActionLogs,
  onClearAllLogs,
  onCopyLogs,
  copySuccess,
}: LogTabActionsProps) {
  return (
    <div className="flex items-center gap-2 px-4">
      {/* General Tab Actions */}
      {activeTab === "general" && (
        <>
          <div className="relative">
            <button
              onClick={() => onToggleLogFilter(!showLogFilter)}
              className="p-2 hover:bg-accent rounded-lg transition-colors"
            >
              <Filter className="w-4 h-4" />
            </button>
            {showLogFilter && (
              <div className="absolute right-0 mt-1 bg-card border border-border rounded-lg shadow-lg z-10">
                {LOG_LEVELS.map((level) => (
                  <button
                    key={level}
                    onClick={() => {
                      onLogLevelChange(level);
                      onToggleLogFilter(false);
                    }}
                    className={`w-full px-4 py-2 text-left hover:bg-accent transition-colors ${
                      logLevel === level ? "bg-accent" : ""
                    }`}
                  >
                    {level.charAt(0).toUpperCase() + level.slice(1)}
                  </button>
                ))}
              </div>
            )}
          </div>
          <button
            onClick={onClearGeneralLogs}
            className="p-2 hover:bg-accent rounded-lg transition-colors"
            title="Clear logs"
          >
            <Trash2 className="w-4 h-4" />
          </button>
        </>
      )}

      {/* Image Tab Actions */}
      {activeTab === "image" && (
        <button
          onClick={onClearImageLogs}
          className="p-2 hover:bg-accent rounded-lg transition-colors"
          title="Clear image logs"
        >
          <Trash2 className="w-4 h-4" />
        </button>
      )}

      {/* Actions Tab Actions */}
      {activeTab === "actions" && (
        <button
          onClick={onClearActionLogs}
          className="p-2 hover:bg-accent rounded-lg transition-colors"
          title="Clear action logs"
        >
          <Trash2 className="w-4 h-4" />
        </button>
      )}

      {/* Clear All button - visible on all tabs */}
      <button
        onClick={onClearAllLogs}
        className="p-2 hover:bg-destructive hover:text-destructive-foreground rounded-lg transition-colors flex items-center"
        title="Clear all logs (General, Image, and Actions)"
      >
        <Trash2 className="w-4 h-4" />
        <Trash2 className="w-4 h-4 -ml-1" />
      </button>

      <button
        onClick={onCopyLogs}
        className="p-2 hover:bg-accent rounded-lg transition-colors"
        title="Copy to clipboard"
      >
        {copySuccess ? <Check className="w-4 h-4 text-green-600" /> : <Copy className="w-4 h-4" />}
      </button>
    </div>
  );
}

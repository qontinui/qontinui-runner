/**
 * SettingsPanel.tsx
 *
 * Settings panel with log sources and auto-continue toggle.
 */

import {
  CheckCircle,
  Info,
  Loader2,
  RotateCcw,
  Settings,
  ToggleLeft,
  ToggleRight,
} from "lucide-react";
import { useAiBuilder } from "./AiBuilderContext";

export function SettingsPanel() {
  const {
    projectLogs,
    onNavigateToLogLocations,
    autoContinueEnabled,
    autoContinueLoading,
    toggleAutoContinue,
  } = useAiBuilder();

  return (
    <div className="card p-4 space-y-3">
      <div className="flex items-center gap-2">
        <Settings className="w-4 h-4 text-muted-foreground" />
        <span className="font-medium">Settings</span>
      </div>

      {/* Project Logs Status */}
      <div className="text-xs space-y-1">
        {projectLogs.config?.logSources &&
        projectLogs.config.logSources.filter((s) => s.enabled).length > 0 ? (
          <>
            <div className="flex items-center gap-2 text-green-600">
              <CheckCircle className="w-3 h-3" />
              <span>
                {projectLogs.config.logSources.filter((s) => s.enabled).length} log source(s) will
                be monitored
              </span>
            </div>
            <div className="text-muted-foreground pl-5">
              {projectLogs.config.logSources
                .filter((s) => s.enabled)
                .map((s) => s.name)
                .join(", ")}
            </div>
          </>
        ) : (
          <div className="flex items-center gap-2 text-yellow-600">
            <Info className="w-3 h-3" />
            <span>
              No log sources configured.{" "}
              <button
                onClick={onNavigateToLogLocations}
                className="underline hover:text-yellow-500 transition-colors"
              >
                Configure in Log Locations
              </button>
            </span>
          </div>
        )}
      </div>

      {/* Auto-Continue Toggle */}
      <div className="flex items-center justify-between p-2 bg-muted/30 rounded-md">
        <div className="flex items-center gap-2">
          <RotateCcw className="w-4 h-4 text-orange-500" />
          <div>
            <span className="text-sm font-medium">Auto-Continue on Restart</span>
            <p className="text-xs text-muted-foreground">
              {autoContinueEnabled
                ? "Workflows resume automatically after runner restart"
                : "Use the Continue button to resume after restart"}
            </p>
          </div>
        </div>
        <button
          onClick={toggleAutoContinue}
          disabled={autoContinueLoading}
          className={`flex items-center transition-colors ${autoContinueEnabled ? "text-orange-500" : "text-muted-foreground"} ${autoContinueLoading ? "opacity-50" : ""}`}
          title={autoContinueEnabled ? "Auto-continue enabled" : "Auto-continue disabled"}
        >
          {autoContinueLoading ? (
            <Loader2 className="w-8 h-8 animate-spin" />
          ) : autoContinueEnabled ? (
            <ToggleRight className="w-8 h-8" />
          ) : (
            <ToggleLeft className="w-8 h-8" />
          )}
        </button>
      </div>
    </div>
  );
}

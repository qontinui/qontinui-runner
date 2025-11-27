/**
 * LogTabNavigation Component
 *
 * Renders the tab navigation for log viewer (General, Image Recognition, Actions, Settings).
 * Single responsibility: Tab navigation UI.
 */

import { Terminal, Image, Zap, Settings as SettingsIcon } from "lucide-react";
import type { LogTab } from "../hooks/useUIState";

export interface LogTabNavigationProps {
  activeTab: LogTab;
  onTabChange: (tab: LogTab) => void;
  logCount: number;
  imageLogCount: number;
  actionCount: number;
}

export function LogTabNavigation({
  activeTab,
  onTabChange,
  logCount,
  imageLogCount,
  actionCount,
}: LogTabNavigationProps) {
  return (
    <div className="flex gap-1 p-1">
      <button
        onClick={() => onTabChange("general")}
        className={`px-4 py-2 rounded-lg flex items-center gap-2 transition-colors ${
          activeTab === "general"
            ? "bg-accent text-accent-foreground font-medium"
            : "text-muted-foreground hover:text-foreground"
        }`}
      >
        <Terminal className="w-4 h-4" />
        <span>General</span>
        {logCount > 0 && (
          <span className="ml-1 px-2 py-0.5 text-xs bg-primary text-primary-foreground rounded-full">
            {logCount}
          </span>
        )}
      </button>

      <button
        onClick={() => onTabChange("image")}
        className={`px-4 py-2 rounded-lg flex items-center gap-2 transition-colors ${
          activeTab === "image"
            ? "bg-accent text-accent-foreground font-medium"
            : "text-muted-foreground hover:text-foreground"
        }`}
      >
        <Image className="w-4 h-4" />
        <span>Image Recognition</span>
        {imageLogCount > 0 && (
          <span className="ml-1 px-2 py-0.5 text-xs bg-purple-600 text-white rounded-full">
            {imageLogCount}
          </span>
        )}
      </button>

      <button
        onClick={() => onTabChange("actions")}
        className={`px-4 py-2 rounded-lg flex items-center gap-2 transition-colors ${
          activeTab === "actions"
            ? "bg-accent text-accent-foreground font-medium"
            : "text-muted-foreground hover:text-foreground"
        }`}
      >
        <Zap className="w-4 h-4" />
        <span>Actions</span>
        {actionCount > 0 && (
          <span className="ml-1 px-2 py-0.5 text-xs bg-green-600 text-white rounded-full">
            {actionCount}
          </span>
        )}
      </button>

      <button
        onClick={() => onTabChange("settings")}
        className={`px-4 py-2 rounded-lg flex items-center gap-2 transition-colors ${
          activeTab === "settings"
            ? "bg-accent text-accent-foreground font-medium"
            : "text-muted-foreground hover:text-foreground"
        }`}
      >
        <SettingsIcon className="w-4 h-4" />
        <span>Settings</span>
      </button>
    </div>
  );
}

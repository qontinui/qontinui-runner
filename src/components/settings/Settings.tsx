/**
 * Settings.tsx
 *
 * Settings tab containing only passive configuration options.
 * Active operations (capture, storage cleanup) have been moved to dedicated tabs.
 */

import { useState, useEffect } from "react";
import { AuthConnectionSettings } from "./AuthConnectionSettings";
import { GeneralSettings } from "./GeneralSettings";
import { StorageSettings } from "./StorageSettings";
import { AdvancedSettings } from "./AdvancedSettings";
import { UpdateSettings } from "./UpdateSettings";
import { AiSettings } from "./AiSettings";
import { AgenticSettings } from "./AgenticSettings";
import { BackupSettings } from "./BackupSettings";
import { PlaywrightSettings } from "./PlaywrightSettings";
import { SelfHealingSettings } from "./SelfHealingSettings";
import { MobileSettings } from "./MobileSettings";
import { CloudRelaySettings } from "./CloudRelaySettings";
import { LogSourcesSettings } from "./LogSourcesSettings";
import { McpSettings } from "./McpSettings";
import { ExecutionVariablesSettings } from "./ExecutionVariablesSettings";
import type { Project, ConnectionInfo } from "../../types/auth";

interface WebSocketState {
  connected: boolean;
  connecting: boolean;
  error: string | null;
  connectionInfo: ConnectionInfo | null;
  connect: () => Promise<void>;
  disconnect: () => Promise<void>;
}

interface SettingsProps {
  /** Default tab to open. If provided, overrides localStorage persistence. */
  defaultTab?: string;
  onLog: (level: "info" | "warning" | "error" | "debug" | "success", message: string) => void;
  onDebugModeChange: (enabled: boolean) => void;
  projects: Project[];
  selectedProjectId: string | null;
  onProjectSelect: (projectId: string | null) => void;
  onLoadProjects: () => Promise<void>;
  webSocketState: WebSocketState;
}

type SettingsTab =
  | "account"
  | "ai"
  | "agentic"
  | "self-healing"
  | "playwright"
  | "mobile"
  | "cloud-relay"
  | "mcp"
  | "log-sources"
  | "execution-variables"
  | "general"
  | "storage"
  | "backup"
  | "advanced"
  | "updates";

const STORAGE_KEY = "qontinui-settings-active-tab";

const VALID_TABS = [
  "account",
  "ai",
  "agentic",
  "self-healing",
  "playwright",
  "mobile",
  "cloud-relay",
  "mcp",
  "log-sources",
  "execution-variables",
  "general",
  "storage",
  "backup",
  "advanced",
  "updates",
] as const;

export function Settings({
  defaultTab,
  onLog,
  onDebugModeChange,
  projects,
  selectedProjectId,
  onProjectSelect,
  onLoadProjects,
  webSocketState,
}: SettingsProps) {
  const [activeTab, setActiveTab] = useState<SettingsTab>(() => {
    // If defaultTab is provided and valid, use it
    if (defaultTab && (VALID_TABS as readonly string[]).includes(defaultTab)) {
      return defaultTab as SettingsTab;
    }
    // Otherwise load persisted tab on mount
    const stored = localStorage.getItem(STORAGE_KEY);
    // Handle migration from old "connection" tab name
    if (stored === "connection") {
      return "account";
    }
    if (stored && (VALID_TABS as readonly string[]).includes(stored)) {
      return stored as SettingsTab;
    }
    return "account";
  });

  // Sync with defaultTab prop when it changes (from navigation)
  useEffect(() => {
    if (defaultTab && (VALID_TABS as readonly string[]).includes(defaultTab)) {
      setActiveTab(defaultTab as SettingsTab);
    }
  }, [defaultTab]);

  // Persist active tab
  useEffect(() => {
    localStorage.setItem(STORAGE_KEY, activeTab);
  }, [activeTab]);

  const renderContent = () => {
    switch (activeTab) {
      case "account":
        return (
          <AuthConnectionSettings
            onLog={onLog}
            projects={projects}
            selectedProjectId={selectedProjectId}
            onProjectSelect={onProjectSelect}
            webSocketState={webSocketState}
            onLoadProjects={onLoadProjects}
          />
        );
      case "ai":
        return <AiSettings onLog={onLog} />;
      case "agentic":
        return <AgenticSettings onLog={onLog} />;
      case "self-healing":
        return <SelfHealingSettings onLog={onLog} />;
      case "playwright":
        return <PlaywrightSettings onLog={onLog} />;
      case "mobile":
        return <MobileSettings onLog={onLog} />;
      case "cloud-relay":
        return <CloudRelaySettings onLog={onLog} />;
      case "mcp":
        return <McpSettings onLog={onLog} />;
      case "log-sources":
        return <LogSourcesSettings onLog={onLog} />;
      case "execution-variables":
        return <ExecutionVariablesSettings onLog={onLog} />;
      case "general":
        return <GeneralSettings onLog={onLog} />;
      case "storage":
        return <StorageSettings onLog={onLog} />;
      case "backup":
        return <BackupSettings onLog={onLog} />;
      case "advanced":
        return <AdvancedSettings onLog={onLog} onDebugModeChange={onDebugModeChange} />;
      case "updates":
        return <UpdateSettings onLog={onLog} />;
      default:
        return null;
    }
  };

  return (
    <div className="h-full flex flex-col p-4 gap-3 overflow-hidden">
      <div className="flex-1 overflow-y-auto scrollbar-dark">{renderContent()}</div>
    </div>
  );
}

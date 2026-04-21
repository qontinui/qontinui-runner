/**
 * Settings.tsx
 *
 * Settings tab containing only passive configuration options.
 * Active operations (capture, storage cleanup) have been moved to dedicated tabs.
 */

import { useState, useEffect } from "react";
import { useUIComponent } from "@qontinui/ui-bridge";
import { instanceStorage } from "@/lib/instance-storage";
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
import { WorldStateVerifierSettings } from "./WorldStateVerifierSettings";
import { MobileSettings } from "./MobileSettings";
import { CloudRelaySettings } from "./CloudRelaySettings";
import { WebIntegrationSettings } from "./WebIntegrationSettings";
import { LogSourcesSettings } from "./LogSourcesSettings";
import { McpSettings } from "./McpSettings";
import { ExecutionVariablesSettings } from "./ExecutionVariablesSettings";
import { RunnerInstancesSettings } from "./RunnerInstancesSettings";
import { NotificationSettings } from "./NotificationSettings";
import { OtelSettings } from "./OtelSettings";
import { ContainerSettings } from "./ContainerSettings";
import { SecuritySettings } from "./SecuritySettings";
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
  /** Default tab to open. If provided, overrides instanceStorage persistence. */
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
  | "world-state-verifier"
  | "playwright"
  | "mobile"
  | "cloud-relay"
  | "web-integration"
  | "mcp"
  | "log-sources"
  | "execution-variables"
  | "notifications"
  | "general"
  | "storage"
  | "backup"
  | "instances"
  | "otel"
  | "containers"
  | "security"
  | "advanced"
  | "updates";

const STORAGE_KEY = "qontinui-settings-active-tab";

const VALID_TABS = [
  "account",
  "ai",
  "agentic",
  "self-healing",
  "world-state-verifier",
  "playwright",
  "mobile",
  "cloud-relay",
  "web-integration",
  "mcp",
  "log-sources",
  "execution-variables",
  "notifications",
  "general",
  "storage",
  "backup",
  "instances",
  "otel",
  "containers",
  "security",
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
    const stored = instanceStorage.getItem(STORAGE_KEY);
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
      // eslint-disable-next-line react-hooks/set-state-in-effect -- sync state from navigation prop
      setActiveTab(defaultTab as SettingsTab);
    }
  }, [defaultTab]);

  // Persist active tab
  useEffect(() => {
    instanceStorage.setItem(STORAGE_KEY, activeTab);
  }, [activeTab]);

  // UI Bridge: Component-level actions for AI control
  useUIComponent({
    id: "settings-panel",
    name: "Settings Panel",
    description: "Application settings with multiple configuration tabs",
    actions: [
      {
        id: "save",
        label: "Save Settings",
        handler: async () => {
          // Each settings sub-tab manages its own persistence independently.
          // Persist the currently active tab selection, then log confirmation.
          instanceStorage.setItem(STORAGE_KEY, activeTab);
          onLog(
            "info",
            `Settings tab "${activeTab}" preference saved. Note: individual setting values are saved within each tab's own Save button.`,
          );
        },
      },
      {
        id: "reset",
        label: "Reset Settings",
        handler: async () => {
          setActiveTab("account");
          onLog("info", "Settings reset to defaults");
        },
      },
    ],
  });

  const settingsContent = (() => {
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
      case "world-state-verifier":
        return <WorldStateVerifierSettings onLog={onLog} />;
      case "playwright":
        return <PlaywrightSettings onLog={onLog} />;
      case "mobile":
        return <MobileSettings onLog={onLog} />;
      case "cloud-relay":
        return <CloudRelaySettings onLog={onLog} />;
      case "web-integration":
        return <WebIntegrationSettings onLog={onLog} />;
      case "mcp":
        return <McpSettings onLog={onLog} />;
      case "log-sources":
        return <LogSourcesSettings onLog={onLog} />;
      case "execution-variables":
        return <ExecutionVariablesSettings onLog={onLog} />;
      case "notifications":
        return <NotificationSettings onLog={onLog} />;
      case "general":
        return <GeneralSettings onLog={onLog} />;
      case "storage":
        return <StorageSettings onLog={onLog} />;
      case "backup":
        return <BackupSettings onLog={onLog} />;
      case "instances":
        return <RunnerInstancesSettings onLog={onLog} />;
      case "otel":
        return <OtelSettings onLog={onLog} />;
      case "containers":
        return <ContainerSettings onLog={onLog} />;
      case "security":
        return <SecuritySettings onLog={onLog} />;
      case "advanced":
        return <AdvancedSettings onLog={onLog} onDebugModeChange={onDebugModeChange} />;
      case "updates":
        return <UpdateSettings onLog={onLog} />;
      default:
        return null;
    }
  })();

  return (
    <div className="h-full flex flex-col p-4 gap-3 overflow-hidden">
      <div className="flex-1 overflow-y-auto scrollbar-dark">{settingsContent}</div>
    </div>
  );
}

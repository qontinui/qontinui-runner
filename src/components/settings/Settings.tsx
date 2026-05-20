/**
 * Settings.tsx
 *
 * Settings tab containing only passive configuration options.
 * Active operations (capture, storage cleanup) have been moved to dedicated tabs.
 */

import { useState, useEffect } from "react";
import { useUIComponent } from "@qontinui/ui-bridge";
import { instanceStorage } from "@/lib/instance-storage";
import { GeneralSettings } from "./GeneralSettings";
import { StorageSettings } from "./StorageSettings";
import { AdvancedSettings } from "./AdvancedSettings";
import { UpdateSettings } from "./UpdateSettings";
import { AiSettings } from "./AiSettings";
import { AgenticSettings } from "./AgenticSettings";
import { BackupSettings } from "./BackupSettings";
import { DiscoverySettings } from "./DiscoverySettings";
import { PlaywrightSettings } from "./PlaywrightSettings";
import { SelfHealingSettings } from "./SelfHealingSettings";
import { WorldStateVerifierSettings } from "./WorldStateVerifierSettings";
import { MobileSettings } from "./MobileSettings";
import { WebIntegrationSettings } from "./WebIntegrationSettings";
import { LogSourcesSettings } from "./LogSourcesSettings";
import { McpSettings } from "./McpSettings";
import { ExecutionVariablesSettings } from "./ExecutionVariablesSettings";
import { RunnerInstancesSettings } from "./RunnerInstancesSettings";
import { NotificationSettings } from "./NotificationSettings";
import { OtelSettings } from "./OtelSettings";
import { ContainerSettings } from "./ContainerSettings";
import { LockYieldPolicySettings } from "./LockYieldPolicySettings";
import { SecuritySettings } from "./SecuritySettings";
import { AccountSettings } from "./AccountSettings";

interface SettingsProps {
  /** Default tab to open. If provided, overrides instanceStorage persistence. */
  defaultTab?: string;
  onLog: (level: "info" | "warning" | "error" | "debug" | "success", message: string) => void;
  onDebugModeChange: (enabled: boolean) => void;
}

type SettingsTab =
  | "account"
  | "ai"
  | "agentic"
  | "self-healing"
  | "world-state-verifier"
  | "playwright"
  | "mobile"
  | "discovery"
  | "backend-connection"
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
  | "lock-yield"
  | "advanced"
  | "updates";

const STORAGE_KEY = "qontinui-settings-active-tab";

/**
 * Canonical list of settings sub-tabs.
 *
 * The runner ↔ web channel is a single outbound WebSocket driven by
 * `WebIntegrationSettings`. The panel id is `backend-connection` to
 * reflect the unified scope, but we still mount `WebIntegrationSettings`
 * underneath until the next UX pass.
 */
const SETTINGS_TABS = [
  { id: "account", label: "Account" },
  { id: "backend-connection", label: "Backend Connection" },
  { id: "ai", label: "AI Providers" },
  { id: "agentic", label: "Advanced AI" },
  { id: "self-healing", label: "Self-Healing" },
  { id: "world-state-verifier", label: "World State Verifier" },
  { id: "playwright", label: "Playwright" },
  { id: "mobile", label: "Mobile" },
  { id: "discovery", label: "Discovery" },
  { id: "mcp", label: "MCP Servers" },
  { id: "log-sources", label: "Log Sources" },
  { id: "execution-variables", label: "Execution Variables" },
  { id: "notifications", label: "Notifications" },
  { id: "general", label: "General" },
  { id: "storage", label: "Storage" },
  { id: "backup", label: "Backup" },
  { id: "instances", label: "Runner Instances" },
  { id: "otel", label: "OpenTelemetry" },
  { id: "containers", label: "Container Isolation" },
  { id: "security", label: "Security" },
  { id: "lock-yield", label: "Lock-yield Policy" },
  { id: "advanced", label: "Debug" },
  { id: "updates", label: "Updates" },
] as const satisfies ReadonlyArray<{ id: SettingsTab; label: string }>;

const VALID_TABS = SETTINGS_TABS.map((t) => t.id);

function migrateStoredTab(stored: string | null): SettingsTab {
  if (!stored) return "backend-connection";
  if ((VALID_TABS as readonly string[]).includes(stored)) {
    return stored as SettingsTab;
  }
  return "backend-connection";
}

export function Settings({ defaultTab, onLog, onDebugModeChange }: SettingsProps) {
  const [activeTab, setActiveTab] = useState<SettingsTab>(() => {
    if (defaultTab && (VALID_TABS as readonly string[]).includes(defaultTab)) {
      return defaultTab as SettingsTab;
    }
    return migrateStoredTab(instanceStorage.getItem(STORAGE_KEY));
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
          setActiveTab("backend-connection");
          onLog("info", "Settings reset to defaults");
        },
      },
      {
        id: "switch-tab",
        label: "Switch settings tab",
        description: "Select a settings sub-tab by id (use list-tabs to enumerate).",
        paramSchema: { tabId: "string (one of the ids from list-tabs)" },
        handler: (params?: unknown) => {
          const { tabId } = (params ?? {}) as { tabId?: string };
          if (!tabId || typeof tabId !== "string") {
            throw new Error("switch-tab requires { tabId: string }");
          }
          const tab = SETTINGS_TABS.find((t) => t.id === tabId);
          if (!tab) {
            throw new Error(
              `Unknown settings tab: ${tabId}. Available: ${SETTINGS_TABS.map((t) => t.id).join(", ")}`,
            );
          }
          setActiveTab(tab.id);
          return { switched: true, activeTab: tab.id };
        },
      },
      {
        id: "list-tabs",
        label: "List available settings tabs",
        description: "Return the id + label of every settings sub-tab.",
        handler: () => SETTINGS_TABS.map((t) => ({ id: t.id, label: t.label })),
      },
    ],
  });

  const settingsContent = (() => {
    switch (activeTab) {
      case "account":
        return <AccountSettings onLog={onLog} />;
      case "backend-connection":
        return <WebIntegrationSettings onLog={onLog} />;
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
      case "discovery":
        return <DiscoverySettings onLog={onLog} />;
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
      case "lock-yield":
        return <LockYieldPolicySettings onLog={onLog} />;
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

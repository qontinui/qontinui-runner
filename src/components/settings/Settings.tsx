/**
 * Settings.tsx
 *
 * Settings tab containing only passive configuration options.
 * Active operations (capture, storage cleanup) have been moved to dedicated tabs.
 */

import { useState, useEffect, useCallback } from "react";
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
import { CiRunnerSettings } from "./CiRunnerSettings";

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
  | "ci-runner"
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
  { id: "ci-runner", label: "CI Runner" },
  { id: "security", label: "Security" },
  { id: "lock-yield", label: "Lock-yield Policy" },
  { id: "advanced", label: "Debug" },
  { id: "updates", label: "Updates" },
] as const satisfies ReadonlyArray<{ id: SettingsTab; label: string }>;

const VALID_TABS = SETTINGS_TABS.map((t) => t.id);

/**
 * Sub-tab id → main `MainTabId` id mapping for the `ui-bridge-set-tab` event
 * that mirrors the sub-tab into the global activeTab so `/control/tabs`
 * reports the right value after a sub-tab click.
 *
 * Most sub-tabs map by simple `settings-${id}` prefix concatenation; the
 * outliers (where the SETTINGS_TABS id and the MainTabId stem disagree) are
 * listed explicitly. Update both lists when adding a new sub-tab. Iter-2
 * item 1: manual-test-loop.
 */
const SUB_TAB_TO_MAIN_TAB: Record<SettingsTab, string> = {
  account: "settings-account",
  "backend-connection": "settings-backend-connection",
  ai: "settings-ai",
  agentic: "settings-agentic",
  "self-healing": "settings-self-healing",
  "world-state-verifier": "settings-world-state-verifier",
  playwright: "settings-playwright",
  mobile: "settings-mobile",
  discovery: "settings-discovery",
  mcp: "settings-mcp",
  "log-sources": "settings-log-sources",
  "execution-variables": "settings-execution-variables",
  notifications: "settings-notifications",
  general: "settings-general",
  storage: "settings-storage",
  backup: "settings-backup",
  instances: "settings-instances",
  otel: "settings-otel",
  containers: "settings-containers",
  "ci-runner": "settings-ci-runner",
  security: "settings-security",
  "lock-yield": "settings-lock-yield",
  // SETTINGS_TABS id is "advanced" (label "Debug"); MainTabId is "settings-debug"
  advanced: "settings-debug",
  updates: "settings-updates",
};

function migrateStoredTab(stored: string | null): SettingsTab {
  if (!stored) return "backend-connection";
  if ((VALID_TABS as readonly string[]).includes(stored)) {
    return stored as SettingsTab;
  }
  return "backend-connection";
}

export function Settings({ defaultTab, onLog, onDebugModeChange }: SettingsProps) {
  const [activeTab, setActiveTabRaw] = useState<SettingsTab>(() => {
    if (defaultTab && (VALID_TABS as readonly string[]).includes(defaultTab)) {
      return defaultTab as SettingsTab;
    }
    return migrateStoredTab(instanceStorage.getItem(STORAGE_KEY));
  });

  /**
   * Wraps `setActiveTabRaw` so a sub-tab switch ALSO updates the runner's
   * top-level activeTab (read by `/control/tabs`).
   *
   * Iter-1 added the sub-tab nav buttons but their onClick only set the
   * local component state; the runner's main `activeTab` (the value
   * `useAppNavigation` persists into ACTIVE_TAB_STORAGE_KEY and `/control/tabs`
   * surfaces) stayed at `"settings"` forever. UI Bridge consumers couldn't
   * tell which sub-panel was on screen — manual-test-loop iter 2 item 1.
   *
   * We mirror the sub-tab id into the global tab id by firing the same
   * `ui-bridge-set-tab` window event that the F4 `tab_activate` handler
   * already dispatches; `useAppNavigation` listens for it and calls
   * `setActiveTabAndPersist("settings-<subtab>")`. The id form
   * `settings-<subtab>` matches the existing `MainTabId` union in
   * `tab-types.ts` (e.g. `settings-account`, `settings-ai`, `settings-mobile`).
   */
  const setActiveTab = useCallback((next: SettingsTab) => {
    setActiveTabRaw(next);
    const mainTabId = SUB_TAB_TO_MAIN_TAB[next] ?? `settings-${next}`;
    window.dispatchEvent(
      new CustomEvent<{ tab: string }>("ui-bridge-set-tab", {
        detail: { tab: mainTabId },
      }),
    );
  }, []);

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
      case "ci-runner":
        return <CiRunnerSettings onLog={onLog} />;
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
      {/*
        Sub-tab navigation. Without this bar the Settings page renders only
        the currently active sub-panel and gives the user no way to switch
        between Account / AI / Storage / etc. from inside the Settings tab —
        they had to use the sidebar one row per sub-tab. The bar lets a single
        `settings`-tab activation see and reach every subsection.

        Clicking a button calls our wrapping `setActiveTab` which both flips
        local state AND fires `ui-bridge-set-tab` so the runner's main
        activeTab (the one surfaced at GET /control/tabs) tracks the sub-tab.
      */}
      <nav
        aria-label="Settings sub-tabs"
        className="flex flex-wrap gap-1 border-b border-zinc-700 pb-2"
      >
        {SETTINGS_TABS.map((tab) => {
          const isActive = activeTab === tab.id;
          return (
            <button
              key={tab.id}
              type="button"
              role="tab"
              aria-selected={isActive}
              onClick={() => setActiveTab(tab.id)}
              className={`px-3 py-1 rounded text-sm transition-colors ${
                isActive
                  ? "bg-blue-600 text-white"
                  : "bg-zinc-800 text-zinc-300 hover:bg-zinc-700"
              }`}
            >
              {tab.label}
            </button>
          );
        })}
      </nav>
      <div className="flex-1 overflow-y-auto scrollbar-dark">{settingsContent}</div>
    </div>
  );
}

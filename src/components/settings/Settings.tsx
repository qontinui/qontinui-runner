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
import { DevenvEnrollSettings } from "./DevenvEnrollSettings";
import { DevLoopSettings } from "./DevLoopSettings";
import {
  DEFAULT_SETTINGS_SUB_TAB,
  migrateSettingsSubTab,
  SETTINGS_SUB_TAB_TO_MAIN_TAB,
  SETTINGS_TABS,
  VALID_SETTINGS_TABS,
  type SettingsTab,
} from "./settings-tabs";

interface SettingsProps {
  /** Default tab to open. If provided, overrides instanceStorage persistence. */
  defaultTab?: string;
  onLog: (level: "info" | "warning" | "error" | "debug" | "success", message: string) => void;
  onDebugModeChange: (enabled: boolean) => void;
}

const STORAGE_KEY = "qontinui-settings-active-tab";

const VALID_TABS = VALID_SETTINGS_TABS;

export function Settings({ defaultTab, onLog, onDebugModeChange }: SettingsProps) {
  const [activeTab, setActiveTabRaw] = useState<SettingsTab>(() => {
    if (defaultTab && (VALID_TABS as readonly string[]).includes(defaultTab)) {
      return defaultTab as SettingsTab;
    }
    return migrateSettingsSubTab(instanceStorage.getItem(STORAGE_KEY));
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
    const mainTabId: string = SETTINGS_SUB_TAB_TO_MAIN_TAB[next] ?? `settings-${next}`;
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
          setActiveTab(DEFAULT_SETTINGS_SUB_TAB);
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
      case "dev-loop":
        return <DevLoopSettings onLog={onLog} />;
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
      case "devenv-enroll":
        return <DevenvEnrollSettings onLog={onLog} />;
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
        // Compile-time exhaustiveness: a new `SettingsTab` with no panel above
        // fails `tsc` here instead of silently rendering an empty sub-panel
        // (the same "default: return null" defect class R1 fixes in TabContent).
        return assertNeverSettingsTab(activeTab);
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
                isActive ? "bg-blue-600 text-white" : "bg-zinc-800 text-zinc-300 hover:bg-zinc-700"
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

/**
 * Compile-time guard: every `SettingsTab` must have a panel in the switch above.
 * Passing a non-`never` here is a `tsc` error naming the unhandled id.
 */
function assertNeverSettingsTab(tab: never): null {
  console.error(`[Settings] No panel registered for settings sub-tab "${String(tab)}"`);
  return null;
}

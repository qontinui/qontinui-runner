import { useState, useEffect } from "react";
import * as Tabs from "@radix-ui/react-tabs";
import { Wifi, Settings as SettingsIcon, Camera, HardDrive, Wrench } from "lucide-react";
import { ConnectionSettings } from "./ConnectionSettings";
import { GeneralSettings } from "./GeneralSettings";
import { CaptureSettings } from "./CaptureSettings";
import { StorageSettings } from "./StorageSettings";
import { AdvancedSettings } from "./AdvancedSettings";

interface SettingsProps {
  onLog: (level: "info" | "warning" | "error" | "debug" | "success", message: string) => void;
  onDebugModeChange: (enabled: boolean) => void;
}

type SettingsTab = "connection" | "general" | "capture" | "storage" | "advanced";

const STORAGE_KEY = "qontinui-settings-active-tab";

export function Settings({ onLog, onDebugModeChange }: SettingsProps) {
  const [activeTab, setActiveTab] = useState<SettingsTab>(() => {
    // Load persisted tab on mount
    const stored = localStorage.getItem(STORAGE_KEY);
    if (stored && ["connection", "general", "capture", "storage", "advanced"].includes(stored)) {
      return stored as SettingsTab;
    }
    return "connection";
  });

  // Persist active tab
  useEffect(() => {
    localStorage.setItem(STORAGE_KEY, activeTab);
  }, [activeTab]);

  const tabs = [
    { id: "connection" as const, label: "Connection", icon: Wifi },
    { id: "general" as const, label: "General", icon: SettingsIcon },
    { id: "capture" as const, label: "Capture", icon: Camera },
    { id: "storage" as const, label: "Storage", icon: HardDrive },
    { id: "advanced" as const, label: "Advanced", icon: Wrench },
  ];

  return (
    <Tabs.Root
      value={activeTab}
      onValueChange={(value) => setActiveTab(value as SettingsTab)}
      orientation="vertical"
      className="flex h-full min-h-[500px]"
    >
      {/* Left sidebar with tabs */}
      <Tabs.List className="flex flex-col w-44 shrink-0 border-r border-border/50 bg-card/50 p-2 gap-1">
        <div className="px-3 py-2 mb-2">
          <h3 className="text-lg font-semibold flex items-center gap-2">
            <SettingsIcon className="w-5 h-5 text-primary" />
            Settings
          </h3>
        </div>

        {tabs.map((tab) => {
          const Icon = tab.icon;
          return (
            <Tabs.Trigger
              key={tab.id}
              value={tab.id}
              className={`
                flex items-center gap-3 px-3 py-2.5 rounded-md text-left text-sm font-medium
                transition-colors duration-150 outline-none
                data-[state=active]:bg-primary data-[state=active]:text-primary-foreground
                data-[state=inactive]:text-muted-foreground data-[state=inactive]:hover:bg-muted/50
                focus-visible:ring-2 focus-visible:ring-primary focus-visible:ring-offset-2
              `}
            >
              <Icon className="w-4 h-4 shrink-0" />
              <span>{tab.label}</span>
            </Tabs.Trigger>
          );
        })}

        {/* Spacer to push help text to bottom */}
        <div className="flex-1" />

        {/* Help text at bottom */}
        <div className="px-3 py-2 text-xs text-muted-foreground border-t border-border/50 mt-2 pt-3">
          Use arrow keys to navigate between tabs
        </div>
      </Tabs.List>

      {/* Content area */}
      <div className="flex-1 overflow-y-auto p-6">
        <Tabs.Content value="connection" className="outline-none">
          <ConnectionSettings onLog={onLog} />
        </Tabs.Content>

        <Tabs.Content value="general" className="outline-none">
          <GeneralSettings onLog={onLog} />
        </Tabs.Content>

        <Tabs.Content value="capture" className="outline-none">
          <CaptureSettings onLog={onLog} />
        </Tabs.Content>

        <Tabs.Content value="storage" className="outline-none">
          <StorageSettings onLog={onLog} />
        </Tabs.Content>

        <Tabs.Content value="advanced" className="outline-none">
          <AdvancedSettings onLog={onLog} onDebugModeChange={onDebugModeChange} />
        </Tabs.Content>
      </div>
    </Tabs.Root>
  );
}

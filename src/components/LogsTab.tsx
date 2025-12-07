/**
 * LogsTab.tsx
 *
 * Dedicated tab for all log viewing functionality.
 * Contains sub-tabs for General, Image Recognition, and Actions logs.
 */

import { useRef } from "react";
import * as Tabs from "@radix-ui/react-tabs";
import { FileText, Image, Zap } from "lucide-react";
import { GeneralLogTab } from "./GeneralLogTab";
import ImageLogTable from "./ImageLogTable";
import ActionLogTable from "./ActionLogTable";
import { LogTabActions } from "./LogTabActions";
import { useAutoScroll } from "../hooks";
import type { LogEntry, ImageRecognitionEntry } from "../managers/LogManager";
import type { ActionLogEntry } from "../types/displayProfile";
import type { LogLevel } from "../hooks/useLogFilter";

type LogSubTab = "general" | "image" | "actions";

interface LogsTabProps {
  // General logs
  logs: LogEntry[];
  filteredLogs: LogEntry[];
  logLevel: LogLevel;
  onLogLevelChange: (level: LogLevel) => void;
  showLogFilter: boolean;
  onToggleLogFilter: (show: boolean) => void;

  // Image logs
  imageLogs: ImageRecognitionEntry[];
  onImageRowClick: (entry: ImageRecognitionEntry) => void;

  // Action logs
  actionLogData: {
    actions: ActionLogEntry[];
    visible_count: number;
  } | null;
  actionLogLoading: boolean;
  actionLogError: string | null;
  onActionRowClick: (action: ActionLogEntry) => void;

  // Log counts
  logCount: number;
  imageLogCount: number;
  actionCount: number;

  // Clear/copy actions
  onClearGeneralLogs: () => void;
  onClearImageLogs: () => void;
  onClearActionLogs: () => void;
  onClearAllLogs: () => void;
  onCopyLogs: () => void;
  copySuccess: boolean;

  // Active sub-tab state
  activeSubTab: LogSubTab;
  onSubTabChange: (tab: LogSubTab) => void;
}

export function LogsTab({
  logs,
  filteredLogs,
  logLevel,
  onLogLevelChange,
  showLogFilter,
  onToggleLogFilter,
  imageLogs,
  onImageRowClick,
  actionLogData,
  actionLogLoading,
  actionLogError,
  onActionRowClick,
  logCount,
  imageLogCount,
  actionCount,
  onClearGeneralLogs,
  onClearImageLogs,
  onClearActionLogs,
  onClearAllLogs,
  onCopyLogs,
  copySuccess,
  activeSubTab,
  onSubTabChange,
}: LogsTabProps) {
  // Auto-scroll for general logs
  const logViewerRef = useRef<HTMLDivElement>(null);
  useAutoScroll({
    enabled: activeSubTab === "general",
    containerRef: logViewerRef,
    dependencies: [logs],
  });

  const subTabs = [
    { id: "general" as const, label: "General", icon: FileText, count: logCount },
    { id: "image" as const, label: "Image Recognition", icon: Image, count: imageLogCount },
    { id: "actions" as const, label: "Actions", icon: Zap, count: actionCount },
  ];

  return (
    <Tabs.Root
      value={activeSubTab}
      onValueChange={(value) => onSubTabChange(value as LogSubTab)}
      className="h-full flex flex-col"
    >
      {/* Sub-tab Navigation */}
      <div className="flex items-center justify-between border-b border-border">
        <Tabs.List className="flex">
          {subTabs.map((tab) => {
            const Icon = tab.icon;
            return (
              <Tabs.Trigger
                key={tab.id}
                value={tab.id}
                className={`
                  flex items-center gap-2 px-4 py-3 text-sm font-medium
                  border-b-2 transition-colors
                  data-[state=active]:border-primary data-[state=active]:text-primary
                  data-[state=inactive]:border-transparent data-[state=inactive]:text-muted-foreground
                  data-[state=inactive]:hover:text-foreground
                `}
              >
                <Icon className="w-4 h-4" />
                {tab.label}
                {tab.count > 0 && (
                  <span className="ml-1 px-1.5 py-0.5 text-xs bg-muted rounded-full">
                    {tab.count}
                  </span>
                )}
              </Tabs.Trigger>
            );
          })}
        </Tabs.List>

        {/* Tab Actions */}
        <LogTabActions
          activeTab={activeSubTab}
          showLogFilter={showLogFilter}
          onToggleLogFilter={onToggleLogFilter}
          logLevel={logLevel}
          onLogLevelChange={onLogLevelChange}
          onClearGeneralLogs={onClearGeneralLogs}
          onClearImageLogs={onClearImageLogs}
          onClearActionLogs={onClearActionLogs}
          onClearAllLogs={onClearAllLogs}
          onCopyLogs={onCopyLogs}
          copySuccess={copySuccess}
        />
      </div>

      {/* Tab Content */}
      <div className="flex-1 p-4 overflow-hidden">
        <Tabs.Content value="general" className="h-full outline-none">
          <GeneralLogTab logs={filteredLogs} containerRef={logViewerRef} />
        </Tabs.Content>

        <Tabs.Content value="image" className="h-full outline-none">
          <div style={{ maxHeight: "400px", overflowY: "auto" }}>
            <ImageLogTable imageLogs={imageLogs} onRowClick={onImageRowClick} />
          </div>
        </Tabs.Content>

        <Tabs.Content value="actions" className="h-full outline-none">
          {actionLogLoading && (
            <div className="text-center text-muted-foreground py-8">Loading action log...</div>
          )}
          {actionLogError && (
            <div className="text-center text-red-600 py-8">Error: {actionLogError}</div>
          )}
          {!actionLogLoading && !actionLogError && actionLogData && (
            <ActionLogTable actions={actionLogData.actions} onRowClick={onActionRowClick} />
          )}
        </Tabs.Content>
      </div>
    </Tabs.Root>
  );
}

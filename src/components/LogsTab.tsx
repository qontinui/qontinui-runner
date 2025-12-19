/**
 * LogsTab.tsx
 *
 * Dedicated tab for all log viewing functionality.
 * Contains sub-tabs for General, Image Recognition, and Actions logs.
 */

import { useRef, useState, useEffect } from "react";
import * as Tabs from "@radix-ui/react-tabs";
import { FileText, Image, Zap, Brain, FolderOpen, Database, AlertTriangle } from "lucide-react";
import { GeneralLogTab } from "./GeneralLogTab";
import ImageLogTable from "./ImageLogTable";
import ActionLogTable from "./ActionLogTable";
import { AiOutputTab, type AiOutputLine } from "./AiOutputTab";
import { ExternalLogsTab } from "./ExternalLogsTab";
import { RagLogTab, type RagProcessingState } from "./RagLogTab";
import { IssuesPanel } from "./IssuesPanel";
import { LogTabActions } from "./LogTabActions";
import { useAutoScroll } from "../hooks";
import { issueTracker } from "../services";
import type { LogEntry, ImageRecognitionEntry } from "../managers/LogManager";
import type { ActionLogEntry } from "../types/displayProfile";
import type { LogLevel } from "../hooks/useLogFilter";
import type { LogSourceContent, ProjectLogConfig } from "../types/projectLogs";

type LogSubTab = "general" | "image" | "actions" | "ai" | "issues" | "rag" | "external";

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

  // AI output
  aiOutputLines: AiOutputLine[];
  onClearAiOutput: () => void;

  // External logs (project logs)
  projectLogConfig: ProjectLogConfig | null;
  projectLogSources: LogSourceContent[];
  projectLogsLoading: boolean;
  projectLogsError?: string;
  projectLogsLastRefresh?: string;
  onRefreshProjectLogs: () => void;
  onConfigureProjectLogs: () => void;

  // RAG processing
  ragState: RagProcessingState;
  onStartRagProcessing: () => void;
  onClearRagLogs: () => void;
  canStartRagProcessing: boolean;

  // Log counts
  logCount: number;
  imageLogCount: number;
  actionCount: number;
  aiOutputCount: number;
  ragLogCount: number;
  externalLogCount: number;

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
  aiOutputLines,
  onClearAiOutput,
  projectLogConfig,
  projectLogSources,
  projectLogsLoading,
  projectLogsError,
  projectLogsLastRefresh,
  onRefreshProjectLogs,
  onConfigureProjectLogs,
  ragState,
  onStartRagProcessing,
  onClearRagLogs,
  canStartRagProcessing,
  logCount,
  imageLogCount,
  actionCount,
  aiOutputCount,
  ragLogCount,
  externalLogCount,
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

  // Track issue count from IssueTracker
  const [issueCount, setIssueCount] = useState(issueTracker.count);
  const [unresolvedCount, setUnresolvedCount] = useState(issueTracker.unresolvedCount);

  useEffect(() => {
    const updateCounts = () => {
      setIssueCount(issueTracker.count);
      setUnresolvedCount(issueTracker.unresolvedCount);
    };
    const unsubscribe = issueTracker.subscribe(updateCounts);
    return unsubscribe;
  }, []);

  const subTabs = [
    { id: "general" as const, label: "General", icon: FileText, count: logCount },
    { id: "image" as const, label: "Image Recognition", icon: Image, count: imageLogCount },
    { id: "actions" as const, label: "Actions", icon: Zap, count: actionCount },
    { id: "ai" as const, label: "AI Output", icon: Brain, count: aiOutputCount },
    {
      id: "issues" as const,
      label: "Issues",
      icon: AlertTriangle,
      count: issueCount,
      highlight: unresolvedCount > 0,
    },
    { id: "rag" as const, label: "RAG", icon: Database, count: ragLogCount },
    { id: "external" as const, label: "Project Logs", icon: FolderOpen, count: externalLogCount },
  ];

  return (
    <Tabs.Root
      value={activeSubTab}
      onValueChange={(value) => onSubTabChange(value as LogSubTab)}
      className="h-full flex flex-col overflow-hidden"
    >
      {/* Sub-tab Navigation - Fixed Header (flex-shrink-0 keeps it from scrolling) */}
      <div className="flex-shrink-0 bg-background flex items-center justify-between border-b border-border z-10 relative">
        <Tabs.List className="flex">
          {subTabs.map((tab) => {
            const Icon = tab.icon;
            const hasHighlight = "highlight" in tab && tab.highlight;
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
                <Icon className={`w-4 h-4 ${hasHighlight ? "text-red-500" : ""}`} />
                {tab.label}
                {tab.count > 0 && (
                  <span
                    className={`ml-1 px-1.5 py-0.5 text-xs rounded-full ${
                      hasHighlight ? "bg-red-500/20 text-red-400" : "bg-muted"
                    }`}
                  >
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

      {/* Tab Content - Each tab manages its own scrolling */}
      <div className="flex-1 flex flex-col min-h-0 overflow-hidden">
        <Tabs.Content
          value="general"
          className="flex-1 p-4 outline-none overflow-auto data-[state=inactive]:hidden"
        >
          <GeneralLogTab logs={filteredLogs} containerRef={logViewerRef} />
        </Tabs.Content>

        <Tabs.Content
          value="image"
          className="flex-1 p-4 outline-none overflow-auto data-[state=inactive]:hidden"
        >
          <ImageLogTable imageLogs={imageLogs} onRowClick={onImageRowClick} />
        </Tabs.Content>

        <Tabs.Content
          value="actions"
          className="flex-1 p-4 outline-none overflow-auto data-[state=inactive]:hidden"
        >
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

        <Tabs.Content
          value="ai"
          className="flex-1 flex flex-col p-4 outline-none overflow-hidden data-[state=inactive]:hidden"
        >
          <AiOutputTab lines={aiOutputLines} onClear={onClearAiOutput} />
        </Tabs.Content>

        <Tabs.Content
          value="issues"
          className="flex-1 p-4 outline-none overflow-auto data-[state=inactive]:hidden"
        >
          <IssuesPanel />
        </Tabs.Content>

        <Tabs.Content
          value="rag"
          className="flex-1 p-4 outline-none overflow-auto data-[state=inactive]:hidden"
        >
          <RagLogTab
            state={ragState}
            onStartProcessing={onStartRagProcessing}
            onClearLogs={onClearRagLogs}
            canStartProcessing={canStartRagProcessing}
          />
        </Tabs.Content>

        <Tabs.Content
          value="external"
          className="flex-1 p-4 outline-none overflow-auto data-[state=inactive]:hidden"
        >
          <ExternalLogsTab
            config={projectLogConfig}
            sources={projectLogSources}
            loading={projectLogsLoading}
            error={projectLogsError}
            lastRefresh={projectLogsLastRefresh}
            onRefresh={onRefreshProjectLogs}
            onConfigureSources={onConfigureProjectLogs}
          />
        </Tabs.Content>
      </div>
    </Tabs.Root>
  );
}

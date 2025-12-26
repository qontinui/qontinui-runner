/**
 * AiWorkflowsTab.tsx
 *
 * Consolidated AI Workflows tab with sub-pages:
 * - Workflow Builder: Build and run AI automation workflows
 * - Workflow Library: Manage saved AI workflows
 * - Prompts: Prompt library management
 * - Scripts: Playwright script management
 * - AI Output: View AI conversation output
 * - Issues: AI-detected issues from workflow execution
 * - Learnings: Analyze AI loops to extract patterns and insights
 * - Log Locations: Configure external log sources for AI analysis
 */

import { useState, useEffect } from "react";
import * as Tabs from "@radix-ui/react-tabs";
import {
  Sparkles,
  BookOpen,
  FileText,
  TestTube,
  MessageSquare,
  AlertTriangle,
  FolderOpen,
  Lightbulb,
  Puzzle,
} from "lucide-react";

import type { UseProjectLogsReturn } from "../hooks/useProjectLogs";
import type { AiOutputLine } from "./AiOutputTab";
import { issueTracker } from "../services";

// Sub-tab components
import { WorkflowBuilderSubTab } from "./ai-workflows/WorkflowBuilderSubTab";
import { WorkflowLibrarySubTab } from "./ai-workflows/WorkflowLibrarySubTab";
import { PromptsSubTab } from "./ai-workflows/PromptsSubTab";
import { ScriptsSubTab } from "./ai-workflows/ScriptsSubTab";
import { AiOutputSubTab } from "./ai-workflows/AiOutputSubTab";
import { LearningsSubTab } from "./ai-workflows/LearningsSubTab";
import { ScriptletsSubTab } from "./ai-workflows/ScriptletsSubTab";
import { IssuesPanel } from "./IssuesPanel";
import { ExternalLogsTab } from "./ExternalLogsTab";

type AiWorkflowSubTab =
  | "builder"
  | "library"
  | "prompts"
  | "scripts"
  | "output"
  | "issues"
  | "learnings"
  | "scriptlets"
  | "log-locations";
type LogLevel = "info" | "warning" | "error" | "debug" | "success";

const SUB_TAB_STORAGE_KEY = "qontinui-ai-workflows-sub-tab";

interface AiWorkflowsTabProps {
  projectLogs: UseProjectLogsReturn;
  aiOutputLines: AiOutputLine[];
  onClearAiOutput: () => void;
  onLog: (level: LogLevel, message: string) => void;
  onConfigureLogLocations: () => void;
}

export function AiWorkflowsTab({
  projectLogs,
  aiOutputLines,
  onClearAiOutput,
  onLog,
  onConfigureLogLocations,
}: AiWorkflowsTabProps) {
  const [activeSubTab, setActiveSubTab] = useState<AiWorkflowSubTab>(() => {
    const stored = localStorage.getItem(SUB_TAB_STORAGE_KEY);
    if (
      stored &&
      [
        "builder",
        "library",
        "prompts",
        "scripts",
        "output",
        "issues",
        "learnings",
        "scriptlets",
        "log-locations",
      ].includes(stored)
    ) {
      return stored as AiWorkflowSubTab;
    }
    return "builder";
  });

  // Track issue count from IssueTracker (using session-filtered counts to match IssuesPanel)
  const [issueCount, setIssueCount] = useState(() => issueTracker.getSessionIssues().length);
  const [unresolvedCount, setUnresolvedCount] = useState(
    () =>
      issueTracker
        .getSessionIssues()
        .filter((i) => i.status === "detected" || i.status === "in_progress").length,
  );

  useEffect(() => {
    const updateCounts = () => {
      const sessionIssues = issueTracker.getSessionIssues();
      setIssueCount(sessionIssues.length);
      setUnresolvedCount(
        sessionIssues.filter((i) => i.status === "detected" || i.status === "in_progress").length,
      );
    };
    // Initial update
    updateCounts();
    const unsubscribe = issueTracker.subscribe(updateCounts);
    return unsubscribe;
  }, []);

  // Persist sub-tab selection
  useEffect(() => {
    localStorage.setItem(SUB_TAB_STORAGE_KEY, activeSubTab);
  }, [activeSubTab]);

  const subTabs = [
    { id: "builder" as const, label: "Builder", icon: Sparkles },
    { id: "library" as const, label: "Workflows", icon: BookOpen },
    { id: "prompts" as const, label: "Prompts", icon: FileText },
    { id: "scripts" as const, label: "Scripts", icon: TestTube },
    { id: "output" as const, label: "AI Output", icon: MessageSquare },
    {
      id: "issues" as const,
      label: "Issues",
      icon: AlertTriangle,
      count: issueCount,
      highlight: unresolvedCount > 0,
    },
    { id: "learnings" as const, label: "Learnings", icon: Lightbulb },
    { id: "scriptlets" as const, label: "Scriptlets", icon: Puzzle },
    { id: "log-locations" as const, label: "Log Locations", icon: FolderOpen },
  ];

  return (
    <div className="h-full flex flex-col overflow-hidden">
      <Tabs.Root
        value={activeSubTab}
        onValueChange={(value) => setActiveSubTab(value as AiWorkflowSubTab)}
        className="flex-1 flex flex-col min-h-0"
      >
        {/* Sub-Tab Navigation - Fixed */}
        <Tabs.List className="flex border-b border-border bg-card/50 px-4 flex-shrink-0">
          {subTabs.map((tab) => {
            const Icon = tab.icon;
            const hasCount = "count" in tab && typeof tab.count === "number";
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
                  data-[state=inactive]:hover:text-foreground data-[state=inactive]:hover:bg-muted/30
                `}
              >
                <Icon className={`w-4 h-4 ${hasHighlight ? "text-red-500" : ""}`} />
                {tab.label}
                {hasCount && tab.count > 0 && (
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

        {/* Sub-Tab Content */}
        <div className="flex-1 min-h-0 overflow-hidden">
          {/* Workflow Builder */}
          <Tabs.Content
            value="builder"
            className="h-full outline-none overflow-y-auto data-[state=inactive]:hidden"
          >
            <WorkflowBuilderSubTab
              projectLogs={projectLogs}
              onNavigateToLogLocations={() => setActiveSubTab("log-locations")}
            />
          </Tabs.Content>

          {/* Workflow Library */}
          <Tabs.Content
            value="library"
            className="h-full outline-none overflow-y-auto data-[state=inactive]:hidden"
          >
            <WorkflowLibrarySubTab onLog={onLog} />
          </Tabs.Content>

          {/* Prompts */}
          <Tabs.Content
            value="prompts"
            className="h-full outline-none overflow-y-auto data-[state=inactive]:hidden"
          >
            <PromptsSubTab onLog={onLog} />
          </Tabs.Content>

          {/* Scripts */}
          <Tabs.Content
            value="scripts"
            forceMount
            className="h-full outline-none overflow-y-auto data-[state=inactive]:hidden"
          >
            <ScriptsSubTab onLog={onLog} />
          </Tabs.Content>

          {/* AI Output */}
          <Tabs.Content
            value="output"
            className="h-full outline-none overflow-y-auto data-[state=inactive]:hidden"
          >
            <AiOutputSubTab aiOutputLines={aiOutputLines} onClearAiOutput={onClearAiOutput} />
          </Tabs.Content>

          {/* Issues */}
          <Tabs.Content
            value="issues"
            className="h-full outline-none overflow-y-auto data-[state=inactive]:hidden"
          >
            <div className="h-full p-4">
              <IssuesPanel />
            </div>
          </Tabs.Content>

          {/* Learnings */}
          <Tabs.Content
            value="learnings"
            className="h-full outline-none overflow-hidden data-[state=inactive]:hidden"
          >
            <LearningsSubTab aiOutputLines={aiOutputLines} />
          </Tabs.Content>

          {/* Scriptlets */}
          <Tabs.Content
            value="scriptlets"
            className="h-full outline-none overflow-y-auto data-[state=inactive]:hidden"
          >
            <ScriptletsSubTab onLog={onLog} aiOutputLines={aiOutputLines} />
          </Tabs.Content>

          {/* Log Locations */}
          <Tabs.Content
            value="log-locations"
            className="h-full outline-none overflow-y-auto data-[state=inactive]:hidden"
          >
            <div className="h-full p-4">
              <ExternalLogsTab
                config={projectLogs.config}
                sources={projectLogs.logsState.sources}
                loading={projectLogs.logsState.loading}
                error={projectLogs.logsState.error}
                lastRefresh={projectLogs.logsState.lastRefresh}
                onRefresh={projectLogs.refreshLogs}
                onConfigureSources={onConfigureLogLocations}
              />
            </div>
          </Tabs.Content>
        </div>
      </Tabs.Root>
    </div>
  );
}

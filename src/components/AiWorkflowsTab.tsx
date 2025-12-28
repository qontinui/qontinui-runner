/**
 * AiWorkflowsTab.tsx
 *
 * Redesigned AI Workflows tab with streamlined navigation (4 tabs):
 * - Session: Active workflow view with AI Output (Continue button here)
 * - Library: Combined prompts, workflows, and scripts
 * - Monitor: Issues and learnings analysis
 * - Configure: Workflow builder, log sources, scriptlets
 *
 * This redesign addresses UX confusion where users would start prompts
 * in one tab but need to go to another for Continue/Output viewing.
 */

import { useState, useEffect, useCallback } from "react";
import * as Tabs from "@radix-ui/react-tabs";
import { Activity, BookOpen, Eye, Settings } from "lucide-react";

import type { UseProjectLogsReturn } from "../hooks/useProjectLogs";
import type { AiOutputLine } from "./AiOutputTab";
import { issueTracker } from "../services";

// New consolidated sub-tab components
import { SessionSubTab } from "./ai-workflows/SessionSubTab";
import { LibrarySubTab } from "./ai-workflows/LibrarySubTab";
import { MonitorSubTab } from "./ai-workflows/MonitorSubTab";
import { ConfigureSubTab } from "./ai-workflows/ConfigureSubTab";

type AiWorkflowSubTab = "session" | "library" | "monitor" | "configure";
type LogLevel = "info" | "warning" | "error" | "debug" | "success";

const SUB_TAB_STORAGE_KEY = "qontinui-ai-workflows-sub-tab-v2";

interface AiWorkflowsTabProps {
  projectLogs: UseProjectLogsReturn;
  aiOutputLines: AiOutputLine[];
  onClearAiOutput: () => void;
  onAddAiOutputLine?: (line: Omit<AiOutputLine, "id">) => void;
  onLog: (level: LogLevel, message: string) => void;
  onConfigureLogLocations: () => void;
}

export function AiWorkflowsTab({
  projectLogs,
  aiOutputLines,
  onClearAiOutput,
  onAddAiOutputLine,
  onLog,
  onConfigureLogLocations,
}: AiWorkflowsTabProps) {
  const [activeSubTab, setActiveSubTab] = useState<AiWorkflowSubTab>(() => {
    const stored = localStorage.getItem(SUB_TAB_STORAGE_KEY);
    // Map old tab names to new ones for backward compatibility
    if (stored === "builder" || stored === "output") return "session";
    if (stored === "prompts" || stored === "library" || stored === "scripts") return "library";
    if (stored === "issues" || stored === "learnings") return "monitor";
    if (stored === "scriptlets" || stored === "log-locations") return "configure";
    if (stored && ["session", "library", "monitor", "configure"].includes(stored)) {
      return stored as AiWorkflowSubTab;
    }
    return "session";
  });

  // Track issue count for badge
  const [unresolvedCount, setUnresolvedCount] = useState(
    () =>
      issueTracker
        .getSessionIssues()
        .filter((i) => i.status === "detected" || i.status === "in_progress").length,
  );

  useEffect(() => {
    const updateCounts = () => {
      const sessionIssues = issueTracker.getSessionIssues();
      setUnresolvedCount(
        sessionIssues.filter((i) => i.status === "detected" || i.status === "in_progress").length,
      );
    };
    updateCounts();
    const unsubscribe = issueTracker.subscribe(updateCounts);
    return unsubscribe;
  }, []);

  // Persist sub-tab selection
  useEffect(() => {
    localStorage.setItem(SUB_TAB_STORAGE_KEY, activeSubTab);
  }, [activeSubTab]);

  // Navigation callback for Library -> Session
  const handleNavigateToSession = useCallback(() => {
    setActiveSubTab("session");
  }, []);

  // Navigation callback for Session -> Library
  const handleNavigateToLibrary = useCallback(() => {
    setActiveSubTab("library");
  }, []);

  const subTabs = [
    {
      id: "session" as const,
      label: "Session",
      icon: Activity,
      description: "Active AI session & output",
    },
    {
      id: "library" as const,
      label: "Library",
      icon: BookOpen,
      description: "Prompts, workflows, scripts",
    },
    {
      id: "monitor" as const,
      label: "Monitor",
      icon: Eye,
      description: "Issues & learnings",
      count: unresolvedCount > 0 ? unresolvedCount : undefined,
      highlight: unresolvedCount > 0,
    },
    {
      id: "configure" as const,
      label: "Configure",
      icon: Settings,
      description: "Builder, logs, scriptlets",
    },
  ];

  return (
    <div className="h-full flex flex-col overflow-hidden">
      <Tabs.Root
        value={activeSubTab}
        onValueChange={(value) => setActiveSubTab(value as AiWorkflowSubTab)}
        className="flex-1 flex flex-col min-h-0"
      >
        {/* Sub-Tab Navigation */}
        <Tabs.List className="flex border-b border-border bg-card/50 px-4 flex-shrink-0">
          {subTabs.map((tab) => {
            const Icon = tab.icon;
            const hasCount = tab.count !== undefined;
            const hasHighlight = tab.highlight;
            return (
              <Tabs.Trigger
                key={tab.id}
                value={tab.id}
                className={`
                  flex items-center gap-2 px-6 py-3 text-sm font-medium
                  border-b-2 transition-colors
                  data-[state=active]:border-primary data-[state=active]:text-primary
                  data-[state=inactive]:border-transparent data-[state=inactive]:text-muted-foreground
                  data-[state=inactive]:hover:text-foreground data-[state=inactive]:hover:bg-muted/30
                `}
                title={tab.description}
              >
                <Icon className={`w-4 h-4 ${hasHighlight ? "text-red-500" : ""}`} />
                {tab.label}
                {hasCount && (
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
          {/* Session Tab - Active workflow & AI Output */}
          <Tabs.Content
            value="session"
            className="h-full outline-none overflow-hidden data-[state=inactive]:hidden"
          >
            <SessionSubTab
              aiOutputLines={aiOutputLines}
              onClearAiOutput={onClearAiOutput}
              onAddAiOutputLine={onAddAiOutputLine}
              onNavigateToLibrary={handleNavigateToLibrary}
            />
          </Tabs.Content>

          {/* Library Tab - Prompts, Workflows, Scripts */}
          <Tabs.Content
            value="library"
            className="h-full outline-none overflow-hidden data-[state=inactive]:hidden"
          >
            <LibrarySubTab onLog={onLog} onNavigateToSession={handleNavigateToSession} />
          </Tabs.Content>

          {/* Monitor Tab - Issues & Learnings */}
          <Tabs.Content
            value="monitor"
            className="h-full outline-none overflow-hidden data-[state=inactive]:hidden"
          >
            <MonitorSubTab aiOutputLines={aiOutputLines} />
          </Tabs.Content>

          {/* Configure Tab - Builder, Log Sources, Scriptlets */}
          <Tabs.Content
            value="configure"
            className="h-full outline-none overflow-hidden data-[state=inactive]:hidden"
          >
            <ConfigureSubTab
              projectLogs={projectLogs}
              aiOutputLines={aiOutputLines}
              onLog={onLog}
              onConfigureLogLocations={onConfigureLogLocations}
            />
          </Tabs.Content>
        </div>
      </Tabs.Root>
    </div>
  );
}

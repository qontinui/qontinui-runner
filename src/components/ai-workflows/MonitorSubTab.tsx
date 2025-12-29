/**
 * MonitorSubTab.tsx
 *
 * Monitoring view that combines:
 * - Summary panel (code-focused execution summary)
 * - Findings panel (all categorized findings from AI execution)
 * - Issues panel (legacy detected issues during AI execution)
 * - Learnings panel (AI pattern analysis from sessions)
 *
 * Uses tabs to organize the different monitoring areas.
 */

import { useState } from "react";
import * as Tabs from "@radix-ui/react-tabs";
import { AlertTriangle, Lightbulb, FileText, ClipboardList } from "lucide-react";
import { IssuesPanel } from "../IssuesPanel";
import { LearningsSubTab } from "./LearningsSubTab";
import { ExecutionSummaryTab } from "./ExecutionSummaryTab";
import { ExecutionReport } from "../findings";
import type { AiOutputLine } from "../AiOutputTab";

interface MonitorSubTabProps {
  aiOutputLines: AiOutputLine[];
}

type MonitorTab = "summary" | "findings" | "issues" | "learnings";

export function MonitorSubTab({ aiOutputLines }: MonitorSubTabProps) {
  const [activeTab, setActiveTab] = useState<MonitorTab>("summary");

  const tabTriggerClass = `
    flex items-center gap-2 px-4 py-3 text-sm font-medium
    border-b-2 transition-colors
    data-[state=active]:border-primary data-[state=active]:text-primary
    data-[state=inactive]:border-transparent data-[state=inactive]:text-muted-foreground
    data-[state=inactive]:hover:text-foreground data-[state=inactive]:hover:bg-muted/30
  `;

  return (
    <div className="h-full flex flex-col overflow-hidden">
      <Tabs.Root
        value={activeTab}
        onValueChange={(value) => setActiveTab(value as MonitorTab)}
        className="flex-1 flex flex-col min-h-0"
      >
        {/* Tab Navigation */}
        <Tabs.List className="flex border-b border-border bg-muted/30 px-4 flex-shrink-0">
          <Tabs.Trigger value="summary" className={tabTriggerClass}>
            <ClipboardList className="w-4 h-4" />
            Execution Report
          </Tabs.Trigger>
          <Tabs.Trigger value="findings" className={tabTriggerClass}>
            <FileText className="w-4 h-4" />
            All Findings
          </Tabs.Trigger>
          <Tabs.Trigger value="issues" className={tabTriggerClass}>
            <AlertTriangle className="w-4 h-4" />
            Issues
          </Tabs.Trigger>
          <Tabs.Trigger value="learnings" className={tabTriggerClass}>
            <Lightbulb className="w-4 h-4" />
            Learnings
          </Tabs.Trigger>
        </Tabs.List>

        {/* Tab Content */}
        <div className="flex-1 min-h-0 overflow-hidden">
          <Tabs.Content
            value="summary"
            className="h-full outline-none overflow-hidden data-[state=inactive]:hidden"
          >
            <ExecutionSummaryTab />
          </Tabs.Content>

          <Tabs.Content
            value="findings"
            className="h-full outline-none overflow-hidden data-[state=inactive]:hidden"
          >
            <ExecutionReport />
          </Tabs.Content>

          <Tabs.Content
            value="issues"
            className="h-full outline-none overflow-y-auto p-4 data-[state=inactive]:hidden"
          >
            <IssuesPanel />
          </Tabs.Content>

          <Tabs.Content
            value="learnings"
            className="h-full outline-none overflow-hidden data-[state=inactive]:hidden"
          >
            <LearningsSubTab aiOutputLines={aiOutputLines} />
          </Tabs.Content>
        </div>
      </Tabs.Root>
    </div>
  );
}

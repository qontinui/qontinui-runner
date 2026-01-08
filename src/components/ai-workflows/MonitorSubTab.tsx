/**
 * MonitorSubTab.tsx
 *
 * Monitoring view that combines:
 * - Summary panel (code-focused execution summary)
 * - Findings panel (all categorized findings from AI execution)
 * - Verification panel (AI-driven state verification)
 * - Statistics panel (config health, flaky items, run history)
 * - Discoveries panel (sync queue for discovered items)
 *
 * Uses tabs to organize the different monitoring areas.
 */

import { useState } from "react";
import * as Tabs from "@radix-ui/react-tabs";
import { FileText, ClipboardList, FileSearch, BarChart3, Cloud } from "lucide-react";
import { ExecutionSummaryTab } from "./ExecutionSummaryTab";
import { ExecutionReport } from "../findings";
import { VerificationTab } from "../verification";
import { StatisticsTab } from "../statistics";
import { DiscoverySyncPanel } from "../discoveries";
import { useExecution } from "../../contexts/ExecutionContext";

type MonitorTab = "summary" | "findings" | "verification" | "statistics" | "discoveries";

export function MonitorSubTab() {
  const [activeTab, setActiveTab] = useState<MonitorTab>("summary");
  const { config } = useExecution();

  // Use config path as the config ID for statistics
  const configId = config?.path ?? null;
  const configName = config?.name ?? undefined;

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
          <Tabs.Trigger value="verification" className={tabTriggerClass}>
            <FileSearch className="w-4 h-4" />
            Verification
          </Tabs.Trigger>
          <Tabs.Trigger value="statistics" className={tabTriggerClass}>
            <BarChart3 className="w-4 h-4" />
            Statistics
          </Tabs.Trigger>
          <Tabs.Trigger value="discoveries" className={tabTriggerClass}>
            <Cloud className="w-4 h-4" />
            Discoveries
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
            value="verification"
            className="h-full outline-none overflow-hidden data-[state=inactive]:hidden"
          >
            <VerificationTab />
          </Tabs.Content>

          <Tabs.Content
            value="statistics"
            className="h-full outline-none overflow-hidden data-[state=inactive]:hidden"
          >
            <StatisticsTab configId={configId} configName={configName} />
          </Tabs.Content>

          <Tabs.Content
            value="discoveries"
            className="h-full outline-none overflow-y-auto p-4 data-[state=inactive]:hidden"
          >
            <DiscoverySyncPanel />
          </Tabs.Content>
        </div>
      </Tabs.Root>
    </div>
  );
}

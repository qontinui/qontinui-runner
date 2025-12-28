/**
 * ConfigureSubTab.tsx
 *
 * Configuration view that combines:
 * - Workflow Builder (create new AI workflows)
 * - Prompt Builder (create/edit prompts with workflow settings)
 * - Script Builder (create Playwright scripts with AI refinement)
 * - Log Sources (external log sources)
 * - Scriptlets (reusable snippets)
 *
 * Uses tabs to organize the different configuration areas.
 */

import { useState } from "react";
import * as Tabs from "@radix-ui/react-tabs";
import { Sparkles, FolderOpen, Puzzle, FileText, TestTube, Tag } from "lucide-react";
import { AiBuilderTab } from "../AiBuilderTab";
import { PromptLibraryTab } from "../PromptLibraryTab";
import { ScriptBuilderTab } from "../ScriptBuilderTab";
import { ExternalLogsTab } from "../ExternalLogsTab";
import { ScriptletsSubTab } from "./ScriptletsSubTab";
import { CategoryManager } from "../findings/CategoryManager";
import type { UseProjectLogsReturn } from "../../hooks/useProjectLogs";
import type { AiOutputLine } from "../AiOutputTab";

type LogLevel = "info" | "warning" | "error" | "debug" | "success";
type ConfigureTab = "builder" | "prompts" | "scripts" | "logs" | "scriptlets" | "findings";

interface ConfigureSubTabProps {
  projectLogs: UseProjectLogsReturn;
  aiOutputLines: AiOutputLine[];
  onLog: (level: LogLevel, message: string) => void;
  onConfigureLogLocations: () => void;
}

export function ConfigureSubTab({
  projectLogs,
  aiOutputLines,
  onLog,
  onConfigureLogLocations,
}: ConfigureSubTabProps) {
  const [activeTab, setActiveTab] = useState<ConfigureTab>("builder");

  const tabTriggerClass = `
    flex items-center gap-2 px-3 py-3 text-sm font-medium
    border-b-2 transition-colors
    data-[state=active]:border-primary data-[state=active]:text-primary
    data-[state=inactive]:border-transparent data-[state=inactive]:text-muted-foreground
    data-[state=inactive]:hover:text-foreground data-[state=inactive]:hover:bg-muted/30
  `;

  return (
    <div className="h-full flex flex-col overflow-hidden">
      <Tabs.Root
        value={activeTab}
        onValueChange={(value) => setActiveTab(value as ConfigureTab)}
        className="flex-1 flex flex-col min-h-0"
      >
        {/* Tab Navigation */}
        <Tabs.List className="flex border-b border-border bg-muted/30 px-2 flex-shrink-0 overflow-x-auto">
          <Tabs.Trigger value="builder" className={tabTriggerClass}>
            <Sparkles className="w-4 h-4" />
            Workflows
          </Tabs.Trigger>
          <Tabs.Trigger value="prompts" className={tabTriggerClass}>
            <FileText className="w-4 h-4" />
            Prompts
          </Tabs.Trigger>
          <Tabs.Trigger value="scripts" className={tabTriggerClass}>
            <TestTube className="w-4 h-4" />
            Scripts
          </Tabs.Trigger>
          <Tabs.Trigger value="scriptlets" className={tabTriggerClass}>
            <Puzzle className="w-4 h-4" />
            Scriptlets
          </Tabs.Trigger>
          <Tabs.Trigger value="logs" className={tabTriggerClass}>
            <FolderOpen className="w-4 h-4" />
            Log Sources
          </Tabs.Trigger>
          <Tabs.Trigger value="findings" className={tabTriggerClass}>
            <Tag className="w-4 h-4" />
            Findings
          </Tabs.Trigger>
        </Tabs.List>

        {/* Tab Content */}
        <div className="flex-1 min-h-0 overflow-hidden">
          {/* Workflow Builder */}
          <Tabs.Content
            value="builder"
            className="h-full outline-none overflow-y-auto data-[state=inactive]:hidden"
          >
            <AiBuilderTab
              projectLogs={projectLogs}
              onNavigateToLogLocations={() => setActiveTab("logs")}
            />
          </Tabs.Content>

          {/* Prompt Builder */}
          <Tabs.Content
            value="prompts"
            className="h-full outline-none overflow-y-auto data-[state=inactive]:hidden"
          >
            <PromptLibraryTab onLog={onLog} />
          </Tabs.Content>

          {/* Script Builder */}
          <Tabs.Content
            value="scripts"
            className="h-full outline-none overflow-y-auto data-[state=inactive]:hidden"
          >
            <ScriptBuilderTab onLog={onLog} />
          </Tabs.Content>

          {/* Scriptlets */}
          <Tabs.Content
            value="scriptlets"
            className="h-full outline-none overflow-y-auto data-[state=inactive]:hidden"
          >
            <ScriptletsSubTab onLog={onLog} aiOutputLines={aiOutputLines} />
          </Tabs.Content>

          {/* Log Sources */}
          <Tabs.Content
            value="logs"
            className="h-full outline-none overflow-y-auto p-4 data-[state=inactive]:hidden"
          >
            <ExternalLogsTab
              config={projectLogs.config}
              sources={projectLogs.logsState.sources}
              loading={projectLogs.logsState.loading}
              error={projectLogs.logsState.error}
              lastRefresh={projectLogs.logsState.lastRefresh}
              onRefresh={projectLogs.refreshLogs}
              onConfigureSources={onConfigureLogLocations}
            />
          </Tabs.Content>

          {/* Findings Categories */}
          <Tabs.Content
            value="findings"
            className="h-full outline-none overflow-y-auto data-[state=inactive]:hidden"
          >
            <CategoryManager onLog={onLog} />
          </Tabs.Content>
        </div>
      </Tabs.Root>
    </div>
  );
}

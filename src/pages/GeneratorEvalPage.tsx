import * as Tabs from "@radix-ui/react-tabs";
import { BarChart3, Microscope, PenLine, FlaskConical, BookOpen } from "lucide-react";
import { useState } from "react";
import { DashboardTab } from "./generator-eval/DashboardTab";
import { PipelineInspectorTab } from "./generator-eval/PipelineInspectorTab";
import { EditAnalysisTab } from "./generator-eval/EditAnalysisTab";
import { BenchmarksTab } from "./generator-eval/BenchmarksTab";
import { ExampleLibraryTab } from "./generator-eval/ExampleLibraryTab";

const TABS = [
  { id: "dashboard", label: "Dashboard", icon: BarChart3 },
  { id: "inspector", label: "Pipeline Inspector", icon: Microscope },
  { id: "edits", label: "Edit Analysis", icon: PenLine },
  { id: "benchmarks", label: "Benchmarks", icon: FlaskConical },
  { id: "examples", label: "Example Library", icon: BookOpen },
] as const;

export function GeneratorEvalPage() {
  const [activeTab, setActiveTab] = useState<string>("dashboard");

  return (
    <div className="h-full flex flex-col p-4 gap-4">
      <div className="flex items-center gap-3">
        <FlaskConical className="w-5 h-5 text-purple-400" />
        <h1 className="text-lg font-semibold">Generator Evaluation</h1>
        <span className="text-xs px-1.5 py-0.5 rounded bg-purple-500/20 text-purple-400 border border-purple-500/30 font-medium">
          dev
        </span>
      </div>

      <Tabs.Root
        value={activeTab}
        onValueChange={setActiveTab}
        className="flex-1 flex flex-col min-h-0"
      >
        <Tabs.List className="flex gap-1 border-b border-border pb-px shrink-0">
          {TABS.map((tab) => {
            const Icon = tab.icon;
            return (
              <Tabs.Trigger
                key={tab.id}
                value={tab.id}
                className="flex items-center gap-1.5 px-3 py-1.5 text-sm text-muted-foreground
                           hover:text-foreground border-b-2 border-transparent
                           data-[state=active]:text-foreground data-[state=active]:border-purple-500
                           transition-colors"
              >
                <Icon className="w-3.5 h-3.5" />
                {tab.label}
              </Tabs.Trigger>
            );
          })}
        </Tabs.List>

        <Tabs.Content value="dashboard" className="flex-1 min-h-0 pt-4 overflow-auto">
          <DashboardTab />
        </Tabs.Content>
        <Tabs.Content value="inspector" className="flex-1 min-h-0 pt-4 overflow-auto">
          <PipelineInspectorTab />
        </Tabs.Content>
        <Tabs.Content value="edits" className="flex-1 min-h-0 pt-4 overflow-auto">
          <EditAnalysisTab />
        </Tabs.Content>
        <Tabs.Content value="benchmarks" className="flex-1 min-h-0 pt-4 overflow-auto">
          <BenchmarksTab />
        </Tabs.Content>
        <Tabs.Content value="examples" className="flex-1 min-h-0 pt-4 overflow-auto">
          <ExampleLibraryTab />
        </Tabs.Content>
      </Tabs.Root>
    </div>
  );
}

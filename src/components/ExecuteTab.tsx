/**
 * ExecuteTab.tsx
 *
 * Redesigned execution page for GUI automation featuring:
 * - New header with config status indicator (animated ping, active badge)
 * - Two-column layout: WorkflowRunner (2/3) + AutomationToolkit sidebar (1/3)
 * - Tabbed interface for Quick Actions & Macros
 * - Searchable workflow dropdown
 * - Status preview showing workflow execution state
 * - Modern visual styling with large start button (h-16)
 */

import { useState, useEffect, useCallback } from "react";
import { RefreshCw } from "lucide-react";
import { PageTutorialMenu } from "./tutorial";
import { useExecution } from "../contexts/ExecutionContext";
import {
  GuiAutomationHeader,
  WorkflowRunnerPanel,
  AutomationToolkitSidebar,
} from "./gui-automation";
import type { SavedMacro } from "../types";
import { getApiBase } from "@/lib/runner-api";

type LogLevel = "info" | "warning" | "error" | "debug" | "success";

interface ExecuteTabProps {
  onLog: (level: LogLevel, message: string) => void;
  onNavigateToActive: () => void;
}

export function ExecuteTab({ onLog, onNavigateToActive }: ExecuteTabProps) {
  const execution = useExecution();

  // Data state
  const [macros, setMacros] = useState<SavedMacro[]>([]);
  const [macrosLoading, setMacrosLoading] = useState(true);
  const [runningMacroId, setRunningMacroId] = useState<string | null>(null);

  // Fetch macros
  const fetchMacros = useCallback(async () => {
    setMacrosLoading(true);
    try {
      const macrosRes = await fetch(`${getApiBase()}/macros`).catch(() => ({ ok: false }));
      const macrosData = macrosRes.ok ? await (macrosRes as Response).json() : { success: false };

      // Sort by modified_at descending (most recent first)
      if (macrosData.success) {
        const sorted = [...(macrosData.data || [])].sort(
          (a: SavedMacro, b: SavedMacro) =>
            new Date(b.modified_at).getTime() - new Date(a.modified_at).getTime(),
        );
        setMacros(sorted);
      }
    } catch (error) {
      console.error("Failed to fetch macros:", error);
    } finally {
      setMacrosLoading(false);
    }
  }, []);

  useEffect(() => {
    fetchMacros();
  }, [fetchMacros]);

  // Run macro handler
  const runMacro = async (macro: SavedMacro) => {
    setRunningMacroId(macro.id);
    try {
      const response = await fetch(`${getApiBase()}/macros/${macro.id}/run`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({}),
      });

      const result = await response.json();
      if (result.success) {
        const data = result.data;
        if (data.failed_steps === 0) {
          onLog(
            "success",
            `Completed macro: ${macro.name} (${data.successful_steps}/${data.total_steps} steps in ${data.duration_ms}ms)`,
          );
        } else {
          onLog(
            "warning",
            `Macro completed with errors: ${data.failed_steps}/${data.total_steps} steps failed`,
          );
        }
        onNavigateToActive();
      } else {
        throw new Error(result.error || "Failed to run macro");
      }
    } catch (error) {
      onLog("error", `Failed to run macro: ${error}`);
    } finally {
      setRunningMacroId(null);
    }
  };

  return (
    <div className="h-full flex flex-col overflow-hidden">
      {/* Header with config status */}
      <GuiAutomationHeader
        config={execution.config}
        configLoaded={execution.configLoaded}
        onLoadConfiguration={execution.loadConfiguration}
        onLoadLastConfiguration={execution.loadLastConfiguration}
        onLog={onLog}
      />

      {/* Main content area */}
      <div className="flex-1 min-h-0 overflow-auto scrollbar-dark">
        <div className="p-6">
          {/* Header row with refresh button */}
          <div className="flex items-center justify-between mb-6">
            <div className="flex items-center gap-2">
              <PageTutorialMenu page="run" variant="compact" />
            </div>
            <button
              onClick={fetchMacros}
              disabled={macrosLoading}
              className="p-2 text-muted-foreground hover:text-foreground rounded-lg hover:bg-muted/50 transition-colors"
              title="Refresh macros"
            >
              <RefreshCw className={`w-4 h-4 ${macrosLoading ? "animate-spin" : ""}`} />
            </button>
          </div>

          {/* Two-column layout */}
          <div className="max-w-7xl mx-auto grid grid-cols-1 lg:grid-cols-3 gap-6">
            {/* Main Panel - 2/3 width */}
            <div className="lg:col-span-2">
              <WorkflowRunnerPanel
                workflows={execution.workflows}
                automationEnabledCategories={execution.automationEnabledCategories}
                selectedWorkflow={execution.selectedWorkflow}
                configLoaded={execution.configLoaded}
                onWorkflowSelect={(id) => {
                  execution.selectWorkflowWithPersistence(id);
                }}
                selectedMonitors={execution.selectedMonitors}
                onMonitorSelectionChange={(indices) => {
                  if (indices.length > 0) {
                    execution.selectMonitorsWithPersistence(indices);
                  }
                }}
                autoMinimize={execution.autoMinimize}
                onAutoMinimizeChange={execution.setAutoMinimize}
                states={execution.config?.states}
                resolvedInitialStates={execution.resolvedInitialStates}
                initialStatesOverride={execution.initialStatesOverride}
                onInitialStatesOverrideChange={execution.setInitialStatesOverride}
                executionActive={execution.executionActive}
                onStartExecution={execution.startExecution}
                onStopExecution={execution.stopExecution}
                onNavigateToActive={onNavigateToActive}
              />
            </div>

            {/* Sidebar - 1/3 width */}
            <div className="lg:col-span-1">
              <AutomationToolkitSidebar
                config={execution.config}
                configLoaded={execution.configLoaded}
                selectedMonitors={execution.selectedMonitors}
                macros={macros}
                macrosLoading={macrosLoading}
                onRunMacro={runMacro}
                runningMacroId={runningMacroId}
                onLog={onLog}
              />
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}

export default ExecuteTab;

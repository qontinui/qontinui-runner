/**
 * ExecuteTab.tsx
 *
 * GUI automation execution page matching the runner's modern layout:
 * - Compact header with config status
 * - Two-panel layout: WorkflowRunner (main) + collapsible Toolkit sidebar
 * - Bottom status bar with Start/Stop execution controls
 */

import { useState, useEffect, useCallback, useMemo } from "react";
import { Play, Square, ChevronLeft } from "lucide-react";
import { instanceStorage } from "@/lib/instance-storage";
import { PageTutorialMenu } from "./tutorial";
import { useExecution } from "../contexts/ExecutionContext";
import {
  GuiAutomationHeader,
  WorkflowRunnerPanel,
  AutomationToolkitSidebar,
} from "./gui-automation";
import type { SavedMacro } from "../types";
import { getApiBase, tracedFetch } from "@/lib/runner-api";

type LogLevel = "info" | "warning" | "error" | "debug" | "success";

const TOOLKIT_OPEN_KEY = "qontinui-gui-automation-toolkit-open";

interface ExecuteTabProps {
  onLog: (level: LogLevel, message: string) => void;
  onNavigateToActive: () => void;
}

export function ExecuteTab({ onLog, onNavigateToActive }: ExecuteTabProps) {
  const execution = useExecution();

  // Toolkit sidebar visibility — persisted
  const [isToolkitOpen, setIsToolkitOpen] = useState<boolean>(() => {
    return instanceStorage.getJSON(TOOLKIT_OPEN_KEY, true);
  });

  const toggleToolkit = useCallback(() => {
    setIsToolkitOpen((prev) => {
      const next = !prev;
      instanceStorage.setJSON(TOOLKIT_OPEN_KEY, next);
      return next;
    });
  }, []);

  // Macro state
  const [macros, setMacros] = useState<SavedMacro[]>([]);
  const [macrosLoading, setMacrosLoading] = useState(true);
  const [runningMacroId, setRunningMacroId] = useState<string | null>(null);

  const fetchMacros = useCallback(async () => {
    setMacrosLoading(true);
    try {
      const macrosRes = await tracedFetch(`${getApiBase()}/macros`).catch(() => ({ ok: false }));
      const macrosData = macrosRes.ok ? await (macrosRes as Response).json() : { success: false };

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
    // eslint-disable-next-line react-hooks/set-state-in-effect
    fetchMacros();
  }, [fetchMacros]);

  const runMacro = async (macro: SavedMacro) => {
    setRunningMacroId(macro.id);
    try {
      const response = await tracedFetch(`${getApiBase()}/macros/${macro.id}/run`, {
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

  const selectedWorkflow = useMemo(
    () => execution.workflows.find((w) => w.id === execution.selectedWorkflow),
    [execution.workflows, execution.selectedWorkflow],
  );

  const canStart =
    execution.configLoaded && !!execution.selectedWorkflow && !execution.executionActive;

  return (
    <div className="h-full flex flex-col bg-background">
      {/* Header */}
      <GuiAutomationHeader
        config={execution.config}
        configLoaded={execution.configLoaded}
        onLoadConfiguration={execution.loadConfiguration}
        onLoadLastConfiguration={execution.loadLastConfiguration}
        onLog={onLog}
      />

      {/* Main content — two panels */}
      <div className="flex-1 flex overflow-hidden">
        {/* Main panel */}
        <div className="flex-1 overflow-auto scrollbar-dark p-4">
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
          />
        </div>

        {/* Right: Toolkit sidebar (collapsible) */}
        {isToolkitOpen ? (
          <div
            className="shrink-0 border-l border-border overflow-auto scrollbar-dark"
            style={{ width: 320 }}
          >
            <AutomationToolkitSidebar
              config={execution.config}
              configLoaded={execution.configLoaded}
              selectedMonitors={execution.selectedMonitors}
              macros={macros}
              macrosLoading={macrosLoading}
              onRunMacro={runMacro}
              runningMacroId={runningMacroId}
              onLog={onLog}
              onCollapse={toggleToolkit}
              onRefreshMacros={fetchMacros}
            />
          </div>
        ) : (
          <button
            onClick={toggleToolkit}
            className="shrink-0 flex flex-col items-center justify-center gap-2 w-8 border-l border-white/10
                       text-muted-foreground hover:text-foreground hover:bg-white/5 transition-colors"
            title="Expand toolkit"
          >
            <ChevronLeft className="w-4 h-4" />
            <span
              className="text-xs font-medium"
              style={{ writingMode: "vertical-rl", transform: "rotate(180deg)" }}
            >
              Toolkit
            </span>
          </button>
        )}
      </div>

      {/* Bottom bar */}
      <div className="flex items-center justify-between px-4 py-3 border-t border-border bg-card/50">
        <div className="flex items-center gap-3 text-body-sm text-muted-foreground">
          {execution.executionActive ? (
            <div className="flex items-center gap-2">
              <span className="relative flex h-2 w-2">
                <span className="absolute inline-flex h-full w-full animate-ping rounded-full bg-amber-500 opacity-75" />
                <span className="relative inline-flex h-2 w-2 rounded-full bg-amber-500" />
              </span>
              <span className="text-amber-400">
                Running{selectedWorkflow ? `: ${selectedWorkflow.name}` : ""}
              </span>
            </div>
          ) : selectedWorkflow ? (
            <span>Ready: {selectedWorkflow.name}</span>
          ) : execution.configLoaded ? (
            <span>Select a workflow to run</span>
          ) : (
            <span>Load a configuration to begin</span>
          )}
        </div>

        <div className="flex items-center gap-2">
          <PageTutorialMenu page="run" variant="compact" />
          <button
            onClick={execution.stopExecution}
            disabled={!execution.executionActive}
            className="btn-danger flex items-center gap-2"
          >
            <Square className="w-4 h-4" />
            Stop
          </button>
          <button
            onClick={() => {
              execution.startExecution();
              onNavigateToActive();
            }}
            disabled={!canStart}
            className="btn-primary flex items-center gap-2"
            data-tutorial-id="start-execution-button"
          >
            <Play className="w-4 h-4" />
            Start
          </button>
        </div>
      </div>
    </div>
  );
}

export default ExecuteTab;

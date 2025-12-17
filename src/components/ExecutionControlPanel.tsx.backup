/**
 * ExecutionControlPanel Component
 *
 * Handles workflow execution controls (workflow selector, monitor selector, start/stop).
 * Single responsibility: Execution control UI.
 */

import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Play, Square, Cpu, ChevronDown } from "lucide-react";
import CollapsiblePanel from "./CollapsiblePanel";
import { MonitorSelector } from "./MonitorSelector";
import type { Workflow } from "../contexts/ExecutionContext";

export interface ExecutionControlPanelProps {
  collapsed: boolean;
  onToggle: (collapsed: boolean) => void;

  // Workflow selection
  workflows: Workflow[];
  selectedWorkflow: string;
  configLoaded: boolean;
  showWorkflowDropdown: boolean;
  onWorkflowDropdownToggle: (show: boolean) => void;
  onWorkflowSelect: (workflowId: string) => void;

  // Monitor selection
  selectedMonitors: number[];
  onMonitorSelectionChange: (indices: number[]) => void;

  // Auto-minimize
  autoMinimize: boolean;
  onAutoMinimizeChange: (enabled: boolean) => void;

  // Execution controls
  executionActive: boolean;
  onStartExecution: () => void;
  onStopExecution: () => void;
}

export function ExecutionControlPanel({
  collapsed,
  onToggle,
  workflows,
  selectedWorkflow,
  configLoaded,
  showWorkflowDropdown,
  onWorkflowDropdownToggle,
  onWorkflowSelect,
  selectedMonitors,
  onMonitorSelectionChange,
  autoMinimize,
  onAutoMinimizeChange,
  executionActive,
  onStartExecution,
  onStopExecution,
}: ExecutionControlPanelProps) {
  const [calculatedScreens, setCalculatedScreens] = useState<number[]>([]);
  const [isOverrideActive, setIsOverrideActive] = useState(false);
  const [isCalculating, setIsCalculating] = useState(false);
  const [availableMonitors, setAvailableMonitors] = useState<number[]>([]);

  // Load available monitors on mount
  useEffect(() => {
    const loadMonitors = async () => {
      try {
        const result = await invoke<{
          success: boolean;
          data?: { monitors: { index: number; is_primary: boolean }[] };
        }>("get_monitors");
        if (result?.success && result.data?.monitors) {
          setAvailableMonitors(result.data.monitors.map((m) => m.index));
        }
      } catch (err) {
        console.error("Failed to load monitors:", err);
      }
    };
    loadMonitors();
  }, []);

  // Reset override mode when workflow changes
  useEffect(() => {
    setIsOverrideActive(false);
  }, [selectedWorkflow]);

  // Calculate required screens when workflow changes
  useEffect(() => {
    if (!selectedWorkflow || !configLoaded) {
      setCalculatedScreens([]);
      return;
    }

    const calculateScreens = async () => {
      setIsCalculating(true);
      try {
        const result = await invoke<{ success: boolean; data?: { screens: number[] } }>(
          "get_workflow_required_screens",
          { workflowId: selectedWorkflow },
        );

        if (result?.success && result.data?.screens && result.data.screens.length > 0) {
          setCalculatedScreens(result.data.screens);
          // Auto-select calculated screens (override mode is reset above)
          onMonitorSelectionChange(result.data.screens);
        } else {
          // If no screens calculated from workflow, default to primary (index 0)
          const defaultScreen = availableMonitors.length > 0 ? [availableMonitors[0]] : [0];
          setCalculatedScreens(defaultScreen);
          onMonitorSelectionChange(defaultScreen);
        }
      } catch (err) {
        console.error("Failed to calculate required screens:", err);
        // Default to primary monitor on error
        const defaultScreen = availableMonitors.length > 0 ? [availableMonitors[0]] : [0];
        setCalculatedScreens(defaultScreen);
        onMonitorSelectionChange(defaultScreen);
      } finally {
        setIsCalculating(false);
      }
    };

    calculateScreens();
  }, [selectedWorkflow, configLoaded, availableMonitors]);

  // Handle override toggle
  const handleOverrideToggle = () => {
    const newOverrideState = !isOverrideActive;
    setIsOverrideActive(newOverrideState);

    // If turning off override, reset to calculated screens
    if (!newOverrideState && calculatedScreens.length > 0) {
      onMonitorSelectionChange(calculatedScreens);
    }
  };

  // Determine if monitor selector is read-only (read-only when workflow selected and not in override mode)
  const monitorSelectorReadOnly = !!selectedWorkflow && !isOverrideActive;

  return (
    <CollapsiblePanel
      title="Execution Control"
      icon={<Cpu className="w-4 h-4" />}
      collapsed={collapsed}
      onToggle={onToggle}
    >
      <div className="space-y-4">
        {/* Workflow Selector */}
        <div className="relative">
          <button
            onClick={() => onWorkflowDropdownToggle(!showWorkflowDropdown)}
            disabled={!configLoaded || workflows.length === 0}
            className="w-full btn-secondary flex items-center justify-between gap-2"
          >
            <span className="truncate">
              {selectedWorkflow
                ? workflows.find((w) => w.id === selectedWorkflow)?.name || "Select Workflow"
                : "Select Workflow"}
            </span>
            <ChevronDown className="w-4 h-4 flex-shrink-0" />
          </button>

          {showWorkflowDropdown && (
            <div className="absolute z-10 w-full mt-1 bg-card border border-border rounded-lg shadow-lg max-h-60 overflow-y-auto">
              {workflows.map((workflow) => (
                <button
                  key={workflow.id}
                  onClick={() => onWorkflowSelect(workflow.id)}
                  className="w-full px-4 py-2 text-left hover:bg-accent hover:text-accent-foreground transition-colors"
                >
                  {workflow.name}
                </button>
              ))}
            </div>
          )}
        </div>

        {/* Monitor Selector */}
        <div className="space-y-2">
          {selectedWorkflow && !isOverrideActive && (
            <div className="text-xs text-muted-foreground">
              {isCalculating ? (
                <span>Calculating required monitors...</span>
              ) : (
                <span>
                  This workflow will use monitor{calculatedScreens.length > 1 ? "s" : ""}:{" "}
                  {calculatedScreens.map((s) => `#${s}`).join(", ")}
                </span>
              )}
            </div>
          )}
          <MonitorSelector
            selectedMonitors={selectedMonitors}
            onSelectionChange={onMonitorSelectionChange}
            multiSelect={true}
            compact={true}
            readOnly={monitorSelectorReadOnly}
            isOverrideActive={isOverrideActive}
            onOverrideToggle={selectedWorkflow ? handleOverrideToggle : undefined}
          />
        </div>

        {/* Auto-minimize Toggle */}
        {selectedMonitors.length === 1 && (
          <label className="flex items-center gap-2 text-sm">
            <input
              type="checkbox"
              checked={autoMinimize}
              onChange={(e) => onAutoMinimizeChange(e.target.checked)}
              className="rounded"
            />
            <span>Auto-minimize window on start</span>
          </label>
        )}

        {/* Start/Stop Buttons */}
        <div className="flex gap-2">
          <button
            onClick={onStartExecution}
            disabled={executionActive || !configLoaded || !selectedWorkflow}
            className="flex-1 btn-success flex items-center justify-center gap-2"
          >
            <Play className="w-4 h-4" />
            Start
          </button>
          <button
            onClick={onStopExecution}
            disabled={!executionActive}
            className="flex-1 btn-danger flex items-center justify-center gap-2"
          >
            <Square className="w-4 h-4" />
            Stop
          </button>
        </div>
      </div>
    </CollapsiblePanel>
  );
}

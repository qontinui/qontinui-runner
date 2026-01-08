/**
 * ExecutionControlPanel Component
 *
 * Handles workflow execution controls (workflow selector, monitor selector, initial states, start/stop).
 * Single responsibility: Execution control UI.
 */

import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Play, Square, Cpu, ChevronDown, CircleDot } from "lucide-react";
import { getAccentColors } from "@/design-system";
import CollapsiblePanel from "./CollapsiblePanel";
import { MonitorSelector } from "./MonitorSelector";
import { InitialStatesSelector } from "./InitialStatesSelector";
import type { Workflow, ConfigState } from "../contexts/ExecutionContext";
import type { ResolvedInitialStates } from "../types/state-machine";

export interface ExecutionControlPanelProps {
  // Workflow selection
  workflows: Workflow[];
  automationEnabledCategories: string[];
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

  // Navigation after start
  onNavigateToActive?: () => void;

  // Initial states (optional - only show if provided)
  states?: ConfigState[];
  resolvedInitialStates?: ResolvedInitialStates;
  initialStatesOverride?: string[] | null;
  onInitialStatesOverrideChange?: (ids: string[] | null) => void;
}

export function ExecutionControlPanel({
  workflows,
  automationEnabledCategories,
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
  onNavigateToActive,
  states,
  resolvedInitialStates,
  initialStatesOverride,
  onInitialStatesOverrideChange,
}: ExecutionControlPanelProps) {
  const [calculatedScreens, setCalculatedScreens] = useState<number[]>([]);
  const [isCalculating, setIsCalculating] = useState(false);
  const [availableMonitors, setAvailableMonitors] = useState<number[]>([]);
  const [initialStatesExpanded, setInitialStatesExpanded] = useState(false);
  const [expandedCategory, setExpandedCategory] = useState<string | null>(null);

  // Debug logging
  console.log("[EXEC_PANEL] automationEnabledCategories:", automationEnabledCategories);
  console.log("[EXEC_PANEL] workflows count:", workflows.length);

  // Determine if we need nested menus (more than one category)
  const hasMultipleCategories = automationEnabledCategories.length > 1;
  console.log("[EXEC_PANEL] hasMultipleCategories:", hasMultipleCategories);

  // Group workflows by category for nested display
  const workflowsByCategory = hasMultipleCategories
    ? automationEnabledCategories.reduce(
        (acc, category) => {
          acc[category] = workflows.filter(
            (w) => w.category?.toLowerCase() === category.toLowerCase(),
          );
          return acc;
        },
        {} as Record<string, Workflow[]>,
      )
    : null;

  // Show initial states section only if all required props are provided
  const showInitialStates =
    states && resolvedInitialStates && onInitialStatesOverrideChange && selectedWorkflow;

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
          // Auto-select calculated screens
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

  return (
    <CollapsiblePanel
      title="Execution Control"
      icon={<Cpu className="w-4 h-4" />}
      collapsible={false}
    >
      <div className="space-y-3">
        {/* Workflow Selector */}
        <div className="relative">
          <button
            onClick={() => onWorkflowDropdownToggle(!showWorkflowDropdown)}
            disabled={!configLoaded || workflows.length === 0}
            className="w-full flex items-center justify-between gap-2 px-3 py-2 text-sm rounded-lg bg-muted/50 hover:bg-muted transition-colors disabled:opacity-50"
          >
            <span className="truncate">
              {selectedWorkflow
                ? workflows.find((w) => w.id === selectedWorkflow)?.name || "Select Workflow"
                : "Select Workflow"}
            </span>
            <ChevronDown className="w-4 h-4 flex-shrink-0" />
          </button>

          {showWorkflowDropdown && (
            <div className="absolute z-10 w-full mt-1 bg-card/95 backdrop-blur rounded-lg shadow-lg max-h-60 overflow-y-auto scrollbar-dark">
              {hasMultipleCategories && workflowsByCategory
                ? // Nested menus for multiple categories
                  automationEnabledCategories.map((category) => {
                    const categoryWorkflows = workflowsByCategory[category] || [];
                    const isExpanded = expandedCategory === category;
                    const displayName = category.charAt(0).toUpperCase() + category.slice(1);

                    return (
                      <div key={category}>
                        <button
                          onClick={() => setExpandedCategory(isExpanded ? null : category)}
                          className="w-full flex items-center justify-between px-3 py-2 text-sm text-left hover:bg-muted/50 transition-colors font-medium"
                        >
                          <span>{displayName}</span>
                          <div className="flex items-center gap-2">
                            <span className="text-[10px] text-muted-foreground">
                              {categoryWorkflows.length}
                            </span>
                            <ChevronDown
                              className={`w-3.5 h-3.5 transition-transform ${isExpanded ? "rotate-180" : ""}`}
                            />
                          </div>
                        </button>
                        {isExpanded && (
                          <div className="bg-muted/20">
                            {categoryWorkflows.map((workflow) => (
                              <button
                                key={workflow.id}
                                onClick={() => {
                                  onWorkflowSelect(workflow.id);
                                  setExpandedCategory(null);
                                }}
                                className={`w-full px-5 py-1.5 text-sm text-left hover:bg-muted/50 transition-colors ${
                                  selectedWorkflow === workflow.id
                                    ? "bg-primary/10 text-primary"
                                    : ""
                                }`}
                              >
                                {workflow.name}
                              </button>
                            ))}
                            {categoryWorkflows.length === 0 && (
                              <div className="px-5 py-1.5 text-xs text-muted-foreground italic">
                                No workflows
                              </div>
                            )}
                          </div>
                        )}
                      </div>
                    );
                  })
                : // Flat list for single category
                  workflows.map((workflow) => (
                    <button
                      key={workflow.id}
                      onClick={() => onWorkflowSelect(workflow.id)}
                      className={`w-full px-3 py-2 text-sm text-left hover:bg-muted/50 transition-colors ${
                        selectedWorkflow === workflow.id ? "bg-primary/10 text-primary" : ""
                      }`}
                    >
                      {workflow.name}
                    </button>
                  ))}
            </div>
          )}
        </div>

        {/* Monitor Selector */}
        <div className="space-y-1.5">
          {selectedWorkflow && (
            <div className="text-[10px] text-muted-foreground">
              {isCalculating ? (
                <span>Calculating required monitors...</span>
              ) : calculatedScreens.length > 0 ? (
                <span>
                  Using monitor{calculatedScreens.length > 1 ? "s" : ""}:{" "}
                  {calculatedScreens.map((s) => `#${s}`).join(", ")}
                </span>
              ) : (
                <span>Monitors configured per element</span>
              )}
            </div>
          )}
          <MonitorSelector
            selectedMonitors={selectedMonitors}
            onSelectionChange={onMonitorSelectionChange}
            multiSelect={true}
            compact={true}
            readOnly={!!selectedWorkflow}
          />
        </div>

        {/* Initial States Section */}
        {showInitialStates && (
          <div className="rounded-lg bg-muted/30 overflow-hidden">
            <button
              onClick={() => setInitialStatesExpanded(!initialStatesExpanded)}
              className="w-full flex items-center justify-between px-3 py-2 hover:bg-muted/50 transition-colors"
            >
              <div className="flex items-center gap-2">
                <CircleDot className="w-3.5 h-3.5 text-muted-foreground" />
                <span className="text-xs font-medium">Initial States</span>
                {resolvedInitialStates.stateIds.length > 0 && (
                  <span className="text-[10px] text-muted-foreground bg-muted px-1.5 py-0.5 rounded">
                    {initialStatesOverride !== null
                      ? `${initialStatesOverride.length} override`
                      : `${resolvedInitialStates.stateIds.length}`}
                  </span>
                )}
              </div>
              <ChevronDown
                className={`w-3.5 h-3.5 text-muted-foreground transition-transform ${
                  initialStatesExpanded ? "rotate-180" : ""
                }`}
              />
            </button>
            {initialStatesExpanded && (
              <div className="px-3 pb-3">
                <InitialStatesSelector
                  states={states}
                  resolvedStates={resolvedInitialStates}
                  overrideStateIds={initialStatesOverride ?? null}
                  onOverrideChange={onInitialStatesOverrideChange}
                  disabled={executionActive}
                />
              </div>
            )}
          </div>
        )}

        {/* Auto-minimize Toggle */}
        <label className="flex items-center gap-2 text-xs text-muted-foreground">
          <input
            type="checkbox"
            checked={autoMinimize}
            onChange={(e) => onAutoMinimizeChange(e.target.checked)}
            className="rounded w-3.5 h-3.5"
          />
          <span>Auto-minimize on start</span>
        </label>

        {/* Start/Stop Buttons */}
        <div className="flex gap-2">
          <button
            onClick={() => {
              onStartExecution();
              onNavigateToActive?.();
            }}
            disabled={executionActive || !configLoaded || !selectedWorkflow}
            className={`flex-1 flex items-center justify-center gap-2 px-3 py-2 text-xs rounded-lg transition-colors disabled:opacity-50 ${getAccentColors("green").bg} ${getAccentColors("green").text} hover:opacity-80`}
          >
            <Play className="w-3.5 h-3.5" />
            Start
          </button>
          <button
            onClick={onStopExecution}
            disabled={!executionActive}
            className={`flex-1 flex items-center justify-center gap-2 px-3 py-2 text-xs rounded-lg transition-colors disabled:opacity-50 ${getAccentColors("red").bg} ${getAccentColors("red").text} hover:opacity-80`}
          >
            <Square className="w-3.5 h-3.5" />
            Stop
          </button>
        </div>
      </div>
    </CollapsiblePanel>
  );
}

/**
 * WorkflowRunnerPanel Component
 *
 * Main execution panel combining workflow selection and execution controls.
 * Features:
 * - Large workflow selector button with dashed border that opens searchable dropdown
 * - Monitor selector (reuses MonitorSelector component)
 * - Advanced Settings accordion with InitialStatesSelector and auto-minimize
 * - Large START/STOP execution buttons (h-16)
 * - Status preview showing workflow execution state
 */

import { useState, useEffect, useMemo } from "react";
import { invoke } from "@tauri-apps/api/core";
import { ChevronDown, Search, Settings2, CircleDot, Cpu } from "lucide-react";
import { Card, CardHeader, CardTitle, CardContent } from "../ui/Card";
import { MonitorSelector } from "../MonitorSelector";
import { InitialStatesSelector } from "../InitialStatesSelector";
import { cn } from "../../lib/utils";
import type { Workflow, ConfigState } from "../../contexts/ExecutionContext";
import type { ResolvedInitialStates } from "../../types/state-machine";

interface WorkflowRunnerPanelProps {
  // Workflow data
  workflows: Workflow[];
  automationEnabledCategories: string[];
  selectedWorkflow: string;
  configLoaded: boolean;
  onWorkflowSelect: (workflowId: string) => void;

  // Monitor selection
  selectedMonitors: number[];
  onMonitorSelectionChange: (indices: number[]) => void;

  // Auto-minimize
  autoMinimize: boolean;
  onAutoMinimizeChange: (enabled: boolean) => void;

  // Initial states
  states?: ConfigState[];
  resolvedInitialStates?: ResolvedInitialStates;
  initialStatesOverride?: string[] | null;
  onInitialStatesOverrideChange?: (ids: string[] | null) => void;

  // Execution state (for disabling controls during execution)
  executionActive: boolean;
}

export function WorkflowRunnerPanel({
  workflows,
  automationEnabledCategories,
  selectedWorkflow,
  configLoaded,
  onWorkflowSelect,
  selectedMonitors,
  onMonitorSelectionChange,
  autoMinimize,
  onAutoMinimizeChange,
  states,
  resolvedInitialStates,
  initialStatesOverride,
  onInitialStatesOverrideChange,
  executionActive,
}: WorkflowRunnerPanelProps) {
  // Local state
  const [showWorkflowDropdown, setShowWorkflowDropdown] = useState(false);
  const [searchQuery, setSearchQuery] = useState("");
  const [expandedCategory, setExpandedCategory] = useState<string | null>(null);
  const [advancedSettingsOpen, setAdvancedSettingsOpen] = useState(false);
  // `fetchedScreens` is keyed by the workflow id it came from, so we can
  // derive `calculatedScreens` as a pure value — no setState(default) in the
  // effect body when deps become invalid (react-hooks/set-state-in-effect).
  const [fetchedScreens, setFetchedScreens] = useState<{
    workflowId: string | null;
    screens: number[];
  }>({ workflowId: null, screens: [] });
  const [isCalculating, setIsCalculating] = useState(false);
  const [availableMonitors, setAvailableMonitors] = useState<number[]>([]);

  // Get selected workflow object
  const selectedWorkflowObj = useMemo(
    () => workflows.find((w) => w.id === selectedWorkflow),
    [workflows, selectedWorkflow],
  );

  // Determine if we need nested menus (more than one category)
  const hasMultipleCategories = automationEnabledCategories.length > 1;

  // Group workflows by category for nested display
  const workflowsByCategory = useMemo(() => {
    if (!hasMultipleCategories) return null;
    return automationEnabledCategories.reduce(
      (acc, category) => {
        acc[category] = workflows.filter(
          (w) => w.category?.toLowerCase() === category.toLowerCase(),
        );
        return acc;
      },
      {} as Record<string, Workflow[]>,
    );
  }, [automationEnabledCategories, hasMultipleCategories, workflows]);

  // Filter workflows by search query
  const filteredWorkflows = useMemo(() => {
    if (!searchQuery.trim()) return workflows;
    const query = searchQuery.toLowerCase();
    return workflows.filter(
      (w) => w.name.toLowerCase().includes(query) || w.category?.toLowerCase().includes(query),
    );
  }, [workflows, searchQuery]);

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

  // Calculate required screens when workflow changes. setIsCalculating lives
  // inside the IIFE and the invalid-deps branch returns without setState;
  // the final display derives from `fetchedScreens` keyed by workflow id.
  useEffect(() => {
    if (!selectedWorkflow || !configLoaded) return;
    let cancelled = false;

    (async () => {
      setIsCalculating(true);
      try {
        const result = await invoke<{ success: boolean; data?: { screens: number[] } }>(
          "get_workflow_required_screens",
          { workflowId: selectedWorkflow },
        );
        if (cancelled) return;

        if (result?.success && result.data?.screens && result.data.screens.length > 0) {
          setFetchedScreens({ workflowId: selectedWorkflow, screens: result.data.screens });
          onMonitorSelectionChange(result.data.screens);
        } else {
          const defaultScreen = availableMonitors.length > 0 ? [availableMonitors[0]] : [0];
          setFetchedScreens({ workflowId: selectedWorkflow, screens: defaultScreen });
          onMonitorSelectionChange(defaultScreen);
        }
      } catch (err) {
        console.error("Failed to calculate required screens:", err);
        if (cancelled) return;
        const defaultScreen = availableMonitors.length > 0 ? [availableMonitors[0]] : [0];
        setFetchedScreens({ workflowId: selectedWorkflow, screens: defaultScreen });
        onMonitorSelectionChange(defaultScreen);
      } finally {
        if (!cancelled) setIsCalculating(false);
      }
    })();
    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [selectedWorkflow, configLoaded, availableMonitors]);

  const calculatedScreens =
    selectedWorkflow && fetchedScreens.workflowId === selectedWorkflow
      ? fetchedScreens.screens
      : [];

  const handleWorkflowSelect = (workflowId: string) => {
    onWorkflowSelect(workflowId);
    setShowWorkflowDropdown(false);
    setSearchQuery("");
    setExpandedCategory(null);
  };

  return (
    <Card>
      <CardHeader>
        <CardTitle>
          <Cpu className="w-5 h-5 text-primary" />
          Workflow Runner
        </CardTitle>
      </CardHeader>
      <CardContent className="space-y-4">
        {/* Workflow Selector - Large button with dashed border */}
        <div className="relative" data-tutorial-id="workflow-selector">
          <button
            onClick={() => setShowWorkflowDropdown(!showWorkflowDropdown)}
            disabled={!configLoaded || workflows.length === 0}
            className={cn(
              "w-full h-14 flex items-center justify-between px-4",
              "text-sm rounded-lg transition-all",
              "border-2 border-dashed border-border bg-secondary/30",
              "hover:border-primary/50 hover:bg-secondary/50",
              "disabled:opacity-50 disabled:cursor-not-allowed",
            )}
          >
            <span
              className={cn(
                "truncate",
                selectedWorkflowObj ? "text-foreground font-medium" : "text-muted-foreground",
              )}
            >
              {selectedWorkflowObj?.name || "Select a workflow to run..."}
            </span>
            <ChevronDown
              className={cn(
                "w-5 h-5 shrink-0 transition-transform",
                showWorkflowDropdown && "rotate-180",
              )}
            />
          </button>

          {/* Workflow Dropdown with search */}
          {showWorkflowDropdown && (
            <div className="absolute z-20 w-full mt-2 bg-card border border-border rounded-lg shadow-xl max-h-80 overflow-hidden">
              {/* Search input */}
              <div className="p-2 border-b border-border">
                <div className="relative">
                  <Search className="absolute left-3 top-1/2 -translate-y-1/2 w-4 h-4 text-muted-foreground" />
                  <input
                    type="text"
                    value={searchQuery}
                    onChange={(e) => setSearchQuery(e.target.value)}
                    placeholder="Search workflows..."
                    className="w-full pl-9 pr-3 py-2 text-sm bg-secondary/50 border border-border rounded-md focus:outline-hidden focus:ring-1 focus:ring-primary"
                    autoFocus
                  />
                </div>
              </div>

              {/* Workflow list */}
              <div className="max-h-60 overflow-y-auto scrollbar-dark">
                {hasMultipleCategories && workflowsByCategory && !searchQuery
                  ? // Nested menus for multiple categories (only when not searching)
                    automationEnabledCategories.map((category) => {
                      const categoryWorkflows = workflowsByCategory[category] || [];
                      const isExpanded = expandedCategory === category;
                      const displayName = category.charAt(0).toUpperCase() + category.slice(1);

                      return (
                        <div key={category}>
                          <button
                            onClick={() => setExpandedCategory(isExpanded ? null : category)}
                            className="w-full flex items-center justify-between px-4 py-2.5 text-sm text-left hover:bg-muted/50 transition-colors font-medium"
                          >
                            <span>{displayName}</span>
                            <div className="flex items-center gap-2">
                              <span className="text-xs text-muted-foreground bg-muted px-1.5 py-0.5 rounded">
                                {categoryWorkflows.length}
                              </span>
                              <ChevronDown
                                className={cn(
                                  "w-4 h-4 transition-transform",
                                  isExpanded && "rotate-180",
                                )}
                              />
                            </div>
                          </button>
                          {isExpanded && (
                            <div className="bg-muted/20">
                              {categoryWorkflows.map((workflow) => (
                                <button
                                  key={workflow.id}
                                  onClick={() => handleWorkflowSelect(workflow.id)}
                                  className={cn(
                                    "w-full px-6 py-2 text-sm text-left hover:bg-muted/50 transition-colors",
                                    selectedWorkflow === workflow.id &&
                                      "bg-primary/10 text-primary",
                                  )}
                                >
                                  {workflow.name}
                                </button>
                              ))}
                              {categoryWorkflows.length === 0 && (
                                <div className="px-6 py-2 text-xs text-muted-foreground italic">
                                  No workflows
                                </div>
                              )}
                            </div>
                          )}
                        </div>
                      );
                    })
                  : // Flat list (single category or search results)
                    filteredWorkflows.map((workflow) => (
                      <button
                        key={workflow.id}
                        onClick={() => handleWorkflowSelect(workflow.id)}
                        className={cn(
                          "w-full px-4 py-2.5 text-sm text-left hover:bg-muted/50 transition-colors",
                          selectedWorkflow === workflow.id && "bg-primary/10 text-primary",
                        )}
                      >
                        <div className="flex items-center justify-between">
                          <span>{workflow.name}</span>
                          {workflow.category && (
                            <span className="text-xs text-muted-foreground bg-muted px-1.5 py-0.5 rounded">
                              {workflow.category}
                            </span>
                          )}
                        </div>
                      </button>
                    ))}

                {filteredWorkflows.length === 0 && (
                  <div className="px-4 py-8 text-center text-sm text-muted-foreground">
                    No workflows found
                  </div>
                )}
              </div>
            </div>
          )}
        </div>

        {/* Monitor Selector */}
        <div className="space-y-2" data-tutorial-id="monitor-selector">
          <div className="text-xs text-muted-foreground">
            {isCalculating ? (
              <span>Calculating required monitors...</span>
            ) : selectedWorkflow && calculatedScreens.length > 0 ? (
              <span>
                Using monitor{calculatedScreens.length > 1 ? "s" : ""}:{" "}
                {calculatedScreens.map((s) => `#${s}`).join(", ")}
              </span>
            ) : (
              <span>Select target monitors</span>
            )}
          </div>
          <MonitorSelector
            selectedMonitors={selectedMonitors}
            onSelectionChange={onMonitorSelectionChange}
            multiSelect={true}
            compact={true}
            readOnly={!!selectedWorkflow}
          />
        </div>

        {/* Advanced Settings Accordion */}
        <div className="rounded-lg border border-border overflow-hidden">
          <button
            onClick={() => setAdvancedSettingsOpen(!advancedSettingsOpen)}
            className="w-full flex items-center justify-between px-4 py-3 text-sm font-medium hover:bg-muted/50 transition-colors"
          >
            <div className="flex items-center gap-2">
              <Settings2 className="w-4 h-4 text-muted-foreground" />
              <span>Advanced Settings</span>
            </div>
            <ChevronDown
              className={cn(
                "w-4 h-4 text-muted-foreground transition-transform",
                advancedSettingsOpen && "rotate-180",
              )}
            />
          </button>

          {advancedSettingsOpen && (
            <div className="px-4 pb-4 space-y-4 border-t border-border bg-muted/20">
              {/* Initial States Section */}
              {showInitialStates && (
                <div className="pt-4">
                  <div className="flex items-center gap-2 mb-2">
                    <CircleDot className="w-4 h-4 text-muted-foreground" />
                    <span className="text-sm font-medium">Initial States</span>
                    {resolvedInitialStates.stateIds.length > 0 && (
                      <span className="text-[10px] text-muted-foreground bg-muted px-1.5 py-0.5 rounded">
                        {initialStatesOverride != null
                          ? `${initialStatesOverride.length} override`
                          : `${resolvedInitialStates.stateIds.length}`}
                      </span>
                    )}
                  </div>
                  <InitialStatesSelector
                    states={states}
                    resolvedStates={resolvedInitialStates}
                    overrideStateIds={initialStatesOverride ?? null}
                    onOverrideChange={onInitialStatesOverrideChange}
                    disabled={executionActive}
                  />
                </div>
              )}

              {/* Auto-minimize Toggle */}
              <label className="flex items-center gap-3 pt-2">
                <input
                  type="checkbox"
                  checked={autoMinimize}
                  onChange={(e) => onAutoMinimizeChange(e.target.checked)}
                  className="rounded w-4 h-4"
                />
                <span className="text-sm">Auto-minimize window on start</span>
              </label>
            </div>
          )}
        </div>
      </CardContent>
    </Card>
  );
}

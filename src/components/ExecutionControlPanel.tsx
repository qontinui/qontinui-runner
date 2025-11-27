/**
 * ExecutionControlPanel Component
 *
 * Handles workflow execution controls (workflow selector, monitor selector, start/stop).
 * Single responsibility: Execution control UI.
 */

import { Play, Square, Cpu, ChevronDown } from "lucide-react";
import CollapsiblePanel from "./CollapsiblePanel";
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
  selectedMonitor: number;
  availableMonitors: number[];
  showMonitorDropdown: boolean;
  onMonitorDropdownToggle: (show: boolean) => void;
  onMonitorSelect: (index: number) => void;

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
  selectedMonitor,
  availableMonitors,
  showMonitorDropdown,
  onMonitorDropdownToggle,
  onMonitorSelect,
  autoMinimize,
  onAutoMinimizeChange,
  executionActive,
  onStartExecution,
  onStopExecution,
}: ExecutionControlPanelProps) {
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
        <div className="relative">
          <button
            onClick={() => onMonitorDropdownToggle(!showMonitorDropdown)}
            className="w-full btn-secondary flex items-center justify-between gap-2"
          >
            <span>Monitor {selectedMonitor}</span>
            <ChevronDown className="w-4 h-4" />
          </button>

          {showMonitorDropdown && (
            <div className="absolute z-10 w-full mt-1 bg-card border border-border rounded-lg shadow-lg">
              {availableMonitors.map((index) => (
                <button
                  key={index}
                  onClick={() => onMonitorSelect(index)}
                  className="w-full px-4 py-2 text-left hover:bg-accent hover:text-accent-foreground transition-colors"
                >
                  Monitor {index}
                </button>
              ))}
            </div>
          )}
        </div>

        {/* Auto-minimize Toggle */}
        {availableMonitors.length === 1 && (
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

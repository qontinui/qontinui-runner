/**
 * FlowSidebar.tsx
 *
 * Sidebar with step palette and properties panel for Flow Designer.
 */

import { useState } from "react";
import {
  Bot,
  Wrench,
  GitBranch,
  GitMerge,
  User,
  Shuffle,
  Clock,
  Repeat,
  Square,
  XSquare,
  ChevronRight,
  Trash2,
} from "lucide-react";
import type { FlowStep, StepType, Condition } from "../../types/flow";
import { STEP_TYPE_COLORS, STEP_TYPE_LABELS, createExpressionCondition } from "../../types/flow";

interface FlowSidebarProps {
  selectedStep: FlowStep | null;
  onAddStep: (stepType: string) => void;
  onUpdateStep: (step: FlowStep) => void;
  onDeleteStep: (stepId: string) => void;
}

const STEP_PALETTE = [
  { type: "agent", icon: Bot, description: "AI agent with role and prompt" },
  { type: "tool", icon: Wrench, description: "Execute a tool" },
  { type: "conditional", icon: GitBranch, description: "If/else branching" },
  { type: "parallel", icon: GitMerge, description: "Run steps in parallel" },
  { type: "human_input", icon: User, description: "Wait for user input" },
  { type: "transform", icon: Shuffle, description: "Transform data" },
  { type: "wait", icon: Clock, description: "Delay execution" },
  { type: "loop", icon: Repeat, description: "Loop over collection" },
  { type: "end", icon: Square, description: "End flow successfully" },
  { type: "fail", icon: XSquare, description: "End flow with error" },
];

function StepPalette({ onAddStep }: { onAddStep: (type: string) => void }) {
  return (
    <div className="space-y-2">
      <h3 className="text-xs font-medium text-gray-400 uppercase tracking-wider px-2">Add Step</h3>
      <div className="grid grid-cols-2 gap-2">
        {STEP_PALETTE.map(({ type, icon: Icon, description }) => (
          <button
            key={type}
            onClick={() => onAddStep(type)}
            className="flex flex-col items-center gap-1 p-2 rounded-lg bg-gray-800 hover:bg-gray-700 transition-colors group"
            title={description}
          >
            <Icon
              className="w-5 h-5 group-hover:scale-110 transition-transform"
              style={{ color: STEP_TYPE_COLORS[type] }}
            />
            <span className="text-xs text-gray-400">{STEP_TYPE_LABELS[type]}</span>
          </button>
        ))}
      </div>
    </div>
  );
}

interface StepPropertiesProps {
  step: FlowStep;
  onUpdate: (step: FlowStep) => void;
  onDelete: () => void;
}

function StepProperties({ step, onUpdate, onDelete }: StepPropertiesProps) {
  const stepType = step.step_type.type;
  const color = STEP_TYPE_COLORS[stepType] || "#6b7280";

  const handleFieldChange = (field: keyof FlowStep, value: unknown) => {
    onUpdate({ ...step, [field]: value });
  };

  const handleStepTypeFieldChange = (field: string, value: unknown) => {
    onUpdate({
      ...step,
      step_type: { ...step.step_type, [field]: value } as StepType,
    });
  };

  return (
    <div className="space-y-4">
      <div className="flex items-center justify-between">
        <h3 className="text-sm font-medium" style={{ color }}>
          {STEP_TYPE_LABELS[stepType]} Properties
        </h3>
        <button
          onClick={onDelete}
          className="p-1 text-gray-400 hover:text-red-500 hover:bg-gray-700 rounded transition-colors"
          title="Delete step"
        >
          <Trash2 className="w-4 h-4" />
        </button>
      </div>

      {/* Common Properties */}
      <div className="space-y-3">
        <div>
          <label className="block text-xs text-gray-400 mb-1">ID</label>
          <input
            type="text"
            value={step.id}
            readOnly
            className="w-full bg-gray-800 border border-gray-700 rounded px-2 py-1.5 text-sm text-gray-500"
          />
        </div>

        <div>
          <label className="block text-xs text-gray-400 mb-1">Name</label>
          <input
            type="text"
            value={step.name}
            onChange={(e) => handleFieldChange("name", e.target.value)}
            className="w-full bg-gray-800 border border-gray-700 rounded px-2 py-1.5 text-sm text-white focus:outline-none focus:ring-1 focus:ring-blue-500"
          />
        </div>

        <div>
          <label className="block text-xs text-gray-400 mb-1">Description</label>
          <textarea
            value={step.description || ""}
            onChange={(e) => handleFieldChange("description", e.target.value || null)}
            rows={2}
            className="w-full bg-gray-800 border border-gray-700 rounded px-2 py-1.5 text-sm text-white focus:outline-none focus:ring-1 focus:ring-blue-500 resize-none"
          />
        </div>
      </div>

      {/* Step-Specific Properties */}
      {stepType === "agent" && "role" in step.step_type && (
        <div className="space-y-3 border-t border-gray-700 pt-3">
          <div>
            <label className="block text-xs text-gray-400 mb-1">Role</label>
            <input
              type="text"
              value={step.step_type.role}
              onChange={(e) => handleStepTypeFieldChange("role", e.target.value)}
              className="w-full bg-gray-800 border border-gray-700 rounded px-2 py-1.5 text-sm text-white focus:outline-none focus:ring-1 focus:ring-blue-500"
            />
          </div>
          <div>
            <label className="block text-xs text-gray-400 mb-1">Prompt</label>
            <textarea
              value={step.step_type.prompt}
              onChange={(e) => handleStepTypeFieldChange("prompt", e.target.value)}
              rows={3}
              className="w-full bg-gray-800 border border-gray-700 rounded px-2 py-1.5 text-sm text-white focus:outline-none focus:ring-1 focus:ring-blue-500 resize-none"
            />
          </div>
        </div>
      )}

      {stepType === "tool" && "tool_id" in step.step_type && (
        <div className="space-y-3 border-t border-gray-700 pt-3">
          <div>
            <label className="block text-xs text-gray-400 mb-1">Tool ID</label>
            <input
              type="text"
              value={step.step_type.tool_id}
              onChange={(e) => handleStepTypeFieldChange("tool_id", e.target.value)}
              className="w-full bg-gray-800 border border-gray-700 rounded px-2 py-1.5 text-sm text-white focus:outline-none focus:ring-1 focus:ring-blue-500"
            />
          </div>
        </div>
      )}

      {stepType === "wait" && "seconds" in step.step_type && (
        <div className="space-y-3 border-t border-gray-700 pt-3">
          <div>
            <label className="block text-xs text-gray-400 mb-1">Duration (seconds)</label>
            <input
              type="number"
              value={step.step_type.seconds}
              onChange={(e) => handleStepTypeFieldChange("seconds", parseInt(e.target.value) || 0)}
              className="w-full bg-gray-800 border border-gray-700 rounded px-2 py-1.5 text-sm text-white focus:outline-none focus:ring-1 focus:ring-blue-500"
            />
          </div>
        </div>
      )}

      {stepType === "human_input" && "prompt" in step.step_type && (
        <div className="space-y-3 border-t border-gray-700 pt-3">
          <div>
            <label className="block text-xs text-gray-400 mb-1">Prompt</label>
            <textarea
              value={step.step_type.prompt}
              onChange={(e) => handleStepTypeFieldChange("prompt", e.target.value)}
              rows={2}
              className="w-full bg-gray-800 border border-gray-700 rounded px-2 py-1.5 text-sm text-white focus:outline-none focus:ring-1 focus:ring-blue-500 resize-none"
            />
          </div>
        </div>
      )}

      {stepType === "fail" && "error" in step.step_type && (
        <div className="space-y-3 border-t border-gray-700 pt-3">
          <div>
            <label className="block text-xs text-gray-400 mb-1">Error Message</label>
            <input
              type="text"
              value={step.step_type.error}
              onChange={(e) => handleStepTypeFieldChange("error", e.target.value)}
              className="w-full bg-gray-800 border border-gray-700 rounded px-2 py-1.5 text-sm text-white focus:outline-none focus:ring-1 focus:ring-blue-500"
            />
          </div>
        </div>
      )}

      {/* Advanced Options */}
      <div className="space-y-3 border-t border-gray-700 pt-3">
        <h4 className="text-xs font-medium text-gray-400 uppercase">Advanced</h4>

        <div>
          <label className="block text-xs text-gray-400 mb-1">Timeout (seconds)</label>
          <input
            type="number"
            value={step.timeout_secs || ""}
            onChange={(e) => handleFieldChange("timeout_secs", parseInt(e.target.value) || null)}
            placeholder="No timeout"
            className="w-full bg-gray-800 border border-gray-700 rounded px-2 py-1.5 text-sm text-white focus:outline-none focus:ring-1 focus:ring-blue-500"
          />
        </div>

        <div>
          <label className="block text-xs text-gray-400 mb-1">Retry Count</label>
          <input
            type="number"
            value={step.retry_count}
            min={0}
            onChange={(e) => handleFieldChange("retry_count", parseInt(e.target.value) || 0)}
            className="w-full bg-gray-800 border border-gray-700 rounded px-2 py-1.5 text-sm text-white focus:outline-none focus:ring-1 focus:ring-blue-500"
          />
        </div>

        <div className="flex items-center gap-2">
          <input
            type="checkbox"
            id="continue_on_error"
            checked={step.continue_on_error}
            onChange={(e) => handleFieldChange("continue_on_error", e.target.checked)}
            className="rounded bg-gray-700 border-gray-600"
          />
          <label htmlFor="continue_on_error" className="text-sm text-gray-300">
            Continue on error
          </label>
        </div>
      </div>
    </div>
  );
}

export function FlowSidebar({
  selectedStep,
  onAddStep,
  onUpdateStep,
  onDeleteStep,
}: FlowSidebarProps) {
  const [activeTab, setActiveTab] = useState<"palette" | "properties">(
    selectedStep ? "properties" : "palette",
  );

  // Auto-switch to properties when a step is selected
  if (selectedStep && activeTab === "palette") {
    setActiveTab("properties");
  }

  return (
    <div className="w-64 bg-gray-900 border-l border-gray-700 flex flex-col">
      {/* Tab Headers */}
      <div className="flex border-b border-gray-700">
        <button
          onClick={() => setActiveTab("palette")}
          className={`flex-1 px-4 py-2 text-sm font-medium transition-colors ${
            activeTab === "palette" ? "text-white bg-gray-800" : "text-gray-400 hover:text-white"
          }`}
        >
          Steps
        </button>
        <button
          onClick={() => setActiveTab("properties")}
          disabled={!selectedStep}
          className={`flex-1 px-4 py-2 text-sm font-medium transition-colors ${
            activeTab === "properties" ? "text-white bg-gray-800" : "text-gray-400 hover:text-white"
          } ${!selectedStep ? "opacity-50 cursor-not-allowed" : ""}`}
        >
          Properties
        </button>
      </div>

      {/* Tab Content */}
      <div className="flex-1 overflow-auto p-3">
        {activeTab === "palette" && <StepPalette onAddStep={onAddStep} />}
        {activeTab === "properties" && selectedStep && (
          <StepProperties
            step={selectedStep}
            onUpdate={onUpdateStep}
            onDelete={() => onDeleteStep(selectedStep.id)}
          />
        )}
        {activeTab === "properties" && !selectedStep && (
          <div className="flex items-center justify-center h-full text-gray-500 text-sm">
            Select a step to edit
          </div>
        )}
      </div>
    </div>
  );
}

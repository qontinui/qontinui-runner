/**
 * StepItem.tsx
 *
 * Displays a single step in the workflow builder.
 * Shows step icon, name, type, and action buttons.
 */

import React from "react";
import {
  FileCode,
  Navigation,
  GitBranch,
  MousePointer2,
  MousePointerClick,
  MousePointer,
  Keyboard,
  Command,
  ArrowUpDown,
  TestTube2,
  Eye,
  Code,
  Package,
  Terminal,
  Camera,
  MessageSquare,
  Globe,
  ChevronUp,
  ChevronDown,
  Trash2,
  GripVertical,
  AlertCircle,
  Search,
  CheckCircle,
  List,
  Play,
  FileSearch,
  Lock,
} from "lucide-react";
import type { UnifiedStep, WorkflowPhase, PromptStep } from "../../types";

// Icon mapping for step types
const STEP_ICONS: Record<string, React.ElementType> = {
  script: FileCode,
  state: Navigation,
  workflow_ref: GitBranch,
  gui_action: MousePointer2,
  api_request: Globe,
  test: TestTube2,
  screenshot: Camera,
  prompt: MessageSquare,
  // AWAS step types
  awas_discover: Search,
  awas_execute: Play,
  awas_check_support: CheckCircle,
  awas_list_actions: List,
  awas_extract_elements: FileSearch,
};

// Icon mapping for GUI action types
const GUI_ACTION_ICONS: Record<string, React.ElementType> = {
  click: MousePointer2,
  double_click: MousePointerClick,
  right_click: MousePointer,
  type: Keyboard,
  hotkey: Command,
  scroll: ArrowUpDown,
};

// Icon mapping for test types
const TEST_ICONS: Record<string, React.ElementType> = {
  playwright: TestTube2,
  qontinui_vision: Eye,
  python: Code,
  repository: Package,
  custom_command: Terminal,
};

// Color classes for step types
const STEP_COLORS: Record<string, string> = {
  script: "text-emerald-400 bg-emerald-500/10",
  state: "text-blue-400 bg-blue-500/10",
  workflow_ref: "text-purple-400 bg-purple-500/10",
  gui_action: "text-orange-400 bg-orange-500/10",
  api_request: "text-cyan-400 bg-cyan-500/10",
  test: "text-green-400 bg-green-500/10",
  screenshot: "text-pink-400 bg-pink-500/10",
  prompt: "text-amber-400 bg-amber-500/10",
  // AWAS step types - teal color scheme
  awas_discover: "text-teal-400 bg-teal-500/10",
  awas_execute: "text-teal-400 bg-teal-500/10",
  awas_check_support: "text-teal-400 bg-teal-500/10",
  awas_list_actions: "text-teal-400 bg-teal-500/10",
  awas_extract_elements: "text-teal-400 bg-teal-500/10",
};

interface StepItemProps {
  step: UnifiedStep;
  phase: WorkflowPhase;
  index: number;
  isFirst: boolean;
  isLast: boolean;
  isSelected: boolean;
  onMoveUp: () => void;
  onMoveDown: () => void;
  onDelete: () => void;
  onClick: () => void;
}

export function StepItem({
  step,
  phase,
  index,
  isFirst,
  isLast,
  isSelected,
  onMoveUp,
  onMoveDown,
  onDelete,
  onClick,
}: StepItemProps) {
  // Check if this is a summary step (locked to last position)
  const isSummaryStep =
    step.type === "prompt" && (step as PromptStep).is_summary_step === true;

  // Get the appropriate icon
  const getIcon = (): React.ElementType => {
    if (step.type === "gui_action") {
      return GUI_ACTION_ICONS[step.action] || MousePointer2;
    }
    if (step.type === "test") {
      return TEST_ICONS[step.test_type] || TestTube2;
    }
    return STEP_ICONS[step.type] || FileCode;
  };

  const Icon = getIcon();
  const colorClass = STEP_COLORS[step.type] || "text-zinc-400 bg-zinc-500/10";
  const [textColor, bgColor] = colorClass.split(" ");

  // Get step subtitle/description
  const getSubtitle = (): string => {
    switch (step.type) {
      case "script":
        return step.target_url ? `URL: ${step.target_url}` : "Playwright script";
      case "state":
        return step.state_name || step.state_id || "Select a state";
      case "workflow_ref":
        return step.workflow_name || step.workflow_id || "Select a workflow";
      case "gui_action":
        if (step.action === "type") return step.text_input || "Enter text...";
        if (step.action === "hotkey") return step.hotkey || "Set hotkey...";
        return step.target_image_names?.join(", ") || "Select target...";
      case "test":
        if (step.command) return step.command;
        if (step.test_type === "playwright") return "Browser test";
        if (step.test_type === "qontinui_vision") return "Vision test";
        if (step.test_type === "python") return "Python test";
        if (step.test_type === "repository") return "Repository test";
        return "Custom command";
      case "screenshot":
        return step.monitor ? `Monitor: ${step.monitor}` : "Capture screen";
      case "api_request":
        return step.url ? `${step.method} ${step.url}` : "Configure request...";
      case "prompt":
        return step.content
          ? step.content.substring(0, 50) + (step.content.length > 50 ? "..." : "")
          : "Enter prompt...";
      // AWAS step types
      case "awas_discover":
        return step.url || "Enter URL to discover...";
      case "awas_execute":
        return step.action_id ? `Action: ${step.action_id}` : "Select action...";
      case "awas_check_support":
        return step.url || "Enter URL to check...";
      case "awas_list_actions":
        return step.url || "List available actions";
      case "awas_extract_elements":
        return step.base_url || "Extract AWAS elements";
      default:
        return "";
    }
  };

  // Check if step needs configuration
  const needsConfig = (): boolean => {
    switch (step.type) {
      case "state":
        return !step.state_id;
      case "workflow_ref":
        return !step.workflow_id;
      case "gui_action":
        if (
          step.action === "click" ||
          step.action === "double_click" ||
          step.action === "right_click"
        ) {
          return !step.target_image_ids || step.target_image_ids.length === 0;
        }
        if (step.action === "type") return !step.text_input;
        if (step.action === "hotkey") return !step.hotkey;
        return false;
      case "api_request":
        return !step.url;
      case "prompt":
        return !step.content;
      // AWAS step types
      case "awas_discover":
        return !step.url;
      case "awas_execute":
        return !step.url || !step.action_id;
      case "awas_check_support":
        return !step.url;
      case "awas_extract_elements":
        return !step.html;
      default:
        return false;
    }
  };

  const showWarning = needsConfig();

  return (
    <div
      onClick={onClick}
      data-step-type={step.type}
      className={`
        flex items-center gap-2 p-2 rounded-md
        bg-zinc-800 hover:bg-zinc-750
        border border-zinc-700
        ${isSelected ? "ring-2 ring-blue-500/50 border-blue-500/30" : "hover:border-zinc-600"}
        cursor-pointer transition-all group
      `}
    >
      {/* Drag Handle - hidden for summary steps */}
      {!isSummaryStep && (
        <div className="text-zinc-600 group-hover:text-zinc-500 cursor-grab">
          <GripVertical className="w-4 h-4" />
        </div>
      )}
      {isSummaryStep && <div className="w-4" />}

      {/* Step Icon */}
      <div className={`p-1.5 rounded ${bgColor}`}>
        <Icon className={`w-4 h-4 ${textColor}`} />
      </div>

      {/* Step Info */}
      <div className="flex-1 min-w-0">
        <div className="flex items-center gap-2">
          <span className="text-sm font-medium text-zinc-200 truncate">{step.name}</span>
          {isSummaryStep && (
            <span className="text-xs px-1.5 py-0.5 rounded bg-purple-500/20 text-purple-400 flex items-center gap-1">
              <Lock className="w-3 h-3" />
              Locked
            </span>
          )}
          {step.type === "test" && step.is_critical && (
            <span className="text-xs px-1.5 py-0.5 rounded bg-red-500/20 text-red-400">
              Critical
            </span>
          )}
          {showWarning && (
            <span title="Needs configuration">
              <AlertCircle className="w-4 h-4 text-yellow-500" />
            </span>
          )}
        </div>
        <div className="text-xs text-zinc-500 truncate">{getSubtitle()}</div>
      </div>

      {/* Action Buttons */}
      <div className="flex items-center gap-1 opacity-0 group-hover:opacity-100 transition-opacity">
        <button
          onClick={(e) => {
            e.stopPropagation();
            onMoveUp();
          }}
          disabled={isFirst || isSummaryStep}
          className={`
            p-1 rounded hover:bg-zinc-700 transition-colors
            ${isFirst || isSummaryStep ? "text-zinc-600 cursor-not-allowed" : "text-zinc-400 hover:text-zinc-200"}
          `}
          title={isSummaryStep ? "Summary step cannot be moved" : "Move up"}
        >
          <ChevronUp className="w-4 h-4" />
        </button>
        <button
          onClick={(e) => {
            e.stopPropagation();
            onMoveDown();
          }}
          disabled={isLast || isSummaryStep}
          className={`
            p-1 rounded hover:bg-zinc-700 transition-colors
            ${isLast || isSummaryStep ? "text-zinc-600 cursor-not-allowed" : "text-zinc-400 hover:text-zinc-200"}
          `}
          title={isSummaryStep ? "Summary step cannot be moved" : "Move down"}
        >
          <ChevronDown className="w-4 h-4" />
        </button>
        <button
          onClick={(e) => {
            e.stopPropagation();
            onDelete();
          }}
          className="p-1 rounded hover:bg-red-500/20 text-zinc-400 hover:text-red-400 transition-colors"
          title="Delete"
        >
          <Trash2 className="w-4 h-4" />
        </button>
      </div>
    </div>
  );
}

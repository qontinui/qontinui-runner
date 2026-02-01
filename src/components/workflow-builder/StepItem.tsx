/**
 * StepItem.tsx
 *
 * Displays a single step in the workflow builder.
 * Shows step icon, name, type, and action buttons.
 */

import React from "react";
import {
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
  ChevronUp,
  ChevronDown,
  Trash2,
  GripVertical,
  AlertCircle,
  Lock,
  Check,
} from "lucide-react";
import type { UnifiedStep, WorkflowPhase, PromptStep } from "../../types";
import { getStepIconConfig } from "@/lib/step-icons";

// Icon mapping for GUI action types (sub-types of gui_action)
const GUI_ACTION_ICONS: Record<string, React.ElementType> = {
  click: MousePointer2,
  double_click: MousePointerClick,
  right_click: MousePointer,
  type: Keyboard,
  hotkey: Command,
  scroll: ArrowUpDown,
};

// Icon mapping for test types (sub-types of test)
const TEST_ICONS: Record<string, React.ElementType> = {
  playwright: TestTube2,
  qontinui_vision: Eye,
  python: Code,
  repository: Package,
  custom_command: Terminal,
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
  /** Whether the phase is in selection mode for batch delete */
  isSelectionMode?: boolean;
  /** Whether this step is selected for deletion */
  isSelectedForDelete?: boolean;
  /** Callback to toggle selection for batch delete */
  onToggleSelect?: () => void;
}

export function StepItem({
  step,
  phase: _phase,
  index: _index,
  isFirst,
  isLast,
  isSelected,
  onMoveUp,
  onMoveDown,
  onDelete,
  onClick,
  isSelectionMode = false,
  isSelectedForDelete = false,
  onToggleSelect,
}: StepItemProps) {
  // Check if this is a summary step (locked to last position)
  const isSummaryStep =
    step.type === "prompt" && (step as PromptStep).is_summary_step === true;

  // Get the appropriate icon and colors from shared config
  const getIconAndColors = () => {
    // Handle sub-types that have their own icon mappings
    if (step.type === "gui_action" && step.action) {
      const SubIcon = GUI_ACTION_ICONS[step.action];
      if (SubIcon) {
        const config = getStepIconConfig("gui_action");
        return { Icon: SubIcon, bgClass: config.bgClass, textClass: config.textClass };
      }
    }
    if (step.type === "test" && step.test_type) {
      const SubIcon = TEST_ICONS[step.test_type];
      if (SubIcon) {
        const config = getStepIconConfig("test");
        return { Icon: SubIcon, bgClass: config.bgClass, textClass: config.textClass };
      }
    }

    // Use shared icon config for all other step types
    const config = getStepIconConfig(step.type);
    return { Icon: config.icon, bgClass: config.bgClass, textClass: config.textClass };
  };

  const { Icon, bgClass: bgColor, textClass: textColor } = getIconAndColors();

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
      case "shell_command":
        return step.command
          ? step.command.substring(0, 50) + (step.command.length > 50 ? "..." : "")
          : "Enter command...";
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
      case "shell_command":
        return !step.command;
      default:
        return false;
    }
  };

  const showWarning = needsConfig();

  return (
    <div
      onClick={isSelectionMode && onToggleSelect ? onToggleSelect : onClick}
      data-step-type={step.type}
      className={`
        flex items-center gap-2 p-2 rounded-md
        bg-zinc-800 hover:bg-zinc-750
        border
        ${isSelectionMode && isSelectedForDelete
          ? "border-red-500/50 bg-red-500/10"
          : isSelected
            ? "ring-2 ring-blue-500/50 border-blue-500/30"
            : "border-zinc-700 hover:border-zinc-600"
        }
        cursor-pointer transition-all group
      `}
    >
      {/* Checkbox in selection mode */}
      {isSelectionMode && (
        <div
          className={`flex-shrink-0 w-5 h-5 rounded border-2 flex items-center justify-center transition-colors ${
            isSelectedForDelete
              ? "bg-red-500 border-red-500"
              : "border-zinc-500"
          }`}
          data-ui-id={`workflow-builder-step-${step.id}-select-checkbox`}
        >
          {isSelectedForDelete && (
            <Check className="w-3 h-3 text-white" />
          )}
        </div>
      )}

      {/* Drag Handle - hidden for summary steps and in selection mode */}
      {!isSelectionMode && !isSummaryStep && (
        <div className="text-zinc-600 group-hover:text-zinc-500 cursor-grab">
          <GripVertical className="w-4 h-4" />
        </div>
      )}
      {!isSelectionMode && isSummaryStep && <div className="w-4" />}

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

      {/* Action Buttons - hidden in selection mode */}
      {!isSelectionMode && (
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
            data-ui-id={`workflow-builder-step-${step.id}-move-up-btn`}
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
            data-ui-id={`workflow-builder-step-${step.id}-move-down-btn`}
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
            data-ui-id={`workflow-builder-step-${step.id}-delete-btn`}
          >
            <Trash2 className="w-4 h-4" />
          </button>
        </div>
      )}
    </div>
  );
}

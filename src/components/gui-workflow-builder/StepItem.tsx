/**
 * StepItem Component
 *
 * Displays a single step in the workflow with action icon, name, and controls.
 */

import {
  MousePointer2,
  MousePointerClick,
  Keyboard,
  Command,
  Navigation,
  GripVertical,
  Trash2,
  ChevronUp,
  ChevronDown,
  Edit2,
} from "lucide-react";
import type { GuiWorkflowStep } from "../../types/gui-workflow";
import { getActionTypeLabel } from "../../types/gui-workflow";

interface StepItemProps {
  step: GuiWorkflowStep;
  index: number;
  isEditing: boolean;
  isFirst: boolean;
  isLast: boolean;
  onEdit: () => void;
  onRemove: () => void;
  onMoveUp: () => void;
  onMoveDown: () => void;
}

function getActionIcon(actionType: string) {
  switch (actionType) {
    case "click":
      return MousePointer2;
    case "double_click":
      return MousePointerClick;
    case "right_click":
      return MousePointer2;
    case "type":
      return Keyboard;
    case "hotkey":
      return Command;
    case "go_to_state":
      return Navigation;
    default:
      return MousePointer2;
  }
}

function getStepDescription(step: GuiWorkflowStep): string {
  switch (step.action_type) {
    case "click":
    case "double_click":
    case "right_click":
      if (step.target_image_names && step.target_image_names.length > 0) {
        return step.target_image_names.join(", ");
      }
      return "No target selected";
    case "type":
      if (step.text_input) {
        const truncated =
          step.text_input.length > 30 ? step.text_input.slice(0, 30) + "..." : step.text_input;
        return `"${truncated}"`;
      }
      return "No text specified";
    case "hotkey":
      return step.hotkey || "No hotkey specified";
    case "go_to_state":
      if (step.target_state_names && step.target_state_names.length > 0) {
        return step.target_state_names.join(", ");
      }
      return "No state selected";
    default:
      return "";
  }
}

export function StepItem({
  step,
  index,
  isEditing,
  isFirst,
  isLast,
  onEdit,
  onRemove,
  onMoveUp,
  onMoveDown,
}: StepItemProps) {
  const Icon = getActionIcon(step.action_type);
  const description = getStepDescription(step);

  return (
    <div
      className={`group flex items-center gap-2 p-3 rounded-lg border transition-colors ${
        isEditing ? "border-primary bg-primary/5" : "border-border hover:border-primary/50 bg-card"
      }`}
    >
      {/* Drag handle */}
      <div className="cursor-grab text-muted-foreground hover:text-foreground">
        <GripVertical className="h-4 w-4" />
      </div>

      {/* Step number */}
      <div className="flex-shrink-0 w-6 h-6 rounded-full bg-muted flex items-center justify-center text-xs font-medium">
        {index + 1}
      </div>

      {/* Action icon */}
      <div className="flex-shrink-0 p-1.5 rounded bg-primary/10 text-primary">
        <Icon className="h-4 w-4" />
      </div>

      {/* Step info */}
      <div className="flex-1 min-w-0 cursor-pointer" onClick={onEdit}>
        <div className="flex items-center gap-2">
          <span className="font-medium truncate">{step.name}</span>
          <span className="text-xs text-muted-foreground">
            {getActionTypeLabel(step.action_type)}
          </span>
        </div>
        <div className="text-sm text-muted-foreground truncate">{description}</div>
        {step.pause_after_ms && (
          <div className="text-xs text-muted-foreground">+ {step.pause_after_ms}ms pause</div>
        )}
      </div>

      {/* Controls */}
      <div className="flex items-center gap-1 opacity-0 group-hover:opacity-100 transition-opacity">
        <button
          className="p-1.5 rounded hover:bg-muted text-muted-foreground hover:text-foreground transition-colors"
          onClick={onEdit}
          title="Edit step"
        >
          <Edit2 className="h-3.5 w-3.5" />
        </button>
        <button
          className="p-1.5 rounded hover:bg-muted text-muted-foreground hover:text-foreground transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
          onClick={onMoveUp}
          disabled={isFirst}
          title="Move up"
        >
          <ChevronUp className="h-3.5 w-3.5" />
        </button>
        <button
          className="p-1.5 rounded hover:bg-muted text-muted-foreground hover:text-foreground transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
          onClick={onMoveDown}
          disabled={isLast}
          title="Move down"
        >
          <ChevronDown className="h-3.5 w-3.5" />
        </button>
        <button
          className="p-1.5 rounded hover:bg-muted text-destructive hover:text-destructive transition-colors"
          onClick={onRemove}
          title="Remove step"
        >
          <Trash2 className="h-3.5 w-3.5" />
        </button>
      </div>
    </div>
  );
}

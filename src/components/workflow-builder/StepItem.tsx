/**
 * StepItem.tsx
 *
 * Thin wrapper around StepItemConcrete.
 * Provides move up/down buttons as reorderSlot and
 * selection checkbox as selectionCheckbox.
 */

import React from "react";
import {
  Terminal,
  TestTube2,
  Eye,
  Code,
  Package,
  MessageSquare,
  Monitor,
  Activity,
  AlertTriangle,
  CheckCircle,
  Workflow,
  ChevronUp,
  ChevronDown,
  Check,
} from "lucide-react";
import type { UnifiedStep, WorkflowPhase } from "../../types";
import { StepItemConcrete } from "@qontinui/workflow-ui/components";

// =============================================================================
// Icon Resolver
// =============================================================================

const ICON_MAP: Record<string, React.ComponentType<{ className?: string }>> = {
  terminal: Terminal,
  "message-square": MessageSquare,
  "test-tube-2": TestTube2,
  monitor: Monitor,
  "alert-triangle": AlertTriangle,
  "check-circle": CheckCircle,
  workflow: Workflow,
  activity: Activity,
  eye: Eye,
  code: Code,
  package: Package,
};

function resolveIcon(iconId: string): React.ComponentType<{ className?: string }> {
  return ICON_MAP[iconId] ?? Activity;
}

// =============================================================================
// Props
// =============================================================================

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
  isSelectionMode?: boolean;
  isSelectedForDelete?: boolean;
  onToggleSelect?: () => void;
}

// =============================================================================
// Component — Move-button wrapper around StepItemConcrete
// =============================================================================

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
  const moveButtons = (
    <div className="flex flex-col gap-0.5 shrink-0 opacity-0 group-hover:opacity-100 transition-opacity">
      <button
        onClick={(e) => {
          e.stopPropagation();
          onMoveUp();
        }}
        disabled={isFirst}
        className={`p-0.5 rounded transition-colors ${
          isFirst
            ? "text-zinc-600 cursor-not-allowed"
            : "text-zinc-400 hover:text-zinc-200 hover:bg-zinc-700"
        }`}
        title="Move up"
      >
        <ChevronUp className="w-3.5 h-3.5" />
      </button>
      <button
        onClick={(e) => {
          e.stopPropagation();
          onMoveDown();
        }}
        disabled={isLast}
        className={`p-0.5 rounded transition-colors ${
          isLast
            ? "text-zinc-600 cursor-not-allowed"
            : "text-zinc-400 hover:text-zinc-200 hover:bg-zinc-700"
        }`}
        title="Move down"
      >
        <ChevronDown className="w-3.5 h-3.5" />
      </button>
    </div>
  );

  const selectionCheckbox = (
    <div
      role="checkbox"
      aria-checked={isSelectedForDelete}
      tabIndex={0}
      className={`flex-shrink-0 w-5 h-5 rounded border-2 flex items-center justify-center transition-colors ${
        isSelectedForDelete ? "bg-red-500 border-red-500" : "border-zinc-500"
      }`}
      onClick={(e) => {
        e.stopPropagation();
        onToggleSelect?.();
      }}
      onKeyDown={(e) => {
        if (e.key === "Enter" || e.key === " ") {
          e.stopPropagation();
          onToggleSelect?.();
        }
      }}
    >
      {isSelectedForDelete && <Check className="w-3 h-3 text-white" />}
    </div>
  );

  return (
    <StepItemConcrete
      step={step}
      isSelected={isSelected}
      onClick={isSelectionMode && onToggleSelect ? onToggleSelect : onClick}
      onDelete={onDelete}
      isSelectionMode={isSelectionMode}
      isSelectedForDelete={isSelectedForDelete}
      reorderSlot={moveButtons}
      selectionCheckbox={selectionCheckbox}
      resolveIcon={resolveIcon}
    />
  );
}

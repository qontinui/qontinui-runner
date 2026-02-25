import React, { useState, useRef, useEffect } from "react";
import { Plus, Trash2, ChevronUp, ChevronDown, Settings2 } from "lucide-react";
import { Button } from "../ui/Button";
import { Badge } from "../ui/Badge";
import { useWorkflowBuilder } from "./WorkflowBuilderContext";

/**
 * StageSelector — renders a stage tab bar when the workflow has stages.
 * When no stages exist, shows a toggle to enable multi-stage mode.
 */
export function StageSelector() {
  const {
    state,
    currentStageIndex,
    currentStage,
    addStage,
    removeStage,
    selectStage,
    updateStage,
    moveStage,
    updateWorkflow,
  } = useWorkflowBuilder();

  const stages = state.workflow.stages ?? [];
  const hasStages = stages.length > 0;

  // Always select first phase if nothing is selected and stages exist
  React.useEffect(() => {
    if (hasStages && currentStageIndex === null) {
      selectStage(0);
    }
  }, [hasStages, currentStageIndex, selectStage]);

  if (!hasStages) {
    // Single-phase workflow — show a single "Phase 1" tab
    return (
      <div className="border-b border-zinc-800">
        <div className="flex items-center gap-1 px-2 py-1.5">
          <button className="flex items-center gap-1.5 px-2.5 py-1 rounded text-xs font-medium bg-zinc-700 text-zinc-100 ring-1 ring-zinc-600">
            <span className="text-[10px] text-zinc-500">1.</span>
            <span className="max-w-[120px] truncate">{state.workflow.name || "Phase 1"}</span>
          </button>
          <Button
            variant="ghost"
            size="sm"
            className="h-7 px-2 text-xs text-zinc-400 hover:text-zinc-200 shrink-0"
            onClick={() => addStage("Phase 2")}
            title="Add phase"
          >
            <Plus className="size-3" />
          </Button>
        </div>
      </div>
    );
  }

  return (
    <div className="border-b border-zinc-800">
      {/* Phase tabs */}
      <div className="flex items-center gap-1 px-2 py-1.5 overflow-x-auto">
        {stages.map((stage, index) => (
          <StageTab
            key={stage.id}
            name={stage.name}
            index={index}
            isActive={currentStageIndex === index}
            stepCount={
              (stage.setup_steps?.length ?? 0) +
              (stage.verification_steps?.length ?? 0) +
              (stage.agentic_steps?.length ?? 0) +
              (stage.completion_steps?.length ?? 0)
            }
            onSelect={() => selectStage(index)}
            onRename={(name) => updateStage(index, { name })}
            onRemove={stages.length > 1 ? () => removeStage(index) : undefined}
            onMoveUp={index > 0 ? () => moveStage(index, "up") : undefined}
            onMoveDown={index < stages.length - 1 ? () => moveStage(index, "down") : undefined}
          />
        ))}

        <Button
          variant="ghost"
          size="sm"
          className="h-7 px-2 text-xs text-zinc-400 hover:text-zinc-200 shrink-0"
          onClick={() => addStage(`Phase ${stages.length + 1}`)}
        >
          <Plus className="size-3" />
        </Button>

        {stages.length > 1 && (
          <div className="ml-auto flex items-center gap-1.5 shrink-0">
            <label className="flex items-center gap-1.5 text-[10px] text-zinc-500">
              Stop on failure
              <input
                type="checkbox"
                checked={state.workflow.stop_on_failure ?? false}
                onChange={(e) => updateWorkflow({ stop_on_failure: e.target.checked })}
                className="size-3"
              />
            </label>
          </div>
        )}
      </div>

      {/* Active phase settings */}
      {currentStage && currentStageIndex !== null && (
        <StageSettings
          stage={currentStage}
          onUpdate={(updates) => updateStage(currentStageIndex, updates)}
        />
      )}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Popover — lightweight inline popover since runner lacks shadcn Popover
// ---------------------------------------------------------------------------

function SimplePopover({
  trigger,
  children,
}: {
  trigger: React.ReactNode;
  children: React.ReactNode;
}) {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    function handleClick(e: MouseEvent) {
      if (ref.current && !ref.current.contains(e.target as Node)) {
        setOpen(false);
      }
    }
    document.addEventListener("mousedown", handleClick);
    return () => document.removeEventListener("mousedown", handleClick);
  }, [open]);

  return (
    <div ref={ref} className="relative">
      <div onClick={() => setOpen((prev) => !prev)}>{trigger}</div>
      {open && (
        <div className="absolute top-full left-0 z-50 mt-1 w-48 p-1.5 bg-zinc-900 border border-zinc-700 rounded-md shadow-lg">
          {children}
        </div>
      )}
    </div>
  );
}

// ---------------------------------------------------------------------------
// StageTab
// ---------------------------------------------------------------------------

function StageTab({
  name,
  index,
  isActive,
  stepCount,
  onSelect,
  onRename,
  onRemove,
  onMoveUp,
  onMoveDown,
}: {
  name: string;
  index: number;
  isActive: boolean;
  stepCount: number;
  onSelect: () => void;
  onRename: (name: string) => void;
  onRemove?: () => void;
  onMoveUp?: () => void;
  onMoveDown?: () => void;
}) {
  return (
    <SimplePopover
      trigger={
        <button
          className={`
            flex items-center gap-1.5 px-2.5 py-1 rounded text-xs font-medium
            transition-colors shrink-0
            ${
              isActive
                ? "bg-zinc-700 text-zinc-100 ring-1 ring-zinc-600"
                : "text-zinc-400 hover:bg-zinc-800 hover:text-zinc-300"
            }
          `}
          onClick={onSelect}
        >
          <span className="text-[10px] text-zinc-500">{index + 1}.</span>
          <span className="max-w-[120px] truncate">{name}</span>
          <Badge variant="muted" size="sm" className="h-4 px-1 text-[9px] bg-zinc-600/50">
            {stepCount}
          </Badge>
        </button>
      }
    >
      <StageTabMenu
        name={name}
        onRename={onRename}
        onRemove={onRemove}
        onMoveUp={onMoveUp}
        onMoveDown={onMoveDown}
      />
    </SimplePopover>
  );
}

function StageTabMenu({
  name,
  onRename,
  onRemove,
  onMoveUp,
  onMoveDown,
}: {
  name: string;
  onRename: (name: string) => void;
  onRemove?: () => void;
  onMoveUp?: () => void;
  onMoveDown?: () => void;
}) {
  const [editName, setEditName] = useState(name);

  return (
    <div className="space-y-1">
      <input
        value={editName}
        onChange={(e) => setEditName(e.target.value)}
        onBlur={() => {
          if (editName.trim() && editName !== name) onRename(editName.trim());
        }}
        onKeyDown={(e) => {
          if (e.key === "Enter" && editName.trim()) {
            onRename(editName.trim());
          }
        }}
        className="w-full h-7 px-2 text-xs bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 placeholder:text-zinc-500 focus:outline-none focus:ring-1 focus:ring-zinc-600"
        placeholder="Phase name"
      />
      <div className="flex gap-1">
        {onMoveUp && (
          <Button
            variant="ghost"
            size="sm"
            className="h-6 px-1.5 text-xs flex-1"
            onClick={onMoveUp}
          >
            <ChevronUp className="size-3" />
          </Button>
        )}
        {onMoveDown && (
          <Button
            variant="ghost"
            size="sm"
            className="h-6 px-1.5 text-xs flex-1"
            onClick={onMoveDown}
          >
            <ChevronDown className="size-3" />
          </Button>
        )}
        {onRemove && (
          <Button
            variant="ghost"
            size="sm"
            className="h-6 px-1.5 text-xs text-red-400 hover:text-red-300 flex-1"
            onClick={onRemove}
          >
            <Trash2 className="size-3" />
          </Button>
        )}
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------
// StageSettings — collapsible per-stage settings (shown below tabs)
// ---------------------------------------------------------------------------

function StageSettings({
  stage,
  onUpdate,
}: {
  stage: { description?: string; max_iterations?: number; provider?: string; model?: string };
  onUpdate: (updates: Record<string, unknown>) => void;
}) {
  const [expanded, setExpanded] = useState(false);

  return (
    <div className="px-3 pb-1.5">
      <button
        className="flex items-center gap-1 text-[10px] text-zinc-500 hover:text-zinc-400"
        onClick={() => setExpanded(!expanded)}
      >
        <Settings2 className="size-2.5" />
        Phase settings
      </button>
      {expanded && (
        <div className="mt-1.5 grid grid-cols-2 gap-2">
          <div className="col-span-2">
            <input
              value={stage.description ?? ""}
              onChange={(e) => onUpdate({ description: e.target.value })}
              placeholder="Phase description"
              className="w-full h-7 px-2 text-xs bg-zinc-800/50 border border-zinc-700 rounded-md text-zinc-200 placeholder:text-zinc-500 focus:outline-none focus:ring-1 focus:ring-zinc-600"
            />
          </div>
          <div>
            <label className="text-[10px] text-zinc-500">Max iterations</label>
            <input
              type="number"
              value={stage.max_iterations ?? 10}
              onChange={(e) => onUpdate({ max_iterations: parseInt(e.target.value) || 10 })}
              className="w-full h-7 px-2 text-xs bg-zinc-800/50 border border-zinc-700 rounded-md text-zinc-200 placeholder:text-zinc-500 focus:outline-none focus:ring-1 focus:ring-zinc-600"
              min={1}
              max={100}
            />
          </div>
          <div>
            <label className="text-[10px] text-zinc-500">Provider override</label>
            <input
              value={stage.provider ?? ""}
              onChange={(e) => onUpdate({ provider: e.target.value || undefined })}
              placeholder="(inherit)"
              className="w-full h-7 px-2 text-xs bg-zinc-800/50 border border-zinc-700 rounded-md text-zinc-200 placeholder:text-zinc-500 focus:outline-none focus:ring-1 focus:ring-zinc-600"
            />
          </div>
          <div>
            <label className="text-[10px] text-zinc-500">Model override</label>
            <input
              value={stage.model ?? ""}
              onChange={(e) => onUpdate({ model: e.target.value || undefined })}
              placeholder="(inherit)"
              className="w-full h-7 px-2 text-xs bg-zinc-800/50 border border-zinc-700 rounded-md text-zinc-200 placeholder:text-zinc-500 focus:outline-none focus:ring-1 focus:ring-zinc-600"
            />
          </div>
        </div>
      )}
    </div>
  );
}
